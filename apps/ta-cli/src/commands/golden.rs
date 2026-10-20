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
    arm_stats, default_entry_id, diff_similarity, diff_stats, parse_overrides, recompute_entry,
    render_file_diff, sha256_hex, verdict, ArmStats, Baseline, CheckRun, GoldenEntry, GoldenStore,
    RecomputeOutcome, ReplayRecord, RunEnvironment, SeriesStats, Verdict, VerdictResult,
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
    /// Record what a baseline actually ran under (optimizers, tool versions), declared by you.
    ///
    /// Entries captured before environments were recorded have none, and a baseline without
    /// one cannot be compared fairly. Use the settings you know the original goal ran with.
    Annotate {
        /// Golden entry id.
        id: String,
        /// Input optimizer slot the baseline ran with: none, rtk, headroom, ...
        #[arg(long)]
        input: String,
        /// Context optimizer slot (default none).
        #[arg(long, default_value = "none")]
        context: String,
        /// Output optimizer slot (default none).
        #[arg(long, default_value = "none")]
        output: String,
        /// Free text, e.g. "all optimizers on per owner; unverified".
        #[arg(long)]
        note: String,
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
        /// Keep the replay's goal, draft and staging for inspection. By default they
        /// are denied and deleted once the result is recorded.
        #[arg(long)]
        keep: bool,
    },
    /// Recompute files, lines and similarity of an entry's baseline and replays from their
    /// stored diffs, into new fields (the original numbers are never overwritten).
    ///
    /// Older entries stored a changed-lines-only diff whose counts were inflated for files
    /// with large test modules. Those cannot be read exactly: they are flagged, not guessed,
    /// unless you pass --reconstruct, which rebuilds the real changes from the changed lines
    /// and labels the result as reconstructed.
    Recompute {
        /// Golden entry id.
        id: String,
        /// Print what would change and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// For diffs without context, rebuild the real changes from the stored changed lines.
        #[arg(long)]
        reconstruct: bool,
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
        GoldenCommands::Annotate {
            id,
            input,
            context,
            output,
            note,
        } => {
            let store = GoldenStore::new(&config.workspace_root);
            let mut entry = store.load_entry(id)?;
            let mut env = entry.baseline_environment.clone().unwrap_or_default();
            env.provenance = format!("declared by owner, not measured: {note}");
            env.input_slot = input.clone();
            env.context_slot = context.clone();
            env.output_slot = output.clone();
            env.compression_enabled = input != "none";
            env.note = Some(note.clone());
            entry.baseline_environment = Some(env);
            store.update_entry(&entry)?;
            println!(
                "Recorded the baseline environment for '{id}': input={input} context={context} output={output}. \
                 It is marked as declared, not measured. Verify it with a replay under the same settings."
            );
            Ok(())
        }
        GoldenCommands::List { json } => list(config, *json),
        GoldenCommands::Replay {
            id,
            set,
            tag,
            max_usd,
            keep,
        } => {
            let ta_bin = std::env::current_exe().context(
                "could not find the running `ta` binary to launch the replay; \
                 run `ta golden replay` from an installed ta",
            )?;
            let mut run = |cmd: &mut Command| cmd.status();
            let outcome = replay(config, &ta_bin, id, set, tag.as_deref(), *max_usd, &mut run)?;
            print!("{}", render_replay(&outcome));
            if !*keep {
                if let Some(goal_id) = &outcome.record.goal_id {
                    discard_replay_goal(&ta_bin, goal_id);
                }
            }
            Ok(())
        }
        GoldenCommands::Recompute {
            id,
            dry_run,
            reconstruct,
        } => {
            let store = GoldenStore::new(&config.workspace_root);
            let items = recompute_entry(&store, id, *reconstruct, *dry_run)?;
            print!("{}", render_recompute(id, &items, *dry_run, *reconstruct));
            Ok(())
        }
        GoldenCommands::Report { ids, json } => report(config, ids, *json),
    }
}

/// Deny the replay's draft and delete its goal and staging. The result is already recorded
/// under `.ta/golden/<id>/replays/`, and the draft is a throwaway, so leaving it would only
/// pile up reviews and gigabytes of staging. Failures are reported with the command to retry.
fn discard_replay_goal(ta_bin: &Path, full_goal_id: &str) {
    // `ta draft deny` resolves a goal by its short prefix, not its full UUID.
    let goal_id = &full_goal_id[..full_goal_id.len().min(8)];
    let steps: [(&str, Vec<&str>); 2] = [
        (
            "deny the replay draft",
            vec![
                "draft",
                "deny",
                goal_id,
                "--reason",
                "golden replay result recorded; draft is a throwaway",
                "--accept-terms",
            ],
        ),
        (
            "delete the replay goal and staging",
            vec![
                "goal",
                "delete",
                goal_id,
                "--reason",
                "golden replay finished",
                "--accept-terms",
            ],
        ),
    ];
    for (what, args) in steps {
        match Command::new(ta_bin).args(&args).output() {
            Ok(o) if o.status.success() => println!("Cleanup: {what} ({}): done.", short(goal_id)),
            Ok(o) => println!(
                "Cleanup: could not {what} ({}): {}. Retry: ta {}",
                short(goal_id),
                String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .last()
                    .unwrap_or("no error text"),
                args.join(" ")
            ),
            Err(e) => println!(
                "Cleanup: could not {what}: {e}. Retry: ta {}",
                args.join(" ")
            ),
        }
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

    let mut check_run: Option<CheckRun> = None;
    let check_passed = match check {
        Some(_) if result_commit.is_some() => None,
        Some(cmd) => match run_check(cmd, &goal.workspace_path) {
            Ok(run) => {
                let pass = run.passed;
                check_run = Some(run);
                Some(pass)
            }
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
        baseline_environment: Some(collect_environment(
            config,
            None,
            "measured at capture, after the run finished, so it may differ from the run's own settings",
        )),
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
            check_run,
            recomputed: None,
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
        .arg("--new-phase")
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
    let run_index = store.next_run_index(id)?;
    let run_started_at = Utc::now();
    let status = runner(&mut cmd);
    let run_finished_at = Utc::now();
    let status = status.map_err(|e| {
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
        // Wall clock of the whole replay command (agent run included), not of this bookkeeping.
        started_at: run_started_at,
        completed_at: Some(run_finished_at),
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
        lines_added: None,
        lines_removed: None,
        diff_sha256: None,
        similarity: None,
        check_passed: None,
        check_run: None,
        run_index: Some(run_index),
        recomputed: None,
        failure: None,
        environment: Some(collect_environment(
            config,
            Some(&overrides),
            "measured at replay start",
        )),
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
                    record.lines_added = Some(stats.added);
                    record.lines_removed = Some(stats.removed);
                    record.diff_sha256 = Some(sha256_hex(&d));
                    record.similarity = Some(diff_similarity(&store.load_baseline_diff(id)?, &d));
                    diff = Some(d);
                    if let Some(cmd) = &entry.check_command {
                        match run_check(cmd, &goal.workspace_path) {
                            Ok(run) => {
                                record.check_passed = Some(run.passed);
                                record.check_run = Some(run);
                            }
                            Err(e) => {
                                record.failure = Some(format!("the check could not be run: {e}"))
                            }
                        }
                    }
                    draft_note = Some(format!(
                        "The replay's draft is never applied. It is denied and deleted after the result is recorded unless you pass --keep (goal {}).",
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
                serde_json::json!({
                    "entry": e,
                    "replays": rows,
                    "arms": arm_stats(replays),
                    "note": COST_NOTE,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", render_report(&sections));
    }
    Ok(())
}

/// `+a -r` of a replay's diff and whether the numbers can be trusted.
fn replay_lines(r: &ReplayRecord) -> String {
    if let Some(rc) = &r.recomputed {
        return format!(
            "{} file(s) +{} -{} (recomputed: {})",
            rc.files_changed,
            rc.lines_added,
            rc.lines_removed,
            rc.method.split(';').next().unwrap_or("")
        );
    }
    match (r.lines_added, r.lines_removed) {
        (Some(a), Some(d)) => format!("{} file(s) +{a} -{d}", r.files_changed),
        _ => format!(
            "{} file(s), line counts not recorded (older differ, unreliable; run `ta golden recompute {}`)",
            r.files_changed, r.golden_id
        ),
    }
}

fn baseline_lines(b: &Baseline, id: &str) -> String {
    match &b.recomputed {
        Some(rc) => format!(
            "{} file(s) +{} -{} (recomputed: {}; recorded at capture: {} file(s) +{} -{})",
            rc.files_changed,
            rc.lines_added,
            rc.lines_removed,
            rc.method.split(';').next().unwrap_or(""),
            b.files_changed,
            b.lines_added,
            b.lines_removed
        ),
        None => format!(
            "{} file(s) +{} -{} (as recorded at capture; if captured before the diff fix these are inflated, see `ta golden recompute {id}`)",
            b.files_changed, b.lines_added, b.lines_removed
        ),
    }
}

fn check_run_line(r: &ReplayRecord) -> String {
    match &r.check_run {
        Some(c) => {
            let tests = c
                .tests
                .map(|t| {
                    format!(
                        ", tests {} passed {} failed {} ignored",
                        t.passed, t.failed, t.ignored
                    )
                })
                .unwrap_or_default();
            format!(
                "{} (exit {}, {}s, `{}`{})",
                if c.passed { "pass" } else { "FAIL" },
                c.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into()),
                (c.finished_at - c.started_at).num_seconds(),
                truncate(&c.command, 60),
                tests
            )
        }
        None => match r.check_passed {
            Some(true) => "pass (no run record)".to_string(),
            Some(false) => "FAIL (no run record)".to_string(),
            None => "not run".to_string(),
        },
    }
}

fn fmt_tokens(s: &SeriesStats) -> String {
    format!(
        "mean {:.0}, min {:.0}, max {:.0}, n={}",
        s.mean, s.min, s.max, s.n
    )
}

fn fmt_cost_series(s: &SeriesStats) -> String {
    format!(
        "mean ${:.2}, min ${:.2}, max ${:.2}, n={}",
        s.mean, s.min, s.max, s.n
    )
}

fn render_arms(arms: &[ArmStats]) -> String {
    let mut out = String::from("  Per arm (tokens are the primary metric, cost is secondary):\n");
    for a in arms {
        out.push_str(&format!("    {}  [labels: {}]\n", a.arm, a.tags.join(", ")));
        match &a.tokens {
            Some(s) => {
                out.push_str(&format!("      total tokens: {}\n", fmt_tokens(s)));
                if s.n == 1 {
                    out.push_str(
                        "      n=1, not a distribution: one run says nothing about spread.\n",
                    );
                }
            }
            None => out.push_str("      total tokens: n/a\n"),
        }
        if let Some(s) = &a.output_tokens {
            out.push_str(&format!("      output tokens: {}\n", fmt_tokens(s)));
        }
        match &a.cost_usd {
            Some(s) => {
                out.push_str(&format!("      cost: {}\n", fmt_cost_series(s)));
                if s.n == 1 {
                    out.push_str("      n=1, not a distribution.\n");
                }
            }
            None => out.push_str("      cost: n/a (none recorded)\n"),
        }
    }
    out
}

const COST_NOTE: &str = "Note: cost_usd is as reported by the agent runtime. It does not reconcile with the \
token counts at any fixed per-token prices (cache pricing, plan discounts and runtime accounting differ), \
so compare tokens first and treat cost as a secondary signal.";

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
            "{} ({})\n  Baseline: {} tokens (in {} out {} cache-read {} cache-create {}), {}, outcome {}, check {}\n  Baseline diff: {}\n",
            e.id,
            e.title,
            base_tokens,
            b.input_tokens,
            b.output_tokens,
            b.cache_read_tokens,
            b.cache_creation_tokens,
            fmt_cost(b.cost_usd, b.cost_estimated),
            b.outcome,
            check_label(e.check_command.as_deref(), b.check_passed),
            baseline_lines(b, &e.id),
        ));
        out.push_str(&match &e.baseline_environment {
            Some(env) => format!("  Baseline ran under: {}\n", env.summary()),
            None => "  Baseline ran under: NOT RECORDED. Not a valid comparison until `ta golden annotate` records it.\n".to_string(),
        });
        if replays.is_empty() {
            out.push_str(&format!(
                "  No replays yet. Try: ta golden replay {} --set key=value\n\n",
                e.id
            ));
            continue;
        }
        for r in replays {
            let v = verdict(e, r);
            let tokens = total_tokens(
                r.input_tokens,
                r.output_tokens,
                r.cache_read_tokens,
                r.cache_creation_tokens,
            );
            out.push_str(&format!(
                "  Run {} '{}' ({}): {}\n",
                r.run_index
                    .map(|i| format!("#{i}"))
                    .unwrap_or_else(|| "(no index)".into()),
                r.tag,
                short(&r.replay_id),
                v.verdict.as_str()
            ));
            out.push_str(&format!(
                "    tokens:  {} total ({} vs baseline): in {} / out {} / cache-read {} / cache-create {}\n",
                tokens,
                delta_pct(base_tokens as f64, tokens as f64),
                r.input_tokens,
                r.output_tokens,
                r.cache_read_tokens,
                r.cache_creation_tokens
            ));
            out.push_str(&format!(
                "    cost:    {} ({} vs baseline, as reported by the runtime)\n",
                fmt_cost(r.cost_usd, r.cost_estimated),
                delta_pct(b.cost_usd, r.cost_usd)
            ));
            out.push_str(&format!(
                "    outcome: {}\n    check:   {}\n    diff:    {}, similarity {}\n",
                r.outcome,
                check_run_line(r),
                replay_lines(r),
                match (
                    r.recomputed.as_ref().and_then(|c| c.similarity),
                    r.similarity
                ) {
                    (Some(s), Some(orig)) => format!(
                        "{:.0}% (recomputed; {:.0}% as originally recorded)",
                        s * 100.0,
                        orig * 100.0
                    ),
                    (Some(s), None) => format!("{:.0}% (recomputed)", s * 100.0),
                    (None, Some(s)) => format!("{:.0}%", s * 100.0),
                    (None, None) => "n/a".to_string(),
                }
            ));
            out.push_str(&format!(
                "    when:    {}{}\n",
                r.started_at.format("%Y-%m-%d %H:%M:%S UTC"),
                r.completed_at
                    .map(|c| format!(" to {}", c.format("%H:%M:%S UTC")))
                    .unwrap_or_default()
            ));
            if let Some(env) = &r.environment {
                out.push_str(&format!("    ran under: {}\n", env.summary()));
            }
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
        out.push_str(&render_arms(&arm_stats(replays)));
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
    out.push_str(COST_NOTE);
    out.push('\n');
    if single_sample {
        out.push_str(&format!(
            "One replay is one sample (n=1, not a distribution): agent runs vary, so treat a difference as a lead and repeat \
             the replay before acting on a borderline result. Equivalent means check passes (when one is \
             defined) and diff similarity is at least {:.0}%.\n",
            EQUIVALENCE_THRESHOLD * 100.0
        ));
    }
    out
}

fn render_recompute(
    id: &str,
    items: &[ta_goal::golden::RecomputeItem],
    dry_run: bool,
    reconstruct: bool,
) -> String {
    let mut out = format!(
        "Recompute for '{id}'{}\n",
        if dry_run {
            " (dry run, nothing written)"
        } else {
            ""
        }
    );
    let (mut changed, mut flagged, mut same) = (0, 0, 0);
    for i in items {
        let (f, a, r, s) = i.original;
        let orig = format!(
            "{} file(s) +{} -{}{}",
            f,
            a.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            r.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            s.map(|s| format!(", similarity {:.0}%", s * 100.0))
                .unwrap_or_default()
        );
        match &i.outcome {
            RecomputeOutcome::Recomputed(n) => {
                changed += 1;
                out.push_str(&format!(
                    "  {}: {} -> {} file(s) +{} -{}{}\n    method: {}\n",
                    i.subject,
                    orig,
                    n.files_changed,
                    n.lines_added,
                    n.lines_removed,
                    n.similarity
                        .map(|s| format!(", similarity {:.0}%", s * 100.0))
                        .unwrap_or_default(),
                    n.method
                ));
            }
            RecomputeOutcome::Unchanged => {
                same += 1;
                out.push_str(&format!("  {}: already recomputed, unchanged\n", i.subject));
            }
            RecomputeOutcome::Flagged(why) => {
                flagged += 1;
                out.push_str(&format!(
                    "  {}: NOT recomputed ({orig} stays as recorded). {why}\n",
                    i.subject
                ));
            }
        }
    }
    out.push_str(&format!(
        "{changed} {}, {same} unchanged, {flagged} flagged. The original numbers are never overwritten; \
         recomputed values are stored beside them in a `recomputed` field.\n",
        if dry_run { "would change" } else { "updated" }
    ));
    if flagged > 0 && !reconstruct {
        out.push_str(&format!(
            "Flagged diffs have no context lines. To estimate their true counts from the stored \
             changed lines: ta golden recompute {id} --reconstruct (add --dry-run to preview).\n"
        ));
    }
    out
}

fn tool_version(bin: &str) -> String {
    Command::new(bin)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// The settings a run happens under: resolved optimizer slots (with `overrides` applied),
/// tool versions, proxy URL and the Claude Code compaction window. No secrets are read.
fn collect_environment(
    config: &GatewayConfig,
    overrides: Option<&Value>,
    provenance: &str,
) -> RunEnvironment {
    let cfg = super::compression::load_optimizers(&config.workspace_root).with_overrides(overrides);
    let slot = |s: &ta_context_opt::SlotSelection| {
        s.use_.clone().unwrap_or_else(|| "not chosen".to_string())
    };
    let daemon_toml = config.workspace_root.join(".ta").join("daemon.toml");
    let claude_settings = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".claude").join("settings.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let input = slot(&cfg.input);
    RunEnvironment {
        provenance: provenance.to_string(),
        ta_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_code_version: tool_version("claude"),
        rtk_version: tool_version("rtk"),
        headroom_version: tool_version("headroom"),
        compression_enabled: input != "none" && input != "not chosen",
        input_slot: input,
        context_slot: slot(&cfg.context),
        output_slot: slot(&cfg.output),
        anthropic_base_url: std::env::var("ANTHROPIC_BASE_URL").ok(),
        auto_compact_window: claude_settings
            .as_ref()
            .and_then(|v| v.get("autoCompactWindow"))
            .and_then(Value::as_u64),
        daemon_toml_sha256: std::fs::read_to_string(daemon_toml)
            .ok()
            .map(|t| sha256_hex(&t)),
        note: None,
    }
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

/// Run a check command in `dir` and return the full record of the run: the command line,
/// start and finish times, exit code, bounded output tails and parsed test counts. An `Err`
/// means the command could not be started at all.
fn run_check(cmd: &str, dir: &Path) -> anyhow::Result<CheckRun> {
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
    let started_at = Utc::now();
    let out = c.current_dir(dir).output().map_err(|e| {
        anyhow::anyhow!(
            "could not start `{cmd}` in {}: {e}. Run it by hand to see why.",
            dir.display()
        )
    })?;
    let run = CheckRun::from_output(
        cmd,
        started_at,
        Utc::now(),
        out.status.code(),
        &out.stdout,
        &out.stderr,
    );
    tracing::info!(command = %cmd, exit_code = ?run.exit_code, passed = run.passed, "golden check finished");
    Ok(run)
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
            diff.contains("--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1,1 @@\n+new\n"),
            "{diff}"
        );
        let e = store.load_entry(&c.entry.id).unwrap();
        let run = e.baseline.check_run.clone().expect("check run is recorded");
        assert_eq!(run.command, "exit 0");
        assert_eq!(run.exit_code, Some(0));
        assert!(run.passed && run.finished_at >= run.started_at);
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
            diff.contains("--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1,1 @@\n+new\n"),
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

    #[test]
    fn capture_records_a_baseline_environment_and_annotate_marks_it_declared() {
        let f = fixture();
        let entry = captured_entry(&f, None);
        let env = entry
            .baseline_environment
            .as_ref()
            .expect("capture records one");
        assert!(env.provenance.starts_with("measured at capture"));
        let store = GoldenStore::new(&f.config.workspace_root);
        execute(
            &GoldenCommands::Annotate {
                id: entry.id.clone(),
                input: "rtk".into(),
                context: "agent-native".into(),
                output: "none".into(),
                note: "all on per owner".into(),
            },
            &f.config,
        )
        .unwrap();
        let after = store
            .load_entry(&entry.id)
            .unwrap()
            .baseline_environment
            .unwrap();
        assert_eq!(after.input_slot, "rtk");
        assert!(after.compression_enabled);
        assert!(after
            .provenance
            .starts_with("declared by owner, not measured"));
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
        let env = out
            .record
            .environment
            .as_ref()
            .expect("replay records its environment");
        assert_eq!(env.provenance, "measured at replay start");
        assert!(!env.ta_version.is_empty());
        assert_eq!(out.record.similarity, Some(1.0));
        assert_eq!(out.record.check_passed, Some(true));
        assert_eq!(out.record.cost_usd, 1.0);
        assert_eq!(out.verdict.verdict, Verdict::Equivalent);
        assert!(estimate_lines(&entry, Some(5.0)).contains("$2.00"));
        assert!(out.draft_note.as_deref().unwrap().contains("never applied"));
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
    fn two_replays_each_keep_their_own_check_result_in_their_own_record() {
        let f = fixture();
        let entry = captured_entry(&f, Some("echo checking; echo oops >&2; exit 0"));
        let head = rev(f.project.path());
        let store = GoldenStore::new(f.project.path());
        let mut outs = Vec::new();
        for tag in ["arm-a", "arm-a"] {
            // The same label twice: the old label-keyed result would have collided.
            let mut run = fake_run(&f, "one\n2\n", head.clone(), Default::default());
            outs.push(
                replay(
                    &f.config,
                    Path::new("ta"),
                    &entry.id,
                    &[],
                    Some(tag),
                    None,
                    &mut run,
                )
                .unwrap(),
            );
        }
        let records = store.list_replays(&entry.id).unwrap();
        assert_eq!(records.len(), 2);
        assert_ne!(records[0].replay_id, records[1].replay_id);
        assert_eq!(
            records.iter().map(|r| r.run_index).collect::<Vec<_>>(),
            vec![Some(1), Some(2)]
        );
        for r in &records {
            let c = r
                .check_run
                .as_ref()
                .expect("check run persisted in the record");
            assert!(c.command.contains("echo checking"));
            assert_eq!(c.exit_code, Some(0));
            assert!(c.passed && c.stdout_tail.contains("checking"));
            assert!(c.stderr_tail.contains("oops"));
            assert!(c.finished_at >= c.started_at);
            assert_eq!((r.lines_added, r.lines_removed), (Some(2), Some(1)));
            assert!(r.completed_at.unwrap() >= r.started_at);
        }
        // The first record is exactly what was written; the second run did not touch it.
        let first = std::fs::read_to_string(&outs[0].record_path).unwrap();
        assert!(first.contains(&records[0].replay_id));
        assert!(!first.contains(&records[1].replay_id));
    }

    #[test]
    fn a_failing_check_is_recorded_with_its_exit_code_and_output() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_check(
            "echo bad; echo 'test result: FAILED. 1 passed; 2 failed; 0 ignored;'; exit 3",
            dir.path(),
        )
        .unwrap();
        assert_eq!(run.exit_code, Some(3));
        assert!(!run.passed);
        assert!(run.stdout_tail.contains("bad"));
        assert_eq!(run.tests.map(|t| (t.passed, t.failed)), Some((1, 2)));
    }

    #[test]
    fn report_shows_every_token_kind_check_lines_similarity_and_arm_stats() {
        let f = fixture();
        let entry = captured_entry(&f, Some("exit 0"));
        let head = rev(f.project.path());
        let mut run = fake_run(&f, "one\n2\n", head, Default::default());
        replay(
            &f.config,
            Path::new("ta"),
            &entry.id,
            &[],
            Some("solo"),
            None,
            &mut run,
        )
        .unwrap();
        let store = GoldenStore::new(f.project.path());
        let mut replays = store.list_replays(&entry.id).unwrap();
        replays[0].cache_read_tokens = 777;
        replays[0].cache_creation_tokens = 333;
        let text = render_report(&[(entry.clone(), replays.clone())]);
        assert!(text.contains("cache-read 777"), "{text}");
        assert!(text.contains("cache-create 333"), "{text}");
        assert!(text.contains("out 900"), "{text}");
        assert!(text.contains("check:   pass (exit 0"), "{text}");
        assert!(text.contains("+2 -1"), "{text}");
        assert!(text.contains("similarity 100%"), "{text}");
        assert!(text.contains("Run #1"), "{text}");
        assert!(
            text.contains("mean") && text.contains("min") && text.contains("max"),
            "{text}"
        );
        assert!(text.contains("n=1, not a distribution"), "{text}");
        assert!(
            text.contains("does not reconcile with the token counts"),
            "{text}"
        );

        // Two more samples in the same arm: n=3 and no single-sample warning for the arm.
        let mut more = replays.clone();
        for i in 0..2 {
            let mut r = replays[0].clone();
            r.replay_id = format!("extra-{i}");
            r.output_tokens = 1000 + i as u64 * 100;
            more.push(r);
        }
        let text = render_report(&[(entry, more)]);
        assert!(text.contains("n=3"), "{text}");
    }

    #[test]
    fn recompute_command_renders_dry_run_and_flags_old_diffs() {
        let f = fixture();
        let entry = captured_entry(&f, None);
        let store = GoldenStore::new(f.project.path());
        // Replace the baseline diff with an old-format one.
        std::fs::write(
            store.entry_dir(&entry.id).unwrap().join("baseline.diff"),
            "--- a/a.txt\n+++ b/a.txt\n-two\n+2\n",
        )
        .unwrap();
        let items = recompute_entry(&store, &entry.id, false, true).unwrap();
        let text = render_recompute(&entry.id, &items, true, false);
        assert!(text.contains("dry run, nothing written"), "{text}");
        assert!(text.contains("NOT recomputed"), "{text}");
        assert!(text.contains("--reconstruct"), "{text}");
        let items = recompute_entry(&store, &entry.id, true, true).unwrap();
        let text = render_recompute(&entry.id, &items, true, true);
        assert!(text.contains("would change"), "{text}");
        assert!(text.contains("reconstructed"), "{text}");
        assert!(store
            .load_entry(&entry.id)
            .unwrap()
            .baseline
            .recomputed
            .is_none());
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
