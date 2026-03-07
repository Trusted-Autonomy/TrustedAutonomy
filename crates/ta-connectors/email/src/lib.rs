//! # ta-connector-email
//!
//! Email channel delivery adapter for Trusted Autonomy.
//!
//! Sends agent questions as emails via a configurable HTTP-based email
//! sending endpoint. Responses come back through an inbound webhook that
//! parses reply emails and calls `POST /api/interactions/:id/respond`.

use serde::{Deserialize, Serialize};
use ta_events::channel::{ChannelDelivery, ChannelQuestion, DeliveryResult};

/// Email adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    /// HTTP endpoint for sending emails (e.g., SendGrid, Mailgun, or custom).
    /// Expected to accept POST with JSON body containing `to`, `subject`, `body_html`, `body_text`.
    pub send_endpoint: String,
    /// API key or bearer token for the send endpoint.
    pub api_key: String,
    /// Sender email address (e.g., "ta-agent@yourcompany.com").
    pub from_address: String,
    /// Recipient email address.
    pub to_address: String,
}

/// Email channel delivery adapter.
///
/// Sends questions as formatted emails. The email contains the question text,
/// context, and available choices. For choice-based questions, clickable links
/// call the daemon respond endpoint directly.
pub struct EmailAdapter {
    config: EmailConfig,
    client: reqwest::Client,
}

impl EmailAdapter {
    pub fn new(config: EmailConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Build the email subject line.
    fn build_subject(&self, question: &ChannelQuestion) -> String {
        let truncated = if question.question.len() > 60 {
            format!("{}...", &question.question[..57])
        } else {
            question.question.clone()
        };
        format!(
            "[TA] Agent question (turn {}): {}",
            question.turn, truncated
        )
    }

    /// Build plain text email body.
    fn build_body_text(&self, question: &ChannelQuestion) -> String {
        let mut body = format!(
            "An agent needs your input.\n\nQuestion: {}\n",
            question.question
        );

        if let Some(ctx) = &question.context {
            body.push_str(&format!("\nContext: {}\n", ctx));
        }

        match question.response_hint.as_str() {
            "yes_no" => {
                body.push_str(&format!(
                    "\nTo answer YES:\n  {}/api/interactions/{}/respond\n  Body: {{\"answer\": \"yes\"}}\n",
                    question.callback_url, question.interaction_id
                ));
                body.push_str(&format!(
                    "\nTo answer NO:\n  {}/api/interactions/{}/respond\n  Body: {{\"answer\": \"no\"}}\n",
                    question.callback_url, question.interaction_id
                ));
            }
            "choice" if !question.choices.is_empty() => {
                body.push_str("\nChoices:\n");
                for (i, choice) in question.choices.iter().enumerate() {
                    body.push_str(&format!("  {}. {}\n", i + 1, choice));
                }
                body.push_str(&format!(
                    "\nRespond via: POST {}/api/interactions/{}/respond\nBody: {{\"answer\": \"your choice\"}}\n",
                    question.callback_url, question.interaction_id
                ));
            }
            _ => {
                body.push_str(&format!(
                    "\nReply to this email or respond via:\n  POST {}/api/interactions/{}/respond\n  Body: {{\"answer\": \"your response\"}}\n",
                    question.callback_url, question.interaction_id
                ));
            }
        }

        body.push_str(&format!(
            "\nInteraction ID: {}\nGoal ID: {}\n",
            question.interaction_id, question.goal_id
        ));

        body
    }

    /// Build HTML email body with clickable response links.
    fn build_body_html(&self, question: &ChannelQuestion) -> String {
        let mut html = String::from("<div style=\"font-family: sans-serif; max-width: 600px;\">");
        html.push_str(&format!(
            "<h2 style=\"color: #333;\">Agent Question (turn {})</h2>",
            question.turn
        ));
        html.push_str(&format!(
            "<p style=\"font-size: 16px;\">{}</p>",
            question.question
        ));

        if let Some(ctx) = &question.context {
            html.push_str(&format!(
                "<p style=\"color: #666; font-style: italic;\">Context: {}</p>",
                ctx
            ));
        }

        match question.response_hint.as_str() {
            "yes_no" => {
                html.push_str("<div style=\"margin: 20px 0;\">");
                html.push_str(
                    "<p>Reply to this email with <strong>yes</strong> or <strong>no</strong>, \
                     or use the API endpoint below.</p>",
                );
                html.push_str("</div>");
            }
            "choice" if !question.choices.is_empty() => {
                html.push_str("<div style=\"margin: 20px 0;\">");
                html.push_str("<p><strong>Options:</strong></p><ul>");
                for choice in &question.choices {
                    html.push_str(&format!("<li>{}</li>", choice));
                }
                html.push_str("</ul>");
                html.push_str("<p>Reply to this email with your choice.</p>");
                html.push_str("</div>");
            }
            _ => {
                html.push_str("<p>Reply to this email with your answer.</p>");
            }
        }

        html.push_str(&format!(
            "<hr style=\"border: 1px solid #eee;\"><p style=\"font-size: 12px; color: #999;\">\
             Interaction ID: <code>{}</code><br>Goal ID: <code>{}</code></p>",
            question.interaction_id, question.goal_id
        ));
        html.push_str("</div>");

        html
    }
}

#[async_trait::async_trait]
impl ChannelDelivery for EmailAdapter {
    fn name(&self) -> &str {
        "email"
    }

    async fn deliver_question(&self, question: &ChannelQuestion) -> DeliveryResult {
        let body = serde_json::json!({
            "from": self.config.from_address,
            "to": self.config.to_address,
            "subject": self.build_subject(question),
            "body_text": self.build_body_text(question),
            "body_html": self.build_body_html(question),
            "headers": {
                "X-TA-Interaction-ID": question.interaction_id.to_string(),
                "X-TA-Goal-ID": question.goal_id.to_string(),
            }
        });

        match self
            .client
            .post(&self.config.send_endpoint)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let delivery_id = match resp.json::<serde_json::Value>().await {
                        Ok(json) => json
                            .get("id")
                            .or_else(|| json.get("message_id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        Err(_) => String::new(),
                    };
                    tracing::info!(
                        channel = "email",
                        interaction_id = %question.interaction_id,
                        to = %self.config.to_address,
                        "Question delivered via email"
                    );
                    DeliveryResult {
                        channel: "email".into(),
                        delivery_id,
                        success: true,
                        error: None,
                    }
                } else {
                    let err_body = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        channel = "email",
                        interaction_id = %question.interaction_id,
                        status = %status,
                        "Email send endpoint returned error"
                    );
                    DeliveryResult {
                        channel: "email".into(),
                        delivery_id: String::new(),
                        success: false,
                        error: Some(format!(
                            "Email send endpoint returned HTTP {} for question {}: {}",
                            status, question.interaction_id, err_body
                        )),
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    channel = "email",
                    interaction_id = %question.interaction_id,
                    error = %e,
                    "Failed to send question email"
                );
                DeliveryResult {
                    channel: "email".into(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "HTTP request to email send endpoint failed for question {}: {}",
                        question.interaction_id, e
                    )),
                }
            }
        }
    }

    async fn validate(&self) -> Result<(), String> {
        if self.config.send_endpoint.is_empty() {
            return Err(
                "Email send_endpoint is empty. Set it in .ta/daemon.toml under [channels.email]"
                    .into(),
            );
        }
        if self.config.from_address.is_empty() {
            return Err(
                "Email from_address is empty. Set it in .ta/daemon.toml under [channels.email]"
                    .into(),
            );
        }
        if self.config.to_address.is_empty() {
            return Err(
                "Email to_address is empty. Set it in .ta/daemon.toml under [channels.email]"
                    .into(),
            );
        }
        if !self.config.from_address.contains('@') {
            return Err(format!(
                "Email from_address '{}' is not a valid email address",
                self.config.from_address
            ));
        }
        if !self.config.to_address.contains('@') {
            return Err(format!(
                "Email to_address '{}' is not a valid email address",
                self.config.to_address
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_config() -> EmailConfig {
        EmailConfig {
            send_endpoint: "https://api.example.com/send".into(),
            api_key: "test-key".into(),
            from_address: "agent@example.com".into(),
            to_address: "human@example.com".into(),
        }
    }

    fn test_question() -> ChannelQuestion {
        ChannelQuestion {
            interaction_id: Uuid::new_v4(),
            goal_id: Uuid::new_v4(),
            question: "Which database should I use?".into(),
            context: Some("Setting up the backend".into()),
            response_hint: "choice".into(),
            choices: vec!["PostgreSQL".into(), "SQLite".into()],
            turn: 1,
            callback_url: "http://localhost:7700".into(),
        }
    }

    #[test]
    fn build_subject_short() {
        let adapter = EmailAdapter::new(test_config());
        let q = test_question();
        let subject = adapter.build_subject(&q);
        assert!(subject.starts_with("[TA]"));
        assert!(subject.contains("turn 1"));
    }

    #[test]
    fn build_subject_long_truncates() {
        let adapter = EmailAdapter::new(test_config());
        let mut q = test_question();
        q.question = "A".repeat(100);
        let subject = adapter.build_subject(&q);
        assert!(subject.contains("..."));
    }

    #[test]
    fn build_body_text_choice() {
        let adapter = EmailAdapter::new(test_config());
        let q = test_question();
        let body = adapter.build_body_text(&q);
        assert!(body.contains("PostgreSQL"));
        assert!(body.contains("SQLite"));
        assert!(body.contains("Interaction ID:"));
    }

    #[test]
    fn build_body_text_freeform() {
        let adapter = EmailAdapter::new(test_config());
        let mut q = test_question();
        q.response_hint = "freeform".into();
        q.choices = vec![];
        let body = adapter.build_body_text(&q);
        assert!(body.contains("Reply to this email"));
    }

    #[test]
    fn build_body_html_has_structure() {
        let adapter = EmailAdapter::new(test_config());
        let q = test_question();
        let html = adapter.build_body_html(&q);
        assert!(html.contains("<h2"));
        assert!(html.contains("PostgreSQL"));
        assert!(html.contains("</div>"));
    }

    #[test]
    fn validate_empty_endpoint() {
        let adapter = EmailAdapter::new(EmailConfig {
            send_endpoint: String::new(),
            api_key: "key".into(),
            from_address: "a@b.com".into(),
            to_address: "c@d.com".into(),
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(adapter.validate());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("send_endpoint"));
    }

    #[test]
    fn validate_bad_email() {
        let adapter = EmailAdapter::new(EmailConfig {
            send_endpoint: "https://api.example.com/send".into(),
            api_key: "key".into(),
            from_address: "not-an-email".into(),
            to_address: "c@d.com".into(),
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(adapter.validate());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a valid email"));
    }

    #[test]
    fn validate_ok() {
        let adapter = EmailAdapter::new(test_config());
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt.block_on(adapter.validate()).is_ok());
    }

    #[test]
    fn adapter_name() {
        let adapter = EmailAdapter::new(test_config());
        assert_eq!(adapter.name(), "email");
    }
}
