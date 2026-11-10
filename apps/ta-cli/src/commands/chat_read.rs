//! `ta chat-read`: see, debug and set what the chat-mode Chief of Staff may READ.
//!
//! * `check <path>` says whether a path would be readable and WHY: which rule (an
//!   `allow` entry, a `deny` entry, the project root, the built-in protected list, TA
//!   infrastructure, or the policy engine's secrets backstop) decided it.
//! * `list` prints the compiled policy: the project root, every allow and deny entry
//!   as resolved with the layer it came from, and the built-in protected locations that
//!   no config can override. `--json` is the machine-readable form the Wayfinder
//!   poller reads.
//! * `resolve <entry> --list allow|deny` says what TA would make of one entry and
//!   writes nothing.
//! * `machine set|show` writes and reads the per-machine overlay
//!   (`<TA state dir>/projects/<project-id>/chat-read.toml`), which is added to
//!   `[chat.read]` of the project's `.ta/workflow.toml`.
//!
//! All of them build the policy exactly as `ta run --chat-mode` does
//! (`ta_mcp_gateway::chat_launch::load_read_policy`); only `machine set` writes. They
//! show what a NEW session would get; a running session keeps the policy in its token.

use std::path::Path;

use clap::Subcommand;
use serde_json::{json, Value};
use ta_credentials::state::StateLocator;
use ta_mcp_gateway::chat_launch::{self, ReadListing};
use ta_mcp_gateway::GatewayConfig;
use ta_policy::read_policy::{
    display_path, ListKind, ListedEntry, MachineLists, ReadPolicy, Verdict,
};
use ta_policy::read_policy::{EntrySource, EntryState};

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
    ///
    /// Every entry says where it comes from (`project`: `[chat.read]` in
    /// `.ta/workflow.toml`; `machine`: the per-machine overlay) and whether it is
    /// `applied`, `unresolved` (TA cannot resolve it: missing path, unknown project) or
    /// `refused` (a protected place, the whole home folder), with the reason.
    List {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Say what TA would make of ENTRY (applied, unresolved or refused) without
    /// writing anything.
    Resolve {
        /// The entry: an absolute path, `~/path`, a registered project name or a glob.
        entry: String,
        /// Which list the entry is for.
        #[arg(long = "list", value_parser = ["allow", "deny"])]
        list: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// The per-machine overlay: read lists that belong to THIS computer.
    Machine {
        #[command(subcommand)]
        command: MachineCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum MachineCommands {
    /// Atomically REPLACE the per-machine overlay with exactly these entries.
    ///
    /// Each entry is resolved first. `refused` entries (protected places, the whole home
    /// folder) and `unresolved` allow entries are not written; an `unresolved` deny entry
    /// is written (a deny for a path that does not exist yet is harmless). Nothing is
    /// dropped silently: every entry is reported with its state. With no entries the
    /// overlay is cleared.
    Set {
        /// An allow entry (repeatable).
        #[arg(long = "allow")]
        allow: Vec<String>,
        /// A deny entry (repeatable).
        #[arg(long = "deny")]
        deny: Vec<String>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print the per-machine overlay as stored.
    Show {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// True when the command prints a JSON document on stdout.
pub fn prints_json(cmd: &ChatReadCommands) -> bool {
    match cmd {
        ChatReadCommands::Check { json, .. }
        | ChatReadCommands::List { json }
        | ChatReadCommands::Resolve { json, .. } => *json,
        ChatReadCommands::Machine { command } => match command {
            MachineCommands::Set { json, .. } | MachineCommands::Show { json } => *json,
        },
    }
}

pub fn execute(cmd: &ChatReadCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let root = config.source_root();
    match cmd {
        ChatReadCommands::Check {
            path,
            project,
            json,
        } => {
            let listing = chat_launch::load_read_listing(&root, true).map_err(|e| {
                anyhow::anyhow!(
                    "{e}. Fix `[chat.read]` in {}/.ta/workflow.toml (or the machine file shown by \
                     `ta chat-read machine show`) and run this again.",
                    root.display()
                )
            })?;
            check(&root, &listing, path, project.as_deref(), *json)
        }
        ChatReadCommands::List { json } => {
            let listing = chat_launch::load_read_listing(&root, false).map_err(|e| {
                anyhow::anyhow!(
                    "{e}. `ta chat-read list` could not read the lists of {}; fix the file named \
                     above and run it again.",
                    root.display()
                )
            })?;
            list(&listing, *json)
        }
        ChatReadCommands::Resolve {
            entry,
            list: which,
            json,
        } => {
            let listing = chat_launch::load_read_listing(&root, false).map_err(|e| {
                anyhow::anyhow!("{e}. `ta chat-read resolve` needs readable lists.")
            })?;
            resolve(&listing, entry, list_kind(which), *json)
        }
        ChatReadCommands::Machine { command } => match command {
            MachineCommands::Set { allow, deny, json } => {
                let locator = StateLocator::from_env().map_err(|e| {
                    anyhow::anyhow!(
                        "cannot find the TA state directory to store the machine read lists: {e}"
                    )
                })?;
                let out = machine_set(&root, &locator, allow, deny)?;
                print_set(&out, *json)
            }
            MachineCommands::Show { json } => {
                let locator = StateLocator::from_env().ok();
                machine_show(&root, locator.as_ref(), *json)
            }
        },
    }
}

fn list_kind(s: &str) -> ListKind {
    if s == "deny" {
        ListKind::Deny
    } else {
        ListKind::Allow
    }
}

// ── check ──────────────────────────────────────────────────────────────────

fn check(
    root: &Path,
    listing: &ReadListing,
    path: &str,
    project: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let home = dirs::home_dir();
    let j = judge(
        root,
        &listing.policy,
        &listing.entries,
        path,
        project,
        home.as_deref(),
    )?;
    let (allowed, resolved, reason, rule) = (j.allowed, j.resolved, j.reason, j.rule);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
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
                "  to change it: edit `[chat.read]` (`allow` / `deny`) in {}/.ta/workflow.toml, \
                 or set this computer's lists in Wayfinder Settings (stored in {}). Built-in \
                 protected locations cannot be changed.",
                root.display(),
                listing.machine_file.display()
            );
        }
    }
    if !allowed {
        // Distinct non-zero exit for scripts; the text above already said why.
        std::process::exit(1);
    }
    Ok(())
}

/// The verdict for `path`, with the reason a person needs to act on it.
struct Judgement {
    allowed: bool,
    resolved: std::path::PathBuf,
    reason: String,
    rule: Option<String>,
}

/// Where the applied entry `entry` of `list` comes from, for messages.
fn source_of(entries: &[ListedEntry], list: ListKind, entry: &str) -> &'static str {
    entries
        .iter()
        .find(|e| e.list == list && e.state == EntryState::Applied && e.entry == entry)
        .map(|e| match e.source {
            EntrySource::Project => "the project file",
            EntrySource::Machine => "this computer's machine list",
        })
        .unwrap_or("the lists")
}

fn judge(
    root: &Path,
    policy: &ReadPolicy,
    entries: &[ListedEntry],
    path: &str,
    project: Option<&str>,
    home: Option<&Path>,
) -> anyhow::Result<Judgement> {
    let base = policy
        .root_of_label(project)
        .map_err(|why| anyhow::anyhow!("{why}"))?;
    let verdict = policy.decide_requested(&base, home, path);
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
                            "matched `allow` entry {rule:?} from {} (root {})",
                            source_of(entries, ListKind::Allow, &rule),
                            display_path(&admitting)
                        )
                    };
                (true, resolved, how, Some(rule))
            }
        },
        Verdict::Denied(refusal) => {
            let mut message = refusal.message.clone();
            if refusal.code == ta_policy::read_policy::RefusalCode::UserDeny {
                if let Some(rule) = &refusal.rule {
                    message.push_str(&format!(
                        " (the `deny` entry comes from {})",
                        source_of(entries, ListKind::Deny, rule)
                    ));
                }
            }
            (
                false,
                refusal.resolved.clone(),
                message,
                refusal.rule.clone(),
            )
        }
    };
    Ok(Judgement {
        allowed,
        resolved,
        reason,
        rule,
    })
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

// ── list ───────────────────────────────────────────────────────────────────

/// One entry as JSON. The first four keys plus `detail` are the contract the poller
/// reads; the rest describe the resolved rule (null when not applied).
fn entry_json(e: &ListedEntry) -> Value {
    json!({
        "entry": e.entry,
        "source": e.source.as_str(),
        "state": e.state.as_str(),
        "detail": e.detail,
        "resolved": e.rule.as_ref().map(|r| r.pattern.clone()),
        "kind": e.rule.as_ref().map(|r| format!("{:?}", r.kind).to_lowercase()),
        "project_name": e.rule.as_ref().and_then(|r| r.label.clone()),
        "registered_project": e.rule.as_ref().map(|r| r.registered),
    })
}

/// The JSON of `list` and of `machine set`.
fn listing_json(entries: &[ListedEntry], policy: &ReadPolicy, machine_file: &Path) -> Value {
    let of = |list: ListKind| -> Vec<Value> {
        entries
            .iter()
            .filter(|e| e.list == list)
            .map(entry_json)
            .collect()
    };
    let hard = policy.hard_deny_list();
    json!({
        "project_root": display_path(&policy.project_root),
        "allow": of(ListKind::Allow),
        "deny": of(ListKind::Deny),
        "protected": hard.iter().map(|(p, _)| display_path(p)).collect::<Vec<_>>(),
        "built_in_deny": hard
            .iter()
            .map(|(p, why)| json!({"path": display_path(p), "why": why}))
            .collect::<Vec<_>>(),
        "machine_file": display_path(machine_file),
    })
}

fn describe(e: &ListedEntry) -> String {
    let layer = match e.source {
        EntrySource::Project => "project file",
        EntrySource::Machine => "machine list",
    };
    match (&e.rule, e.state) {
        (Some(r), _) => format!("  {}  ->  {}  ({layer})", e.entry, r.pattern),
        (None, state) => format!(
            "  {}  ->  {} ({layer}): {}",
            e.entry,
            state.as_str().to_uppercase(),
            e.detail.as_deref().unwrap_or("no reason recorded")
        ),
    }
}

fn print_entries(entries: &[ListedEntry]) {
    for (list, title) in [(ListKind::Allow, "allow"), (ListKind::Deny, "deny")] {
        let of: Vec<&ListedEntry> = entries.iter().filter(|e| e.list == list).collect();
        println!("{title} ({}):", of.len());
        for e in of {
            println!("{}", describe(e));
        }
    }
}

fn list(listing: &ReadListing, json: bool) -> anyhow::Result<()> {
    let policy = &listing.policy;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&listing_json(
                &listing.entries,
                policy,
                &listing.machine_file
            ))?
        );
        return Ok(());
    }
    println!(
        "project root (always readable): {}",
        display_path(&policy.project_root)
    );
    print_entries(&listing.entries);
    println!(
        "machine list file (this computer only): {}",
        display_path(&listing.machine_file)
    );
    println!("built-in deny (cannot be overridden):");
    for (p, why) in policy.hard_deny_list() {
        println!("  {}  ({why})", display_path(&p));
    }
    if policy.allow.is_empty() {
        println!(
            "\nNo applied `allow` entries: the session reads the project root only. Add \
             `[chat.read] allow = [...]` to .ta/workflow.toml, or set this computer's list in \
             Wayfinder Settings."
        );
    }
    let unusable = listing
        .entries
        .iter()
        .filter(|e| e.state != EntryState::Applied)
        .count();
    if unusable > 0 {
        println!(
            "\n{unusable} entr{} not applied (see UNRESOLVED / REFUSED above). A new session \
             ignores machine-list entries that are not applied; fix or remove them.",
            if unusable == 1 { "y is" } else { "ies are" }
        );
    }
    Ok(())
}

// ── resolve ────────────────────────────────────────────────────────────────

fn resolve(listing: &ReadListing, entry: &str, list: ListKind, json: bool) -> anyhow::Result<()> {
    let current = match list {
        ListKind::Allow => &listing.policy.allow,
        ListKind::Deny => &listing.policy.deny,
    };
    let (state, detail) =
        ta_policy::read_policy::resolve_single(entry, list, &listing.env, current);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "entry": entry,
                "list": list.as_str(),
                "state": state.as_str(),
                "detail": detail,
            }))?
        );
    } else {
        println!(
            "{} entry {entry:?}: {}{}",
            list.as_str(),
            state.as_str().to_uppercase(),
            detail.map(|d| format!(": {d}")).unwrap_or_default()
        );
    }
    Ok(())
}

// ── machine set / show ─────────────────────────────────────────────────────

/// What `machine set` did.
struct SetOutcome {
    /// Every entry of both layers with its state (requested machine entries included
    /// even when they were not written).
    entries: Vec<ListedEntry>,
    policy: ReadPolicy,
    machine_file: std::path::PathBuf,
    written_allow: usize,
    written_deny: usize,
}

fn unique_trimmed(entries: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in entries {
        let e = e.trim().to_string();
        if !e.is_empty() && !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

/// Resolve the requested entries against this machine, then atomically replace the
/// overlay with the writable ones (see [`MachineCommands::Set`]).
fn machine_set(
    root: &Path,
    locator: &StateLocator,
    allow: &[String],
    deny: &[String],
) -> anyhow::Result<SetOutcome> {
    let requested = MachineLists {
        allow: unique_trimmed(allow),
        deny: unique_trimmed(deny),
    };
    if let Some(blank) = allow.iter().chain(deny).find(|e| e.trim().is_empty()) {
        anyhow::bail!(
            "an empty entry ({blank:?}) was passed to `ta chat-read machine set`; nothing was \
             written. Pass real paths, for example `--allow ~/notes`."
        );
    }
    // Resolve against the state directory the overlay will live in (it is part of
    // the protected floor), creating it on first use.
    let state = ta_credentials::state::ProjectState::open(root, locator).map_err(|e| {
        anyhow::anyhow!(
            "could not open the TA state directory of {} to store the machine read lists: {e}. \
             Nothing was written.",
            root.display()
        )
    })?;
    let (project_allow, project_deny) = chat_launch::project_lists(root)
        .map_err(|e| anyhow::anyhow!("{e}. Nothing was written."))?;
    let env =
        ta_policy::read_policy::ReadEnv::for_project(root).with_state_dir(Some(state.dir.clone()));
    let (_, entries) = ta_policy::read_policy::compile_layers(
        &env,
        ta_policy::read_policy::Layers {
            project_allow: &project_allow,
            project_deny: &project_deny,
            machine: &requested,
        },
        false,
    )
    .map_err(|e| anyhow::anyhow!("{e}. Nothing was written."))?;

    let mut keep = MachineLists::default();
    for e in entries.iter().filter(|e| e.source == EntrySource::Machine) {
        let writable = !matches!(
            (e.list, e.state),
            (_, EntryState::Refused) | (ListKind::Allow, EntryState::Unresolved)
        );
        if writable {
            match e.list {
                ListKind::Allow => keep.allow.push(e.entry.clone()),
                ListKind::Deny => keep.deny.push(e.entry.clone()),
            }
        }
    }
    let machine_file = chat_launch::write_machine_lists(root, locator, &keep)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // The policy after the write, as a new session would compile it.
    let (policy, _) = ta_policy::read_policy::compile_layers(
        &env,
        ta_policy::read_policy::Layers {
            project_allow: &project_allow,
            project_deny: &project_deny,
            machine: &keep,
        },
        false,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(SetOutcome {
        entries,
        policy,
        machine_file,
        written_allow: keep.allow.len(),
        written_deny: keep.deny.len(),
    })
}

fn print_set(out: &SetOutcome, json: bool) -> anyhow::Result<()> {
    if json {
        let mut v = listing_json(&out.entries, &out.policy, &out.machine_file);
        v["written"] = json!({"allow": out.written_allow, "deny": out.written_deny});
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!(
        "Replaced the machine read lists in {} ({} allow, {} deny written).",
        display_path(&out.machine_file),
        out.written_allow,
        out.written_deny
    );
    print_entries(&out.entries);
    let skipped = out
        .entries
        .iter()
        .filter(|e| e.source == EntrySource::Machine && e.state != EntryState::Applied)
        .count();
    if skipped > 0 {
        println!(
            "\n{skipped} machine entr{} not applied: REFUSED entries are never written, \
             UNRESOLVED allow entries are not written (fix the path and set again); an \
             UNRESOLVED deny entry is written and applies once it resolves.",
            if skipped == 1 { "y is" } else { "ies are" }
        );
    }
    Ok(())
}

fn machine_show(root: &Path, locator: Option<&StateLocator>, json: bool) -> anyhow::Result<()> {
    let (path, lists) = match locator {
        Some(locator) => {
            let path = chat_launch::machine_lists_path(root, locator);
            let lists =
                chat_launch::read_machine_lists(&path).map_err(|e| anyhow::anyhow!("{e}"))?;
            (path, lists)
        }
        None => (
            std::path::PathBuf::from("<no TA state directory could be determined>"),
            MachineLists::default(),
        ),
    };
    let exists = path.is_file();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "allow": lists.allow,
                "deny": lists.deny,
                "machine_file": display_path(&path),
                "exists": exists,
            }))?
        );
    } else {
        println!(
            "machine read lists ({}): {}",
            if exists { "stored" } else { "not written yet" },
            display_path(&path)
        );
        println!("allow ({}):", lists.allow.len());
        for e in &lists.allow {
            println!("  {e}");
        }
        println!("deny ({}):", lists.deny.len());
        for e in &lists.deny {
            println!("  {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use ta_policy::read_policy::{ReadEnv, ReadPolicy};

    fn policy(root: &Path, home: &Path, allow: &[&str], deny: &[&str]) -> ReadPolicy {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        ReadPolicy::from_config(
            &ReadEnv {
                project_root: root.to_path_buf(),
                home: Some(home.to_path_buf()),
                state_dir: None,
                office_yaml: root.join("office.yaml"),
            },
            &s(allow),
            &s(deny),
        )
        .unwrap()
    }

    #[test]
    fn check_names_the_rule_behind_every_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(home.join("notes/private")).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::create_dir_all(home.join("other")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "x").unwrap();
        std::fs::write(home.join("notes/a.md"), "x").unwrap();
        std::fs::write(home.join("notes/private/b.md"), "x").unwrap();
        std::fs::write(home.join(".ssh/id"), "x").unwrap();
        std::fs::write(home.join("other/c.md"), "x").unwrap();
        let p = policy(&root, &home, &["~/notes"], &["~/notes/private"]);
        let judge_of = |path: &str| judge(&root, &p, &[], path, None, Some(&home)).unwrap();

        let j = judge_of("src/lib.rs");
        assert!(
            j.allowed && j.reason.contains("project root"),
            "{}",
            j.reason
        );

        let j = judge_of("~/notes/a.md");
        assert!(
            j.allowed && j.reason.contains("\"~/notes\""),
            "{}",
            j.reason
        );

        let j = judge_of("~/notes/private/b.md");
        assert!(!j.allowed, "deny beats allow");
        assert_eq!(j.rule.as_deref(), Some("~/notes/private"));
        assert!(j.reason.contains("deny"), "{}", j.reason);

        let j = judge_of("~/other/c.md");
        assert!(!j.allowed && j.reason.contains("[chat.read]") && j.reason.contains("allow"));

        let j = judge_of("~/.ssh/id");
        assert!(
            !j.allowed && j.reason.contains("built-in hard deny"),
            "{}",
            j.reason
        );

        let j = judge_of("~/notes/../other/c.md");
        assert!(!j.allowed && j.reason.contains(".."), "{}", j.reason);
    }

    // ── per-machine overlay ────────────────────────────────────────────────

    struct Scratch {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        base: PathBuf,
        locator: StateLocator,
    }

    /// A scratch project and a scratch TA state directory, both in a tempdir (never
    /// the real HOME or state dir).
    fn scratch() -> Scratch {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let root = base.join("proj");
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for d in ["notes/private", "notes/ok", "shared/secret", "gone-later"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        for f in [
            "notes/ok/a.md",
            "notes/private/b.md",
            "shared/x.md",
            "shared/secret/s.md",
        ] {
            std::fs::write(base.join(f), "x").unwrap();
        }
        let locator = StateLocator::at(base.join("state"));
        Scratch {
            _tmp: tmp,
            root,
            base,
            locator,
        }
    }

    impl Scratch {
        fn p(&self, rel: &str) -> String {
            self.base.join(rel).to_string_lossy().to_string()
        }
        fn project_toml(&self, body: &str) {
            std::fs::write(self.root.join(".ta/workflow.toml"), body).unwrap();
        }
        fn set(&self, allow: &[String], deny: &[String]) -> SetOutcome {
            machine_set(&self.root, &self.locator, allow, deny).unwrap()
        }
        fn listing(&self, strict: bool) -> ReadListing {
            chat_launch::load_read_listing_in(&self.root, Some(&self.locator), strict).unwrap()
        }
    }

    fn state_of(entries: &[ListedEntry], list: ListKind, entry: &str) -> EntryState {
        entries
            .iter()
            .find(|e| e.list == list && e.entry == entry)
            .unwrap_or_else(|| panic!("no {list:?} entry {entry}"))
            .state
    }

    #[test]
    fn machine_set_writes_what_applies_and_reports_the_rest() {
        let s = scratch();
        let notes = s.p("notes");
        let missing = s.p("does-not-exist");
        let future_deny = s.p("not-created-yet");
        let private = s.p("notes/private");
        let out = s.set(
            &[notes.clone(), missing.clone(), "/".to_string()],
            &[private.clone(), future_deny.clone()],
        );
        assert_eq!(
            state_of(&out.entries, ListKind::Allow, &notes),
            EntryState::Applied
        );
        assert_eq!(
            state_of(&out.entries, ListKind::Allow, &missing),
            EntryState::Unresolved
        );
        assert_eq!(
            state_of(&out.entries, ListKind::Allow, "/"),
            EntryState::Refused
        );
        assert_eq!(
            state_of(&out.entries, ListKind::Deny, &private),
            EntryState::Applied
        );
        // A deny for a path that does not exist yet is harmless: resolved and written.
        assert_eq!(
            state_of(&out.entries, ListKind::Deny, &future_deny),
            EntryState::Applied
        );
        assert_eq!((out.written_allow, out.written_deny), (1, 2));

        // Only the writable entries are on disk; refused and unresolved allow are not.
        let on_disk = chat_launch::read_machine_lists(&out.machine_file).unwrap();
        assert_eq!(on_disk.allow, vec![notes]);
        assert_eq!(on_disk.deny, vec![private, future_deny]);
        let file = std::fs::read_to_string(&out.machine_file).unwrap();
        assert!(
            !file.contains("does-not-exist") && !file.contains("\"/\""),
            "{file}"
        );

        // The path is <state>/projects/<project-id>/chat-read.toml.
        let ps = ta_credentials::state::ProjectState::locate(&s.root, &s.locator)
            .unwrap()
            .unwrap();
        assert_eq!(out.machine_file, ps.dir.join("chat-read.toml"));
        assert!(out.machine_file.starts_with(s.locator.projects_dir()));
    }

    #[cfg(unix)]
    #[test]
    fn machine_file_is_private_and_replace_leaves_no_temp_files() {
        use std::os::unix::fs::PermissionsExt;
        let s = scratch();
        let first = s.set(&[s.p("notes")], &[]);
        let mode = std::fs::metadata(&first.machine_file)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        let second = s.set(&[s.p("shared")], &[]);
        assert_eq!(second.machine_file, first.machine_file);
        let mode = std::fs::metadata(&second.machine_file)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        let names: Vec<String> = std::fs::read_dir(second.machine_file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            names.iter().all(|n| !n.contains(".tmp-")),
            "leftover temp file: {names:?}"
        );
    }

    #[test]
    fn machine_set_replaces_rather_than_merges_and_empty_clears() {
        let s = scratch();
        s.set(&[s.p("notes")], &[s.p("notes/private")]);
        let second = s.set(&[s.p("shared")], &[]);
        let on_disk = chat_launch::read_machine_lists(&second.machine_file).unwrap();
        assert_eq!(on_disk.allow, vec![s.p("shared")]);
        assert!(on_disk.deny.is_empty(), "{on_disk:?}");
        let cleared = s.set(&[], &[]);
        let on_disk = chat_launch::read_machine_lists(&cleared.machine_file).unwrap();
        assert_eq!(on_disk, MachineLists::default());
        assert_eq!((cleared.written_allow, cleared.written_deny), (0, 0));
    }

    #[test]
    fn project_deny_beats_machine_allow_and_machine_deny_beats_project_allow() {
        let s = scratch();
        s.project_toml(&format!(
            "[chat.read]\nallow = [{:?}]\ndeny = [{:?}]\n",
            s.p("notes"),
            s.p("shared/secret")
        ));
        s.set(&[s.p("shared")], &[s.p("notes/private")]);
        let l = s.listing(true);
        let decide = |rel: &str| l.policy.decide(&s.base.join(rel)).is_allowed();
        assert!(decide("notes/ok/a.md"), "project allow");
        assert!(decide("shared/x.md"), "machine allow");
        assert!(
            !decide("notes/private/b.md"),
            "machine deny beats project allow"
        );
        assert!(
            !decide("shared/secret/s.md"),
            "project deny beats machine allow"
        );
        let sources: Vec<(&str, &str)> = l
            .entries
            .iter()
            .map(|e| (e.list.as_str(), e.source.as_str()))
            .collect();
        assert_eq!(
            sources,
            vec![
                ("allow", "project"),
                ("allow", "machine"),
                ("deny", "project"),
                ("deny", "machine")
            ]
        );
    }

    #[test]
    fn the_project_file_alone_is_unchanged_when_there_is_no_machine_file() {
        let s = scratch();
        s.project_toml(&format!("[chat.read]\nallow = [{:?}]\n", s.p("notes")));
        let l = s.listing(true);
        assert!(l.policy.decide(&s.base.join("notes/ok/a.md")).is_allowed());
        assert!(!l.policy.decide(&s.base.join("shared/x.md")).is_allowed());
        assert!(l.entries.iter().all(|e| e.source == EntrySource::Project));
        assert!(!l.machine_file.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_dotdot_escapes_stay_refused_for_machine_entries() {
        let s = scratch();
        std::os::unix::fs::symlink(s.base.join("shared"), s.base.join("notes/ok/link")).unwrap();
        let out = s.set(&[s.p("notes"), format!("{}/../shared", s.p("notes"))], &[]);
        let dotdot = format!("{}/../shared", s.p("notes"));
        assert_eq!(
            state_of(&out.entries, ListKind::Allow, &dotdot),
            EntryState::Refused
        );
        let l = s.listing(true);
        assert!(l.policy.decide(&s.base.join("notes/ok/a.md")).is_allowed());
        assert!(
            !l.policy
                .decide(&s.base.join("notes/ok/link/x.md"))
                .is_allowed(),
            "a symlink out of an allowed root is refused"
        );
        assert!(!l.policy.decide(&s.base.join("shared/x.md")).is_allowed());
    }

    #[test]
    fn the_floor_cannot_be_widened_by_the_machine_list() {
        let s = scratch();
        let git = s.root.join(".git").to_string_lossy().to_string();
        let ta = s.root.join(".ta").to_string_lossy().to_string();
        let state_dir = ta_credentials::state::ProjectState::open(&s.root, &s.locator)
            .unwrap()
            .dir;
        let state = state_dir.to_string_lossy().to_string();
        let out = s.set(&[git.clone(), ta.clone(), state.clone()], &[]);
        for e in [&git, &ta, &state] {
            assert_eq!(
                state_of(&out.entries, ListKind::Allow, e),
                EntryState::Refused,
                "{e}"
            );
        }
        assert_eq!(out.written_allow, 0);
        let l = s.listing(true);
        assert!(!l.policy.decide(&s.root.join(".git/config")).is_allowed());
        assert!(!l
            .policy
            .decide(&s.root.join(".ta/workflow.toml"))
            .is_allowed());
    }

    #[test]
    fn a_machine_entry_that_stopped_resolving_is_skipped_not_fatal_and_is_listed() {
        let s = scratch();
        let gone = s.p("gone-later");
        s.set(&[gone.clone(), s.p("notes")], &[]);
        std::fs::remove_dir_all(s.base.join("gone-later")).unwrap();
        // The launch path (strict project entries) still compiles.
        let l = s.listing(true);
        assert_eq!(
            state_of(&l.entries, ListKind::Allow, &gone),
            EntryState::Unresolved
        );
        assert!(l.policy.decide(&s.base.join("notes/ok/a.md")).is_allowed());
        let j = listing_json(&l.entries, &l.policy, &l.machine_file);
        let e = &j["allow"][0];
        assert_eq!(e["state"], "unresolved");
        assert!(
            e["detail"].as_str().unwrap().contains("cannot be resolved"),
            "{e}"
        );
    }

    #[test]
    fn a_broken_project_entry_stays_loud_at_launch_but_is_reported_by_list() {
        let s = scratch();
        s.project_toml("[chat.read]\nallow = [\"/definitely/not/here\"]\n");
        let err = chat_launch::load_read_listing_in(&s.root, Some(&s.locator), true)
            .err()
            .expect("strict launch must fail")
            .to_string();
        assert!(
            err.contains("/definitely/not/here") && err.contains("workflow.toml"),
            "{err}"
        );
        let l = s.listing(false);
        assert_eq!(
            state_of(&l.entries, ListKind::Allow, "/definitely/not/here"),
            EntryState::Unresolved
        );
    }

    #[test]
    fn a_corrupt_machine_file_is_a_loud_error_naming_the_file_and_the_fix() {
        let s = scratch();
        let out = s.set(&[s.p("notes")], &[]);
        std::fs::write(&out.machine_file, "allow = [\"a\"\nthis is not toml").unwrap();
        let err = chat_launch::load_read_listing_in(&s.root, Some(&s.locator), true)
            .err()
            .expect("a corrupt overlay must not silently drop a deny")
            .to_string();
        assert!(
            err.contains("chat-read.toml") && err.contains("ta chat-read machine set"),
            "{err}"
        );
        // `machine set` replaces a corrupt file instead of tripping over it.
        let again = s.set(&[s.p("shared")], &[]);
        assert_eq!(again.written_allow, 1);
        s.listing(true);
    }

    #[test]
    fn unknown_keys_in_the_machine_file_are_rejected() {
        let err = MachineLists::parse("alow = [\"x\"]\n", Path::new("chat-read.toml"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("chat-read.toml") && err.contains("alow"),
            "{err}"
        );
    }

    #[test]
    fn render_round_trips_awkward_entries() {
        let lists = MachineLists {
            allow: vec![
                "/a b/\"quoted\"".to_string(),
                "C:\\Users\\me\\notes".to_string(),
                "tab\there".to_string(),
                "ünï/💡".to_string(),
            ],
            deny: vec!["~/x".to_string()],
        };
        let text = lists.render();
        assert_eq!(MachineLists::parse(&text, Path::new("f")).unwrap(), lists);
    }

    #[test]
    fn resolve_reports_without_writing() {
        let s = scratch();
        let l = s.listing(false);
        let (state, detail) = ta_policy::read_policy::resolve_single(
            &s.p("does-not-exist"),
            ListKind::Allow,
            &l.env,
            &l.policy.allow,
        );
        assert_eq!(state, EntryState::Unresolved);
        assert!(detail.unwrap().contains("cannot be resolved"));
        let (state, _) = ta_policy::read_policy::resolve_single(
            &s.p("notes"),
            ListKind::Allow,
            &l.env,
            &l.policy.allow,
        );
        assert_eq!(state, EntryState::Applied);
        assert!(
            !s.locator.root().exists(),
            "resolve and list must not create the state directory"
        );
    }

    #[test]
    fn json_shapes_carry_the_contract_keys() {
        let s = scratch();
        let out = s.set(&[s.p("notes"), s.p("nope")], &[s.p("notes/private")]);
        let j = listing_json(&out.entries, &out.policy, &out.machine_file);
        let first = &j["allow"][0];
        for key in ["entry", "source", "state", "detail"] {
            assert!(first.get(key).is_some(), "missing {key}: {first}");
        }
        assert_eq!(first["source"], "machine");
        assert_eq!(first["state"], "applied");
        assert!(first["detail"].is_null());
        assert_eq!(j["allow"][1]["state"], "unresolved");
        assert!(j["allow"][1]["detail"].is_string());
        assert_eq!(j["deny"][0]["state"], "applied");
        assert!(j["protected"].as_array().is_some_and(|a| !a.is_empty()));
        assert!(j["machine_file"]
            .as_str()
            .unwrap()
            .ends_with("chat-read.toml"));
    }

    #[test]
    fn check_names_the_layer_an_entry_comes_from() {
        let s = scratch();
        s.project_toml(&format!("[chat.read]\nallow = [{:?}]\n", s.p("notes")));
        s.set(&[s.p("shared")], &[s.p("notes/private")]);
        let l = s.listing(true);
        let judge_of =
            |path: &str| judge(&s.root, &l.policy, &l.entries, path, None, None).unwrap();
        let j = judge_of(&s.p("notes/ok/a.md"));
        assert!(
            j.allowed && j.reason.contains("the project file"),
            "{}",
            j.reason
        );
        let j = judge_of(&s.p("shared/x.md"));
        assert!(
            j.allowed && j.reason.contains("machine list"),
            "{}",
            j.reason
        );
        let j = judge_of(&s.p("notes/private/b.md"));
        assert!(
            !j.allowed && j.reason.contains("machine list"),
            "{}",
            j.reason
        );
    }
}
