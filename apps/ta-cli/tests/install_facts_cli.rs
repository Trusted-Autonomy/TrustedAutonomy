// install_facts_cli.rs — Black-box proof of the machine-readable install facts
// an installer relies on (v0.17.12.5.1): `ta capabilities --json`,
// `ta terms-status --json`, `--secret-stdin`, and the `--json` modes of
// `ta init` / `ta setup vcs` / `ta daemon status`.
//
// Runs the real compiled `ta` binary against throwaway directories with HOME
// and the XDG dirs redirected, so nothing in the real home, Keychain or any
// running daemon is touched. No daemon is ever started: `daemon status` runs
// against an empty project, where nothing answers.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// A throwaway home + project. Every `ta` call sees only these.
struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: TempDir::new().unwrap(),
            project: TempDir::new().unwrap(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let home = self.home.path();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ta"));
        cmd.arg("--project-root")
            .arg(self.project.path())
            .arg("--no-version-check")
            .args(args)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("TA_NO_KEYCHAIN", "1")
            .env_remove("TA_ACCEPT_TERMS");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("failed to run ta")
    }

    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Output {
        use std::io::Write;
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn ta");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn accept_terms(&self) {
        let out = self.run(&["accept-terms", "--yes"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// stdout must be exactly one JSON document and nothing else.
fn stdout_json(out: &Output) -> serde_json::Value {
    let stdout = text(&out.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not a single JSON document ({e}):\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            text(&out.stderr)
        )
    })
}

#[test]
fn capabilities_json_is_one_document_on_stdout() {
    let sb = Sandbox::new();
    let out = sb.run(&["capabilities", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v = stdout_json(&out);
    assert_eq!(v["capabilities_schema"], 1);
    assert_eq!(v["schema_version"], 1);
    for key in [
        "daemon_passes_restricted_flags",
        "supports_chat_mode",
        "supports_origin",
        "rules_engine",
        "outcome_roles",
        "nats_credentials",
        "terms_status",
        "atomic_apply",
    ] {
        assert!(v[key].is_boolean(), "{key} must be a boolean: {v}");
    }
    assert_eq!(v["platform"]["os"], std::env::consts::OS);
    assert_eq!(v["platform"]["arch"], std::env::consts::ARCH);
    // This binary's own surface is probed in-process, so these are true here.
    for key in [
        "supports_chat_mode",
        "supports_origin",
        "rules_engine",
        "atomic_apply",
        "terms_status",
    ] {
        assert_eq!(v[key], true, "{key}: {}", v["evidence"][key]);
    }
    // The daemon answer is never assumed: a daemon that cannot vouch for the
    // flags (absent, or older than the probe) leaves the capability false.
    let state = v["daemon_probe"]["state"].as_str().unwrap();
    assert!(
        [
            "ok",
            "binary_not_found",
            "probe_unsupported",
            "probe_failed"
        ]
        .contains(&state),
        "{state}"
    );
    if state != "ok" {
        for key in [
            "daemon_passes_restricted_flags",
            "outcome_roles",
            "nats_credentials",
        ] {
            assert_eq!(
                v[key], false,
                "{key} must be false when the daemon is `{state}`"
            );
        }
        assert!(!v["daemon_probe"]["detail"].as_str().unwrap().is_empty());
    }
    assert!(
        !text(&out.stderr).contains("capabilities_schema"),
        "the document belongs on stdout only"
    );
}

#[test]
fn capabilities_text_mode_is_for_humans() {
    let sb = Sandbox::new();
    let out = sb.run(&["capabilities"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("supports_chat_mode"), "{stdout}");
    assert!(stdout.contains("ta capabilities --json"), "{stdout}");
    assert!(serde_json::from_str::<serde_json::Value>(&stdout).is_err());
}

#[test]
fn terms_status_json_tracks_acceptance_and_keeps_the_text_form() {
    let sb = Sandbox::new();

    let out = sb.run(&["terms-status", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let before = stdout_json(&out);
    assert_eq!(before["schema_version"], 1);
    assert_eq!(before["status"], "not_accepted");
    let sha = before["document_sha256"].as_str().unwrap();
    assert_eq!(sha.len(), 64);
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(before["document_version"].is_string());
    assert!(before["accepted_document_sha256"].is_null());
    assert!(before["next_step"]
        .as_str()
        .unwrap()
        .contains("accept-terms"));

    sb.accept_terms();
    let out = sb.run(&["terms-status", "--json"]);
    let after = stdout_json(&out);
    assert_eq!(after["status"], "current");
    assert_eq!(after["accepted_document_sha256"], before["document_sha256"]);
    assert_eq!(
        after["accepted_document_version"],
        before["document_version"]
    );
    assert_eq!(after["legacy_record"], false);
    assert!(after["accepted_at"].is_string());

    // The text form the shell installer greps is unchanged.
    let human = text(&sb.run(&["terms-status"]).stdout);
    assert!(human.contains("Status: current"), "{human}");
}

#[test]
fn a_changed_terms_text_makes_the_recorded_acceptance_stale() {
    let sb = Sandbox::new();
    sb.accept_terms();
    let status = stdout_json(&sb.run(&["terms-status", "--json"]));
    let record = std::path::PathBuf::from(status["record_path"].as_str().unwrap());
    assert!(
        record.starts_with(sb.home.path()),
        "the record must live under the sandbox home, not the real one: {}",
        record.display()
    );
    let raw = std::fs::read_to_string(&record)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", record.display()));
    let current_sha = status["document_sha256"].as_str().unwrap().to_string();
    // Pretend the user accepted an earlier text.
    let edited = raw.replace(&current_sha, &"0".repeat(64));
    assert_ne!(edited, raw, "the record should hold the full hash: {raw}");
    std::fs::write(&record, edited).unwrap();

    let v = stdout_json(&sb.run(&["terms-status", "--json"]));
    assert_eq!(v["status"], "stale", "{v}");
    assert!(v["next_step"].as_str().unwrap().contains("accept-terms"));
}

#[test]
fn a_stdin_secret_is_stored_and_never_echoed() {
    const SECRET: &str = "tok-from-stdin-Zk93qLmP";
    let sb = Sandbox::new();
    let out = sb.run_with_stdin(
        &[
            "credentials",
            "add",
            "--name",
            "probe",
            "--service",
            "svc",
            "--secret-stdin",
        ],
        &format!("{SECRET}\n"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(stdout.contains("Credential added"), "{stdout}");
    assert!(!stdout.contains(SECRET) && !stderr.contains(SECRET));
    assert!(
        !stderr.contains("warning: --secret"),
        "stdin must not trigger the argv warning: {stderr}"
    );
    assert!(
        !tree_contains(sb.project.path(), SECRET),
        "clear text on disk"
    );

    // The same secret on argv warns, and the warning does not repeat it.
    let out = sb.run(&[
        "credentials",
        "add",
        "--name",
        "argv",
        "--service",
        "svc",
        "--secret",
        SECRET,
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("warning: --secret"), "{stderr}");
    assert!(!stderr.contains(SECRET) && !text(&out.stdout).contains(SECRET));
}

#[test]
fn credentials_add_without_a_secret_source_fails_with_the_options() {
    let sb = Sandbox::new();
    let out = sb.run(&["credentials", "add", "--name", "n", "--service", "s"]);
    assert!(!out.status.success());
    let stderr = text(&out.stderr);
    assert!(stderr.contains("--secret-stdin"), "{stderr}");
    assert!(out.stdout.is_empty());
}

#[test]
fn an_unset_secret_env_variable_is_named_in_the_error() {
    let sb = Sandbox::new();
    let out = sb
        .command(&[
            "credentials",
            "add",
            "--name",
            "n",
            "--service",
            "s",
            "--secret-env",
            "TA_PROBE_TOKEN_UNSET",
        ])
        .env_remove("TA_PROBE_TOKEN_UNSET")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("TA_PROBE_TOKEN_UNSET"));
}

#[test]
fn init_json_prints_only_the_document_on_stdout() {
    let sb = Sandbox::new();
    // First run, terms not yet accepted: the terms notice must go to stderr.
    let out = sb.run(&["init", "--json"]);
    if out.status.success() {
        let v = stdout_json(&out);
        assert!(v["schema_version"].is_number(), "{v}");
    } else {
        // Acceptance is required first; stdout must still stay empty.
        assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
        assert!(
            text(&out.stderr).contains("accept-terms"),
            "{}",
            text(&out.stderr)
        );
        sb.accept_terms();
        let out = sb.run(&["init", "--json"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        let v = stdout_json(&out);
        assert!(v["schema_version"].is_number(), "{v}");
    }
}

#[test]
fn setup_vcs_dry_run_json_prints_only_the_document_and_writes_nothing() {
    let sb = Sandbox::new();
    let out = sb.run(&["setup", "vcs", "--dry-run", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v = stdout_json(&out);
    assert!(v["schema_version"].is_number(), "{v}");
    assert_eq!(v["dry_run"], true);
    assert!(!sb.project.path().join(".gitignore").exists());
}

#[test]
fn daemon_status_json_on_an_empty_project_reports_not_running() {
    let sb = Sandbox::new();
    let out = sb.run(&["daemon", "status", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v = stdout_json(&out);
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["action"], "status");
    assert_eq!(v["state"], "not_running");
    assert_eq!(v["running"], false);
    assert!(v["pid"].is_null());
    assert!(v["pid_file"].as_str().unwrap().ends_with("daemon.pid"));
    assert!(v["log_file"].as_str().unwrap().ends_with("daemon.log"));
    assert!(v["url"].as_str().unwrap().starts_with("http://"));
    assert!(v["detail"].as_str().unwrap().contains("ta daemon start"));
}

#[test]
fn daemon_start_foreground_json_is_refused_before_starting_anything() {
    let sb = Sandbox::new();
    let out = sb.run(&["daemon", "start", "--foreground", "--json"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert!(text(&out.stderr).contains("--foreground --json"));
    assert!(!sb.project.path().join(".ta/daemon.pid").exists());
}

/// True when any file under `root` contains `needle`.
fn tree_contains(root: &Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|e| {
        let p = e.path();
        if p.is_dir() {
            tree_contains(&p, needle)
        } else {
            std::fs::read(&p)
                .map(|b| String::from_utf8_lossy(&b).contains(needle))
                .unwrap_or(false)
        }
    })
}
