// client.rs — thin, single-attempt REST client for Wayfinder's task API.
// Deliberately synchronous (`reqwest::blocking`), matching `PlanStore`'s own
// deliberate synchronicity (see `ta-plan/src/store.rs`'s module doc). Every
// method makes exactly one HTTP request and returns; retry/backoff is
// `store.rs`'s job (via the local outbox), not this client's — mixing the
// two would make it impossible to tell "the network is down" from "we're
// deliberately waiting" from inside a single blocking call.
//
// Only the task API is used, not the goal API: Wayfinder's `Goal` has no
// settable `status` (`UpdateGoalRequest` covers name/description/kind/
// target_date only — status is presumably derived, not a client-settable
// field), so both TA's `PlanPhase` (as synthetic "gate" tasks) and TA's
// `GoalRun` (as real work tasks) map onto Wayfinder *tasks*, distinguished
// only by their `external_id` prefix (see `mapping.rs`). This matches the
// design doc's own risk note: phase ordering is faked entirely via
// `task_dependency` edges, since Wayfinder's `Goal` has no grouping/
// ordering layer above it.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::WayfinderPlanConfig;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Valid `task.status` values, per `wayfinder-core`'s `TaskStatus` — kept
/// here as a closed set rather than accepting an arbitrary string, so a
/// typo in a call site fails at the call site, not as an opaque 400 from
/// Wayfinder.
pub const STATUS_OPEN: &str = "open";
pub const STATUS_IN_PROGRESS: &str = "in_progress";
pub const STATUS_DONE: &str = "done";
pub const STATUS_CANCELLED: &str = "cancelled";
pub const STATUS_ON_HOLD: &str = "on_hold";

/// Only the fields this crate actually reads. Wayfinder's real `TaskDto`
/// carries more (`title`, `description`, `verb`, ...) — extra JSON fields
/// are silently ignored on deserialize, so there's no need to declare
/// fields nothing in this crate consumes.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskDto {
    pub id: String,
    pub status: String,
    pub hold_reason: Option<String>,
    pub external_id: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateTaskRequest {
    pub title: String,
    pub description: Option<String>,
    pub verb: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct UpdateTaskStatusRequest<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hold_reason: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
struct AddDependencyRequest<'a> {
    depends_on_id: &'a str,
}

/// Wayfinder's real export response carries goals/team_roles/kpis too;
/// `bootstrap_export` (the only caller) only needs the tasks, so only
/// `tasks` is declared here — same "extra JSON fields are ignored"
/// reasoning as `TaskDto` above.
#[derive(Debug, Clone, Deserialize)]
pub struct ExportResponse {
    pub tasks: Vec<TaskDto>,
}

/// Distinguishes "the credential is dead" from "the credential is fine but
/// lacks the role for this specific call" — callers (`store.rs`) treat
/// these very differently: the former means the whole integration is
/// broken and should surface loudly; the latter is expected for a
/// `member`-role token calling the owner-gated export endpoint.
#[derive(Debug, thiserror::Error)]
pub enum WayfinderClientError {
    #[error(
        "Wayfinder rejected the service-account token as unknown or revoked. Check it hasn't \
         been revoked in Wayfinder's Settings, or re-issue and update it with `ta credential add`."
    )]
    Unauthorized,
    #[error(
        "Wayfinder refused this operation for the current service-account role ({context}). This \
         is expected if the token is `member`-role and the operation requires `owner` (e.g. the \
         bulk export endpoint) — grant a higher role in Wayfinder's Settings if this operation is \
         actually needed."
    )]
    Forbidden { context: &'static str },
    #[error("Wayfinder returned an unexpected status {status} for {context}")]
    UnexpectedStatus {
        status: reqwest::StatusCode,
        context: &'static str,
    },
    #[error("request to Wayfinder failed while {context}: {source}")]
    Request {
        context: &'static str,
        #[source]
        source: reqwest::Error,
    },
}

pub struct WayfinderClient {
    http: reqwest::blocking::Client,
    base_url: Url,
    // `org_id` is deliberately not carried here: every route this client
    // calls is scoped by `project_id` alone (the gateway derives `org_id`
    // server-side from the project, never from client input — see
    // `wayfinder`'s threat-model.md §1), so there is no request that would
    // ever use it. `WayfinderPlanConfig` still validates it's non-empty, as
    // a sanity check that the config was filled in deliberately.
    project_id: String,
    secret: crate::secret::RedactedSecret,
}

impl WayfinderClient {
    pub fn new(config: &WayfinderPlanConfig) -> anyhow::Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("failed to build the Wayfinder HTTP client")?;
        Ok(Self {
            http,
            base_url: config.base_url.clone(),
            project_id: config.project_id.clone(),
            secret: config.secret.clone(),
        })
    }

    /// Builds `<base_url>/api/projects/<project_id>/<segments...>`, letting
    /// `Url`'s own `path_segments_mut` percent-encode each segment — unlike
    /// `format!`+`join`, this can't be corrupted by a `project_id` or task
    /// id containing a `?`/`#`/`/` (`org_id`/`project_id` come from user
    /// config, task ids come from Wayfinder responses; neither is a string
    /// this crate should trust enough to interpolate raw into a URL).
    fn project_url(&self, segments: &[&str]) -> Url {
        let mut url = self.base_url.clone();
        {
            // `base_url` is validated as an absolute http(s) URL at
            // `WayfinderPlanConfig::load` time, so `path_segments_mut`
            // cannot fail here (it only fails for schemes like `data:`
            // that have no path — never the case after that validation).
            let mut path = url
                .path_segments_mut()
                .expect("base_url is validated to be a path-having http(s) URL");
            path.push("api").push("projects").push(&self.project_id);
            for segment in segments {
                path.push(segment);
            }
        }
        url
    }

    /// `GET /api/projects/:id/tasks[?updated_since=...]`. `None` fetches
    /// everything (used only by the explicit `bootstrap`-style callers, not
    /// by ordinary sync — see `store.rs`).
    pub fn list_tasks(
        &self,
        updated_since: Option<&str>,
    ) -> Result<Vec<TaskDto>, WayfinderClientError> {
        let mut url = self.project_url(&["tasks"]);
        if let Some(since) = updated_since {
            url.query_pairs_mut().append_pair("updated_since", since);
        }
        let response = self
            .http
            .get(url)
            .bearer_auth(self.secret.expose_secret())
            .send()
            .map_err(|source| WayfinderClientError::Request {
                context: "listing tasks",
                source,
            })?;
        parse_json_response(response, "listing tasks")
    }

    /// `POST /api/projects/:id/tasks` — idempotent upsert by `external_id`.
    pub fn upsert_task(&self, req: &CreateTaskRequest) -> Result<TaskDto, WayfinderClientError> {
        let response = self
            .http
            .post(self.project_url(&["tasks"]))
            .bearer_auth(self.secret.expose_secret())
            .json(req)
            .send()
            .map_err(|source| WayfinderClientError::Request {
                context: "creating/updating a task",
                source,
            })?;
        parse_json_response(response, "creating/updating a task")
    }

    /// `PATCH /api/projects/:id/tasks/:task_id/status`. `task_id` is
    /// Wayfinder's own id (from a prior `upsert_task`/`list_tasks` call),
    /// never the `external_id`.
    pub fn update_task_status(
        &self,
        task_id: &str,
        status: &str,
        hold_reason: Option<&str>,
    ) -> Result<(), WayfinderClientError> {
        let response = self
            .http
            .patch(self.project_url(&["tasks", task_id, "status"]))
            .bearer_auth(self.secret.expose_secret())
            .json(&UpdateTaskStatusRequest {
                status,
                hold_reason,
            })
            .send()
            .map_err(|source| WayfinderClientError::Request {
                context: "updating task status",
                source,
            })?;
        expect_success(response, "updating task status")
    }

    /// `POST /api/projects/:id/tasks/:task_id/dependencies` — declares that
    /// `task_id` depends on `depends_on_id` (both Wayfinder task ids).
    pub fn add_dependency(
        &self,
        task_id: &str,
        depends_on_id: &str,
    ) -> Result<(), WayfinderClientError> {
        let response = self
            .http
            .post(self.project_url(&["tasks", task_id, "dependencies"]))
            .bearer_auth(self.secret.expose_secret())
            .json(&AddDependencyRequest { depends_on_id })
            .send()
            .map_err(|source| WayfinderClientError::Request {
                context: "adding a task dependency",
                source,
            })?;
        // Already-satisfied dependency edges are not re-added by callers in
        // this crate (`mapping.rs` only wires a phase's gate task to its
        // immediate predecessor once), so any successful status here is
        // treated as success; a 409 (already exists) is treated the same
        // way as 2xx, since the end state either way is "the edge exists".
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Ok(());
        }
        expect_success(response, "adding a task dependency")
    }

    /// `GET /api/projects/:id/export` — owner-role-gated bulk snapshot.
    /// Never called by any `PlanStore` trait method; only by an explicit,
    /// separately-invoked bootstrap path (see PLAN.md v0.17.11.3 item 9).
    pub fn export(&self) -> Result<ExportResponse, WayfinderClientError> {
        let response = self
            .http
            .get(self.project_url(&["export"]))
            .bearer_auth(self.secret.expose_secret())
            .send()
            .map_err(|source| WayfinderClientError::Request {
                context: "bootstrapping via export",
                source,
            })?;
        parse_json_response(response, "bootstrapping via export")
    }
}

fn expect_success(
    response: reqwest::blocking::Response,
    context: &'static str,
) -> Result<(), WayfinderClientError> {
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(WayfinderClientError::Unauthorized);
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(WayfinderClientError::Forbidden { context });
    }
    if !status.is_success() {
        return Err(WayfinderClientError::UnexpectedStatus { status, context });
    }
    Ok(())
}

fn parse_json_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::blocking::Response,
    context: &'static str,
) -> Result<T, WayfinderClientError> {
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(WayfinderClientError::Unauthorized);
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(WayfinderClientError::Forbidden { context });
    }
    if !status.is_success() {
        return Err(WayfinderClientError::UnexpectedStatus { status, context });
    }
    response
        .json()
        .map_err(|source| WayfinderClientError::Request { context, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::RedactedSecret;
    use crate::test_support::BlockingMockServer;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, ResponseTemplate};

    fn client_for(mock: &BlockingMockServer) -> WayfinderClient {
        let config = WayfinderPlanConfig {
            base_url: Url::parse(mock.uri()).unwrap(),
            org_id: "org-1".to_string(),
            project_id: "proj-1".to_string(),
            secret: RedactedSecret::new("wfsa_test_secret".to_string()),
        };
        WayfinderClient::new(&config).unwrap()
    }

    #[test]
    fn list_tasks_sends_bearer_auth_and_parses_the_response() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/projects/proj-1/tasks"))
                .and(header("authorization", "Bearer wfsa_test_secret"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {
                        "id": "task-1",
                        "title": "Phase gate",
                        "description": null,
                        "verb": "gate",
                        "status": "open",
                        "hold_reason": null,
                        "external_id": "ta-phase-gate:v0.1.0",
                        "updated_at": "1000000000"
                    }
                ])))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        let tasks = client.list_tasks(None).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].external_id.as_deref(),
            Some("ta-phase-gate:v0.1.0")
        );
    }

    #[test]
    fn list_tasks_forwards_updated_since_as_a_query_param() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/projects/proj-1/tasks"))
                .and(query_param("updated_since", "1700000000"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        let tasks = client.list_tasks(Some("1700000000")).unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn upsert_task_posts_the_request_body() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("POST"))
                .and(path("/api/projects/proj-1/tasks"))
                .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "id": "task-1",
                    "title": "New task",
                    "description": null,
                    "verb": "implement",
                    "status": "open",
                    "hold_reason": null,
                    "external_id": "ta-goal:abc",
                    "updated_at": "1000000000"
                })))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        let task = client
            .upsert_task(&CreateTaskRequest {
                title: "New task".to_string(),
                description: None,
                verb: "implement".to_string(),
                external_id: Some("ta-goal:abc".to_string()),
            })
            .unwrap();
        assert_eq!(task.id, "task-1");
    }

    #[test]
    fn update_task_status_patches_the_status_endpoint() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("PATCH"))
                .and(path("/api/projects/proj-1/tasks/task-1/status"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "task-1",
                    "title": "t",
                    "description": null,
                    "verb": "implement",
                    "status": "in_progress",
                    "hold_reason": null,
                    "external_id": null,
                    "updated_at": "1000000000"
                })))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        client
            .update_task_status("task-1", STATUS_IN_PROGRESS, None)
            .unwrap();
    }

    #[test]
    fn a_task_id_containing_a_slash_does_not_escape_its_path_segment() {
        // `task_id` comes from Wayfinder's own response, but this proves
        // the client can't be tricked into hitting a different route even
        // if it ever did contain a separator -- `path_segments_mut`
        // percent-encodes it rather than splicing it in raw.
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("PATCH"))
                .and(path("/api/projects/proj-1/tasks/a%2Fb/status"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "a/b",
                    "title": "t",
                    "description": null,
                    "verb": "implement",
                    "status": "done",
                    "hold_reason": null,
                    "external_id": null,
                    "updated_at": "1000000000"
                })))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        client.update_task_status("a/b", STATUS_DONE, None).unwrap();
    }

    #[test]
    fn add_dependency_treats_conflict_as_success() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("POST"))
                .and(path("/api/projects/proj-1/tasks/task-2/dependencies"))
                .respond_with(ResponseTemplate::new(409))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        client.add_dependency("task-2", "task-1").unwrap();
    }

    #[test]
    fn unauthorized_response_is_a_distinct_error_from_forbidden() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/projects/proj-1/tasks"))
                .respond_with(ResponseTemplate::new(401))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        let err = client.list_tasks(None).unwrap_err();
        assert!(matches!(err, WayfinderClientError::Unauthorized));
    }

    #[test]
    fn forbidden_export_response_names_the_operation_in_the_error() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/projects/proj-1/export"))
                .respond_with(ResponseTemplate::new(403))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        let err = client.export().unwrap_err();
        assert!(matches!(err, WayfinderClientError::Forbidden { .. }));
        assert!(err.to_string().contains("owner"));
    }

    #[test]
    fn an_unexpected_status_is_surfaced_with_the_status_code() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/projects/proj-1/tasks"))
                .respond_with(ResponseTemplate::new(500))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        let err = client.list_tasks(None).unwrap_err();
        assert!(matches!(
            err,
            WayfinderClientError::UnexpectedStatus { status, .. } if status == 500
        ));
    }

    #[test]
    fn error_messages_never_contain_the_bearer_secret() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/projects/proj-1/tasks"))
                .respond_with(ResponseTemplate::new(401))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        let err = client.list_tasks(None).unwrap_err();
        assert!(!err.to_string().contains("wfsa_test_secret"));
    }
}
