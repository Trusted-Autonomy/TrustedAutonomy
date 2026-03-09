//! Slack Block Kit payload builders for the channel plugin.
//!
//! Builds rich Block Kit messages with interactive button components for
//! different question types. This is a standalone implementation that does
//! not depend on internal TA crates.

use crate::ChannelQuestion;

/// Build a Slack Block Kit message payload for the given question.
///
/// Adapts the question type (yes/no, choice, freeform) to the appropriate
/// Slack Block Kit components.
pub fn build_payload(question: &ChannelQuestion, channel_id: &str) -> serde_json::Value {
    let mut blocks = Vec::new();

    // Header block with turn info.
    blocks.push(serde_json::json!({
        "type": "header",
        "text": {
            "type": "plain_text",
            "text": format!("Agent Question (turn {})", question.turn),
            "emoji": true
        }
    }));

    // Question text as a section block.
    blocks.push(serde_json::json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": truncate(&question.question, 3000)
        }
    }));

    // Context block if present.
    if let Some(ctx) = &question.context {
        blocks.push(serde_json::json!({
            "type": "divider"
        }));
        blocks.push(serde_json::json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": format!("*Context:*\n{}", truncate(ctx, 2900))
            }
        }));
    }

    // Interactive elements based on response hint.
    match question.response_hint.as_str() {
        "yes_no" => {
            blocks.push(serde_json::json!({
                "type": "actions",
                "block_id": format!("ta_actions_{}", question.interaction_id),
                "elements": [
                    {
                        "type": "button",
                        "text": {
                            "type": "plain_text",
                            "text": "Approve",
                            "emoji": true
                        },
                        "style": "primary",
                        "action_id": format!("ta_{}_yes", question.interaction_id),
                        "value": "yes"
                    },
                    {
                        "type": "button",
                        "text": {
                            "type": "plain_text",
                            "text": "Deny",
                            "emoji": true
                        },
                        "style": "danger",
                        "action_id": format!("ta_{}_no", question.interaction_id),
                        "value": "no"
                    }
                ]
            }));
        }
        "choice" if !question.choices.is_empty() => {
            let buttons: Vec<serde_json::Value> = question
                .choices
                .iter()
                .enumerate()
                .take(5) // Slack allows up to 25 elements per actions block, but 5 keeps it readable
                .map(|(i, choice)| {
                    serde_json::json!({
                        "type": "button",
                        "text": {
                            "type": "plain_text",
                            "text": truncate(choice, 75),
                            "emoji": true
                        },
                        "action_id": format!("ta_{}_choice_{}", question.interaction_id, i),
                        "value": choice
                    })
                })
                .collect();

            blocks.push(serde_json::json!({
                "type": "actions",
                "block_id": format!("ta_actions_{}", question.interaction_id),
                "elements": buttons
            }));
        }
        _ => {
            // Freeform: add context block with callback instructions.
            blocks.push(serde_json::json!({
                "type": "divider"
            }));
            blocks.push(serde_json::json!({
                "type": "context",
                "elements": [{
                    "type": "mrkdwn",
                    "text": format!(
                        "Reply in this thread to answer. Or POST to `{}/api/interactions/{}/respond`",
                        question.callback_url, question.interaction_id
                    )
                }]
            }));
        }
    }

    // Interaction ID context footer for all types.
    blocks.push(serde_json::json!({
        "type": "context",
        "elements": [{
            "type": "mrkdwn",
            "text": format!("Interaction: `{}`", question.interaction_id)
        }]
    }));

    serde_json::json!({
        "channel": channel_id,
        "blocks": blocks,
        "text": format!("Agent Question (turn {}): {}", question.turn, truncate(&question.question, 200))
    })
}

/// Build Block Kit blocks for a thread reply with additional detail (e.g., diff context).
pub fn build_thread_detail(
    channel_id: &str,
    thread_ts: &str,
    detail_text: &str,
) -> serde_json::Value {
    serde_json::json!({
        "channel": channel_id,
        "thread_ts": thread_ts,
        "blocks": [
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": truncate(detail_text, 3000)
                }
            }
        ],
        "text": truncate(detail_text, 200)
    })
}

/// Truncate a string to the given max length.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a char boundary at or before max to avoid splitting multi-byte chars.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_question(hint: &str, choices: Vec<String>) -> ChannelQuestion {
        ChannelQuestion {
            interaction_id: "test-id-123".into(),
            goal_id: "goal-456".into(),
            question: "Which database?".into(),
            context: Some("Setting up backend".into()),
            response_hint: hint.into(),
            choices,
            turn: 1,
            callback_url: "http://localhost:7700".into(),
        }
    }

    #[test]
    fn payload_yes_no_buttons() {
        let q = test_question("yes_no", vec![]);
        let payload = build_payload(&q, "C123");
        let blocks = payload["blocks"].as_array().unwrap();
        // Find the actions block.
        let actions = blocks
            .iter()
            .find(|b| b["type"] == "actions")
            .expect("should have actions block");
        let elements = actions["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0]["text"]["text"], "Approve");
        assert_eq!(elements[0]["style"], "primary");
        assert_eq!(elements[1]["text"]["text"], "Deny");
        assert_eq!(elements[1]["style"], "danger");
    }

    #[test]
    fn payload_choice_buttons() {
        let q = test_question("choice", vec!["PostgreSQL".into(), "SQLite".into()]);
        let payload = build_payload(&q, "C123");
        let blocks = payload["blocks"].as_array().unwrap();
        let actions = blocks
            .iter()
            .find(|b| b["type"] == "actions")
            .expect("should have actions block");
        let elements = actions["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0]["text"]["text"], "PostgreSQL");
        assert_eq!(elements[1]["text"]["text"], "SQLite");
    }

    #[test]
    fn payload_freeform_has_reply_instructions() {
        let q = test_question("freeform", vec![]);
        let payload = build_payload(&q, "C123");
        let blocks = payload["blocks"].as_array().unwrap();
        // Should NOT have an actions block.
        assert!(blocks.iter().all(|b| b["type"] != "actions"));
        // Should have a context block with reply instructions.
        let ctx = blocks
            .iter()
            .find(|b| {
                b["type"] == "context"
                    && b["elements"]
                        .as_array()
                        .map(|e| {
                            e.iter()
                                .any(|el| el["text"].as_str().unwrap_or("").contains("Reply in"))
                        })
                        .unwrap_or(false)
            })
            .expect("should have context block with reply instructions");
        let text = ctx["elements"][0]["text"].as_str().unwrap();
        assert!(text.contains("test-id-123"));
    }

    #[test]
    fn payload_has_context_section() {
        let q = test_question("freeform", vec![]);
        let payload = build_payload(&q, "C123");
        let blocks = payload["blocks"].as_array().unwrap();
        let context_section = blocks
            .iter()
            .find(|b| {
                b["type"] == "section"
                    && b["text"]["text"]
                        .as_str()
                        .unwrap_or("")
                        .contains("*Context:*")
            })
            .expect("should have context section");
        assert!(context_section["text"]["text"]
            .as_str()
            .unwrap()
            .contains("Setting up backend"));
    }

    #[test]
    fn payload_no_context() {
        let mut q = test_question("freeform", vec![]);
        q.context = None;
        let payload = build_payload(&q, "C123");
        let blocks = payload["blocks"].as_array().unwrap();
        assert!(blocks.iter().all(|b| {
            if b["type"] == "section" {
                !b["text"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("*Context:*")
            } else {
                true
            }
        }));
    }

    #[test]
    fn payload_max_five_choice_buttons() {
        let choices: Vec<String> = (0..10).map(|i| format!("Option {}", i)).collect();
        let q = test_question("choice", choices);
        let payload = build_payload(&q, "C123");
        let blocks = payload["blocks"].as_array().unwrap();
        let actions = blocks
            .iter()
            .find(|b| b["type"] == "actions")
            .expect("should have actions block");
        let elements = actions["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 5);
    }

    #[test]
    fn payload_has_channel() {
        let q = test_question("yes_no", vec![]);
        let payload = build_payload(&q, "C123456");
        assert_eq!(payload["channel"], "C123456");
    }

    #[test]
    fn payload_has_fallback_text() {
        let q = test_question("yes_no", vec![]);
        let payload = build_payload(&q, "C123");
        let text = payload["text"].as_str().unwrap();
        assert!(text.contains("Which database?"));
        assert!(text.contains("turn 1"));
    }

    #[test]
    fn button_action_ids_contain_interaction_id() {
        let q = test_question("choice", vec!["A".into(), "B".into()]);
        let payload = build_payload(&q, "C123");
        let blocks = payload["blocks"].as_array().unwrap();
        let actions = blocks
            .iter()
            .find(|b| b["type"] == "actions")
            .unwrap();
        for element in actions["elements"].as_array().unwrap() {
            let action_id = element["action_id"].as_str().unwrap();
            assert!(action_id.contains("test-id-123"));
        }
    }

    #[test]
    fn thread_detail_payload() {
        let payload = build_thread_detail("C123", "1234567890.123456", "Here are the details...");
        assert_eq!(payload["channel"], "C123");
        assert_eq!(payload["thread_ts"], "1234567890.123456");
        let blocks = payload["blocks"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0]["text"]["text"]
            .as_str()
            .unwrap()
            .contains("details"));
    }

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long() {
        let long = "a".repeat(100);
        assert_eq!(truncate(&long, 50).len(), 50);
    }

    #[test]
    fn interaction_id_in_footer() {
        let q = test_question("freeform", vec![]);
        let payload = build_payload(&q, "C123");
        let blocks = payload["blocks"].as_array().unwrap();
        let footer = blocks
            .iter()
            .rfind(|b| b["type"] == "context")
            .expect("should have context footer");
        let text = footer["elements"][0]["text"].as_str().unwrap();
        assert!(text.contains("test-id-123"));
    }
}
