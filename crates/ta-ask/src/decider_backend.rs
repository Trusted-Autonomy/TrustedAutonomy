// crates/ta-ask/src/decider_backend.rs

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{
    DecisionBackend, DecisionError, DecisionRequest, DecisionResponse, DecisionResult,
    DecisionSchema,
};

// NOTE on wire shape (corrected 2026-10-04 against a genuinely running
// decider-4b server -- see crates/ta-ask/README.md / this task's report):
//
// The real `/decide` endpoint (decider's own `decider/serve.py`) does NOT
// take `{"context": ..., "questions": [{"question": ..., "options": [...]}]}`
// and does NOT return a bare `Vec<{choice, confidence, probs}>`. The real
// wire format is:
//
//   request:  {"context": str, "schema": {<question text>: {"type": "bool"}
//                                          | {"type": "choice", "options": [...]}
//                                          | {"type": "scale", "legend": [...]}}}
//   response: {<question text>: {"noul": <0..1>, "type": "noul"}                          -- bool
//                               | {"choice": str, "confidence": <0..1>, "type": "choice",
//                                  "probabilities": {...}}                                 -- choice
//                               | {"score": num, "confidence": <0..1>, "type": "scale",
//                                  "legend": ..., "probabilities": {...}}}                  -- scale (unused here)
//
// i.e. a JSON *object* keyed by the question's own text, not a bare array,
// and a "bool" question's answer carries no "confidence" field at all --
// only a `noul` probability-of-yes. `Score` is rejected before any HTTP
// call is made (see `decide()` below), so the "scale" shape is parsed for
// completeness but never actually exercised.
#[derive(serde::Serialize)]
struct DecideRequestBody {
    context: String,
    schema: std::collections::HashMap<String, QuestionSchema>,
}

#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum QuestionSchema {
    Bool,
    Choice { options: Vec<String> },
}

#[derive(serde::Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
enum DecideAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        confidence: f64,
        #[allow(dead_code)]
        probabilities: std::collections::HashMap<String, f64>,
    },
    #[allow(dead_code)]
    Scale {
        score: f64,
        confidence: f64,
        legend: serde_json::Value,
        probabilities: std::collections::HashMap<String, f64>,
    },
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
        let question_schema = match &req.schema {
            DecisionSchema::YesNo => QuestionSchema::Bool,
            DecisionSchema::Choice(options) => QuestionSchema::Choice {
                options: options.clone(),
            },
            DecisionSchema::Score { .. } => {
                return Err(DecisionError::UnsupportedSchema(req.schema.clone()));
            }
        };

        let mut schema = std::collections::HashMap::with_capacity(1);
        schema.insert(req.question.clone(), question_schema);
        let body = DecideRequestBody {
            context: req.context.clone(),
            schema,
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

        // The server answers with a JSON *object* keyed by the question's
        // own text (see the wire-shape note above `DecideRequestBody`), not
        // a bare array -- we sent exactly one question, so take its one
        // entry.
        let mut answers: std::collections::HashMap<String, DecideAnswer> = response
            .json()
            .map_err(|e| DecisionError::MalformedResponse(e.to_string()))?;

        let answer = answers.remove(&req.question).or_else(|| answers.into_values().next()).ok_or_else(|| {
            DecisionError::MalformedResponse(
                "server returned an empty response object (expected one answer keyed by the question text)"
                    .to_string(),
            )
        })?;

        let (result, confidence) = match answer {
            // A Noul answer carries no `confidence` field of its own (per
            // decider's README: "A Noul answer has no confidence; its noul
            // value is the probability of yes"). We derive one as the
            // probability of whichever outcome we chose, which is always in
            // (0.5, 1.0] and matches this crate's confidence contract.
            DecideAnswer::Noul { noul } => {
                let is_yes = noul > 0.5;
                let confidence = if is_yes { noul } else { 1.0 - noul };
                (DecisionResult::Bool(is_yes), confidence)
            }
            DecideAnswer::Choice {
                choice, confidence, ..
            } => (DecisionResult::Choice(choice), confidence),
            DecideAnswer::Scale { .. } => {
                return Err(DecisionError::MalformedResponse(
                    "server returned a 'scale' answer, but this backend never sends a Score \
                     schema (Score is rejected before any HTTP call) -- this indicates a \
                     request/response mismatch"
                        .to_string(),
                ));
            }
        };

        Ok(DecisionResponse {
            result,
            confidence,
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
        // Real wire shape (verified against a genuinely running decider-4b
        // server): a JSON object keyed by the question text, and a bool
        // question's answer is a bare `noul` probability-of-yes, not a
        // "choice"/"confidence"/"probs" item.
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Does this diff look safe to commit?": {"noul": 0.93, "type": "noul"}
            })))
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
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Which department should handle this?": {
                    "choice": "billing", "confidence": 0.97, "type": "choice",
                    "probabilities": {"billing": 0.97, "technical": 0.02, "sales": 0.01}
                }
            })))
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
    async fn decide_errors_clearly_on_empty_result_object() {
        let server = MockServer::start().await;
        // The real server returns `{}` (an empty object) when it has
        // nothing to score -- not `[]` (see decider/serve.py's /decide
        // route: `if not qs: return {}`).
        Mock::given(method("POST"))
            .and(path("/decide"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
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

    #[test]
    #[ignore = "requires a running decider-4b server at 127.0.0.1:8700 -- see crates/ta-ask/README.md"]
    fn real_decider_backend_answers_a_real_question() {
        let backend = DeciderBackend::connect("http://127.0.0.1:8700", "decider-4b");
        let response = backend
            .decide(&DecisionRequest {
                question: "Does this draft clearly recommend one specific option over the \
                            others, rather than just listing them neutrally?"
                    .to_string(),
                context: "We should use PostgreSQL for this project because it has the best \
                           support for our use case among the three options we considered."
                    .to_string(),
                schema: DecisionSchema::YesNo,
            })
            .expect("real decider-4b server should answer this question");

        println!("REAL DECIDER RESPONSE: {:?}", response);
        assert!(response.confidence > 0.0 && response.confidence <= 1.0);
        assert!(matches!(response.result, DecisionResult::Bool(_)));
    }
}
