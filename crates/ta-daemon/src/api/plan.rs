// api/plan.rs — Plan phase browser, goal-start, and phase-claim API.
//
// GET  /api/plan/phases          — parse PLAN.md, return phase list with items
// POST /api/plan/phase/add       — append a new pending phase to PLAN.md
// POST /api/plan/phase/claim     — atomically claim a phase (pending → in_progress)
// POST /api/goal/start           — start a goal (optionally linked to a phase)
//
// Response caching (v0.17.0.12.29): `parse_plan_phases()` re-parses PLAN.md
// (10,000+ lines) and the `.ta/goals/*.json` directory scan re-reads every
// goal file on *every* call, including the Dashboard's SSE-triggered
// background refreshes (not just Plan-tab clicks). `PlanCache` memoizes the
// parsed phase list keyed on PLAN.md's mtime (invalidated the instant the
// file changes, never stale) and the goals-directory scan behind a short TTL
// matching `StatusCache`'s pattern (goal files don't carry a single mtime to
// key on, and re-scanning the directory every `STATUS_CACHE_TTL_SECS` is
// cheap enough not to need write-triggered invalidation).

use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::api::AppState;

// ── Cached regexes (v0.17.0.12.29) ────────────────────────────
// Previously recompiled on every `parse_plan_phases()` call (i.e. on every
// `/api/plan/phases` request, including background SSE-triggered refreshes).

static PHASE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^(?:##\s+Phase[\s\u{00a0}]+([0-9a-z.]+)\s+[—\-]\s+(.+)|###\s+(v[\d.]+[a-z]?)\s+[—\-]\s+(.+))$",
    )
    .expect("static regex")
});
static STATUS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<!--\s*status:\s*(\w+)\s*-->").expect("static regex"));
static DEP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<!--\s*depends_on:\s*([^>]+?)\s*-->").expect("static regex"));
static ITEM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:-|\d+\.)\s+\[([ xX])\]\s+(.+)$").expect("static regex"));

// ── PlanCache (v0.17.0.12.29) ─────────────────────────────────

/// TTL for the cached `.ta/goals/*.json` directory scan, matching
/// `StatusCache`'s 5-second window (`status.rs`).
const GOALS_SCAN_CACHE_TTL_SECS: u64 = 5;

/// Thread-safe cache for `/api/plan/phases`: the parsed (pre-annotation)
/// phase list keyed on PLAN.md's mtime, plus a short-TTL cache of the
/// `.ta/goals/*.json` scan (`active_phase_states`/`active_phase_draft_ids`).
///
/// The parsed phase list itself never carries `running`/`pr_ready`/`draft_id`
/// — those are re-applied on every request from the (possibly cached) goals
/// scan, so a cache hit on the phase list never returns stale goal state.
type GoalsScanEntry = (
    Instant,
    std::collections::HashMap<String, String>,
    std::collections::HashMap<String, String>,
);

pub struct PlanCache {
    phases: RwLock<Option<(std::time::SystemTime, Vec<ApiPlanPhase>)>>,
    goals_scan: RwLock<Option<GoalsScanEntry>>,
}

impl PlanCache {
    pub fn new() -> Self {
        Self {
            phases: RwLock::new(None),
            goals_scan: RwLock::new(None),
        }
    }

    /// Return cached phases if PLAN.md's mtime matches, or `None`.
    pub async fn get_phases(&self, mtime: std::time::SystemTime) -> Option<Vec<ApiPlanPhase>> {
        let guard = self.phases.read().await;
        if let Some((cached_mtime, phases)) = guard.as_ref() {
            if *cached_mtime == mtime {
                return Some(phases.clone());
            }
        }
        None
    }

    /// Store a freshly-parsed phase list against PLAN.md's current mtime.
    pub async fn set_phases(&self, mtime: std::time::SystemTime, phases: Vec<ApiPlanPhase>) {
        let mut guard = self.phases.write().await;
        *guard = Some((mtime, phases));
    }

    /// Return the cached goals-directory scan if younger than the TTL.
    pub async fn get_goals_scan(
        &self,
    ) -> Option<(
        std::collections::HashMap<String, String>,
        std::collections::HashMap<String, String>,
    )> {
        let guard = self.goals_scan.read().await;
        if let Some((ts, states, drafts)) = guard.as_ref() {
            if ts.elapsed().as_secs() < GOALS_SCAN_CACHE_TTL_SECS {
                return Some((states.clone(), drafts.clone()));
            }
        }
        None
    }

    /// Store a freshly-scanned goals directory snapshot with the current timestamp.
    pub async fn set_goals_scan(
        &self,
        states: std::collections::HashMap<String, String>,
        drafts: std::collections::HashMap<String, String>,
    ) {
        let mut guard = self.goals_scan.write().await;
        *guard = Some((Instant::now(), states, drafts));
    }
}

impl Default for PlanCache {
    fn default() -> Self {
        Self::new()
    }
}

// ── Data types ─────────────────────────────────────────────────

/// A single checklist item from a plan phase.
#[derive(Debug, Clone, Serialize)]
pub struct PlanItem {
    pub text: String,
    pub done: bool,
}

/// A plan phase with full details for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct ApiPlanPhase {
    pub id: String,
    pub title: String,
    /// "pending" | "in_progress" | "done" | "deferred"
    pub status: String,
    /// Short description from the Goal/Focus line, or first paragraph.
    pub description: String,
    pub items: Vec<PlanItem>,
    pub depends_on: Vec<String>,
    /// True if a goal referencing this phase is actively running (not pr_ready).
    pub running: bool,
    /// True if a goal referencing this phase has reached pr_ready state (draft ready for review).
    pub pr_ready: bool,
    /// UUID of the draft package for the pr_ready goal, if any (v0.17.0.1).
    /// Present only when `pr_ready` is true. Allows Studio to navigate directly
    /// to the draft detail panel without a separate lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
}

// ── Parsing ────────────────────────────────────────────────────

/// Parse PLAN.md content into API-friendly phase objects.
///
/// Extracts id, title, status, description (first paragraph / Goal line),
/// checklist items (`- [ ]` / `- [x]`), and depends_on comments.
pub fn parse_plan_phases(content: &str) -> Vec<ApiPlanPhase> {
    // Matches either:
    //   ## Phase 4b — Title
    //   ### v0.3.1 — Title   (or — with em-dash)
    // Compiled once at process startup (LazyLock statics above) instead of on
    // every call — this function runs on every `/api/plan/phases` request.
    let phase_re = &*PHASE_RE;
    let status_re = &*STATUS_RE;
    let dep_re = &*DEP_RE;
    // Matches both "- [ ] text" (unordered) and "1. [ ] text" (ordered) checklist items.
    let item_re = &*ITEM_RE;

    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();

    // Collect (line_index, id, title) for every phase header.
    let mut headers: Vec<(usize, String, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let l = line.trim();
        if let Some(caps) = phase_re.captures(l) {
            let (id, title) = if caps.get(1).is_some() {
                (
                    caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string(),
                    caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string(),
                )
            } else {
                (
                    caps.get(3).map(|m| m.as_str()).unwrap_or("").to_string(),
                    caps.get(4).map(|m| m.as_str()).unwrap_or("").to_string(),
                )
            };
            if id.is_empty() {
                continue;
            }
            // Strip trailing markdown decoration from title.
            let title = title.trim_end_matches(['*', '(', ')']).trim().to_string();
            headers.push((i, id, title));
        }
    }

    let mut phases = Vec::new();
    for h_idx in 0..headers.len() {
        let (start, ref id, ref title) = headers[h_idx];
        let end = headers.get(h_idx + 1).map(|(i, _, _)| *i).unwrap_or(n);

        let section = &lines[start..end];

        // Status: search lines 1–4 after header.
        let mut status = "pending".to_string();
        let mut status_offset: usize = 1; // default body start
        for (j, line) in section[1..section.len().min(5)].iter().enumerate() {
            if let Some(caps) = status_re.captures(line.trim()) {
                status = caps
                    .get(1)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_else(|| "pending".to_string());
                status_offset = j + 2; // line after status marker
                break;
            }
        }

        // depends_on: search up to 8 lines after header.
        let mut depends_on: Vec<String> = Vec::new();
        for line in section[1..section.len().min(9)].iter() {
            if let Some(caps) = dep_re.captures(line.trim()) {
                let raw = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                depends_on = raw
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                break;
            }
        }

        // Description and items from body.
        let mut description_lines: Vec<&str> = Vec::new();
        let mut items: Vec<PlanItem> = Vec::new();
        let mut past_description = false;

        for line in section[status_offset..].iter() {
            let trimmed = line.trim();

            // Stop at phase-level headers (## or ###) but not deeper sub-headers (####).
            if (trimmed.starts_with("## ") || trimmed.starts_with("### "))
                && !trimmed.starts_with("####")
            {
                break;
            }
            // Skip HTML comments (status/depends markers).
            if trimmed.starts_with("<!--") {
                continue;
            }
            // Checklist items.
            if let Some(caps) = item_re.captures(trimmed) {
                past_description = true;
                let done = caps.get(1).map(|m| m.as_str() != " ").unwrap_or(false);
                let raw_text = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                // Strip leading bold/code markers like **`foo`** → foo.
                let text = raw_text
                    .trim_start_matches("**")
                    .trim_start_matches('`')
                    .to_string();
                items.push(PlanItem { text, done });
                continue;
            }
            // Collect description text (before items, skip fences/horizontal rules).
            if !past_description
                && !trimmed.is_empty()
                && trimmed != "---"
                && !trimmed.starts_with("```")
                && !trimmed.starts_with('|') // skip table rows
                && description_lines.len() < 4
            {
                // Prefer the **Goal**: or **Focus**: line as the description.
                let stripped = trimmed
                    .trim_start_matches("**Goal**:")
                    .trim_start_matches("**Focus**:")
                    .trim_start_matches("**Objective**:")
                    .trim();
                description_lines.push(stripped);
            }
        }

        let raw_desc = description_lines.join(" ");
        // Strip remaining markdown bold/italic markers.
        let description = raw_desc.replace("**", "").replace('*', "");

        phases.push(ApiPlanPhase {
            id: id.clone(),
            title: title.clone(),
            status,
            description,
            items,
            depends_on,
            running: false,  // populated separately
            pr_ready: false, // populated separately
            draft_id: None,  // populated separately
        });
    }

    phases
}

/// Check which phase IDs have a currently active goal, and what state they are in.
///
/// Scans `.ta/goals/*.json` for goal files whose `plan_phase` matches one of
/// the supplied phase IDs and whose `state` is an active (non-terminal) state.
///
/// `known_ids` is the set of phase IDs found in the parsed plan. Any goal whose
/// `plan_phase` has no matching entry in `known_ids` is skipped — this suppresses
/// ghost "running" badges for stale goals linked to phases that no longer exist
/// (or whose heading was removed by staging drift).
///
/// Returns a map of phase_id → state string. When multiple goals reference the
/// same phase, the higher-priority state wins: `running` > `pr_ready` > others.
pub fn active_phase_states(
    goals_dir: &std::path::Path,
    known_ids: &std::collections::HashSet<String>,
) -> std::collections::HashMap<String, String> {
    let mut states: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let dir = match std::fs::read_dir(goals_dir) {
        Ok(d) => d,
        Err(_) => return states,
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(phase_id) = val.get("plan_phase").and_then(|v| v.as_str()) else {
            continue;
        };
        // Skip goals whose phase_id has no matching heading in the parsed plan.
        // This prevents ghost badges for stale goals linked to removed/renamed phases.
        if !known_ids.is_empty() && !phase_id_in_known(phase_id, known_ids) {
            continue;
        }
        // GoalRunState serializes as {"state": "running"} (internally-tagged enum),
        // so we must read the nested "state" key, not the top-level field directly.
        let state = val
            .get("state")
            .and_then(|v| v.get("state").or(Some(v)))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        // Active states: created, configured, running, awaiting_input, finalizing, pr_ready.
        let is_active = matches!(
            state,
            "created" | "configured" | "running" | "awaiting_input" | "finalizing" | "pr_ready"
        );
        if is_active {
            // When multiple goals map to the same phase, prefer running > pr_ready > others.
            let existing = states.get(phase_id).map(|s| s.as_str());
            let should_insert = match existing {
                None => true,
                Some("running") => false,
                Some("pr_ready") => state == "running",
                Some(_) => state == "running" || state == "pr_ready",
            };
            if should_insert {
                states.insert(phase_id.to_string(), state.to_string());
            }
        }
    }
    states
}

/// Returns a map of phase_id → draft_id (pr_package_id) for phases whose
/// goal has reached `pr_ready` state (v0.17.0.1).
///
/// Used by `get_plan_phases` to populate `ApiPlanPhase.draft_id` so Studio
/// can navigate directly to the draft detail panel.
pub fn active_phase_draft_ids(
    goals_dir: &std::path::Path,
) -> std::collections::HashMap<String, String> {
    let mut result: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let dir = match std::fs::read_dir(goals_dir) {
        Ok(d) => d,
        Err(_) => return result,
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let state = val
            .get("state")
            .and_then(|v| v.get("state").or(Some(v)))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        if state != "pr_ready" {
            continue;
        }
        let Some(phase_id) = val.get("plan_phase").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(draft_id) = val.get("pr_package_id").and_then(|v| v.as_str()) else {
            continue;
        };
        result.insert(phase_id.to_string(), draft_id.to_string());
    }
    result
}

/// Returns the set of phase IDs that have at least one active goal.
/// Convenience wrapper around `active_phase_states` for callers that only
/// need presence (not the specific state).
#[allow(dead_code)]
fn active_phases(
    goals_dir: &std::path::Path,
    known_ids: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    active_phase_states(goals_dir, known_ids)
        .into_keys()
        .collect()
}

/// Check whether `phase_id` matches any entry in `known_ids`, normalising the
/// optional `v` prefix on both sides.
fn phase_id_in_known(phase_id: &str, known_ids: &std::collections::HashSet<String>) -> bool {
    let stripped = phase_id.strip_prefix('v').unwrap_or(phase_id);
    known_ids.iter().any(|k| {
        let k_stripped = k.strip_prefix('v').unwrap_or(k.as_str());
        k_stripped == stripped
    })
}

// ── Handlers ───────────────────────────────────────────────────

/// `GET /api/plan/phases` — Return all plan phases with description and items.
pub async fn get_plan_phases(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let project_root = state.active_project_root.read().unwrap().clone();
    let plan_path = project_root.join("PLAN.md");

    let mtime = std::fs::metadata(&plan_path)
        .and_then(|m| m.modified())
        .ok();

    // Fast path: PLAN.md's mtime hasn't changed since the last parse — reuse
    // the cached phase list instead of re-reading and re-parsing 10,000+
    // lines. A cache miss (e.g. first request, or the file just changed)
    // falls through to a full read+parse below.
    let cached_phases = match mtime {
        Some(mt) => state.plan_cache.get_phases(mt).await,
        None => None,
    };
    let mut phases = match cached_phases {
        Some(cached) => cached,
        None => {
            let content = match std::fs::read_to_string(&plan_path) {
                Ok(c) => c,
                Err(e) => {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({
                            "error": format!("Could not read PLAN.md: {}", e),
                            "path": plan_path.display().to_string(),
                            "hint": "Run `ta plan create` to generate a plan, or create PLAN.md manually."
                        })),
                    )
                        .into_response();
                }
            };
            let parsed = parse_plan_phases(&content);
            if let Some(mt) = mtime {
                state.plan_cache.set_phases(mt, parsed.clone()).await;
            }
            parsed
        }
    };

    // Build the set of known phase IDs for orphan suppression.
    let known_ids: std::collections::HashSet<String> =
        phases.iter().map(|p| p.id.clone()).collect();

    // Annotate phases with their active goal state (orphaned phase IDs are
    // suppressed). The goals-directory scan is itself cached behind a short
    // TTL (`GOALS_SCAN_CACHE_TTL_SECS`) — it has no single mtime to key on,
    // unlike PLAN.md, and the Dashboard's SSE-triggered refreshes would
    // otherwise re-scan+re-parse every goal JSON file on every event.
    let goals_dir = project_root.join(".ta").join("goals");
    let (active_states, draft_ids) = match state.plan_cache.get_goals_scan().await {
        Some(cached) => cached,
        None => {
            let states = active_phase_states(&goals_dir, &known_ids);
            let drafts = active_phase_draft_ids(&goals_dir);
            state
                .plan_cache
                .set_goals_scan(states.clone(), drafts.clone())
                .await;
            (states, drafts)
        }
    };
    for ph in &mut phases {
        let state = active_states
            .get(&ph.id)
            .or_else(|| active_states.get(&format!("v{}", ph.id)))
            .or_else(|| {
                active_states
                    .iter()
                    .find(|(a, _)| ids_match(a, &ph.id))
                    .map(|(_, s)| s)
            });
        if let Some(s) = state {
            ph.pr_ready = s == "pr_ready";
            ph.running = !ph.pr_ready; // running = active but not pr_ready
        }
        if ph.pr_ready {
            ph.draft_id = draft_ids
                .get(&ph.id)
                .or_else(|| draft_ids.get(&format!("v{}", ph.id)))
                .or_else(|| {
                    draft_ids
                        .iter()
                        .find(|(a, _)| ids_match(a, &ph.id))
                        .map(|(_, v)| v)
                })
                .cloned();
        }
    }

    Json(phases).into_response()
}

/// Request body for `POST /api/plan/phase/add`.
#[derive(Deserialize)]
pub struct AddPhaseRequest {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

/// `POST /api/plan/phase/add` — Append a new pending phase to PLAN.md.
pub async fn add_plan_phase(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AddPhaseRequest>,
) -> impl IntoResponse {
    if body.title.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "title is required"})),
        )
            .into_response();
    }

    let project_root = state.active_project_root.read().unwrap().clone();
    let plan_path = project_root.join("PLAN.md");

    let content = match std::fs::read_to_string(&plan_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("Could not read PLAN.md: {}", e),
                    "hint": "Run `ta plan create` to generate a plan first."
                })),
            )
                .into_response();
        }
    };

    let phases = parse_plan_phases(&content);
    let new_id = next_phase_id(&phases);

    let desc_section = if body.description.trim().is_empty() {
        String::new()
    } else {
        format!("\n**Goal**: {}\n", body.description.trim())
    };

    let new_block = format!(
        "\n### {} — {}\n<!-- status: pending -->{}\n",
        new_id,
        body.title.trim(),
        desc_section,
    );

    let separator = if content.ends_with('\n') { "" } else { "\n" };
    let new_content = format!(
        "{}{}{}",
        content,
        separator,
        new_block.trim_start_matches('\n')
    );

    // v0.17.0.12.7: If a goal is running, queue this write as an advisor
    // patch instead of writing directly — a running goal's staging copy may
    // also be editing PLAN.md, and `ta draft apply` would otherwise silently
    // discard whichever side applies last.
    if let Err(e) = ta_workspace::advisor_patch::queue_or_write(
        &project_root,
        "PLAN.md",
        new_content.as_bytes(),
        &format!("add plan phase: {}", body.title.trim()),
        || ta_workspace::advisor_patch::has_active_goal_in_project(&project_root),
    ) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Could not write PLAN.md: {}", e)})),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "id": new_id,
        "title": body.title.trim(),
        "status": "pending",
        "description": body.description.trim(),
        "items": [],
        "depends_on": [],
        "running": false,
    }))
    .into_response()
}

/// Determine the next phase ID by incrementing the highest semver-style ID.
fn next_phase_id(phases: &[ApiPlanPhase]) -> String {
    let mut best: Option<(u32, u32, u32)> = None;
    for ph in phases {
        if let Some(ver) = parse_semver_triple(&ph.id) {
            if best.is_none_or(|b| ver > b) {
                best = Some(ver);
            }
        }
    }
    match best {
        Some((maj, min, patch)) => format!("v{}.{}.{}", maj, min, patch + 1),
        None => "v0.1.0".to_string(),
    }
}

fn parse_semver_triple(id: &str) -> Option<(u32, u32, u32)> {
    let id = id.strip_prefix('v').unwrap_or(id);
    let parts: Vec<&str> = id.splitn(4, '.').collect();
    let maj = parts.first()?.parse::<u32>().ok()?;
    let min = parts.get(1)?.parse::<u32>().ok()?;
    let patch = parts
        .get(2)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    Some((maj, min, patch))
}

/// Compare two phase IDs normalising the optional `v` prefix.
pub fn ids_match(a: &str, b: &str) -> bool {
    let a = a.strip_prefix('v').unwrap_or(a);
    let b = b.strip_prefix('v').unwrap_or(b);
    a == b
}

// ── Phase claim ────────────────────────────────────────────────

/// Request body for `POST /api/plan/phase/claim`.
#[derive(Deserialize)]
pub struct ClaimPhaseRequest {
    /// The phase ID to claim (e.g., "v0.15.24.2").
    pub phase_id: String,
    /// Optional goal ID that will own this phase (recorded for diagnostics).
    pub goal_id: Option<String>,
}

/// `POST /api/plan/phase/claim` — Atomically claim a plan phase.
///
/// Flow:
///   1. Acquire the in-memory `PhaseClaims` mutex — serialises concurrent requests.
///   2. If the phase is already in the claim registry → 409.
///   3. Read PLAN.md and check the phase status:
///      - `done` or `in_progress` → release memory claim + 409.
///      - `pending` → write `in_progress` marker + record history.
///   4. Return 200 with `{ "status": "claimed" }`.
///
/// If `ta run` calls this endpoint and receives 409, it must NOT launch the agent.
/// If the daemon is unreachable, `ta run` falls back to a direct file-write with
/// the same pending-only guard.
pub async fn claim_phase(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ClaimPhaseRequest>,
) -> impl IntoResponse {
    let phase_id = body.phase_id.trim().to_string();
    if phase_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "phase_id must not be empty" })),
        )
            .into_response();
    }

    // Step 1: read PLAN.md and validate phase status.
    //
    // We check PLAN.md BEFORE the in-memory registry so we can self-heal stale
    // claims: if PLAN.md says `pending` but the registry still holds an entry
    // (e.g. the goal was deleted without the daemon being notified), we auto-
    // release the stale registry entry so the new run proceeds without requiring
    // a daemon restart.
    let plan_path = state.project_root.join("PLAN.md");
    let plan_content: Option<String> = if plan_path.exists() {
        match std::fs::read_to_string(&plan_path) {
            Ok(c) => Some(c),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": format!("Failed to read PLAN.md: {}", e) })),
                )
                    .into_response();
            }
        }
    } else {
        None
    };

    if let Some(ref content) = plan_content {
        let phases = parse_plan_phases(content);
        let maybe_phase = phases.iter().find(|p| ids_match(&p.id, &phase_id));

        match maybe_phase.map(|p| p.status.as_str()) {
            Some("done") => {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": format!("Phase {} is already done", phase_id)
                    })),
                )
                    .into_response();
            }
            Some("pending") => {
                // PLAN.md says pending — any in-memory claim is stale.
                // Auto-release so this run can proceed without a daemon restart.
                state.phase_claims.release(&phase_id);
            }
            // PLAN.md in_progress + in-memory claim held → genuine concurrent run,
            // block and surface recovery options.
            Some("in_progress") if state.phase_claims.is_claimed(&phase_id) => {
                let goal_hint = state
                    .phase_claims
                    .snapshot()
                    .into_iter()
                    .find(|(k, _)| k == &phase_id)
                    .and_then(|(_, g)| g)
                    .map(|g| format!("goal {}", g))
                    .unwrap_or_else(|| "unknown goal".to_string());
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": format!(
                            "Phase {} could not be claimed: already in progress ({}). \
                             If the previous run was killed or failed before producing a draft, \
                             run `ta goal delete <id>` or `ta plan reset {}` to reclaim it.",
                            phase_id, goal_hint, phase_id
                        ),
                        "hint": {
                            "phase_id": phase_id,
                            "held_by": goal_hint,
                            "recovery": [
                                "ta goal list   # find the stuck goal ID",
                                format!("ta goal delete <id>       # delete goal + auto-unclaim"),
                                format!("ta plan reset {}          # force-clear if goal is gone", phase_id),
                            ]
                        }
                    })),
                )
                    .into_response();
            }
            // PLAN.md in_progress but in-memory claim absent — daemon was restarted and
            // its registry was cleared. Allow the claim; the in_progress marker will be
            // rewritten below.
            Some("in_progress") => {}
            _ => {} // phase not found or unrecognised status — proceed
        }
    }

    // Step 2: acquire the in-memory claim (serialised by mutex).
    if let Err(msg) = state
        .phase_claims
        .try_claim(&phase_id, body.goal_id.as_deref())
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response();
    }

    // Step 3: write `in_progress` marker to PLAN.md.
    if let Some(ref content) = plan_content {
        let updated = update_phase_status_in_content(content, &phase_id, "in_progress");
        if let Err(e) = std::fs::write(&plan_path, &updated) {
            state.phase_claims.release(&phase_id);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to write PLAN.md: {}", e) })),
            )
                .into_response();
        }
    }

    // Step 4: record in plan_history.jsonl.
    let history_path = state.project_root.join(".ta/plan_history.jsonl");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_path)
    {
        use std::io::Write as _;
        let entry = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "phase_id": phase_id,
            "old_status": "pending",
            "new_status": "in_progress",
            "source": "daemon_claim",
        });
        let _ = writeln!(file, "{}", entry);
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "claimed", "phase_id": phase_id })),
    )
        .into_response()
}

/// `POST /api/plan/phase/release` — Release an in-memory phase claim.
///
/// Called by `ta draft deny` and `ta draft close` after resetting PLAN.md to `pending`.
/// This releases the daemon's in-memory claim registry so future `claim` calls succeed.
///
/// If the phase was not claimed, returns 200 with `{ "status": "not_claimed" }` — this
/// is not an error (idempotent by design).
pub async fn release_phase(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let phase_id = match body.get("phase_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id.trim().to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "phase_id must not be empty" })),
            )
                .into_response();
        }
    };

    let was_claimed = state.phase_claims.is_claimed(&phase_id);
    state.phase_claims.release(&phase_id);

    tracing::info!(
        phase = %phase_id,
        was_claimed,
        "Phase claim released via API"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": if was_claimed { "released" } else { "not_claimed" },
            "phase_id": phase_id,
        })),
    )
        .into_response()
}

/// Update a phase status marker in PLAN.md content.
fn update_phase_status_in_content(content: &str, phase_id: &str, new_status: &str) -> String {
    let status_re = regex::Regex::new(r"<!--\s*status:\s*\w+\s*-->").expect("static regex");
    // Phase header patterns (same as parse_plan_phases).
    let phase_re = regex::Regex::new(
        r"(?m)^(?:##\s+Phase[\s\u{00a0}]+([0-9a-z.]+)\s+[—\-]|###\s+(v[\d.]+[a-z]?)\s+[—\-])",
    )
    .expect("static regex");

    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::with_capacity(lines.len());
    let mut in_target = false;
    let mut replaced = false;

    for line in &lines {
        if phase_re.is_match(line) {
            // Extract the ID from this header.
            let header_id = if let Some(caps) = phase_re.captures(line) {
                caps.get(1)
                    .or_else(|| caps.get(2))
                    .map(|m| m.as_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                String::new()
            };
            in_target = ids_match(&header_id, phase_id);
            replaced = false;
        }
        if in_target && !replaced && status_re.is_match(line) {
            result.push(format!("<!-- status: {} -->", new_status));
            replaced = true;
            in_target = false;
            continue;
        }
        result.push(line.to_string());
    }
    result.join("\n")
}

// ── Goal start ─────────────────────────────────────────────────

/// Request body for `POST /api/goal/start`.
#[derive(Deserialize)]
pub struct GoalStartRequest {
    /// Goal title. If omitted, the phase title is used (requires `phase_id`).
    pub title: Option<String>,
    /// Optional freeform prompt / description passed as `--description`.
    pub prompt: Option<String>,
    /// Optional plan phase link (e.g., "v0.14.19"). Passed as `--phase`.
    pub phase_id: Option<String>,
}

/// `POST /api/goal/start` — Start a goal, optionally linked to a plan phase.
///
/// Spawns `ta run <title> [--phase <id>] [--description <text>]` as a
/// background process using the same mechanism as `POST /api/cmd`. Returns
/// the output key so the caller can tail the stream.
pub async fn start_goal(
    State(state): State<Arc<AppState>>,
    Json(body): Json<GoalStartRequest>,
) -> impl IntoResponse {
    // Resolve title.
    let title = match body.title.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(t) => t.to_string(),
        None => match body.phase_id.as_deref() {
            Some(phase_id) => {
                // Derive title from PLAN.md.
                let project_root = state.active_project_root.read().unwrap().clone();
                let plan_path = project_root.join("PLAN.md");
                let phase_title = std::fs::read_to_string(&plan_path)
                    .ok()
                    .and_then(|c| {
                        let phases = parse_plan_phases(&c);
                        phases
                            .into_iter()
                            .find(|p| ids_match(&p.id, phase_id))
                            .map(|p| format!("{} — {}", p.id, p.title))
                    })
                    .unwrap_or_else(|| format!("Phase {}", phase_id));
                phase_title
            }
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "title or phase_id is required"})),
                )
                    .into_response();
            }
        },
    };

    // Build args: ["run", "<title>", "--phase", "<id>", ...]
    let mut args: Vec<String> = vec!["run".to_string(), title.clone()];
    if let Some(ref phase_id) = body.phase_id {
        args.push("--phase".to_string());
        args.push(phase_id.clone());
    }
    if let Some(ref prompt) = body.prompt {
        if !prompt.trim().is_empty() {
            args.push("--description".to_string());
            args.push(prompt.clone());
        }
    }

    let binary = find_ta_binary();
    let working_dir = state.active_project_root.read().unwrap().clone();
    let output_key = extract_goal_key(&args);

    let goal_title_display = args.get(1).cloned().unwrap_or_default();
    let goal_output = state.goal_output.clone_ref();
    let tx = goal_output.create_channel(&output_key).await;
    let output_key_response = output_key.clone();
    let output_key_display = output_key.clone();

    tokio::spawn(async move {
        tracing::info!(
            "Goal start (plan tab): {} (output key: {})",
            title,
            output_key_display
        );

        let consent_path = working_dir.join(".ta/consent.json");
        let has_consent = consent_path.exists();

        let mut cmd_builder = tokio::process::Command::new(&binary);
        cmd_builder.arg("--project-root").arg(&working_dir);
        if has_consent {
            cmd_builder.arg("--accept-terms");
        }
        // Inject --headless after the "run" subcommand.
        if let Some(subcmd) = args.first() {
            cmd_builder.arg(subcmd);
            cmd_builder.arg("--headless");
            cmd_builder.args(&args[1..]);
        }

        let goal_input = state.goal_input.clone();
        let output_key_stdin = output_key_display.clone();

        let result = cmd_builder
            .current_dir(&working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::piped())
            .spawn();

        match result {
            Ok(mut child) => {
                use tokio::io::{AsyncBufReadExt, BufReader};

                if let Some(stdin) = child.stdin.take() {
                    goal_input.register(&output_key_stdin, stdin).await;
                }

                let stdout = child.stdout.take();
                let stderr = child.stderr.take();

                let tx2 = tx.clone();
                let tx3 = tx.clone();

                let stdout_task = tokio::spawn(async move {
                    if let Some(out) = stdout {
                        let mut reader = BufReader::new(out).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            tx.publish("stdout", line).await;
                        }
                    }
                });

                let stderr_task = tokio::spawn(async move {
                    if let Some(err) = stderr {
                        let mut reader = BufReader::new(err).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            tx2.publish("stderr", line).await;
                        }
                    }
                });

                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                tx3.publish("stdout", "[goal process exited]".to_string())
                    .await;
            }
            Err(e) => {
                tx.publish(
                    "stderr",
                    format!("Failed to start goal: {}. Is `ta` on PATH?", e),
                )
                .await;
            }
        }
    });

    Json(serde_json::json!({
        "status": "started",
        "title": goal_title_display,
        "output_key": output_key_response,
    }))
    .into_response()
}

// ── Utilities ──────────────────────────────────────────────────

/// Locate the `ta` binary. Prefers the one adjacent to the running daemon.
fn find_ta_binary() -> String {
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let ta_path = dir.join("ta");
            if ta_path.exists() {
                return ta_path.to_string_lossy().to_string();
            }
        }
    }
    "ta".to_string()
}

/// Derive an output-stream key from args (phase ID → title → UUID fallback).
fn extract_goal_key(args: &[String]) -> String {
    for arg in args {
        if arg.starts_with("v0.") || arg.starts_with("v1.") {
            return arg.clone();
        }
    }
    for (i, arg) in args.iter().enumerate() {
        if i > 0 && !arg.starts_with('-') {
            return arg.clone();
        }
    }
    uuid::Uuid::new_v4().to_string()
}

// ── Plan generation ────────────────────────────────────────────

/// Request body for plan generation.
#[derive(Deserialize)]
pub struct PlanGenerateRequest {
    pub description: String,
}

/// `POST /api/plan/generate` — Generate draft plan phases from a project description.
///
/// Returns proposed phases as structured JSON. The user reviews them in Studio
/// before committing to PLAN.md via `/api/plan/phase/add`.
pub async fn generate_plan_phases(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<PlanGenerateRequest>,
) -> impl IntoResponse {
    if body.description.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "description is required"})),
        )
            .into_response();
    }

    // Generate a starter set of phases based on the description.
    // In a full implementation, this would spawn an agent to draft phases.
    // For now, we generate a sensible default scaffold that the user can edit.
    let phases = vec![
        serde_json::json!({
            "id": "v0.1.0",
            "title": "Project Foundation",
            "description": "Initial setup, dependencies, and core data structures.",
            "status": "pending",
        }),
        serde_json::json!({
            "id": "v0.2.0",
            "title": "Core Implementation",
            "description": format!("Main implementation for: {}", body.description.trim()),
            "status": "pending",
        }),
        serde_json::json!({
            "id": "v0.3.0",
            "title": "Testing & Quality",
            "description": "Unit tests, integration tests, and quality checks.",
            "status": "pending",
        }),
        serde_json::json!({
            "id": "v0.4.0",
            "title": "Documentation & Polish",
            "description": "User docs, README, and final polish.",
            "status": "pending",
        }),
    ];

    Json(serde_json::json!({
        "phases": phases,
        "description": body.description.trim(),
        "message": "Review these proposed phases. Edit titles/descriptions, then save each to your plan.",
    }))
    .into_response()
}

// ── Plan new (v0.14.21) ────────────────────────────────────────

/// Request body for `POST /api/plan/new`.
#[derive(Deserialize)]
pub struct PlanNewRequest {
    /// Short project description (use when no file_content given).
    pub description: Option<String>,
    /// Full document content (Markdown or plain text) for detailed spec input.
    pub file_content: Option<String>,
    /// Planning framework: "default" or "bmad". Defaults to "default".
    #[serde(default)]
    pub framework: Option<String>,
}

/// `POST /api/plan/new` — Start a plan-generation goal for the current project.
///
/// Spawns `ta plan new "<description>"` or `ta plan new --stdin` (piping file_content)
/// as a background process. Returns `{ output_key }` so Studio can poll.
pub async fn plan_new(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PlanNewRequest>,
) -> impl IntoResponse {
    // Require at least description or file_content.
    let has_description = body
        .description
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_file = body
        .file_content
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    if !has_description && !has_file {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "description or file_content is required"})),
        )
            .into_response();
    }

    let framework = body.framework.as_deref().unwrap_or("default").to_string();

    // Build args for `ta plan new`.
    let mut args: Vec<String> = vec!["plan".to_string(), "new".to_string()];

    let stdin_content: Option<String> = if has_file {
        args.push("--stdin".to_string());
        args.push("--framework".to_string());
        args.push(framework.clone());
        body.file_content.clone()
    } else {
        args.push(body.description.clone().unwrap_or_default());
        args.push("--framework".to_string());
        args.push(framework.clone());
        None
    };

    let binary = find_ta_binary();
    let working_dir = state.active_project_root.read().unwrap().clone();
    let output_key = format!("plan-new-{}", uuid::Uuid::new_v4());

    let goal_output = state.goal_output.clone_ref();
    let tx = goal_output.create_channel(&output_key).await;
    let output_key_response = output_key.clone();
    let output_key_display = output_key.clone();

    tokio::spawn(async move {
        tracing::info!(
            "plan new (API): framework={}, output_key={}",
            framework,
            output_key_display
        );

        let consent_path = working_dir.join(".ta/consent.json");
        let has_consent = consent_path.exists();

        let mut cmd_builder = tokio::process::Command::new(&binary);
        cmd_builder.arg("--project-root").arg(&working_dir);
        if has_consent {
            cmd_builder.arg("--accept-terms");
        }
        cmd_builder.args(&args);

        if stdin_content.is_some() {
            cmd_builder.stdin(std::process::Stdio::piped());
        } else {
            cmd_builder.stdin(std::process::Stdio::null());
        }

        let result = cmd_builder
            .current_dir(&working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        match result {
            Ok(mut child) => {
                use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

                if let (Some(mut stdin_handle), Some(content)) = (child.stdin.take(), stdin_content)
                {
                    tokio::spawn(async move {
                        let _ = stdin_handle.write_all(content.as_bytes()).await;
                    });
                }

                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let tx2 = tx.clone();
                let tx3 = tx.clone();

                let stdout_task = tokio::spawn(async move {
                    if let Some(out) = stdout {
                        let mut reader = BufReader::new(out).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            tx.publish("stdout", line).await;
                        }
                    }
                });
                let stderr_task = tokio::spawn(async move {
                    if let Some(err) = stderr {
                        let mut reader = BufReader::new(err).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            tx2.publish("stderr", line).await;
                        }
                    }
                });

                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                tx3.publish("stdout", "[plan new process exited]".to_string())
                    .await;
            }
            Err(e) => {
                tx.publish("stderr", format!("Failed to spawn ta plan new: {}", e))
                    .await;
            }
        }
    });

    Json(serde_json::json!({
        "output_key": output_key_response,
        "message": "Plan generation started. Poll /api/goals/output/<output_key> for progress.",
    }))
    .into_response()
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PLAN: &str = r#"# Project Plan

## Versioning

Some intro text.

### v0.14.18 — TA Studio: Multi-Project Support
<!-- status: done -->

**Goal**: Add multi-project support to TA Studio.

#### Items

1. [x] Project browser UI
2. [x] Platform launchers

### v0.14.19 — TA Studio: Plan Tab
<!-- status: pending -->
<!-- depends_on: v0.14.18 -->

**Goal**: Replace "Start a Goal" with a Plan tab.

#### Items

1. [ ] `GET /api/plan/phases`
2. [ ] Phase card UI
3. [x] Something already done

### v0.15.0 — Generic Binary & Text Assets
<!-- status: pending -->

Future work.
"#;

    #[test]
    fn parse_plan_phases_extracts_all() {
        let phases = parse_plan_phases(SAMPLE_PLAN);
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].id, "v0.14.18");
        assert_eq!(phases[0].status, "done");
        assert_eq!(phases[1].id, "v0.14.19");
        assert_eq!(phases[1].status, "pending");
        assert_eq!(phases[2].id, "v0.15.0");
        assert_eq!(phases[2].status, "pending");
    }

    #[test]
    fn parse_plan_phases_items_correct() {
        let phases = parse_plan_phases(SAMPLE_PLAN);
        // v0.14.19 has 3 items: 2 undone, 1 done
        let p = phases.iter().find(|p| p.id == "v0.14.19").unwrap();
        assert_eq!(p.items.len(), 3);
        assert!(!p.items[0].done);
        assert!(!p.items[1].done);
        assert!(p.items[2].done);
    }

    #[test]
    fn parse_plan_phases_depends_on() {
        let phases = parse_plan_phases(SAMPLE_PLAN);
        let p = phases.iter().find(|p| p.id == "v0.14.19").unwrap();
        assert_eq!(p.depends_on, vec!["v0.14.18".to_string()]);
    }

    #[test]
    fn parse_plan_phases_description() {
        let phases = parse_plan_phases(SAMPLE_PLAN);
        let p = phases.iter().find(|p| p.id == "v0.14.19").unwrap();
        // Description should contain the Goal line text.
        assert!(!p.description.is_empty());
    }

    #[test]
    fn next_phase_id_increments_patch() {
        let phases = parse_plan_phases(SAMPLE_PLAN);
        // Highest version is v0.15.0 → next is v0.15.1
        let next = next_phase_id(&phases);
        assert_eq!(next, "v0.15.1");
    }

    #[test]
    fn ids_match_normalises_v_prefix() {
        assert!(ids_match("v0.14.19", "0.14.19"));
        assert!(ids_match("0.14.19", "v0.14.19"));
        assert!(ids_match("v0.14.19", "v0.14.19"));
        assert!(!ids_match("v0.14.18", "v0.14.19"));
    }

    #[test]
    fn parse_plan_pending_phases_only_filter() {
        let phases = parse_plan_phases(SAMPLE_PLAN);
        let pending: Vec<_> = phases.iter().filter(|p| p.status == "pending").collect();
        assert_eq!(pending.len(), 2);
    }

    // ── plan_new request validation (v0.14.21) ─────────────────────────────

    #[test]
    fn plan_new_requires_description_or_file() {
        let req = PlanNewRequest {
            description: None,
            file_content: None,
            framework: None,
        };
        let has_description = req
            .description
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let has_file = req
            .file_content
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        assert!(!has_description && !has_file);
    }

    #[test]
    fn plan_new_framework_defaults_to_default() {
        let req = PlanNewRequest {
            description: Some("test".to_string()),
            file_content: None,
            framework: None,
        };
        let framework = req.framework.as_deref().unwrap_or("default");
        assert_eq!(framework, "default");
    }

    // ── v0.16.1.1: v0.15.9 heading, orphan suppression, phase_id_in_known ──

    const PLAN_WITH_V0_15_9: &str = r#"### v0.15.8 — Foo
<!-- status: done -->

Some content.

### v0.15.9 — Messaging Adapter Plugin Architecture
<!-- status: done -->

1. [x] Implement plugin protocol
2. [x] Write tests

#### Version: `0.15.9-alpha`

### v0.15.10 — Email Assistant
<!-- status: pending -->

Future work.
"#;

    #[test]
    fn parse_plan_phases_finds_v0_15_9_with_heading() {
        let phases = parse_plan_phases(PLAN_WITH_V0_15_9);
        let phase = phases.iter().find(|p| p.id == "v0.15.9");
        assert!(
            phase.is_some(),
            "v0.15.9 should be found when heading exists"
        );
        let phase = phase.unwrap();
        assert_eq!(phase.status, "done");
        assert_eq!(phase.title, "Messaging Adapter Plugin Architecture");
        assert_eq!(phase.items.len(), 2);
        assert!(phase.items[0].done);
        assert!(phase.items[1].done);
    }

    #[test]
    fn phase_id_in_known_matches_with_and_without_v_prefix() {
        let known: std::collections::HashSet<String> = ["v0.15.9", "v0.15.10", "v0.16.0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(phase_id_in_known("v0.15.9", &known));
        assert!(phase_id_in_known("0.15.9", &known)); // no v-prefix
        assert!(!phase_id_in_known("v0.15.11", &known));
        assert!(!phase_id_in_known("v0.99.0", &known));
    }

    #[test]
    fn active_phases_suppresses_orphaned_phase_ids() {
        let dir = tempfile::tempdir().unwrap();
        let goals_dir = dir.path().join("goals");
        std::fs::create_dir_all(&goals_dir).unwrap();

        // Write a goal JSON whose plan_phase is NOT in the known set.
        let orphan = serde_json::json!({
            "plan_phase": "v0.99.0",
            "state": "running",
        });
        std::fs::write(goals_dir.join("orphan.json"), orphan.to_string()).unwrap();

        // Write a goal JSON whose plan_phase IS in the known set.
        let valid = serde_json::json!({
            "plan_phase": "v0.15.9",
            "state": "running",
        });
        std::fs::write(goals_dir.join("valid.json"), valid.to_string()).unwrap();

        let known: std::collections::HashSet<String> =
            ["v0.15.9"].iter().map(|s| s.to_string()).collect();

        let active = active_phases(&goals_dir, &known);
        assert!(active.contains("v0.15.9"), "known phase should be active");
        assert!(
            !active.contains("v0.99.0"),
            "orphaned phase should be suppressed"
        );
    }

    #[test]
    fn active_phases_allows_all_when_known_ids_empty() {
        let dir = tempfile::tempdir().unwrap();
        let goals_dir = dir.path().join("goals");
        std::fs::create_dir_all(&goals_dir).unwrap();

        let goal = serde_json::json!({
            "plan_phase": "v0.15.9",
            "state": "running",
        });
        std::fs::write(goals_dir.join("g.json"), goal.to_string()).unwrap();

        // Empty known_ids — should not suppress anything.
        let known: std::collections::HashSet<String> = std::collections::HashSet::new();
        let active = active_phases(&goals_dir, &known);
        assert!(active.contains("v0.15.9"));
    }

    // ── v0.16.1.2: pr_ready state tracking ────────────────────────────────────

    #[test]
    fn active_phase_states_tracks_pr_ready() {
        let dir = tempfile::tempdir().unwrap();
        let goals_dir = dir.path().join("goals");
        std::fs::create_dir_all(&goals_dir).unwrap();

        let goal = serde_json::json!({
            "plan_phase": "v0.16.0",
            "state": "pr_ready",
        });
        std::fs::write(goals_dir.join("g.json"), goal.to_string()).unwrap();

        let known: std::collections::HashSet<String> =
            ["v0.16.0"].iter().map(|s| s.to_string()).collect();
        let states = active_phase_states(&goals_dir, &known);
        assert_eq!(
            states.get("v0.16.0").map(|s| s.as_str()),
            Some("pr_ready"),
            "pr_ready goal should be tracked with state 'pr_ready'"
        );
    }

    #[test]
    fn active_phase_states_running_beats_pr_ready() {
        let dir = tempfile::tempdir().unwrap();
        let goals_dir = dir.path().join("goals");
        std::fs::create_dir_all(&goals_dir).unwrap();

        // Two goals for the same phase: one running, one pr_ready.
        let g1 = serde_json::json!({ "plan_phase": "v0.16.0", "state": "pr_ready" });
        let g2 = serde_json::json!({ "plan_phase": "v0.16.0", "state": "running" });
        std::fs::write(goals_dir.join("g1.json"), g1.to_string()).unwrap();
        std::fs::write(goals_dir.join("g2.json"), g2.to_string()).unwrap();

        let known: std::collections::HashSet<String> =
            ["v0.16.0"].iter().map(|s| s.to_string()).collect();
        let states = active_phase_states(&goals_dir, &known);
        assert_eq!(
            states.get("v0.16.0").map(|s| s.as_str()),
            Some("running"),
            "running state should take precedence over pr_ready"
        );
    }

    #[test]
    fn active_phases_orphan_regression_no_ghost_badge() {
        // A goal whose plan_phase is not in the known set must not produce a badge.
        let dir = tempfile::tempdir().unwrap();
        let goals_dir = dir.path().join("goals");
        std::fs::create_dir_all(&goals_dir).unwrap();

        let orphan = serde_json::json!({ "plan_phase": "v0.99.99", "state": "running" });
        std::fs::write(goals_dir.join("orphan.json"), orphan.to_string()).unwrap();

        let known: std::collections::HashSet<String> =
            ["v0.16.0"].iter().map(|s| s.to_string()).collect();
        let states = active_phase_states(&goals_dir, &known);
        assert!(
            states.is_empty(),
            "orphaned phase_id must not produce an active state entry"
        );
    }

    // ── phase claim release tests (v0.16.1.6.1) ─────────────────────────────

    #[test]
    fn denied_draft_releases_phase_claim() {
        // The phase claim registry must allow re-claiming after release.
        let claims = crate::phase_claim::PhaseClaims::new();
        claims.try_claim("v0.1.0", Some("goal-a")).unwrap();
        assert!(claims.is_claimed("v0.1.0"));

        // Simulate what release_phase handler does.
        claims.release("v0.1.0");
        assert!(!claims.is_claimed("v0.1.0"));

        // Phase can be claimed again after release.
        assert!(claims.try_claim("v0.1.0", Some("goal-b")).is_ok());
    }

    #[test]
    fn denied_draft_resets_planmd_to_pending() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("PLAN.md");
        std::fs::write(
            &plan_path,
            "### v0.1.0 — Test\n<!-- status: in_progress -->\n",
        )
        .unwrap();

        let updated = update_phase_status_in_content(
            &std::fs::read_to_string(&plan_path).unwrap(),
            "v0.1.0",
            "pending",
        );
        std::fs::write(&plan_path, &updated).unwrap();

        let content = std::fs::read_to_string(&plan_path).unwrap();
        assert!(
            content.contains("status: pending"),
            "PLAN.md should have pending status after reset: {}",
            content
        );
        assert!(
            !content.contains("in_progress"),
            "PLAN.md should not have in_progress: {}",
            content
        );
    }

    #[test]
    fn closed_goal_releases_phase_claim() {
        let claims = crate::phase_claim::PhaseClaims::new();
        claims.try_claim("v0.2.0", Some("goal-c")).unwrap();

        // release() is idempotent — calling it twice is fine.
        claims.release("v0.2.0");
        claims.release("v0.2.0"); // should not panic
        assert!(!claims.is_claimed("v0.2.0"));
    }

    // ── v0.17.0.1: active_phase_draft_ids ────────────────────────────────────

    #[test]
    fn active_phase_draft_ids_returns_draft_for_pr_ready() {
        let dir = tempfile::tempdir().unwrap();
        let goals_dir = dir.path().join("goals");
        std::fs::create_dir_all(&goals_dir).unwrap();

        let draft_uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let goal = serde_json::json!({
            "plan_phase": "v0.17.0",
            "state": "pr_ready",
            "pr_package_id": draft_uuid,
        });
        std::fs::write(goals_dir.join("g.json"), goal.to_string()).unwrap();

        let ids = active_phase_draft_ids(&goals_dir);
        assert_eq!(
            ids.get("v0.17.0").map(|s| s.as_str()),
            Some(draft_uuid),
            "draft_id should be the pr_package_id of the pr_ready goal"
        );
    }

    #[test]
    fn active_phase_draft_ids_ignores_running_goals() {
        let dir = tempfile::tempdir().unwrap();
        let goals_dir = dir.path().join("goals");
        std::fs::create_dir_all(&goals_dir).unwrap();

        let goal = serde_json::json!({
            "plan_phase": "v0.17.0",
            "state": "running",
            "pr_package_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        });
        std::fs::write(goals_dir.join("g.json"), goal.to_string()).unwrap();

        let ids = active_phase_draft_ids(&goals_dir);
        assert!(
            ids.is_empty(),
            "running goals should not appear in draft_ids"
        );
    }

    #[test]
    fn active_phase_draft_ids_ignores_goal_without_package() {
        let dir = tempfile::tempdir().unwrap();
        let goals_dir = dir.path().join("goals");
        std::fs::create_dir_all(&goals_dir).unwrap();

        // pr_ready but no pr_package_id yet (draft build still in progress).
        let goal = serde_json::json!({
            "plan_phase": "v0.17.0",
            "state": "pr_ready",
        });
        std::fs::write(goals_dir.join("g.json"), goal.to_string()).unwrap();

        let ids = active_phase_draft_ids(&goals_dir);
        assert!(
            ids.is_empty(),
            "pr_ready goal without pr_package_id should not produce a draft_id entry"
        );
    }

    #[test]
    fn applied_draft_marks_phase_done() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("PLAN.md");
        std::fs::write(
            &plan_path,
            "### v0.3.0 — Apply Test\n<!-- status: in_progress -->\n",
        )
        .unwrap();

        let updated = update_phase_status_in_content(
            &std::fs::read_to_string(&plan_path).unwrap(),
            "v0.3.0",
            "done",
        );
        std::fs::write(&plan_path, &updated).unwrap();

        let content = std::fs::read_to_string(&plan_path).unwrap();
        assert!(
            content.contains("status: done"),
            "PLAN.md should have done status after apply: {}",
            content
        );
    }

    // ── PlanCache tests (v0.17.0.12.29) ──────────────────────────

    async fn call_get_plan_phases(state: &Arc<AppState>) -> Vec<serde_json::Value> {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;
        let resp = get_plan_phases(State(state.clone())).await.into_response();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn plan_phases_cache_reuses_content_within_same_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("PLAN.md");
        std::fs::write(
            &plan_path,
            "### v0.1.0 — Original Title\n<!-- status: pending -->\n",
        )
        .unwrap();

        let state = Arc::new(AppState::new(
            std::path::PathBuf::from(dir.path()),
            crate::config::DaemonConfig::default(),
        ));

        let first = call_get_plan_phases(&state).await;
        assert_eq!(first[0]["title"], "Original Title");

        // Overwrite PLAN.md's content on disk but force the mtime back to its
        // original value — this simulates the read racing a concurrent write
        // that hasn't bumped the mtime yet, and proves the second call is
        // served from `PlanCache` rather than re-reading the file: if it had
        // re-read, it would see "Changed Title" instead.
        let original_mtime = std::fs::metadata(&plan_path).unwrap().modified().unwrap();
        std::fs::write(
            &plan_path,
            "### v0.1.0 — Changed Title\n<!-- status: pending -->\n",
        )
        .unwrap();
        std::fs::File::open(&plan_path)
            .unwrap()
            .set_modified(original_mtime)
            .unwrap();

        let second = call_get_plan_phases(&state).await;
        assert_eq!(
            second[0]["title"], "Original Title",
            "a second call within the same mtime window must be served from PlanCache, not re-read from disk"
        );
    }

    #[tokio::test]
    async fn plan_phases_cache_invalidates_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("PLAN.md");
        std::fs::write(
            &plan_path,
            "### v0.1.0 — Original Title\n<!-- status: pending -->\n",
        )
        .unwrap();

        let state = Arc::new(AppState::new(
            std::path::PathBuf::from(dir.path()),
            crate::config::DaemonConfig::default(),
        ));

        let first = call_get_plan_phases(&state).await;
        assert_eq!(first[0]["title"], "Original Title");

        // A real mtime bump (the normal case — no artificial set_modified)
        // must invalidate the cache and pick up the new content.
        let bumped = std::fs::metadata(&plan_path).unwrap().modified().unwrap()
            + std::time::Duration::from_secs(1);
        std::fs::write(
            &plan_path,
            "### v0.1.0 — Changed Title\n<!-- status: pending -->\n",
        )
        .unwrap();
        std::fs::File::open(&plan_path)
            .unwrap()
            .set_modified(bumped)
            .unwrap();

        let second = call_get_plan_phases(&state).await;
        assert_eq!(
            second[0]["title"], "Changed Title",
            "a real mtime change must invalidate PlanCache"
        );
    }

    #[tokio::test]
    async fn plan_cache_goals_scan_reused_within_ttl() {
        let cache = PlanCache::new();
        assert!(cache.get_goals_scan().await.is_none());

        let mut states = std::collections::HashMap::new();
        states.insert("v0.1.0".to_string(), "running".to_string());
        cache
            .set_goals_scan(states.clone(), Default::default())
            .await;

        let cached = cache.get_goals_scan().await;
        assert_eq!(
            cached.map(|(s, _)| s),
            Some(states),
            "goals scan should be served from cache within the TTL window"
        );
    }
}
