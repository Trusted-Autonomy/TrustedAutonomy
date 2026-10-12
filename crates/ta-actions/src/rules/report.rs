// rules/report.rs: printing the table (v0.17.12.4).
//
// `ta rules show` prints every cell with the source that decided it.
// `ta rules diff` prints the cells a change would loosen or tighten.
// `ta rules effective --role <name> --json` prints the resolved table for
// machines (the VT poller's ceiling check, Wayfinder's adoption screen).
//
// The JSON is built from typed structs, not `json!` maps, so key order is the
// declaration order and never depends on serde_json's `preserve_order` feature
// being unified in by another crate. A golden-file test pins the exact text.

use std::collections::BTreeMap;

use serde::Serialize;

use super::load::{Loaded, Status};
use super::model::{Effect, Environment, When};
use super::table::{CompiledRules, Request};

/// Version of the `effective --json` document.
pub const EFFECTIVE_SCHEMA_VERSION: u32 = 1;

/// One cell of the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cell {
    pub action: String,
    pub environment: Environment,
    pub when: When,
    pub effect: Effect,
    pub source: String,
}

/// A rule that names file paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyRow {
    pub effect: Effect,
    /// `null` means both.
    pub when: Option<When>,
    pub paths: Vec<String>,
    pub source: String,
}

/// Every cell for every action the table talks about, in a fixed order.
pub fn cells(c: &CompiledRules) -> Vec<Cell> {
    let mut out = Vec::new();
    for action in c.actions() {
        for env in Environment::ALL {
            for when in When::ALL {
                let d = c.decide_for_environment(&Request::new(&action, when), env);
                out.push(Cell {
                    action: action.clone(),
                    environment: env,
                    when,
                    effect: d.effect,
                    source: d.rule,
                });
            }
        }
    }
    out
}

pub fn apply_rows(c: &CompiledRules) -> Vec<ApplyRow> {
    c.apply_rules()
        .into_iter()
        .map(|r| ApplyRow {
            effect: r.effect,
            when: r.when,
            paths: r.paths.iter().map(|p| p.join("/")).collect(),
            source: r.label.clone(),
        })
        .collect()
}

#[derive(Serialize)]
struct DefaultEffects {
    unattended: Effect,
    attended: Effect,
}

#[derive(Serialize)]
struct ApplyDefault {
    unattended: CellSide,
    attended: CellSide,
}

#[derive(Serialize)]
struct CellSide {
    effect: Effect,
    source: String,
}

#[derive(Serialize)]
struct ApplyDoc {
    /// What an apply gets when no path rule names the changed file.
    default: ApplyDefault,
    rules: Vec<ApplyRow>,
}

#[derive(Serialize)]
struct EffectiveDoc {
    schema_version: u32,
    role: Option<String>,
    preset_found: bool,
    /// `true` when the rules verified and compiled.
    valid: bool,
    /// `default` (no owner file), `verified` or `invalid`.
    status: &'static str,
    key_id: Option<String>,
    /// When `valid` is false nothing runs unattended and no table follows.
    fail_closed: bool,
    posture: Option<String>,
    posture_revision: Option<u32>,
    org_floor: bool,
    /// Saga order for a draft's outside actions (`true` also when failing closed).
    ordered_actions: bool,
    problems: Vec<String>,
    warnings: Vec<String>,
    default: DefaultEffects,
    cells: Vec<Cell>,
    apply: Option<ApplyDoc>,
}

/// The resolved table as pretty JSON (trailing newline).
pub fn effective_json(loaded: &Loaded, role: Option<&str>) -> String {
    let (status, key_id) = match &loaded.status {
        Status::Default => ("default", None),
        Status::Verified { key_id } => ("verified", Some(key_id.clone())),
        Status::Invalid => ("invalid", None),
    };
    let doc = match &loaded.compiled {
        Some(c) => {
            let side = |when| {
                let d = c.decide(&Request {
                    action: "apply",
                    target: Default::default(),
                    when,
                    path: None,
                });
                CellSide {
                    effect: d.effect,
                    source: d.rule,
                }
            };
            EffectiveDoc {
                schema_version: EFFECTIVE_SCHEMA_VERSION,
                role: role.map(str::to_string),
                preset_found: c.preset_found,
                valid: true,
                status,
                key_id,
                fail_closed: false,
                posture: Some(c.posture.clone()),
                posture_revision: Some(c.posture_revision),
                org_floor: c.org_floor_present,
                ordered_actions: c.ordered_actions,
                problems: loaded.problems.clone(),
                warnings: loaded.warnings.clone(),
                default: DefaultEffects {
                    unattended: Effect::Deny,
                    attended: Effect::Ask,
                },
                cells: cells(c),
                apply: Some(ApplyDoc {
                    default: ApplyDefault {
                        unattended: side(When::Unattended),
                        attended: side(When::Attended),
                    },
                    rules: apply_rows(c),
                }),
            }
        }
        None => EffectiveDoc {
            schema_version: EFFECTIVE_SCHEMA_VERSION,
            role: role.map(str::to_string),
            preset_found: false,
            valid: false,
            status,
            key_id,
            fail_closed: true,
            posture: None,
            posture_revision: None,
            org_floor: false,
            ordered_actions: true,
            problems: loaded.problems.clone(),
            warnings: loaded.warnings.clone(),
            default: DefaultEffects {
                unattended: Effect::Deny,
                attended: Effect::Deny,
            },
            cells: Vec::new(),
            apply: None,
        },
    };
    let mut text = serde_json::to_string_pretty(&doc).expect("effective policy serializes");
    text.push('\n');
    text
}

fn group_label(envs: &[Environment]) -> String {
    if envs.len() == Environment::ALL.len() {
        "any environment".into()
    } else {
        envs.iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The table as text, one block per action, environments with identical
/// answers grouped. Every answer names its source.
pub fn render_show(loaded: &Loaded, project: &str) -> String {
    let mut out = String::new();
    let Some(c) = &loaded.compiled else {
        out.push_str(&format!(
            "Rules for {project}: NOT IN EFFECT (failing closed)\n"
        ));
        for p in &loaded.problems {
            out.push_str(&format!("  problem: {p}\n"));
        }
        out.push_str("  Nothing runs unattended and no outside action runs until this is fixed.\n");
        return out;
    };
    let owner = match &loaded.status {
        Status::Verified { key_id } => format!("owner file verified ({key_id})"),
        Status::Default => "no owner file, shipped posture only".to_string(),
        Status::Invalid => "invalid".to_string(),
    };
    out.push_str(&format!(
        "Rules for {project}: posture {} (revision {}), {owner}{}\n",
        c.posture,
        c.posture_revision,
        if c.org_floor_present {
            ", organization floor active"
        } else {
            ""
        }
    ));
    if let Some(role) = &c.role {
        out.push_str(&format!(
            "Role {role}: {}\n",
            if c.preset_found {
                "preset applied (tightens only)"
            } else {
                "no preset file, base rules"
            }
        ));
    }
    for w in &loaded.warnings {
        out.push_str(&format!("  warning: {w}\n"));
    }
    out.push_str(
        "Order: org floor, legacy denies, role preset, your rules, posture. Deny beats allow.\n",
    );
    out.push_str("Default: unattended deny; attended ask. Unknown targets are production.\n\n");

    let all = cells(c);
    let mut by_action: BTreeMap<&str, Vec<&Cell>> = BTreeMap::new();
    for cell in &all {
        by_action
            .entry(cell.action.as_str())
            .or_default()
            .push(cell);
    }
    for (action, rows) in by_action {
        out.push_str(&format!("{action}\n"));
        // Group environments whose (unattended, attended) answers are identical.
        let mut groups: Vec<(Vec<Environment>, &Cell, &Cell)> = Vec::new();
        for env in Environment::ALL {
            let un = rows
                .iter()
                .find(|r| r.environment == env && r.when == When::Unattended)
                .unwrap();
            let at = rows
                .iter()
                .find(|r| r.environment == env && r.when == When::Attended)
                .unwrap();
            match groups.iter_mut().find(|(_, u, a)| {
                u.effect == un.effect
                    && u.source == un.source
                    && a.effect == at.effect
                    && a.source == at.source
            }) {
                Some((envs, _, _)) => envs.push(env),
                None => groups.push((vec![env], un, at)),
            }
        }
        for (envs, un, at) in groups {
            out.push_str(&format!(
                "  {:<22} unattended: {:<5} [{}]\n  {:<22} attended:   {:<5} [{}]\n",
                group_label(&envs),
                un.effect.as_str(),
                un.source,
                "",
                at.effect.as_str(),
                at.source
            ));
        }
    }
    out.push_str("\napply (files)\n");
    for when in When::ALL {
        let d = c.decide(&Request {
            action: "apply",
            target: Default::default(),
            when,
            path: None,
        });
        out.push_str(&format!(
            "  any other file         {:<11} {:<5} [{}]\n",
            format!("{}:", when.as_str()),
            d.effect.as_str(),
            d.rule
        ));
    }
    for row in apply_rows(c) {
        out.push_str(&format!(
            "  {:<22} {:<11} {:<5} [{}]\n",
            row.paths.join(", "),
            format!("{}:", row.when.map(|w| w.as_str()).unwrap_or("both")),
            row.effect.as_str(),
            row.source
        ));
    }
    out.push_str(
        "  Never reachable by any rule: .ta, .git, .claude, .mcp.json, .github, .husky, .githooks,\n  \
         prompt files (CLAUDE.md, AGENTS.md ...), links, and non-ASCII names.\n",
    );
    out
}

// ── Diff ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Loosened,
    Tightened,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub what: String,
    pub before: String,
    pub after: String,
    pub kind: ChangeKind,
}

fn kind_of(before: Effect, after: Effect) -> ChangeKind {
    if after < before {
        ChangeKind::Loosened
    } else {
        ChangeKind::Tightened
    }
}

/// The cells and apply rules that differ between two tables. Source-only
/// changes (the same answer from a different rule) are not changes.
pub fn diff(before: &CompiledRules, after: &CompiledRules) -> Vec<Change> {
    let key = |c: &Cell| (c.action.clone(), c.environment, c.when);
    let mut b: BTreeMap<_, Effect> = BTreeMap::new();
    let mut a: BTreeMap<_, Effect> = BTreeMap::new();
    // Compare over the union of both action sets.
    let union: std::collections::BTreeSet<String> = before
        .actions()
        .into_iter()
        .chain(after.actions())
        .collect();
    let cells_over = |c: &CompiledRules| -> Vec<Cell> {
        let mut out = Vec::new();
        for action in &union {
            for env in Environment::ALL {
                for when in When::ALL {
                    let d = c.decide_for_environment(&Request::new(action, when), env);
                    out.push(Cell {
                        action: action.clone(),
                        environment: env,
                        when,
                        effect: d.effect,
                        source: d.rule,
                    });
                }
            }
        }
        out
    };
    for c in cells_over(before) {
        b.insert(key(&c), c.effect);
    }
    for c in cells_over(after) {
        a.insert(key(&c), c.effect);
    }
    let mut changes = Vec::new();
    for (k, eb) in &b {
        let ea = a[k];
        if *eb != ea {
            changes.push(Change {
                what: format!("{} in {} {}", k.0, k.1.as_str(), k.2.as_str()),
                before: eb.as_str().into(),
                after: ea.as_str().into(),
                kind: kind_of(*eb, ea),
            });
        }
    }
    // Apply rules, by (effect, when, paths).
    let rows = |c: &CompiledRules| -> std::collections::BTreeSet<(String, String, String)> {
        apply_rows(c)
            .into_iter()
            .map(|r| {
                (
                    r.effect.as_str().to_string(),
                    r.when.map(|w| w.as_str()).unwrap_or("both").to_string(),
                    r.paths.join(", "),
                )
            })
            .collect()
    };
    let (rb, ra) = (rows(before), rows(after));
    for (effect, when, paths) in ra.difference(&rb) {
        let kind = if effect == "allow" {
            ChangeKind::Loosened
        } else {
            ChangeKind::Tightened
        };
        changes.push(Change {
            what: format!("apply {paths} ({when})"),
            before: "no such rule".into(),
            after: effect.clone(),
            kind,
        });
    }
    for (effect, when, paths) in rb.difference(&ra) {
        let kind = if effect == "allow" {
            ChangeKind::Tightened
        } else {
            ChangeKind::Loosened
        };
        changes.push(Change {
            what: format!("apply {paths} ({when})"),
            before: effect.clone(),
            after: "no such rule".into(),
            kind,
        });
    }
    changes
}

pub fn has_loosening(changes: &[Change]) -> bool {
    changes.iter().any(|c| c.kind == ChangeKind::Loosened)
}

pub fn render_diff(changes: &[Change], from: &str, to: &str) -> String {
    if changes.is_empty() {
        return format!("No difference between {from} and {to}.\n");
    }
    let mut out = format!("Changes from {from} to {to}:\n");
    let mut sorted: Vec<&Change> = changes.iter().collect();
    sorted.sort_by_key(|c| (c.kind != ChangeKind::Loosened, c.what.clone()));
    for c in sorted {
        let mark = match c.kind {
            ChangeKind::Loosened => "LOOSER ",
            ChangeKind::Tightened => "tighter",
        };
        out.push_str(&format!(
            "  {mark}  {}: {} -> {}\n",
            c.what, c.before, c.after
        ));
    }
    let loose = changes
        .iter()
        .filter(|c| c.kind == ChangeKind::Loosened)
        .count();
    out.push_str(&format!(
        "{} change(s), {} loosen what is allowed.\n",
        changes.len(),
        loose
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::load::test_support::Fixture;

    fn loaded(owner: Option<&str>) -> Loaded {
        let f = Fixture::new();
        if let Some(o) = owner {
            f.write_owner(o);
            f.sign_owner();
        }
        f.loader().load(None)
    }

    #[test]
    fn every_cell_has_a_source() {
        let l = loaded(None);
        let c = l.compiled.as_ref().unwrap();
        let all = cells(c);
        assert_eq!(all.len(), c.actions().len() * 4 * 2);
        assert!(all.iter().all(|c| !c.source.is_empty()));
    }

    #[test]
    fn show_names_posture_rules_and_the_owner_rules() {
        let l = loaded(Some(
            "[[allow]]\naction = \"email\"\nwhen = \"unattended\"\n\
             [[allow]]\naction = \"apply\"\nwhen = \"unattended\"\npaths = [\"docs/research/**\"]\n",
        ));
        let text = render_show(&l, "demo");
        assert!(text.contains("posture strict"), "{text}");
        assert!(text.contains("owner rules [[allow]] #1"), "{text}");
        assert!(text.contains("posture strict [[deny]]"), "{text}");
        assert!(text.contains("docs/research"), "{text}");
        assert!(text.contains("any environment"), "{text}");
    }

    #[test]
    fn show_of_invalid_rules_lists_the_problems() {
        let f = Fixture::new();
        f.write_owner("[[allow]]\naction = \"email\"\n");
        let l = f.loader().load(None);
        let text = render_show(&l, "demo");
        assert!(
            text.contains("NOT IN EFFECT") && text.contains("ta rules sign"),
            "{text}"
        );
    }

    #[test]
    fn diff_reports_loosening_and_tightening() {
        let strict = loaded(None);
        let owner = loaded(Some(
            "[[allow]]\naction = \"email\"\nwhen = \"unattended\"\n",
        ));
        let changes = diff(
            strict.compiled.as_ref().unwrap(),
            owner.compiled.as_ref().unwrap(),
        );
        assert!(has_loosening(&changes));
        assert!(changes
            .iter()
            .any(|c| c.what == "email in production unattended"
                && c.before == "deny"
                && c.after == "allow"));
        let back = diff(
            owner.compiled.as_ref().unwrap(),
            strict.compiled.as_ref().unwrap(),
        );
        assert!(!has_loosening(&back));
        assert!(render_diff(&[], "a", "b").contains("No difference"));
        assert!(render_diff(&changes, "a", "b").contains("LOOSER"));
    }

    #[test]
    fn diff_sees_apply_path_rules() {
        let none = loaded(None);
        let some = loaded(Some(
            "[[allow]]\naction = \"apply\"\nwhen = \"unattended\"\npaths = [\"docs/**\"]\n",
        ));
        let changes = diff(
            none.compiled.as_ref().unwrap(),
            some.compiled.as_ref().unwrap(),
        );
        assert!(changes
            .iter()
            .any(|c| c.what.starts_with("apply docs") && c.kind == ChangeKind::Loosened));
    }

    // ── Golden files for `ta rules effective --json` ────────────────────────
    //
    // The JSON is a contract with the installer and the VT UI. Regenerate with
    // `TA_UPDATE_GOLDEN=1 cargo test -p ta-actions golden` and review the diff.

    fn golden(name: &str, actual: &str) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("golden")
            .join(name);
        if std::env::var_os("TA_UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, actual).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "golden file {} is missing ({e}); run with TA_UPDATE_GOLDEN=1 to create it",
                path.display()
            )
        });
        // A checkout with autocrlf must not fail the test.
        let want = want.replace("\r\n", "\n");
        assert_eq!(
            actual, want,
            "effective JSON drifted from {name}. If the change is intended, regenerate with \
             TA_UPDATE_GOLDEN=1 and review the diff."
        );
    }

    #[test]
    fn golden_effective_default_posture() {
        golden(
            "effective_default.json",
            &effective_json(&loaded(None), None),
        );
    }

    #[test]
    fn golden_effective_owner_rules() {
        let l = loaded(Some(
            "version = 1\nposture = \"balanced\"\n\
             [targets.scratch]\ntype = \"database\"\nenvironment = \"dev\"\nconnection = \"postgres://dev.example.com/scratch\"\n\
             [[allow]]\naction = \"apply\"\nwhen = \"unattended\"\npaths = [\"docs/research\"]\n\
             [[deny]]\naction = \"social_post\"\n",
        ));
        golden("effective_owner.json", &effective_json(&l, None));
    }

    #[test]
    fn golden_effective_role_preset() {
        let f = Fixture::new();
        f.write_owner("version = 1\nposture = \"open\"\n");
        f.sign_owner();
        let dir = f.root.path().join(".ta").join("rules-presets");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("research.toml"), "[[deny]]\naction = \"email\"\n").unwrap();
        let l = f.loader().load(Some("research"));
        golden(
            "effective_role_research.json",
            &effective_json(&l, Some("research")),
        );
    }

    #[test]
    fn effective_json_when_invalid_is_fail_closed_and_has_no_table() {
        let f = Fixture::new();
        f.write_owner("[[allow]]\naction = \"email\"\n");
        let v: serde_json::Value =
            serde_json::from_str(&effective_json(&f.loader().load(None), None)).unwrap();
        assert_eq!(v["valid"], false);
        assert_eq!(v["fail_closed"], true);
        assert_eq!(v["default"]["unattended"], "deny");
        assert_eq!(v["default"]["attended"], "deny");
        assert!(v["cells"].as_array().unwrap().is_empty());
        assert!(!v["problems"].as_array().unwrap().is_empty());
    }

    #[test]
    fn effective_json_is_stable_across_runs_and_line_endings() {
        let a = effective_json(&loaded(None), None);
        let b = effective_json(&loaded(None), None);
        assert_eq!(a, b);
        assert!(!a.contains('\r'));
        assert!(a.ends_with('\n'));
    }
}
