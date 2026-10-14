// doctor_fix.rs: `ta doctor --fix`, scriptable and consistent.
//
// The findings are computed ONCE, up front, and numbered by stable ids. The
// header count, the printed list and the prompts all come from that one list,
// so "Found 3 issue(s)" is followed by exactly three prompts and applying one
// fix can never shift which prompt a piped answer lands on.
//
//   ta doctor --fix                 ask about each finding (needs a terminal)
//   ta doctor --fix --yes           apply every NON-destructive finding
//   ta doctor --fix --only a,b      apply exactly these ids, no prompts
//   ta doctor --fix --dry-run       print what would change, change nothing
//
// Destructive fixes (they delete staging directories) are never covered by
// `--yes`: each needs its own id in `--only`. A fix that rewrites a file the
// project's VCS tracks names that file before it runs. Without a terminal,
// `--yes` or `--only`, the command prints the findings and the exact commands
// and exits non-zero rather than guessing.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use ta_mcp_gateway::GatewayConfig;

use super::doctor::{self, OrphanedClaim};
use super::health_signals::{HealthSignal, SignalSeverity};

/// Health-signal kinds whose fix deletes staging directories or goal data.
/// They need their own id even with `--yes`.
const DESTRUCTIVE_SIGNAL_KINDS: &[&str] = &[
    "disk_staging",
    "orphan_staging",
    "stale_failed_goals",
    "stale_pr_ready",
    "disk_free",
];

/// Flags of `ta doctor --fix`.
#[derive(Debug, Clone, Default)]
pub struct FixOptions {
    pub yes: bool,
    pub only: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
enum FixAction {
    Secrets,
    Signal(HealthSignal),
    Orphan(OrphanedClaim),
    VcsGitignore,
    VscodeExcludes,
    IdeManifest,
    /// Windows: remove AppContainer ACEs left by goals whose `ta` process died.
    StaleAppContainerAcls(std::path::PathBuf),
}

#[derive(Debug, Clone)]
struct Finding {
    id: String,
    label: &'static str,
    message: String,
    /// Extra lines shown under the message (paths, PLAN.md lines).
    detail: Vec<String>,
    fix: String,
    destructive: bool,
    /// Project-relative files the fix rewrites; named before and during the fix.
    rewrites: Vec<String>,
    action: FixAction,
}

/// A finding TA cannot fix on its own. Listed, never prompted.
#[derive(Debug, Clone)]
struct ManualFinding {
    id: String,
    message: String,
    fix: String,
}

/// What a `--fix` run found and did. Tests assert on this; the same facts are
/// printed for the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixReport {
    /// Ids of every finding with an automated fix, in the order listed.
    pub finding_ids: Vec<String>,
    /// Ids of findings that need manual action (not counted above).
    pub manual_ids: Vec<String>,
    pub prompts_asked: usize,
    pub applied: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
    /// Destructive findings `--yes` did not touch.
    pub needs_explicit_id: Vec<String>,
    pub dry_run: bool,
}

fn severity_label(s: SignalSeverity) -> &'static str {
    match s {
        SignalSeverity::Crit => "[crit]",
        SignalSeverity::Warn => "[warn]",
        SignalSeverity::Info => "[info]",
    }
}

fn collect(config: &GatewayConfig) -> (Vec<Finding>, Vec<ManualFinding>) {
    let mut findings = Vec::new();
    let mut manual = Vec::new();

    // Control-plane secrets first: the highest-risk finding.
    let summary = super::state_secrets::summarize(&config.workspace_root);
    if summary.fixable_count() > 0 {
        findings.push(Finding {
            id: "secrets".into(),
            label: "[warn]",
            message: format!(
                "{} control-plane secret(s) are inside the project tree, reachable by any agent",
                summary.fixable_count()
            ),
            detail: super::state_secrets::describe_items(&summary),
            fix: "move them to the TA state directory (each copy is verified before the old one \
                  is removed)"
                .into(),
            destructive: false,
            rewrites: Vec::new(),
            action: FixAction::Secrets,
        });
    }
    if !summary.cred_files.is_empty() {
        manual.push(ManualFinding {
            id: "plaintext-cred-files".into(),
            message: format!(
                "{} plaintext credential file(s) inside the project tree: {}",
                summary.cred_files.len(),
                summary
                    .cred_files
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            fix: "TA cannot move these for the script that reads them. Store the secret with \
                  `ta credentials add` (it is encrypted, with its key outside the project), or \
                  keep the file in the per-user state directory (`ta show state`) and point the \
                  script that writes it there, then delete the copy in the project."
                .into(),
        });
    }

    // Health signals, one finding per kind.
    let mut by_kind: BTreeMap<String, Vec<HealthSignal>> = BTreeMap::new();
    let mut kind_order: Vec<String> = Vec::new();
    for s in super::health_signals::compute_health_signals(config) {
        if !by_kind.contains_key(&s.kind) {
            kind_order.push(s.kind.clone());
        }
        by_kind.entry(s.kind.clone()).or_default().push(s);
    }
    for kind in kind_order {
        let signals = &by_kind[&kind];
        let first = &signals[0];
        let mut detail: Vec<String> = signals.iter().skip(1).map(|s| s.message.clone()).collect();
        detail.dedup();
        findings.push(Finding {
            id: kind.clone(),
            label: severity_label(first.severity),
            message: first.message.clone(),
            detail,
            fix: first.action.clone(),
            destructive: DESTRUCTIVE_SIGNAL_KINDS.contains(&kind.as_str()),
            rewrites: Vec::new(),
            action: FixAction::Signal(first.clone()),
        });
    }

    // Orphaned in_progress phase claims rewrite PLAN.md.
    for o in doctor::find_orphaned_claims(config) {
        findings.push(Finding {
            id: format!("orphaned-phase-{}", o.phase_id),
            label: "[warn]",
            message: format!(
                "Phase {} is in_progress but no live goal holds it{}",
                o.phase_id,
                if o.adhoc_stub { " (ad-hoc stub)" } else { "" }
            ),
            detail: o.plan_lines.clone(),
            fix: doctor::orphan_fix_description(&o),
            destructive: false,
            rewrites: vec!["PLAN.md".into()],
            action: FixAction::Orphan(o),
        });
    }

    // Missing ignore entries rewrite the VCS ignore file.
    let gaps = doctor::vcs_gitignore_gaps(config);
    if !gaps.is_empty() {
        let ignore_file = match ta_submit::detect_vcs(&config.workspace_root).backend() {
            ta_workspace::partitioning::VcsBackend::Perforce => ".p4ignore",
            ta_workspace::partitioning::VcsBackend::Svn => ".ta (svn:ignore property)",
            _ => ".gitignore",
        };
        findings.push(Finding {
            id: "vcs-gitignore".into(),
            label: "[warn]",
            message: format!(
                "{} .ta/ path(s) exist but are not ignored by version control",
                gaps.len()
            ),
            detail: gaps.iter().map(|g| format!(".ta/{g}")).collect(),
            fix: format!("ta setup vcs (refreshes the TA block of {ignore_file} in place)"),
            destructive: false,
            rewrites: vec![ignore_file.into()],
            action: FixAction::VcsGitignore,
        });
    }

    let vscode_missing = doctor::vscode_missing_excludes(&config.workspace_root);
    if !vscode_missing.is_empty() {
        findings.push(Finding {
            id: "vscode-excludes".into(),
            label: "[warn]",
            message: format!(
                ".vscode/settings.json is missing {} TA runtime exclude(s)",
                vscode_missing.len()
            ),
            detail: Vec::new(),
            fix: "add the missing entries to files.exclude and search.exclude".into(),
            destructive: false,
            rewrites: vec![".vscode/settings.json".into()],
            action: FixAction::VscodeExcludes,
        });
    }

    if doctor::ide_manifest_needs_fix(&config.workspace_root) {
        findings.push(Finding {
            id: "ide-excludes-manifest".into(),
            label: "[warn]",
            message: ".ta/ide-excludes.json is missing or out of date".into(),
            detail: Vec::new(),
            fix: "regenerate the manifest from the current runtime directory list".into(),
            destructive: false,
            rewrites: vec![".ta/ide-excludes.json".into()],
            action: FixAction::IdeManifest,
        });
    }

    // Windows AppContainer grants that outlived their goal (kill -9, power loss).
    if let Some(state_dir) = super::sandbox_launch::project_state_dir(&config.workspace_root) {
        let stale = ta_runtime::sandbox_windows::stale_acl_grants(&state_dir);
        if !stale.is_empty() {
            findings.push(Finding {
                id: "appcontainer-acls".into(),
                label: "[warn]",
                message: format!(
                    "{} AppContainer access entr(ies) were left on disk by goals whose ta \
                     process is gone",
                    stale.len()
                ),
                detail: stale
                    .iter()
                    .map(|r| {
                        format!(
                            "{} (goal {}, container {})",
                            r.path, r.goal_id, r.container_name
                        )
                    })
                    .collect(),
                fix: "remove the stale entries; live goals are not touched and a failed removal \
                      stays recorded for the next run"
                    .into(),
                destructive: false,
                rewrites: Vec::new(),
                action: FixAction::StaleAppContainerAcls(state_dir),
            });
        }
    }

    (findings, manual)
}

/// Whether the project's git tracks `rel`. Runs git with the variables a TA
/// session exports (`GIT_DIR`, `GIT_WORK_TREE`, ...) cleared, so it answers
/// for the project at `root`, not for the session's repository.
fn is_tracked(root: &Path, rel: &str) -> bool {
    std::process::Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", rel])
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// "PLAN.md (tracked by git)" / "docs/x.md".
fn describe_rewrites(root: &Path, f: &Finding) -> String {
    f.rewrites
        .iter()
        .map(|rel| {
            if is_tracked(root, rel) {
                format!("{rel} (tracked by git)")
            } else {
                rel.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_finding(root: &Path, f: &Finding) {
    println!("{} (id: {}) {}", f.label, f.id, f.message);
    for line in &f.detail {
        println!("       {line}");
    }
    println!("  Fix: {}", f.fix);
    if !f.rewrites.is_empty() {
        println!("  Rewrites: {}", describe_rewrites(root, f));
    }
    if f.destructive {
        println!(
            "  Destructive: deletes data, so --yes does not apply it. Needs: ta doctor --fix \
             --only {}",
            f.id
        );
    }
}

fn apply(config: &GatewayConfig, f: &Finding) -> anyhow::Result<String> {
    if !f.rewrites.is_empty() {
        println!(
            "  Rewriting {} ...",
            describe_rewrites(&config.workspace_root, f)
        );
    }
    match &f.action {
        FixAction::Secrets => {
            let outcome = super::state_secrets::apply_fix(config)?;
            if outcome.problems > 0 {
                anyhow::bail!(
                    "{} secret item(s) need attention; nothing unverified was deleted. Resolve \
                     them, then re-run `ta doctor --fix --only secrets`",
                    outcome.problems
                );
            }
            Ok(format!(
                "moved or scrubbed {} item(s) out of the project tree",
                outcome.moved
            ))
        }
        FixAction::Signal(signal) => doctor::apply_fix(config, signal),
        FixAction::Orphan(o) => doctor::fix_orphaned_claim(config, o),
        FixAction::VcsGitignore => {
            super::setup::run_vcs_setup(config, false, false, None, None)?;
            Ok("ran ta setup vcs; the ignore block is updated".to_string())
        }
        FixAction::VscodeExcludes => {
            crate::commands::init::write_vscode_settings_excludes(&config.workspace_root)?;
            Ok("updated .vscode/settings.json with the TA runtime excludes".to_string())
        }
        FixAction::IdeManifest => {
            crate::commands::init::write_ide_excludes_manifest(&config.workspace_root)?;
            Ok("regenerated .ta/ide-excludes.json".to_string())
        }
        FixAction::StaleAppContainerAcls(state_dir) => {
            let report = ta_runtime::sandbox_windows::sweep_stale_acls(state_dir);
            for (rec, why) in &report.failed {
                tracing::warn!(path = %rec.path, container = %rec.container_name, error = %why,
                    "could not remove a stale AppContainer access entry");
            }
            if !report.failed.is_empty() {
                anyhow::bail!(
                    "removed {} stale entr(ies), {} could not be removed: {}. They stay recorded \
                     in {}; close whatever holds the path and re-run `ta doctor --fix --only \
                     appcontainer-acls`",
                    report.revoked.len(),
                    report.failed.len(),
                    report
                        .failed
                        .iter()
                        .map(|(r, e)| format!("{}: {e}", r.path))
                        .collect::<Vec<_>>()
                        .join("; "),
                    state_dir.display()
                );
            }
            Ok(format!(
                "removed {} stale AppContainer access entr(ies); {} belong to running goals and \
                 were left alone",
                report.revoked.len(),
                report.live.len()
            ))
        }
    }
}

/// `ta doctor --fix`: reads answers from the real stdin.
pub fn execute(config: &GatewayConfig, opts: &FixOptions) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let report = run(config, opts, &mut stdin.lock(), interactive)?;
    if !report.failed.is_empty() {
        anyhow::bail!(
            "{} fix(es) failed: {}. See the messages above; fix the cause and re-run \
             `ta doctor --fix --only {}`.",
            report.failed.len(),
            report.failed.join(", "),
            report.failed.join(",")
        );
    }
    Ok(())
}

/// The whole flow with the input and the terminal-ness injected.
pub(crate) fn run(
    config: &GatewayConfig,
    opts: &FixOptions,
    input: &mut dyn BufRead,
    interactive: bool,
) -> anyhow::Result<FixReport> {
    let (findings, manual) = collect(config);
    run_with(config, opts, input, interactive, findings, manual)
}

/// [`run`] over an already-computed finding list.
fn run_with(
    config: &GatewayConfig,
    opts: &FixOptions,
    input: &mut dyn BufRead,
    interactive: bool,
    findings: Vec<Finding>,
    manual: Vec<ManualFinding>,
) -> anyhow::Result<FixReport> {
    let root = &config.workspace_root;
    let mut report = FixReport {
        finding_ids: findings.iter().map(|f| f.id.clone()).collect(),
        manual_ids: manual.iter().map(|m| m.id.clone()).collect(),
        dry_run: opts.dry_run,
        ..FixReport::default()
    };

    if findings.is_empty() && manual.is_empty() {
        println!("No health issues detected, nothing to fix.");
        return Ok(report);
    }

    // Validate --only before printing or changing anything.
    let unknown: Vec<&String> = opts
        .only
        .iter()
        .filter(|id| !report.finding_ids.contains(id))
        .collect();
    if !unknown.is_empty() {
        anyhow::bail!(
            "unknown finding id(s) for --only: {}. Findings in this project right now: {}. Run \
             `ta doctor --fix --dry-run` to list them with their ids.",
            unknown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if report.finding_ids.is_empty() {
                "none".to_string()
            } else {
                report.finding_ids.join(", ")
            }
        );
    }

    println!("TA Doctor: Fix Mode");
    println!();
    println!("Found {} issue(s):", findings.len());
    println!();
    for f in &findings {
        print_finding(root, f);
        println!();
    }
    if !manual.is_empty() {
        println!(
            "{} more finding(s) need manual action (not counted above, never prompted):",
            manual.len()
        );
        for m in &manual {
            println!("[warn] (id: {}) {}", m.id, m.message);
            println!("  Fix: {}", m.fix);
        }
        println!();
    }

    let non_destructive: Vec<&Finding> = findings.iter().filter(|f| !f.destructive).collect();
    let destructive: Vec<&Finding> = findings.iter().filter(|f| f.destructive).collect();

    let explicit = !opts.only.is_empty();
    // Which findings this run applies without asking (None = ask each).
    let selected: Option<Vec<&Finding>> = if explicit {
        Some(
            findings
                .iter()
                .filter(|f| opts.only.contains(&f.id))
                .collect(),
        )
    } else if opts.yes {
        Some(non_destructive.clone())
    } else {
        None
    };

    if opts.dry_run {
        println!("Dry run: nothing was changed.");
        match &selected {
            Some(sel) => {
                for f in sel {
                    println!("  would apply {}: {}", f.id, f.fix);
                    if !f.rewrites.is_empty() {
                        println!("    rewrites {}", describe_rewrites(root, f));
                    }
                }
                if sel.is_empty() {
                    println!("  nothing selected.");
                }
            }
            None => {
                println!(
                    "  without --yes or --only this would ask about each of the {} finding(s) \
                     above, in order.",
                    findings.len()
                );
            }
        }
        if opts.yes && !explicit {
            for f in &destructive {
                report.needs_explicit_id.push(f.id.clone());
                println!(
                    "  would NOT apply {} (destructive): needs ta doctor --fix --only {}",
                    f.id, f.id
                );
            }
        }
        print_commands(&non_destructive, &destructive);
        return Ok(report);
    }

    match selected {
        Some(sel) => {
            for f in &sel {
                run_one(config, f, &mut report);
            }
            if opts.yes && !explicit {
                for f in &destructive {
                    report.needs_explicit_id.push(f.id.clone());
                    println!(
                        "Not applied: {} is destructive ({}). To apply it: ta doctor --fix \
                         --only {}",
                        f.id, f.message, f.id
                    );
                }
            }
            // Findings not selected by --only are left alone.
            for f in &findings {
                if explicit && !opts.only.contains(&f.id) && !report.skipped.contains(&f.id) {
                    report.skipped.push(f.id.clone());
                }
            }
        }
        None if interactive => {
            for f in &findings {
                print!("Apply fix for {}? [y/N]: ", f.id);
                std::io::stdout().flush().ok();
                report.prompts_asked += 1;
                let mut line = String::new();
                input.read_line(&mut line).ok();
                if line.trim().eq_ignore_ascii_case("y") {
                    run_one(config, f, &mut report);
                } else {
                    println!("  Skipped {}.", f.id);
                    report.skipped.push(f.id.clone());
                }
                println!();
            }
        }
        None => {
            println!(
                "No fix was applied: stdin is not a terminal and neither --yes nor --only was \
                 given, so TA will not guess which fixes you want."
            );
            print_commands(&non_destructive, &destructive);
            anyhow::bail!(
                "ta doctor --fix needs a terminal, --yes or --only: {} finding(s) are listed \
                 above and nothing was changed",
                findings.len()
            );
        }
    }

    println!(
        "Summary: {} found, {} applied, {} skipped, {} failed{}.",
        findings.len(),
        report.applied.len(),
        report.skipped.len(),
        report.failed.len(),
        if report.needs_explicit_id.is_empty() {
            String::new()
        } else {
            format!(
                ", {} destructive left for an explicit --only",
                report.needs_explicit_id.len()
            )
        }
    );
    Ok(report)
}

fn run_one(config: &GatewayConfig, f: &Finding, report: &mut FixReport) {
    println!("-> Applying {}", f.id);
    match apply(config, f) {
        Ok(msg) => {
            println!("  ok  {msg}");
            report.applied.push(f.id.clone());
        }
        Err(e) => {
            println!("  FAILED {}: {e}", f.id);
            report.failed.push(f.id.clone());
        }
    }
}

fn print_commands(non_destructive: &[&Finding], destructive: &[&Finding]) {
    println!("Commands:");
    if !non_destructive.is_empty() {
        println!(
            "  ta doctor --fix --yes        apply every non-destructive fix ({})",
            non_destructive
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let all: Vec<&str> = non_destructive
        .iter()
        .chain(destructive.iter())
        .map(|f| f.id.as_str())
        .collect();
    if !all.is_empty() {
        println!(
            "  ta doctor --fix --only <ids> apply exactly these ids (comma separated): {}",
            all.join(",")
        );
    }
    if !destructive.is_empty() {
        println!(
            "  destructive (explicit id required, never covered by --yes): {}",
            destructive
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!("  ta doctor --fix --dry-run    show what would change, change nothing");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A project whose findings are known: a vault key in the tree (secrets)
    /// and no IDE exclude manifest.
    fn project_with_findings() -> (tempfile::TempDir, GatewayConfig) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/credentials.key"),
            "AGE-SECRET-KEY-1TESTTESTTESTTESTTESTTESTTESTTESTTESTTESTTESTTESTTESTTEST",
        )
        .unwrap();
        let config = GatewayConfig::for_project(dir.path());
        (dir, config)
    }

    /// Every file under `root` with its bytes, for "byte-identical" checks.
    fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else {
                    let rel = p
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.insert(rel, std::fs::read(&p).unwrap_or_default());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn answers(text: &str) -> Cursor<Vec<u8>> {
        Cursor::new(text.as_bytes().to_vec())
    }

    #[test]
    fn summary_count_equals_the_prompts_asked() {
        let (dir, config) = project_with_findings();
        let before = snapshot(dir.path());
        let mut input = answers(&"n\n".repeat(32));
        let report = run(&config, &FixOptions::default(), &mut input, true).unwrap();
        assert!(report.finding_ids.len() >= 2, "{:?}", report.finding_ids);
        assert!(report.finding_ids.contains(&"secrets".to_string()));
        assert_eq!(
            report.prompts_asked,
            report.finding_ids.len(),
            "\"Found N issue(s)\" must be followed by exactly N prompts"
        );
        assert_eq!(report.skipped.len(), report.finding_ids.len());
        assert!(report.applied.is_empty());
        assert_eq!(snapshot(dir.path()), before, "answering no changes nothing");
    }

    #[test]
    fn piped_answers_land_on_the_finding_they_were_meant_for() {
        // The first finding is `secrets`; "y" then "n" for everything else.
        // Applying the first fix must not shift the later prompts.
        let (dir, config) = project_with_findings();
        let mut input = answers(&format!("y\n{}", "n\n".repeat(32)));
        let report = run(&config, &FixOptions::default(), &mut input, true).unwrap();
        assert_eq!(report.finding_ids[0], "secrets");
        assert_eq!(report.applied, vec!["secrets".to_string()], "{report:?}");
        assert!(!dir.path().join(".ta/credentials.key").exists());
        assert_eq!(report.prompts_asked, report.finding_ids.len());
    }

    #[test]
    fn yes_applies_every_non_destructive_fix_and_prints_nothing_to_prompt() {
        let (dir, config) = project_with_findings();
        let opts = FixOptions {
            yes: true,
            ..FixOptions::default()
        };
        let report = run(&config, &opts, &mut answers(""), false).unwrap();
        assert_eq!(report.prompts_asked, 0);
        assert!(
            report.applied.contains(&"secrets".to_string()),
            "{report:?}"
        );
        assert!(report
            .applied
            .contains(&"ide-excludes-manifest".to_string()));
        assert!(report.failed.is_empty(), "{report:?}");
        assert!(!dir.path().join(".ta/credentials.key").exists());
        assert!(dir.path().join(".ta/ide-excludes.json").exists());
    }

    #[test]
    fn only_applies_exactly_the_named_ids() {
        let (dir, config) = project_with_findings();
        let opts = FixOptions {
            only: vec!["ide-excludes-manifest".into()],
            ..FixOptions::default()
        };
        let report = run(&config, &opts, &mut answers(""), false).unwrap();
        assert_eq!(report.applied, vec!["ide-excludes-manifest".to_string()]);
        assert!(report.skipped.contains(&"secrets".to_string()));
        assert!(
            dir.path().join(".ta/credentials.key").exists(),
            "an id that was not named is left alone"
        );
        assert_eq!(report.prompts_asked, 0);
    }

    #[test]
    fn only_with_an_unknown_id_fails_listing_the_valid_ids() {
        let (dir, config) = project_with_findings();
        let before = snapshot(dir.path());
        let opts = FixOptions {
            only: vec!["no-such-finding".into()],
            ..FixOptions::default()
        };
        let err = run(&config, &opts, &mut answers(""), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no-such-finding"), "{err}");
        assert!(err.contains("secrets"), "{err}");
        assert!(err.contains("--dry-run"), "{err}");
        assert_eq!(snapshot(dir.path()), before);
    }

    #[test]
    fn dry_run_changes_nothing_even_with_yes() {
        let (dir, config) = project_with_findings();
        let before = snapshot(dir.path());
        for opts in [
            FixOptions {
                dry_run: true,
                yes: true,
                ..FixOptions::default()
            },
            FixOptions {
                dry_run: true,
                only: vec!["secrets".into()],
                ..FixOptions::default()
            },
            FixOptions {
                dry_run: true,
                ..FixOptions::default()
            },
        ] {
            let report = run(&config, &opts, &mut answers(""), false).unwrap();
            assert!(report.dry_run);
            assert!(report.applied.is_empty() && report.failed.is_empty());
            assert_eq!(report.prompts_asked, 0);
            assert_eq!(snapshot(dir.path()), before, "dry run must not write");
        }
    }

    #[test]
    fn non_interactive_without_yes_or_only_refuses_and_exits_non_zero() {
        let (dir, config) = project_with_findings();
        let before = snapshot(dir.path());
        let err = run(
            &config,
            &FixOptions::default(),
            &mut answers("y\ny\ny\n"),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--yes") && err.contains("--only"), "{err}");
        assert_eq!(
            snapshot(dir.path()),
            before,
            "must not guess from piped input"
        );
    }

    fn synthetic_destructive() -> Finding {
        Finding {
            id: "orphan_staging".into(),
            label: "[info]",
            message: "1 orphaned staging dir(s)".into(),
            detail: Vec::new(),
            fix: "remove orphaned staging directories".into(),
            destructive: true,
            rewrites: Vec::new(),
            // A signal kind whose fix has no side effects, so the test can
            // tell "applied" from "not applied" without deleting anything.
            action: FixAction::Signal(HealthSignal {
                kind: "daemon_error_rate".into(),
                severity: SignalSeverity::Info,
                message: "x".into(),
                action: "x".into(),
            }),
        }
    }

    #[test]
    fn destructive_fixes_need_their_own_id_even_with_yes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let yes = FixOptions {
            yes: true,
            ..FixOptions::default()
        };
        let report = run_with(
            &config,
            &yes,
            &mut answers(""),
            false,
            vec![synthetic_destructive()],
            Vec::new(),
        )
        .unwrap();
        assert!(report.applied.is_empty(), "{report:?}");
        assert_eq!(report.needs_explicit_id, vec!["orphan_staging".to_string()]);

        let only = FixOptions {
            only: vec!["orphan_staging".into()],
            ..FixOptions::default()
        };
        let report = run_with(
            &config,
            &only,
            &mut answers(""),
            false,
            vec![synthetic_destructive()],
            Vec::new(),
        )
        .unwrap();
        assert_eq!(report.applied, vec!["orphan_staging".to_string()]);
    }

    #[test]
    fn every_finding_is_accounted_for_in_the_report() {
        let (_dir, config) = project_with_findings();
        let opts = FixOptions {
            yes: true,
            ..FixOptions::default()
        };
        let r = run(&config, &opts, &mut answers(""), false).unwrap();
        assert_eq!(
            r.applied.len() + r.skipped.len() + r.failed.len() + r.needs_explicit_id.len(),
            r.finding_ids.len(),
            "{r:?}"
        );
    }

    #[test]
    fn plaintext_cred_files_are_reported_for_manual_action_and_not_prompted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/wayfinder-service-account.cred"),
            "client_secret=hunter2\n",
        )
        .unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let before = snapshot(dir.path());
        let (_, manual) = collect(&config);
        assert!(manual.iter().any(|m| m.id == "plaintext-cred-files"));
        assert!(manual[0].message.contains("wayfinder-service-account.cred"));
        let report = run(
            &config,
            &FixOptions {
                dry_run: true,
                ..FixOptions::default()
            },
            &mut answers(""),
            false,
        )
        .unwrap();
        assert!(report
            .manual_ids
            .contains(&"plaintext-cred-files".to_string()));
        assert!(!report
            .finding_ids
            .contains(&"plaintext-cred-files".to_string()));
        assert_eq!(
            snapshot(dir.path()),
            before,
            "the file is flagged, never touched"
        );
    }

    /// Run git in `dir` with the variables a TA session exports cleared, so
    /// the child talks to `dir` and not to the session's repository.
    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .status()
            .expect("git must be installed to run this test");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn rewriting_a_tracked_file_is_named_as_tracked() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("PLAN.md"), "# plan\n").unwrap();
        git(dir.path(), &["add", "PLAN.md"]);
        assert!(is_tracked(dir.path(), "PLAN.md"));
        assert!(!is_tracked(dir.path(), "NOT-THERE.md"));
        let f = Finding {
            id: "orphaned-phase-v1".into(),
            label: "[warn]",
            message: "m".into(),
            detail: Vec::new(),
            fix: "f".into(),
            destructive: false,
            rewrites: vec!["PLAN.md".into(), "other.md".into()],
            action: FixAction::IdeManifest,
        };
        let text = describe_rewrites(dir.path(), &f);
        assert!(text.contains("PLAN.md (tracked by git)"), "{text}");
        assert!(
            text.contains("other.md") && !text.contains("other.md (tracked"),
            "{text}"
        );
    }
}
