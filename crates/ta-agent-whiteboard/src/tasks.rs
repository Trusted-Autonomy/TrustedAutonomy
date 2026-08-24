//! Shared, dependency-aware task/claim list (v0.17.11.2 item 4) —
//! generalizes Claude Code's own validated Agent Teams pattern (a shared
//! task list, file-locked claiming) past single-team/single-machine scope.
//! Claiming is race-free via [`WhiteboardTransport::kv_create`]'s
//! create-if-absent semantics rather than a get-then-put race.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::transport::WhiteboardTransport;

pub const TASKS_BUCKET: &str = "wb_tasks";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhiteboardTask {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub claimed_by: Option<String>,
}

impl WhiteboardTask {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            depends_on: Vec::new(),
            status: TaskStatus::Pending,
            claimed_by: None,
        }
    }

    pub fn depends_on(mut self, deps: Vec<String>) -> Self {
        self.depends_on = deps;
        self
    }
}

/// Publish `task` to the shared list. Overwrites any existing task with the
/// same `id` — use [`claim_task`] to transition state, not repeated
/// `publish_task` calls, to avoid clobbering a concurrent claim.
pub async fn publish_task(
    transport: &dyn WhiteboardTransport,
    task: &WhiteboardTask,
) -> Result<()> {
    let payload = serde_json::to_vec(task)?;
    transport
        .kv_put(TASKS_BUCKET, &task.id, payload, None)
        .await
}

/// All tasks currently on the shared list.
pub async fn list_tasks(transport: &dyn WhiteboardTransport) -> Result<Vec<WhiteboardTask>> {
    let raw = transport.kv_list(TASKS_BUCKET).await?;
    let mut tasks = Vec::with_capacity(raw.len());
    for (key, value) in raw {
        match serde_json::from_slice::<WhiteboardTask>(&value) {
            Ok(task) => tasks.push(task),
            Err(e) => tracing::warn!(key, error = %e, "skipping malformed whiteboard task"),
        }
    }
    Ok(tasks)
}

/// Tasks that are `Pending` and whose `depends_on` are all `Done` — the
/// live "what's actually claimable right now" set, mirroring `ta-plan`'s
/// `next_ready_phases` / `task-graph`'s wave-readiness concept.
pub async fn claimable_tasks(transport: &dyn WhiteboardTransport) -> Result<Vec<WhiteboardTask>> {
    let all = list_tasks(transport).await?;
    let done: std::collections::HashSet<&str> = all
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .map(|t| t.id.as_str())
        .collect();
    Ok(all
        .iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .filter(|t| t.depends_on.iter().all(|dep| done.contains(dep.as_str())))
        .cloned()
        .collect())
}

/// Attempt to claim `task_id` for `agent_id`. Race-free: uses
/// `kv_create` on a separate claim-marker key, so two agents racing to
/// claim the same task can never both succeed. Returns `Ok(true)` if this
/// call won the claim, `Ok(false)` if someone else already holds it (or the
/// task doesn't exist / isn't currently claimable).
pub async fn claim_task(
    transport: &dyn WhiteboardTransport,
    task_id: &str,
    agent_id: &str,
) -> Result<bool> {
    let Some(raw) = transport.kv_get(TASKS_BUCKET, task_id).await? else {
        return Ok(false);
    };
    let mut task: WhiteboardTask = serde_json::from_slice(&raw)?;
    if task.status != TaskStatus::Pending {
        return Ok(false);
    }

    let claim_key = claim_marker_key(task_id);
    let won = transport
        .kv_create(TASKS_BUCKET, &claim_key, agent_id.as_bytes().to_vec())
        .await?;
    if !won {
        return Ok(false);
    }

    task.status = TaskStatus::InProgress;
    task.claimed_by = Some(agent_id.to_string());
    publish_task(transport, &task).await?;
    Ok(true)
}

/// Mark `task_id` done. Idempotent — safe to call even if the task was
/// never claimed (e.g. resolved out-of-band).
pub async fn complete_task(transport: &dyn WhiteboardTransport, task_id: &str) -> Result<()> {
    let Some(raw) = transport.kv_get(TASKS_BUCKET, task_id).await? else {
        return Ok(());
    };
    let mut task: WhiteboardTask = serde_json::from_slice(&raw)?;
    task.status = TaskStatus::Done;
    publish_task(transport, &task).await
}

/// NATS JetStream KV keys are restricted to `[-/_=.a-zA-Z0-9]` (no `:`),
/// so this uses `claim_<id>` rather than the more conventional
/// `claim:<id>` — kept consistent across backends even though only the
/// NATS one enforces it.
fn claim_marker_key(task_id: &str) -> String {
    format!("claim_{task_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_transport::InMemoryTransport;

    #[tokio::test]
    async fn claimable_tasks_respects_dependencies() {
        let t = InMemoryTransport::new();
        publish_task(&t, &WhiteboardTask::new("a", "first"))
            .await
            .unwrap();
        publish_task(
            &t,
            &WhiteboardTask::new("b", "second").depends_on(vec!["a".to_string()]),
        )
        .await
        .unwrap();

        let claimable = claimable_tasks(&t).await.unwrap();
        assert_eq!(claimable.len(), 1);
        assert_eq!(claimable[0].id, "a");

        complete_task(&t, "a").await.unwrap();
        let claimable = claimable_tasks(&t).await.unwrap();
        assert_eq!(claimable.len(), 1);
        assert_eq!(claimable[0].id, "b");
    }

    #[tokio::test]
    async fn claim_task_is_race_free_between_two_claimants() {
        let t = InMemoryTransport::new();
        publish_task(&t, &WhiteboardTask::new("a", "first"))
            .await
            .unwrap();

        let first = claim_task(&t, "a", "agent-1").await.unwrap();
        let second = claim_task(&t, "a", "agent-2").await.unwrap();
        assert!(first);
        assert!(!second);

        let raw = t.kv_get(TASKS_BUCKET, "a").await.unwrap().unwrap();
        let task: WhiteboardTask = serde_json::from_slice(&raw).unwrap();
        assert_eq!(task.claimed_by.as_deref(), Some("agent-1"));
        assert_eq!(task.status, TaskStatus::InProgress);
    }

    #[tokio::test]
    async fn claim_task_fails_for_unknown_task() {
        let t = InMemoryTransport::new();
        assert!(!claim_task(&t, "nonexistent", "agent-1").await.unwrap());
    }

    #[tokio::test]
    async fn claim_task_fails_once_already_in_progress() {
        let t = InMemoryTransport::new();
        publish_task(&t, &WhiteboardTask::new("a", "first"))
            .await
            .unwrap();
        assert!(claim_task(&t, "a", "agent-1").await.unwrap());
        assert!(!claim_task(&t, "a", "agent-1").await.unwrap());
    }

    #[tokio::test]
    async fn complete_task_is_idempotent_for_unknown_task() {
        let t = InMemoryTransport::new();
        complete_task(&t, "never-existed").await.unwrap();
    }
}
