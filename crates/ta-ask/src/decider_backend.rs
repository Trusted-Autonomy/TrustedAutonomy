// crates/ta-ask/src/decider_backend.rs

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{
    DecisionBackend, DecisionError, DecisionRequest, DecisionResponse, DecisionResult,
    DecisionSchema,
};

#[derive(serde::Serialize)]
struct DecideRequestBody {
    context: String,
    questions: Vec<QuestionBody>,
}

#[derive(serde::Serialize)]
struct QuestionBody {
    question: String,
    options: Vec<String>,
}

#[derive(serde::Deserialize, Debug)]
struct DecideResponseItem {
    choice: String,
    confidence: f64,
    #[allow(dead_code)]
    probs: std::collections::HashMap<String, f64>,
}

/// Owns the spawned `scripts/serve.sh` child process, if any. `None` when
/// this backend was built via `connect()` against a server it doesn't own
/// (tests, or a caller managing the process itself) -- `Drop` then has
/// nothing to kill.
#[derive(Debug)]
struct ServerProcess {
    child: Child,
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `DecisionBackend` backed by Decider-4b's own HTTP server
/// (github.com/Mapika/decider, `scripts/serve.sh`). Supports `YesNo` and
/// `Choice` schemas; `Score` returns `DecisionError::UnsupportedSchema` --
/// see this crate's `README.md` for why.
#[derive(Debug)]
pub struct DeciderBackend {
    base_url: String,
    client: reqwest::blocking::Client,
    model_id: String,
    /// Held only for its `Drop` side effect (killing the child process on
    /// backend teardown) -- never read otherwise.
    #[allow(dead_code)]
    server: Mutex<Option<ServerProcess>>,
}

impl DeciderBackend {
    fn new_client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("building a reqwest client with static, valid config cannot fail")
    }

    /// Spawn `serve_script <model_id> <port>` (Decider's own
    /// `scripts/serve.sh`) and wait for it to report healthy before
    /// returning. The model takes 13-21s to load per
    /// `docs/superpowers/specs/2026-10-04-local-decision-model-evaluation.md`'s
    /// own measurements, so this polls `GET /health` every 500ms until
    /// `health_timeout` elapses, then gives up with a clear, actionable
    /// error rather than hanging forever.
    pub fn spawn(
        serve_script: &Path,
        model_id: &str,
        port: u16,
        health_timeout: Duration,
    ) -> Result<Self, DecisionError> {
        let child = Command::new(serve_script)
            .arg(model_id)
            .arg(port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| DecisionError::SpawnFailed {
                command: format!("{} {} {}", serve_script.display(), model_id, port),
                source: e,
            })?;

        let base_url = format!("http://127.0.0.1:{port}");
        let client = Self::new_client();
        let health_url = format!("{base_url}/health");
        let deadline = Instant::now() + health_timeout;
        loop {
            if let Ok(resp) = client.get(&health_url).send() {
                if resp.status().is_success() {
                    break;
                }
            }
            if Instant::now() >= deadline {
                return Err(DecisionError::BackendUnavailable {
                    url: health_url,
                    timeout_secs: health_timeout.as_secs(),
                });
            }
            std::thread::sleep(Duration::from_millis(500));
        }

        Ok(Self {
            base_url,
            client,
            model_id: model_id.to_string(),
            server: Mutex::new(Some(ServerProcess { child })),
        })
    }

    /// Point at an already-running server this backend doesn't own --
    /// used by this module's own tests (against a `wiremock` server) and
    /// available to any caller that manages the Decider server process
    /// itself.
    pub fn connect(base_url: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: Self::new_client(),
            model_id: model_id.into(),
            server: Mutex::new(None),
        }
    }
}

impl DecisionBackend for DeciderBackend {
    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        let options = match &req.schema {
            DecisionSchema::YesNo => vec!["yes".to_string(), "no".to_string()],
            DecisionSchema::Choice(options) => options.clone(),
            DecisionSchema::Score { .. } => {
                return Err(DecisionError::UnsupportedSchema(req.schema.clone()));
            }
        };

        let body = DecideRequestBody {
            context: req.context.clone(),
            questions: vec![QuestionBody {
                question: req.question.clone(),
                options,
            }],
        };

        let start = Instant::now();
        let response = self
            .client
            .post(format!("{}/decide", self.base_url))
            .json(&body)
            .send()
            .map_err(DecisionError::RequestFailed)?;
        let latency_ms = start.elapsed().as_millis() as u64;

        if !response.status().is_success() {
            return Err(DecisionError::MalformedResponse(format!(
                "server returned HTTP {}",
                response.status()
            )));
        }

        let items: Vec<DecideResponseItem> = response
            .json()
            .map_err(|e| DecisionError::MalformedResponse(e.to_string()))?;

        let item = items.into_iter().next().ok_or_else(|| {
            DecisionError::MalformedResponse("server returned an empty result array".to_string())
        })?;

        let result = match &req.schema {
            DecisionSchema::YesNo => match item.choice.as_str() {
                "yes" => DecisionResult::Bool(true),
                "no" => DecisionResult::Bool(false),
                other => {
                    return Err(DecisionError::MalformedResponse(format!(
                        "expected 'yes' or 'no' for a YesNo schema, got '{}'",
                        other
                    )));
                }
            },
            DecisionSchema::Choice(_) => DecisionResult::Choice(item.choice),
            DecisionSchema::Score { .. } => unreachable!("Score schema already rejected above"),
        };

        Ok(DecisionResponse {
            result,
            confidence: item.confidence,
            model_id: self.model_id.clone(),
            latency_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DecisionError, DecisionRequest, DecisionResponse, DecisionResult, DecisionSchema};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Builds the `DeciderBackend` and calls `decide()` entirely on a
    /// blocking-pool thread via `spawn_blocking`. Required because
    /// `reqwest::blocking::Client` is documented as unsafe to construct or
    /// use directly on a tokio async-task thread -- building (and later
    /// dropping) it there panics with "Cannot drop a runtime in a context
    /// where blocking is not allowed," since the blocking client owns its
    /// own inner tokio runtime. `spawn_blocking` moves all of that work
    /// (build, request, drop) onto a thread the async executor doesn't
    /// manage cooperatively, which is exactly what the reqwest docs
    /// prescribe for calling the blocking client from async code.
    async fn decide_blocking(
        uri: String,
        model_id: &'static str,
        req: DecisionRequest,
    ) -> Result<DecisionResponse, DecisionError> {
        tokio::task::spawn_blocking(move || {
            let backend = DeciderBackend::connect(uri, model_id);
            backend.decide(&req)
        })
        .await
        .expect("spawn_blocking should not panic")
    }

    #[tokio::test]
    async fn decide_maps_yes_no_schema_and_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"choice": "yes", "confidence": 0.93, "probs": {"yes": 0.93, "no": 0.07}}
            ])))
            .mount(&server)
            .await;

        let response = decide_blocking(
            server.uri(),
            "decider-4b-test",
            DecisionRequest {
                question: "Does this diff look safe to commit?".to_string(),
                context: "3 files changed, 12 lines".to_string(),
                schema: DecisionSchema::YesNo,
            },
        )
        .await
        .expect("decide should succeed against the mocked server");

        assert_eq!(response.result, DecisionResult::Bool(true));
        assert_eq!(response.confidence, 0.93);
        assert_eq!(response.model_id, "decider-4b-test");
    }

    #[tokio::test]
    async fn decide_maps_choice_schema() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"choice": "billing", "confidence": 0.97, "probs": {"billing": 0.97, "technical": 0.02, "sales": 0.01}}
            ])))
            .mount(&server)
            .await;

        let response = decide_blocking(
            server.uri(),
            "decider-4b-test",
            DecisionRequest {
                question: "Which department should handle this?".to_string(),
                context: "customer was charged twice".to_string(),
                schema: DecisionSchema::Choice(vec![
                    "billing".to_string(),
                    "technical".to_string(),
                    "sales".to_string(),
                ]),
            },
        )
        .await
        .expect("decide should succeed against the mocked server");

        assert_eq!(
            response.result,
            DecisionResult::Choice("billing".to_string())
        );
    }

    #[tokio::test]
    async fn decide_rejects_score_schema_without_calling_the_server() {
        // No Mock registered -- if decide() ever made an HTTP call for a
        // Score schema, wiremock would fail this test on the unexpected
        // request, proving the rejection happens before any network call.
        let server = MockServer::start().await;
        let err = decide_blocking(
            server.uri(),
            "decider-4b-test",
            DecisionRequest {
                question: "how risky is this?".to_string(),
                context: "...".to_string(),
                schema: DecisionSchema::Score { min: 0.0, max: 1.0 },
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DecisionError::UnsupportedSchema(_)));
    }

    #[tokio::test]
    async fn decide_errors_clearly_on_malformed_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let err = decide_blocking(
            server.uri(),
            "decider-4b-test",
            DecisionRequest {
                question: "q".to_string(),
                context: "c".to_string(),
                schema: DecisionSchema::YesNo,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DecisionError::MalformedResponse(_)));
    }

    #[tokio::test]
    async fn decide_errors_clearly_on_empty_result_array() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let err = decide_blocking(
            server.uri(),
            "decider-4b-test",
            DecisionRequest {
                question: "q".to_string(),
                context: "c".to_string(),
                schema: DecisionSchema::YesNo,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DecisionError::MalformedResponse(_)));
    }

    #[test]
    fn spawn_errors_clearly_when_the_script_does_not_exist() {
        let err = DeciderBackend::spawn(
            std::path::Path::new("/definitely/not/a/real/script.sh"),
            "decider-4b",
            18237,
            std::time::Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(err, DecisionError::SpawnFailed { .. }));
    }

    #[test]
    fn spawn_errors_clearly_when_health_check_never_succeeds() {
        // A script that starts but never serves /health -- exercises the
        // timeout path without needing the real model or a real server.
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("never_healthy.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        let err = DeciderBackend::spawn(
            &script_path,
            "decider-4b",
            18238,
            std::time::Duration::from_millis(800),
        )
        .unwrap_err();
        assert!(matches!(err, DecisionError::BackendUnavailable { .. }));
    }
}
