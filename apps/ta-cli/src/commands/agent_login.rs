//! `ta agent-login`: give each agent framework (Claude, OpenAI, Gemini, ...) the
//! login it needs once, so sandboxed and daemon-launched agents just work.
//!
//! The frameworks are data (`agent-frameworks.default.toml`, overridable by
//! `~/.config/ta/agent-frameworks.toml` and `<project>/.ta/agent-frameworks.toml`).
//! Logins live in TA's per-user credential custody (the OS keychain or an
//! owner-only file, see `ta login`), never in a project tree. A secret is only
//! ever read from standard input or captured from the vendor's own login
//! terminal; it is never an argument and never printed.
//!
//! Exit codes: 0 done (the login is stored / the answer is on stdout), 2 usage
//! (unknown framework, no secret supplied), 3 the vendor login failed, was
//! cancelled or timed out, 4 the vendor command is not installed.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Subcommand;
use serde_json::json;
use ta_credentials::agent_login::{
    Catalog, Framework, FrameworkStatus, LoginMethod, LoginStore, Record, Secret, Source, State,
};

use super::agent_login_capture::{run_vendor_login, VendorError};
use super::agent_login_verify::{verify, VerifyResult};

#[derive(Subcommand, Debug, Clone)]
pub enum AgentLoginCommands {
    /// Show each agent framework and whether it has a login (no network).
    ///
    /// `--json` prints {"active", "frameworks": [{id, label, kind, method, how,
    /// state, checked_at, detail, source}]}. `--verify` checks every login that
    /// exists with a real request first.
    List {
        #[arg(long)]
        json: bool,
        /// Check each existing login with a real request to its vendor first.
        #[arg(long)]
        verify: bool,
    },
    /// Log an agent framework in and store the login in TA's custody.
    ///
    /// Frameworks whose login is a vendor command (Claude) are logged in by
    /// running that command here and capturing the token it prints. For the
    /// rest, or to hand over a token you already have, pipe it in:
    /// `printf %s "$KEY" | ta agent-login login openai --secret-stdin`.
    Login {
        /// Framework id (see `ta agent-login list`).
        id: String,
        /// Read the secret from standard input instead of running the vendor command.
        #[arg(long)]
        secret_stdin: bool,
        /// Store the login without checking it with the vendor.
        #[arg(long)]
        no_verify: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete the stored login of a framework.
    Logout {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Check a login with a real request to its vendor (all frameworks when no id).
    Verify {
        id: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// Everything the commands touch, so tests can substitute each piece.
#[allow(clippy::type_complexity)]
pub struct Ctx {
    pub store: LoginStore,
    pub catalog: Catalog,
    pub project_root: PathBuf,
    pub env: Box<dyn Fn(&str) -> Option<String>>,
    pub now: i64,
    pub vendor_login:
        Box<dyn Fn(&[String], Duration, &mut dyn FnMut(&str)) -> Result<Secret, VendorError>>,
    pub verifier: Box<dyn Fn(&Framework, &Secret) -> VerifyResult>,
}

impl Ctx {
    fn real(project_root: &Path, catalog: Catalog) -> Self {
        Ctx {
            store: LoginStore::open(),
            catalog,
            project_root: project_root.to_path_buf(),
            env: Box::new(|k| std::env::var(k).ok()),
            now: now_secs(),
            vendor_login: Box::new(run_vendor_login),
            verifier: Box::new(verify),
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// True when the command prints a JSON document on stdout.
pub fn prints_json(cmd: &AgentLoginCommands) -> bool {
    match cmd {
        AgentLoginCommands::List { json, .. }
        | AgentLoginCommands::Login { json, .. }
        | AgentLoginCommands::Logout { json, .. }
        | AgentLoginCommands::Verify { json, .. } => *json,
    }
}

/// Entry point from `main`: runs the command and exits with its code.
pub fn execute(cmd: &AgentLoginCommands, project_root: &Path) -> anyhow::Result<()> {
    let catalog = match Catalog::load(Some(project_root)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let ctx = Ctx::real(project_root, catalog);
    let stdin = std::io::stdin();
    let code = run(
        cmd,
        &ctx,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
        &mut stdin.lock(),
    );
    if code != 0 {
        let _ = std::io::stdout().flush();
        std::process::exit(code);
    }
    Ok(())
}

/// Run one command; returns the exit code.
pub fn run(
    cmd: &AgentLoginCommands,
    ctx: &Ctx,
    out: &mut dyn Write,
    err: &mut dyn Write,
    stdin: &mut dyn Read,
) -> i32 {
    match cmd {
        AgentLoginCommands::List { json, verify } => list(ctx, *json, *verify, out),
        AgentLoginCommands::Login {
            id,
            secret_stdin,
            no_verify,
            json,
        } => login(ctx, id, *secret_stdin, *no_verify, *json, out, err, stdin),
        AgentLoginCommands::Logout { id, json } => logout(ctx, id, *json, out, err),
        AgentLoginCommands::Verify { id, json } => verify_cmd(ctx, id.as_deref(), *json, out, err),
    }
}

// ── list ───────────────────────────────────────────────────────────────────

fn status_json(fw: &Framework, st: &FrameworkStatus) -> serde_json::Value {
    json!({
        "id": fw.id,
        "label": fw.label,
        "kind": fw.kind.as_str(),
        "method": fw.login.method.as_str(),
        "how": fw.how,
        "state": st.state.as_str(),
        "checked_at": st.checked_at,
        "detail": st.detail,
        "source": match &st.source {
            Source::Env(var) => json!(format!("env:{var}")),
            Source::Stored(_) => json!("stored"),
            Source::None => serde_json::Value::Null,
        },
    })
}

/// The default agent of this project (`.ta/workflow.toml` `[agent].default`),
/// `claude-code` when none is set, `None` when it is `auto`.
fn default_agent(project_root: &Path) -> Option<String> {
    let configured = std::fs::read_to_string(project_root.join(".ta").join("workflow.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("agent")
                .and_then(|a| a.get("default"))
                .and_then(|d| d.as_str())
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty());
    match configured {
        Some(a) if a.eq_ignore_ascii_case("auto") => None,
        Some(a) => Some(a),
        None => Some("claude-code".to_string()),
    }
}

/// The framework a launch of the project's default agent would use.
fn active_framework(ctx: &Ctx, statuses: &[(&Framework, FrameworkStatus)]) -> Option<String> {
    let agent = default_agent(&ctx.project_root)?;
    let serving: Vec<&(&Framework, FrameworkStatus)> = statuses
        .iter()
        .filter(|(fw, _)| fw.agents.contains(&agent))
        .collect();
    serving
        .iter()
        .find(|(_, st)| st.state != State::Missing)
        .or(serving.first())
        .map(|(fw, _)| fw.id.clone())
}

fn statuses(ctx: &Ctx) -> Vec<(&Framework, FrameworkStatus)> {
    ctx.catalog
        .frameworks
        .iter()
        .map(|fw| (fw, ctx.store.status(fw, &*ctx.env, ctx.now)))
        .collect()
}

fn list(ctx: &Ctx, json_out: bool, verify_first: bool, out: &mut dyn Write) -> i32 {
    if verify_first {
        for fw in &ctx.catalog.frameworks {
            let _ = verify_one(ctx, fw, true);
        }
    }
    let rows = statuses(ctx);
    let active = active_framework(ctx, &rows);
    if json_out {
        let doc = json!({
            "active": active,
            "frameworks": rows.iter().map(|(fw, st)| status_json(fw, st)).collect::<Vec<_>>(),
        });
        let _ = writeln!(out, "{doc}");
        return 0;
    }
    let _ = writeln!(
        out,
        "Agent logins (frameworks come from agent-frameworks.toml):\n"
    );
    for (fw, st) in &rows {
        let mark = if active.as_deref() == Some(fw.id.as_str()) {
            "*"
        } else {
            " "
        };
        let _ = writeln!(
            out,
            "{mark} {:<18} {:<8} {}",
            fw.id,
            st.state.as_str(),
            fw.label
        );
        let _ = writeln!(out, "    {}", st.detail);
        if st.state == State::Missing {
            let _ = writeln!(out, "    how: {}", fw.how);
        }
    }
    let _ = writeln!(
        out,
        "\n* = used by this project's default agent. Log in with `ta agent-login login <id>`."
    );
    0
}

// ── login ──────────────────────────────────────────────────────────────────

fn outcome_json(id: &str, state: State, checked_at: i64, detail: &str) -> serde_json::Value {
    json!({"id": id, "state": state.as_str(), "checked_at": checked_at, "detail": detail})
}

fn read_secret(stdin: &mut dyn Read) -> Result<Secret, String> {
    let mut raw = String::new();
    // Longer than any login; keeps a wrong pipe from filling memory.
    stdin
        .take(64 * 1024)
        .read_to_string(&mut raw)
        .map_err(|e| {
            format!(
                "Could not read the secret from standard input ({}). Pipe it in as UTF-8 text, \
                 e.g. `printf %s \"$KEY\" | ta agent-login login <id> --secret-stdin`.",
                e.kind()
            )
        })?;
    let value = raw.trim();
    if value.is_empty() {
        return Err(
            "The secret on standard input was empty, so nothing was stored. Pipe the key or \
             token in, e.g. `printf %s \"$KEY\" | ta agent-login login <id> --secret-stdin`."
                .to_string(),
        );
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(
            "The secret on standard input contains spaces or line breaks, which no key or token \
             does, so nothing was stored. Send exactly the key, e.g. \
             `printf %s \"$KEY\" | ta agent-login login <id> --secret-stdin`."
                .to_string(),
        );
    }
    Ok(Secret::new(value))
}

#[allow(clippy::too_many_arguments)]
fn login(
    ctx: &Ctx,
    id: &str,
    secret_stdin: bool,
    no_verify: bool,
    json_out: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
    stdin: &mut dyn Read,
) -> i32 {
    let fw = match ctx.catalog.get(id) {
        Ok(fw) => fw,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return 2;
        }
    };
    let prior = ctx.store.record(&fw.id);

    let secret = if secret_stdin {
        match read_secret(stdin) {
            Ok(s) => s,
            Err(message) => {
                let _ = writeln!(err, "{message}");
                return 2;
            }
        }
    } else if fw.login.method == LoginMethod::OauthCli {
        let _ = ctx.store.set_state(
            &fw.id,
            State::LoggingIn,
            "a login prompt is open",
            None,
            ctx.now,
        );
        let timeout = Duration::from_secs(fw.login_timeout_secs());
        let mut progress = |line: &str| {
            let _ = writeln!(err, "{line}");
        };
        match (ctx.vendor_login)(&fw.login.command, timeout, &mut progress) {
            Ok(secret) => secret,
            Err(e) => {
                restore_record(ctx, &fw.id, prior);
                let message = e.to_string();
                let _ = writeln!(err, "{message}");
                if json_out {
                    let st = ctx.store.status(fw, &*ctx.env, ctx.now);
                    let _ = writeln!(
                        out,
                        "{}",
                        outcome_json(&fw.id, st.state, st.checked_at, &message)
                    );
                }
                return e.exit_code();
            }
        }
    } else {
        let _ = writeln!(
            err,
            "{} needs a key that you copy from the vendor. Pipe it in so it never appears on a \
             command line: `printf %s \"$KEY\" | ta agent-login login {} --secret-stdin`. \
             How to get one: {}",
            fw.label, fw.id, fw.how
        );
        return 2;
    };

    let stored = match ctx.store.store(fw, &secret, ctx.now) {
        Ok(s) => s,
        Err(e) => {
            restore_record(ctx, &fw.id, prior);
            let _ = writeln!(err, "{e}");
            return 3;
        }
    };
    let held = stored.custody.describe();
    let _ = writeln!(err, "Stored the {} login in {held}.", fw.label);
    if let Some(reason) = &stored.keychain_fallback_reason {
        let _ = writeln!(
            err,
            "note: the OS keychain was not used ({reason}); the owner-only file holds the login instead."
        );
    }
    if let Some(var) = fw.env.iter().find(|v| (ctx.env)(v).is_some()) {
        let _ = writeln!(
            err,
            "note: {var} is set in this environment and wins over the stored login when an agent \
             starts; unset it to use the one just stored."
        );
    }

    let result = if no_verify {
        VerifyResult {
            state: State::Unknown,
            detail: "stored; not verified (--no-verify)".to_string(),
        }
    } else {
        let _ = writeln!(err, "Checking the login with {} ...", fw.label);
        (ctx.verifier)(fw, &secret)
    };
    let _ = ctx.store.set_state(
        &fw.id,
        result.state,
        &result.detail,
        Some("stored".to_string()),
        ctx.now,
    );
    if json_out {
        let _ = writeln!(
            out,
            "{}",
            outcome_json(&fw.id, result.state, ctx.now, &result.detail)
        );
    } else {
        let _ = writeln!(
            out,
            "{}: {} ({}). Stored in {held}.",
            fw.label,
            result.state.as_str(),
            result.detail
        );
    }
    0
}

fn restore_record(ctx: &Ctx, id: &str, prior: Option<Record>) {
    match prior {
        Some(r) => {
            let _ = ctx.store.write_record(id, &r);
        }
        None => {
            let _ = ctx.store.set_state(id, State::Missing, "", None, ctx.now);
        }
    }
}

// ── logout ─────────────────────────────────────────────────────────────────

fn logout(ctx: &Ctx, id: &str, json_out: bool, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let fw = match ctx.catalog.get(id) {
        Ok(fw) => fw,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return 2;
        }
    };
    let removed = match ctx.store.remove(fw, ctx.now) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return 3;
        }
    };
    let detail = if removed {
        format!("the stored {} login was deleted", fw.label)
    } else {
        format!("no stored {} login existed, nothing was deleted", fw.label)
    };
    let env_note = fw
        .env
        .iter()
        .find(|v| (ctx.env)(v).is_some_and(|x| !x.trim().is_empty()))
        .map(|v| format!(" ({v} is still set in this environment and still applies)"))
        .unwrap_or_default();
    if json_out {
        let _ = writeln!(
            out,
            "{}",
            outcome_json(
                &fw.id,
                State::Missing,
                ctx.now,
                &format!("{detail}{env_note}")
            )
        );
    } else {
        let _ = writeln!(out, "{detail}{env_note}.");
    }
    0
}

// ── verify ─────────────────────────────────────────────────────────────────

/// Check the login `ta run` would use for `fw` and record the answer.
/// `quiet_when_absent`: nothing to check is not worth a record.
fn verify_one(ctx: &Ctx, fw: &Framework, quiet_when_absent: bool) -> Option<VerifyResult> {
    let found = match ctx.store.effective(fw, &*ctx.env) {
        Ok(f) => f,
        Err(e) => {
            let result = VerifyResult {
                state: State::Unknown,
                detail: format!("{e}"),
            };
            let _ = ctx
                .store
                .set_state(&fw.id, result.state, &result.detail, None, ctx.now);
            return Some(result);
        }
    };
    let Some((source, secret)) = found else {
        if quiet_when_absent {
            return None;
        }
        let result = VerifyResult {
            state: State::Missing,
            detail: format!(
                "no login on this computer; run `ta agent-login login {}`",
                fw.id
            ),
        };
        return Some(result);
    };
    let result = (ctx.verifier)(fw, &secret);
    let source = match source {
        Source::Env(var) => format!("env:{var}"),
        _ => "stored".to_string(),
    };
    let _ = ctx
        .store
        .set_state(&fw.id, result.state, &result.detail, Some(source), ctx.now);
    Some(result)
}

fn verify_cmd(
    ctx: &Ctx,
    id: Option<&str>,
    json_out: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    if let Some(id) = id {
        let fw = match ctx.catalog.get(id) {
            Ok(fw) => fw,
            Err(e) => {
                let _ = writeln!(err, "{e}");
                return 2;
            }
        };
        let result = verify_one(ctx, fw, false).unwrap_or(VerifyResult {
            state: State::Missing,
            detail: String::new(),
        });
        if json_out {
            let _ = writeln!(
                out,
                "{}",
                outcome_json(&fw.id, result.state, ctx.now, &result.detail)
            );
        } else {
            let _ = writeln!(
                out,
                "{}: {} ({})",
                fw.label,
                result.state.as_str(),
                result.detail
            );
        }
        return 0;
    }
    // No id: check everything that has a login, then print the list.
    list(ctx, json_out, true, out)
}

// ── launch injection ───────────────────────────────────────────────────────

/// Put the stored login for `agent` into a sandboxed launch's environment.
///
/// Only the launching agent's framework variable is added (never another
/// framework's), and a login variable already present in `env` wins. Prints
/// variable NAMES and where the login is held; never a value.
pub fn inject_stored_login(
    agent: &str,
    project_root: Option<&Path>,
    env: &mut std::collections::HashMap<String, String>,
) {
    let catalog = match Catalog::load(project_root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[warn] agent-login: {e}");
            return;
        }
    };
    inject_with(&LoginStore::open(), &catalog, agent, env);
}

fn inject_with(
    store: &LoginStore,
    catalog: &Catalog,
    agent: &str,
    env: &mut std::collections::HashMap<String, String>,
) {
    use ta_credentials::agent_login::{launch_login, LaunchOutcome};
    let outcome = launch_login(store, catalog, agent, &|k| env.get(k).cloned());
    match outcome {
        LaunchOutcome::Inject(login) => {
            println!(
                "[agent-login] {agent}: {} set from the stored {} login ({})",
                login.var, login.framework_id, login.held_in
            );
            env.insert(login.var, login.secret.expose().to_string());
        }
        LaunchOutcome::EnvAlreadySet(var) => {
            println!(
                "[agent-login] {agent}: {var} is already set in the environment; the stored login is not used"
            );
        }
        LaunchOutcome::NothingStored => {}
        LaunchOutcome::Unreadable(why) => {
            eprintln!("[warn] agent-login: {agent}: {why}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const SENTINEL: &str = "sk-ant-SENTINELSECRETdonotprint0123456789";

    struct Harness {
        dir: tempfile::TempDir,
        env: std::rc::Rc<RefCell<HashMap<String, String>>>,
        verdict: std::rc::Rc<RefCell<VerifyResult>>,
        vendor: std::rc::Rc<RefCell<Result<String, i32>>>,
    }

    impl Harness {
        fn new() -> Self {
            Harness {
                dir: tempfile::tempdir().unwrap(),
                env: Default::default(),
                verdict: std::rc::Rc::new(RefCell::new(VerifyResult {
                    state: State::Ok,
                    detail: "api.example accepted the login (HTTP 200).".into(),
                })),
                vendor: std::rc::Rc::new(RefCell::new(Ok(SENTINEL.to_string()))),
            }
        }

        fn ctx(&self) -> Ctx {
            let env = self.env.clone();
            let verdict = self.verdict.clone();
            let vendor = self.vendor.clone();
            Ctx {
                store: LoginStore::with(self.dir.path(), false),
                catalog: Catalog::shipped(),
                project_root: self.dir.path().to_path_buf(),
                env: Box::new(move |k| env.borrow().get(k).cloned()),
                now: 1_700_000_000,
                vendor_login: Box::new(move |_cmd, _t, progress| {
                    progress("starting the fake vendor");
                    match &*vendor.borrow() {
                        Ok(token) => Ok(Secret::new(token.clone())),
                        Err(4) => Err(VendorError::NotFound {
                            command: "claude".into(),
                            searched: "/nowhere".into(),
                        }),
                        Err(_) => Err(VendorError::NoToken {
                            command: "claude setup-token".into(),
                            code: Some(1),
                            tail: "cancelled".into(),
                        }),
                    }
                }),
                verifier: Box::new(move |_fw, secret| {
                    // The verifier sees the real secret; it must still never print it.
                    assert!(!secret.expose().is_empty());
                    verdict.borrow().clone()
                }),
            }
        }

        /// Run a command; returns (exit code, stdout, stderr).
        fn run(&self, cmd: AgentLoginCommands, stdin: &str) -> (i32, String, String) {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let code = run(&cmd, &self.ctx(), &mut out, &mut err, &mut stdin.as_bytes());
            (
                code,
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        }

        fn list_json(&self) -> serde_json::Value {
            let (code, out, _) = self.run(
                AgentLoginCommands::List {
                    json: true,
                    verify: false,
                },
                "",
            );
            assert_eq!(code, 0);
            serde_json::from_str(&out).unwrap()
        }

        fn state_of(&self, id: &str) -> String {
            let doc = self.list_json();
            doc["frameworks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["id"] == id)
                .unwrap_or_else(|| panic!("no {id} in {doc}"))["state"]
                .as_str()
                .unwrap()
                .to_string()
        }

        /// Everything under the store directory except the secret files.
        fn non_secret_files_text(&self) -> String {
            let mut all = String::new();
            let root = self.dir.path().join("agent-logins");
            if let Ok(rd) = std::fs::read_dir(root) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) == Some("secret") {
                        continue;
                    }
                    all.push_str(&std::fs::read_to_string(p).unwrap_or_default());
                }
            }
            all
        }
    }

    fn login_cmd(id: &str, secret_stdin: bool, no_verify: bool) -> AgentLoginCommands {
        AgentLoginCommands::Login {
            id: id.to_string(),
            secret_stdin,
            no_verify,
            json: true,
        }
    }

    #[test]
    fn list_json_has_the_contract_shape_for_every_shipped_framework() {
        let h = Harness::new();
        let doc = h.list_json();
        assert_eq!(doc["active"], "claude-code");
        let frameworks = doc["frameworks"].as_array().unwrap();
        let ids: Vec<&str> = frameworks
            .iter()
            .map(|f| f["id"].as_str().unwrap())
            .collect();
        for want in [
            "claude-code",
            "anthropic-api-key",
            "openai",
            "gemini",
            "xai",
        ] {
            assert!(ids.contains(&want), "{ids:?}");
        }
        for f in frameworks {
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
                assert!(f.get(key).is_some(), "{key} missing in {f}");
            }
            assert!(f["checked_at"].is_i64());
            assert!(["token", "api_key"].contains(&f["kind"].as_str().unwrap()));
            assert!(["oauth_cli", "prompt"].contains(&f["method"].as_str().unwrap()));
            assert_eq!(f["state"], "missing");
        }
        let claude = frameworks
            .iter()
            .find(|f| f["id"] == "claude-code")
            .unwrap();
        assert_eq!(claude["kind"], "token");
        assert_eq!(claude["method"], "oauth_cli");
    }

    #[test]
    fn login_logout_list_state_transitions() {
        let h = Harness::new();
        assert_eq!(h.state_of("openai"), "missing");

        // Stored without a check: unknown.
        let (code, out, _) = h.run(login_cmd("openai", true, true), &format!("{SENTINEL}\n"));
        assert_eq!(code, 0);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], "openai");
        assert_eq!(v["state"], "unknown");
        assert!(v["checked_at"].is_i64());
        assert_eq!(h.state_of("openai"), "unknown");

        // Verified by the vendor: ok; then rejected: invalid.
        let (code, out, _) = h.run(
            AgentLoginCommands::Verify {
                id: Some("openai".into()),
                json: true,
            },
            "",
        );
        assert_eq!(code, 0);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["state"],
            "ok"
        );
        assert_eq!(h.state_of("openai"), "ok");
        *h.verdict.borrow_mut() = VerifyResult {
            state: State::Invalid,
            detail: "rejected (HTTP 401)".into(),
        };
        h.run(
            AgentLoginCommands::Verify {
                id: Some("openai".into()),
                json: true,
            },
            "",
        );
        assert_eq!(h.state_of("openai"), "invalid");

        // A new login replaces it and is verified by default.
        *h.verdict.borrow_mut() = VerifyResult {
            state: State::Ok,
            detail: "accepted".into(),
        };
        let (code, out, _) = h.run(login_cmd("openai", true, false), "sk-new-key-value\n");
        assert_eq!(code, 0);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["state"],
            "ok"
        );

        // Other frameworks are untouched.
        assert_eq!(h.state_of("xai"), "missing");

        let (code, out, _) = h.run(
            AgentLoginCommands::Logout {
                id: "openai".into(),
                json: true,
            },
            "",
        );
        assert_eq!(code, 0);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["state"],
            "missing"
        );
        assert_eq!(h.state_of("openai"), "missing");
    }

    #[test]
    fn an_oauth_cli_login_runs_the_vendor_and_a_failed_one_leaves_state_unchanged() {
        let h = Harness::new();
        let (code, out, err) = h.run(login_cmd("claude-code", false, true), "");
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["state"],
            "unknown"
        );
        assert!(err.contains("starting the fake vendor"), "{err}");
        assert!(err.contains("Stored the"), "{err}");
        assert_eq!(h.state_of("claude-code"), "unknown");

        // Cancelled sign-in: exit 3, the stored login and its state stay.
        *h.vendor.borrow_mut() = Err(1);
        let (code, out, err) = h.run(login_cmd("claude-code", false, true), "");
        assert_eq!(code, 3);
        assert!(err.contains("without printing a login token"), "{err}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["id"],
            "claude-code"
        );
        assert_eq!(h.state_of("claude-code"), "unknown");

        // Missing binary: exit 4 and the message says what to do.
        *h.vendor.borrow_mut() = Err(4);
        let (code, _, err) = h.run(login_cmd("claude-code", false, true), "");
        assert_eq!(code, 4);
        assert!(err.contains("--secret-stdin"), "{err}");
    }

    #[test]
    fn a_failed_first_login_leaves_the_framework_missing() {
        let h = Harness::new();
        *h.vendor.borrow_mut() = Err(1);
        let (code, _, _) = h.run(login_cmd("claude-code", false, false), "");
        assert_eq!(code, 3);
        assert_eq!(h.state_of("claude-code"), "missing");
    }

    #[test]
    fn usage_errors_exit_2_and_store_nothing() {
        let h = Harness::new();
        let (code, _, err) = h.run(login_cmd("no-such-framework", true, true), "x\n");
        assert_eq!(code, 2);
        assert!(err.contains("Known frameworks"), "{err}");

        // A key-prompt framework without --secret-stdin: explain how.
        let (code, _, err) = h.run(login_cmd("openai", false, true), "");
        assert_eq!(code, 2);
        assert!(err.contains("--secret-stdin"), "{err}");

        let (code, _, err) = h.run(login_cmd("openai", true, true), "\n");
        assert_eq!(code, 2);
        assert!(err.contains("empty"), "{err}");

        let (code, _, err) = h.run(login_cmd("openai", true, true), "two words\n");
        assert_eq!(code, 2);
        assert!(err.contains("spaces or line breaks"), "{err}");
        assert!(!err.contains("two words"), "{err}");

        assert_eq!(h.state_of("openai"), "missing");
    }

    #[test]
    fn an_environment_variable_wins_and_logout_says_it_still_applies() {
        let h = Harness::new();
        h.env
            .borrow_mut()
            .insert("OPENAI_API_KEY".into(), "sk-from-the-environment".into());
        let doc = h.list_json();
        let openai = doc["frameworks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == "openai")
            .unwrap();
        assert_eq!(openai["state"], "unknown");
        assert_eq!(openai["source"], "env:OPENAI_API_KEY");
        assert!(!openai.to_string().contains("sk-from-the-environment"));

        // Verifying checks the environment's value, as a launch would use it.
        h.run(
            AgentLoginCommands::Verify {
                id: Some("openai".into()),
                json: true,
            },
            "",
        );
        assert_eq!(h.state_of("openai"), "ok");

        let (code, out, _) = h.run(
            AgentLoginCommands::Logout {
                id: "openai".into(),
                json: true,
            },
            "",
        );
        assert_eq!(code, 0);
        assert!(out.contains("OPENAI_API_KEY is still set"), "{out}");
    }

    #[test]
    fn stored_login_is_stored_with_owner_only_permissions_under_agent_logins() {
        let h = Harness::new();
        h.run(login_cmd("xai", true, true), &format!("{SENTINEL}\n"));
        let file = h.dir.path().join("agent-logins").join("xai.secret");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), SENTINEL);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn the_secret_never_appears_in_any_output_or_record() {
        let h = Harness::new();
        let mut all = String::new();
        let mut collect = |(code, out, err): (i32, String, String)| {
            assert!(code == 0 || code == 2 || code == 3, "{code} {err}");
            all.push_str(&out);
            all.push_str(&err);
        };
        collect(h.run(login_cmd("claude-code", false, false), ""));
        collect(h.run(login_cmd("openai", true, false), &format!("{SENTINEL}\n")));
        collect(h.run(
            login_cmd("openai", true, false),
            &format!("{SENTINEL} extra\n"),
        ));
        collect(h.run(
            AgentLoginCommands::List {
                json: false,
                verify: true,
            },
            "",
        ));
        collect(h.run(
            AgentLoginCommands::Verify {
                id: None,
                json: true,
            },
            "",
        ));
        collect(h.run(
            AgentLoginCommands::Logout {
                id: "openai".into(),
                json: false,
            },
            "",
        ));
        assert!(!all.is_empty());
        assert!(
            !all.contains("SENTINEL"),
            "secret leaked into output:\n{all}"
        );
        assert!(
            !h.non_secret_files_text().contains("SENTINEL"),
            "secret leaked into a record"
        );
    }

    #[test]
    fn active_follows_the_projects_default_agent() {
        let h = Harness::new();
        std::fs::create_dir_all(h.dir.path().join(".ta")).unwrap();
        std::fs::write(
            h.dir.path().join(".ta").join("workflow.toml"),
            "[agent]\ndefault = \"codex\"\n",
        )
        .unwrap();
        assert_eq!(h.list_json()["active"], "openai");
        std::fs::write(
            h.dir.path().join(".ta").join("workflow.toml"),
            "[agent]\ndefault = \"auto\"\n",
        )
        .unwrap();
        assert!(h.list_json()["active"].is_null());
    }

    #[test]
    fn injection_adds_only_the_launching_agents_variable_and_env_wins() {
        let dir = tempfile::tempdir().unwrap();
        let store = LoginStore::with(dir.path(), false);
        let catalog = Catalog::shipped();
        let claude = catalog.get("claude-code").unwrap();
        let openai = catalog.get("openai").unwrap();
        store
            .store(claude, &Secret::new("claude-secret-value-1"), 1)
            .unwrap();
        store
            .store(openai, &Secret::new("openai-secret-value-1"), 1)
            .unwrap();

        let mut env: HashMap<String, String> = HashMap::new();
        inject_with(&store, &catalog, "claude-code", &mut env);
        assert_eq!(env.len(), 1);
        assert_eq!(env["CLAUDE_CODE_OAUTH_TOKEN"], "claude-secret-value-1");

        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("ANTHROPIC_API_KEY".into(), "from-env".into());
        inject_with(&store, &catalog, "claude-code", &mut env);
        assert_eq!(env.len(), 1, "{:?}", env.keys());
        assert_eq!(env["ANTHROPIC_API_KEY"], "from-env");

        let mut env: HashMap<String, String> = HashMap::new();
        inject_with(&store, &catalog, "codex", &mut env);
        assert_eq!(env.keys().collect::<Vec<_>>(), vec!["OPENAI_API_KEY"]);

        let mut env: HashMap<String, String> = HashMap::new();
        inject_with(&store, &catalog, "some-other-agent", &mut env);
        assert!(env.is_empty());
    }

    /// The sandbox's scrub, then the stored-login injection, then a real child
    /// started from the cleared environment: the agent sees the stored login of
    /// ITS framework only, and the daemon's own variables stay out.
    #[cfg(unix)]
    #[test]
    fn a_scrubbed_launch_receives_only_its_frameworks_stored_login() {
        use std::os::unix::fs::PermissionsExt;
        use ta_runtime::sandbox_launch::{scrub_env, EnvScrub};

        let dir = tempfile::tempdir().unwrap();
        let store = LoginStore::with(dir.path().join("state"), false);
        let catalog = Catalog::shipped();
        let secret = "sk-ant-oat01-SENTINELinjectedlogin0123456789";
        store
            .store(catalog.get("claude-code").unwrap(), &Secret::new(secret), 1)
            .unwrap();
        store
            .store(
                catalog.get("openai").unwrap(),
                &Secret::new("sk-openai-OTHER-0123456789"),
                1,
            )
            .unwrap();

        // What the daemon's own environment carries: a token the scrub must drop.
        let mut daemon_env: HashMap<String, String> = HashMap::new();
        daemon_env.insert("PATH".into(), "/usr/bin:/bin".into());
        daemon_env.insert("GITHUB_TOKEN".into(), "ghp_daemononly".into());
        let agent_home = dir.path().join("home");
        let scrub = EnvScrub {
            goal_id: "g",
            agent_home: &agent_home,
            protected: &[],
            claude_config_dir: None,
            mcp_endpoint: None,
            connection_token: None,
        };
        let mut env = scrub_env(&daemon_env, &HashMap::new(), &scrub).env;
        assert!(!env.contains_key("GITHUB_TOKEN"));
        assert!(!env.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
        inject_with(&store, &catalog, "claude-code", &mut env);

        // A fake agent that prints variable NAMES only.
        let script = dir.path().join("fake-agent");
        std::fs::write(&script, "#!/bin/sh\n/usr/bin/env | /usr/bin/cut -d= -f1\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = std::process::Command::new(&script)
            .env_clear()
            .envs(&env)
            .output()
            .unwrap();
        let names = String::from_utf8(out.stdout).unwrap();
        let names: Vec<&str> = names.lines().collect();
        assert!(names.contains(&"CLAUDE_CODE_OAUTH_TOKEN"), "{names:?}");
        assert!(!names.contains(&"OPENAI_API_KEY"), "{names:?}");
        assert!(!names.contains(&"GITHUB_TOKEN"), "{names:?}");
        // Nothing but the launching agent's framework variable was ADDED.
        assert!(!names.contains(&"XAI_API_KEY"), "{names:?}");
        assert_eq!(env["CLAUDE_CODE_OAUTH_TOKEN"], secret);
    }
}
