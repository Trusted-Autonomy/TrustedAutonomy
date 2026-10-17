//! `ta golden`: capture a completed goal and replay it with an override.
//!
//! A golden entry is (git commit, prompt, baseline result). A replay checks out
//! the commit in a scratch worktree (never the user's tree), runs a normal
//! `ta run` with the stored prompt tagged `experiment_id = golden:<id>` and
//! `experiment_arm = <tag>`, and records cost, outcome, check result and diff.
//! `ta golden report` compares each replay to its baseline. Nothing here
//! applies or approves anything: replay drafts are left for a person.
//!
//! The data model, storage, diff similarity and verdict live in
//! `ta_goal::golden`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::Context;
use chrono::{DateTime, Utc};
use clap::Subcommand;
use serde_json::Value;
use ta_goal::golden::{
    default_entry_id, diff_similarity, diff_stats, parse_overrides, render_file_diff, sha256_hex,
    verdict, Baseline, GoldenEntry, GoldenStore, ReplayRecord, Verdict, VerdictResult,
    EQUIVALENCE_THRESHOLD,
};
use ta_goal::{GoalRun, GoalRunState, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;

#[derive(Debug, Subcommand)]
pub enum GoldenCommands {
    /// Store a completed goal as a golden entry: its start commit, prompt and
    /// baseline result, under .ta/golden/<id>/.
    ///
    /// Refuses a goal that started from uncommitted changes (it cannot be
    /// replayed exactly) unless --allow-dirty is given.
    Capture {
        /// Goal id, id prefix or tag.
        goal_id: String,
        /// Command that decides whether a result is acceptable, run in the
        /// goal's workspace (for example "cargo test -p mycrate"). Exit 0 means pass.
        #[arg(long, value_name = "CMD")]
        check: Option<String>,
        /// Capture a goal that started from uncommitted changes anyway.
        #[arg(long)]
        allow_dirty: bool,
    },
    /// List golden entries with baseline cost and age.
    List {
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Replay an entry: same prompt, same commit, optionally with an override.
    ///
    /// Runs in a scratch worktree of the start commit, so your working tree is
    /// not touched. The replay's draft is left unapplied for you to review.
    Replay {
        /// Golden entry id (see `ta golden list`).
        id: String,
        /// Override applied to this replay, repeatable: --set key=value.
        /// Stored on the goal as its experiment overrides.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Label for this replay (the experiment arm). Default: replay-<timestamp>.
        #[arg(long)]
        tag: Option<String>,
        /// Stop the replay if it would spend more than this many USD.
        #[arg(long, value_name = "USD")]
        max_usd: Option<f64>,
    },
    /// Compare replays with their baselines: cost, tokens, outcome, check and
    /// diff, with a verdict of equivalent, differs or failed.
    Report {
        /// Entry ids to report (default: every entry).
        ids: Vec<String>,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub fn execute(cmd: &GoldenCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        GoldenCommands::Capture {
            goal_id,
            check,
            allow_dirty,
        } => {
            let captured = capture(config, goal_id, check.as_deref(), *allow_dirty)?;
            print!("{}", render_capture(&captured));
            Ok(())
        }
        GoldenCommands::List { json } => list(config, *json),
        GoldenCommands::Replay {
            id,
            set,
            tag,
            max_usd,
        } => {
            let ta_bin = std::env::current_exe().context(
                "could not find the running `ta` binary to launch the replay; \
                 run `ta golden replay` from an installed ta",
            )?;
            let mut run = |cmd: &mut Command| cmd.status();
            let outcome = replay(config, &ta_bin, id, set, tag.as_deref(), *max_usd, &mut run)?;
            print!("{}", render_replay(&outcome));
            Ok(())
        }
        GoldenCommands::Report { ids, json } => report(config, ids, *json),
    }
}

// ── Capture ─────────────────────────────────────────────────────────────

struct Captured {
    entry: GoldenEntry,
    dir: PathBuf,
    notes: Vec<String>,
}

/// Goal states that mean the agent finished and produced a result worth
/// keeping as a baseline.
fn is_completed(state: &GoalRunState) -> bool {
    matches!(
        state,
        GoalRunState::PrReady
            | GoalRunState::UnderReview
            | GoalRunState::Approved { .. }
            | GoalRunState::Applied
            | GoalRunState::Completed
            | GoalRunState::Merged
    )
}

fn resolve_goal(store: &GoalRunStore, input: &str) -> anyhow::Result<GoalRun> {
    if let Some(g) = store.resolve_tag_or_id(input)? {
        return Ok(g);
    }
    let matches = store
        .list()?
        .into_iter()
        .filter(|g| g.goal_run_id.to_string().starts_with(input))
        .count();
    if matches > 1 {
        anyhow::bail!(
            "'{input}' matches {matches} goals. Use a longer id prefix or the goal's tag (see `ta goal list`)."
        );
    }
    anyhow::bail!("No goal found matching '{input}'. Run `ta goal list` to see goal ids and tags.")
}

fn capture(
    config: &GatewayConfig,
    goal_ref: &str,
    check: Option<&str>,
    allow_dirty: bool,
) -> anyhow::Result<Captured> {
    let store = GoalRunStore::new(&config.goals_dir)?;
    let goal = resolve_goal(&store, goal_ref)?;
    let goal_id = goal.goal_run_id.to_string();

    if !is_completed(&goal.state) {
        anyhow::bail!(
            "Goal {} is in state `{}`, so it has no finished result to use as a baseline. \
             Capture a goal that has reached pr_ready, approved, applied or completed.",
            goal.display_tag(),
            goal.state
        );
    }
    if let Some(b) = &goal.budget_abort {
        anyhow::bail!(
            "Goal {} stopped early on its {} ceiling ({}), so its result is incomplete and cannot be a baseline. \
             Capture a goal that ran to completion.",
            goal.display_tag(),
            b.limit,
            b.message
        );
    }
    let Some(start_commit) = goal.start_commit.clone() else {
        anyhow::bail!(
            "Goal {} has no recorded start commit, so it cannot be replayed. Only goals started in a git \
             project after start-state capture was added record one.",
            goal.display_tag()
        );
    };
    if goal.start_dirty && !allow_dirty {
        anyhow::bail!(
            "Goal {} {}. A replay would start from the clean commit {} and could differ for that reason alone. \
             Re-run with --allow-dirty to capture it anyway.",
            goal.display_tag(),
            ta_goal::start_state::DIRTY_NOTICE,
            short(&start_commit)
        );
    }
    let prompt = goal
        .objective_file_content
        .clone()
        .unwrap_or_else(|| goal.objective.clone());
    if prompt.trim().is_empty() {
        anyhow::bail!(
            "Goal {} has no objective text, so there is no prompt to replay.",
            goal.display_tag()
        );
    }
    // The staging folder is removed by `ta cleanup` once a goal is applied. The result is then
    // read from the commit the apply recorded on the draft package, so a finished goal stays
    // capturable. The check command needs the workspace and is skipped in that case.
    let workspace_present = goal.workspace_path.exists();
    let result_commit = if workspace_present {
        None
    } else {
        match draft_commit_sha(config, &goal_id) {
            Some(sha) if ensure_commit(&config.workspace_root, &sha).is_ok() => Some(sha),
            _ => anyhow::bail!(
                "The staging workspace of goal {} ({}) no longer exists and its draft records no apply commit \
                 present in this repository, so its baseline diff cannot be computed. Capture a goal whose \
                 workspace is still present, or fetch the apply commit (`git fetch --all`).",
                goal.display_tag(),
                goal.workspace_path.display()
            ),
        }
    };
    let paths = draft_paths(config, &goal_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Goal {} has no draft package, so there is no result to use as a baseline. \
             Run `ta draft build {}` first.",
            goal.display_tag(),
            goal_id
        )
    })?;
    ensure_commit(&config.workspace_root, &start_commit)?;

    let mut notes = Vec::new();
    let diff = build_diff(
        &paths,
        |rel| git_show(&config.workspace_root, &start_commit, rel),
        |rel| match &result_commit {
            Some(sha) => git_show(&config.workspace_root, sha, rel),
            None => read_text(&goal.workspace_path.join(rel)),
        },
    );
    let stats = diff_stats(&diff);
    if let Some(sha) = &result_commit {
        notes.push(format!(
            "The staging workspace is gone, so the baseline diff was read from apply commit {}.{}",
            short(sha),
            if check.is_some() {
                " The check was not run at capture."
            } else {
                ""
            }
        ));
    }

    let check_passed = match check {
        Some(_) if result_commit.is_some() => None,
        Some(cmd) => match run_check(cmd, &goal.workspace_path) {
            Ok(pass) => Some(pass),
            Err(e) => {
                notes.push(format!(
                    "The check could not be run, so the baseline records no check result: {e}"
                ));
                None
            }
        },
        None => None,
    };
    if check.is_some() && check_passed == Some(false) {
        notes.push(
            "The check FAILED against the original result. Replays that pass it would be an \
             improvement; make sure the check command is the one you mean."
                .to_string(),
        );
    }

    let outcome = goal.state.to_string();
    let id = default_entry_id(&goal.title, &goal_id);
    let entry = GoldenEntry {
        id,
        title: goal.title.clone(),
        source_goal_id: goal_id,
        start_commit,
        start_dirty: goal.start_dirty,
        start_branch: goal.start_branch.clone(),
        prompt_sha256: sha256_hex(&prompt),
        agent_id: goal.agent_id.clone(),
        agent_model: goal.agent_model.clone(),
        check_command: check.map(str::to_string),
        baseline: Baseline {
            cost_usd: goal.cost_usd,
            cost_estimated: goal.cost_estimated,
            input_tokens: goal.input_tokens,
            output_tokens: goal.output_tokens,
            cache_read_tokens: goal.cache_read_input_tokens,
            cache_creation_tokens: goal.cache_creation_input_tokens,
            turns: None,
            outcome,
            files_changed: stats.files,
            lines_added: stats.added,
            lines_removed: stats.removed,
            diff_sha256: sha256_hex(&diff),
            check_passed,
            captured_at: Utc::now(),
        },
    };
    if goal.start_dirty {
        notes.push(format!(
            "This goal {}; replays will not match it exactly.",
            ta_goal::start_state::DIRTY_NOTICE
        ));
    }
    if goal.cost_usd == 0.0 {
        notes.push(
            "The goal has no recorded cost (its model may have no price), so cost comparisons will be \
             unavailable. Tokens are still compared."
                .to_string(),
        );
    }
    let dir = GoldenStore::new(&config.workspace_root).save_entry(&entry, &prompt, &diff)?;
    tracing::info!(golden_id = %entry.id, goal_id = %entry.source_goal_id, path = %dir.display(), "golden entry captured");
    Ok(Captured { entry, dir, notes })
}

fn render_capture(c: &Captured) -> String {
    let e = &c.entry;
    let b = &e.baseline;
    let mut out = format!(
        "Captured golden entry '{}' from goal {}\n  Stored in:   {}\n  Start:       {}{}\n  Baseline:    {}, {} tokens, outcome {}\n  Diff:        {} file(s), +{} -{} (sha256 {})\n  Check:       {}\n",
        e.id,
        short(&e.source_goal_id),
        c.dir.display(),
        short(&e.start_commit),
        e.start_branch
            .as_deref()
            .map(|b| format!(" on {b}"))
            .unwrap_or_default(),
        fmt_cost(b.cost_usd, b.cost_estimated),
        total_tokens(b.input_tokens, b.output_tokens, b.cache_read_tokens, b.cache_creation_tokens),
        b.outcome,
        b.files_changed,
        b.lines_added,
        b.lines_removed,
        &b.diff_sha256[..12.min(b.diff_sha256.len())],
        match (&e.check_command, b.check_passed) {
            (None, _) => "none (equivalence will rest on diff similarity alone)".to_string(),
            (Some(c), Some(true)) => format!("{c} (passes on the baseline)"),
            (Some(c), Some(false)) => format!("{c} (FAILS on the baseline)"),
            (Some(c), None) => format!("{c} (not run at capture)"),
        }
    );
    for n in &c.notes {
        out.push_str(&format!("  Note: {n}\n"));
    }
    out.push_str(&format!(
        "Next: ta golden replay {} --set key=value\n",
        e.id
    ));
    out
}

// ── List ────────────────────────────────────────────────────────────────

fn list(config: &GatewayConfig, json: bool) -> anyhow::Result<()> {
    let store = GoldenStore::new(&config.workspace_root);
    let (entries, problems) = store.list()?;
    let counts = store.replay_counts();
    if json {
        let rows: Vec<Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "title": e.title,
                    "start_commit": e.start_commit,
                    "baseline_cost_usd": e.baseline.cost_usd,
                    "baseline_cost_estimated": e.baseline.cost_estimated,
                    "captured_at": e.baseline.captured_at,
                    "replays": counts.get(&e.id).copied().unwrap_or(0),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print!("{}", render_list(&entries, &counts, Utc::now()));
    }
    for p in problems {
        eprintln!("[warn] skipped an unreadable golden entry: {p}");
    }
    Ok(())
}

fn render_list(
    entries: &[GoldenEntry],
    counts: &std::collections::BTreeMap<String, usize>,
    now: DateTime<Utc>,
) -> String {
    if entries.is_empty() {
        return "No golden entries yet. Capture one from a completed goal:\n  ta golden capture <goal-id> [--check \"<cmd>\"]\n"
            .to_string();
    }
    let mut out = format!(
        "{:<44} {:>12} {:>6} {:>8}  {}\n",
        "ID", "BASELINE", "AGE", "REPLAYS", "TITLE"
    );
    for e in entries {
        out.push_str(&format!(
            "{:<44} {:>12} {:>6} {:>8}  {}\n",
            e.id,
            fmt_cost(e.baseline.cost_usd, e.baseline.cost_estimated),
            age(now, e.baseline.captured_at),
            counts.get(&e.id).copied().unwrap_or(0),
            e.title
        ));
    }
    out
}

// ── Replay ──────────────────────────────────────────────────────────────

struct ReplayOutcome {
    record: ReplayRecord,
    verdict: VerdictResult,
    record_path: PathBuf,
    draft_note: Option<String>,
}

/// Plain-words cost estimate printed before the replay starts.
fn estimate_lines(entry: &GoldenEntry, max_usd: Option<f64>) -> String {
    let b = &entry.baseline;
    let mut s = if b.cost_usd > 0.0 {
        format!(
            "Estimated cost: about {} (the baseline's {}; an override can move it either way, and a single run varies).",
            fmt_cost(b.cost_usd, b.cost_estimated),
            if b.cost_estimated { "estimated cost" } else { "cost" }
        )
    } else {
        format!(
            "Estimated cost: unknown (the baseline has no recorded cost). Baseline used {} tokens.",
            total_tokens(
                b.input_tokens,
                b.output_tokens,
                b.cache_read_tokens,
                b.cache_creation_tokens
            )
        )
    };
    match max_usd {
        Some(cap) if b.cost_usd > 0.0 && cap < b.cost_usd => s.push_str(&format!(
            "\n[warn] --max-usd ${cap:.2} is below the baseline cost; the replay will likely stop early and be reported as failed."
        )),
        Some(cap) => s.push_str(&format!("\nCeiling: the replay stops at ${cap:.2}.")),
        None => s.push_str("\nNo ceiling set; pass --max-usd <USD> to cap this replay."),
    }
    s
}

/// The `ta run` invocation for a replay. Pure: the caller spawns it.
#[allow(clippy::too_many_arguments)]
fn build_replay_command(
    ta_bin: &Path,
    project_root: &Path,
    entry: &GoldenEntry,
    prompt_path: &Path,
    scratch: &Path,
    tag: &str,
    overrides: &Value,
    max_usd: Option<f64>,
) -> Command {
    let mut cmd = Command::new(ta_bin);
    cmd.arg("run")
        .arg(format!("Golden replay {} {}", entry.id, tag))
        .arg("--headless")
        .arg("--objective-file")
        .arg(prompt_path)
        .arg("--source")
        .arg(scratch)
        .arg("--experiment-shadow-id")
        .arg(format!("golden:{}", entry.id))
        .arg("--experiment-shadow-arm")
        .arg(tag);
    if !entry.agent_id.is_empty() {
        cmd.arg("--agent").arg(&entry.agent_id);
    }
    if overrides.as_object().is_some_and(|m| !m.is_empty()) {
        cmd.arg("--experiment-shadow-overrides")
            .arg(overrides.to_string());
    }
    if let Some(cap) = max_usd {
        cmd.arg("--max-usd").arg(cap.to_string());
    }
    cmd.current_dir(project_root);
    cmd
}

/// A detached worktree of the start commit in a temp directory. Removed on
/// drop, including on error paths. Never touches the user's working tree,
/// index, branch or HEAD.
struct Scratch {
    project_root: PathBuf,
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Scratch {
    fn create(project_root: &Path, commit: &str) -> anyhow::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("ta-golden-")
            .tempdir()
            .context(
            "could not create a scratch directory for the replay; check free disk space and TMPDIR",
        )?;
        let path = dir.path().join("worktree");
        git(
            project_root,
            &[
                "worktree",
                "add",
                "--detach",
                "--force",
                &path.to_string_lossy(),
                commit,
            ],
        )
        .with_context(|| {
            format!(
                "could not check out {} into a scratch worktree. Your working tree was not changed",
                short(commit)
            )
        })?;
        Ok(Self {
            project_root: project_root.to_path_buf(),
            _dir: dir,
            path,
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let removed = git(
            &self.project_root,
            &[
                "worktree",
                "remove",
                "--force",
                &self.path.to_string_lossy(),
            ],
        );
        if let Err(e) = removed {
            tracing::warn!(path = %self.path.display(), error = %e, "could not remove golden scratch worktree");
            eprintln!(
                "[warn] Could not remove the scratch worktree {}: {e}. Remove it with: \
                 git -C {} worktree remove --force {}",
                self.path.display(),
                self.project_root.display(),
                self.path.display()
            );
        }
        // The temp dir is deleted right after this; prune drops the now-dangling entry.
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = git(&self.project_root, &["worktree", "prune"]);
    }
}

fn replay(
    config: &GatewayConfig,
    ta_bin: &Path,
    id: &str,
    set: &[String],
    tag: Option<&str>,
    max_usd: Option<f64>,
    runner: &mut dyn FnMut(&mut Command) -> std::io::Result<ExitStatus>,
) -> anyhow::Result<ReplayOutcome> {
    let store = GoldenStore::new(&config.workspace_root);
    let entry = store.load_entry(id)?;
    let overrides = parse_overrides(set)?;
    // The project's `[optimizers.input.<name>]` specs apply, so `--set optimizers.input.use=rtk`
    // is judged by exactly the spec the replayed goal will install with.
    let project_cfg = super::compression::load_optimizers(&config.workspace_root)
        .with_overrides(Some(&overrides));
    ta_context_opt::OptimizersConfig::validate_overrides(
        &overrides,
        &ta_context_opt::Registry::builtin().with_config(&project_cfg)?,
    )?;
    if let Some(cap) = max_usd {
        if !(cap.is_finite() && cap > 0.0) {
            anyhow::bail!("--max-usd must be a positive number of USD, got {cap}.");
        }
    }
    let started_at = Utc::now();
    let tag = tag
        .map(str::to_string)
        .unwrap_or_else(|| format!("replay-{}", started_at.format("%Y%m%d-%H%M%S")));
    ensure_commit(&config.workspace_root, &entry.start_commit)?;
    let prompt_path = store.prompt_path(id)?;
    if !prompt_path.exists() {
        anyhow::bail!(
            "{} is missing, so entry '{id}' has no prompt to replay. Capture the goal again.",
            prompt_path.display()
        );
    }
    // The entry records the prompt's hash at capture. A prompt that no longer matches it
    // (hand-edited, corrupted, or swapped) would run a different task than the baseline
    // measured, so the comparison would be wrong and the agent would run unreviewed text.
    let stored_prompt = store.load_prompt(id)?;
    let actual_sha = sha256_hex(&stored_prompt);
    if actual_sha != entry.prompt_sha256 {
        anyhow::bail!(
            "{} no longer matches the hash recorded when entry '{id}' was captured \
             (recorded {}, found {}). Replaying it would run a different task than the baseline \
             measured, so nothing was launched. Restore the file from version control or a \
             backup, or capture the goal again with `ta golden capture`.",
            prompt_path.display(),
            short(&entry.prompt_sha256),
            short(&actual_sha)
        );
    }

    let estimate = estimate_lines(&entry, max_usd);
    println!("{estimate}");
    println!(
        "Replaying '{}' from {} as arm '{}' in a scratch worktree (your working tree is not touched)...",
        entry.id,
        short(&entry.start_commit),
        tag
    );

    let scratch = Scratch::create(&config.workspace_root, &entry.start_commit)?;
    let mut cmd = build_replay_command(
        ta_bin,
        &config.workspace_root,
        &entry,
        &prompt_path,
        &scratch.path,
        &tag,
        &overrides,
        max_usd,
    );
    tracing::info!(golden_id = %entry.id, tag = %tag, scratch = %scratch.path.display(), "launching golden replay");
    let status = runner(&mut cmd).map_err(|e| {
        anyhow::anyhow!(
            "could not launch `{}` for the replay: {e}. Run it by hand to see why: {}",
            ta_bin.display(),
            describe(&cmd)
        )
    })?;

    let goals = GoalRunStore::new(&config.goals_dir)?;
    let experiment_id = format!("golden:{}", entry.id);
    let goal = goals
        .list()?
        .into_iter()
        .filter(|g| {
            g.experiment_id.as_deref() == Some(experiment_id.as_str())
                && g.experiment_arm.as_deref() == Some(tag.as_str())
                && g.created_at >= started_at
        })
        .max_by_key(|g| g.created_at);

    let mut record = ReplayRecord {
        replay_id: uuid::Uuid::new_v4().to_string(),
        golden_id: entry.id.clone(),
        tag: tag.clone(),
        set: set.to_vec(),
        goal_id: None,
        started_at,
        completed_at: Some(Utc::now()),
        outcome: "no goal created".to_string(),
        cost_usd: 0.0,
        cost_estimated: false,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        turns: None,
        aborted_budget: false,
        files_changed: 0,
        diff_sha256: None,
        similarity: None,
        check_passed: None,
        failure: None,
    };
    let mut diff: Option<String> = None;
    let mut draft_note = None;
    match goal {
        None => {
            record.failure = Some(format!(
                "`ta run` exited with {status} and created no goal tagged {experiment_id}/{tag}. \
                 Run it by hand to see why: {}",
                describe(&cmd)
            ));
        }
        Some(goal) => {
            record.goal_id = Some(goal.goal_run_id.to_string());
            record.outcome = goal.state.to_string();
            record.cost_usd = goal.cost_usd;
            record.cost_estimated = goal.cost_estimated;
            record.input_tokens = goal.input_tokens;
            record.output_tokens = goal.output_tokens;
            record.cache_read_tokens = goal.cache_read_input_tokens;
            record.cache_creation_tokens = goal.cache_creation_input_tokens;
            if let Some(b) = &goal.budget_abort {
                record.aborted_budget = true;
                record.outcome = "aborted_budget".to_string();
                tracing::warn!(goal_id = %goal.goal_run_id, limit = %b.limit, "golden replay stopped on its budget ceiling");
            }
            match draft_paths(config, &goal.goal_run_id.to_string())? {
                Some(paths) if goal.workspace_path.exists() => {
                    let d = build_diff(
                        &paths,
                        |rel| read_text(&scratch.path.join(rel)),
                        |rel| read_text(&goal.workspace_path.join(rel)),
                    );
                    let stats = diff_stats(&d);
                    record.files_changed = stats.files;
                    record.diff_sha256 = Some(sha256_hex(&d));
                    record.similarity = Some(diff_similarity(&store.load_baseline_diff(id)?, &d));
                    diff = Some(d);
                    if let Some(cmd) = &entry.check_command {
                        match run_check(cmd, &goal.workspace_path) {
                            Ok(pass) => record.check_passed = Some(pass),
                            Err(e) => {
                                record.failure = Some(format!("the check could not be run: {e}"))
                            }
                        }
                    }
                    draft_note = Some(format!(
                        "The replay's draft is left unapplied (goal {}). Review it with `ta draft list`, \
                         and close it with `ta draft close` when you are done.",
                        short(&record.goal_id.clone().unwrap_or_default())
                    ));
                }
                Some(_) => {
                    record.failure = Some(format!(
                        "the replay's staging workspace {} no longer exists, so its diff and check could not be read",
                        goal.workspace_path.display()
                    ));
                }
                None if record.aborted_budget => {}
                None => {
                    record.failure = Some(format!(
                        "the replay goal ended in state `{}` without a draft package, so there is nothing to compare. \
                         See `ta goal status {}`.",
                        goal.state,
                        short(&goal.goal_run_id.to_string())
                    ));
                }
            }
        }
    }
    let record_path = store.save_replay(&record, diff.as_deref())?;
    let v = verdict(&entry, &record);
    tracing::info!(golden_id = %entry.id, tag = %tag, verdict = v.verdict.as_str(), cost_usd = record.cost_usd, "golden replay recorded");
    drop(scratch);
    Ok(ReplayOutcome {
        record,
        verdict: v,
        record_path,
        draft_note,
    })
}

fn render_replay(o: &ReplayOutcome) -> String {
    let r = &o.record;
    let mut out = format!(
        "Replay '{}' as arm '{}' finished: outcome {}, {}, verdict {}\n  Recorded in {}\n",
        r.golden_id,
        r.tag,
        r.outcome,
        fmt_cost(r.cost_usd, r.cost_estimated),
        o.verdict.verdict.as_str(),
        o.record_path.display()
    );
    for reason in &o.verdict.reasons {
        out.push_str(&format!("  - {reason}\n"));
    }
    if let Some(n) = &o.draft_note {
        out.push_str(&format!("  {n}\n"));
    }
    out.push_str(&format!(
        "Compare with the baseline: ta golden report {}\n",
        r.golden_id
    ));
    out
}

// ── Report ──────────────────────────────────────────────────────────────

fn report(config: &GatewayConfig, ids: &[String], json: bool) -> anyhow::Result<()> {
    let store = GoldenStore::new(&config.workspace_root);
    let entries = if ids.is_empty() {
        store.list()?.0
    } else {
        ids.iter()
            .map(|i| store.load_entry(i))
            .collect::<anyhow::Result<Vec<_>>>()?
    };
    if entries.is_empty() {
        println!("No golden entries yet. Capture one with: ta golden capture <goal-id>");
        return Ok(());
    }
    let mut sections = Vec::new();
    for e in entries {
        let replays = store.list_replays(&e.id)?;
        sections.push((e, replays));
    }
    if json {
        let out: Vec<Value> = sections
            .iter()
            .map(|(e, replays)| {
                let rows: Vec<Value> = replays
                    .iter()
                    .map(|r| {
                        let v = verdict(e, r);
                        serde_json::json!({
                            "replay": r,
                            "verdict": v.verdict,
                            "reasons": v.reasons,
                            "needs_human_review": v.needs_human_review(),
                            "cost_delta_usd": r.cost_usd - e.baseline.cost_usd,
                        })
                    })
                    .collect();
                serde_json::json!({ "entry": e, "replays": rows })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", render_report(&sections));
    }
    Ok(())
}

fn render_report(sections: &[(GoldenEntry, Vec<ReplayRecord>)]) -> String {
    let mut out = String::new();
    let mut review: Vec<String> = Vec::new();
    let mut single_sample = false;
    for (e, replays) in sections {
        let b = &e.baseline;
        let base_tokens = total_tokens(
            b.input_tokens,
            b.output_tokens,
            b.cache_read_tokens,
            b.cache_creation_tokens,
        );
        out.push_str(&format!(
            "{} ({})\n  Baseline: {}, {} tokens, outcome {}, check {}\n",
            e.id,
            e.title,
            fmt_cost(b.cost_usd, b.cost_estimated),
            base_tokens,
            b.outcome,
            check_label(e.check_command.as_deref(), b.check_passed),
        ));
        if replays.is_empty() {
            out.push_str(&format!(
                "  No replays yet. Try: ta golden replay {} --set key=value\n\n",
                e.id
            ));
            continue;
        }
        out.push_str(&format!(
            "  {:<24} {:>10} {:>9} {:>11} {:>9} {:<16} {:<7} {:>5}  {}\n",
            "TAG", "COST", "D COST", "TOKENS", "D TOKENS", "OUTCOME", "CHECK", "DIFF", "VERDICT"
        ));
        for r in replays {
            let v = verdict(e, r);
            let tokens = total_tokens(
                r.input_tokens,
                r.output_tokens,
                r.cache_read_tokens,
                r.cache_creation_tokens,
            );
            out.push_str(&format!(
                "  {:<24} {:>10} {:>9} {:>11} {:>9} {:<16} {:<7} {:>5}  {}\n",
                truncate(&r.tag, 24),
                fmt_cost(r.cost_usd, r.cost_estimated),
                delta_pct(b.cost_usd, r.cost_usd),
                tokens,
                delta_pct(base_tokens as f64, tokens as f64),
                truncate(&r.outcome, 16),
                match r.check_passed {
                    Some(true) => "pass",
                    Some(false) => "FAIL",
                    None => "-",
                },
                r.similarity
                    .map(|s| format!("{:.0}%", s * 100.0))
                    .unwrap_or_else(|| "-".into()),
                v.verdict.as_str(),
            ));
            if v.needs_human_review() {
                review.push(format!(
                    "  {} / {}: {} ({})",
                    e.id,
                    r.tag,
                    v.verdict.as_str(),
                    v.reasons.join("; ")
                ));
            }
        }
        let n = replays.len();
        if n == 1 {
            single_sample = true;
        }
        out.push_str(&format!(
            "  {} replay{}. Turns are not recorded by the agent runtime, so they are not compared.\n\n",
            n,
            if n == 1 { " (one sample)" } else { "s" }
        ));
    }
    if !review.is_empty() {
        out.push_str("Needs human review (never auto-accepted):\n");
        for l in review {
            out.push_str(&l);
            out.push('\n');
        }
        out.push_str("Review a replay's draft with `ta draft list`; its diff is in .ta/golden/<id>/replays/.\n");
    }
    if single_sample {
        out.push_str(&format!(
            "One replay is one sample: agent runs vary, so treat a cost difference as a lead and repeat \
             the replay before acting on a borderline result. Equivalent means check passes (when one is \
             defined) and diff similarity is at least {:.0}%.\n",
            EQUIVALENCE_THRESHOLD * 100.0
        ));
    }
    out
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn short(s: &str) -> String {
    s.chars().take(8).collect()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{t}~")
    }
}

fn fmt_cost(usd: f64, estimated: bool) -> String {
    if usd == 0.0 {
        "n/a".to_string()
    } else {
        format!("${:.2}{}", usd, if estimated { "~" } else { "" })
    }
}

fn total_tokens(input: u64, output: u64, cache_read: u64, cache_creation: u64) -> u64 {
    input + output + cache_read + cache_creation
}

fn delta_pct(base: f64, now: f64) -> String {
    if base <= 0.0 || now <= 0.0 {
        "n/a".to_string()
    } else {
        format!("{:+.0}%", (now - base) / base * 100.0)
    }
}

fn check_label(cmd: Option<&str>, passed: Option<bool>) -> String {
    match (cmd, passed) {
        (None, _) => "none".to_string(),
        (Some(c), Some(true)) => format!("{c} (pass)"),
        (Some(c), Some(false)) => format!("{c} (FAIL)"),
        (Some(c), None) => format!("{c} (not run)"),
    }
}

fn age(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn describe(cmd: &Command) -> String {
    let mut s = cmd.get_program().to_string_lossy().to_string();
    for a in cmd.get_args() {
        let a = a.to_string_lossy();
        if a.contains(' ') || a.contains('{') || a.is_empty() {
            s.push_str(&format!(" '{a}'"));
        } else {
            s.push(' ');
            s.push_str(&a);
        }
    }
    s
}

/// Run `git args` in `root` and return trimmed stdout. A non-zero exit is an
/// error carrying git's own message.
fn git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // An inherited GIT_DIR / GIT_WORK_TREE / GIT_INDEX_FILE would point git
        // at a different repository than `root`.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "could not run `git {}` in {}: {e}. Install git or put it on PATH.",
                args.join(" "),
                root.display()
            )
        })?;
    if !out.status.success() {
        anyhow::bail!(
            "`git {}` failed in {}: {}",
            args.join(" "),
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn ensure_commit(root: &Path, commit: &str) -> anyhow::Result<()> {
    git(root, &["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .map(|_| ())
        .map_err(|e| {
            anyhow::anyhow!(
                "start commit {} is not in this repository ({e}). Fetch it (`git fetch --all`) or \
             capture the goal again from a clone that has it.",
                short(commit)
            )
        })
}

/// File content at `commit`, `None` when the file did not exist there.
fn git_show(root: &Path, commit: &str, rel: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &format!("{commit}:{rel}")])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(text_or_marker(&out.stdout))
}

/// File content, `None` when the file does not exist. Binary content becomes a
/// one-line marker holding its hash so a changed binary still shows as a change.
fn read_text(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| text_or_marker(&b))
}

/// Text for diffing, with line endings normalised to `\n`. A git checkout applies the platform's
/// `core.autocrlf` (CRLF on Windows) while `git show` and an agent's own files may be LF, so
/// without this every line of a file would look changed and a replay would score as "differs"
/// only because of the platform. A file whose only difference is CRLF versus LF therefore
/// diffs as unchanged; for a similarity heuristic that is the intended trade.
fn text_or_marker(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.replace("\r\n", "\n"),
        Err(_) => {
            use sha2::{Digest, Sha256};
            let h: String = Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("<binary file sha256:{h}>\n")
        }
    }
}

fn build_diff(
    paths: &[String],
    old: impl Fn(&str) -> Option<String>,
    new: impl Fn(&str) -> Option<String>,
) -> String {
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort();
    sorted.dedup();
    let mut out = String::new();
    for rel in sorted {
        out.push_str(&render_file_diff(
            rel,
            old(rel).as_deref(),
            new(rel).as_deref(),
        ));
    }
    out
}

/// Workspace-relative paths changed by the newest draft package of `goal_id`.
/// `None` when the goal has no package. Read as loose JSON so an added field
/// elsewhere in the package never breaks this.
fn draft_paths(config: &GatewayConfig, goal_id: &str) -> anyhow::Result<Option<Vec<String>>> {
    let read = match std::fs::read_dir(&config.pr_packages_dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => anyhow::bail!(
            "could not read the draft packages in {}: {e}",
            config.pr_packages_dir.display()
        ),
    };
    let mut best: Option<(String, Vec<String>)> = None;
    for item in read.flatten() {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if v.pointer("/goal/goal_id").and_then(Value::as_str) != Some(goal_id) {
            continue;
        }
        let created = v
            .pointer("/created_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let paths: Vec<String> = v
            .pointer("/changes/artifacts")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("resource_uri").and_then(Value::as_str))
                    .filter_map(|u| u.strip_prefix("fs://workspace/"))
                    .filter(|p| !p.starts_with(".ta/") && !p.split('/').any(|c| c == ".."))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if best.as_ref().is_none_or(|(c, _)| created > *c) {
            best = Some((created, paths));
        }
    }
    Ok(best.map(|(_, p)| p))
}

/// The apply commit recorded on the newest draft package of `goal_id`, if any.
fn draft_commit_sha(config: &GatewayConfig, goal_id: &str) -> Option<String> {
    let mut best: Option<(String, String)> = None;
    for item in std::fs::read_dir(&config.pr_packages_dir).ok()?.flatten() {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if v.pointer("/goal/goal_id").and_then(Value::as_str) != Some(goal_id) {
            continue;
        }
        let Some(sha) = v.pointer("/vcs_status/commit_sha").and_then(Value::as_str) else {
            continue;
        };
        let created = v
            .pointer("/created_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if best.as_ref().is_none_or(|(c, _)| created > *c) {
            best = Some((created, sha.to_string()));
        }
    }
    best.map(|(_, s)| s)
}

/// Run a check command in `dir`; `Ok(true)` when it exits 0.
fn run_check(cmd: &str, dir: &Path) -> anyhow::Result<bool> {
    #[cfg(windows)]
    let mut c = {
        let mut c = Command::new("cmd");
        c.args(["/C", cmd]);
        c
    };
    #[cfg(not(windows))]
    let mut c = {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    let status = c
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| {
            anyhow::anyhow!(
                "could not start `{cmd}` in {}: {e}. Run it by hand to see why.",
                dir.display()
            )
        })?;
    Ok(status.success())
}

#[allow(dead_code)]
fn _verdict_is_exported(_: Verdict) {}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_goal::BudgetAbort;

    fn git_ok(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?} failed");
    }

    fn rev(dir: &Path) -> String {
        git(dir, &["rev-parse", "HEAD"]).unwrap()
    }

    /// A project with two commits: `start` (a.txt = "one\ntwo\n") and a later
    /// HEAD where a.txt has moved on, so a replay checkout is distinguishable.
    struct Fixture {
        project: tempfile::TempDir,
        config: GatewayConfig,
        start: String,
    }

    fn fixture() -> Fixture {
        let project = tempfile::tempdir().unwrap();
        let p = project.path();
        git_ok(p, &["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        git_ok(p, &["add", "."]);
        git_ok(p, &["commit", "-q", "-m", "start"]);
        let start = rev(p);
        std::fs::write(p.join("a.txt"), "one\ntwo\nlater\n").unwrap();
        git_ok(p, &["commit", "-q", "-am", "later"]);
        let config = GatewayConfig::for_project(p);
        std::fs::create_dir_all(&config.goals_dir).unwrap();
        std::fs::create_dir_all(&config.pr_packages_dir).unwrap();
        Fixture {
            project,
            config,
            start,
        }
    }

    /// A finished goal with a staging workspace where a.txt line two changed
    /// to "2" and b.txt was added, plus its draft package.
    fn finished_goal(f: &Fixture, title: &str) -> GoalRun {
        let staging = f
            .project
            .path()
            .join(".ta")
            .join("staging")
            .join(title.replace(' ', "-"));
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("a.txt"), "one\n2\n").unwrap();
        std::fs::write(staging.join("b.txt"), "new\n").unwrap();
        let mut g = GoalRun::new(
            title,
            "Change line two",
            "claude-code",
            staging,
            f.project.path().join(".ta/store"),
        );
        g.state = GoalRunState::PrReady;
        g.start_commit = Some(f.start.clone());
        g.start_branch = Some("main".into());
        g.input_tokens = 100;
        g.output_tokens = 2000;
        g.cost_usd = 2.0;
        GoalRunStore::new(&f.config.goals_dir)
            .unwrap()
            .save(&g)
            .unwrap();
        write_draft(f, &g, &["a.txt", "b.txt"]);
        g
    }

    fn write_draft(f: &Fixture, g: &GoalRun, files: &[&str]) {
        let artifacts: Vec<Value> = files
            .iter()
            .map(|p| serde_json::json!({"resource_uri": format!("fs://workspace/{p}")}))
            .collect();
        let pkg = serde_json::json!({
            "created_at": "2026-10-14T00:00:00Z",
            "goal": {"goal_id": g.goal_run_id.to_string()},
            "changes": {"artifacts": artifacts},
        });
        std::fs::write(
            f.config
                .pr_packages_dir
                .join(format!("{}.json", uuid::Uuid::new_v4())),
            pkg.to_string(),
        )
        .unwrap();
    }

    fn save(f: &Fixture, g: &GoalRun) {
        GoalRunStore::new(&f.config.goals_dir)
            .unwrap()
            .save(g)
            .unwrap();
    }

    #[test]
    fn capture_refuses_a_dirty_goal_unless_allowed() {
        let f = fixture();
        let mut g = finished_goal(&f, "Dirty goal");
        g.start_dirty = true;
        save(&f, &g);
        let err = capture(&f.config, &g.goal_run_id.to_string(), None, false)
            .err()
            .expect("dirty goal must be refused")
            .to_string();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert!(err.contains("--allow-dirty"), "{err}");
        assert!(!GoldenStore::new(f.project.path())
            .list()
            .unwrap()
            .0
            .iter()
            .any(|_| true));

        let c = capture(&f.config, &g.goal_run_id.to_string(), None, true).unwrap();
        assert!(c.entry.start_dirty);
        assert!(c.notes.iter().any(|n| n.contains("will not match")));
    }

    #[test]
    fn capture_stores_entry_prompt_and_baseline_diff_against_the_start_commit() {
        let f = fixture();
        let mut g = finished_goal(&f, "Fix the thing");
        g.objective_file_content = Some("The full prompt from a file\n".into());
        save(&f, &g);
        let c = capture(&f.config, &g.goal_run_id.to_string(), Some("exit 0"), false).unwrap();

        let store = GoldenStore::new(f.project.path());
        assert_eq!(
            store.load_prompt(&c.entry.id).unwrap(),
            "The full prompt from a file\n"
        );
        let diff = store.load_baseline_diff(&c.entry.id).unwrap();
        // Diffed against the start commit's a.txt, not the later HEAD's.
        assert!(diff.contains("-two\n+2\n"), "{diff}");
        assert!(!diff.contains("later"), "{diff}");
        assert!(
            diff.contains("--- /dev/null\n+++ b/b.txt\n+new\n"),
            "{diff}"
        );
        let e = store.load_entry(&c.entry.id).unwrap();
        assert_eq!(e.start_commit, f.start);
        assert_eq!(e.baseline.cost_usd, 2.0);
        assert_eq!(e.baseline.input_tokens, 100);
        assert_eq!(e.baseline.output_tokens, 2000);
        assert_eq!(e.baseline.outcome, "pr_ready");
        assert_eq!(e.baseline.files_changed, 2);
        assert_eq!(e.baseline.check_passed, Some(true));
        assert_eq!(e.baseline.diff_sha256, sha256_hex(&diff));
        assert_eq!(e.prompt_sha256, sha256_hex("The full prompt from a file\n"));
        assert!(render_capture(&c).contains("ta golden replay"));

        let again = capture(&f.config, &g.goal_run_id.to_string(), None, false)
            .err()
            .expect("capturing twice must be refused")
            .to_string();
        assert!(again.contains("already exists"), "{again}");
    }

    /// Commit the staging result onto a branch off `start`, the way an apply does, and record it.
    fn apply_commit(f: &Fixture, g: &GoalRun) -> String {
        let p = f.project.path();
        git_ok(p, &["checkout", "-q", "-b", "applied", &f.start]);
        std::fs::write(p.join("a.txt"), "one\n2\n").unwrap();
        std::fs::write(p.join("b.txt"), "new\n").unwrap();
        git_ok(p, &["add", "a.txt", "b.txt"]);
        git_ok(p, &["commit", "-q", "-m", "applied"]);
        let sha = rev(p);
        git_ok(p, &["checkout", "-q", "main"]);
        let pkg = serde_json::json!({
            "created_at": "2026-10-15T00:00:00Z",
            "goal": {"goal_id": g.goal_run_id.to_string()},
            "changes": {"artifacts": [
                {"resource_uri": "fs://workspace/a.txt"},
                {"resource_uri": "fs://workspace/b.txt"},
            ]},
            "vcs_status": {"commit_sha": sha},
        });
        std::fs::write(
            f.config.pr_packages_dir.join("applied.json"),
            pkg.to_string(),
        )
        .unwrap();
        sha
    }

    #[test]
    fn capture_reads_the_baseline_from_the_apply_commit_when_staging_is_gone() {
        let f = fixture();
        let g = finished_goal(&f, "Applied goal");
        apply_commit(&f, &g);
        std::fs::remove_dir_all(&g.workspace_path).unwrap();
        let c = capture(&f.config, &g.goal_run_id.to_string(), Some("exit 0"), false).unwrap();
        let diff = GoldenStore::new(f.project.path())
            .load_baseline_diff(&c.entry.id)
            .unwrap();
        assert!(diff.contains("-two\n+2\n"), "{diff}");
        assert!(
            diff.contains("--- /dev/null\n+++ b/b.txt\n+new\n"),
            "{diff}"
        );
        assert_eq!(c.entry.baseline.files_changed, 2);
        assert_eq!(c.entry.baseline.check_passed, None);
        assert!(
            c.notes
                .iter()
                .any(|n| n.contains("apply commit") && n.contains("check was not run")),
            "{:?}",
            c.notes
        );
    }

    #[test]
    fn capture_without_staging_or_apply_commit_is_refused_with_next_steps() {
        let f = fixture();
        let g = finished_goal(&f, "Gone goal");
        std::fs::remove_dir_all(&g.workspace_path).unwrap();
        let err = capture(&f.config, &g.goal_run_id.to_string(), None, false)
            .err()
            .expect("must be refused")
            .to_string();
        assert!(err.contains("no apply commit"), "{err}");
        assert!(err.contains("git fetch"), "{err}");
    }

    #[test]
    fn capture_refuses_goals_that_cannot_be_a_baseline() {
        let f = fixture();

        let mut running = finished_goal(&f, "Still running");
        running.state = GoalRunState::Running;
        save(&f, &running);
        let e = capture(&f.config, &running.goal_run_id.to_string(), None, false)
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("no finished result"), "{e}");

        let mut no_commit = finished_goal(&f, "No commit");
        no_commit.start_commit = None;
        save(&f, &no_commit);
        let e = capture(&f.config, &no_commit.goal_run_id.to_string(), None, false)
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("no recorded start commit"), "{e}");

        let mut aborted = finished_goal(&f, "Aborted");
        aborted.budget_abort = Some(BudgetAbort {
            limit: "max_usd".into(),
            limit_value: 1.0,
            observed: 1.2,
            message: "stopped".into(),
        });
        save(&f, &aborted);
        let e = capture(&f.config, &aborted.goal_run_id.to_string(), None, false)
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("incomplete"), "{e}");

        let e = capture(&f.config, "00000000-nothing", None, false)
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("ta goal list"), "{e}");
    }

    #[test]
    fn capture_stores_no_environment_values() {
        let f = fixture();
        std::env::set_var("TA_GOLDEN_TEST_SECRET", "s3cret-value-123");
        let g = finished_goal(&f, "Env check");
        let c = capture(&f.config, &g.goal_run_id.to_string(), None, false).unwrap();
        let mut all = String::new();
        for entry in std::fs::read_dir(&c.dir).unwrap().flatten() {
            all.push_str(&std::fs::read_to_string(entry.path()).unwrap());
        }
        std::env::remove_var("TA_GOLDEN_TEST_SECRET");
        assert!(!all.contains("s3cret-value-123"));
    }

    fn ok_status() -> ExitStatus {
        #[cfg(windows)]
        let s = Command::new("cmd").args(["/C", "exit 0"]).status().unwrap();
        #[cfg(not(windows))]
        let s = Command::new("true").status().unwrap();
        s
    }

    fn captured_entry(f: &Fixture, check: Option<&str>) -> GoldenEntry {
        let g = finished_goal(f, "Fix the thing");
        capture(&f.config, &g.goal_run_id.to_string(), check, false)
            .unwrap()
            .entry
    }

    /// Stand-in for `ta run`: inspects the command and the scratch checkout,
    /// then records the goal a real run would have produced.
    fn fake_run<'a>(
        f: &'a Fixture,
        staging_a: &str,
        user_head_before: String,
        seen: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    ) -> impl FnMut(&mut Command) -> std::io::Result<ExitStatus> + 'a {
        let staging_a = staging_a.to_string();
        move |cmd: &mut Command| {
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().to_string())
                .collect();
            *seen.borrow_mut() = args.clone();
            let src = PathBuf::from(&args[args.iter().position(|a| a == "--source").unwrap() + 1]);
            // The scratch worktree is the start commit, not the user's HEAD.
            assert_eq!(rev(&src), f.start);
            // A git checkout applies the platform's `core.autocrlf` (CRLF on Windows), so compare
            // the content, not the line endings.
            assert_eq!(
                std::fs::read_to_string(src.join("a.txt"))
                    .unwrap()
                    .replace("\r\n", "\n"),
                "one\ntwo\n"
            );
            assert_ne!(src, f.project.path());
            // The user's tree is untouched while the replay runs.
            assert_eq!(rev(f.project.path()), user_head_before);
            assert_eq!(
                std::fs::read_to_string(f.project.path().join("a.txt")).unwrap(),
                "one\ntwo\nlater\n"
            );

            let staging = f.project.path().join(".ta").join("staging").join("replay");
            std::fs::create_dir_all(&staging).unwrap();
            std::fs::write(staging.join("a.txt"), &staging_a).unwrap();
            std::fs::write(staging.join("b.txt"), "new\n").unwrap();
            let mut g = GoalRun::new(
                "Replay",
                "x",
                "claude-code",
                staging,
                f.project.path().join(".ta/store"),
            );
            g.state = GoalRunState::PrReady;
            g.experiment_id = Some(
                args[args
                    .iter()
                    .position(|a| a == "--experiment-shadow-id")
                    .unwrap()
                    + 1]
                .clone(),
            );
            g.experiment_arm = Some(
                args[args
                    .iter()
                    .position(|a| a == "--experiment-shadow-arm")
                    .unwrap()
                    + 1]
                .clone(),
            );
            g.input_tokens = 50;
            g.output_tokens = 900;
            g.cost_usd = 1.0;
            save(f, &g);
            write_draft(f, &g, &["a.txt", "b.txt"]);
            Ok(ok_status())
        }
    }

    #[test]
    fn replay_uses_scratch_checkout_leaves_user_tree_alone_and_tags_the_goal() {
        let f = fixture();
        let entry = captured_entry(&f, Some("exit 0"));
        let head_before = rev(f.project.path());
        git_ok(f.project.path(), &["status", "--porcelain"]);
        let status_before = git(f.project.path(), &["status", "--porcelain"]).unwrap();
        let branch_before = git(f.project.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();

        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut run = fake_run(&f, "one\n2\n", head_before.clone(), seen.clone());
        let out = replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &["optimizers.input.use=none".to_string()],
            Some("no-optimizer"),
            Some(5.0),
            &mut run,
        )
        .unwrap();

        // Command: experiment fields, overrides, ceiling, prompt file, scratch source.
        let args = seen.borrow().clone();
        let after = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!(
            after("--experiment-shadow-id"),
            format!("golden:{}", entry.id)
        );
        assert_eq!(after("--experiment-shadow-arm"), "no-optimizer");
        assert_eq!(
            serde_json::from_str::<Value>(&after("--experiment-shadow-overrides")).unwrap(),
            serde_json::json!({"optimizers.input.use": "none"})
        );
        assert_eq!(after("--max-usd"), "5");
        assert_eq!(after("--agent"), "claude-code");
        assert!(after("--objective-file").ends_with("prompt.md"));
        assert!(!args.contains(&"--auto-cancel-after-draft".to_string()));
        assert!(!args.contains(&"--phase".to_string()));

        // User's tree, HEAD, branch and worktree list are unchanged; scratch is gone.
        assert_eq!(rev(f.project.path()), head_before);
        assert_eq!(
            git(f.project.path(), &["status", "--porcelain"]).unwrap(),
            status_before
        );
        assert_eq!(
            git(f.project.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            branch_before
        );
        let worktrees = git(f.project.path(), &["worktree", "list", "--porcelain"]).unwrap();
        assert_eq!(worktrees.matches("worktree ").count(), 1, "{worktrees}");
        assert_eq!(
            std::fs::read_to_string(f.project.path().join("a.txt")).unwrap(),
            "one\ntwo\nlater\n"
        );

        // Result: identical diff to the baseline, check passes, saved and reportable.
        assert_eq!(out.record.outcome, "pr_ready");
        assert_eq!(out.record.similarity, Some(1.0));
        assert_eq!(out.record.check_passed, Some(true));
        assert_eq!(out.record.cost_usd, 1.0);
        assert_eq!(out.verdict.verdict, Verdict::Equivalent);
        assert!(estimate_lines(&entry, Some(5.0)).contains("$2.00"));
        assert!(out.draft_note.as_deref().unwrap().contains("unapplied"));
        let store = GoldenStore::new(f.project.path());
        assert_eq!(store.list_replays(&entry.id).unwrap().len(), 1);

        let text = render_report(&[(entry.clone(), store.list_replays(&entry.id).unwrap())]);
        assert!(text.contains("no-optimizer"), "{text}");
        assert!(text.contains("-50%"), "{text}");
        assert!(text.contains("equivalent"), "{text}");
        assert!(text.contains("one sample"), "{text}");
        assert!(!text.contains("Needs human review"), "{text}");
    }

    #[test]
    fn replay_that_drifts_from_baseline_is_listed_for_review() {
        let f = fixture();
        let entry = captured_entry(&f, None);
        let head = rev(f.project.path());
        let seen = Default::default();
        let mut run = fake_run(&f, "one\ntwo\nentirely different\n", head, seen);
        let out = replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &[],
            None,
            None,
            &mut run,
        )
        .unwrap();
        assert!(out.record.tag.starts_with("replay-"));
        assert_ne!(out.verdict.verdict, Verdict::Equivalent);
        assert!(out.verdict.needs_human_review());

        let store = GoldenStore::new(f.project.path());
        let text = render_report(&[(entry.clone(), store.list_replays(&entry.id).unwrap())]);
        assert!(
            text.contains("Needs human review (never auto-accepted)"),
            "{text}"
        );
        assert!(text.contains(&entry.id));
    }

    #[test]
    fn replay_with_failing_check_is_failed_even_when_diff_matches() {
        let f = fixture();
        let entry = captured_entry(&f, Some("exit 0"));
        // Same stored entry, but the check now fails in the replay workspace.
        let mut failing = entry.clone();
        failing.check_command = Some("exit 3".into());
        let dir = GoldenStore::new(f.project.path())
            .entry_dir(&entry.id)
            .unwrap();
        std::fs::write(
            dir.join("entry.json"),
            serde_json::to_string(&failing).unwrap(),
        )
        .unwrap();

        let head = rev(f.project.path());
        let mut run = fake_run(&f, "one\n2\n", head, Default::default());
        let out = replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &[],
            Some("t"),
            None,
            &mut run,
        )
        .unwrap();
        assert_eq!(out.record.similarity, Some(1.0));
        assert_eq!(out.record.check_passed, Some(false));
        assert_eq!(out.verdict.verdict, Verdict::Failed);
    }

    #[test]
    fn replay_that_creates_no_goal_is_recorded_as_failed() {
        let f = fixture();
        let entry = captured_entry(&f, None);
        let mut run = |_: &mut Command| Ok(ok_status());
        let out = replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &[],
            Some("t"),
            None,
            &mut run,
        )
        .unwrap();
        assert_eq!(out.record.goal_id, None);
        assert_eq!(out.verdict.verdict, Verdict::Failed);
        assert!(
            out.verdict.reasons[0].contains("created no goal"),
            "{:?}",
            out.verdict.reasons
        );
        // Scratch worktree removed even on this path.
        let worktrees = git(f.project.path(), &["worktree", "list", "--porcelain"]).unwrap();
        assert_eq!(worktrees.matches("worktree ").count(), 1);
    }

    #[test]
    fn replay_validates_optimizer_slot_selection_before_launching() {
        let f = fixture();
        let entry = captured_entry(&f, None);
        let launched = std::cell::Cell::new(0);
        let mut run = |_: &mut Command| {
            launched.set(launched.get() + 1);
            Ok(ok_status())
        };
        let e = replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &["optimizers.input.use=rkt".into()],
            None,
            None,
            &mut run,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(
            e.contains("'rkt'") && e.contains("none, rtk, headroom"),
            "{e}"
        );
        assert_eq!(
            launched.get(),
            0,
            "a bad slot name must not launch a replay"
        );
    }

    #[test]
    fn replay_rejects_bad_input_before_launching_anything() {
        let f = fixture();
        let entry = captured_entry(&f, None);
        let mut launched = false;
        let mut run = |_: &mut Command| {
            launched = true;
            Ok(ok_status())
        };
        let e = replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &["nokeyvalue".into()],
            None,
            None,
            &mut run,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(e.contains("key=value"), "{e}");
        let e = replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &[],
            None,
            Some(-1.0),
            &mut run,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(e.contains("--max-usd"), "{e}");
        let e = replay(
            &f.config,
            Path::new("ta"),
            "missing-entry",
            &[],
            None,
            None,
            &mut run,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(e.contains("ta golden list"), "{e}");
        let e = replay(
            &f.config,
            Path::new("ta"),
            "../escape",
            &[],
            None,
            None,
            &mut run,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(e.contains("not a valid golden id"), "{e}");

        // A commit that is not in the repository.
        let mut bad = entry.clone();
        bad.start_commit = "0".repeat(40);
        let dir = GoldenStore::new(f.project.path())
            .entry_dir(&entry.id)
            .unwrap();
        std::fs::write(dir.join("entry.json"), serde_json::to_string(&bad).unwrap()).unwrap();
        let e = replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &[],
            None,
            None,
            &mut run,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(e.contains("not in this repository"), "{e}");
        assert!(!launched);
    }

    #[test]
    fn replay_refuses_a_prompt_that_no_longer_matches_its_recorded_hash() {
        let f = fixture();
        let entry = captured_entry(&f, None);
        let store = GoldenStore::new(f.project.path());
        let prompt_path = store.prompt_path(&entry.id).unwrap();
        std::fs::write(&prompt_path, "A different task than the one captured\n").unwrap();

        let mut launched = false;
        let mut run = |_: &mut Command| {
            launched = true;
            Ok(ok_status())
        };
        let e = replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &[],
            None,
            None,
            &mut run,
        )
        .err()
        .expect("a tampered prompt must be refused")
        .to_string();
        assert!(e.contains("no longer matches the hash"), "{e}");
        assert!(e.contains("ta golden capture"), "{e}");
        assert!(
            !launched,
            "nothing may be launched with an unverified prompt"
        );
        assert!(
            store
                .list_replays(&entry.id)
                .map(|r| r.is_empty())
                .unwrap_or(true),
            "a refused replay must not leave a record"
        );
    }

    #[test]
    fn estimate_mentions_baseline_and_ceiling() {
        let f = fixture();
        let entry = captured_entry(&f, None);
        assert!(estimate_lines(&entry, None).contains("about $2.00"));
        assert!(estimate_lines(&entry, None).contains("--max-usd"));
        assert!(estimate_lines(&entry, Some(1.0)).contains("below the baseline"));
        assert!(estimate_lines(&entry, Some(9.0)).contains("stops at $9.00"));
        let mut free = entry.clone();
        free.baseline.cost_usd = 0.0;
        assert!(estimate_lines(&free, None).contains("unknown"));
    }

    #[test]
    fn list_shows_cost_age_and_replay_count() {
        let f = fixture();
        let entry = captured_entry(&f, None);
        let store = GoldenStore::new(f.project.path());
        let (entries, _) = store.list().unwrap();
        let later = entry.baseline.captured_at + chrono::Duration::days(3);
        let text = render_list(&entries, &store.replay_counts(), later);
        assert!(text.contains(&entry.id), "{text}");
        assert!(text.contains("$2.00"), "{text}");
        assert!(text.contains("3d"), "{text}");
        assert!(render_list(&[], &Default::default(), Utc::now()).contains("ta golden capture"));
    }

    #[test]
    fn draft_paths_picks_newest_package_and_skips_unsafe_paths() {
        let f = fixture();
        let g = finished_goal(&f, "Paths");
        let newer = serde_json::json!({
            "created_at": "2026-10-15T00:00:00Z",
            "goal": {"goal_id": g.goal_run_id.to_string()},
            "changes": {"artifacts": [
                {"resource_uri": "fs://workspace/keep.rs"},
                {"resource_uri": "fs://workspace/../escape"},
                {"resource_uri": "fs://workspace/.ta/state.json"},
                {"resource_uri": "gmail://thread/1"},
            ]},
        });
        std::fs::write(
            f.config.pr_packages_dir.join("newer.json"),
            newer.to_string(),
        )
        .unwrap();
        std::fs::write(f.config.pr_packages_dir.join("broken.json"), "{not json").unwrap();
        let paths = draft_paths(&f.config, &g.goal_run_id.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(paths, vec!["keep.rs".to_string()]);
        assert!(draft_paths(&f.config, "no-such-goal").unwrap().is_none());
    }

    #[test]
    fn line_endings_do_not_make_a_replay_diff_look_different() {
        // A Windows checkout (autocrlf) versus an LF file: same content, same diff.
        let lf = text_or_marker(b"one\ntwo\n");
        let crlf = text_or_marker(b"one\r\ntwo\r\n");
        assert_eq!(lf, crlf);
        let same = build_diff(
            &["a.txt".to_string()],
            |_| Some(crlf.clone()),
            |_| Some(lf.clone()),
        );
        assert!(same.is_empty(), "no change expected, got: {same}");
        // A real edit still shows, whichever ending the old side used.
        let changed = build_diff(
            &["a.txt".to_string()],
            |_| Some(text_or_marker(b"one\r\ntwo\r\n")),
            |_| Some(text_or_marker(b"one\n2\n")),
        );
        assert!(
            changed.contains("-two") && changed.contains("+2"),
            "{changed}"
        );
        // Binary content is still a hash marker, not altered by the normalisation.
        assert!(text_or_marker(&[0xff, 0xfe, 0x00]).starts_with("<binary file sha256:"));
    }

    #[test]
    fn helpers_format_money_deltas_and_age() {
        assert_eq!(fmt_cost(0.0, false), "n/a");
        assert_eq!(fmt_cost(1.234, true), "$1.23~");
        assert_eq!(delta_pct(2.0, 1.0), "-50%");
        assert_eq!(delta_pct(0.0, 1.0), "n/a");
        let now = Utc::now();
        assert_eq!(age(now, now - chrono::Duration::minutes(5)), "5m");
        assert_eq!(age(now, now - chrono::Duration::hours(7)), "7h");
        assert_eq!(truncate("abcdefghij", 5), "abcd~");
    }
}
