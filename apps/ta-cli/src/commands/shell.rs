// shell.rs -- Interactive TA Shell REPL (`ta shell`).
//
// A thin terminal client for the TA daemon. All business logic lives in the
// daemon (v0.9.7); this module is just REPL + rendering (~200 lines).
//
// Features:
//   - Line editing with history (rustyline)
//   - Input routing through POST /api/input
//   - Background SSE event listener (GET /api/events)
//   - Status header from GET /api/status
//   - Tab-completion from GET /api/routes
//   - `--init` generates default .ta/shell.toml
//   - `--attach <session_id>` attaches to an agent session

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config, Editor, Helper};

/// Run `ta shell --init` to generate a default `.ta/shell.toml`.
pub fn init_config(project_root: &Path) -> anyhow::Result<()> {
    let config_path = project_root.join(".ta").join("shell.toml");
    if config_path.exists() {
        println!("Config already exists: {}", config_path.display());
        return Ok(());
    }
    std::fs::create_dir_all(config_path.parent().unwrap())?;
    std::fs::write(&config_path, include_str!("../../templates/shell.toml"))?;
    println!("Created {}", config_path.display());
    Ok(())
}

/// Main shell entry point.
pub fn execute(
    project_root: &Path,
    attach: Option<&str>,
    daemon_url: Option<&str>,
    init: bool,
    classic: bool,
    no_version_check: bool,
) -> anyhow::Result<()> {
    if init {
        return init_config(project_root);
    }

    // Default to TUI mode; --classic falls back to rustyline.
    if !classic {
        return super::shell_tui::run(project_root, attach, daemon_url, false, no_version_check);
    }

    let base_url = daemon_url
        .map(|u| u.to_string())
        .unwrap_or_else(|| resolve_daemon_url(project_root));

    let rt = tokio::runtime::Runtime::new()?;

    // Version guard check (v0.10.10).
    if !no_version_check {
        let client = reqwest::Client::new();
        let _guard = super::version_guard::check_daemon_version(
            &client,
            &base_url,
            project_root,
            true, // interactive
            &rt,
        );
        // All results (Match, Stale, Restarted, Unreachable) proceed —
        // the function already printed warnings/prompts as needed.
    }

    rt.block_on(run_shell(base_url, attach.map(|s| s.to_string())))
}

/// Resolve the daemon URL from `.ta/daemon.toml` or default.
pub(crate) fn resolve_daemon_url(project_root: &Path) -> String {
    let config_path = project_root.join(".ta").join("daemon.toml");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(config) = content.parse::<toml::Table>() {
            let bind = config
                .get("server")
                .and_then(|s| s.get("bind"))
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1");
            let port = config
                .get("server")
                .and_then(|s| s.get("port"))
                .and_then(|v| v.as_integer())
                .unwrap_or(7700);
            return format!("http://{}:{}", bind, port);
        }
    }
    "http://127.0.0.1:7700".to_string()
}

async fn run_shell(base_url: String, attach_session: Option<String>) -> anyhow::Result<()> {
    let client = reqwest::Client::new();

    // Fetch initial status for the header.
    let status = fetch_status(&client, &base_url).await;

    if status.version.is_empty() || status.version == "?" {
        eprintln!("Error: Cannot reach daemon at {}", base_url);
        eprintln!();
        eprintln!("Start the daemon with:");
        eprintln!("  ./scripts/ta-shell.sh          # builds + starts daemon + opens shell");
        eprintln!("  ta-daemon --api --project-root .");
        return Err(anyhow::anyhow!("daemon not reachable at {}", base_url));
    }

    // Warn if the daemon version doesn't match the CLI version.
    let cli_version = env!("CARGO_PKG_VERSION");
    if status.version != cli_version {
        eprintln!(
            "Warning: daemon is v{} but this CLI is v{}",
            status.version, cli_version
        );
        eprintln!("  The daemon may behave differently than expected.");
        eprintln!("  To fix: kill the old daemon and restart with the matching version:");
        eprintln!("    pkill -f 'ta-daemon' && ta-daemon --api --project-root .");
        eprintln!("  Or use: ./scripts/ta-shell.sh (auto-restarts stale daemons)");
        eprintln!();
    }

    print_header(&status, &base_url);

    // Determine agent session ID.
    let session_id = match attach_session {
        Some(id) => {
            println!("Attached to agent session: {}", id);
            Some(id)
        }
        None => None,
    };

    // Fetch completion words from routes endpoint.
    let completions = fetch_completions(&client, &base_url).await;

    // Connection state: 0 = connected, 1 = disconnected.
    let running = Arc::new(AtomicBool::new(true));
    let daemon_down = Arc::new(AtomicBool::new(false));

    // Start background SSE listener. Use `since=now` to skip historical events
    // and only show events that occur after the shell connects.
    let sse_running = running.clone();
    // Use `to_rfc3339_opts` with `Z` suffix to avoid unescaped `+` in URL query.
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let sse_url = format!("{}/api/events?since={}", base_url, now);
    let sse_client = client.clone();
    let _sse_handle = tokio::spawn(async move {
        sse_listener(sse_client, &sse_url, sse_running).await;
    });

    // Start periodic health check (every 10s).
    let health_running = running.clone();
    let health_down = daemon_down.clone();
    let health_client = client.clone();
    let health_url = base_url.clone();
    let _health_handle = tokio::spawn(async move {
        health_monitor(health_client, &health_url, health_running, health_down).await;
    });

    // Set up rustyline.
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .build();
    let helper = ShellHelper { completions };
    let mut rl = Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    // Load history.
    let history_path = dirs_history_path();
    if let Some(ref p) = history_path {
        let _ = rl.load_history(p);
    }

    loop {
        let prompt = "ta> ";
        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                rl.add_history_entry(trimmed)?;

                // Built-in shell commands.
                match trimmed {
                    "exit" | "quit" | ":q" => break,
                    "help" | ":help" | "?" => {
                        print_help();
                        continue;
                    }
                    ":status" => {
                        let s = fetch_status(&client, &base_url).await;
                        print_header(&s, &base_url);
                        continue;
                    }
                    s if s.starts_with(":tail") => {
                        let arg = s.strip_prefix(":tail").unwrap().trim();
                        tail_goal_output(
                            &client,
                            &base_url,
                            if arg.is_empty() { None } else { Some(arg) },
                        )
                        .await;
                        continue;
                    }
                    _ => {}
                }

                // Send input to daemon.
                if daemon_down.load(Ordering::Relaxed) {
                    eprintln!("Warning: daemon is disconnected. Waiting for reconnect...");
                    eprintln!("  (Restart with: ./scripts/ta-shell.sh)");
                }
                let result = send_input(&client, &base_url, trimmed, session_id.as_deref()).await;
                match result {
                    Ok(output) => {
                        // If we thought the daemon was down, it's back.
                        if daemon_down.load(Ordering::Relaxed) {
                            daemon_down.store(false, Ordering::Relaxed);
                            eprintln!("[reconnected] Daemon is back.");
                        }
                        print!("{}", output);
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("Cannot reach daemon") {
                            daemon_down.store(true, Ordering::Relaxed);
                            eprintln!("Error: {}", e);
                            eprintln!(
                                "  The shell will auto-reconnect when the daemon comes back."
                            );
                        } else {
                            eprintln!("Error: {}", e);
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }

    running.store(false, Ordering::Relaxed);

    if let Some(ref p) = history_path {
        let _ = rl.save_history(p);
    }

    println!("Goodbye.");
    Ok(())
}

// -- Daemon API calls -------------------------------------------------------

#[derive(Default)]
pub(crate) struct StatusInfo {
    pub(crate) project: String,
    pub(crate) version: String,
    pub(crate) daemon_version: String,
    pub(crate) next_phase: Option<String>,
    pub(crate) pending_drafts: usize,
    pub(crate) active_agents: usize,
    /// The default agent binary name (e.g., "claude-code").
    pub(crate) default_agent: String,
}

pub(crate) async fn fetch_status(client: &reqwest::Client, base_url: &str) -> StatusInfo {
    let url = format!("{}/api/status", base_url);
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return StatusInfo::default(),
    };
    let json: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return StatusInfo::default(),
    };
    StatusInfo {
        project: json["project"].as_str().unwrap_or("unknown").to_string(),
        version: json["version"].as_str().unwrap_or("?").to_string(),
        daemon_version: json["daemon_version"].as_str().unwrap_or("?").to_string(),
        next_phase: json["current_phase"]["id"].as_str().map(|id| {
            let title = json["current_phase"]["title"].as_str().unwrap_or("");
            format!("{} -- {}", id, title)
        }),
        pending_drafts: json["pending_drafts"].as_u64().unwrap_or(0) as usize,
        active_agents: json["active_agents"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0),
        default_agent: json["default_agent"]
            .as_str()
            .unwrap_or("claude-code")
            .to_string(),
    }
}

fn print_header(status: &StatusInfo, base_url: &str) {
    let phase_str = status.next_phase.as_deref().unwrap_or("(none)");
    println!(
        "{} v{} | Next: {} | {} drafts | {} agents | {}",
        status.project,
        status.version,
        phase_str,
        status.pending_drafts,
        status.active_agents,
        base_url,
    );
    println!("Type 'help' for commands, 'exit' to quit.\n");
}

pub(crate) async fn send_input(
    client: &reqwest::Client,
    base_url: &str,
    text: &str,
    session_id: Option<&str>,
) -> anyhow::Result<String> {
    let url = format!("{}/api/input", base_url);
    let mut body = serde_json::json!({ "text": text });
    if let Some(sid) = session_id {
        body["session_id"] = serde_json::Value::String(sid.to_string());
    }

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Cannot reach daemon at {}: {}", base_url, e))?;

    let status_code = resp.status();
    let json: serde_json::Value = resp.json().await?;

    if !status_code.is_success() {
        if let Some(err) = json["error"].as_str() {
            return Err(anyhow::anyhow!("{}", err));
        }
        return Err(anyhow::anyhow!("HTTP {}", status_code));
    }

    // Check for disambiguation responses (ambiguous command parse).
    if json["ambiguous"].as_bool() == Some(true) {
        let mut output = String::new();
        if let Some(msg) = json["message"].as_str() {
            output.push_str(msg);
            output.push('\n');
        }
        if let Some(options) = json["options"].as_array() {
            for opt in options {
                let idx = opt["index"].as_u64().unwrap_or(0);
                let desc = opt["description"].as_str().unwrap_or("?");
                let cmd = opt["command"].as_str().unwrap_or("?");
                output.push_str(&format!("  {}> {}\n", idx, desc));
                output.push_str(&format!("     command: {}\n", cmd));
            }
        }
        output.push_str("\nRe-run with the exact command, or quote the title:\n");
        output.push_str("  run \"your multi-word title here\" --flag value\n");
        return Ok(output);
    }

    // Agent streaming response — return the request ID for the TUI to subscribe.
    if json["status"].as_str() == Some("processing") {
        if let Some(request_id) = json["request_id"].as_str() {
            return Ok(format!("__streaming__:{}\n", request_id));
        }
    }

    // For command results, prefer stdout; for agent results, prefer response.
    if let Some(stdout) = json["stdout"].as_str() {
        let mut output = stdout.to_string();
        if let Some(stderr) = json["stderr"].as_str() {
            if !stderr.is_empty() {
                output.push_str(stderr);
            }
        }
        if !output.ends_with('\n') {
            output.push('\n');
        }
        Ok(output)
    } else if let Some(response) = json["response"].as_str() {
        Ok(format!("{}\n", response))
    } else if let Some(result) = json.get("result") {
        Ok(format!("{}\n", serde_json::to_string_pretty(result)?))
    } else {
        Ok(format!("{}\n", serde_json::to_string_pretty(&json)?))
    }
}

pub(crate) async fn fetch_completions(client: &reqwest::Client, base_url: &str) -> Vec<String> {
    let url = format!("{}/api/routes", base_url);
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return default_completions(),
    };
    let json: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return default_completions(),
    };

    let mut words: Vec<String> = Vec::new();
    if let Some(shortcuts) = json["shortcuts"].as_array() {
        for s in shortcuts {
            if let Some(m) = s["match"].as_str() {
                words.push(m.to_string());
            }
        }
    }
    // Add built-in shell commands.
    words.extend(["exit", "quit", "help", ":status", ":help"].map(String::from));
    words
}

fn default_completions() -> Vec<String> {
    vec![
        // Shortcuts
        "approve",
        "deny",
        "view",
        "apply",
        "goals",
        "drafts",
        // ta subcommands (bare word → `ta <word>`)
        "goal",
        "draft",
        "audit",
        "run",
        "session",
        "plan",
        "context",
        "credentials",
        "events",
        "token",
        "dev",
        "setup",
        "init",
        "agent",
        "adapter",
        "release",
        "office",
        "plugin",
        "workflow",
        "policy",
        "config",
        "gc",
        "status",
        "conversation",
        // Shell built-ins
        "exit",
        "quit",
        "help",
        ":status",
        ":help",
        ":tail",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

// -- SSE background listener ------------------------------------------------

async fn sse_listener(client: reqwest::Client, url: &str, running: Arc<AtomicBool>) {
    // Simple line-based SSE reader. Reconnects on failure.
    while running.load(Ordering::Relaxed) {
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut stream = resp.bytes_stream();
        use tokio_stream::StreamExt;
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            if !running.load(Ordering::Relaxed) {
                return;
            }
            let bytes = match chunk {
                Ok(b) => b,
                Err(_) => break,
            };
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Parse SSE frames from the buffer.
            while let Some(pos) = buffer.find("\n\n") {
                let frame = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();
                if let Some(rendered) = render_sse_event(&frame) {
                    // Print event inline (interrupt prompt).
                    let _ = std::io::stdout().write_all(rendered.as_bytes());
                    let _ = std::io::stdout().flush();
                }
            }
        }

        if running.load(Ordering::Relaxed) {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
}

/// Tail live output from a running goal. Ctrl-C to detach.
async fn tail_goal_output(client: &reqwest::Client, base_url: &str, goal_id: Option<&str>) {
    // If no goal ID given, check active output streams.
    let target = match goal_id {
        Some(id) => id.to_string(),
        None => {
            // Fetch active output streams.
            let url = format!("{}/api/goals/active-output", base_url);
            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: Cannot reach daemon: {}", e);
                    return;
                }
            };
            let json: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("Error: Invalid response from daemon");
                    return;
                }
            };
            let goals: Vec<String> = json["goals"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            match goals.len() {
                0 => {
                    eprintln!("No goals with active output. Start one with: ta run <phase>");
                    return;
                }
                1 => {
                    let id = &goals[0];
                    eprintln!("Auto-attaching to: {}", id);
                    id.clone()
                }
                _ => {
                    eprintln!("Multiple goals running. Pick one:");
                    for (i, g) in goals.iter().enumerate() {
                        eprintln!("  [{}] {}", i + 1, g);
                    }
                    eprintln!("Usage: :tail <id>");
                    return;
                }
            }
        }
    };

    eprintln!("Tailing output for {} (Ctrl-C to detach)...", target);
    eprintln!("---");

    let url = format!("{}/api/goals/{}/output", base_url, target);
    let resp = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let json: serde_json::Value = r.json().await.unwrap_or_default();
            let err = json["error"].as_str().unwrap_or("unknown error");
            let hint = json["hint"].as_str().unwrap_or("");
            eprintln!("Error: {}", err);
            if !hint.is_empty() {
                eprintln!("  {}", hint);
            }
            return;
        }
        Err(e) => {
            eprintln!("Error: Cannot reach daemon: {}", e);
            return;
        }
    };

    let mut stream = resp.bytes_stream();
    use tokio_stream::StreamExt;
    let mut buffer = String::new();

    // Handle Ctrl-C to detach (not exit the shell).
    let detach = Arc::new(AtomicBool::new(false));
    let detach2 = detach.clone();
    let _guard = tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        detach2.store(true, Ordering::Relaxed);
    });

    while let Some(chunk) = stream.next().await {
        if detach.load(Ordering::Relaxed) {
            break;
        }
        let bytes = match chunk {
            Ok(b) => b,
            Err(_) => break,
        };
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(pos) = buffer.find("\n\n") {
            let frame = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            // Parse SSE frame.
            let mut event_type = None;
            let mut data = None;
            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("event: ") {
                    event_type = Some(rest.trim().to_string());
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    data = Some(rest.trim().to_string());
                }
            }

            match event_type.as_deref() {
                Some("output") => {
                    if let Some(d) = &data {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(d) {
                            let stream_name = json["stream"].as_str().unwrap_or("?");
                            let line = json["line"].as_str().unwrap_or("");
                            if stream_name == "stderr" {
                                eprintln!("{}", line);
                            } else {
                                println!("{}", line);
                            }
                        }
                    }
                }
                Some("done") => {
                    eprintln!("--- Goal process exited ---");
                    return;
                }
                Some("lagged") => {
                    eprintln!("[skipped some output lines]");
                }
                _ => {}
            }
        }
    }
    eprintln!("--- Detached ---");
}

/// Periodic health check — pings `/api/status` every 10s.
/// Prints a notice when the daemon goes down or comes back up.
async fn health_monitor(
    client: reqwest::Client,
    base_url: &str,
    running: Arc<AtomicBool>,
    daemon_down: Arc<AtomicBool>,
) {
    let url = format!("{}/api/status", base_url);
    let mut was_down = false;

    while running.load(Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        if !running.load(Ordering::Relaxed) {
            return;
        }

        let healthy = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .is_ok();

        if !healthy && !was_down {
            daemon_down.store(true, Ordering::Relaxed);
            was_down = true;
            let _ = write!(
                std::io::stderr(),
                "\n[disconnected] Daemon unreachable. Will auto-reconnect when it's back.\n"
            );
            let _ = std::io::stderr().flush();
        } else if healthy && was_down {
            daemon_down.store(false, Ordering::Relaxed);
            was_down = false;
            let _ = write!(
                std::io::stderr(),
                "\n[reconnected] Daemon is back online.\n"
            );
            let _ = std::io::stderr().flush();
        }
    }
}

pub(crate) fn render_sse_event(frame: &str) -> Option<String> {
    let mut event_type = None;
    let mut data = None;
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            event_type = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest.trim());
        }
    }
    let event_type = event_type?;
    let data = data?;

    // Parse JSON data for a human-readable one-liner.
    let json: serde_json::Value = serde_json::from_str(data).ok()?;
    let payload = &json["payload"];

    let detail = match event_type {
        "goal_started" => {
            let title = payload["title"].as_str().unwrap_or("untitled");
            let agent = payload["agent_id"].as_str().unwrap_or("?");
            format!("goal started: \"{}\" ({})", title, agent)
        }
        "goal_completed" => {
            let title = payload["title"].as_str().unwrap_or("untitled");
            let goal_id = payload["goal_id"]
                .as_str()
                .map(|s| &s[..8.min(s.len())])
                .unwrap_or("?");
            let secs = payload["duration_secs"].as_u64().unwrap_or(0);
            let mins = secs / 60;
            let duration = if mins > 0 {
                format!("{}m", mins)
            } else {
                format!("{}s", secs)
            };
            format!("goal completed: \"{}\" ({}) [{}]", title, duration, goal_id)
        }
        "draft_built" => {
            let count = payload["artifact_count"].as_u64().unwrap_or(0);
            let draft_id = payload["draft_id"]
                .as_str()
                .map(|s| &s[..8.min(s.len())])
                .unwrap_or("?");
            format!("draft ready: {} files ({})", count, draft_id)
        }
        "draft_approved" | "draft_denied" => {
            let decision = if event_type == "draft_approved" {
                "approved"
            } else {
                "denied"
            };
            let by = payload["approved_by"]
                .as_str()
                .or_else(|| payload["denied_by"].as_str())
                .unwrap_or("?");
            format!("draft {}: by {}", decision, by)
        }
        "workflow_started" => {
            let name = payload["name"].as_str().unwrap_or("unnamed");
            let stages = payload["stage_count"].as_u64().unwrap_or(0);
            format!("workflow started: \"{}\" ({} stages)", name, stages)
        }
        "stage_started" => {
            let stage = payload["stage"].as_str().unwrap_or("?");
            let roles: Vec<&str> = payload["roles"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            format!("stage started: {} [{}]", stage, roles.join(", "))
        }
        "stage_completed" => {
            let stage = payload["stage"].as_str().unwrap_or("?");
            let score = payload["aggregate_score"].as_f64().unwrap_or(0.0);
            format!("stage completed: {} (score: {:.0}%)", stage, score * 100.0)
        }
        "workflow_routed" => {
            let from = payload["from_stage"].as_str().unwrap_or("?");
            let to = payload["to_stage"].as_str().unwrap_or("?");
            let severity = payload["severity"].as_str().unwrap_or("?");
            format!("workflow routed: {} -> {} ({})", from, to, severity)
        }
        "workflow_completed" => {
            let name = payload["name"].as_str().unwrap_or("unnamed");
            let stages = payload["stages_executed"].as_u64().unwrap_or(0);
            format!("workflow completed: \"{}\" ({} stages)", name, stages)
        }
        "workflow_failed" => {
            let name = payload["name"].as_str().unwrap_or("unnamed");
            let reason = payload["reason"].as_str().unwrap_or("unknown");
            format!("workflow failed: \"{}\" - {}", name, reason)
        }
        "workflow_awaiting_human" => {
            let stage = payload["stage"].as_str().unwrap_or("?");
            let prompt = payload["prompt"].as_str().unwrap_or("Input needed");
            let options: Vec<&str> = payload["options"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let options_str: Vec<String> = options
                .iter()
                .enumerate()
                .map(|(i, o)| format!("[{}] {}", i + 1, o))
                .collect();
            format!(
                "workflow paused at '{}': {}\n  Options: {}",
                stage,
                prompt,
                options_str.join("  ")
            )
        }
        "agent_needs_input" => {
            let question = payload["question"].as_str().unwrap_or("?");
            let turn = payload["turn"].as_u64().unwrap_or(1);
            let iid = payload["interaction_id"]
                .as_str()
                .map(|s| &s[..8.min(s.len())])
                .unwrap_or("?");
            let choices: Vec<&str> = payload["choices"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let mut msg = format!("agent needs input (turn {}, {}): {}", turn, iid, question);
            if !choices.is_empty() {
                let opts: Vec<String> = choices
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("[{}] {}", i + 1, c))
                    .collect();
                msg.push_str(&format!("\n  Options: {}", opts.join("  ")));
            }
            msg
        }
        "agent_question_answered" => {
            let turn = payload["turn"].as_u64().unwrap_or(1);
            let responder = payload["responder_id"].as_str().unwrap_or("?");
            format!("agent question answered (turn {}, by {})", turn, responder)
        }
        "command_failed" => {
            let cmd = payload["command"].as_str().unwrap_or("?");
            let code = payload["exit_code"].as_i64().unwrap_or(-1);
            let stderr = payload["stderr"].as_str().unwrap_or("");
            let mut msg = format!("command failed (exit {}): {}", code, cmd);
            if !stderr.is_empty() {
                // Show last 3 lines of stderr inline.
                let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
                for line in tail.iter().rev() {
                    msg.push_str(&format!("\n  {}", line));
                }
            }
            msg
        }
        _ => {
            // Fallback: use summary field or event type.
            json["event"]["summary"]
                .as_str()
                .or_else(|| json["event_type"].as_str())
                .unwrap_or(event_type)
                .to_string()
        }
    };

    // Render structured actions from the event envelope (if present).
    // Falls back to hardcoded suggestions for backwards compatibility with
    // events written before the actions field was added.
    let actions_suffix = render_actions_suffix(event_type, &json);

    Some(format!("\n[event] {}{}\n", detail, actions_suffix))
}

/// Render the actions section of an SSE event for terminal display.
///
/// Reads the `actions` array from the event envelope. If no actions are
/// present (e.g., older events), falls back to hardcoded suggestions for
/// the known lifecycle events that warrant them.
fn render_actions_suffix(event_type: &str, json: &serde_json::Value) -> String {
    // Prefer structured actions from the envelope.
    if let Some(actions) = json["actions"].as_array() {
        if !actions.is_empty() {
            let mut lines = String::new();
            for action in actions {
                if let (Some(label), Some(command)) =
                    (action["label"].as_str(), action["command"].as_str())
                {
                    lines.push_str(&format!("\n  {}: {}", label, command));
                }
            }
            return lines;
        }
    }

    // Backwards-compat fallback for events without the actions field.
    match event_type {
        "goal_completed" => "\n  Next: ta draft list | ta draft view <id>".to_string(),
        "draft_built" => {
            let full_id = json["payload"]["draft_id"].as_str().unwrap_or("?");
            format!(
                "\n  View:    ta draft view {}\n  Approve: ta draft approve {}\n  Deny:    ta draft deny {}",
                full_id, full_id, full_id
            )
        }
        "goal_started" => {
            let goal_id = json["payload"]["goal_id"].as_str().unwrap_or("");
            if goal_id.len() >= 8 {
                format!("\n  Tail: ta shell :tail {}", &goal_id[..8])
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

// -- Rustyline helper --------------------------------------------------------

struct ShellHelper {
    completions: Vec<String>,
}

impl Completer for ShellHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];
        let word_start = prefix.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let word = &prefix[word_start..];

        let matches: Vec<Pair> = self
            .completions
            .iter()
            .filter(|c| c.starts_with(word))
            .map(|c| Pair {
                display: c.clone(),
                replacement: c.clone(),
            })
            .collect();

        Ok((word_start, matches))
    }
}

impl Hinter for ShellHelper {
    type Hint = String;
}
impl Highlighter for ShellHelper {}
impl Validator for ShellHelper {}
impl Helper for ShellHelper {}

// -- Helpers -----------------------------------------------------------------

pub(crate) fn dirs_history_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::Path::new(&home).join(".ta");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("shell_history"))
}

fn print_help() {
    println!(
        "\
TA Shell -- Interactive terminal for Trusted Autonomy

Commands:
  ta <cmd>           Run any ta CLI command (e.g., ta draft list)
  git <cmd>          Run git commands
  !<cmd>             Shell escape (e.g., !ls -la)
  approve <id>       Shortcut for: ta draft approve <id>
  deny <id>          Shortcut for: ta draft deny <id>
  view <id>          Shortcut for: ta draft view <id>
  apply <id>         Shortcut for: ta draft apply <id>
  status             Shortcut for: ta status
  plan               Shortcut for: ta plan list
  goals              Shortcut for: ta goal list
  drafts             Shortcut for: ta draft list
  <anything else>    Sent to agent session (if attached)

Shell commands:
  :tail [id]         Tail live agent output (auto-picks if one goal running)
  :status            Refresh the status header
  help / ?           Show this help
  exit / quit / :q   Exit the shell
"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_default_daemon_url() {
        // Nonexistent project root should give default URL.
        let url = resolve_daemon_url(Path::new("/nonexistent"));
        assert_eq!(url, "http://127.0.0.1:7700");
    }

    #[test]
    fn render_sse_event_basic() {
        let frame = "event: draft_ready\ndata: {\"event_type\":\"draft_ready\",\"event\":{\"summary\":\"Draft abc123 is ready\"}}";
        let rendered = render_sse_event(frame).unwrap();
        assert!(rendered.contains("Draft abc123 is ready"));
    }

    #[test]
    fn render_sse_event_goal_started_with_payload() {
        let frame = "event: goal_started\ndata: {\"event_type\":\"goal_started\",\"payload\":{\"title\":\"Fix auth\",\"agent_id\":\"claude-code\"}}";
        let rendered = render_sse_event(frame).unwrap();
        assert!(rendered.contains("goal started"));
        assert!(rendered.contains("Fix auth"));
        assert!(rendered.contains("claude-code"));
    }

    #[test]
    fn render_sse_event_uses_structured_actions_when_present() {
        let frame = concat!(
            "event: draft_built\n",
            "data: {",
            "\"event_type\":\"draft_built\",",
            "\"payload\":{\"artifact_count\":3,\"draft_id\":\"abc12345-0000-0000-0000-000000000000\"},",
            "\"actions\":[",
            "{\"verb\":\"view\",\"command\":\"ta draft view abc12345\",\"label\":\"View draft abc12345\"},",
            "{\"verb\":\"approve\",\"command\":\"ta draft approve abc12345\",\"label\":\"Approve draft abc12345\"}",
            "]}"
        );
        let rendered = render_sse_event(frame).unwrap();
        // Should use structured actions, not hardcoded fallback.
        assert!(rendered.contains("View draft abc12345"));
        assert!(rendered.contains("ta draft view abc12345"));
        assert!(rendered.contains("Approve draft abc12345"));
        assert!(rendered.contains("ta draft approve abc12345"));
    }

    #[test]
    fn render_sse_event_falls_back_when_no_actions_field() {
        // Old-format event without an actions field: should use hardcoded fallback.
        let frame = concat!(
            "event: goal_completed\n",
            "data: {",
            "\"event_type\":\"goal_completed\",",
            "\"payload\":{\"title\":\"Fix auth\",\"goal_id\":\"abc12345-dead-beef-cafe-000000000000\",\"duration_secs\":120}",
            "}"
        );
        let rendered = render_sse_event(frame).unwrap();
        assert!(rendered.contains("goal completed"));
        assert!(rendered.contains("Fix auth"));
        // Backwards-compat fallback action should appear.
        assert!(rendered.contains("ta draft list"));
    }

    #[test]
    fn render_sse_event_empty_actions_array_uses_fallback() {
        // Event with an empty actions array: should fall back to hardcoded suggestions.
        let frame = concat!(
            "event: draft_built\n",
            "data: {",
            "\"event_type\":\"draft_built\",",
            "\"payload\":{\"artifact_count\":2,\"draft_id\":\"deadbeef-0000-0000-0000-000000000000\"},",
            "\"actions\":[]",
            "}"
        );
        let rendered = render_sse_event(frame).unwrap();
        assert!(rendered.contains("draft ready"));
        // Backwards-compat fallback should appear since actions list is empty.
        assert!(rendered.contains("ta draft view"));
    }

    #[test]
    fn render_sse_event_no_payload_uses_fallback() {
        let frame = "event: custom_event\ndata: {\"event_type\":\"custom_event\"}";
        let rendered = render_sse_event(frame).unwrap();
        assert!(rendered.contains("custom_event"));
    }

    #[test]
    fn render_sse_event_malformed_returns_none() {
        assert!(render_sse_event("just text").is_none());
        assert!(render_sse_event("event: x").is_none());
    }

    #[test]
    fn default_completions_includes_shortcuts() {
        let comps = default_completions();
        assert!(comps.contains(&"approve".to_string()));
        assert!(comps.contains(&"exit".to_string()));
    }

    #[test]
    fn shell_helper_completes_prefix() {
        let helper = ShellHelper {
            completions: vec![
                "approve".to_string(),
                "apply".to_string(),
                "deny".to_string(),
            ],
        };
        let history = rustyline::history::DefaultHistory::new();
        let ctx = rustyline::Context::new(&history);
        let (start, matches) = helper.complete("app", 3, &ctx).unwrap();
        assert_eq!(start, 0);
        assert_eq!(matches.len(), 2); // approve, apply
    }

    #[test]
    fn init_config_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        init_config(dir.path()).unwrap();
        assert!(dir.path().join(".ta").join("shell.toml").exists());
    }

    #[test]
    fn init_config_no_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("shell.toml"), "existing").unwrap();
        init_config(dir.path()).unwrap();
        let content = std::fs::read_to_string(ta_dir.join("shell.toml")).unwrap();
        assert_eq!(content, "existing");
    }
}
