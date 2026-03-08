//! Discord embed and component payload builders.

use ta_changeset::interaction::{
    InteractionKind, InteractionRequest, Notification, NotificationLevel,
};

/// Discord embed color constants.
const COLOR_REVIEW: u32 = 0xFFA500; // Orange — draft review
const COLOR_APPROVE: u32 = 0x57F287; // Green — approval-related
const COLOR_ESCALATION: u32 = 0xED4245; // Red — escalation
const COLOR_INFO: u32 = 0x5865F2; // Discord blurple — info
const COLOR_WARNING: u32 = 0xFEE75C; // Yellow — warning
const COLOR_ERROR: u32 = 0xED4245; // Red — error
const COLOR_DEBUG: u32 = 0x99AAB5; // Grey — debug

/// Build a Discord message payload for a review interaction request.
///
/// Returns a JSON value ready to POST to Discord's create-message endpoint.
/// Includes an embed describing the interaction and button components for
/// Approve/Deny/Discuss actions.
pub fn build_review_embed(request: &InteractionRequest) -> serde_json::Value {
    let (title, color) = match &request.kind {
        InteractionKind::DraftReview => ("Draft Review", COLOR_REVIEW),
        InteractionKind::ApprovalDiscussion => ("Approval Discussion", COLOR_APPROVE),
        InteractionKind::PlanNegotiation => ("Plan Negotiation", COLOR_INFO),
        InteractionKind::Escalation => ("Escalation", COLOR_ESCALATION),
        InteractionKind::AgentQuestion => ("Agent Question", COLOR_INFO),
        InteractionKind::Custom(name) => return build_custom_embed(name, request, COLOR_INFO),
    };

    let description = build_description(request);
    let interaction_id = request.interaction_id.to_string();

    let mut embed = serde_json::json!({
        "title": format!("TA: {}", title),
        "description": description,
        "color": color,
        "timestamp": request.created_at.to_rfc3339(),
        "footer": {
            "text": format!("Interaction: {}", &interaction_id[..8])
        }
    });

    // Add fields from context.
    let mut fields = Vec::new();
    if let Some(summary) = request.context.get("summary").and_then(|v| v.as_str()) {
        fields.push(serde_json::json!({
            "name": "Summary",
            "value": truncate(summary, 1024),
            "inline": false
        }));
    }
    if let Some(count) = request
        .context
        .get("artifact_count")
        .and_then(|v| v.as_u64())
    {
        fields.push(serde_json::json!({
            "name": "Artifacts",
            "value": count.to_string(),
            "inline": true
        }));
    }
    if let Some(draft_id) = request.context.get("draft_id").and_then(|v| v.as_str()) {
        fields.push(serde_json::json!({
            "name": "Draft ID",
            "value": format!("`{}`", &draft_id[..draft_id.len().min(12)]),
            "inline": true
        }));
    }
    if let Some(phase) = request.context.get("phase").and_then(|v| v.as_str()) {
        fields.push(serde_json::json!({
            "name": "Phase",
            "value": phase,
            "inline": true
        }));
    }
    if let Some(reason) = request.context.get("reason").and_then(|v| v.as_str()) {
        fields.push(serde_json::json!({
            "name": "Reason",
            "value": truncate(reason, 1024),
            "inline": false
        }));
    }
    if let Some(goal_id) = &request.goal_id {
        fields.push(serde_json::json!({
            "name": "Goal",
            "value": format!("`{}`", &goal_id.to_string()[..8]),
            "inline": true
        }));
    }
    if !fields.is_empty() {
        embed["fields"] = serde_json::Value::Array(fields);
    }

    let components = build_review_buttons(&interaction_id, &request.kind);

    serde_json::json!({
        "embeds": [embed],
        "components": components
    })
}

/// Build a Discord message payload for a notification.
pub fn build_notification_embed(notification: &Notification) -> serde_json::Value {
    let (prefix, color) = match notification.level {
        NotificationLevel::Debug => ("Debug", COLOR_DEBUG),
        NotificationLevel::Info => ("Info", COLOR_INFO),
        NotificationLevel::Warning => ("Warning", COLOR_WARNING),
        NotificationLevel::Error => ("Error", COLOR_ERROR),
    };

    let mut embed = serde_json::json!({
        "title": format!("TA: {}", prefix),
        "description": truncate(&notification.message, 4096),
        "color": color,
        "timestamp": notification.created_at.to_rfc3339(),
    });

    if let Some(goal_id) = &notification.goal_id {
        embed["footer"] = serde_json::json!({
            "text": format!("Goal: {}", &goal_id.to_string()[..8])
        });
    }

    serde_json::json!({
        "embeds": [embed]
    })
}

/// Build button components for review interactions.
fn build_review_buttons(interaction_id: &str, kind: &InteractionKind) -> serde_json::Value {
    match kind {
        InteractionKind::DraftReview | InteractionKind::ApprovalDiscussion => {
            serde_json::json!([{
                "type": 1, // ACTION_ROW
                "components": [
                    {
                        "type": 2, // BUTTON
                        "style": 3, // SUCCESS (green)
                        "label": "Approve",
                        "custom_id": format!("ta_review_{}_approve", interaction_id),
                        "emoji": { "name": "✅" }
                    },
                    {
                        "type": 2,
                        "style": 4, // DANGER (red)
                        "label": "Deny",
                        "custom_id": format!("ta_review_{}_deny", interaction_id),
                        "emoji": { "name": "❌" }
                    },
                    {
                        "type": 2,
                        "style": 1, // PRIMARY (blurple)
                        "label": "Discuss",
                        "custom_id": format!("ta_review_{}_discuss", interaction_id),
                        "emoji": { "name": "💬" }
                    }
                ]
            }])
        }
        InteractionKind::PlanNegotiation => {
            serde_json::json!([{
                "type": 1,
                "components": [
                    {
                        "type": 2,
                        "style": 3,
                        "label": "Accept",
                        "custom_id": format!("ta_review_{}_approve", interaction_id),
                    },
                    {
                        "type": 2,
                        "style": 4,
                        "label": "Reject",
                        "custom_id": format!("ta_review_{}_deny", interaction_id),
                    }
                ]
            }])
        }
        InteractionKind::Escalation => {
            serde_json::json!([{
                "type": 1,
                "components": [
                    {
                        "type": 2,
                        "style": 3,
                        "label": "Acknowledge",
                        "custom_id": format!("ta_review_{}_approve", interaction_id),
                    },
                    {
                        "type": 2,
                        "style": 4,
                        "label": "Intervene",
                        "custom_id": format!("ta_review_{}_deny", interaction_id),
                    }
                ]
            }])
        }
        InteractionKind::AgentQuestion => {
            serde_json::json!([{
                "type": 1,
                "components": [
                    {
                        "type": 2,
                        "style": 3,
                        "label": "Yes",
                        "custom_id": format!("ta_review_{}_approve", interaction_id),
                    },
                    {
                        "type": 2,
                        "style": 4,
                        "label": "No",
                        "custom_id": format!("ta_review_{}_deny", interaction_id),
                    },
                    {
                        "type": 2,
                        "style": 1,
                        "label": "Discuss",
                        "custom_id": format!("ta_review_{}_discuss", interaction_id),
                    }
                ]
            }])
        }
        InteractionKind::Custom(_) => serde_json::json!([]),
    }
}

fn build_custom_embed(name: &str, request: &InteractionRequest, color: u32) -> serde_json::Value {
    serde_json::json!({
        "embeds": [{
            "title": format!("TA: {}", name),
            "description": format!("{}", request.context),
            "color": color,
            "timestamp": request.created_at.to_rfc3339(),
        }]
    })
}

fn build_description(request: &InteractionRequest) -> String {
    match &request.kind {
        InteractionKind::DraftReview => {
            "A draft is ready for your review. Use the buttons below to approve, deny, or discuss."
                .to_string()
        }
        InteractionKind::ApprovalDiscussion => {
            "The agent is requesting approval to proceed.".to_string()
        }
        InteractionKind::PlanNegotiation => {
            "The agent proposes a plan change — please accept or reject.".to_string()
        }
        InteractionKind::Escalation => {
            "An issue has been escalated that requires your attention.".to_string()
        }
        InteractionKind::AgentQuestion => {
            let question = request
                .context
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("The agent has a question.");
            question.to_string()
        }
        InteractionKind::Custom(name) => format!("Custom interaction: {}", name),
    }
}

/// Truncate a string to the given max length, appending "…" if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_changeset::interaction::{InteractionKind, InteractionRequest, Urgency};
    use uuid::Uuid;

    #[test]
    fn review_embed_draft_review() {
        let request = InteractionRequest::draft_review(Uuid::new_v4(), "Test summary", 5);
        let payload = build_review_embed(&request);

        let embeds = payload["embeds"].as_array().unwrap();
        assert_eq!(embeds.len(), 1);
        assert!(embeds[0]["title"]
            .as_str()
            .unwrap()
            .contains("Draft Review"));
        assert_eq!(embeds[0]["color"], COLOR_REVIEW);

        let components = payload["components"].as_array().unwrap();
        assert_eq!(components.len(), 1);
        let buttons = components[0]["components"].as_array().unwrap();
        assert_eq!(buttons.len(), 3); // Approve, Deny, Discuss
        assert_eq!(buttons[0]["label"], "Approve");
        assert_eq!(buttons[1]["label"], "Deny");
        assert_eq!(buttons[2]["label"], "Discuss");
    }

    #[test]
    fn review_embed_has_fields() {
        let request = InteractionRequest::draft_review(Uuid::new_v4(), "My summary", 3);
        let payload = build_review_embed(&request);
        let fields = payload["embeds"][0]["fields"].as_array().unwrap();
        let field_names: Vec<&str> = fields.iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert!(field_names.contains(&"Summary"));
        assert!(field_names.contains(&"Artifacts"));
        assert!(field_names.contains(&"Draft ID"));
    }

    #[test]
    fn review_embed_escalation() {
        let request =
            InteractionRequest::escalation("budget exceeded", serde_json::json!({"budget": 100}));
        let payload = build_review_embed(&request);
        let embeds = payload["embeds"].as_array().unwrap();
        assert!(embeds[0]["title"].as_str().unwrap().contains("Escalation"));
        assert_eq!(embeds[0]["color"], COLOR_ESCALATION);

        let buttons = payload["components"][0]["components"].as_array().unwrap();
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0]["label"], "Acknowledge");
        assert_eq!(buttons[1]["label"], "Intervene");
    }

    #[test]
    fn review_embed_plan_negotiation() {
        let request = InteractionRequest::plan_negotiation("v0.10.1", "done");
        let payload = build_review_embed(&request);
        let buttons = payload["components"][0]["components"].as_array().unwrap();
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0]["label"], "Accept");
        assert_eq!(buttons[1]["label"], "Reject");
    }

    #[test]
    fn notification_embed_levels() {
        use ta_changeset::interaction::Notification;

        let info = build_notification_embed(&Notification::info("test info"));
        assert!(info["embeds"][0]["title"]
            .as_str()
            .unwrap()
            .contains("Info"));
        assert_eq!(info["embeds"][0]["color"], COLOR_INFO);

        let warn = build_notification_embed(&Notification::warning("test warning"));
        assert!(warn["embeds"][0]["title"]
            .as_str()
            .unwrap()
            .contains("Warning"));
        assert_eq!(warn["embeds"][0]["color"], COLOR_WARNING);
    }

    #[test]
    fn notification_embed_with_goal_id() {
        let goal_id = Uuid::new_v4();
        let notif = Notification::info("test").with_goal_id(goal_id);
        let payload = build_notification_embed(&notif);
        let footer = payload["embeds"][0]["footer"]["text"].as_str().unwrap();
        assert!(footer.contains("Goal:"));
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let long = "a".repeat(100);
        let truncated = truncate(&long, 50);
        // 49 'a' chars + '…' (3 bytes in UTF-8)
        assert!(truncated.chars().count() <= 50);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn custom_interaction_kind() {
        let request = InteractionRequest::new(
            InteractionKind::Custom("webhook_alert".into()),
            serde_json::json!({"msg": "hello"}),
            Urgency::Informational,
        );
        let payload = build_review_embed(&request);
        assert!(payload["embeds"][0]["title"]
            .as_str()
            .unwrap()
            .contains("webhook_alert"));
        // Custom kinds have no buttons.
        assert!(payload.get("components").is_none());
    }

    #[test]
    fn button_custom_ids_contain_interaction_id() {
        let request = InteractionRequest::draft_review(Uuid::new_v4(), "test", 1);
        let payload = build_review_embed(&request);
        let id = request.interaction_id.to_string();
        let buttons = payload["components"][0]["components"].as_array().unwrap();
        for button in buttons {
            let custom_id = button["custom_id"].as_str().unwrap();
            assert!(
                custom_id.contains(&id),
                "custom_id '{}' should contain interaction_id '{}'",
                custom_id,
                id
            );
        }
    }
}
