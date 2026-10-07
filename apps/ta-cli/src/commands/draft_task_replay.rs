//! Apply-time replay of the draft-bundled Wayfinder task proposals
//! (`ta_propose_task_*`, see `ta_mcp_gateway::tools::wayfinder_task`).
//!
//! Per the CoS read-only chat-mode design
//! (`docs/superpowers/specs/2026-10-06-cos-read-only-chat-mode-design.md`,
//! "Task mutation" and "Dispatched" item 4): no agent ever mutates a
//! Wayfinder task live. A worker's task proposals are captured as
//! `PendingAction`s in its own draft, reviewed together with the rest of the
//! draft, and only replayed here, after `ta draft apply` has actually
//! applied that draft.
//!
//! Safety properties:
//! - Only a draft whose status is `Applied` is replayed (the caller also
//!   skips dry runs). Actions the reviewer marked `Rejected` or `Discuss`
//!   are skipped.
//! - Idempotent: each successfully replayed `action_id` is appended to a
//!   local ledger (`.ta/wayfinder-task-replay.log`) and skipped on any later
//!   replay. Independently of the ledger, creates always carry an
//!   `external_id` (Wayfinder upserts on it), status/assignee updates are
//!   naturally idempotent, and a duplicate dependency edge (409) is treated
//!   as success, so even a lost ledger cannot double-create a task.
//! - One action failing never blocks the others; every failure is logged
//!   with structured fields (`action_id`, `tool_name`, `task_id`, `error`)
//!   and printed with a next step.
//! - A project with no `[plan] backend = "wayfinder"` is skipped with a log
//!   line, never an error: the rest of the draft has already applied.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;

use ta_changeset::draft_package::{ArtifactDisposition, DraftPackage, DraftStatus, PendingAction};
use ta_mcp_gateway::tools::wayfinder_task::ProposeKind;
use ta_mcp_gateway::GatewayConfig;
use ta_plan_wayfinder::{
    CreateTaskRequest, WayfinderClient, STATUS_DONE, STATUS_ON_HOLD, STATUS_OPEN,
};

/// Ledger of replayed `action_id`s, one UUID per line, under `.ta/`.
pub(crate) const LEDGER_FILE: &str = "wayfinder-task-replay.log";

/// Outcome counts of one replay pass, printed and returned for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ReplaySummary {
    pub replayed: usize,
    pub already_replayed: usize,
    pub failed: usize,
    pub skipped_by_reviewer: usize,
    /// Set when no Wayfinder backend/config/client was available, so nothing
    /// was attempted.
    pub backend_unavailable: bool,
}

/// The hook `apply_package` calls after a successful, non-dry-run apply.
pub(crate) fn replay_task_proposals(config: &GatewayConfig, pkg: &DraftPackage) {
    if !is_replayable(&pkg.status) {
        tracing::debug!(
            draft_id = %pkg.package_id,
            "not replaying ta_propose_task_* actions: draft is not in Applied status"
        );
        return;
    }
    replay_actions(config, &pkg.changes.pending_actions);
}

/// Only an actually-applied draft's proposals may reach Wayfinder.
pub(crate) fn is_replayable(status: &DraftStatus) -> bool {
    matches!(status, DraftStatus::Applied { .. })
}

/// Replays every `ta_propose_task_*` action in `actions`. Actions of other
/// tools are ignored (they are not this module's to replay).
pub(crate) fn replay_actions(config: &GatewayConfig, actions: &[PendingAction]) -> ReplaySummary {
    let mut summary = ReplaySummary::default();
    let mut ours: Vec<(ProposeKind, &PendingAction)> = Vec::new();
    for action in actions {
        let Some(kind) = ProposeKind::from_tool_name(&action.tool_name) else {
            continue;
        };
        if matches!(
            action.disposition,
            ArtifactDisposition::Rejected | ArtifactDisposition::Discuss
        ) {
            tracing::info!(
                action_id = %action.action_id,
                tool_name = %action.tool_name,
                disposition = %action.disposition,
                "not replaying task proposal: reviewer did not approve this action"
            );
            summary.skipped_by_reviewer += 1;
            continue;
        }
        ours.push((kind, action));
    }
    if ours.is_empty() {
        return summary;
    }

    let Some(client) = load_client(config, ours.len()) else {
        summary.backend_unavailable = true;
        return summary;
    };

    let ledger_path = config.workspace_root.join(".ta").join(LEDGER_FILE);
    let mut done = read_ledger(&ledger_path);

    for (kind, action) in ours {
        let task_id = action
            .parameters
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if done.contains(&action.action_id.to_string()) {
            tracing::info!(
                action_id = %action.action_id,
                tool_name = %action.tool_name,
                task_id = %task_id,
                "skipping task proposal: already replayed against Wayfinder (ledger hit)"
            );
            summary.already_replayed += 1;
            continue;
        }
        match replay_one(&client, kind, &action.parameters) {
            Ok(what) => {
                println!("  [applied] Wayfinder: {what}");
                summary.replayed += 1;
                if let Err(e) = append_ledger(&ledger_path, &action.action_id.to_string()) {
                    tracing::warn!(
                        action_id = %action.action_id,
                        tool_name = %action.tool_name,
                        path = %ledger_path.display(),
                        error = %e,
                        "replayed task proposal but could not record it in the replay ledger; \
                         a later re-apply may repeat it (safe: creates upsert by external_id)"
                    );
                }
                done.insert(action.action_id.to_string());
            }
            Err(e) => {
                summary.failed += 1;
                tracing::warn!(
                    action_id = %action.action_id,
                    tool_name = %action.tool_name,
                    task_id = %task_id,
                    error = %e,
                    "failed to replay approved task proposal against Wayfinder"
                );
                eprintln!(
                    "  [failed] Wayfinder {} for task {} (action {}): {e}\n           \
                     The rest of the draft is applied. Fix the cause above, then re-run \
                     `ta draft apply` for this draft, or make the change in Wayfinder by hand.",
                    action.tool_name,
                    if task_id.is_empty() { "<new>" } else { task_id },
                    action.action_id,
                );
            }
        }
    }

    if summary.replayed + summary.failed > 0 {
        println!(
            "  Wayfinder task proposals: {} applied, {} failed, {} already applied earlier",
            summary.replayed, summary.failed, summary.already_replayed
        );
    }
    summary
}

/// Loads the project's Wayfinder client, or logs why it can't and returns
/// `None`.
fn load_client(config: &GatewayConfig, count: usize) -> Option<WayfinderClient> {
    let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);
    if workflow_config.plan.backend != "wayfinder" {
        tracing::warn!(
            count,
            path = %workflow_toml.display(),
            "skipping ta_propose_task_* pending action(s): this project has no \
             [plan] backend = \"wayfinder\" configured, so there is no Wayfinder task to update"
        );
        return None;
    }
    let Some(raw) = workflow_config.plan.wayfinder.as_ref() else {
        tracing::warn!(
            count,
            path = %workflow_toml.display(),
            "skipping ta_propose_task_* pending action(s): [plan] backend = \"wayfinder\" \
             but no [plan.wayfinder] table was found; add it, then re-run `ta draft apply`"
        );
        return None;
    };
    let mut cred_config = ta_credentials::CredentialsConfig::for_project(&config.workspace_root);
    // Respect this gateway's own keychain setting (see `tools/wiki.rs`).
    cred_config.use_keychain = config.credential_vault_use_keychain;
    let wf_config = match ta_plan_wayfinder::WayfinderPlanConfig::load_with_credentials_config(
        raw,
        &cred_config,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(count, error = %e, "skipping ta_propose_task_* pending action(s): failed to load Wayfinder config");
            eprintln!(
                "  [skipped] {count} Wayfinder task proposal(s): could not load Wayfinder config: {e}"
            );
            return None;
        }
    };
    match WayfinderClient::new(&wf_config) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(count, error = %e, "skipping ta_propose_task_* pending action(s): failed to build Wayfinder client");
            None
        }
    }
}

fn str_param<'a>(params: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(|v| v.as_str())
}

fn required<'a>(params: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    str_param(params, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("pending action is missing required parameter `{key}`"))
}

/// The per-kind dispatcher: one match over [`ProposeKind`], each arm the
/// exact Wayfinder call(s) for that outcome. Returns a one-line summary of
/// what changed.
fn replay_one(
    client: &WayfinderClient,
    kind: ProposeKind,
    p: &serde_json::Value,
) -> anyhow::Result<String> {
    match kind {
        ProposeKind::Update => {
            let task_id = required(p, "task_id")?;
            client.update_task_content(
                task_id,
                str_param(p, "title"),
                str_param(p, "description"),
            )?;
            Ok(format!("task {task_id} content updated"))
        }
        ProposeKind::Create => {
            let external_id = required(p, "external_id")?;
            let task = client.upsert_task(&CreateTaskRequest {
                title: required(p, "title")?.to_string(),
                description: str_param(p, "description").map(str::to_string),
                verb: required(p, "verb")?.to_string(),
                external_id: Some(external_id.to_string()),
                assignee_id: str_param(p, "assignee_id").map(str::to_string),
            })?;
            Ok(format!(
                "task {} created/upserted (external_id {external_id})",
                task.id
            ))
        }
        ProposeKind::Reassign => {
            let task_id = required(p, "task_id")?;
            let assignee = str_param(p, "assignee_id");
            client.update_task_assignee(task_id, assignee)?;
            Ok(match assignee {
                Some(a) => format!("task {task_id} reassigned to {a}"),
                None => format!("task {task_id} assignee cleared"),
            })
        }
        ProposeKind::NeedsRevision => {
            let task_id = required(p, "task_id")?;
            client.update_task_status(task_id, STATUS_OPEN, None)?;
            Ok(format!("task {task_id} marked needs-revision (open)"))
        }
        ProposeKind::OnHold => {
            let task_id = required(p, "task_id")?;
            let reason = required(p, "hold_reason")?;
            let mut what = format!("task {task_id} put on hold");
            if let Some(b) = p.get("blocking_task").filter(|b| !b.is_null()) {
                let precursor = client.upsert_task(&CreateTaskRequest {
                    title: required(b, "title")?.to_string(),
                    description: str_param(b, "description").map(str::to_string),
                    verb: required(b, "verb")?.to_string(),
                    external_id: Some(required(b, "external_id")?.to_string()),
                    assignee_id: None,
                })?;
                client.add_dependency(task_id, &precursor.id).map_err(|e| {
                    anyhow::anyhow!(
                        "created precursor task {} but could not record that task {task_id} \
                             depends on it (Wayfinder rejects dependency cycles): {e}",
                        precursor.id
                    )
                })?;
                what.push_str(&format!(", blocked by precursor task {}", precursor.id));
            }
            client.update_task_status(task_id, STATUS_ON_HOLD, Some(reason))?;
            Ok(what)
        }
        ProposeKind::Complete => {
            let task_id = required(p, "task_id")?;
            client.update_task_status(task_id, STATUS_DONE, None)?;
            Ok(format!("task {task_id} marked done"))
        }
    }
}

fn read_ledger(path: &Path) -> HashSet<String> {
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn append_ledger(path: &Path, action_id: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{action_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ta_changeset::draft_package::{ActionKind, ApplyProvenance};
    use uuid::Uuid;

    /// A `wiremock::MockServer` usable from synchronous tests:
    /// `WayfinderClient` is `reqwest::blocking`, which panics inside an
    /// existing tokio runtime, so the mock server gets its own runtime.
    struct BlockingMockServer {
        runtime: tokio::runtime::Runtime,
        server: wiremock::MockServer,
    }

    impl BlockingMockServer {
        fn start() -> Self {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let server = runtime.block_on(wiremock::MockServer::start());
            Self { runtime, server }
        }

        /// Answers every request with a TaskDto whose id is `task_id`.
        fn respond_all_with_task(&self, task_id: &str) {
            self.runtime.block_on(
                wiremock::Mock::given(wiremock::matchers::any())
                    .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                        serde_json::json!({
                            "id": task_id,
                            "status": "open",
                            "hold_reason": null,
                            "external_id": null,
                            "updated_at": "1000000000"
                        }),
                    ))
                    .mount(&self.server),
            );
        }

        /// (method, path, body) of every request received, in order.
        fn requests(&self) -> Vec<(String, String, serde_json::Value)> {
            self.runtime
                .block_on(self.server.received_requests())
                .unwrap()
                .into_iter()
                .map(|r| {
                    let body = if r.body.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::from_slice(&r.body).unwrap()
                    };
                    (r.method.to_string(), r.url.path().to_string(), body)
                })
                .collect()
        }
    }

    fn project() -> (tempfile::TempDir, GatewayConfig) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = GatewayConfig::for_project(dir.path());
        config.credential_vault_use_keychain = false;
        (dir, config)
    }

    fn write_wayfinder_plan_config(config: &GatewayConfig, base_url: &str) {
        std::fs::create_dir_all(config.workspace_root.join(".ta")).unwrap();
        std::fs::write(
            config.workspace_root.join(".ta/workflow.toml"),
            format!(
                "[plan]\nbackend = \"wayfinder\"\n\n[plan.wayfinder]\n\
                 base_url = \"{base_url}\"\norg_id = \"org-1\"\nproject_id = \"proj-1\"\n\
                 credential_name = \"wayfinder-service-account\"\n"
            ),
        )
        .unwrap();
        let mut cred_config =
            ta_credentials::CredentialsConfig::for_project(&config.workspace_root);
        cred_config.use_keychain = false;
        let mut vault = ta_credentials::FileVault::open(&cred_config).unwrap();
        use ta_credentials::CredentialVault;
        vault
            .add(
                "wayfinder-service-account",
                "wayfinder",
                "wfsa-test-secret",
                vec![],
            )
            .unwrap();
    }

    fn action(kind: ProposeKind, parameters: serde_json::Value) -> PendingAction {
        PendingAction {
            action_id: Uuid::new_v4(),
            tool_name: kind.tool_name().to_string(),
            parameters,
            kind: ActionKind::StateChanging,
            intercepted_at: Utc::now(),
            description: "test proposal".to_string(),
            target_uri: None,
            disposition: ArtifactDisposition::Pending,
        }
    }

    /// Runs `actions` against a fresh mock and returns what it received.
    fn replay_against_mock(
        response_task_id: &str,
        actions: &[PendingAction],
    ) -> (ReplaySummary, Vec<(String, String, serde_json::Value)>) {
        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        mock.respond_all_with_task(response_task_id);
        write_wayfinder_plan_config(&config, &mock.server.uri());
        let summary = replay_actions(&config, actions);
        (summary, mock.requests())
    }

    fn req(
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> (String, String, serde_json::Value) {
        (method.to_string(), path.to_string(), body)
    }

    #[test]
    fn update_replays_as_patch_content() {
        let (s, reqs) = replay_against_mock(
            "wf-1",
            &[action(
                ProposeKind::Update,
                serde_json::json!({"task_id": "wf-1", "title": "Revised", "description": null}),
            )],
        );
        assert_eq!(s.replayed, 1);
        assert_eq!(
            reqs,
            vec![req(
                "PATCH",
                "/api/projects/proj-1/tasks/wf-1/content",
                serde_json::json!({"title": "Revised"})
            )]
        );
    }

    #[test]
    fn create_replays_as_post_tasks_with_external_id_and_verb() {
        let (s, reqs) = replay_against_mock(
            "wf-new",
            &[action(
                ProposeKind::Create,
                serde_json::json!({
                    "title": "Write docs",
                    "verb": "implement",
                    "description": "details",
                    "external_id": "ta-proposal:g:a",
                    "assignee_id": "role-dev",
                }),
            )],
        );
        assert_eq!(s.replayed, 1);
        assert_eq!(
            reqs,
            vec![req(
                "POST",
                "/api/projects/proj-1/tasks",
                serde_json::json!({
                    "title": "Write docs",
                    "description": "details",
                    "verb": "implement",
                    "external_id": "ta-proposal:g:a",
                    "assignee_id": "role-dev",
                })
            )]
        );
    }

    #[test]
    fn reassign_replays_as_patch_assignee_with_assignee_id_or_null() {
        let (s, reqs) = replay_against_mock(
            "wf-1",
            &[
                action(
                    ProposeKind::Reassign,
                    serde_json::json!({"task_id": "wf-1", "assignee_id": "role-qa"}),
                ),
                action(
                    ProposeKind::Reassign,
                    serde_json::json!({"task_id": "wf-2", "assignee_id": null}),
                ),
            ],
        );
        assert_eq!(s.replayed, 2);
        assert_eq!(
            reqs,
            vec![
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/wf-1/assignee",
                    serde_json::json!({"assignee_id": "role-qa"})
                ),
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/wf-2/assignee",
                    serde_json::json!({"assignee_id": null})
                ),
            ]
        );
    }

    #[test]
    fn needs_revision_maps_to_open_and_on_hold_maps_to_on_hold_distinctly() {
        let (s, reqs) = replay_against_mock(
            "wf-1",
            &[
                action(
                    ProposeKind::NeedsRevision,
                    serde_json::json!({"task_id": "wf-1", "reason": "tests missing"}),
                ),
                action(
                    ProposeKind::OnHold,
                    serde_json::json!({
                        "task_id": "wf-2",
                        "hold_reason": "waiting on legal",
                        "blocking_task": null,
                    }),
                ),
            ],
        );
        assert_eq!(s.replayed, 2);
        assert_eq!(
            reqs,
            vec![
                // needs-revision: plain open, no hold_reason, reason not sent.
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/wf-1/status",
                    serde_json::json!({"status": "open"})
                ),
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/wf-2/status",
                    serde_json::json!({"status": "on_hold", "hold_reason": "waiting on legal"})
                ),
            ]
        );
        assert_ne!(reqs[0].2["status"], reqs[1].2["status"]);
    }

    #[test]
    fn on_hold_with_blocking_task_creates_precursor_then_dependency_then_status() {
        let (s, reqs) = replay_against_mock(
            "precursor-1",
            &[action(
                ProposeKind::OnHold,
                serde_json::json!({
                    "task_id": "held-1",
                    "hold_reason": "needs sign-off",
                    "blocking_task": {
                        "title": "Get sign-off",
                        "verb": "review",
                        "description": null,
                        "external_id": "ta-proposal:g:a:precursor",
                    },
                }),
            )],
        );
        assert_eq!(s.replayed, 1);
        assert_eq!(
            reqs,
            vec![
                req(
                    "POST",
                    "/api/projects/proj-1/tasks",
                    serde_json::json!({
                        "title": "Get sign-off",
                        "description": null,
                        "verb": "review",
                        "external_id": "ta-proposal:g:a:precursor",
                    })
                ),
                req(
                    "POST",
                    "/api/projects/proj-1/tasks/held-1/dependencies",
                    serde_json::json!({"depends_on_id": "precursor-1"})
                ),
                req(
                    "PATCH",
                    "/api/projects/proj-1/tasks/held-1/status",
                    serde_json::json!({"status": "on_hold", "hold_reason": "needs sign-off"})
                ),
            ]
        );
    }

    #[test]
    fn complete_replays_as_status_done() {
        let (s, reqs) = replay_against_mock(
            "wf-1",
            &[action(
                ProposeKind::Complete,
                serde_json::json!({"task_id": "wf-1"}),
            )],
        );
        assert_eq!(s.replayed, 1);
        assert_eq!(
            reqs,
            vec![req(
                "PATCH",
                "/api/projects/proj-1/tasks/wf-1/status",
                serde_json::json!({"status": "done"})
            )]
        );
    }

    #[test]
    fn re_replaying_the_same_actions_makes_no_further_calls() {
        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        mock.respond_all_with_task("wf-new");
        write_wayfinder_plan_config(&config, &mock.server.uri());
        let actions = vec![
            action(
                ProposeKind::Create,
                serde_json::json!({"title": "T", "verb": "implement", "external_id": "x:1"}),
            ),
            action(
                ProposeKind::Complete,
                serde_json::json!({"task_id": "wf-1"}),
            ),
        ];

        let first = replay_actions(&config, &actions);
        assert_eq!(first.replayed, 2);
        let second = replay_actions(&config, &actions);
        assert_eq!(second.replayed, 0);
        assert_eq!(second.already_replayed, 2);
        assert_eq!(
            mock.requests().len(),
            2,
            "second replay must not call Wayfinder"
        );
    }

    #[test]
    fn one_failing_action_does_not_block_the_others_and_is_not_ledgered() {
        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        mock.runtime.block_on(
            wiremock::Mock::given(wiremock::matchers::path(
                "/api/projects/proj-1/tasks/bad/status",
            ))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&mock.server),
        );
        mock.respond_all_with_task("good");
        write_wayfinder_plan_config(&config, &mock.server.uri());
        let failing = action(ProposeKind::Complete, serde_json::json!({"task_id": "bad"}));
        let ok = action(
            ProposeKind::Complete,
            serde_json::json!({"task_id": "good"}),
        );

        let s = replay_actions(&config, &[failing.clone(), ok.clone()]);
        assert_eq!(s.failed, 1);
        assert_eq!(s.replayed, 1);
        let ledger = read_ledger(&config.workspace_root.join(".ta").join(LEDGER_FILE));
        assert!(ledger.contains(&ok.action_id.to_string()));
        assert!(!ledger.contains(&failing.action_id.to_string()));
    }

    #[test]
    fn reviewer_rejected_actions_and_other_tools_are_not_replayed() {
        let mut rejected = action(
            ProposeKind::Complete,
            serde_json::json!({"task_id": "wf-1"}),
        );
        rejected.disposition = ArtifactDisposition::Rejected;
        let mut other = action(
            ProposeKind::Complete,
            serde_json::json!({"task_id": "wf-2"}),
        );
        other.tool_name = "ta_external_action".to_string();
        let (s, reqs) = replay_against_mock("wf-1", &[rejected, other]);
        assert_eq!(s.skipped_by_reviewer, 1);
        assert_eq!(s.replayed, 0);
        assert!(reqs.is_empty());
    }

    #[test]
    fn a_project_without_a_wayfinder_backend_is_skipped_without_error() {
        // No .ta/workflow.toml at all: the common case.
        let (_dir, config) = project();
        let s = replay_actions(
            &config,
            &[action(
                ProposeKind::Complete,
                serde_json::json!({"task_id": "wf-1"}),
            )],
        );
        assert!(s.backend_unavailable);
        assert_eq!(s.replayed + s.failed, 0);
    }

    #[test]
    fn no_task_proposals_means_no_backend_lookup_or_calls() {
        let (_dir, config) = project();
        let s = replay_actions(&config, &[]);
        assert_eq!(s, ReplaySummary::default());
    }

    #[test]
    fn only_applied_drafts_are_replayable() {
        assert!(is_replayable(&DraftStatus::Applied {
            applied_at: Utc::now(),
            applied_via: ApplyProvenance::Manual,
        }));
        assert!(!is_replayable(&DraftStatus::Draft));
        assert!(!is_replayable(&DraftStatus::PendingReview));
    }

    /// End to end with the real gateway handlers: what each tool captures is
    /// exactly what replay knows how to send.
    #[test]
    fn gateway_captured_actions_replay_cleanly() {
        use std::sync::{Arc, Mutex};
        use ta_mcp_gateway::tools::wayfinder_task as wt;

        let (_dir, config) = project();
        let mock = BlockingMockServer::start();
        mock.respond_all_with_task("wf-x");
        write_wayfinder_plan_config(&config, &mock.server.uri());

        let state = Arc::new(Mutex::new(
            ta_mcp_gateway::server::GatewayState::new(config.clone()).unwrap(),
        ));
        let goal = Uuid::new_v4();
        let g = goal.to_string();
        wt::handle_propose_task_create(
            &state,
            wt::ProposeTaskCreateParams {
                title: "New".into(),
                verb: "implement".into(),
                description: None,
                external_id: None,
                assignee_id: None,
                goal_run_id: g.clone(),
            },
        )
        .unwrap();
        wt::handle_propose_task_on_hold(
            &state,
            wt::ProposeTaskOnHoldParams {
                task_id: "wf-x".into(),
                hold_reason: "blocked".into(),
                blocking_task: Some(wt::BlockingTaskSpec {
                    title: "Pre".into(),
                    verb: "implement".into(),
                    description: None,
                }),
                goal_run_id: g,
            },
        )
        .unwrap();
        // Nothing reached Wayfinder at capture time.
        assert!(mock.requests().is_empty());

        let actions = state.lock().unwrap().pending_actions[&goal].clone();
        let s = replay_actions(&config, &actions);
        assert_eq!(s.replayed, 2);
        assert_eq!(s.failed, 0);
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 4);
        assert!(reqs[0].2["external_id"]
            .as_str()
            .unwrap()
            .starts_with(&format!("ta-proposal:{goal}:")));
    }
}
