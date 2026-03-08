// office.rs — `ta office` subcommands for multi-project daemon management.
//
// Commands:
//   ta office start --config office.yaml  — start multi-project daemon
//   ta office stop                        — graceful shutdown
//   ta office status [project]            — overview or per-project detail
//   ta office project add <name> <path>   — add project at runtime
//   ta office project remove <name>       — remove project
//   ta office reload                      — reload config without restart

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum OfficeCommands {
    /// Start the multi-project daemon with an office configuration.
    Start {
        /// Path to office.yaml configuration file.
        #[arg(long, default_value = "office.yaml")]
        config: String,
        /// Run in foreground (don't daemonize).
        #[arg(long)]
        foreground: bool,
    },
    /// Stop a running office daemon.
    Stop,
    /// Show office status — projects, active goals, channel connections.
    Status {
        /// Show detail for a specific project.
        project: Option<String>,
    },
    /// Manage projects within the office.
    Project {
        #[command(subcommand)]
        command: ProjectCommands,
    },
    /// Reload office configuration without restarting the daemon.
    Reload {
        /// Path to office.yaml (defaults to the config used at startup).
        #[arg(long)]
        config: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ProjectCommands {
    /// Add a project to the running office.
    Add {
        /// Project name (used as routing key).
        name: String,
        /// Path to the project root.
        path: String,
        /// Plan file name.
        #[arg(long, default_value = "PLAN.md")]
        plan: String,
        /// Default git branch.
        #[arg(long, default_value = "main")]
        branch: String,
    },
    /// Remove a project from the running office.
    Remove {
        /// Project name to remove.
        name: String,
    },
    /// List all managed projects.
    List,
}

pub fn execute(command: &OfficeCommands, project_root: &std::path::Path) -> Result<()> {
    match command {
        OfficeCommands::Start { config, foreground } => {
            start_office(config, project_root, *foreground)
        }
        OfficeCommands::Stop => stop_office(project_root),
        OfficeCommands::Status { project } => show_status(project.as_deref(), project_root),
        OfficeCommands::Project { command } => execute_project_command(command, project_root),
        OfficeCommands::Reload { config } => reload_office(config.as_deref(), project_root),
    }
}

fn start_office(config_path: &str, project_root: &std::path::Path, foreground: bool) -> Result<()> {
    let config_file = if std::path::Path::new(config_path).is_absolute() {
        std::path::PathBuf::from(config_path)
    } else {
        project_root.join(config_path)
    };

    if !config_file.exists() {
        anyhow::bail!(
            "Office config not found at {}. Create an office.yaml or use `ta daemon` for single-project mode.\n\
             Example office.yaml:\n\n\
             office:\n\
             \x20 name: \"My Dev Office\"\n\
             \x20 daemon:\n\
             \x20   http_port: 3140\n\
             projects:\n\
             \x20 my-project:\n\
             \x20   path: ~/dev/my-project",
            config_file.display()
        );
    }

    // Validate the config before starting.
    let config_content = std::fs::read_to_string(&config_file)?;
    let office_config: serde_yaml::Value = serde_yaml::from_str(&config_content)
        .map_err(|e| anyhow::anyhow!("Invalid office.yaml: {}. Check YAML syntax.", e))?;

    let project_count = office_config
        .get("projects")
        .and_then(|p| p.as_mapping())
        .map(|m| m.len())
        .unwrap_or(0);

    let office_name = office_config
        .get("office")
        .and_then(|o| o.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("Unnamed Office");

    println!("Starting office: {}", office_name);
    println!("Config: {}", config_file.display());
    println!("Projects: {}", project_count);

    if foreground {
        // Start daemon in foreground — used for development.
        // In a real implementation, this would call the daemon binary with --office-config.
        println!();
        println!("Starting daemon in foreground...");
        let status = std::process::Command::new("ta-daemon")
            .arg("--api")
            .arg("--project-root")
            .arg(project_root)
            .env("TA_OFFICE_CONFIG", &config_file)
            .status();

        match status {
            Ok(s) if s.success() => println!("Office daemon exited."),
            Ok(s) => anyhow::bail!("Office daemon exited with status: {}", s),
            Err(e) => {
                anyhow::bail!(
                    "Cannot start ta-daemon: {}. Ensure ta-daemon is in your PATH.\n\
                     You can also run: ta-daemon --api --project-root {} (with TA_OFFICE_CONFIG={})",
                    e,
                    project_root.display(),
                    config_file.display()
                );
            }
        }
    } else {
        // Background daemon start.
        let child = std::process::Command::new("ta-daemon")
            .arg("--api")
            .arg("--project-root")
            .arg(project_root)
            .env("TA_OFFICE_CONFIG", &config_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        match child {
            Ok(child) => {
                // Write PID file for later stop.
                let pid_file = project_root.join(".ta").join("office.pid");
                if let Some(parent) = pid_file.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                std::fs::write(&pid_file, child.id().to_string()).ok();

                println!("Office daemon started (PID: {})", child.id());
                println!("PID file: {}", pid_file.display());
                println!();
                println!("Use `ta office status` to check the office.");
                println!("Use `ta office stop` to shut down.");
            }
            Err(e) => {
                anyhow::bail!(
                    "Cannot start ta-daemon: {}. Ensure ta-daemon is in your PATH.\n\
                     Try `ta office start --foreground` to run in the foreground.",
                    e
                );
            }
        }
    }

    Ok(())
}

fn stop_office(project_root: &std::path::Path) -> Result<()> {
    let pid_file = project_root.join(".ta").join("office.pid");
    if !pid_file.exists() {
        anyhow::bail!(
            "No office PID file found at {}. Is an office daemon running?\n\
             Check with `ta office status` or start one with `ta office start --config office.yaml`.",
            pid_file.display()
        );
    }

    let pid_str = std::fs::read_to_string(&pid_file)?;
    let pid: u32 = pid_str.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "Invalid PID in {}: '{}'. Remove the file and check for running ta-daemon processes manually.",
            pid_file.display(),
            pid_str.trim()
        )
    })?;

    // Send SIGTERM on Unix.
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if result == 0 {
            println!("Sent SIGTERM to office daemon (PID: {})", pid);
            std::fs::remove_file(&pid_file).ok();
            println!("Office daemon stopped.");
        } else {
            std::fs::remove_file(&pid_file).ok();
            println!("Process {} not running (stale PID file removed).", pid);
        }
    }
    #[cfg(not(unix))]
    {
        println!(
            "Stopping PID {} (manual kill may be required on this platform)",
            pid
        );
        std::fs::remove_file(&pid_file).ok();
    }

    Ok(())
}

fn show_status(project: Option<&str>, project_root: &std::path::Path) -> Result<()> {
    // Try to load office.yaml for multi-project status.
    let config_path = std::env::var("TA_OFFICE_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| project_root.join("office.yaml"));

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        let config: serde_yaml::Value = serde_yaml::from_str(&content)?;

        let office_name = config
            .get("office")
            .and_then(|o| o.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("Unnamed Office");

        // Check if daemon is running.
        let pid_file = project_root.join(".ta").join("office.pid");
        let daemon_status = if pid_file.exists() {
            "running"
        } else {
            "stopped"
        };

        if let Some(project_name) = project {
            // Per-project detail.
            let projects = config.get("projects").and_then(|p| p.as_mapping());
            match projects.and_then(|m| m.get(serde_yaml::Value::String(project_name.into()))) {
                Some(proj) => {
                    let path = proj.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                    let branch = proj
                        .get("default_branch")
                        .and_then(|b| b.as_str())
                        .unwrap_or("main");
                    let plan = proj
                        .get("plan")
                        .and_then(|p| p.as_str())
                        .unwrap_or("PLAN.md");

                    println!("Project: {}", project_name);
                    println!("  Path: {}", path);
                    println!("  Branch: {}", branch);
                    println!("  Plan: {}", plan);

                    // Count goals if .ta/goals exists.
                    let expanded = expand_home(path);
                    let goals_dir = std::path::Path::new(&expanded).join(".ta").join("goals");
                    if goals_dir.exists() {
                        let goal_count = std::fs::read_dir(&goals_dir)
                            .map(|entries| entries.filter_map(|e| e.ok()).count())
                            .unwrap_or(0);
                        println!("  Goals: {}", goal_count);
                    }
                }
                None => {
                    eprintln!(
                        "Project '{}' not found in office config. Available projects:",
                        project_name
                    );
                    if let Some(projects) = config.get("projects").and_then(|p| p.as_mapping()) {
                        for key in projects.keys() {
                            if let Some(name) = key.as_str() {
                                eprintln!("  - {}", name);
                            }
                        }
                    }
                }
            }
        } else {
            // Overview.
            println!("Office: {}", office_name);
            println!("Daemon: {}", daemon_status);
            println!("Config: {}", config_path.display());
            println!();

            if let Some(projects) = config.get("projects").and_then(|p| p.as_mapping()) {
                println!("Projects ({}):", projects.len());
                for (key, val) in projects {
                    if let Some(name) = key.as_str() {
                        let path = val.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                        println!("  {} → {}", name, path);
                    }
                }
            }

            println!();

            if let Some(channels) = config.get("channels").and_then(|c| c.as_mapping()) {
                println!("Channels ({}):", channels.len());
                for (key, val) in channels {
                    if let Some(name) = key.as_str() {
                        let route_count = val
                            .get("routes")
                            .and_then(|r| r.as_mapping())
                            .map(|m| m.len())
                            .unwrap_or(0);
                        println!("  {} ({} routes)", name, route_count);
                    }
                }
            }
        }
    } else {
        // Single-project mode.
        println!("Mode: single-project");
        println!("Project root: {}", project_root.display());

        let pid_file = project_root.join(".ta").join("office.pid");
        if pid_file.exists() {
            println!("Daemon: running");
        } else {
            println!("Daemon: not running");
        }

        println!();
        println!("No office.yaml found. Running in single-project mode.");
        println!("Create office.yaml to manage multiple projects.");
    }

    Ok(())
}

fn execute_project_command(
    command: &ProjectCommands,
    project_root: &std::path::Path,
) -> Result<()> {
    match command {
        ProjectCommands::Add {
            name,
            path,
            plan,
            branch,
        } => {
            let daemon_url = get_daemon_url(project_root);
            let body = serde_json::json!({
                "name": name,
                "path": path,
                "plan": plan,
                "default_branch": branch,
            });

            let output =
                daemon_api_request("POST", &format!("{}/api/projects", daemon_url), Some(&body))?;
            println!("Project '{}' added to running office.", name);
            if !output.is_empty() {
                println!("{}", output);
            }
            Ok(())
        }
        ProjectCommands::Remove { name } => {
            let daemon_url = get_daemon_url(project_root);
            daemon_api_request(
                "DELETE",
                &format!("{}/api/projects/{}", daemon_url, name),
                None,
            )?;
            println!("Project '{}' removed from office.", name);
            Ok(())
        }
        ProjectCommands::List => {
            let daemon_url = get_daemon_url(project_root);
            let output = daemon_api_request("GET", &format!("{}/api/projects", daemon_url), None)?;
            let projects: Vec<serde_json::Value> =
                serde_json::from_str(&output).unwrap_or_default();
            if projects.is_empty() {
                println!("No projects registered.");
            } else {
                println!("Projects ({}):", projects.len());
                for proj in &projects {
                    let name = proj.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                    let path = proj.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                    let active = proj
                        .get("active")
                        .and_then(|a| a.as_bool())
                        .unwrap_or(false);
                    let status = if active { "active" } else { "inactive" };
                    println!("  {} \u{2192} {} [{}]", name, path, status);
                }
            }
            Ok(())
        }
    }
}

fn reload_office(config: Option<&str>, project_root: &std::path::Path) -> Result<()> {
    let daemon_url = get_daemon_url(project_root);
    let body = match config {
        Some(path) => serde_json::json!({"config": path}),
        None => serde_json::json!({}),
    };
    daemon_api_request(
        "POST",
        &format!("{}/api/office/reload", daemon_url),
        Some(&body),
    )?;
    println!("Office configuration reloaded.");
    Ok(())
}

/// Make an HTTP request to the daemon API using curl.
fn daemon_api_request(method: &str, url: &str, body: Option<&serde_json::Value>) -> Result<String> {
    let mut cmd = std::process::Command::new("curl");
    cmd.arg("-s")
        .arg("-f") // fail on HTTP errors
        .arg("-X")
        .arg(method);

    if let Some(body) = body {
        cmd.arg("-H")
            .arg("Content-Type: application/json")
            .arg("-d")
            .arg(body.to_string());
    }

    cmd.arg(url);

    let output = cmd.output().map_err(|e| {
        anyhow::anyhow!(
            "Cannot run curl: {}. Is the office daemon running? \
             Start it with `ta office start --config office.yaml`.",
            e
        )
    })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Daemon API request failed ({} {}): {}. \
             Is the office daemon running?",
            method,
            url,
            stderr.trim()
        )
    }
}

/// Get the daemon URL from config or defaults.
fn get_daemon_url(project_root: &std::path::Path) -> String {
    // Check for TA_DAEMON_URL env var first.
    if let Ok(url) = std::env::var("TA_DAEMON_URL") {
        return url;
    }

    // Try to read from .ta/daemon.toml.
    let config_path = project_root.join(".ta").join("daemon.toml");
    if config_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(config) = toml::from_str::<toml::Value>(&content) {
                let bind = config
                    .get("server")
                    .and_then(|s| s.get("bind"))
                    .and_then(|b| b.as_str())
                    .unwrap_or("127.0.0.1");
                let port = config
                    .get("server")
                    .and_then(|s| s.get("port"))
                    .and_then(|p| p.as_integer())
                    .unwrap_or(7700);
                return format!("http://{}:{}", bind, port);
            }
        }
    }

    "http://127.0.0.1:7700".to_string()
}

fn expand_home(path: &str) -> String {
    if path.starts_with("~/") || path == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return path.replacen('~', &home, 1);
        }
    }
    path.to_string()
}
