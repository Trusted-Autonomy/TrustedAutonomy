// agent_login_cli.rs — Black-box proof of `ta agent-login` (list / login / logout /
// verify) with the real compiled `ta` binary.
//
// Hermetic: HOME, the XDG dirs and TA_USER_STATE_DIR point at throwaway
// directories, TA_NO_KEYCHAIN=1 keeps the real Keychain out, no daemon runs, and
// no real `claude`/`codex` is ever started. The "vendor" login command is a shell
// script in a temp directory (deliberately not named after a paid agent), wired in
// through the catalog override file exactly as a user would. Verify talks to a
// local fake HTTP server.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

/// A token as `claude setup-token` prints it, with a sentinel that must never leak.
const TOKEN: &str = concat!(
    "sk-ant-",
    "oat01-SENTINELtokenABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz_-ABCDEFGHIJ"
);

struct Sandbox {
    home: TempDir,
    project: TempDir,
    state: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: TempDir::new().unwrap(),
            project: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let home = self.home.path();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ta"));
        cmd.arg("--project-root")
            .arg(self.project.path())
            .arg("--no-version-check")
            .arg("agent-login")
            .args(args)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("TA_USER_STATE_DIR", self.state.path())
            .env("TA_NO_KEYCHAIN", "1")
            .env_remove("TA_ACCEPT_TERMS")
            .env_remove("TA_AGENT_LOGIN_TIMEOUT_SECS")
            .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("OPENAI_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("GOOGLE_API_KEY")
            .env_remove("XAI_API_KEY");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args)
            .stdin(Stdio::null())
            .output()
            .expect("failed to run ta")
    }

    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Output {
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

    fn write_catalog(&self, toml: &str) {
        let dir = self.home.path().join(".config").join("ta");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent-frameworks.toml"), toml).unwrap();
    }

    fn secret_file(&self, id: &str) -> PathBuf {
        self.state
            .path()
            .join("agent-logins")
            .join(format!("{id}.secret"))
    }

    /// Every file under the sandbox (home, project, state) except the secret
    /// files themselves, as text: the places a secret must never reach.
    fn text_outside_secret_files(&self) -> String {
        fn walk(dir: &Path, out: &mut String) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().and_then(|x| x.to_str()) != Some("secret") {
                    out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
                }
            }
        }
        let mut all = String::new();
        walk(self.home.path(), &mut all);
        walk(self.project.path(), &mut all);
        walk(self.state.path(), &mut all);
        all
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn stdout_json(out: &Output) -> serde_json::Value {
    let stdout = text(&out.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not a single JSON document ({e}):\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            text(&out.stderr)
        )
    })
}

fn state_of(doc: &serde_json::Value, id: &str) -> String {
    doc["frameworks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing in {doc}"))["state"]
        .as_str()
        .unwrap()
        .to_string()
}

// ── Fake vendor HTTP endpoint ──────────────────────────────────────────────

/// A local HTTP server answering every request with one status and recording the
/// request headers it saw.
struct FakeVendor {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

fn fake_vendor(status: u16) -> FakeVendor {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/check", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in_thread = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            seen_in_thread
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf).to_string());
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
        }
    });
    FakeVendor { url, seen }
}

/// A catalog with one key framework verified against `url`.
fn key_catalog(url: &str) -> String {
    format!(
        r#"
[[framework]]
id = "fake-key"
label = "Fake Key"
kind = "api_key"
how = "Paste the key."
agents = ["fake-agent"]
env = ["FAKE_VENDOR_API_KEY"]

[framework.login]
method = "prompt"

[framework.verify]
method = "http_get"
url = "{url}"
auth = "header"
header = "x-api-key"
invalid_status = [401]
"#
    )
}

#[test]
fn list_json_is_cheap_and_has_the_contract_shape() {
    let sb = Sandbox::new();
    let out = sb.run(&["list", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let doc = stdout_json(&out);
    assert_eq!(doc["active"], "claude-code");
    for want in [
        "claude-code",
        "anthropic-api-key",
        "openai",
        "gemini",
        "xai",
    ] {
        assert_eq!(state_of(&doc, want), "missing", "{want}");
    }
    let claude = &doc["frameworks"][0];
    for key in [
        "id",
        "label",
        "kind",
        "method",
        "how",
        "state",
        "checked_at",
        "detail",
    ] {
        assert!(claude.get(key).is_some(), "{key} missing: {claude}");
    }
    // Human output works too and says how to log in.
    let human = sb.run(&["list"]);
    assert!(human.status.success());
    assert!(text(&human.stdout).contains("ta agent-login login"));
}

#[test]
fn login_via_stdin_then_verify_logout_with_a_fake_vendor() {
    let sb = Sandbox::new();
    let vendor = fake_vendor(200);
    sb.write_catalog(&key_catalog(&vendor.url));
    let key = "SENTINELfakekey0123456789";

    // Not logged in yet.
    let doc = stdout_json(&sb.run(&["list", "--json"]));
    assert_eq!(state_of(&doc, "fake-key"), "missing");

    // Login stores, verifies with a real HTTP request, reports ok.
    let out = sb.run_with_stdin(
        &["login", "fake-key", "--secret-stdin", "--json"],
        &format!("{key}\n"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v = stdout_json(&out);
    assert_eq!(v["id"], "fake-key");
    assert_eq!(v["state"], "ok");
    assert!(v["checked_at"].as_i64().unwrap() > 1_600_000_000);
    {
        let seen = vendor.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(
            seen[0]
                .to_lowercase()
                .contains(&format!("x-api-key: {}", key.to_lowercase())),
            "the key must reach the vendor in the configured header: {}",
            seen[0]
        );
    }
    assert_eq!(
        std::fs::read_to_string(sb.secret_file("fake-key")).unwrap(),
        key
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(sb.secret_file("fake-key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    let doc = stdout_json(&sb.run(&["list", "--json"]));
    assert_eq!(state_of(&doc, "fake-key"), "ok");
    // `list` is cheap: no second request.
    assert_eq!(vendor.seen.lock().unwrap().len(), 1);

    // verify asks the vendor again.
    let v = stdout_json(&sb.run(&["verify", "fake-key", "--json"]));
    assert_eq!(v["state"], "ok");
    assert_eq!(vendor.seen.lock().unwrap().len(), 2);

    // logout removes the secret and reports missing.
    let out = sb.run(&["logout", "fake-key", "--json"]);
    assert!(out.status.success());
    assert_eq!(stdout_json(&out)["state"], "missing");
    assert!(!sb.secret_file("fake-key").exists());
    let doc = stdout_json(&sb.run(&["list", "--json"]));
    assert_eq!(state_of(&doc, "fake-key"), "missing");
}

#[test]
fn verify_maps_invalid_unknown_and_unreachable() {
    let sb = Sandbox::new();
    let rejecting = fake_vendor(401);
    sb.write_catalog(&key_catalog(&rejecting.url));
    let out = sb.run_with_stdin(
        &["login", "fake-key", "--secret-stdin", "--json"],
        "SENTINELbadkey123456\n",
    );
    assert!(
        out.status.success(),
        "stored even when rejected: {}",
        text(&out.stderr)
    );
    assert_eq!(stdout_json(&out)["state"], "invalid");
    assert_eq!(
        state_of(&stdout_json(&sb.run(&["list", "--json"])), "fake-key"),
        "invalid"
    );

    // A server error says nothing about the key: unknown.
    let flaky = fake_vendor(500);
    sb.write_catalog(&key_catalog(&flaky.url));
    let v = stdout_json(&sb.run(&["verify", "fake-key", "--json"]));
    assert_eq!(v["state"], "unknown");
    assert!(v["detail"].as_str().unwrap().contains("HTTP 500"), "{v}");

    // Nothing listening: unknown, with the knob to wait longer.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/v1/check", l.local_addr().unwrap())
    };
    sb.write_catalog(&key_catalog(&dead));
    let v = stdout_json(&sb.run(&["verify", "fake-key", "--json"]));
    assert_eq!(v["state"], "unknown");
    assert!(v["detail"].as_str().unwrap().contains("could not"), "{v}");

    // --no-verify never makes a request.
    let never = fake_vendor(200);
    sb.write_catalog(&key_catalog(&never.url));
    let out = sb.run_with_stdin(
        &[
            "login",
            "fake-key",
            "--secret-stdin",
            "--no-verify",
            "--json",
        ],
        "SENTINELkey2-0123456789\n",
    );
    assert_eq!(stdout_json(&out)["state"], "unknown");
    assert!(never.seen.lock().unwrap().is_empty());
}

#[test]
fn an_environment_variable_wins_in_list_and_is_what_verify_checks() {
    let sb = Sandbox::new();
    let vendor = fake_vendor(200);
    sb.write_catalog(&key_catalog(&vendor.url));
    sb.run_with_stdin(
        &[
            "login",
            "fake-key",
            "--secret-stdin",
            "--no-verify",
            "--json",
        ],
        "SENTINELstoredkey0123456\n",
    );
    let mut cmd = sb.command(&["verify", "fake-key", "--json"]);
    cmd.env("FAKE_VENDOR_API_KEY", "SENTINELenvkey0123456789");
    let out = cmd.stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(stdout_json(&out)["state"], "ok");
    let seen = vendor.seen.lock().unwrap();
    assert!(
        seen[0]
            .to_lowercase()
            .contains("x-api-key: sentinelenvkey0123456789"),
        "the environment's key is the one a launch would use: {}",
        seen[0]
    );
    drop(seen);

    let mut cmd = sb.command(&["list", "--json"]);
    cmd.env("FAKE_VENDOR_API_KEY", "SENTINELenvkey0123456789");
    let out = cmd.stdin(Stdio::null()).output().unwrap();
    let doc = stdout_json(&out);
    let f = doc["frameworks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == "fake-key")
        .unwrap();
    assert_eq!(f["source"], "env:FAKE_VENDOR_API_KEY");
}

#[test]
fn usage_errors_exit_2() {
    let sb = Sandbox::new();
    let out = sb.run(&["login", "not-a-framework", "--secret-stdin"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("Known frameworks"),
        "{}",
        text(&out.stderr)
    );
    // A key framework without --secret-stdin explains how to pipe the key.
    let out = sb.run(&["login", "openai"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("--secret-stdin"));
    // A broken override names the file.
    sb.write_catalog("[[framework]]\nid = \"x\"\nbogus = 1\n");
    let out = sb.run(&["list", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("agent-frameworks.toml"),
        "{}",
        text(&out.stderr)
    );
}

// ── The vendor login command in a pty (Unix: needs /bin/sh) ────────────────

#[cfg(unix)]
mod pty {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn vendor_catalog(command: &str, verify_url: &str, timeout_secs: u64) -> String {
        format!(
            r#"
[[framework]]
id = "claude-code"
label = "Claude"
kind = "token"
how = "Opens your browser."
agents = ["claude-code"]
env = ["CLAUDE_CODE_OAUTH_TOKEN"]

[framework.login]
method = "oauth_cli"
command = ["{command}", "setup-token"]
timeout_secs = {timeout_secs}

[framework.verify]
method = "http_get"
url = "{verify_url}"
auth = "bearer"
invalid_status = [401]
"#
        )
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// What `claude setup-token` paints: colors, words placed with cursor-column
    /// escapes, in-place repaints, a sign-in URL wrapped at 80 columns, and the
    /// token wrapped across two lines, with `\r\r\n` line ends.
    fn fake_setup_token(token: &str) -> String {
        let (first, second) = token.split_at(80);
        let (url_a, url_b) = (
            "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88",
            "ed5944d1962f5e&state=abc123",
        );
        format!(
            r#"#!/bin/sh
printf '\033[?25l\033[1m\033[38;5;174mWelcome\033[9Gto\033[12GClaude\033[18GCode\033[0m\r\r\n'
printf 'This will guide you through long-lived (1-year) auth token setup\r\r\n'
printf '\033[2K\rOpening browser to sign in\342\200\246\r\r\n'
sleep 1
printf 'Browser did not open? Use the url below to sign in (c to copy)\r\r\n\r\r\n'
printf '%s\r\r\n' '{url_a}'
printf '%s\r\r\n' '{url_b}'
sleep 1
printf '\033[2J\033[H'
printf '\033[32m\342\234\223\033[0m Long-lived authentication token created successfully!\r\r\n\r\r\n'
printf 'Your OAuth token (valid for 1 year):\r\r\n\r\r\n'
printf '\033[38;5;174m%s\033[0m\r\r\n' '{first}'
printf '\033[38;5;174m%s\033[0m\r\r\n' '{second}'
printf '\r\r\nStore this token securely. You won'"'"'t be able to see it again.\r\r\n'
printf 'Use this token by setting: export CLAUDE_CODE_OAUTH_TOKEN=<token>\r\r\n'
"#
        )
    }

    #[test]
    fn oauth_cli_login_captures_the_token_from_a_pty_without_ever_printing_it() {
        let sb = Sandbox::new();
        let vendor = fake_vendor(200);
        let bin = TempDir::new().unwrap();
        let script = write_script(bin.path(), "fake-vendor-login", &fake_setup_token(TOKEN));
        sb.write_catalog(&vendor_catalog(script.to_str().unwrap(), &vendor.url, 60));

        let out = sb.run(&["login", "claude-code", "--json"]);
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert!(
            out.status.success(),
            "exit {:?}\n{stderr}",
            out.status.code()
        );
        let v = stdout_json(&out);
        assert_eq!(v["id"], "claude-code");
        assert_eq!(v["state"], "ok");

        // The captured token is exactly the wrapped token, stored 0600 ...
        assert_eq!(
            std::fs::read_to_string(sb.secret_file("claude-code")).unwrap(),
            TOKEN
        );
        // ... verified with it as a bearer token ...
        let seen = vendor.seen.lock().unwrap();
        assert!(
            seen[0].contains(&format!("Bearer {TOKEN}"))
                || seen[0]
                    .to_lowercase()
                    .contains(&format!("bearer {}", TOKEN.to_lowercase())),
            "{}",
            seen[0]
        );
        drop(seen);
        // ... progress (including the reassembled sign-in address) went to stderr ...
        assert!(stderr.contains("Starting"), "{stderr}");
        assert!(
            stderr.contains("https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed5944d1962f5e&state=abc123"),
            "{stderr}"
        );
        // ... and the secret is in no output, record, or log line.
        assert!(!stdout.contains("SENTINEL"), "{stdout}");
        assert!(!stderr.contains("SENTINEL"), "{stderr}");
        assert!(!sb.text_outside_secret_files().contains("SENTINEL"));

        let doc = stdout_json(&sb.run(&["list", "--json"]));
        assert_eq!(state_of(&doc, "claude-code"), "ok");
    }

    #[test]
    fn a_vendor_that_exits_without_a_token_is_a_failed_login_with_exit_3() {
        let sb = Sandbox::new();
        let bin = TempDir::new().unwrap();
        let script = write_script(
            bin.path(),
            "fake-vendor-cancel",
            "#!/bin/sh\nprintf 'Opening browser to sign in\\r\\r\\n'\nprintf 'Login cancelled\\r\\r\\n'\nexit 1\n",
        );
        sb.write_catalog(&vendor_catalog(
            script.to_str().unwrap(),
            "http://127.0.0.1:9/",
            60,
        ));
        let out = sb.run(&["login", "claude-code", "--json"]);
        assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
        let stderr = text(&out.stderr);
        assert!(
            stderr.contains("without printing a login token"),
            "{stderr}"
        );
        assert!(stderr.contains("Login cancelled"), "{stderr}");
        assert!(!sb.secret_file("claude-code").exists());
        let doc = stdout_json(&sb.run(&["list", "--json"]));
        assert_eq!(state_of(&doc, "claude-code"), "missing");
    }

    #[test]
    fn a_vendor_that_never_finishes_times_out_naming_the_knob_with_exit_3() {
        let sb = Sandbox::new();
        let bin = TempDir::new().unwrap();
        let script = write_script(
            bin.path(),
            "fake-vendor-hang",
            "#!/bin/sh\nprintf 'Opening browser to sign in\\r\\r\\n'\nexec sleep 60\n",
        );
        sb.write_catalog(&vendor_catalog(
            script.to_str().unwrap(),
            "http://127.0.0.1:9/",
            300,
        ));
        let started = std::time::Instant::now();
        let mut cmd = sb.command(&["login", "claude-code", "--json"]);
        cmd.env("TA_AGENT_LOGIN_TIMEOUT_SECS", "2");
        let out = cmd.stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
        let stderr = text(&out.stderr);
        assert!(stderr.contains("TA_AGENT_LOGIN_TIMEOUT_SECS"), "{stderr}");
        assert!(stderr.contains("2 s"), "{stderr}");
        assert!(
            started.elapsed().as_secs() < 30,
            "the vendor must be stopped at the timeout"
        );
        assert!(!sb.secret_file("claude-code").exists());
    }

    #[test]
    fn a_missing_vendor_binary_exits_4_and_points_at_secret_stdin() {
        let sb = Sandbox::new();
        sb.write_catalog(&vendor_catalog(
            "ta-test-no-such-vendor-cli",
            "http://127.0.0.1:9/",
            60,
        ));
        let out = sb.run(&["login", "claude-code", "--json"]);
        assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
        let stderr = text(&out.stderr);
        assert!(stderr.contains("was not found"), "{stderr}");
        assert!(stderr.contains("--secret-stdin"), "{stderr}");
        // The caller's fallback works: the same login by stdin.
        let out = sb.run_with_stdin(
            &[
                "login",
                "claude-code",
                "--secret-stdin",
                "--no-verify",
                "--json",
            ],
            &format!("{TOKEN}\n"),
        );
        assert!(out.status.success(), "{}", text(&out.stderr));
        assert_eq!(
            std::fs::read_to_string(sb.secret_file("claude-code")).unwrap(),
            TOKEN
        );
    }
}
