//! Smoke stage: run the real command against real (scratch) state before a draft exists.
//!
//! Unit tests passed for behaviour that was wrong in real use: the replay hash check, the
//! `rtk` slot, and others were only found by a human running the command. A `[[smoke.cases]]`
//! entry in `.ta/workflow.toml` is such a run, written down: a shell command executed in a
//! scratch git repository, expected to succeed, or (the bad-input case) to be refused with
//! a named message.
//!
//! `draft::build_package` runs the stage for every draft build (`ta run`, background build,
//! `ta draft build`), so it needs no human. `ta smoke` runs the same cases by hand.
//!
//! Isolation: the command runs in a temporary directory, with `HOME` pointed at it,
//! `TA_NO_KEYCHAIN=1` and `TA_TEST_NO_EXTERNAL_AGENTS=1`, so a case cannot spend money or
//! touch the developer's credentials. `CARGO_HOME`/`RUSTUP_HOME` keep their real values so
//! `cargo run` still works.

use std::path::Path;
use std::time::Duration;

use ta_changeset::draft_package::{ChangeType, ValidationEntry, VerificationWarning};
use ta_submit::{SmokeCase, SmokeConfig, SmokeExpect, SmokeOnFailure};

use super::verify::{run_single_command_ext, CommandLimits};

/// A file the draft changes.
#[derive(Debug, Clone)]
pub struct ChangedPath {
    pub path: String,
    pub change: ChangeType,
}

/// The outcome of one case.
#[derive(Debug, Clone)]
pub struct CaseResult {
    pub name: String,
    pub run: String,
    pub expect: SmokeExpect,
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub elapsed_secs: u64,
    /// Last lines of combined output.
    pub output_tail: String,
    /// Why the case failed (empty when it passed).
    pub problem: String,
}

/// Everything the stage found.
#[derive(Debug, Clone, Default)]
pub struct GateReport {
    pub results: Vec<CaseResult>,
    /// Configuration and coverage problems (not tied to one case's run).
    pub findings: Vec<String>,
}

impl GateReport {
    pub fn failed(&self) -> bool {
        !self.findings.is_empty() || self.results.iter().any(|r| !r.passed)
    }

    /// Nothing ran and nothing was found.
    pub fn is_empty(&self) -> bool {
        self.results.is_empty() && self.findings.is_empty()
    }

    /// Validation-log lines for the draft: one per case, one per finding, and the
    /// "smoke-tested the headline behaviour, including bad input" checklist entry.
    pub fn validation_entries(&self, cfg: &SmokeConfig) -> Vec<ValidationEntry> {
        let mut out = Vec::new();
        for r in &self.results {
            let kind = match r.expect {
                SmokeExpect::Success => "headline",
                SmokeExpect::Failure => "bad input",
            };
            let mut tail = r.output_tail.clone();
            if !r.problem.is_empty() {
                tail = format!("{}\n--\n{}", r.problem, tail);
            }
            out.push(ValidationEntry {
                command: format!("[smoke:{}] ({kind}) {}", r.name, r.run),
                exit_code: if r.passed {
                    0
                } else {
                    r.exit_code.filter(|c| *c != 0).unwrap_or(1)
                },
                duration_secs: r.elapsed_secs,
                stdout_tail: tail,
            });
        }
        for f in &self.findings {
            out.push(ValidationEntry {
                command: "[smoke:coverage]".to_string(),
                exit_code: 1,
                duration_secs: 0,
                stdout_tail: f.clone(),
            });
        }
        if !self.results.is_empty() || !self.findings.is_empty() {
            let headline = self
                .results
                .iter()
                .any(|r| r.passed && r.expect == SmokeExpect::Success);
            let bad_input = self
                .results
                .iter()
                .any(|r| r.passed && r.expect == SmokeExpect::Failure);
            // The checklist item holds when every case that applied passed. A new command
            // must additionally carry a bad-input case (`coverage_findings`), so a draft that
            // adds one without it already fails above.
            let ok = !self.failed();
            out.push(ValidationEntry {
                command:
                    "[smoke:checklist] smoke-tested the headline behaviour, including bad input"
                        .to_string(),
                exit_code: i32::from(!ok),
                duration_secs: 0,
                stdout_tail: format!(
                    "headline case passed: {headline}; bad-input (refusal) case passed: {bad_input}; \
                     cases run: {}; on_failure = {}",
                    self.results.len(),
                    cfg.on_failure
                ),
            });
        }
        out
    }

    /// The same failures as draft warnings (for `on_failure = "warn"`).
    pub fn warnings(&self) -> Vec<VerificationWarning> {
        let mut out: Vec<VerificationWarning> = self
            .results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| VerificationWarning {
                command: format!("[smoke:{}] {}", r.name, r.run),
                exit_code: r.exit_code,
                output: format!("{}\n--\n{}", r.problem, r.output_tail),
            })
            .collect();
        out.extend(self.findings.iter().map(|f| VerificationWarning {
            command: "[smoke:coverage]".to_string(),
            exit_code: None,
            output: f.clone(),
        }));
        out
    }

    /// Human-readable failure text for the blocked draft build.
    pub fn failure_text(&self) -> String {
        let mut s = String::new();
        for r in self.results.iter().filter(|r| !r.passed) {
            s.push_str(&format!(
                "  FAILED smoke case '{}': {}\n    command: {}\n",
                r.name, r.problem, r.run
            ));
            for line in r.output_tail.lines() {
                s.push_str(&format!("      {line}\n"));
            }
        }
        for f in &self.findings {
            s.push_str(&format!("  FAILED: {f}\n"));
        }
        s
    }
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    pattern == path
        || glob::Pattern::new(pattern)
            .map(|p| p.matches_with(path, opts))
            .unwrap_or(false)
}

/// Cases that apply to a draft changing `changed`: cases without `covers`, and cases
/// whose `covers` matches a changed path.
pub fn cases_to_run<'a>(cfg: &'a SmokeConfig, changed: &[ChangedPath]) -> Vec<&'a SmokeCase> {
    cfg.cases
        .iter()
        .filter(|c| match &c.covers {
            None => true,
            Some(p) => changed.iter().any(|ch| glob_matches(p, &ch.path)),
        })
        .collect()
}

/// A newly added file matching `new_command_paths` is a new command: it needs a smoke
/// case that covers it, and one of those cases must be a refusal case.
pub fn coverage_findings(cfg: &SmokeConfig, changed: &[ChangedPath]) -> Vec<String> {
    let mut out = Vec::new();
    for ch in changed.iter().filter(|c| c.change == ChangeType::Add) {
        if !cfg
            .new_command_paths
            .iter()
            .any(|p| glob_matches(p, &ch.path))
        {
            continue;
        }
        let covering: Vec<&SmokeCase> = cfg
            .cases
            .iter()
            .filter(|c| {
                c.covers
                    .as_deref()
                    .is_some_and(|p| glob_matches(p, &ch.path))
            })
            .collect();
        if covering.is_empty() {
            out.push(format!(
                "this draft adds {path}, a new command file (it matches [smoke] new_command_paths), \
                 but no smoke case covers it. Add a [[smoke.cases]] entry with covers = \"{path}\" \
                 to .ta/workflow.toml that runs the new command for real, and a second one with \
                 expect = \"failure\" and an expect_output that feeds it bad input.",
                path = ch.path
            ));
        } else if !covering.iter().any(|c| c.expect == SmokeExpect::Failure) {
            out.push(format!(
                "{path} is a new command file and its smoke case(s) ({}) only test the good path. \
                 Add a [[smoke.cases]] entry with covers = \"{path}\", expect = \"failure\" and an \
                 expect_output that feeds the command bad input and checks the refusal message.",
                covering
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                path = ch.path
            ));
        }
    }
    out
}

fn tail(output: &str, n: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// The runner's timeout/hang error without its trailing `[[verify.commands]]` advice, which
/// does not apply to smoke cases (they are tuned with `timeout_secs` on the case).
fn runner_error_text(e: &anyhow::Error) -> String {
    let text = e.to_string();
    match text.find("\n\nTo ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// Run one case in a fresh scratch git repository.
///
/// `project` is the tree under test (a goal's staging copy, or the project itself) and is
/// exported to the case as `TA_SMOKE_PROJECT`; `TA_SMOKE_SCRATCH` is the scratch repo (also the
/// working directory); `TA_SMOKE_TA` is a command prefix that runs this project's `ta` (unless the
/// caller already set it, for example to a prebuilt binary).
pub fn run_case(
    case: &SmokeCase,
    default_timeout_secs: u64,
    project: &Path,
    base_env: &[(String, String)],
) -> CaseResult {
    let started = std::time::Instant::now();
    let mut result = CaseResult {
        name: case.name.clone(),
        run: case.run.clone(),
        expect: case.expect,
        passed: false,
        exit_code: None,
        elapsed_secs: 0,
        output_tail: String::new(),
        problem: String::new(),
    };
    let scratch = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            result.problem = format!(
                "could not create the scratch directory for the smoke case: {e}. \
                 Free disk space or set TMPDIR to a writable directory."
            );
            return result;
        }
    };
    let repo = scratch.path().join("repo");
    let home = scratch.path().join("home");
    if let Err(e) = std::fs::create_dir_all(&repo).and_then(|()| std::fs::create_dir_all(&home)) {
        result.problem = format!("could not prepare the scratch repository: {e}.");
        return result;
    }

    let mut env: Vec<(String, String)> = base_env.to_vec();
    let real_home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let mut set = |k: &str, v: String| {
        env.retain(|(ek, _)| ek != k);
        env.push((k.to_string(), v));
    };
    for (k, fallback) in [(".cargo", "CARGO_HOME"), (".rustup", "RUSTUP_HOME")] {
        if std::env::var_os(fallback).is_none() && !real_home.is_empty() {
            set(
                fallback,
                Path::new(&real_home).join(k).display().to_string(),
            );
        }
    }
    set("HOME", home.display().to_string());
    set("TA_NO_KEYCHAIN", "1".to_string());
    set("TA_TEST_NO_EXTERNAL_AGENTS", "1".to_string());
    set("TA_SMOKE_PROJECT", project.display().to_string());
    set("TA_SMOKE_SCRATCH", repo.display().to_string());
    for (k, v) in [
        ("GIT_AUTHOR_NAME", "TA Smoke"),
        ("GIT_AUTHOR_EMAIL", "smoke@ta.invalid"),
        ("GIT_COMMITTER_NAME", "TA Smoke"),
        ("GIT_COMMITTER_EMAIL", "smoke@ta.invalid"),
    ] {
        set(k, v.to_string());
    }
    if std::env::var_os("TA_SMOKE_TA").is_none() && project.join("apps/ta-cli/Cargo.toml").is_file()
    {
        set(
            "TA_SMOKE_TA",
            format!(
                "cargo run --quiet --manifest-path {} -p ta-cli --bin ta --",
                project.join("Cargo.toml").display()
            ),
        );
    }

    let limits = |secs: u64| CommandLimits {
        timeout: Duration::from_secs(secs),
        no_output_timeout: None,
        heartbeat_interval: Duration::from_secs(60),
    };
    let timeout = case.timeout_secs.unwrap_or(default_timeout_secs).max(1);
    let hint = format!(
        "To allow more time set timeout_secs for smoke case '{}' in .ta/workflow.toml (now {timeout}).",
        case.name
    );

    // The scratch repo: a git repository with one commit, so commands that need a
    // clean tree, a branch or a start commit have one.
    let init =
        "git init -q && echo scratch > README.md && git add README.md && git commit -q -m init";
    let mut steps: Vec<&str> = vec![init];
    steps.extend(case.setup.iter().map(String::as_str));
    for (i, step) in steps.iter().enumerate() {
        match run_single_command_ext(
            step,
            &repo,
            &limits(timeout),
            (i + 1, steps.len() + 1),
            &env,
        ) {
            Ok(out) if out.success => {}
            Ok(out) => {
                result.exit_code = out.exit_code;
                result.output_tail = tail(&out.combined_output, 20);
                result.problem = format!(
                    "setup step `{step}` failed (exit {}), so the case could not run. Fix the `setup` of \
                     smoke case '{}' in .ta/workflow.toml.",
                    out.exit_code.map_or("signal".to_string(), |c| c.to_string()),
                    case.name
                );
                result.elapsed_secs = started.elapsed().as_secs();
                return result;
            }
            Err(e) => {
                result.output_tail = tail(&runner_error_text(&e), 20);
                result.problem = format!("setup step `{step}` could not finish: {hint}");
                result.elapsed_secs = started.elapsed().as_secs();
                return result;
            }
        }
    }

    match run_single_command_ext(
        &case.run,
        &repo,
        &limits(timeout),
        (steps.len() + 1, steps.len() + 1),
        &env,
    ) {
        Err(e) => {
            result.output_tail = tail(&runner_error_text(&e), 20);
            result.problem = format!("the command did not finish (timed out or hung). {hint}");
        }
        Ok(out) => {
            result.exit_code = out.exit_code;
            result.output_tail = tail(&out.combined_output, 20);
            let has_text = case
                .expect_output
                .as_deref()
                .is_none_or(|t| out.combined_output.contains(t));
            let missing = case.expect_output.as_deref().unwrap_or("");
            match case.expect {
                SmokeExpect::Success if !out.success => {
                    result.problem = format!(
                        "expected success but the command exited with {}. This is behaviour a unit \
                         test did not catch: run the command by hand the same way and fix it.",
                        out.exit_code.map_or("a signal".to_string(), |c| format!("code {c}"))
                    );
                }
                SmokeExpect::Failure if out.success => {
                    result.problem =
                        "expected the command to REFUSE this bad input (exit non-zero) \
                         but it succeeded. The input check is missing or too weak."
                            .to_string();
                }
                SmokeExpect::Failure if out.exit_code.is_none() => {
                    result.problem =
                        "expected a refusal but the command was killed by a signal.".to_string();
                }
                _ if !has_text => {
                    result.problem = format!(
                        "the command's output does not contain the expected text `{missing}` \
                         (expect_output of smoke case '{}'). Either the message changed (update \
                         expect_output) or the command failed for a different reason (read the output).",
                        case.name
                    );
                }
                _ => result.passed = true,
            }
        }
    }
    result.elapsed_secs = started.elapsed().as_secs();
    result
}

/// Attach `incoming` to a draft's validation log without dropping what the build already
/// recorded (the smoke entries). An entry already present (same command) is not added twice,
/// so attaching the same log from two paths is harmless.
pub fn merge_validation_log(existing: &mut Vec<ValidationEntry>, incoming: &[ValidationEntry]) {
    for e in incoming {
        if !existing.iter().any(|x| x.command == e.command) {
            existing.push(e.clone());
        }
    }
}

/// Same as [`merge_validation_log`] for verification warnings.
pub fn merge_warnings(existing: &mut Vec<VerificationWarning>, incoming: &[VerificationWarning]) {
    for w in incoming {
        if !existing.iter().any(|x| x.command == w.command) {
            existing.push(w.clone());
        }
    }
}

/// Run the stage for a draft: configuration problems, coverage, then every case that applies.
///
/// `only` limits the run to the named cases (manual `ta smoke --case`); with `run_all` the
/// `covers` filter is ignored.
pub fn run_gate(
    cfg: &SmokeConfig,
    project: &Path,
    changed: &[ChangedPath],
    base_env: &[(String, String)],
    run_all: bool,
    only: &[String],
) -> GateReport {
    let mut report = GateReport {
        findings: cfg.problems(),
        ..GateReport::default()
    };
    if !report.findings.is_empty() {
        return report;
    }
    let selected: Vec<&SmokeCase> = if run_all {
        cfg.cases.iter().collect()
    } else {
        cases_to_run(cfg, changed)
    };
    let selected: Vec<&SmokeCase> = selected
        .into_iter()
        .filter(|c| only.is_empty() || only.contains(&c.name))
        .collect();
    if run_all {
        // Manual runs check what was asked for, not what a draft is missing.
    } else {
        report.findings.extend(coverage_findings(cfg, changed));
    }
    println!(
        "\n  Smoke stage: {} case(s) in a scratch repository (on_failure = {}).",
        selected.len(),
        cfg.on_failure
    );
    for (i, case) in selected.iter().enumerate() {
        println!(
            "    [{}/{}] smoke case '{}'",
            i + 1,
            selected.len(),
            case.name
        );
        let r = run_case(case, cfg.default_timeout_secs, project, base_env);
        println!(
            "    {} smoke case '{}' ({}s){}",
            if r.passed { "PASS" } else { "FAIL" },
            r.name,
            r.elapsed_secs,
            if r.passed {
                String::new()
            } else {
                format!(": {}", r.problem)
            }
        );
        if !r.passed {
            tracing::warn!(case = %r.name, exit_code = ?r.exit_code, problem = %r.problem, "smoke case failed");
        }
        report.results.push(r);
    }
    report
}

/// `ta smoke`: run the project's smoke cases by hand.
pub fn execute(project: &Path, case_names: &[String], list: bool) -> anyhow::Result<()> {
    let cfg = ta_submit::WorkflowConfig::load_or_default(&project.join(".ta/workflow.toml")).smoke;
    if cfg.cases.is_empty() {
        println!(
            "No smoke cases configured in {}.\nAdd [[smoke.cases]] entries to .ta/workflow.toml \
             (see the \"Smoke cases\" section of docs/USAGE.md).",
            project.join(".ta/workflow.toml").display()
        );
        return Ok(());
    }
    if list {
        for c in &cfg.cases {
            println!(
                "{:<40} expect={:<8} covers={}",
                c.name,
                match c.expect {
                    SmokeExpect::Success => "success",
                    SmokeExpect::Failure => "failure",
                },
                c.covers.as_deref().unwrap_or("(every draft)")
            );
        }
        return Ok(());
    }
    for n in case_names {
        if !cfg.cases.iter().any(|c| &c.name == n) {
            anyhow::bail!(
                "no smoke case named '{n}' in .ta/workflow.toml. Run `ta smoke --list` to see the names."
            );
        }
    }
    let env = super::verify::goal_cargo_env(project);
    let report = run_gate(&cfg, project, &[], &env, true, case_names);
    if report.failed() {
        println!("\nSmoke FAILED:\n{}", report.failure_text());
        anyhow::bail!(
            "smoke stage failed ({} case(s) ran). Fix the failures above and re-run `ta smoke`.",
            report.results.len()
        );
    }
    println!("\nSmoke passed ({} case(s)).", report.results.len());
    Ok(())
}

/// What `build_package` does with a report.
pub fn blocks_draft(cfg: &SmokeConfig, report: &GateReport) -> bool {
    report.failed() && cfg.on_failure == SmokeOnFailure::Block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(
        name: &str,
        covers: Option<&str>,
        run: &str,
        expect: SmokeExpect,
        out: Option<&str>,
    ) -> SmokeCase {
        SmokeCase {
            name: name.to_string(),
            covers: covers.map(str::to_string),
            setup: vec![],
            run: run.to_string(),
            expect,
            expect_output: out.map(str::to_string),
            timeout_secs: Some(60),
        }
    }

    fn add(path: &str) -> ChangedPath {
        ChangedPath {
            path: path.to_string(),
            change: ChangeType::Add,
        }
    }

    fn cfg(cases: Vec<SmokeCase>) -> SmokeConfig {
        SmokeConfig {
            new_command_paths: vec!["apps/ta-cli/src/commands/*.rs".to_string()],
            cases,
            ..SmokeConfig::default()
        }
    }

    #[test]
    fn glob_does_not_cross_directories() {
        assert!(glob_matches("apps/x/*.rs", "apps/x/a.rs"));
        assert!(!glob_matches("apps/x/*.rs", "apps/x/sub/a.rs"));
        assert!(glob_matches("apps/x/**/*.rs", "apps/x/sub/a.rs"));
        assert!(glob_matches("exact.rs", "exact.rs"));
    }

    #[test]
    fn cases_run_when_uncovered_or_their_path_changed() {
        let c = cfg(vec![
            case("always", None, "true", SmokeExpect::Success, None),
            case(
                "golden",
                Some("apps/ta-cli/src/commands/golden.rs"),
                "true",
                SmokeExpect::Success,
                None,
            ),
        ]);
        let none = cases_to_run(&c, &[]);
        assert_eq!(none.len(), 1);
        let hit = cases_to_run(
            &c,
            &[ChangedPath {
                path: "apps/ta-cli/src/commands/golden.rs".into(),
                change: ChangeType::Modify,
            }],
        );
        assert_eq!(hit.len(), 2);
    }

    #[test]
    fn a_new_command_file_without_a_case_is_refused_with_the_fix() {
        let c = cfg(vec![]);
        let f = coverage_findings(&c, &[add("apps/ta-cli/src/commands/newthing.rs")]);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("[[smoke.cases]]"), "{}", f[0]);
        assert!(
            f[0].contains("covers = \"apps/ta-cli/src/commands/newthing.rs\""),
            "{}",
            f[0]
        );
    }

    #[test]
    fn a_new_command_needs_a_bad_input_case_too() {
        let p = "apps/ta-cli/src/commands/newthing.rs";
        let good_only = cfg(vec![case("g", Some(p), "true", SmokeExpect::Success, None)]);
        let f = coverage_findings(&good_only, &[add(p)]);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("bad input"), "{}", f[0]);
        let both = cfg(vec![
            case("g", Some(p), "true", SmokeExpect::Success, None),
            case("b", Some(p), "false", SmokeExpect::Failure, Some("x")),
        ]);
        assert!(coverage_findings(&both, &[add(p)]).is_empty());
    }

    #[test]
    fn modified_or_unrelated_files_need_no_case() {
        let c = cfg(vec![]);
        assert!(coverage_findings(&c, &[add("docs/USAGE.md")]).is_empty());
        assert!(coverage_findings(
            &c,
            &[ChangedPath {
                path: "apps/ta-cli/src/commands/draft.rs".into(),
                change: ChangeType::Modify
            }]
        )
        .is_empty());
        assert!(coverage_findings(&c, &[add("apps/ta-cli/src/commands/sub/deep.rs")]).is_empty());
    }

    #[test]
    fn a_failure_case_without_expect_output_is_a_config_problem() {
        let c = cfg(vec![case("b", None, "false", SmokeExpect::Failure, None)]);
        let p = c.problems();
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("expect_output"), "{}", p[0]);
    }

    #[cfg(unix)]
    mod run {
        use super::*;
        use tempfile::tempdir;

        #[test]
        fn a_passing_headline_case_runs_in_a_scratch_git_repo() {
            let project = tempdir().unwrap();
            let c = case(
                "h",
                None,
                "git rev-parse --is-inside-work-tree && test -f README.md && echo \"home=$HOME\" && test \"$TA_TEST_NO_EXTERNAL_AGENTS\" = 1 && test \"$TA_NO_KEYCHAIN\" = 1",
                SmokeExpect::Success,
                Some("true"),
            );
            let r = run_case(&c, 60, project.path(), &[]);
            assert!(r.passed, "{r:?}");
            assert!(r.output_tail.contains("home="), "{}", r.output_tail);
            let real_home = std::env::var("HOME").unwrap_or_default();
            assert!(
                r.output_tail.lines().any(|l| l.starts_with("home=")
                    && l.ends_with("/home")
                    && l != format!("home={real_home}")),
                "{}",
                r.output_tail
            );
        }

        #[test]
        fn a_refusal_case_passes_only_when_the_text_is_there() {
            let project = tempdir().unwrap();
            let ok = case(
                "b",
                None,
                "echo 'hash no longer matches' >&2; exit 3",
                SmokeExpect::Failure,
                Some("no longer matches"),
            );
            assert!(run_case(&ok, 60, project.path(), &[]).passed);

            let wrong_reason = case(
                "b",
                None,
                "echo 'panicked at x' >&2; exit 101",
                SmokeExpect::Failure,
                Some("no longer matches"),
            );
            let r = run_case(&wrong_reason, 60, project.path(), &[]);
            assert!(!r.passed);
            assert!(r.problem.contains("no longer matches"), "{}", r.problem);

            let accepted = case(
                "b",
                None,
                "echo 'no longer matches'",
                SmokeExpect::Failure,
                Some("no longer matches"),
            );
            let r = run_case(&accepted, 60, project.path(), &[]);
            assert!(!r.passed);
            assert!(r.problem.contains("REFUSE"), "{}", r.problem);
        }

        #[test]
        fn a_failing_headline_case_reports_the_exit_code() {
            let project = tempdir().unwrap();
            let r = run_case(
                &case("h", None, "echo boom; exit 7", SmokeExpect::Success, None),
                60,
                project.path(),
                &[],
            );
            assert!(!r.passed);
            assert_eq!(r.exit_code, Some(7));
            assert!(r.problem.contains("code 7"), "{}", r.problem);
            assert!(r.output_tail.contains("boom"));
        }

        #[test]
        fn setup_runs_in_the_scratch_repo_and_a_failing_setup_names_the_step() {
            let project = tempdir().unwrap();
            let mut c = case("s", None, "test -f made.txt", SmokeExpect::Success, None);
            c.setup = vec!["echo hi > made.txt".to_string()];
            assert!(run_case(&c, 60, project.path(), &[]).passed);
            c.setup = vec!["exit 4".to_string()];
            let r = run_case(&c, 60, project.path(), &[]);
            assert!(!r.passed);
            assert!(r.problem.contains("setup step `exit 4`"), "{}", r.problem);
        }

        #[test]
        fn a_timeout_names_the_key_to_raise() {
            let project = tempdir().unwrap();
            let mut c = case("slow", None, "sleep 30", SmokeExpect::Success, None);
            c.timeout_secs = Some(1);
            let r = run_case(&c, 60, project.path(), &[]);
            assert!(!r.passed);
            assert!(r.problem.contains("timeout_secs"), "{}", r.problem);
            assert!(r.problem.contains("slow"), "{}", r.problem);
        }

        #[test]
        fn the_gate_records_cases_and_the_checklist_in_the_validation_log() {
            let project = tempdir().unwrap();
            let c = cfg(vec![
                case("good", None, "true", SmokeExpect::Success, None),
                case(
                    "bad",
                    None,
                    "echo refused; exit 2",
                    SmokeExpect::Failure,
                    Some("refused"),
                ),
            ]);
            let report = run_gate(&c, project.path(), &[], &[], false, &[]);
            assert!(!report.failed(), "{report:?}");
            let entries = report.validation_entries(&c);
            assert_eq!(entries.len(), 3);
            assert!(entries.iter().all(|e| e.exit_code == 0));
            assert!(entries[2]
                .command
                .contains("smoke-tested the headline behaviour, including bad input"));
            assert!(!blocks_draft(&c, &report));
        }

        #[test]
        fn a_failing_case_blocks_by_default_and_only_warns_when_configured() {
            let project = tempdir().unwrap();
            let mut c = cfg(vec![case(
                "good",
                None,
                "exit 1",
                SmokeExpect::Success,
                None,
            )]);
            let report = run_gate(&c, project.path(), &[], &[], false, &[]);
            assert!(report.failed());
            assert!(blocks_draft(&c, &report));
            let text = report.failure_text();
            assert!(text.contains("smoke case 'good'"), "{text}");
            let entries = report.validation_entries(&c);
            assert!(entries.iter().any(|e| e.exit_code != 0));
            // The checklist entry fails: no bad-input case passed.
            assert_eq!(entries.last().unwrap().exit_code, 1);
            c.on_failure = SmokeOnFailure::Warn;
            assert!(!blocks_draft(&c, &report));
            assert_eq!(report.warnings().len(), 1);
        }

        #[test]
        fn manual_runs_ignore_covers_and_honour_case_names() {
            let project = tempdir().unwrap();
            let c = cfg(vec![
                case(
                    "a",
                    Some("never/changed.rs"),
                    "true",
                    SmokeExpect::Success,
                    None,
                ),
                case(
                    "b",
                    Some("never/changed.rs"),
                    "true",
                    SmokeExpect::Success,
                    None,
                ),
            ]);
            let all = run_gate(&c, project.path(), &[], &[], true, &[]);
            assert_eq!(all.results.len(), 2);
            let one = run_gate(&c, project.path(), &[], &[], true, &["b".to_string()]);
            assert_eq!(one.results.len(), 1);
            assert_eq!(one.results[0].name, "b");
            let none = run_gate(&c, project.path(), &[], &[], false, &[]);
            assert!(none.results.is_empty());
        }
    }
}
