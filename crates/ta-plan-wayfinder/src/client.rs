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
    /// Wayfinder roster/team-role id (not a display name). Omitted from the
    /// body when `None`, which on an upsert leaves any stored assignee
    /// unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct UpdateTaskStatusRequest<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hold_reason: Option<&'a str>,
}

/// Body for `PATCH /api/projects/:id/tasks/:task_id/assignee`. `assignee_id`
/// is always serialized (no `skip_serializing_if`): an explicit JSON `null`
/// is how Wayfinder is told to clear the assignee.
#[derive(Debug, Clone, Serialize)]
struct UpdateTaskAssigneeRequest<'a> {
    assignee_id: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
struct AddDependencyRequest<'a> {
    depends_on_id: &'a str,
}

/// Both fields are PATCH-optional, matching `wayfinder-api`'s
/// `UpdateTaskContentRequest`: an omitted field leaves the stored value
/// unchanged.
#[derive(Debug, Clone, Serialize)]
struct UpdateTaskContentRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
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

/// Failure of a read-only `get_*_json` call. Each variant names the route
/// (path and query only, never the token) and what to do about it; 401, 403,
/// 404 and timeouts stay distinct so a caller can tell a dead credential from
/// a missing role, a wrong id, and a slow server.
#[derive(Debug, thiserror::Error)]
pub enum WayfinderReadError {
    #[error(
        "Wayfinder returned 401 Unauthorized for GET {route}: the service-account token is \
         unknown or revoked. Re-issue it in Wayfinder's Settings and store it again with \
         `ta credential add`."
    )]
    Unauthorized { route: String },
    #[error(
        "Wayfinder returned 403 Forbidden for GET {route}: the service account lacks \
         `project:read` on this project, or the project is outside its org. Grant the role in \
         Wayfinder's Settings, or check `project_id` and `org_id` under [plan.wayfinder] in \
         .ta/workflow.toml."
    )]
    Forbidden { route: String },
    #[error(
        "Wayfinder returned 404 Not Found for GET {route}: the id in that route does not exist \
         in this project (or the project id is wrong). List first to find a valid id, and check \
         `project_id` under [plan.wayfinder] in .ta/workflow.toml."
    )]
    NotFound { route: String },
    #[error(
        "Wayfinder returned unexpected status {status} for GET {route}. Check that Wayfinder is \
         healthy and that `base_url` under [plan.wayfinder] in .ta/workflow.toml points at it."
    )]
    UnexpectedStatus {
        route: String,
        status: reqwest::StatusCode,
    },
    #[error(
        "GET {route} to Wayfinder timed out after {timeout:?}. Wayfinder may be down or \
         overloaded: retry shortly, and check that `base_url` under [plan.wayfinder] in \
         .ta/workflow.toml is reachable."
    )]
    Timeout { route: String, timeout: Duration },
    #[error(
        "request GET {route} to Wayfinder failed: {message}. Check that `base_url` under \
         [plan.wayfinder] in .ta/workflow.toml is correct and reachable."
    )]
    Request { route: String, message: String },
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
    // Carried only for the org-scoped read route `get_org_json` (the KPI
    // catalogue has no project-scoped REST route). The plan-sync methods
    // above never use it.
    org_id: String,
    secret: crate::secret::RedactedSecret,
    timeout: Duration,
}

impl WayfinderClient {
    pub fn new(config: &WayfinderPlanConfig) -> anyhow::Result<Self> {
        Self::new_with_timeout(config, REQUEST_TIMEOUT)
    }

    /// `new` with an explicit per-request timeout (the read tools report it
    /// in their timeout error; tests shorten it).
    pub fn new_with_timeout(
        config: &WayfinderPlanConfig,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .context("failed to build the Wayfinder HTTP client")?;
        Ok(Self {
            http,
            base_url: config.base_url.clone(),
            project_id: config.project_id.clone(),
            org_id: config.org_id.clone(),
            secret: config.secret.clone(),
            timeout,
        })
    }

    /// The project every `get_project_json` call is scoped to.
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// `GET /api/projects/:project_id/<segments...>[?query]`, returning the
    /// JSON body. Read-only and single-attempt. The project id is fixed at
    /// construction; a caller cannot name another one.
    pub fn get_project_json(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> Result<serde_json::Value, WayfinderReadError> {
        let mut url = self.project_url(segments);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        self.get_json(url)
    }

    /// `GET /api/orgs/:org_id/<segments...>[?query]`. Only for data that has
    /// no project-scoped route (the KPI catalogue); callers must filter the
    /// result to their own project.
    pub fn get_org_json(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> Result<serde_json::Value, WayfinderReadError> {
        let mut url = self.base_url.clone();
        {
            let mut path = url
                .path_segments_mut()
                .expect("base_url is validated to be a path-having http(s) URL");
            path.push("api").push("orgs").push(&self.org_id);
            for segment in segments {
                path.push(segment);
            }
        }
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        self.get_json(url)
    }

    fn get_json(&self, url: Url) -> Result<serde_json::Value, WayfinderReadError> {
        let route = match url.query() {
            Some(q) => format!("{}?{}", url.path(), q),
            None => url.path().to_string(),
        };
        let response = self
            .http
            .get(url)
            .bearer_auth(self.secret.expose_secret())
            .send()
            .map_err(|source| {
                if source.is_timeout() {
                    WayfinderReadError::Timeout {
                        route: route.clone(),
                        timeout: self.timeout,
                    }
                } else {
                    WayfinderReadError::Request {
                        route: route.clone(),
                        message: source.without_url().to_string(),
                    }
                }
            })?;
        let status = response.status();
        match status {
            s if s == reqwest::StatusCode::UNAUTHORIZED => {
                Err(WayfinderReadError::Unauthorized { route })
            }
            s if s == reqwest::StatusCode::FORBIDDEN => {
                Err(WayfinderReadError::Forbidden { route })
            }
            s if s == reqwest::StatusCode::NOT_FOUND => Err(WayfinderReadError::NotFound { route }),
            s if !s.is_success() => Err(WayfinderReadError::UnexpectedStatus { route, status }),
            _ => response
                .json()
                .map_err(|source| WayfinderReadError::Request {
                    route,
                    message: format!("response was not valid JSON: {}", source.without_url()),
                }),
        }
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

    /// `PATCH /api/projects/:id/tasks/:task_id/content`. Each of
    /// `title`/`description` of `None` leaves the stored value unchanged
    /// (see `wayfinder-api::UpdateTaskContentRequest`). Used by `ta draft
    /// apply`'s replay step for an approved `ta_propose_task_update`
    /// pending action (v0.17.11.12) -- see
    /// `apps/ta-cli/src/commands/draft_task_replay.rs`.
    pub fn update_task_content(
        &self,
        task_id: &str,
        title: Option<&str>,
        description: Option<&str>,
    ) -> Result<(), WayfinderClientError> {
        let response = self
            .http
            .patch(self.project_url(&["tasks", task_id, "content"]))
            .bearer_auth(self.secret.expose_secret())
            .json(&UpdateTaskContentRequest { title, description })
            .send()
            .map_err(|source| WayfinderClientError::Request {
                context: "updating task content",
                source,
            })?;
        expect_success(response, "updating task content")
    }

    /// `PATCH /api/projects/:id/tasks/:task_id/assignee` with body
    /// `{"assignee_id": <roster id> | null}` (route and body verified
    /// against Wayfinder `main`). `None` clears the assignee. This is the
    /// only place the reassign route and body shape are defined.
    pub fn update_task_assignee(
        &self,
        task_id: &str,
        assignee_id: Option<&str>,
    ) -> Result<(), WayfinderClientError> {
        let response = self
            .http
            .patch(self.project_url(&["tasks", task_id, "assignee"]))
            .bearer_auth(self.secret.expose_secret())
            .json(&UpdateTaskAssigneeRequest { assignee_id })
            .send()
            .map_err(|source| WayfinderClientError::Request {
                context: "updating task assignee",
                source,
            })?;
        expect_success(response, "updating task assignee")
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
                assignee_id: None,
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
    fn update_task_content_patches_the_content_endpoint() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("PATCH"))
                .and(path("/api/projects/proj-1/tasks/task-1/content"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "task-1",
                    "title": "Revised title",
                    "description": null,
                    "verb": "implement",
                    "status": "open",
                    "hold_reason": null,
                    "external_id": null,
                    "updated_at": "1000000000"
                })))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        client
            .update_task_content("task-1", Some("Revised title"), None)
            .unwrap();
    }

    #[test]
    fn update_task_content_omits_unset_fields_from_the_request_body() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("PATCH"))
                .and(path("/api/projects/proj-1/tasks/task-1/content"))
                .and(wiremock::matchers::body_json(serde_json::json!({
                    "description": "New description"
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "task-1",
                    "title": "t",
                    "description": "New description",
                    "verb": "implement",
                    "status": "open",
                    "hold_reason": null,
                    "external_id": null,
                    "updated_at": "1000000000"
                })))
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        client
            .update_task_content("task-1", None, Some("New description"))
            .unwrap();
    }

    #[test]
    fn update_task_assignee_patches_the_assignee_endpoint_with_assignee_id() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("PATCH"))
                .and(path("/api/projects/proj-1/tasks/task-1/assignee"))
                .and(wiremock::matchers::body_json(serde_json::json!({
                    "assignee_id": "role-dev-1"
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "task-1",
                    "status": "open",
                    "hold_reason": null,
                    "external_id": null,
                    "updated_at": "1000000000"
                })))
                .expect(1)
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        client
            .update_task_assignee("task-1", Some("role-dev-1"))
            .unwrap();
    }

    #[test]
    fn update_task_assignee_sends_an_explicit_null_to_clear() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("PATCH"))
                .and(path("/api/projects/proj-1/tasks/task-1/assignee"))
                .and(wiremock::matchers::body_json(serde_json::json!({
                    "assignee_id": null
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "task-1",
                    "status": "open",
                    "hold_reason": null,
                    "external_id": null,
                    "updated_at": "1000000000"
                })))
                .expect(1)
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        client.update_task_assignee("task-1", None).unwrap();
    }

    #[test]
    fn upsert_task_sends_assignee_id_only_when_set() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("POST"))
                .and(path("/api/projects/proj-1/tasks"))
                .and(wiremock::matchers::body_json(serde_json::json!({
                    "title": "New task",
                    "description": null,
                    "verb": "implement",
                    "external_id": "x-1",
                    "assignee_id": "role-1"
                })))
                .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "id": "task-9",
                    "status": "open",
                    "hold_reason": null,
                    "external_id": "x-1",
                    "updated_at": "1000000000"
                })))
                .expect(1)
                .mount(mock.server()),
        );

        let client = client_for(&mock);
        let task = client
            .upsert_task(&CreateTaskRequest {
                title: "New task".to_string(),
                description: None,
                verb: "implement".to_string(),
                external_id: Some("x-1".to_string()),
                assignee_id: Some("role-1".to_string()),
            })
            .unwrap();
        assert_eq!(task.id, "task-9");
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

    // ── Read-only GET helpers (CoS Wayfinder read tools) ───────────────

    fn mount_status(mock: &BlockingMockServer, p: &str, status: u16) {
        mock.block_on(
            Mock::given(method("GET"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(status))
                .mount(mock.server()),
        );
    }

    #[test]
    fn get_project_json_hits_the_project_route_with_bearer_auth_and_query() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/projects/proj-1/kpi-summary"))
                .and(query_param("limit", "7"))
                .and(header("authorization", "Bearer wfsa_test_secret"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})),
                )
                .expect(1)
                .mount(mock.server()),
        );
        let value = client_for(&mock)
            .get_project_json(&["kpi-summary"], &[("limit", "7")])
            .unwrap();
        assert_eq!(value["ok"], true);
    }

    #[test]
    fn get_org_json_hits_the_org_route() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/orgs/org-1/kpis"))
                .and(query_param("project_id", "proj-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
                .expect(1)
                .mount(mock.server()),
        );
        let value = client_for(&mock)
            .get_org_json(&["kpis"], &[("project_id", "proj-1")])
            .unwrap();
        assert!(value.as_array().unwrap().is_empty());
    }

    #[test]
    fn get_project_json_keeps_401_403_404_and_other_statuses_distinct() {
        let mock = BlockingMockServer::start();
        mount_status(&mock, "/api/projects/proj-1/a", 401);
        mount_status(&mock, "/api/projects/proj-1/b", 403);
        mount_status(&mock, "/api/projects/proj-1/c", 404);
        mount_status(&mock, "/api/projects/proj-1/d", 500);
        let client = client_for(&mock);
        assert!(matches!(
            client.get_project_json(&["a"], &[]),
            Err(WayfinderReadError::Unauthorized { .. })
        ));
        assert!(matches!(
            client.get_project_json(&["b"], &[]),
            Err(WayfinderReadError::Forbidden { .. })
        ));
        assert!(matches!(
            client.get_project_json(&["c"], &[]),
            Err(WayfinderReadError::NotFound { .. })
        ));
        let err = client.get_project_json(&["d"], &[]).unwrap_err();
        assert!(matches!(err, WayfinderReadError::UnexpectedStatus { .. }));
        let text = err.to_string();
        assert!(text.contains("500"), "{text}");
        assert!(text.contains("/api/projects/proj-1/d"), "{text}");
        assert!(!text.contains("wfsa_test_secret"), "{text}");
    }

    #[test]
    fn get_project_json_reports_a_timeout_distinctly() {
        let mock = BlockingMockServer::start();
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/projects/proj-1/slow"))
                .respond_with(
                    ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(3)),
                )
                .mount(mock.server()),
        );
        let config = WayfinderPlanConfig {
            base_url: Url::parse(mock.uri()).unwrap(),
            org_id: "org-1".to_string(),
            project_id: "proj-1".to_string(),
            secret: RedactedSecret::new("wfsa_test_secret".to_string()),
        };
        let client =
            WayfinderClient::new_with_timeout(&config, std::time::Duration::from_millis(200))
                .unwrap();
        let err = client.get_project_json(&["slow"], &[]).unwrap_err();
        assert!(matches!(err, WayfinderReadError::Timeout { .. }), "{err}");
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[test]
    fn a_task_id_with_path_characters_cannot_escape_the_project_route() {
        let mock = BlockingMockServer::start();
        // Only the encoded single-segment route is mounted; a request that
        // escaped to another path would 404 instead.
        mock.block_on(
            Mock::given(method("GET"))
                .and(path("/api/projects/proj-1/tasks/..%2Fother"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
                .mount(mock.server()),
        );
        assert!(client_for(&mock)
            .get_project_json(&["tasks", "../other"], &[])
            .is_ok());
    }
}
