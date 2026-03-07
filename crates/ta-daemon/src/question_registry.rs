// question_registry.rs — Coordination between MCP tool handler and HTTP response endpoint.
//
// When an agent calls ta_ask_human, the MCP tool handler inserts a PendingQuestion
// here and awaits the oneshot::Receiver. When a human responds via
// POST /api/interactions/:id/respond, the HTTP handler calls answer() which
// fires the oneshot::Sender, unblocking the tool handler.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Metadata about a pending question (serializable for API responses).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingQuestion {
    pub interaction_id: Uuid,
    pub goal_id: Option<Uuid>,
    pub question: String,
    pub context: Option<String>,
    /// Expected response shape: "freeform", "yes_no", "choice".
    pub response_hint: String,
    /// Suggested choices when response_hint is "choice".
    pub choices: Vec<String>,
    /// Turn number in the conversation (1-based).
    pub turn: u32,
    pub created_at: DateTime<Utc>,
    /// Timeout in seconds (None = wait forever).
    pub timeout_secs: Option<u64>,
}

/// The human's answer to a pending question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanAnswer {
    pub text: String,
    pub responder_id: String,
    pub answered_at: DateTime<Utc>,
}

/// Registry of pending questions with oneshot channels for synchronization.
pub struct QuestionRegistry {
    pending: tokio::sync::Mutex<HashMap<Uuid, (PendingQuestion, oneshot::Sender<HumanAnswer>)>>,
}

impl QuestionRegistry {
    pub fn new() -> Self {
        Self {
            pending: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Register a pending question. Returns the Receiver that the caller should await.
    pub async fn register(&self, question: PendingQuestion) -> oneshot::Receiver<HumanAnswer> {
        let (tx, rx) = oneshot::channel();
        let id = question.interaction_id;
        self.pending.lock().await.insert(id, (question, tx));
        rx
    }

    /// Deliver a human's answer to a pending question.
    /// Returns Ok(()) if delivered, Err(answer) if no pending question with that ID.
    pub async fn answer(&self, id: Uuid, answer: HumanAnswer) -> Result<(), HumanAnswer> {
        let entry = self.pending.lock().await.remove(&id);
        match entry {
            Some((_question, tx)) => {
                // If the receiver was dropped (agent timed out), this send fails silently.
                let _ = tx.send(answer);
                Ok(())
            }
            None => Err(answer),
        }
    }

    /// List all pending questions (without the oneshot senders).
    pub async fn list_pending(&self) -> Vec<PendingQuestion> {
        self.pending
            .lock()
            .await
            .values()
            .map(|(q, _)| q.clone())
            .collect()
    }

    /// Cancel a pending question. Drops the sender, causing the awaiting tool handler
    /// to receive a RecvError.
    pub async fn cancel(&self, id: Uuid) -> bool {
        self.pending.lock().await.remove(&id).is_some()
    }

    /// Number of pending questions.
    pub async fn len(&self) -> usize {
        self.pending.lock().await.len()
    }

    /// Whether there are no pending questions.
    pub async fn is_empty(&self) -> bool {
        self.pending.lock().await.is_empty()
    }
}

impl Default for QuestionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_question(id: Uuid) -> PendingQuestion {
        PendingQuestion {
            interaction_id: id,
            goal_id: None,
            question: "What should I do next?".to_string(),
            context: Some("Agent is stuck at a decision point.".to_string()),
            response_hint: "freeform".to_string(),
            choices: vec![],
            turn: 1,
            created_at: Utc::now(),
            timeout_secs: None,
        }
    }

    fn make_answer() -> HumanAnswer {
        HumanAnswer {
            text: "Proceed with option A.".to_string(),
            responder_id: "user-1".to_string(),
            answered_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn register_and_answer() {
        let registry = QuestionRegistry::new();
        let id = Uuid::new_v4();
        let question = make_question(id);

        let rx = registry.register(question).await;

        // Spawn a task that awaits the receiver — simulates the MCP tool handler blocking.
        let waiter = tokio::spawn(rx);

        let answer = make_answer();
        let expected_text = answer.text.clone();

        registry
            .answer(id, answer)
            .await
            .expect("answer should succeed");

        let received = waiter
            .await
            .expect("task should not panic")
            .expect("receiver should get answer");
        assert_eq!(received.text, expected_text);
        assert_eq!(received.responder_id, "user-1");
    }

    #[tokio::test]
    async fn answer_unknown_id() {
        let registry = QuestionRegistry::new();
        let unknown_id = Uuid::new_v4();
        let answer = make_answer();
        let answer_text = answer.text.clone();

        let result = registry.answer(unknown_id, answer).await;

        assert!(result.is_err(), "answering unknown ID should return Err");
        let returned = result.unwrap_err();
        assert_eq!(
            returned.text, answer_text,
            "returned answer should be the original"
        );
    }

    #[tokio::test]
    async fn list_pending() {
        let registry = QuestionRegistry::new();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        let _rx1 = registry.register(make_question(id1)).await;
        let _rx2 = registry.register(make_question(id2)).await;

        let pending = registry.list_pending().await;
        assert_eq!(pending.len(), 2, "should list both pending questions");

        let ids: Vec<Uuid> = pending.iter().map(|q| q.interaction_id).collect();
        assert!(ids.contains(&id1), "should contain first question ID");
        assert!(ids.contains(&id2), "should contain second question ID");
    }

    #[tokio::test]
    async fn cancel_drops_sender() {
        let registry = QuestionRegistry::new();
        let id = Uuid::new_v4();
        let question = make_question(id);

        let rx = registry.register(question).await;

        // Spawn a task that awaits the receiver — expects it to error when sender drops.
        let waiter = tokio::spawn(rx);

        let cancelled = registry.cancel(id).await;
        assert!(cancelled, "cancel should return true for a known ID");

        // The sender was dropped by cancel(), so the receiver should get RecvError.
        let result = waiter.await.expect("task should not panic");
        assert!(
            result.is_err(),
            "receiver should error after sender is dropped"
        );
    }

    #[tokio::test]
    async fn answer_after_cancel_fails() {
        let registry = QuestionRegistry::new();
        let id = Uuid::new_v4();
        let question = make_question(id);

        let _rx = registry.register(question).await;
        let cancelled = registry.cancel(id).await;
        assert!(cancelled, "first cancel should succeed");

        // Now try to answer — the entry was removed by cancel, so it should fail.
        let answer = make_answer();
        let result = registry.answer(id, answer).await;
        assert!(result.is_err(), "answer after cancel should return Err");
    }

    #[tokio::test]
    async fn len_and_is_empty() {
        let registry = QuestionRegistry::new();

        assert!(registry.is_empty().await, "registry starts empty");
        assert_eq!(registry.len().await, 0, "len starts at 0");

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        let _rx1 = registry.register(make_question(id1)).await;
        assert!(!registry.is_empty().await, "not empty after first register");
        assert_eq!(registry.len().await, 1, "len is 1 after first register");

        let _rx2 = registry.register(make_question(id2)).await;
        assert_eq!(registry.len().await, 2, "len is 2 after second register");

        registry.cancel(id1).await;
        assert_eq!(registry.len().await, 1, "len is 1 after cancel");

        let answer = make_answer();
        registry
            .answer(id2, answer)
            .await
            .expect("answer should succeed");
        assert_eq!(registry.len().await, 0, "len is 0 after answer");
        assert!(registry.is_empty().await, "is_empty after all resolved");
    }
}
