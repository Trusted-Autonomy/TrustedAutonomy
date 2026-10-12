//! CLI side of the atomic apply and the durable action queue (v0.17.12.3).
//!
//! * [`open_state`] / [`open_queue`] resolve the daemon-only per-user state
//!   directory (never the project tree).
//! * [`queueable_actions`] turns a draft's pending actions into queue entries,
//!   with the reversibility class each connector declared.
//! * [`recover_interrupted_applies`] finishes or undoes any apply that did not
//!   complete (also run by the daemon at startup).
//! * [`show_actions`] implements `ta show actions`: queue state, the one thing
//!   to do for anything that needs a person, and the recorded undo for
//!   compensable actions.
//!
//! Design: docs/superpowers/specs/2026-10-07-atomic-apply-and-action-queue-design.md.

use std::path::Path;

use anyhow::Context;
use ta_actions::{
    discover_adapter_actions, is_queueable_tool, recover_all, ActionQueue, ActionRegistry,
    AttentionReason, Clock, NewAction, PackageStatusEffects, QueueEntry, QueueState,
    RecoveryOutcome, Resolution, Reversibility, Snapshot, SystemClock, EXTERNAL_ACTION_PREFIX,
};
use ta_changeset::draft_package::{ArtifactDisposition, DraftPackage};
use ta_credentials::state::{ProjectState, StateLocator};
use ta_mcp_gateway::GatewayConfig;

/// The project's daemon-only state directory, created on first use. Refuses
/// from a staging workspace.
pub(crate) fn open_state(project_root: &Path) -> anyhow::Result<ProjectState> {
    ProjectState::open(project_root, &StateLocator::from_env()).with_context(|| {
        format!(
            "could not open the TA state directory for {}. Run this from the project root, \
             not from a staging workspace",
            project_root.display()
        )
    })
}

pub(crate) fn open_queue(project_root: &Path) -> anyhow::Result<ActionQueue> {
    let ps = open_state(project_root)?;
    ActionQueue::open(ps.action_queue_dir()).map_err(Into::into)
}

/// Built-in action stubs plus every discovered adapter plugin.
pub(crate) fn build_registry(project_root: &Path) -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    for action in discover_adapter_actions(project_root) {
        registry.register(action);
    }
    registry
}

/// The draft's outside actions that go into the queue. Excludes:
/// * `ta_propose_*` (replayed against Wayfinder by `draft_task_replay`),
/// * anything the reviewer rejected or marked for discussion,
/// * tools that are not `ta_external_action:<verb>` (no executor exists for them).
pub(crate) fn queueable_actions(pkg: &DraftPackage, registry: &ActionRegistry) -> Vec<NewAction> {
    let draft_id = pkg.package_id.to_string();
    pkg.changes
        .pending_actions
        .iter()
        .filter(|a| is_queueable_tool(&a.tool_name))
        .filter(|a| {
            !matches!(
                a.disposition,
                ArtifactDisposition::Rejected | ArtifactDisposition::Discuss
            )
        })
        .filter_map(|a| {
            let verb = a.tool_name.strip_prefix(EXTERNAL_ACTION_PREFIX)?;
            let decl = registry.declaration_for(verb);
            Some(NewAction {
                draft_id: draft_id.clone(),
                action_id: a.action_id.to_string(),
                tool_name: a.tool_name.clone(),
                parameters: a.parameters.clone(),
                description: a.description.clone(),
                class: decl.reversibility,
                undo: decl.undo,
                idempotency: decl.idempotency,
                undo_of: None,
            })
        })
        .collect()
}

/// After an apply queued actions: say plainly which ones cannot be undone.
pub(crate) fn print_irrecoverable_notice(config: &GatewayConfig, pkg: &DraftPackage) {
    let registry = build_registry(&config.workspace_root);
    let actions = queueable_actions(pkg, &registry);
    let count = |c: Reversibility| actions.iter().filter(|a| a.class == c).count();
    let (irrecoverable, compensable) = (
        count(Reversibility::Irrecoverable),
        count(Reversibility::Compensable),
    );
    if irrecoverable > 0 {
        eprintln!(
            "[apply]   {irrecoverable} cannot be undone once sent (irrecoverable). They run last, \
             only after the other actions succeed."
        );
    }
    if compensable > 0 {
        eprintln!(
            "[apply]   {compensable} can be countered after they run (compensable): \
             `ta show actions` offers the recorded undo."
        );
    }
}

/// Finishes or undoes every apply that never completed. Cheap when there is
/// nothing to do. Prints what it did; returns the number of journals handled.
pub(crate) fn recover_interrupted_applies(config: &GatewayConfig) -> usize {
    let ps = match open_state(&config.workspace_root) {
        Ok(ps) => ps,
        Err(e) => {
            tracing::debug!(error = %e, "skipping apply recovery: no state directory");
            return 0;
        }
    };
    let journals = ps.apply_journal_dir();
    if !journals.exists() {
        return 0;
    }
    let queue = match ActionQueue::open(ps.action_queue_dir()) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("[recover] {e}");
            return 0;
        }
    };
    let effects = PackageStatusEffects {
        pr_packages_dir: config.pr_packages_dir.clone(),
        lock_root: config.workspace_root.clone(),
        pid_alive: super::draft::is_apply_process_alive,
    };
    let reports = recover_all(&journals, &effects, &queue, &SystemClock);
    for r in &reports {
        match &r.outcome {
            RecoveryOutcome::RolledForward { enqueued } => eprintln!(
                "[recover] Draft {}: an earlier apply was interrupted after the draft was marked \
                 Applied. Finished it: {} action(s) queued.",
                r.draft_id, enqueued
            ),
            RecoveryOutcome::RolledBack {
                restored,
                commit_undone,
            } => eprintln!(
                "[recover] Draft {}: an earlier apply was interrupted before the draft was \
                 marked Applied. Rolled it back: {} file(s) restored{}. Run `ta draft apply` again.",
                r.draft_id,
                restored,
                if *commit_undone {
                    ", local commit undone"
                } else {
                    ""
                }
            ),
            RecoveryOutcome::Discarded | RecoveryOutcome::InProgress => {}
            RecoveryOutcome::Failed { reason } => eprintln!(
                "[recover] Draft {}: could not recover an interrupted apply: {reason}",
                r.draft_id
            ),
        }
    }
    reports.len()
}

// ── ta show actions ──────────────────────────────────────────────────────────

#[derive(Debug, Default, PartialEq, Eq)]
struct ShowArgs {
    draft: Option<String>,
    json: bool,
    undo: Option<String>,
    resolve: Option<String>,
    resolution: Option<Resolution>,
    note: String,
}

fn parse_args(id: Option<&str>, extra: &[String]) -> anyhow::Result<ShowArgs> {
    let mut a = ShowArgs {
        draft: id.filter(|s| !s.starts_with("--")).map(str::to_string),
        ..ShowArgs::default()
    };
    let mut items: Vec<String> = Vec::new();
    if let Some(flag) = id.filter(|s| s.starts_with("--")) {
        items.push(flag.to_string());
    }
    items.extend(extra.iter().cloned());
    let mut it = items.into_iter();
    let need = |it: &mut std::vec::IntoIter<String>, flag: &str| {
        it.next().ok_or_else(|| {
            anyhow::anyhow!(
                "{flag} needs a value. Example: ta show actions {flag} <draft-id>:<action-id>"
            )
        })
    };
    let set_resolution = |a: &mut ShowArgs, r: Resolution| -> anyhow::Result<()> {
        if a.resolution.replace(r).is_some() {
            anyhow::bail!("choose exactly one of --sent, --resend, --drop");
        }
        Ok(())
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => a.json = true,
            "--draft" => a.draft = Some(need(&mut it, "--draft")?),
            "--undo" => a.undo = Some(need(&mut it, "--undo")?),
            "--resolve" => a.resolve = Some(need(&mut it, "--resolve")?),
            "--note" => a.note = need(&mut it, "--note")?,
            "--sent" => set_resolution(&mut a, Resolution::SentConfirmed)?,
            "--resend" => set_resolution(&mut a, Resolution::Resend)?,
            "--drop" => set_resolution(&mut a, Resolution::Dropped)?,
            other => anyhow::bail!(
                "unknown option '{other}' for `ta show actions`. Options: --draft <id>, --json, \
                 --undo <key>, --resolve <key> (--sent | --resend | --drop) [--note <text>]"
            ),
        }
    }
    if a.resolve.is_some() && a.resolution.is_none() {
        anyhow::bail!(
            "--resolve needs a decision: --sent (it went out), --resend (it did not), or --drop \
             (do not send it)"
        );
    }
    if a.resolution.is_some() && a.resolve.is_none() {
        anyhow::bail!(
            "--sent/--resend/--drop need --resolve <key>. Run `ta show actions` to see keys"
        );
    }
    Ok(a)
}

pub(crate) fn show_actions(
    project_root: &Path,
    id: Option<&str>,
    extra: &[String],
) -> anyhow::Result<()> {
    let args = parse_args(id, extra)?;
    let queue = open_queue(project_root)?;
    let clock = SystemClock;

    if let Some(key) = &args.undo {
        super::human_auth::require_human_for("undo")?;
        let undo_key = queue.request_undo(key, &clock)?;
        println!("Queued the recorded undo of {key} as a new action: {undo_key}");
        println!("  The daemon sends it like any other action: policy is re-checked first and");
        println!("  the result is logged. Watch it with `ta show actions`.");
        return Ok(());
    }
    if let (Some(key), Some(res)) = (&args.resolve, args.resolution) {
        super::human_auth::require_human_for("resolve")?;
        queue.resolve(key, res, &args.note, &clock)?;
        let what = match res {
            Resolution::SentConfirmed => "marked as sent (it will not be sent again)",
            Resolution::Resend => "queued to be sent again",
            Resolution::Dropped => "dropped (it will not be sent)",
        };
        println!("{key}: {what}.");
        return Ok(());
    }

    let snap = queue.snapshot()?;
    if args.json {
        let entries: Vec<&QueueEntry> = snap
            .entries
            .iter()
            .filter(|e| matches_draft(e, args.draft.as_deref()))
            .collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    print!(
        "{}",
        render_actions(&snap, args.draft.as_deref(), clock.now())
    );
    Ok(())
}

fn matches_draft(e: &QueueEntry, draft: Option<&str>) -> bool {
    draft.is_none_or(|d| e.action.draft_id.starts_with(d) || e.key.starts_with(d))
}

/// Plain-text report for `ta show actions`. Pure so it can be tested.
pub(crate) fn render_actions(
    snap: &Snapshot,
    draft: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    use std::fmt::Write as _;
    let rows: Vec<&QueueEntry> = snap
        .entries
        .iter()
        .filter(|e| matches_draft(e, draft))
        .collect();
    let mut out = String::new();
    if rows.is_empty() {
        out.push_str("No outside actions are queued");
        if let Some(d) = draft {
            let _ = write!(out, " for draft {d}");
        }
        out.push_str(".\n  Actions appear here after `ta draft apply` queues them.\n");
        return out;
    }
    let count = |s: QueueState| rows.iter().filter(|e| e.state == s).count();
    let _ = writeln!(
        out,
        "Outside actions ({}): {} done, {} queued, {} sending, {} retrying, {} need attention",
        rows.len(),
        count(QueueState::Done),
        count(QueueState::Queued),
        count(QueueState::Sending),
        count(QueueState::Retrying),
        count(QueueState::NeedsAttention),
    );
    for e in &rows {
        let _ = writeln!(
            out,
            "\n{}  [{}]  {}",
            e.key,
            e.state.label(),
            e.action.class
        );
        if !e.action.description.is_empty() {
            let _ = writeln!(out, "  {}", e.action.description);
        }
        let _ = writeln!(
            out,
            "  tool: {}   attempts: {}",
            e.action.tool_name, e.attempts
        );
        match e.state {
            QueueState::Retrying => {
                if let Some(at) = e.next_attempt_at {
                    let wait = (at - now).num_seconds().max(0);
                    let _ = writeln!(
                        out,
                        "  next attempt in {wait}s (at {}); last error: {}",
                        at.format("%Y-%m-%d %H:%M:%SZ"),
                        e.last_error.as_deref().unwrap_or("none recorded")
                    );
                }
            }
            QueueState::Queued if e.action.class == Reversibility::Irrecoverable => {
                let waiting = rows
                    .iter()
                    .filter(|o| {
                        o.action.draft_id == e.action.draft_id
                            && o.key != e.key
                            && o.action.undo_of.is_none()
                            && o.action.class != Reversibility::Irrecoverable
                            && o.state != QueueState::Done
                    })
                    .count();
                if waiting > 0 {
                    let _ = writeln!(
                        out,
                        "  waits for {waiting} other action(s) of this draft to finish first \
                         (irrecoverable actions run last)"
                    );
                }
            }
            QueueState::NeedsAttention => {
                if let Some(att) = &e.attention {
                    let (what, next) = att.reason.explain();
                    let _ = writeln!(out, "  NEEDS ATTENTION: {what}");
                    let _ = writeln!(out, "  Detail: {}", att.message);
                    let _ = writeln!(out, "  To do: {}", next.replace("<key>", &e.key));
                    if att.reason == AttentionReason::UnknownOutcome {
                        let _ = writeln!(out, "  TA never resends an unknown outcome on its own.");
                    }
                }
            }
            QueueState::Done => {
                if let Some(u) = &e.undone_by {
                    let _ = writeln!(out, "  undone by {u}");
                } else if e.action.class == Reversibility::Compensable {
                    match &e.action.undo {
                        Some(verb) => {
                            let _ = writeln!(
                                out,
                                "  undo available ({verb}): ta show actions --undo {}",
                                e.key
                            );
                        }
                        None => {
                            let _ = writeln!(out, "  no recorded undo");
                        }
                    }
                } else if e.action.class == Reversibility::Irrecoverable
                    && e.action.undo_of.is_none()
                {
                    let _ = writeln!(out, "  cannot be undone");
                }
            }
            _ => {}
        }
    }
    if snap.skipped_lines > 0 {
        let _ = writeln!(
            out,
            "\nNote: {} unreadable journal line(s) were skipped (a crash mid-write or damage). \
             Nothing was lost that had finished writing.",
            snap.skipped_lines
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};
    use ta_actions::{
        ActionExecutor, ExecOutcome, ManualClock, PolicyVerdict, QueueWorker, RetryPolicy,
        SendPolicy,
    };
    use ta_changeset::draft_package::{ActionKind, PendingAction};
    use uuid::Uuid;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-12T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn pending(tool: &str, disp: ArtifactDisposition) -> PendingAction {
        PendingAction {
            action_id: Uuid::new_v4(),
            tool_name: tool.into(),
            parameters: serde_json::json!({"to": "a@example.com"}),
            kind: ActionKind::StateChanging,
            intercepted_at: t0(),
            description: format!("do {tool}"),
            target_uri: None,
            disposition: disp,
        }
    }

    fn package_with(actions: Vec<PendingAction>) -> DraftPackage {
        let mut pkg: DraftPackage = serde_json::from_value(serde_json::json!({
            "package_version": "1.0.0",
            "package_id": Uuid::new_v4(),
            "created_at": t0(),
            "goal": {"goal_id": "g", "title": "t", "objective": "o", "success_criteria": [],
                     "constraints": [], "parent_goal_title": null},
            "iteration": {"iteration_id": "i", "sequence": 1,
                          "workspace_ref": {"type": "staging_dir", "ref": "s", "base_ref": null}},
            "agent_identity": {"agent_id": "a", "agent_type": "t", "constitution_id": "c",
                               "capability_manifest_hash": "h", "orchestrator_run_id": null},
            "summary": {"what_changed": "w", "why": "y", "impact": "i", "rollback_plan": "r",
                        "open_questions": [], "alternatives_considered": []},
            "plan": {"completed_steps": [], "next_steps": [], "decision_log": []},
            "changes": {"artifacts": [], "patch_sets": [], "pending_actions": []},
            "risk": {"risk_score": 0, "findings": [], "policy_decisions": []},
            "provenance": {"inputs": [], "tool_trace_hash": "t"},
            "review_requests": {"requested_actions": [], "reviewers": [], "required_approvals": 1,
                                "notes_to_reviewer": null},
            "signatures": {"package_hash": "p", "agent_signature": "s", "gateway_attestation": null},
            "status": {"status": "pending_review"}
        }))
        .expect("test package JSON must match DraftPackage");
        pkg.changes.pending_actions = actions;
        pkg
    }

    #[test]
    fn queueable_excludes_propose_rejected_discuss_and_non_connector_tools() {
        let pkg = package_with(vec![
            pending("ta_external_action:email", ArtifactDisposition::Pending),
            pending(
                "ta_external_action:social_post",
                ArtifactDisposition::Approved,
            ),
            pending("ta_propose_task_create", ArtifactDisposition::Pending),
            pending("ta_external_action:email", ArtifactDisposition::Rejected),
            pending("ta_external_action:email", ArtifactDisposition::Discuss),
            pending("gmail_send", ArtifactDisposition::Pending),
        ]);
        let reg = ActionRegistry::new();
        let got = queueable_actions(&pkg, &reg);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|a| a.draft_id == pkg.package_id.to_string()));
        // Built-in stubs declare nothing, so every action is irrecoverable.
        assert!(got.iter().all(|a| a.class == Reversibility::Irrecoverable));
    }

    #[test]
    fn connector_declaration_sets_class_and_undo() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir
            .path()
            .join(".ta")
            .join("plugins")
            .join("adapter")
            .join("slack");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.toml"),
            "name = \"slack\"\ntype = \"adapter\"\ncommand = \"true\"\ncapabilities = [\"verb:slack.post\"]\n\
             reversibility = \"compensable\"\nundo = \"slack.delete\"\nidempotency = \"key\"\n",
        )
        .unwrap();
        let reg = build_registry(dir.path());
        let pkg = package_with(vec![pending(
            "ta_external_action:slack.post",
            ArtifactDisposition::Pending,
        )]);
        let got = queueable_actions(&pkg, &reg);
        assert_eq!(got[0].class, Reversibility::Compensable);
        assert_eq!(got[0].undo.as_deref(), Some("slack.delete"));
        assert_eq!(got[0].idempotency, ta_actions::Idempotency::Key);
    }

    struct AllowAll;
    impl SendPolicy for AllowAll {
        fn check(&self, _: &QueueEntry) -> PolicyVerdict {
            PolicyVerdict::Allow
        }
    }
    struct Ok_;
    impl ActionExecutor for Ok_ {
        fn execute(&self, _: &QueueEntry, _: &str) -> ExecOutcome {
            ExecOutcome::Done {
                result: serde_json::json!({}),
            }
        }
    }

    fn entry(draft: &str, id: &str, class: Reversibility, undo: Option<&str>) -> NewAction {
        NewAction {
            draft_id: draft.into(),
            action_id: id.into(),
            tool_name: "ta_external_action:x".into(),
            parameters: serde_json::json!({}),
            description: format!("action {id}"),
            class,
            undo: undo.map(str::to_string),
            idempotency: ta_actions::Idempotency::None,
            undo_of: None,
        }
    }

    #[test]
    fn show_actions_report_covers_every_state_and_offers_undo_only_for_compensable() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(
            &[
                entry(
                    "d1",
                    "post",
                    Reversibility::Compensable,
                    Some("slack.delete"),
                ),
                entry("d1", "mail", Reversibility::Irrecoverable, None),
                entry("d2", "flaky", Reversibility::Compensable, Some("x.undo")),
            ],
            &clock,
        )
        .unwrap();
        // d1:post succeeds; d2:flaky has an unknown outcome; d1:mail then runs.
        struct Mixed;
        impl ActionExecutor for Mixed {
            fn execute(&self, e: &QueueEntry, _: &str) -> ExecOutcome {
                if e.action.action_id == "flaky" {
                    ExecOutcome::Unknown {
                        message: "timed out".into(),
                    }
                } else {
                    ExecOutcome::Done {
                        result: serde_json::json!({}),
                    }
                }
            }
        }
        QueueWorker {
            queue: &q,
            clock: &clock,
            executor: &Mixed,
            policy: &AllowAll,
            retry: RetryPolicy::default(),
        }
        .run_once()
        .unwrap();
        let text = render_actions(&q.snapshot().unwrap(), None, clock.now());
        assert!(text.contains("3"), "{text}");
        assert!(text.contains("d1:post  [done]  compensable"), "{text}");
        assert!(
            text.contains("undo available (slack.delete): ta show actions --undo d1:post"),
            "{text}"
        );
        assert!(text.contains("d1:mail  [done]  irrecoverable"), "{text}");
        assert!(text.contains("cannot be undone"), "{text}");
        assert!(
            text.contains("NEEDS ATTENTION: TA cannot tell whether"),
            "{text}"
        );
        assert!(
            text.contains("ta show actions --resolve d2:flaky --sent"),
            "the next step must name the real key: {text}"
        );
        assert!(text.contains("never resends"), "{text}");
        // Filtering by draft.
        let only_d2 = render_actions(&q.snapshot().unwrap(), Some("d2"), clock.now());
        assert!(
            only_d2.contains("d2:flaky") && !only_d2.contains("d1:post"),
            "{only_d2}"
        );
        let none = render_actions(&q.snapshot().unwrap(), Some("zzz"), clock.now());
        assert!(
            none.contains("No outside actions are queued for draft zzz"),
            "{none}"
        );
    }

    #[test]
    fn retrying_and_held_actions_explain_themselves() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(
            &[
                entry("d1", "post", Reversibility::Compensable, Some("u")),
                entry("d1", "mail", Reversibility::Irrecoverable, None),
            ],
            &clock,
        )
        .unwrap();
        struct Retry;
        impl ActionExecutor for Retry {
            fn execute(&self, _: &QueueEntry, _: &str) -> ExecOutcome {
                ExecOutcome::Retryable {
                    message: "503".into(),
                }
            }
        }
        QueueWorker {
            queue: &q,
            clock: &clock,
            executor: &Retry,
            policy: &AllowAll,
            retry: RetryPolicy::default(),
        }
        .run_once()
        .unwrap();
        clock.advance(Duration::seconds(10));
        let text = render_actions(&q.snapshot().unwrap(), None, clock.now());
        assert!(text.contains("next attempt in 20s"), "{text}");
        assert!(text.contains("last error: 503"), "{text}");
        assert!(text.contains("waits for 1 other action(s)"), "{text}");
    }

    #[test]
    fn undo_and_resolve_through_the_queue_api() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(
            &[entry(
                "d1",
                "post",
                Reversibility::Compensable,
                Some("slack.delete"),
            )],
            &clock,
        )
        .unwrap();
        QueueWorker {
            queue: &q,
            clock: &clock,
            executor: &Ok_,
            policy: &AllowAll,
            retry: RetryPolicy::default(),
        }
        .run_once()
        .unwrap();
        let k = q.request_undo("d1:post", &clock).unwrap();
        let text = render_actions(&q.snapshot().unwrap(), None, clock.now());
        assert!(text.contains(&format!("undone by {k}")), "{text}");
    }

    #[test]
    fn argument_parsing_is_strict_and_actionable() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let a = parse_args(Some("abc"), &s(&["--json"])).unwrap();
        assert_eq!(a.draft.as_deref(), Some("abc"));
        assert!(a.json);
        let a = parse_args(None, &s(&["--resolve", "d:a", "--resend", "--note", "n"])).unwrap();
        assert_eq!(a.resolution, Some(Resolution::Resend));
        assert_eq!(a.note, "n");
        let a = parse_args(Some("--undo"), &s(&["d:a"])).unwrap();
        assert_eq!(a.undo.as_deref(), Some("d:a"));
        assert!(parse_args(None, &s(&["--resolve", "d:a"]))
            .unwrap_err()
            .to_string()
            .contains("needs a decision"));
        assert!(parse_args(None, &s(&["--sent"]))
            .unwrap_err()
            .to_string()
            .contains("--resolve <key>"));
        assert!(
            parse_args(None, &s(&["--resolve", "k", "--sent", "--drop"]))
                .unwrap_err()
                .to_string()
                .contains("exactly one")
        );
        assert!(parse_args(None, &s(&["--bogus"]))
            .unwrap_err()
            .to_string()
            .contains("Options:"));
        assert!(parse_args(None, &s(&["--undo"]))
            .unwrap_err()
            .to_string()
            .contains("needs a value"));
    }

    #[test]
    fn draft_status_json_shape_matches_what_recovery_reads() {
        use ta_changeset::draft_package::{ApplyProvenance, DraftStatus};
        let mut pkg = package_with(vec![]);
        pkg.status = DraftStatus::Applied {
            applied_at: t0(),
            applied_via: ApplyProvenance::Manual,
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(format!("{}.json", pkg.package_id)),
            serde_json::to_string_pretty(&pkg).unwrap(),
        )
        .unwrap();
        let fx = PackageStatusEffects {
            pr_packages_dir: dir.path().to_path_buf(),
            lock_root: dir.path().to_path_buf(),
            pid_alive: |_| false,
        };
        use ta_actions::ApplyEffects;
        assert!(fx.is_applied(&pkg.package_id.to_string()).unwrap());
        pkg.status = DraftStatus::PendingReview;
        std::fs::write(
            dir.path().join(format!("{}.json", pkg.package_id)),
            serde_json::to_string_pretty(&pkg).unwrap(),
        )
        .unwrap();
        assert!(!fx.is_applied(&pkg.package_id.to_string()).unwrap());
    }

    #[test]
    fn queue_lives_in_the_state_dir_not_the_project_tree() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let loc = StateLocator::at(state.path());
        let ps = ProjectState::open(project.path(), &loc).unwrap();
        let q = ActionQueue::open(ps.action_queue_dir()).unwrap();
        assert!(q.dir().starts_with(state.path()));
        assert!(!q.dir().starts_with(project.path()));
        // And not reachable from a staging workspace.
        let staging = project.path().join(".ta").join("staging").join("g1");
        std::fs::create_dir_all(&staging).unwrap();
        assert!(ProjectState::open(&staging, &loc).is_err());
    }
}
