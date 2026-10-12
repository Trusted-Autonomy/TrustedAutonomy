// rules/migrate.rs: fold the old files into one rules file (v0.17.12.4).
//
// Before this phase the rules about outside actions lived in three places:
// `.ta/constitution.toml` block and warn rules, `[actions.<verb>]` policies in
// `.ta/workflow.toml`, and hard-coded guards. `ta rules migrate` produces ONE
// owner rules file from the first two and leaves the originals untouched.
//
// What folds, and what does not:
//   * `[[rules.block]]` with condition `always`            -> `[[deny]] action = X`
//   * `[[rules.block]]` with `policy_is_not_review`        -> `[[deny]] ... when = "unattended"`
//   * `[actions.X] policy = "block"`                       -> `[[deny]] action = X`
//   * `[actions.X] policy = "auto"`                        -> `[[allow]] ... when = "unattended"`
//     (the owner chose autonomy for X before; migration keeps that choice and
//     says loudly that it covers every environment so they can narrow it)
//   * `email` with policy auto is NOT carried over: the old built-in guard
//     never let email run unattended and neither does the new table
//   * warn rules, database row-count and schema rules, rate limits and every
//     other `[actions]` setting STAY where they are: they are not allow/deny
//     questions and their own checks still run.
//
// The result is a plain file for a person to read. Nothing is signed here.

use std::path::Path;

use serde::Deserialize;

use super::model::{DocKind, RulesDoc};

#[derive(Debug, Clone, Default)]
pub struct MigrationPlan {
    /// The rules file text to write.
    pub text: String,
    /// One line per decision, shown to the person before anything is written.
    pub notes: Vec<String>,
    /// Old files whose rules were folded.
    pub folded: Vec<String>,
}

impl MigrationPlan {
    pub fn is_empty(&self) -> bool {
        self.folded.is_empty()
    }
}

#[derive(Deserialize, Default)]
struct OldConstitution {
    #[serde(default)]
    rules: OldRuleSets,
}

#[derive(Deserialize, Default)]
struct OldRuleSets {
    #[serde(default)]
    block: Vec<OldRule>,
    #[serde(default)]
    warn: Vec<OldRule>,
}

#[derive(Deserialize)]
struct OldRule {
    action_type: String,
    condition: String,
    #[serde(default)]
    message: String,
}

fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// Build the migration plan for a project. `Err` says what could not be read.
pub fn plan(project_root: &Path) -> Result<MigrationPlan, String> {
    let ta = project_root.join(".ta");
    let mut notes = Vec::new();
    let mut folded = Vec::new();
    let mut body = String::new();

    let constitution = ta.join("constitution.toml");
    if constitution.exists() {
        let text = std::fs::read_to_string(&constitution).map_err(|e| {
            format!(
                "could not read {}: {e}. Check its permissions.",
                constitution.display()
            )
        })?;
        let old: OldConstitution = toml::from_str(&text).map_err(|e| {
            format!(
                "{} does not parse: {e}. Fix the line named, then run `ta rules migrate` again. \
                 (Nothing was changed.)",
                constitution.display()
            )
        })?;
        let mut any = false;
        for r in &old.rules.block {
            let action = r.action_type.trim().to_ascii_lowercase();
            match r.condition.as_str() {
                "always" => {
                    body.push_str(&format!(
                        "\n[[deny]]\naction = {}\nreason = {}\n",
                        q(&action),
                        q(&r.message)
                    ));
                    notes.push(format!(
                        "constitution.toml: block {action} (always) -> [[deny]] {action}"
                    ));
                    any = true;
                }
                "policy_is_not_review" => {
                    body.push_str(&format!(
                        "\n[[deny]]\naction = {}\nwhen = \"unattended\"\nreason = {}\n",
                        q(&action),
                        q(&r.message)
                    ));
                    notes.push(format!(
                        "constitution.toml: block {action} unless reviewed -> [[deny]] {action} when unattended"
                    ));
                    any = true;
                }
                other => notes.push(format!(
                    "constitution.toml: left in place (not an allow/deny question): block {action} \
                     condition \"{other}\". Its own check still runs."
                )),
            }
        }
        for r in &old.rules.warn {
            notes.push(format!(
                "constitution.toml: left in place: warn {} ({}). Warnings do not change what runs.",
                r.action_type, r.condition
            ));
        }
        folded.push("constitution.toml".to_string());
        if !any {
            notes.push("constitution.toml: no block rule needed folding.".into());
        }
    }

    let workflow = ta.join("workflow.toml");
    if workflow.exists() {
        let text = std::fs::read_to_string(&workflow).map_err(|e| {
            format!(
                "could not read {}: {e}. Check its permissions.",
                workflow.display()
            )
        })?;
        let value: toml::Value = toml::from_str(&text).map_err(|e| {
            format!(
                "{} does not parse: {e}. Fix the line named, then run `ta rules migrate` again. \
                 (Nothing was changed.)",
                workflow.display()
            )
        })?;
        let mut any = false;
        if let Some(actions) = value.get("actions").and_then(|a| a.as_table()) {
            let mut verbs: Vec<_> = actions.iter().collect();
            verbs.sort_by(|a, b| a.0.cmp(b.0));
            for (verb, cfg) in verbs {
                let verb_l = verb.trim().to_ascii_lowercase();
                match cfg.get("policy").and_then(|p| p.as_str()) {
                    Some("block") => {
                        body.push_str(&format!(
                            "\n[[deny]]\naction = {}\nreason = {}\n",
                            q(&verb_l),
                            q(&format!("Migrated from workflow.toml [actions.{verb}] policy = \"block\"."))
                        ));
                        notes.push(format!("workflow.toml: [actions.{verb}] block -> [[deny]] {verb_l}"));
                        any = true;
                    }
                    Some("auto") if verb_l == "email" => notes.push(
                        "workflow.toml: [actions.email] policy = \"auto\" NOT carried over. Email never \
                         ran unattended and still does not."
                            .into(),
                    ),
                    Some("auto") => {
                        body.push_str(&format!(
                            "\n# You set [actions.{verb}] policy = \"auto\". This keeps that choice for every\n\
                             # environment, production included. Narrow it with environment = [\"dev\", \"test\"].\n\
                             [[allow]]\naction = {}\nwhen = \"unattended\"\n",
                            q(&verb_l)
                        ));
                        notes.push(format!(
                            "workflow.toml: [actions.{verb}] auto -> [[allow]] {verb_l} when unattended \
                             (EVERY environment, including production: narrow it)"
                        ));
                        any = true;
                    }
                    _ => {}
                }
            }
        }
        folded.push("workflow.toml".to_string());
        if !any {
            notes.push(
                "workflow.toml: no [actions] policy needed folding. Rate limits stay there.".into(),
            );
        }
    }

    let mut text = String::from(
        "# Owner rules for this project. Edit this file, then run `ta rules sign`.\n\
         # A file that is unsigned or changed since it was signed is refused, and nothing\n\
         # runs unattended until it is fixed. `ta rules show` prints what these rules do.\n\
         version = 1\nposture = \"strict\"\n",
    );
    if !folded.is_empty() {
        let list = folded.iter().map(|f| q(f)).collect::<Vec<_>>().join(", ");
        text.push_str(&format!("migrated_from = [{list}]\n"));
    }
    text.push_str(&body);

    // The file we are about to offer must itself be valid.
    RulesDoc::parse(&text, DocKind::Owner, "migrated rules").map_err(|e| {
        format!(
            "the migrated rules did not validate ({}). This is a bug in the migration; nothing \
             was written. Please report it, and edit .ta/rules.toml by hand meanwhile.",
            e.join("; ")
        )
    })?;
    Ok(MigrationPlan {
        text,
        notes,
        folded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::model::{Effect, When};

    fn project(constitution: Option<&str>, workflow: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        if let Some(c) = constitution {
            std::fs::write(d.path().join(".ta/constitution.toml"), c).unwrap();
        }
        if let Some(w) = workflow {
            std::fs::write(d.path().join(".ta/workflow.toml"), w).unwrap();
        }
        d
    }

    #[test]
    fn folds_blocks_autos_and_leaves_the_rest() {
        let d = project(
            Some(
                "[[rules.block]]\naction_type = \"social_post\"\ncondition = \"always\"\nmessage = \"No \\\"posting\\\".\"\n\
                 [[rules.block]]\naction_type = \"email\"\ncondition = \"policy_is_not_review\"\nmessage = \"Review.\"\n\
                 [[rules.block]]\naction_type = \"db_query\"\ncondition = \"schema_altering_statement\"\nmessage = \"x\"\n\
                 [[rules.warn]]\naction_type = \"db_query\"\ncondition = \"rows_modified_over_threshold\"\nmessage = \"w\"\n",
            ),
            Some(
                "[actions.webhook]\npolicy = \"auto\"\nmax_per_hour = 5\n\
                 [actions.slack]\npolicy = \"block\"\n\
                 [actions.email]\npolicy = \"auto\"\n\
                 [actions.notes]\npolicy = \"review\"\n[submit]\nadapter = \"git\"\n",
            ),
        );
        let plan = plan(d.path()).unwrap();
        let doc = RulesDoc::parse(&plan.text, DocKind::Owner, "owner rules").unwrap();
        assert_eq!(
            doc.migrated_from,
            vec!["constitution.toml", "workflow.toml"]
        );
        let find = |action: &str, effect: Effect| {
            doc.rules
                .iter()
                .find(|r| r.action == action && r.effect == effect)
        };
        assert!(find("social_post", Effect::Deny).is_some());
        assert_eq!(
            find("social_post", Effect::Deny).unwrap().reason.as_deref(),
            Some("No \"posting\".")
        );
        assert_eq!(
            find("email", Effect::Deny).unwrap().when,
            Some(When::Unattended)
        );
        assert!(find("slack", Effect::Deny).is_some());
        assert_eq!(
            find("webhook", Effect::Allow).unwrap().when,
            Some(When::Unattended)
        );
        assert!(
            find("email", Effect::Allow).is_none(),
            "email auto must not be carried over"
        );
        assert!(find("notes", Effect::Allow).is_none() && find("notes", Effect::Deny).is_none());
        let notes = plan.notes.join("\n");
        assert!(notes.contains("EVERY environment"), "{notes}");
        assert!(notes.contains("left in place"), "{notes}");
        assert!(notes.contains("NOT carried over"), "{notes}");
        // The originals are untouched.
        assert!(d.path().join(".ta/constitution.toml").exists());
    }

    #[test]
    fn nothing_to_fold_gives_an_empty_plan() {
        let d = project(None, None);
        let p = plan(d.path()).unwrap();
        assert!(p.is_empty());
        RulesDoc::parse(&p.text, DocKind::Owner, "x").unwrap();
    }

    #[test]
    fn unparsable_old_files_stop_the_migration_with_the_file_named() {
        let d = project(Some("[[rules.block"), None);
        let e = plan(d.path()).unwrap_err();
        assert!(
            e.contains("constitution.toml") && e.contains("Nothing was changed"),
            "{e}"
        );
        let d = project(None, Some("[actions.x\n"));
        assert!(plan(d.path()).unwrap_err().contains("workflow.toml"));
    }
}
