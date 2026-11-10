//! `ta chat-read`: see and debug what the chat-mode Chief of Staff may READ.
//!
//! * `check <path>` says whether a path would be readable and WHY: which rule (an
//!   `allow` entry, a `deny` entry, the project root, the built-in protected list, TA
//!   infrastructure, or the policy engine's secrets backstop) decided it.
//! * `list` prints the compiled policy: the project root, every allow and deny entry
//!   as resolved, and the built-in protected locations that no config can override.
//!
//! Both build the policy exactly as `ta run --chat-mode` does
//! (`ta_mcp_gateway::chat_launch::load_read_policy`) and change nothing. They show
//! what a NEW session would get; a running session keeps the policy in its token.

use std::path::Path;

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use ta_policy::read_policy::{display_path, ReadPolicy, Verdict};

#[derive(Subcommand, Debug)]
pub enum ChatReadCommands {
    /// Say whether the chat-mode session may read PATH, and which rule decides.
    ///
    /// PATH is absolute, `~/path`, or relative to the project root. Exits 0 when the
    /// path is allowed and 1 when it is denied.
    Check {
        /// The path to judge.
        path: String,
        /// Resolve a relative PATH against this allowed, named project instead of the
        /// project root.
        #[arg(long)]
        project: Option<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print the compiled read policy: project root, allow, deny, built-in protected.
    List {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

pub fn execute(cmd: &ChatReadCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let root = config.source_root();
    let policy = ta_mcp_gateway::chat_launch::load_read_policy(&root).map_err(|e| {
        anyhow::anyhow!(
            "{e}. Fix `[chat.read]` in {}/.ta/workflow.toml and run this again.",
            root.display()
        )
    })?;
    match cmd {
        ChatReadCommands::Check {
            path,
            project,
            json,
        } => check(&root, &policy, path, project.as_deref(), *json),
        ChatReadCommands::List { json } => list(&policy, *json),
    }
}

fn check(
    root: &Path,
    policy: &ReadPolicy,
    path: &str,
    project: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let base = policy
        .root_of_label(project)
        .map_err(|why| anyhow::anyhow!("{why}"))?;
    let home = dirs::home_dir();
    let verdict = policy.decide_requested(&base, home.as_deref(), path);

    // The policy engine's secrets backstop applies on top of the lists, exactly as in
    // the gateway: compile the chat manifest and evaluate the same request.
    let (allowed, resolved, reason, rule) = match verdict {
        Verdict::Allowed {
            resolved,
            rule,
            root: admitting,
        } => match engine_refusal(root, &resolved) {
            Some(why) => (
                false,
                resolved,
                format!(
                    "the policy engine refuses it after the lists allowed it: {why} \
                     (secret and credential files such as .env, keys and *.pem are never \
                     readable; no config entry changes that)"
                ),
                Some(rule),
            ),
            None => {
                let how =
                    if admitting == policy.project_root && rule.starts_with("the project root") {
                        "inside the project root (always readable)".to_string()
                    } else {
                        format!(
                            "matched `allow` entry {rule:?} (root {})",
                            display_path(&admitting)
                        )
                    };
                (true, resolved, how, Some(rule))
            }
        },
        Verdict::Denied(refusal) => (
            false,
            refusal.resolved.clone(),
            refusal.message.clone(),
            refusal.rule.clone(),
        ),
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": path,
                "resolved": display_path(&resolved),
                "allowed": allowed,
                "rule": rule,
                "reason": reason,
            }))?
        );
    } else {
        println!(
            "{}: {}",
            if allowed { "ALLOWED" } else { "DENIED" },
            display_path(&resolved)
        );
        println!("  why: {reason}");
        if !allowed {
            println!(
                "  to change it: edit `[chat.read]` (`allow` / `deny`) in {}/.ta/workflow.toml \
                 (built-in protected locations cannot be changed).",
                root.display()
            );
        }
    }
    if !allowed {
        // Distinct non-zero exit for scripts; the text above already said why.
        std::process::exit(1);
    }
    Ok(())
}

/// Why the policy engine refuses a read of `resolved` for a chat session, if it does.
fn engine_refusal(root: &Path, resolved: &Path) -> Option<String> {
    let manifest = ta_policy::compile_chat_manifest_with_read_access(
        "ta-chat-read-check",
        "fs://workspace/**",
        1,
        &ta_policy::ChatReadAccess {
            project_root: Some(root.to_path_buf()),
            allow: Vec::new(),
            deny: Vec::new(),
            tools: Vec::new(),
        },
    )
    .ok()?;
    let mut engine = ta_policy::PolicyEngine::new();
    engine.load_manifest(manifest);
    match engine.evaluate(&ta_policy::PolicyRequest {
        agent_id: "ta-chat-read-check".to_string(),
        tool: "fs".to_string(),
        verb: "read".to_string(),
        target_uri: ta_policy::read_policy::read_resource(resolved),
    }) {
        ta_policy::PolicyDecision::Allow => None,
        other => Some(format!("{other:?}")),
    }
}

fn list(policy: &ReadPolicy, json: bool) -> anyhow::Result<()> {
    let rules = |v: &[ta_policy::read_policy::ReadRule]| {
        v.iter()
            .map(|r| {
                serde_json::json!({
                    "entry": r.source,
                    "resolved": r.pattern,
                    "kind": format!("{:?}", r.kind).to_lowercase(),
                    "project_name": r.label,
                    "registered_project": r.registered,
                })
            })
            .collect::<Vec<_>>()
    };
    let hard: Vec<_> = policy
        .hard_deny_list()
        .into_iter()
        .map(|(p, why)| serde_json::json!({"path": display_path(&p), "why": why}))
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "project_root": display_path(&policy.project_root),
                "allow": rules(&policy.allow),
                "deny": rules(&policy.deny),
                "built_in_deny": hard,
            }))?
        );
        return Ok(());
    }
    println!(
        "project root (always readable): {}",
        display_path(&policy.project_root)
    );
    println!("allow ({}):", policy.allow.len());
    for r in &policy.allow {
        println!("  {}  ->  {}", r.source, r.pattern);
    }
    println!("deny ({}):", policy.deny.len());
    for r in &policy.deny {
        println!("  {}  ->  {}", r.source, r.pattern);
    }
    println!("built-in deny (cannot be overridden):");
    for (p, why) in policy.hard_deny_list() {
        println!("  {}  ({why})", display_path(&p));
    }
    if policy.allow.is_empty() {
        println!(
            "\nNo `allow` entries: the session reads the project root only. Add \
             `[chat.read] allow = [...]` to .ta/workflow.toml to widen it."
        );
    }
    Ok(())
}
