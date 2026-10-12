//! The rules gate for `ta draft apply` (v0.17.12.4).
//!
//! Every changed file and every queued outside action is put to
//! `CompiledRules::decide` before anything is written. There is no second rule
//! here: this module only collects the requests, audits the answers, refuses
//! with the rule's own sentence, and marks unattended content untrusted.
//!
//! Attendance comes from [`super::human_auth::proven_human_for`] and nowhere
//! else. A person confirming an `ask` is the person running apply, so an
//! attended `ask` goes ahead; an unattended `ask` was already turned into a
//! deny by the table.

use std::path::Path;

use anyhow::bail;
use ta_actions::rules::audit::{self, DecisionRecord};
use ta_actions::rules::untrusted::{self, AutoApplied};
use ta_actions::rules::{paths, Loaded, Request, RulesLoader, When};
use ta_changeset::draft_package::{ArtifactDisposition, DraftPackage};

/// Attendance for this apply: proven by the human gate, else unattended.
pub(crate) fn apply_attendance() -> When {
    if super::human_auth::proven_human_for("apply") {
        When::Attended
    } else {
        When::Unattended
    }
}

/// A file the draft will write, and the rule that allowed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Planned {
    pub path: String,
    pub rule: String,
}

fn rel_of(resource_uri: &str) -> Option<&str> {
    resource_uri.strip_prefix("fs://workspace/")
}

/// Decide every file and queued action. `Err` lists every refusal (not only the
/// first) so one run tells the person everything to fix. `Ok` carries the files
/// that were allowed *without a person*, to be marked untrusted after the write.
pub(crate) fn gate(
    loaded: &Loaded,
    pkg: &DraftPackage,
    audit_log: &Path,
    project_root: &Path,
    staging_root: &Path,
    when: When,
    action_verbs: &[String],
) -> anyhow::Result<Vec<Planned>> {
    let draft_id = pkg.package_id.to_string();
    let mut refusals: Vec<String> = Vec::new();
    let mut auto: Vec<Planned> = Vec::new();

    for a in &pkg.changes.artifacts {
        if matches!(
            a.disposition,
            ArtifactDisposition::Rejected | ArtifactDisposition::Discuss
        ) {
            continue;
        }
        let Some(rel) = rel_of(&a.resource_uri) else {
            refusals.push(format!(
                "'{}' is not a workspace file (expected fs://workspace/<path>); it cannot be applied. \
                 Rebuild the draft with `ta draft build --latest`.",
                a.resource_uri
            ));
            continue;
        };
        let decision = loaded.decide(&Request::new("apply", when).with_path(rel));
        if let Err(e) = audit::record(
            audit_log,
            &DecisionRecord {
                caller: "ta-draft-apply",
                action: "apply",
                when,
                decision: &decision,
                subject: Some(&draft_id),
                path: Some(rel),
            },
        ) {
            tracing::warn!(draft = %draft_id, path = rel, error = %e, "rules decision could not be audited");
        }
        if decision.is_deny() {
            refusals.push(format!("{rel}: {}", decision.refusal("apply", when)));
            continue;
        }
        if when == When::Unattended {
            // Links never ride along on an unattended apply: not in the
            // project (the write would follow them) and not in staging.
            for (label, base) in [("project", project_root), ("staging", staging_root)] {
                if let Err(link) = paths::check_no_links(base, rel) {
                    refusals.push(format!(
                        "{rel}: Blocked: unattended apply refuses links. {} ({label}) {}. A person can \
                         apply this draft with `ta draft apply` from their own terminal.",
                        link.path, link.why
                    ));
                }
            }
            auto.push(Planned {
                path: rel.to_string(),
                rule: decision.rule.clone(),
            });
        }
    }

    for verb in action_verbs {
        let decision = loaded.decide(&Request::new(verb, when));
        if let Err(e) = audit::record(
            audit_log,
            &DecisionRecord {
                caller: "ta-draft-apply",
                action: verb,
                when,
                decision: &decision,
                subject: Some(&draft_id),
                path: None,
            },
        ) {
            tracing::warn!(draft = %draft_id, action = %verb, error = %e, "rules decision could not be audited");
        }
        if decision.is_deny() {
            refusals.push(format!("{verb}: {}", decision.refusal(verb, when)));
        }
    }

    if !refusals.is_empty() {
        bail!(
            "ta draft apply stopped before changing anything: your rules refuse {} item(s) of draft {}.\n  - {}\n\
             Nothing was written and nothing was queued. `ta rules show` prints the table with the source of every cell.",
            refusals.len(),
            &draft_id[..8.min(draft_id.len())],
            refusals.join("\n  - ")
        );
    }
    Ok(auto)
}

/// Mark what an unattended apply wrote as untrusted (outside the tree).
pub(crate) fn mark_untrusted(
    rules_dir: &Path,
    draft_id: &str,
    project_root: &Path,
    planned: &[Planned],
) -> anyhow::Result<()> {
    if planned.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now();
    let entries: Vec<AutoApplied> = planned
        .iter()
        .map(|p| {
            AutoApplied::new(
                draft_id,
                &p.path,
                untrusted::sha256_of_file(&project_root.join(&p.path)),
                &p.rule,
                now,
            )
        })
        .collect();
    untrusted::record(rules_dir, &entries).map_err(|e| {
        anyhow::anyhow!(
            "{} auto-applied file(s) were written but could not be marked untrusted: {e}. \
             Check the state directory (`ta show state`) and treat these files as untrusted: {}",
            entries.len(),
            planned
                .iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Load the rules for a project (verifying the signature) for the gate.
pub(crate) fn load_for(project_root: &Path) -> Loaded {
    RulesLoader::for_project(project_root).load(None)
}

/// Verbs of the queued outside actions, for `gate`.
pub(crate) fn verbs_of(actions: &[ta_actions::NewAction]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| a.tool_name.strip_prefix(ta_actions::EXTERNAL_ACTION_PREFIX))
        .map(str::to_string)
        .collect()
}

/// Saga order for this project's outside actions. Fails closed: when the rules
/// are unusable the strict reading (ordered) applies.
pub(crate) fn ordered_actions(loaded: &Loaded) -> bool {
    loaded.compiled.as_ref().is_none_or(|c| c.ordered_actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_actions::rules::load::sign_owner_file;
    use ta_actions::rules::signing;

    struct Env {
        root: tempfile::TempDir,
        staging: tempfile::TempDir,
        state: tempfile::TempDir,
        seed: [u8; 32],
    }

    impl Env {
        fn new() -> Self {
            let e = Env {
                root: tempfile::tempdir().unwrap(),
                staging: tempfile::tempdir().unwrap(),
                state: tempfile::tempdir().unwrap(),
                seed: [9u8; 32],
            };
            std::fs::create_dir_all(e.root.path().join(".ta")).unwrap();
            std::fs::write(
                e.state.path().join("owner.pub"),
                signing::public_to_b64(&signing::public_of(&e.seed)),
            )
            .unwrap();
            e
        }
        fn loader(&self) -> RulesLoader {
            RulesLoader::with_dirs(
                self.root.path(),
                Some(self.state.path().to_path_buf()),
                None,
            )
        }
        fn rules(&self, text: &str, sign: bool) -> Loaded {
            std::fs::write(self.loader().owner_path(), text).unwrap();
            if sign {
                sign_owner_file(&self.loader(), &self.seed, text, chrono::Utc::now()).unwrap();
            }
            self.loader().load(None)
        }
        fn gate(
            &self,
            loaded: &Loaded,
            uris: &[&str],
            when: When,
            verbs: &[&str],
        ) -> anyhow::Result<Vec<Planned>> {
            let verbs: Vec<String> = verbs.iter().map(|v| v.to_string()).collect();
            gate(
                loaded,
                &package(uris),
                &self.root.path().join(".ta").join("audit.jsonl"),
                self.root.path(),
                self.staging.path(),
                when,
                &verbs,
            )
        }
    }

    fn package(uris: &[&str]) -> DraftPackage {
        let artifacts: Vec<_> = uris
            .iter()
            .map(|u| serde_json::json!({"resource_uri": u, "change_type": "modify", "diff_ref": "d"}))
            .collect();
        serde_json::from_value(serde_json::json!({
            "package_version": "1.0.0",
            "package_id": uuid::Uuid::new_v4(),
            "created_at": "2026-10-12T00:00:00Z",
            "goal": {"goal_id": "g", "title": "t", "objective": "o", "success_criteria": [],
                     "constraints": [], "parent_goal_title": null},
            "iteration": {"iteration_id": "i", "sequence": 1,
                          "workspace_ref": {"type": "staging_dir", "ref": "s", "base_ref": null}},
            "agent_identity": {"agent_id": "a", "agent_type": "t", "constitution_id": "c",
                               "capability_manifest_hash": "h", "orchestrator_run_id": null},
            "summary": {"what_changed": "w", "why": "y", "impact": "i", "rollback_plan": "r",
                        "open_questions": [], "alternatives_considered": []},
            "plan": {"completed_steps": [], "next_steps": [], "decision_log": []},
            "changes": {"artifacts": artifacts, "patch_sets": [], "pending_actions": []},
            "risk": {"risk_score": 0, "findings": [], "policy_decisions": []},
            "provenance": {"inputs": [], "tool_trace_hash": "t"},
            "review_requests": {"requested_actions": [], "reviewers": [], "required_approvals": 1,
                                "notes_to_reviewer": null},
            "signatures": {"package_hash": "p", "agent_signature": "s", "gateway_attestation": null},
            "status": {"status": "pending_review"}
        }))
        .expect("test package JSON must match DraftPackage")
    }

    const ALLOW_DOCS: &str =
        "version = 1\n[[allow]]\naction = \"apply\"\nwhen = \"unattended\"\npaths = [\"docs/research\"]\n";

    #[test]
    fn attended_apply_goes_ahead_for_ordinary_files() {
        let e = Env::new();
        let l = e.rules("version = 1\n", true);
        let planned = e
            .gate(
                &l,
                &["fs://workspace/src/main.rs", "fs://workspace/docs/a.md"],
                When::Attended,
                &[],
            )
            .unwrap();
        assert!(
            planned.is_empty(),
            "attended content is reviewed, so nothing is marked untrusted"
        );
    }

    #[test]
    fn protected_places_are_refused_even_for_a_person_in_every_spelling() {
        let e = Env::new();
        let l = e.rules("version = 1\n", true);
        for bad in [
            ".ta/rules.toml",
            ".ta/rules.toml.sig",
            ".TA\\rules.toml",
            ".git/hooks/pre-commit",
            "docs/../.ta/rules.toml",
            "../outside.txt",
            "/etc/passwd",
            ".ta./rules.toml",
        ] {
            let uri = format!("fs://workspace/{bad}");
            let err = e
                .gate(&l, &[&uri], When::Attended, &[])
                .unwrap_err()
                .to_string();
            assert!(err.contains("Nothing was written"), "{bad}: {err}");
            assert!(err.contains("ta rules show"), "{bad}: {err}");
        }
    }

    #[test]
    fn unattended_apply_is_denied_without_a_rule_and_allowed_only_inside_the_prefix() {
        let e = Env::new();
        let none = e.rules("version = 1\n", true);
        assert!(e
            .gate(
                &none,
                &["fs://workspace/docs/research/a.md"],
                When::Unattended,
                &[]
            )
            .is_err());

        let l = e.rules(ALLOW_DOCS, true);
        let planned = e
            .gate(
                &l,
                &[
                    "fs://workspace/docs/research/a.md",
                    "fs://workspace/Docs\\Research\\b.md",
                ],
                When::Unattended,
                &[],
            )
            .unwrap();
        assert_eq!(planned.len(), 2);
        assert!(planned[0].rule.contains("owner rules"), "{:?}", planned[0]);

        for bad in [
            "docs/other/a.md",
            "docs/research/../../src/main.rs",
            "docs/researchX/a.md",
            "docs/research/../../.ta/rules.toml",
            ".claude/settings.json",
            ".mcp.json",
            ".MCP.JSON",
            ".git/config",
            "CLAUDE.md",
            "docs/research/\u{4e2d}.md",
        ] {
            let uri = format!("fs://workspace/{bad}");
            assert!(
                e.gate(&l, &[&uri], When::Unattended, &[]).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn one_bad_file_refuses_the_whole_draft_and_names_every_problem() {
        let e = Env::new();
        let l = e.rules(ALLOW_DOCS, true);
        let err = e
            .gate(
                &l,
                &[
                    "fs://workspace/docs/research/ok.md",
                    "fs://workspace/src/lib.rs",
                    "fs://workspace/.claude/x",
                ],
                When::Unattended,
                &[],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 item(s)"), "{err}");
        assert!(
            err.contains("src/lib.rs") && err.contains(".claude/x"),
            "{err}"
        );
    }

    #[test]
    fn an_unsigned_or_edited_rules_file_gives_an_agent_nothing() {
        let e = Env::new();
        // The agent writes a permissive rules file into the project and cannot sign it.
        let unsigned = e.rules(ALLOW_DOCS, false);
        assert!(e
            .gate(
                &unsigned,
                &["fs://workspace/docs/research/a.md"],
                When::Unattended,
                &[]
            )
            .is_err());
        // Signed, then edited afterwards.
        let _ = e.rules("version = 1\n", true);
        let edited = e.rules(ALLOW_DOCS, false);
        let err = e
            .gate(
                &edited,
                &["fs://workspace/docs/research/a.md"],
                When::Unattended,
                &[],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("Blocked"), "{err}");
        // A person can still apply (and repair) with a warning in the rules, never into .ta.
        assert!(e
            .gate(&edited, &["fs://workspace/src/a.rs"], When::Attended, &[])
            .is_ok());
        assert!(e
            .gate(
                &edited,
                &["fs://workspace/.ta/rules.toml"],
                When::Attended,
                &[]
            )
            .is_err());
    }

    #[test]
    fn a_key_the_machine_does_not_trust_cannot_sign_for_the_owner() {
        let e = Env::new();
        std::fs::write(e.loader().owner_path(), ALLOW_DOCS).unwrap();
        sign_owner_file(&e.loader(), &[77u8; 32], ALLOW_DOCS, chrono::Utc::now()).unwrap();
        let l = e.loader().load(None);
        assert!(e
            .gate(
                &l,
                &["fs://workspace/docs/research/a.md"],
                When::Unattended,
                &[]
            )
            .is_err());
    }

    #[test]
    fn outside_actions_are_decided_by_the_same_table() {
        let e = Env::new();
        let l = e.rules("version = 1\n", true);
        let err = e
            .gate(&l, &[], When::Unattended, &["email"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("email"), "{err}");
        // A person present gets the strict posture's `ask`, which their apply confirms.
        assert!(e.gate(&l, &[], When::Attended, &["email"]).is_ok());
    }

    #[test]
    fn rejected_files_are_not_gated_because_they_are_not_applied() {
        let e = Env::new();
        let l = e.rules("version = 1\n", true);
        let mut pkg = package(&["fs://workspace/.ta/rules.toml"]);
        pkg.changes.artifacts[0].disposition = ArtifactDisposition::Rejected;
        let out = gate(
            &l,
            &pkg,
            &e.root.path().join(".ta").join("audit.jsonl"),
            e.root.path(),
            e.staging.path(),
            When::Attended,
            &[],
        );
        assert!(out.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn unattended_apply_refuses_links_in_project_and_staging() {
        let e = Env::new();
        let l = e.rules(ALLOW_DOCS, true);
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(e.root.path().join("docs")).unwrap();
        std::os::unix::fs::symlink(outside.path(), e.root.path().join("docs").join("research"))
            .unwrap();
        let err = e
            .gate(
                &l,
                &["fs://workspace/docs/research/a.md"],
                When::Unattended,
                &[],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("links"), "{err}");

        // A link inside the staging copy is refused as well.
        let e2 = Env::new();
        let l2 = e2.rules(ALLOW_DOCS, true);
        std::fs::create_dir_all(e2.staging.path().join("docs").join("research")).unwrap();
        std::os::unix::fs::symlink(
            outside.path(),
            e2.staging.path().join("docs").join("research").join("a.md"),
        )
        .unwrap();
        assert!(e2
            .gate(
                &l2,
                &["fs://workspace/docs/research/a.md"],
                When::Unattended,
                &[]
            )
            .is_err());
    }

    #[test]
    fn auto_applied_files_are_marked_untrusted_outside_the_tree() {
        let e = Env::new();
        std::fs::create_dir_all(e.root.path().join("docs/research")).unwrap();
        std::fs::write(e.root.path().join("docs/research/a.md"), "hello").unwrap();
        let planned = vec![Planned {
            path: "docs/research/a.md".into(),
            rule: "owner rules [[allow]] #1".into(),
        }];
        mark_untrusted(e.state.path(), "draft-1", e.root.path(), &planned).unwrap();
        assert!(untrusted::is_untrusted(
            e.state.path(),
            "docs/research/a.md"
        ));
        assert!(untrusted::is_untrusted(
            e.state.path(),
            "DOCS\\Research\\A.md"
        ));
        assert!(!untrusted::is_untrusted(e.state.path(), "docs/other.md"));
        // Nothing is written inside the project tree for the mark.
        assert!(!e.root.path().join("docs/research/a.md.untrusted").exists());
    }

    #[test]
    fn an_unwritable_mark_directory_is_reported_loudly() {
        let e = Env::new();
        let blocker = e.root.path().join("not-a-dir");
        std::fs::write(&blocker, "x").unwrap();
        let planned = vec![Planned {
            path: "a.md".into(),
            rule: "r".into(),
        }];
        let err = mark_untrusted(&blocker, "d", e.root.path(), &planned)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("could not be marked untrusted") && err.contains("a.md"),
            "{err}"
        );
    }

    #[test]
    fn ordered_actions_follow_the_posture_and_fail_closed() {
        let e = Env::new();
        assert!(ordered_actions(&e.rules("version = 1\n", true)));
        assert!(!ordered_actions(
            &e.rules("version = 1\nposture = \"open\"\n", true)
        ));
        // Unusable rules: the strict reading.
        assert!(ordered_actions(&e.rules(
            "version = 1\nposture = \"open\"\n# edited after signing\n",
            false
        )));
    }
}
