// external_channel.rs — Out-of-process channel adapter for plugin-based delivery.
//
// Implements ChannelDelivery for external plugins using two protocols:
//   1. JSON-over-stdio: Spawn plugin process, write ChannelQuestion JSON to
//      stdin, read DeliveryResult JSON line from stdout.
//   2. HTTP callback: POST ChannelQuestion JSON to a configured URL, parse
//      DeliveryResult from the response body.
//
// Both protocols use the same JSON schema (ChannelQuestion / DeliveryResult
// from ta-events), making plugins interchangeable regardless of protocol.

use std::process::Stdio;
use std::time::Duration;

use ta_changeset::plugin::{PluginManifest, PluginProtocol};
use ta_events::channel::{ChannelDelivery, ChannelQuestion, DeliveryResult};

/// External channel adapter that delegates delivery to an out-of-process plugin.
///
/// Plugins can be written in any language. TA communicates with them using
/// either JSON-over-stdio (subprocess) or HTTP callback.
pub struct ExternalChannelAdapter {
    manifest: PluginManifest,
    /// Resolved auth token value (if auth_token_env was set).
    auth_token: Option<String>,
    /// HTTP client (reused across requests for connection pooling).
    http_client: reqwest::Client,
}

impl ExternalChannelAdapter {
    /// Create an adapter from a plugin manifest.
    ///
    /// Resolves auth_token_env to the actual token value from the environment.
    pub fn from_manifest(manifest: PluginManifest) -> Self {
        let auth_token = manifest
            .auth_token_env
            .as_ref()
            .and_then(|env_var| std::env::var(env_var).ok());

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(manifest.timeout_secs))
            .build()
            .unwrap_or_default();

        Self {
            manifest,
            auth_token,
            http_client,
        }
    }

    /// Deliver via JSON-over-stdio subprocess protocol.
    async fn deliver_stdio(&self, question: &ChannelQuestion) -> DeliveryResult {
        let command = match &self.manifest.command {
            Some(cmd) => cmd,
            None => {
                return DeliveryResult {
                    channel: self.manifest.name.clone(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "Plugin '{}' has no command configured for json-stdio protocol. \
                         Set 'command' in channel.toml.",
                        self.manifest.name
                    )),
                };
            }
        };

        let question_json = match serde_json::to_string(question) {
            Ok(json) => json,
            Err(e) => {
                return DeliveryResult {
                    channel: self.manifest.name.clone(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "Failed to serialize ChannelQuestion for plugin '{}': {}",
                        self.manifest.name, e
                    )),
                };
            }
        };

        // Split command into program + initial args (handles "python3 script.py" style).
        let mut parts = command.split_whitespace();
        let program = match parts.next() {
            Some(p) => p,
            None => {
                return DeliveryResult {
                    channel: self.manifest.name.clone(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "Plugin '{}' has empty command. Set 'command' in channel.toml.",
                        self.manifest.name
                    )),
                };
            }
        };

        let mut cmd = tokio::process::Command::new(program);
        // Add any remaining parts of the command string as args.
        for arg in parts {
            cmd.arg(arg);
        }
        // Add explicit args from manifest.
        for arg in &self.manifest.args {
            cmd.arg(arg);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let timeout = Duration::from_secs(self.manifest.timeout_secs);

        // Spawn the plugin process.
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return DeliveryResult {
                    channel: self.manifest.name.clone(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "Failed to spawn plugin '{}' (command: '{}'): {}. \
                         Ensure the command is installed and on PATH.",
                        self.manifest.name, command, e
                    )),
                };
            }
        };

        // Write question JSON to stdin.
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let write_result = tokio::time::timeout(timeout, async {
                stdin.write_all(question_json.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                // Drop stdin to signal EOF.
                drop(stdin);
                Ok::<(), std::io::Error>(())
            })
            .await;

            match write_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = child.kill().await;
                    return DeliveryResult {
                        channel: self.manifest.name.clone(),
                        delivery_id: String::new(),
                        success: false,
                        error: Some(format!(
                            "Failed to write to plugin '{}' stdin: {}",
                            self.manifest.name, e
                        )),
                    };
                }
                Err(_) => {
                    let _ = child.kill().await;
                    return DeliveryResult {
                        channel: self.manifest.name.clone(),
                        delivery_id: String::new(),
                        success: false,
                        error: Some(format!(
                            "Timeout writing to plugin '{}' stdin after {}s. \
                             Increase timeout_secs in channel.toml if the plugin needs more time.",
                            self.manifest.name, self.manifest.timeout_secs
                        )),
                    };
                }
            }
        }

        // Wait for the process to complete with timeout.
        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return DeliveryResult {
                    channel: self.manifest.name.clone(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "Plugin '{}' process error: {}",
                        self.manifest.name, e
                    )),
                };
            }
            Err(_) => {
                return DeliveryResult {
                    channel: self.manifest.name.clone(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "Plugin '{}' timed out after {}s. The process was killed. \
                         Increase timeout_secs in channel.toml if the plugin needs more time.",
                        self.manifest.name, self.manifest.timeout_secs
                    )),
                };
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return DeliveryResult {
                channel: self.manifest.name.clone(),
                delivery_id: String::new(),
                success: false,
                error: Some(format!(
                    "Plugin '{}' exited with status {}. stderr: {}",
                    self.manifest.name,
                    output.status,
                    stderr.trim()
                )),
            };
        }

        // Parse the first line of stdout as DeliveryResult JSON.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let first_line = stdout.lines().next().unwrap_or("").trim();

        if first_line.is_empty() {
            return DeliveryResult {
                channel: self.manifest.name.clone(),
                delivery_id: String::new(),
                success: false,
                error: Some(format!(
                    "Plugin '{}' produced no output on stdout. \
                     Expected a JSON line with DeliveryResult.",
                    self.manifest.name
                )),
            };
        }

        match serde_json::from_str::<DeliveryResult>(first_line) {
            Ok(result) => result,
            Err(e) => DeliveryResult {
                channel: self.manifest.name.clone(),
                delivery_id: String::new(),
                success: false,
                error: Some(format!(
                    "Plugin '{}' produced invalid JSON on stdout: {}. Got: '{}'",
                    self.manifest.name,
                    e,
                    if first_line.len() > 200 {
                        &first_line[..200]
                    } else {
                        first_line
                    }
                )),
            },
        }
    }

    /// Deliver via HTTP callback protocol.
    async fn deliver_http(&self, question: &ChannelQuestion) -> DeliveryResult {
        let url = match &self.manifest.deliver_url {
            Some(url) => url,
            None => {
                return DeliveryResult {
                    channel: self.manifest.name.clone(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "Plugin '{}' has no deliver_url configured for http protocol. \
                         Set 'deliver_url' in channel.toml.",
                        self.manifest.name
                    )),
                };
            }
        };

        let mut request = self.http_client.post(url).json(question);

        // Add auth token header if configured.
        if let Some(ref token) = self.auth_token {
            request = request.bearer_auth(token);
        }

        match request.send().await {
            Ok(response) => {
                if !response.status().is_success() {
                    let status = response.status();
                    let body = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "<no body>".to_string());
                    return DeliveryResult {
                        channel: self.manifest.name.clone(),
                        delivery_id: String::new(),
                        success: false,
                        error: Some(format!(
                            "Plugin '{}' HTTP endpoint returned {}: {}",
                            self.manifest.name,
                            status,
                            if body.len() > 200 {
                                &body[..200]
                            } else {
                                &body
                            }
                        )),
                    };
                }

                match response.json::<DeliveryResult>().await {
                    Ok(result) => result,
                    Err(e) => DeliveryResult {
                        channel: self.manifest.name.clone(),
                        delivery_id: String::new(),
                        success: false,
                        error: Some(format!(
                            "Plugin '{}' HTTP endpoint returned invalid JSON: {}",
                            self.manifest.name, e
                        )),
                    },
                }
            }
            Err(e) => {
                let hint = if e.is_timeout() {
                    format!(
                        " Timed out after {}s — increase timeout_secs in channel.toml.",
                        self.manifest.timeout_secs
                    )
                } else if e.is_connect() {
                    format!(
                        " Could not connect to '{}' — verify the URL is correct and the service is running.",
                        url
                    )
                } else {
                    String::new()
                };

                DeliveryResult {
                    channel: self.manifest.name.clone(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "Plugin '{}' HTTP request failed: {}.{}",
                        self.manifest.name, e, hint
                    )),
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl ChannelDelivery for ExternalChannelAdapter {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    async fn deliver_question(&self, question: &ChannelQuestion) -> DeliveryResult {
        tracing::info!(
            plugin = %self.manifest.name,
            protocol = %self.manifest.protocol,
            interaction_id = %question.interaction_id,
            "Delivering question via external channel plugin"
        );

        match self.manifest.protocol {
            PluginProtocol::JsonStdio => self.deliver_stdio(question).await,
            PluginProtocol::Http => self.deliver_http(question).await,
        }
    }

    async fn validate(&self) -> Result<(), String> {
        match self.manifest.protocol {
            PluginProtocol::JsonStdio => {
                // Check that the command exists.
                let command = self.manifest.command.as_deref().ok_or_else(|| {
                    format!("Plugin '{}' has no command configured", self.manifest.name)
                })?;
                let program = command
                    .split_whitespace()
                    .next()
                    .ok_or_else(|| format!("Plugin '{}' has empty command", self.manifest.name))?;

                // Try to find the program on PATH.
                match which::which(program) {
                    Ok(path) => {
                        tracing::debug!(
                            plugin = %self.manifest.name,
                            command = %program,
                            resolved = %path.display(),
                            "Plugin command found"
                        );
                        Ok(())
                    }
                    Err(_) => Err(format!(
                        "Plugin '{}' command '{}' not found on PATH. \
                         Install it or set the full path in channel.toml.",
                        self.manifest.name, program
                    )),
                }
            }
            PluginProtocol::Http => {
                let url = self.manifest.deliver_url.as_deref().ok_or_else(|| {
                    format!(
                        "Plugin '{}' has no deliver_url configured",
                        self.manifest.name
                    )
                })?;

                // Validate URL format.
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return Err(format!(
                        "Plugin '{}' deliver_url must start with http:// or https://: '{}'",
                        self.manifest.name, url
                    ));
                }

                // Check auth token if required.
                if let Some(ref env_var) = self.manifest.auth_token_env {
                    if self.auth_token.is_none() {
                        return Err(format!(
                            "Plugin '{}' requires env var {} for authentication, but it is not set",
                            self.manifest.name, env_var
                        ));
                    }
                }

                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_question() -> ChannelQuestion {
        ChannelQuestion {
            interaction_id: Uuid::new_v4(),
            goal_id: Uuid::new_v4(),
            question: "Test question".into(),
            context: None,
            response_hint: "freeform".into(),
            choices: vec![],
            turn: 1,
            callback_url: "http://localhost:7700".into(),
        }
    }

    #[test]
    fn adapter_name_matches_manifest() {
        let manifest = PluginManifest {
            name: "test-plugin".into(),
            version: "0.1.0".into(),
            command: Some("echo".into()),
            args: vec![],
            protocol: PluginProtocol::JsonStdio,
            deliver_url: None,
            auth_token_env: None,
            capabilities: vec![],
            description: None,
            timeout_secs: 30,
        };
        let adapter = ExternalChannelAdapter::from_manifest(manifest);
        assert_eq!(adapter.name(), "test-plugin");
    }

    #[tokio::test]
    async fn stdio_delivery_with_echo() {
        // Use `echo` to simulate a plugin that outputs valid JSON.
        let json_output = r#"{"channel":"test","delivery_id":"msg-1","success":true,"error":null}"#;
        let manifest = PluginManifest {
            name: "echo-test".into(),
            version: "0.1.0".into(),
            command: Some(format!("echo {}", json_output)),
            args: vec![],
            protocol: PluginProtocol::JsonStdio,
            deliver_url: None,
            auth_token_env: None,
            capabilities: vec![],
            description: None,
            timeout_secs: 5,
        };
        // echo doesn't actually read stdin, but the adapter should handle that.
        // However echo "..." as a command won't work well via split_whitespace.
        // Let's use a simpler approach with sh -c.
        let manifest = PluginManifest {
            command: Some("sh".into()),
            args: vec!["-c".into(), format!("echo '{}'", json_output)],
            ..manifest
        };

        let adapter = ExternalChannelAdapter::from_manifest(manifest);
        let result = adapter.deliver_question(&test_question()).await;
        assert!(result.success, "Expected success, got: {:?}", result.error);
        assert_eq!(result.channel, "test");
        assert_eq!(result.delivery_id, "msg-1");
    }

    #[tokio::test]
    async fn stdio_delivery_command_not_found() {
        let manifest = PluginManifest {
            name: "missing-cmd".into(),
            version: "0.1.0".into(),
            command: Some("nonexistent-binary-12345".into()),
            args: vec![],
            protocol: PluginProtocol::JsonStdio,
            deliver_url: None,
            auth_token_env: None,
            capabilities: vec![],
            description: None,
            timeout_secs: 5,
        };

        let adapter = ExternalChannelAdapter::from_manifest(manifest);
        let result = adapter.deliver_question(&test_question()).await;
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Failed to spawn"));
    }

    #[tokio::test]
    async fn stdio_delivery_non_zero_exit() {
        let manifest = PluginManifest {
            name: "fail-plugin".into(),
            version: "0.1.0".into(),
            command: Some("sh".into()),
            args: vec!["-c".into(), "echo 'error message' >&2; exit 1".into()],
            protocol: PluginProtocol::JsonStdio,
            deliver_url: None,
            auth_token_env: None,
            capabilities: vec![],
            description: None,
            timeout_secs: 5,
        };

        let adapter = ExternalChannelAdapter::from_manifest(manifest);
        let result = adapter.deliver_question(&test_question()).await;
        assert!(!result.success);
        assert!(result
            .error
            .as_ref()
            .unwrap()
            .contains("exited with status"));
        assert!(result.error.as_ref().unwrap().contains("error message"));
    }

    #[tokio::test]
    async fn stdio_delivery_invalid_json_output() {
        let manifest = PluginManifest {
            name: "bad-json".into(),
            version: "0.1.0".into(),
            command: Some("echo".into()),
            args: vec!["not-json".into()],
            protocol: PluginProtocol::JsonStdio,
            deliver_url: None,
            auth_token_env: None,
            capabilities: vec![],
            description: None,
            timeout_secs: 5,
        };

        let adapter = ExternalChannelAdapter::from_manifest(manifest);
        let result = adapter.deliver_question(&test_question()).await;
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("invalid JSON"));
    }

    #[tokio::test]
    async fn stdio_delivery_no_command() {
        let manifest = PluginManifest {
            name: "no-cmd".into(),
            version: "0.1.0".into(),
            command: None,
            args: vec![],
            protocol: PluginProtocol::JsonStdio,
            deliver_url: None,
            auth_token_env: None,
            capabilities: vec![],
            description: None,
            timeout_secs: 5,
        };

        let adapter = ExternalChannelAdapter::from_manifest(manifest);
        let result = adapter.deliver_question(&test_question()).await;
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("no command"));
    }

    #[tokio::test]
    async fn http_delivery_no_url() {
        let manifest = PluginManifest {
            name: "no-url".into(),
            version: "0.1.0".into(),
            command: None,
            args: vec![],
            protocol: PluginProtocol::Http,
            deliver_url: None,
            auth_token_env: None,
            capabilities: vec![],
            description: None,
            timeout_secs: 5,
        };

        let adapter = ExternalChannelAdapter::from_manifest(manifest);
        let result = adapter.deliver_question(&test_question()).await;
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("no deliver_url"));
    }

    #[tokio::test]
    async fn validate_stdio_missing_command() {
        let manifest = PluginManifest {
            name: "no-cmd".into(),
            version: "0.1.0".into(),
            command: None,
            args: vec![],
            protocol: PluginProtocol::JsonStdio,
            deliver_url: None,
            auth_token_env: None,
            capabilities: vec![],
            description: None,
            timeout_secs: 5,
        };

        let adapter = ExternalChannelAdapter::from_manifest(manifest);
        let result = adapter.validate().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn validate_http_bad_url() {
        let manifest = PluginManifest {
            name: "bad-url".into(),
            version: "0.1.0".into(),
            command: None,
            args: vec![],
            protocol: PluginProtocol::Http,
            deliver_url: Some("ftp://bad-scheme.com".into()),
            auth_token_env: None,
            capabilities: vec![],
            description: None,
            timeout_secs: 5,
        };

        let adapter = ExternalChannelAdapter::from_manifest(manifest);
        let result = adapter.validate().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("http://"));
    }
}
