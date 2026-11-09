// release_set.rs — `ta release-set`: one open release per project, cut by ONE pull request.
//
// `ta release` (release.rs) is the publish pipeline: version bump, tag, GitHub
// release, artifacts. `ta release-set` is the branch discipline *before* that:
// it decides which branch every goal commits onto and how the work reaches
// main. Without an open release, every `ta draft apply` makes its own branch and
// PR, and every PR runs CI. With one:
//
//   ta release-set start 0.18 --goal "..."    opens release/0.18 from main
//   ta run / ta draft apply                   commit onto release/0.18, no PR, no push
//   ta release-set cut                        merge main, run the gate, push once, ONE PR
//
// The state lives in `.ta/release-set.json` (ta-goal `release_set`); the routing
// rule (hotfix > open release > configured target) lives in ta-submit `routing`.
// This file is the CLI, the git/gh work of start and cut, and the goal-start and
// apply-time hooks.

use std::cell::RefCell;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use ta_goal::release_set::{
    assess_goal, attach_to_umbrella, complete_umbrella, create_umbrella_goal, validate_name,
    GoalReleaseInfo, HotfixGoal, LandedGoal, NewRelease, ReleaseSet, ReleaseSetStore,
};
use ta_goal::{GoalRun, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;
use ta_submit::routing::{resolve_route, RouteKind, SubmitRoute};
use uuid::Uuid;

#[derive(Subcommand, Debug)]
pub enum ReleaseSetCommands {
    /// Open the project's release: create `release/<name>` from main and make it the
    /// branch every goal commits onto.
    ///
    /// Refuses a dirty working tree and a project that already has an open release.
    /// While the release is open no goal opens its own PR; `ta release-set cut`
    /// opens the one PR to main. This is not `ta release` (the publish pipeline).
    Start {
        /// Release name; the branch is `release/<name>` (e.g. `0.18`, `q3-launch`).
        name: String,
        /// What this release delivers. The release PR is titled from it and every new
        /// goal is checked against it (advice only).
        #[arg(long)]
        goal: String,
        /// Wayfinder goal this release delivers (recorded in `status --json`).
        #[arg(long)]
        wayfinder_goal: Option<String>,
        /// Plan phase that belongs to the release (repeatable). A goal on one of these
        /// phases is on track.
        #[arg(long = "phase", num_args = 1..)]
        phase: Vec<String>,
    },
    /// Show the open release: branch, goal, goals landed, hotfixes, off-track goals and
    /// ahead/behind main.
    Status {
        /// Print the stable JSON document (`{"release": {...}}`) instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Cut the release: merge the latest main into it, run the verify gate, push once
    /// and open ONE pull request to main.
    ///
    /// Refuses (and says which command failed) when the gate fails. Nothing is pushed
    /// until the gate passes.
    Cut,
}

/// `release-set status --json` writes the document to stdout; the terms notice must not.
pub fn prints_json(cmd: &ReleaseSetCommands) -> bool {
    matches!(cmd, ReleaseSetCommands::Status { json: true })
}

pub fn execute(cmd: &ReleaseSetCommands, config: &GatewayConfig) -> Result<()> {
    match cmd {
        ReleaseSetCommands::Start {
            name,
            goal,
            wayfinder_goal,
            phase,
        } => start(config, name, goal, wayfinder_goal.as_deref(), phase),
        ReleaseSetCommands::Status { json } => status(config, *json),
        ReleaseSetCommands::Cut => cut(config),
    }
}

// ── `--hotfix` carried to goal start and apply ───────────────────────────────

thread_local! {
    static CLI_HOTFIX: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Scoped carrier for `ta run --hotfix --hotfix-reason` / `ta draft apply --hotfix`, so the
/// long positional parameter lists of `run::execute` and `apply_package` stay as they are.
pub struct CliHotfixGuard {
    previous: Option<String>,
}

impl CliHotfixGuard {
    pub fn set(reason: Option<String>) -> Self {
        let previous = CLI_HOTFIX.with(|c| c.replace(reason));
        Self { previous }
    }
}

impl Drop for CliHotfixGuard {
    fn drop(&mut self) {
        CLI_HOTFIX.with(|c| *c.borrow_mut() = self.previous.take());
    }
}

/// The reason given with `--hotfix` on this command line, if any.
pub fn cli_hotfix_reason() -> Option<String> {
    CLI_HOTFIX.with(|c| c.borrow().clone())
}

/// Validate the `--hotfix` / `--hotfix-reason` pair and return the reason.
/// `--hotfix` without a reason, and a reason without `--hotfix`, are both refused.
pub fn hotfix_reason_from_flags(hotfix: bool, reason: Option<&str>) -> Result<Option<String>> {
    match (hotfix, reason.map(str::trim)) {
        (false, None) => Ok(None),
        (false, Some(_)) => bail!(
            "--hotfix-reason needs --hotfix. Pass both to send this goal to main as a hotfix, \
             or neither to follow the open release."
        ),
        (true, None) | (true, Some("")) => bail!(
            "--hotfix needs a reason: add --hotfix-reason \"<why this cannot wait for the \
             release>\". The reason is recorded in the release state."
        ),
        (true, Some(r)) => Ok(Some(r.to_string())),
    }
}

// ── git helpers ──────────────────────────────────────────────────────────────

fn git_cmd(root: &Path) -> Command {
    let mut c = Command::new("git");
    // `ta run` points agents at their staging copy with GIT_DIR/GIT_WORK_TREE; these commands
    // always mean the project repository at `root`.
    c.arg("-C")
        .arg(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_CEILING_DIRECTORIES");
    c
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let out = git_cmd(root)
        .args(args)
        .output()
        .with_context(|| "could not run `git` — is it installed and on PATH?")?;
    if !out.status.success() {
        bail!(
            "`git {}` failed in {}: {}",
            args.join(" "),
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_try(root: &Path, args: &[&str]) -> Option<String> {
    git(root, args).ok().filter(|s| !s.is_empty())
}

fn is_ancestor(root: &Path, a: &str, b: &str) -> bool {
    git_cmd(root)
        .args(["merge-base", "--is-ancestor", a, b])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn current_branch(root: &Path) -> Option<String> {
    git_try(root, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD")
}

/// The ref holding the latest main: `<remote>/<main>` when it exists, else local `<main>`.
fn latest_main_ref(root: &Path, remote: &str, main: &str) -> Option<String> {
    let remote_ref = format!("{remote}/{main}");
    if git_try(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote_ref}"),
        ],
    )
    .is_some()
    {
        return Some(remote_ref);
    }
    git_try(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{main}"),
        ],
    )
    .map(|_| main.to_string())
}

/// The commit a new release branches from: the most advanced of local `<main>` and
/// `<remote>/<main>` (they diverging is refused: the release would have to pick a side).
fn start_base(root: &Path, remote: &str, main: &str) -> Result<(String, String)> {
    let local = git_try(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{main}"),
        ],
    );
    let remote_ref = format!("{remote}/{main}");
    let remote_sha = git_try(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote_ref}"),
        ],
    );
    match (local, remote_sha) {
        (Some(l), Some(r)) if l == r => Ok((main.to_string(), l)),
        (Some(l), Some(r)) => {
            if is_ancestor(root, &l, &r) {
                Ok((remote_ref, r))
            } else if is_ancestor(root, &r, &l) {
                eprintln!(
                    "[release-set] Note: local {main} is ahead of {remote_ref}; the release \
                     starts from local {main}, so its PR will also carry those unpushed commits."
                );
                Ok((main.to_string(), l))
            } else {
                bail!(
                    "Local {main} and {remote_ref} have diverged, so there is no single main to \
                     start the release from. Reconcile them first (`git fetch {remote} && git \
                     rebase {remote_ref}` on {main}), then run `ta release-set start` again."
                )
            }
        }
        (Some(l), None) => Ok((main.to_string(), l)),
        (None, Some(r)) => Ok((remote_ref, r)),
        (None, None) => bail!(
            "Neither '{main}' nor '{remote_ref}' exists in this repository, so there is nothing \
             to branch the release from. Create the initial commit on {main} (or set \
             `[submit.git] target_branch` in .ta/workflow.toml to your trunk branch) and retry."
        ),
    }
}

fn ensure_clean(root: &Path, what: &str) -> Result<()> {
    match super::apply_base::dirty_path_count(root) {
        Some(0) => Ok(()),
        Some(n) => bail!(
            "The working tree has {n} uncommitted change(s) (outside .ta/), so {what} is \
             refused. Commit or stash them (`git status` lists them), then retry."
        ),
        None => bail!(
            "{} is not a git working tree (or git could not read its status), so {what} cannot \
             proceed. Release sets are git branches; run this in the project root.",
            root.display()
        ),
    }
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

fn short_id(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

// ── start ────────────────────────────────────────────────────────────────────

pub(crate) fn start(
    config: &GatewayConfig,
    name: &str,
    objective: &str,
    wayfinder_goal: Option<&str>,
    phases: &[String],
) -> Result<()> {
    super::human_auth::require_human_for("release-set start")?;
    let root = config.workspace_root.as_path();
    validate_name(name)?;
    let wf = ta_submit::WorkflowConfig::load_or_default(&root.join(".ta/workflow.toml"));
    let remote = wf.submit.git.remote.clone();
    let main = wf.submit.git.target_branch.clone();

    let store = ReleaseSetStore::new(root);
    if let Some(open) = store.open_release()? {
        bail!(
            "A release is already open: '{}' on branch {} (started {}). Only one release is \
             open per project. Finish it with `ta release-set cut`, or inspect it with \
             `ta release-set status`.",
            open.name,
            open.branch,
            open.created_at.to_rfc3339()
        );
    }
    if git_try(root, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        bail!(
            "{} is not a git working tree. Release sets are git branches; run `ta release-set \
             start` in the project root of a git repository.",
            root.display()
        );
    }
    ensure_clean(root, "starting a release")?;

    let branch = format!("{}{name}", ta_goal::release_set::BRANCH_PREFIX);
    if git_try(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_some()
    {
        bail!(
            "Branch {branch} already exists. Pick another release name, or delete the old \
             branch if it is finished (`git branch -D {branch}`)."
        );
    }

    if let Err(e) = git(root, &["fetch", &remote]) {
        eprintln!(
            "[release-set] Warning: could not fetch '{remote}' ({e}); starting from the local \
             {main}. Run `git fetch {remote}` and `ta release-set status` later to see whether \
             main moved."
        );
    }
    let (base_ref, base_sha) = start_base(root, &remote, &main)?;
    let original = current_branch(root);

    git(root, &["checkout", "-b", &branch, &base_sha]).with_context(|| {
        format!("could not create release branch {branch} from {base_ref}; nothing was changed")
    })?;

    let plan_phases: Vec<String> = phases
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let rs = match store.start(NewRelease {
        name: name.to_string(),
        objective: objective.to_string(),
        wayfinder_goal_id: wayfinder_goal.map(str::to_string),
        plan_phases,
        base_branch: main.clone(),
        base_commit: base_sha.clone(),
    }) {
        Ok(rs) => rs,
        Err(e) => {
            // Undo the branch so a retry starts clean.
            if let Some(o) = &original {
                let _ = git(root, &["checkout", o]);
            }
            let _ = git(root, &["branch", "-D", &branch]);
            return Err(e);
        }
    };

    let goals = GoalRunStore::new(&config.goals_dir)?;
    let umbrella = create_umbrella_goal(&goals, &rs, root, &config.store_dir)?;
    let umbrella_id = umbrella.goal_run_id;
    store.update(|r| {
        r.goal.goal_run_id = Some(umbrella_id);
        Ok(())
    })?;

    println!(
        "Release {} started on {} (from {} @ {}; you are now on it).",
        rs.name,
        rs.branch,
        base_ref,
        short(&base_sha)
    );
    println!("  Goal:           {}", rs.goal.title);
    println!(
        "  Umbrella goal:  {} (shown as an umbrella in `ta goal list`)",
        short_id(umbrella_id)
    );
    println!("  State:          {}", store.path().display());
    println!();
    println!(
        "Every goal now commits onto {} with NO per-goal PR and no push. `ta release-set cut` \
         pushes once and opens the one PR to {main}.",
        rs.branch
    );
    println!(
        "Run goals from this branch (`git switch {}`) so their staging copy contains the \
         release's earlier commits. For an urgent fix that must not wait: \
         `ta run \"...\" --hotfix --hotfix-reason \"...\"`.",
        rs.branch
    );
    Ok(())
}

// ── status ───────────────────────────────────────────────────────────────────

/// (ahead, behind) of the release branch relative to the latest main; `None` when git cannot say.
fn ahead_behind(root: &Path, rs: &ReleaseSet, remote: &str) -> (Option<u64>, Option<u64>) {
    let Some(main_ref) = latest_main_ref(root, remote, &rs.base_branch) else {
        return (None, None);
    };
    let Some(counts) = git_try(
        root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{main_ref}...{}", rs.branch),
        ],
    ) else {
        return (None, None);
    };
    let mut it = counts.split_whitespace();
    let behind = it.next().and_then(|n| n.parse().ok());
    let ahead = it.next().and_then(|n| n.parse().ok());
    (ahead, behind)
}

pub(crate) fn status(config: &GatewayConfig, json: bool) -> Result<()> {
    let root = config.workspace_root.as_path();
    let store = ReleaseSetStore::new(root);
    let Some(rs) = store.load()? else {
        if json {
            println!("{}", serde_json::json!({ "release": null }));
        } else {
            println!(
                "No release has been started in this project. Start one with \
                 `ta release-set start <name> --goal \"...\"`; until then every goal opens its \
                 own PR to main."
            );
        }
        return Ok(());
    };
    let wf = ta_submit::WorkflowConfig::load_or_default(&root.join(".ta/workflow.toml"));
    let remote = wf.submit.git.remote.clone();
    let (ahead, behind) = ahead_behind(root, &rs, &remote);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rs.status_report(ahead, behind))?
        );
        return Ok(());
    }

    println!(
        "Release {} ({})",
        rs.name,
        if rs.is_open() { "open" } else { "cut" }
    );
    println!(
        "  Branch:       {} (from {} @ {})",
        rs.branch,
        rs.base_branch,
        short(&rs.base_commit)
    );
    println!("  Goal:         {}", rs.goal.title);
    println!("  Objective:    {}", rs.goal.objective.trim());
    if let Some(w) = &rs.goal.wayfinder_goal_id {
        println!("  Wayfinder:    {w}");
    }
    if !rs.goal.plan_phases.is_empty() {
        println!("  Plan phases:  {}", rs.goal.plan_phases.join(", "));
    }
    match (ahead, behind) {
        (Some(a), Some(b)) => println!(
            "  vs {}:    {a} commit(s) ahead, {b} behind{}",
            rs.base_branch,
            if b > 0 {
                " (the cut merges main in first)"
            } else {
                ""
            }
        ),
        _ => println!(
            "  vs {}:    unknown (is branch {} present? git could not compare)",
            rs.base_branch, rs.branch
        ),
    }
    if rs.is_open() {
        if let Some(cur) = current_branch(root) {
            if cur != rs.branch {
                println!(
                    "  Note:         you are on '{cur}', not {}. Goals started here do not see the \
                     release's commits; `git switch {}` first.",
                    rs.branch, rs.branch
                );
            }
        }
    }
    println!("  Goals landed ({}):", rs.goals_landed.len());
    for g in &rs.goals_landed {
        println!(
            "    - {} {}{}",
            short_id(g.goal_id),
            g.title,
            g.commit
                .as_deref()
                .map(|c| format!("  ({})", short(c)))
                .unwrap_or_default()
        );
    }
    println!(
        "  Hotfixes ({}), each on main with its own PR:",
        rs.hotfixes.len()
    );
    for h in &rs.hotfixes {
        println!("    - {} {}: {}", short_id(h.goal_id), h.title, h.reason);
    }
    println!(
        "  Off-track goals ({}) — advice only, nothing is blocked:",
        rs.off_track.len()
    );
    for o in &rs.off_track {
        println!("    - {} {}", short_id(o.goal_id), o.title);
        for r in &o.reasons {
            println!("        {r}");
        }
    }
    if let Some(url) = &rs.pr_url {
        println!("  Release PR:   {url}");
    }
    println!();
    println!(
        "Merges to {} for this release: 1 (the release PR at `ta release-set cut`), however many \
         goals land. Hotfixes are the only goals that merge on their own.",
        rs.base_branch
    );
    if rs.is_open() {
        println!("Next: `ta release-set cut` when the release goal is done.");
    }
    Ok(())
}

// ── cut ──────────────────────────────────────────────────────────────────────

pub(crate) fn cut(config: &GatewayConfig) -> Result<()> {
    super::human_auth::require_human_for("release-set cut")?;
    let root = config.workspace_root.as_path();
    let store = ReleaseSetStore::new(root);
    let Some(rs) = store.open_release()? else {
        bail!(
            "No release is open, so there is nothing to cut. Start one with \
             `ta release-set start <name> --goal \"...\"` (`ta release-set status` shows the last \
             one)."
        );
    };
    ensure_clean(root, "cutting the release")?;
    let original = current_branch(root);

    let result = cut_on_release_branch(config, &rs);

    // Leave the user on the branch they were on, whatever happened.
    if let Some(o) = original {
        if current_branch(root).as_deref() != Some(o.as_str()) {
            if let Err(e) = git(root, &["checkout", &o]) {
                eprintln!(
                    "[release-set] Warning: could not switch back to '{o}': {e}. Run `git switch {o}`."
                );
            }
        }
    }
    let (pr_url, evidence) = result?;

    let marked = store.update_if_open(&rs.id, |r| r.mark_cut(Some(pr_url.clone())));
    if let Err(e) = &marked {
        eprintln!(
            "[release-set] Warning: the PR was opened ({pr_url}) but the release record could not \
             be marked cut: {e}. Edit {} (status = \"cut\") by hand.",
            store.path().display()
        );
    }
    marked?;
    if let Ok(Some(cutrs)) = store.load() {
        let goals = GoalRunStore::new(&config.goals_dir)?;
        if let Err(e) = complete_umbrella(&goals, &cutrs) {
            eprintln!("[release-set] Warning: could not complete the umbrella goal: {e}");
        }
    }

    println!();
    println!("Release {} cut.", rs.name);
    println!("  PR:       {pr_url}");
    println!("  Evidence: {evidence}");
    println!(
        "  1 merge to {} for {} goal(s) (+ {} hotfix(es) that merged on their own). After the PR \
         merges, `git switch {} && git pull` and start the next release when you are ready.",
        rs.base_branch,
        rs.goals_landed.len(),
        rs.hotfixes.len(),
        rs.base_branch
    );
    Ok(())
}

/// Checkout, merge main, gate, push once, open the PR. Returns (PR url, evidence line).
fn cut_on_release_branch(config: &GatewayConfig, rs: &ReleaseSet) -> Result<(String, String)> {
    let root = config.workspace_root.as_path();
    let wf = ta_submit::WorkflowConfig::load_or_default(&root.join(".ta/workflow.toml"));
    let remote = wf.submit.git.remote.clone();
    let main = rs.base_branch.clone();
    let branch = rs.branch.clone();

    if git_try(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_none()
    {
        bail!(
            "Release branch {branch} does not exist locally. Recreate it from the release's base \
             commit with `git branch {branch} {}` (see `ta release-set status`), then retry.",
            rs.base_commit
        );
    }
    if git_try(root, &["remote", "get-url", &remote]).is_none() {
        bail!(
            "Git remote '{remote}' is not configured, so the release cannot be pushed. Add it \
             (`git remote add {remote} <url>`) or set `[submit.git] remote` in \
             .ta/workflow.toml, then retry."
        );
    }
    git(root, &["fetch", &remote]).with_context(|| {
        format!("could not fetch '{remote}' before cutting; check the network and credentials")
    })?;
    let main_ref = latest_main_ref(root, &remote, &main).ok_or_else(|| {
        anyhow!("neither '{remote}/{main}' nor '{main}' exists; cannot merge the latest main")
    })?;

    git(root, &["checkout", &branch])
        .with_context(|| format!("could not switch to release branch {branch}"))?;

    // 1. Merge the latest main in, so the gate tests what the PR will merge into.
    println!("[release-set] Merging {main_ref} into {branch}...");
    let merge = git_cmd(root)
        .args([
            "merge",
            "--no-edit",
            "-m",
            &format!("Merge {main_ref} into {branch}"),
            &main_ref,
        ])
        .output()
        .context("could not run `git merge`")?;
    if !merge.status.success() {
        let conflicts = git_try(root, &["diff", "--name-only", "--diff-filter=U"])
            .unwrap_or_default()
            .replace('\n', ", ");
        let _ = git(root, &["merge", "--abort"]);
        bail!(
            "Merging {main_ref} into {branch} conflicts{}. The merge was aborted; nothing was \
             pushed. Resolve it yourself: `git switch {branch} && git merge {main_ref}`, fix the \
             conflicts, commit, then run `ta release-set cut` again.",
            if conflicts.is_empty() {
                String::new()
            } else {
                format!(" in: {conflicts}")
            }
        );
    }

    // 2. Nothing to release?
    let ahead: u64 = git_try(
        root,
        &["rev-list", "--count", &format!("{main_ref}..{branch}")],
    )
    .and_then(|n| n.parse().ok())
    .unwrap_or(0);
    if ahead == 0 {
        bail!(
            "{branch} has no commits that {main_ref} lacks, so there is nothing to put in a PR. \
             Land at least one goal on it (`ta draft apply <id>` while the release is open)."
        );
    }

    // 3. The verify gate. Nothing is pushed unless it passes.
    let gate_cmds = if wf.release_set.gate_commands.is_empty() {
        wf.verify.commands.clone()
    } else {
        wf.release_set.gate_commands.clone()
    };
    let head = git(root, &["rev-parse", "HEAD"])?;
    let evidence = if gate_cmds.is_empty() {
        eprintln!(
            "[release-set] Warning: no gate is configured, so the cut is not verified. Set \
             `[release_set] gate_commands` (or `[verify] commands`) in .ta/workflow.toml."
        );
        format!(
            "release-set gate: none configured; not verified by `ta release-set cut` ({} @ {})",
            short(&head),
            chrono::Utc::now().format("%Y-%m-%d")
        )
    } else {
        let mut gate = wf.verify.clone();
        gate.commands = gate_cmds.clone();
        println!(
            "[release-set] Running the release gate on {branch} ({} command(s))...",
            gate_cmds.len()
        );
        let result = super::verify::run_verification(&gate, root);
        if !result.passed {
            let failed: Vec<String> = result
                .warnings
                .iter()
                .map(|w| {
                    format!(
                        "`{}` (exit {})",
                        w.command,
                        w.exit_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "none".into())
                    )
                })
                .collect();
            bail!(
                "Release gate FAILED: {}. Refusing to cut: nothing was pushed and no PR was \
                 opened. Fix it on {branch} (land a fix goal while the release is open), then run \
                 `ta release-set cut` again. Re-run the failing command yourself on {branch} to \
                 see its full output.",
                if failed.is_empty() {
                    "a gate command failed".to_string()
                } else {
                    failed.join("; ")
                }
            );
        }
        format!(
            "release-set gate: {} command(s) passed at {} on {}: {}",
            gate_cmds.len(),
            short(&head),
            chrono::Utc::now().format("%Y-%m-%d"),
            gate_cmds
                .iter()
                .map(|c| c.run.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )
    };

    // 4. Push once.
    println!("[release-set] Pushing {branch} to {remote} (once)...");
    git(root, &["push", "-u", &remote, &branch]).with_context(|| {
        format!(
            "could not push {branch} to '{remote}'. The gate passed and nothing else changed; fix \
             the push (credentials, branch protection) and run `ta release-set cut` again"
        )
    })?;

    // 5. ONE PR to main.
    let body = pr_body(rs, &evidence);
    println!("[release-set] Opening the release PR ({branch} -> {main})...");
    let out = run_gh_pr_create(root, &main, &branch, &rs.goal.title, &body).map_err(|e| {
        anyhow!(
            "could not run `gh` ({e}). {branch} IS pushed. Install/authenticate the GitHub \
                 CLI and run `ta release-set cut` again (the push is then a no-op), or open the \
                 PR yourself: gh pr create --base {main} --head {branch}"
        )
    })?;
    if !out.status.success() {
        bail!(
            "`gh pr create --base {main} --head {branch}` failed: {}. {branch} IS pushed. Fix gh \
             (`gh auth status`) and run `ta release-set cut` again, or open the PR yourself.",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let url = String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "(gh printed no URL)".to_string());
    Ok((url, evidence))
}

fn pr_body(rs: &ReleaseSet, evidence: &str) -> String {
    let mut b = String::new();
    b.push_str(&format!(
        "## {}\n\n{}\n\n",
        rs.goal.title,
        rs.goal.objective.trim()
    ));
    if let Some(w) = &rs.goal.wayfinder_goal_id {
        b.push_str(&format!("Wayfinder goal: {w}\n\n"));
    }
    b.push_str(&format!("**Evidence:** {evidence}\n\n"));
    b.push_str(&format!(
        "### Goals in this release ({})\n",
        rs.goals_landed.len()
    ));
    for g in &rs.goals_landed {
        b.push_str(&format!(
            "- {}{}\n",
            g.title,
            g.commit
                .as_deref()
                .map(|c| format!(" (`{}`)", short(c)))
                .unwrap_or_default()
        ));
    }
    if !rs.hotfixes.is_empty() {
        b.push_str(&format!(
            "\n### Hotfixes that went to {} on their own ({})\n",
            rs.base_branch,
            rs.hotfixes.len()
        ));
        for h in &rs.hotfixes {
            b.push_str(&format!("- {}: {}\n", h.title, h.reason));
        }
    }
    if !rs.off_track.is_empty() {
        b.push_str(&format!(
            "\n### Flagged as off the release goal ({}, advice only)\n",
            rs.off_track.len()
        ));
        for o in &rs.off_track {
            b.push_str(&format!("- {}: {}\n", o.title, o.reasons.join("; ")));
        }
    }
    b.push_str("\nOpened by `ta release-set cut`: one PR for the whole release.\n");
    b
}

/// `gh pr create` for the release PR. A script that was written an instant ago can fail to
/// start with ETXTBSY while another thread still holds its write handle; retry that briefly.
fn run_gh_pr_create(
    root: &Path,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
) -> std::io::Result<std::process::Output> {
    let mut attempt = 0;
    loop {
        let res = Command::new("gh")
            .current_dir(root)
            .args([
                "pr", "create", "--base", base, "--head", head, "--title", title, "--body", body,
            ])
            .output();
        match res {
            Err(e) if e.raw_os_error() == Some(26) && attempt < 20 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

// ── goal start ───────────────────────────────────────────────────────────────

/// Called when a goal is created (`ta goal start`, `ta run`): link it under the release's
/// umbrella goal, run the drift check against the release goal, and print/record the advice.
/// Advice only: this never refuses a goal. Does nothing when no release is open.
pub fn stamp_new_goal(goal: &mut GoalRun, project_root: &Path, goals: &GoalRunStore) -> Result<()> {
    let store = ReleaseSetStore::new(project_root);
    let hotfix = cli_hotfix_reason();
    let Some(rs) = store.open_release()? else {
        if hotfix.is_some() {
            eprintln!(
                "[release] Note: --hotfix was given but no release is open; this goal targets the \
                 configured branch with its own PR, as usual."
            );
        }
        return Ok(());
    };

    if let Some(reason) = hotfix {
        goal.release = Some(GoalReleaseInfo {
            release_id: rs.id.clone(),
            release_branch: rs.branch.clone(),
            hotfix_reason: Some(reason.clone()),
            off_track_reasons: Vec::new(),
        });
        let (id, title) = (goal.goal_run_id, goal.title.clone());
        store.update_if_open(&rs.id, |r| {
            r.record_hotfix(HotfixGoal {
                goal_id: id,
                title,
                reason: reason.clone(),
                commit: None,
                recorded_at: chrono::Utc::now(),
            });
            Ok(())
        })?;
        eprintln!(
            "[release] HOTFIX: this goal bypasses release '{}' and will target {} with its own \
             PR (reason: {reason}). It is recorded in the release state.",
            rs.name, rs.base_branch
        );
        if let Some(cur) = current_branch(goal.source_dir.as_deref().unwrap_or(project_root)) {
            if cur == rs.branch {
                eprintln!(
                    "[release] Note: the source tree is on release branch '{cur}'. A hotfix must \
                     branch from {}: `git switch {}` before starting and applying it.",
                    rs.base_branch, rs.base_branch
                );
            }
        }
        return Ok(());
    }

    attach_to_umbrella(goals, &rs, goal)?;
    let verdict = assess_goal(
        &rs,
        &goal.title,
        &goal.objective,
        goal.plan_phase.as_deref(),
    );
    goal.release = Some(GoalReleaseInfo {
        release_id: rs.id.clone(),
        release_branch: rs.branch.clone(),
        hotfix_reason: None,
        off_track_reasons: verdict.reasons.clone(),
    });
    eprintln!(
        "[release] Release '{}' is open: this goal will commit onto {} (no per-goal PR; \
         `ta release-set cut` opens the one PR).",
        rs.name, rs.branch
    );
    if !verdict.on_track {
        let (id, title, reasons) = (
            goal.goal_run_id,
            goal.title.clone(),
            verdict.reasons.clone(),
        );
        store.update_if_open(&rs.id, |r| {
            r.record_off_track(id, &title, reasons);
            Ok(())
        })?;
        eprintln!(
            "[release] WARNING: this goal looks OFF TRACK for the release goal \"{}\":",
            rs.goal.title
        );
        for r in &verdict.reasons {
            eprintln!("[release]   - {r}");
        }
        eprintln!(
            "[release] Advice only; nothing is blocked and the goal still goes onto {}. If it is \
             not part of this release: cancel it and start it after the cut, or, for an urgent \
             fix, run it with `--hotfix --hotfix-reason \"...\"` (goes to {} with its own PR). \
             It is flagged in `ta release-set status` and in `ta draft view`.",
            rs.branch, rs.base_branch
        );
    }
    let source = goal.source_dir.as_deref().unwrap_or(project_root);
    if let Some(cur) = current_branch(source) {
        if cur != rs.branch {
            eprintln!(
                "[release] Note: the source tree is on '{cur}', not {}. This goal's staging copy \
                 will not contain the release's earlier commits and may conflict at apply. \
                 Advice: `git switch {}` before starting release goals.",
                rs.branch, rs.branch
            );
        }
    }
    Ok(())
}

/// `ta draft apply --hotfix`: mark the goal behind `package_id` as a hotfix, so the apply
/// (and any later re-apply or detached run) targets the configured branch, and record it in
/// the release state. No-op with a note when no release is open.
pub fn mark_draft_goal_hotfix(
    config: &GatewayConfig,
    package_id: Uuid,
    reason: &str,
) -> Result<()> {
    let store = ReleaseSetStore::new(&config.workspace_root);
    let Some(rs) = store.open_release()? else {
        eprintln!(
            "[release] Note: --hotfix was given but no release is open; applying to the \
             configured branch as usual."
        );
        return Ok(());
    };
    let goals = GoalRunStore::new(&config.goals_dir)?;
    let Some(mut goal) = goals
        .list()?
        .into_iter()
        .find(|g| g.pr_package_id == Some(package_id))
    else {
        bail!(
            "No goal found for draft {package_id}, so it cannot be marked as a hotfix. Check \
             `ta goal list`."
        );
    };
    goal.release = Some(GoalReleaseInfo {
        release_id: rs.id.clone(),
        release_branch: rs.branch.clone(),
        hotfix_reason: Some(reason.to_string()),
        off_track_reasons: Vec::new(),
    });
    goals.save(&goal)?;
    let (id, title) = (goal.goal_run_id, goal.title.clone());
    store.update_if_open(&rs.id, |r| {
        r.record_hotfix(HotfixGoal {
            goal_id: id,
            title,
            reason: reason.to_string(),
            commit: None,
            recorded_at: chrono::Utc::now(),
        });
        Ok(())
    })?;
    Ok(())
}

// ── apply ────────────────────────────────────────────────────────────────────

/// Where `ta draft apply` sends this goal. A hotfix recorded on the goal, or `--hotfix` on this
/// command line, wins over the open release; the open release wins over the configured target.
pub fn route_for_goal(
    project_root: &Path,
    submit: &ta_submit::SubmitConfig,
    goal: &GoalRun,
) -> Result<SubmitRoute> {
    let reason = goal
        .release
        .as_ref()
        .and_then(|r| r.hotfix_reason.clone())
        .or_else(cli_hotfix_reason);
    Ok(resolve_route(project_root, submit, reason.as_deref())?)
}

/// After a successful commit: record the goal as landed on the release, or as a hotfix.
/// A failure to record is returned so the caller can say what to do; the commit stands.
pub fn record_applied(
    project_root: &Path,
    goal: &GoalRun,
    draft_id: &str,
    commit: Option<&str>,
    kind: &RouteKind,
) -> Result<()> {
    let store = ReleaseSetStore::new(project_root);
    match kind {
        RouteKind::Release { release_id, .. } => {
            store.update_if_open(release_id, |r| {
                r.record_landed(LandedGoal {
                    goal_id: goal.goal_run_id,
                    title: goal.title.clone(),
                    plan_phase: goal.plan_phase.clone(),
                    draft_id: Some(draft_id.to_string()),
                    commit: commit.map(str::to_string),
                    landed_at: chrono::Utc::now(),
                });
                Ok(())
            })?;
        }
        RouteKind::Hotfix {
            reason,
            bypassed_release: Some(_),
        } => {
            store.update(|r| {
                r.record_hotfix(HotfixGoal {
                    goal_id: goal.goal_run_id,
                    title: goal.title.clone(),
                    reason: reason.clone(),
                    commit: commit.map(str::to_string),
                    recorded_at: chrono::Utc::now(),
                });
                Ok(())
            })?;
        }
        RouteKind::Hotfix { .. } | RouteKind::Configured => {}
    }
    Ok(())
}

/// The "RELEASE TRACK" block of `ta draft view`: where this goal goes and whether it is on
/// track for the release goal. `None` when no release is open or the record is unreadable.
pub fn render_track_block(project_root: &Path, goal: &GoalRun) -> Option<String> {
    let rs = ReleaseSetStore::new(project_root).open_release().ok()??;
    let mut s = String::new();
    s.push_str(&format!(
        "RELEASE TRACK (advice only): release '{}' is open on {}\n",
        rs.name, rs.branch
    ));
    if let Some(reason) = goal.release.as_ref().and_then(|r| r.hotfix_reason.as_ref()) {
        s.push_str(&format!(
            "  HOTFIX ({reason}): applies to {} with its own PR; it does not join the release.\n",
            rs.base_branch
        ));
        return Some(s);
    }
    let verdict = assess_goal(
        &rs,
        &goal.title,
        &goal.objective,
        goal.plan_phase.as_deref(),
    );
    if verdict.on_track {
        s.push_str(&format!(
            "  On track for the release goal \"{}\". Applying commits it onto {} (no per-goal PR).\n",
            rs.goal.title, rs.branch
        ));
    } else {
        s.push_str(&format!(
            "  OFF TRACK for the release goal \"{}\":\n",
            rs.goal.title
        ));
        for r in &verdict.reasons {
            s.push_str(&format!("    - {r}\n"));
        }
        s.push_str(&format!(
            "  Applying still commits it onto {}. Not part of this release? Do not apply it now; \
             or apply it as a hotfix: `ta draft apply <id> --hotfix --hotfix-reason \"...\"`.\n",
            rs.branch
        ));
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotfix_flag_pairing() {
        assert_eq!(hotfix_reason_from_flags(false, None).unwrap(), None);
        assert_eq!(
            hotfix_reason_from_flags(true, Some(" prod down ")).unwrap(),
            Some("prod down".to_string())
        );
        let e = hotfix_reason_from_flags(true, None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("--hotfix-reason"), "{e}");
        assert!(hotfix_reason_from_flags(true, Some("  ")).is_err());
        let e = hotfix_reason_from_flags(false, Some("x"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("needs --hotfix"), "{e}");
    }

    #[test]
    fn cli_hotfix_guard_restores_the_previous_value() {
        assert_eq!(cli_hotfix_reason(), None);
        {
            let _g = CliHotfixGuard::set(Some("outer".into()));
            assert_eq!(cli_hotfix_reason().as_deref(), Some("outer"));
            {
                let _g2 = CliHotfixGuard::set(Some("inner".into()));
                assert_eq!(cli_hotfix_reason().as_deref(), Some("inner"));
            }
            assert_eq!(cli_hotfix_reason().as_deref(), Some("outer"));
        }
        assert_eq!(cli_hotfix_reason(), None);
    }

    #[test]
    fn pr_body_carries_evidence_goals_hotfixes_and_off_track() {
        let mut rs = ReleaseSet::open_new(NewRelease {
            name: "0.18".into(),
            objective: "Ship billing".into(),
            wayfinder_goal_id: Some("wf-9".into()),
            plan_phases: vec![],
            base_branch: "main".into(),
            base_commit: "abc".into(),
        })
        .unwrap();
        let g = Uuid::new_v4();
        rs.record_landed(LandedGoal {
            goal_id: g,
            title: "Invoice export".into(),
            plan_phase: None,
            draft_id: None,
            commit: Some("0123456789abcdef".into()),
            landed_at: chrono::Utc::now(),
        });
        rs.record_hotfix(HotfixGoal {
            goal_id: Uuid::new_v4(),
            title: "Fix crash".into(),
            reason: "prod down".into(),
            commit: None,
            recorded_at: chrono::Utc::now(),
        });
        rs.record_off_track(g, "Invoice export", vec!["no keywords".into()]);
        let body = pr_body(&rs, "release-set gate: 1 command(s) passed");
        assert!(body.contains("**Evidence:** release-set gate: 1 command(s) passed"));
        assert!(body.contains("Wayfinder goal: wf-9"));
        assert!(body.contains("Invoice export (`01234567`)"));
        assert!(body.contains("Fix crash: prod down"));
        assert!(body.contains("no keywords"));
    }
}
