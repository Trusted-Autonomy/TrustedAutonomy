// client.rs — HTTP client for OpenAI-compatible chat completions API.
//
// Supports both Ollama (http://localhost:11434) and any other
// OpenAI-compat endpoint (vLLM, llama.cpp server, LM Studio, OpenAI).

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;

/// HTTP client wrapping the OpenAI-compatible `/v1/chat/completions` endpoint.
pub struct OllamaClient {
    client: Client,
    base_url: String,
    model: String,
    temperature: f32,
}

impl OllamaClient {
    pub fn new(base_url: &str, model: &str, temperature: f32) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            temperature,
        })
    }

    /// Returns the base URL (for display in error messages).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// List available models via GET /v1/models.
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/v1/models", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to reach /v1/models")?;

        let body: Value = resp
            .json()
            .await
            .context("Failed to parse /v1/models response")?;

        let models = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }

    /// Send a chat request with tool definitions and return the raw API response.
    pub async fn chat_with_tools(
        &self,
        system_prompt: &str,
        messages: &[Value],
        tools: &[Value],
    ) -> Result<Value> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let mut all_messages = vec![serde_json::json!({
            "role": "system",
            "content": system_prompt
        })];
        all_messages.extend_from_slice(messages);

        let body = serde_json::json!({
            "model": self.model,
            "messages": all_messages,
            "tools": tools,
            "tool_choice": "auto",
            "temperature": self.temperature,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Chat API request failed")?;

        let status = resp.status();
        let text = resp.text().await.context("Failed to read response body")?;

        if !status.is_success() {
            anyhow::bail!(
                "Chat API returned {} — {}\n\
                 Endpoint: {}\n\
                 Model: {}",
                status,
                text,
                url,
                self.model
            );
        }

        serde_json::from_str(&text).context("Failed to parse chat completion response")
    }

    /// Send a simple chat request without tools, returning the assistant text.
    pub async fn chat_simple(&self, system_prompt: &str, user_message: &str) -> Result<String> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_message}
            ],
            "temperature": self.temperature,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Chat API request failed")?;

        let status = resp.status();
        let text = resp.text().await.context("Failed to read response body")?;

        if !status.is_success() {
            anyhow::bail!(
                "Chat API returned {} — {}\nEndpoint: {}\nModel: {}",
                status,
                text,
                url,
                self.model
            );
        }

        let parsed: Value =
            serde_json::from_str(&text).context("Failed to parse chat completion response")?;

        Ok(parsed
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_new_trims_trailing_slash() {
        let c = OllamaClient::new("http://localhost:11434/", "phi4", 0.1).unwrap();
        assert_eq!(c.base_url(), "http://localhost:11434");
    }

    #[test]
    fn client_new_no_trailing_slash() {
        let c = OllamaClient::new("http://localhost:11434", "phi4", 0.1).unwrap();
        assert_eq!(c.base_url(), "http://localhost:11434");
    }
}
