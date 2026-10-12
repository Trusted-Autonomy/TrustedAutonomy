//! `ta rules`: the owner rules file, postures and the effective policy (v0.17.12.4).
//!
//! The table itself lives in `ta_actions::rules`; this file is only the command
//! line around it. Everything that changes what may run unattended (`init`,
//! `sign`, `migrate --write`, `trust`, `rotate-key`) first requires a human
//! credential, so an agent can never edit, sign or replace the rules or the
//! owner key. Read-only commands (`show`, `status`, `diff`, `effective`) stay
//! open: knowing the rules is not a risk.

use std::path::Path;

use anyhow::{bail, Context};
use clap::Subcommand;
use ta_actions::rules::load::{self, RulesLoader, OWNER_FILE};
use ta_actions::rules::{migrate, postures, report, signing, Status};
use ta_credentials::state::{CustodyOpts, ProjectState, StateLocator};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand, Debug)]
pub enum RulesCommands {
    /// Print the table: every action, environment and attendance, with the
    /// rule that decides each cell.
    Show {
        /// Show the table as it is for this role (reads .ta/rules-presets/<role>.toml).
        #[arg(long)]
        role: Option<String>,
    },
    /// Say whether the rules are in effect, who signed them, and what is wrong if not.
    Status,
    /// Show what changed between the last signed rules and the file now.
    Diff {
        /// Compare against a shipped posture (strict, balanced, open) instead.
        #[arg(long)]
        posture: Option<String>,
    },
    /// The fully resolved policy for a role, for installers and the VT UI.
    Effective {
        /// Role (preset) name; omit for the base table.
        #[arg(long)]
        role: Option<String>,
        /// Print JSON (stable, versioned). Without it, the same table as `show`.
        #[arg(long)]
        json: bool,
    },
    /// Fold .ta/constitution.toml and [actions] in workflow.toml into one rules file.
    Migrate {
        /// Write .ta/rules.toml (refuses to overwrite). Without it, only print the plan.
        #[arg(long)]
        write: bool,
    },
    /// Create the owner key (held outside the project) and a starting rules file, then sign it.
    Init {
        /// Posture to start from: strict, balanced or open.
        #[arg(long, default_value = "strict")]
        posture: String,
    },
    /// Validate .ta/rules.toml and sign it with the owner key.
    Sign,
    /// Trust an owner public key on this machine (a key made on another machine).
    Trust {
        /// Base64 public key printed by `ta rules status` on the machine that signed.
        public_key: String,
    },
    /// Remove the owner key and its trust anchor so a new one can be made.
    RotateKey {
        /// Confirm: after this the current rules stop verifying until re-signed.
        #[arg(long)]
        yes: bool,
    },
}

pub fn execute(cmd: &RulesCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let root = &config.workspace_root;
    let loader = RulesLoader::for_project(root);
    match cmd {
        RulesCommands::Show { role } => show(&loader, root, role.as_deref()),
        RulesCommands::Status => status(&loader),
        RulesCommands::Diff { posture } => diff(&loader, posture.as_deref()),
        RulesCommands::Effective { role, json } => effective(&loader, root, role.as_deref(), *json),
        RulesCommands::Migrate { write } => migrate_cmd(&loader, root, *write),
        RulesCommands::Init { posture } => init(&loader, root, posture),
        RulesCommands::Sign => sign(&loader, root),
        RulesCommands::Trust { public_key } => trust(root, public_key),
        RulesCommands::RotateKey { yes } => rotate(root, *yes),
    }
}

fn project_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into())
}

fn show(loader: &RulesLoader, root: &Path, role: Option<&str>) -> anyhow::Result<()> {
    let loaded = loader.load(role);
    print!("{}", report::render_show(&loaded, &project_name(root)));
    Ok(())
}

fn effective(
    loader: &RulesLoader,
    root: &Path,
    role: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let loaded = loader.load(role);
    if json {
        print!("{}", report::effective_json(&loaded, role));
    } else {
        print!("{}", report::render_show(&loaded, &project_name(root)));
    }
    Ok(())
}

fn status(loader: &RulesLoader) -> anyhow::Result<()> {
    let loaded = loader.load(None);
    match &loaded.status {
        Status::Default => println!(
            "Rules: no owner file ({OWNER_FILE}). The shipped {} posture applies. \
             Run `ta rules init` to make your own.",
            postures::DEFAULT
        ),
        Status::Verified { key_id } => println!("Rules: verified. Signed with key {key_id}."),
        Status::Invalid => {
            println!("Rules: NOT IN EFFECT. Nothing runs unattended until this is fixed.")
        }
    }
    for p in &loaded.problems {
        println!("  problem: {p}");
    }
    for w in &loaded.warnings {
        println!("  note: {w}");
    }
    if let Ok(Some(ps)) = ProjectState::locate(loader.project_root(), &StateLocator::from_env()) {
        match load::read_anchor(&ps) {
            Some(anchor) => println!("  trusted owner key: {anchor}"),
            None => println!(
                "  trusted owner key: none on this machine (`ta rules init` or `ta rules trust <key>`)"
            ),
        }
    }
    for (role, result) in loader.presets() {
        match result {
            Ok(()) => println!("  preset {role}: ok"),
            Err(errs) => println!("  preset {role}: INVALID ({})", errs.join("; ")),
        }
    }
    if matches!(loaded.status, Status::Invalid) {
        bail!("the rules are not in effect (see the problems above)");
    }
    Ok(())
}

fn diff(loader: &RulesLoader, posture: Option<&str>) -> anyhow::Result<()> {
    let now_text = std::fs::read_to_string(loader.owner_path()).ok();
    let current = match &now_text {
        Some(t) => loader.compile_text(t, None),
        None => loader.compile_text("version = 1\n", None),
    }
    .map_err(|e| {
        anyhow::anyhow!(
            "the current rules do not compile: {}. Fix them first.",
            e.join("; ")
        )
    })?;
    let (before, from) = match posture {
        Some(name) => {
            let p = postures::load(name).map_err(|e| anyhow::anyhow!(e))?;
            let c = loader
                .compile_text(&format!("version = 1\nposture = \"{}\"\n", p.name), Some(p))
                .map_err(|e| {
                    anyhow::anyhow!("posture {name} does not compile: {}", e.join("; "))
                })?;
            (c, format!("the {name} posture"))
        }
        None => match loader.last_signed() {
            Some((text, name, posture_text)) => {
                let mut p = postures::load(&name).map_err(|e| anyhow::anyhow!(e))?;
                if let Ok(doc) = ta_actions::rules::RulesDoc::parse(
                    &posture_text,
                    ta_actions::rules::DocKind::Posture,
                    "last signed posture",
                ) {
                    p.doc = doc;
                }
                let c = loader.compile_text(&text, Some(p)).map_err(|e| {
                    anyhow::anyhow!("the last signed rules do not compile: {}", e.join("; "))
                })?;
                (c, "the last signed rules".to_string())
            }
            None => {
                println!("No signed rules yet, so there is nothing to compare with. Showing the change from the shipped {} posture.", postures::DEFAULT);
                let c = loader
                    .compile_text("version = 1\n", None)
                    .map_err(|e| anyhow::anyhow!("{}", e.join("; ")))?;
                (c, format!("the {} posture", postures::DEFAULT))
            }
        },
    };
    let changes = report::diff(&before, &current);
    print!(
        "{}",
        report::render_diff(&changes, &from, "the rules file now")
    );
    if report::has_loosening(&changes) {
        println!(
            "Some cells are looser. `ta rules sign` makes them take effect; review them first."
        );
    }
    Ok(())
}

fn migrate_cmd(loader: &RulesLoader, root: &Path, write: bool) -> anyhow::Result<()> {
    let plan = migrate::plan(root).map_err(|e| anyhow::anyhow!(e))?;
    if plan.is_empty() {
        println!(
            "Nothing to migrate: no .ta/constitution.toml or .ta/workflow.toml found in {}.",
            root.display()
        );
        return Ok(());
    }
    for n in &plan.notes {
        println!("  {n}");
    }
    if !write {
        println!("\n--- {OWNER_FILE} would contain ---\n{}", plan.text);
        println!("Nothing was written. Run `ta rules migrate --write` to create the file, then `ta rules sign`.");
        return Ok(());
    }
    super::human_auth::require_human_for("rules")?;
    let path = loader.owner_path();
    if path.exists() {
        bail!(
            "{} already exists, so migration will not overwrite it. Move it aside or merge by hand, \
             then run `ta rules migrate --write` again.",
            path.display()
        );
    }
    std::fs::write(&path, &plan.text).with_context(|| {
        format!(
            "could not write {}. Check that .ta is writable.",
            path.display()
        )
    })?;
    println!(
        "Wrote {} ({} source file(s) folded). The old files are untouched. \
         Review it, then run `ta rules sign`.",
        path.display(),
        plan.folded.len()
    );
    Ok(())
}

fn open_state(root: &Path) -> anyhow::Result<(ProjectState, CustodyOpts)> {
    let locator = StateLocator::from_env();
    let ps = ProjectState::open(root, &locator).with_context(|| {
        format!(
            "could not open the TA state directory for {}. Run this from the project root",
            root.display()
        )
    })?;
    Ok((ps, CustodyOpts::from_env(&locator)))
}

fn init(loader: &RulesLoader, root: &Path, posture: &str) -> anyhow::Result<()> {
    super::human_auth::require_human_for("rules")?;
    postures::load(posture).map_err(|e| anyhow::anyhow!(e))?;
    let (ps, opts) = open_state(root)?;
    match load::load_owner_seed(&ps, opts).map_err(|e| anyhow::anyhow!(e))? {
        Some(_) => println!("Owner key: already exists, kept."),
        None => {
            let (public, custody) =
                load::create_owner_key(&ps, opts).map_err(|e| anyhow::anyhow!(e))?;
            println!("Owner key: created. Private half held in {custody}. Public key: {public}");
        }
    }
    let path = loader.owner_path();
    if !path.exists() {
        let text = format!(
            "# Owner rules for this project. Edit this file, then run `ta rules sign`.\n\
             version = 1\nposture = \"{posture}\"\n"
        );
        std::fs::write(&path, text).with_context(|| {
            format!(
                "could not write {}. Check that .ta is writable.",
                path.display()
            )
        })?;
        println!("Wrote {}.", path.display());
    }
    sign(loader, root)
}

fn sign(loader: &RulesLoader, root: &Path) -> anyhow::Result<()> {
    super::human_auth::require_human_for("rules")?;
    let path = loader.owner_path();
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "could not read {}. Run `ta rules init` or `ta rules migrate --write` first.",
            path.display()
        )
    })?;
    let (ps, opts) = open_state(root)?;
    let seed = load::load_owner_seed(&ps, opts)
        .map_err(|e| anyhow::anyhow!(e))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "there is no owner key on this machine, so the rules cannot be signed. \
                 Run `ta rules init` (new key) or sign on the machine that holds it."
            )
        })?;
    match load::read_anchor(&ps) {
        Some(anchor) if anchor != signing::public_to_b64(&signing::public_of(&seed)) => bail!(
            "the owner key here does not match the key this machine trusts ({anchor}). \
             Run `ta rules rotate-key --yes` then `ta rules init` to replace it deliberately."
        ),
        None => load::write_anchor(&ps, &signing::public_to_b64(&signing::public_of(&seed)))
            .map_err(|e| anyhow::anyhow!(e))?,
        _ => {}
    }
    let before = loader.last_signed().and_then(|(t, n, pt)| {
        let mut p = postures::load(&n).ok()?;
        p.doc = ta_actions::rules::RulesDoc::parse(
            &pt,
            ta_actions::rules::DocKind::Posture,
            "last signed",
        )
        .ok()?;
        loader.compile_text(&t, Some(p)).ok()
    });
    let key_id =
        load::sign_owner_file(loader, &seed, &text, chrono::Utc::now()).map_err(|errs| {
            anyhow::anyhow!("the rules were not signed:\n  - {}", errs.join("\n  - "))
        })?;
    println!("Signed {} with key {key_id}.", path.display());
    if let (Some(before), Ok(after)) = (before, loader.compile_text(&text, None)) {
        let changes = report::diff(&before, &after);
        if !changes.is_empty() {
            print!(
                "{}",
                report::render_diff(&changes, "the previous signed rules", "these rules")
            );
        }
    }
    Ok(())
}

fn trust(root: &Path, public_key: &str) -> anyhow::Result<()> {
    super::human_auth::require_human_for("rules")?;
    let (ps, _) = open_state(root)?;
    load::write_anchor(&ps, public_key).map_err(|e| anyhow::anyhow!(e))?;
    println!("Trusting owner key {public_key} on this machine.");
    Ok(())
}

fn rotate(root: &Path, yes: bool) -> anyhow::Result<()> {
    super::human_auth::require_human_for("rules")?;
    if !yes {
        bail!(
            "rotate-key removes the owner key and its trust anchor; the current rules stop verifying \
             until you run `ta rules init` and sign again. Re-run with --yes to confirm."
        );
    }
    let (ps, opts) = open_state(root)?;
    let removed = load::remove_owner_key(&ps, opts);
    println!(
        "Owner key {}. Run `ta rules init` to make a new one and sign again.",
        if removed {
            "removed"
        } else {
            "was not present (trust anchor cleared)"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::human_auth::test_support_set_gate;

    fn agent() -> Option<ta_credentials::human::AgentContext> {
        Some(ta_credentials::human::AgentContext {
            signals: vec!["CLAUDECODE is set".into()],
        })
    }

    fn project() -> (tempfile::TempDir, tempfile::TempDir, RulesLoader) {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".ta")).unwrap();
        let loader = RulesLoader::with_dirs(root.path(), Some(state.path().to_path_buf()), None);
        (root, state, loader)
    }

    #[test]
    fn an_agent_cannot_sign_init_trust_rotate_or_write_a_migration() {
        let (root, _state, loader) = project();
        std::fs::write(
            root.path().join(".ta/constitution.toml"),
            "[[rules.block]]\naction_type = \"email\"\ncondition = \"always\"\nmessage = \"no\"\n",
        )
        .unwrap();
        std::fs::write(loader.owner_path(), "version = 1\n").unwrap();
        test_support_set_gate(agent(), false);
        let r = root.path();
        for (what, err) in [
            ("sign", sign(&loader, r).unwrap_err()),
            ("init", init(&loader, r, "strict").unwrap_err()),
            ("trust", trust(r, "AAAA").unwrap_err()),
            ("rotate-key", rotate(r, true).unwrap_err()),
            (
                "migrate --write",
                migrate_cmd(&loader, r, true).unwrap_err(),
            ),
        ] {
            assert!(err.to_string().contains("ta login"), "{what}: {err}");
        }
        // Nothing was signed, written or replaced.
        assert!(!loader.sig_path().exists());
        assert_eq!(
            std::fs::read_to_string(loader.owner_path()).unwrap(),
            "version = 1\n"
        );
        test_support_set_gate(None, false);
    }

    #[test]
    fn read_only_commands_work_without_a_credential() {
        let (root, _state, loader) = project();
        test_support_set_gate(agent(), false);
        assert!(show(&loader, root.path(), None).is_ok());
        assert!(effective(&loader, root.path(), None, true).is_ok());
        assert!(diff(&loader, None).is_ok());
        assert!(diff(&loader, Some("balanced")).is_ok());
        assert!(diff(&loader, Some("nonsense")).is_err());
        assert!(migrate_cmd(&loader, root.path(), false).is_ok());
        test_support_set_gate(None, false);
    }

    #[test]
    fn migrate_write_creates_the_file_once_and_never_overwrites() {
        let (root, _state, loader) = project();
        test_support_set_gate(None, false);
        std::fs::write(
            root.path().join(".ta/workflow.toml"),
            "[actions.db_query]\npolicy = \"block\"\n",
        )
        .unwrap();
        migrate_cmd(&loader, root.path(), true).unwrap();
        let text = std::fs::read_to_string(loader.owner_path()).unwrap();
        assert!(
            text.contains("db_query") && text.contains("migrated_from"),
            "{text}"
        );
        assert!(!loader.sig_path().exists(), "migration never signs");
        let err = migrate_cmd(&loader, root.path(), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read_to_string(loader.owner_path()).unwrap(), text);
    }

    #[test]
    fn rotate_key_needs_an_explicit_yes() {
        let (root, _state, _loader) = project();
        test_support_set_gate(None, false);
        let err = rotate(root.path(), false).unwrap_err().to_string();
        assert!(err.contains("--yes"), "{err}");
    }
}
