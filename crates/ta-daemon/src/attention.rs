// attention.rs: One feed of everything waiting on a person, and the relay
// endpoint to act on it (CR-02 items 7 and 8; contract in
// docs/superpowers/specs/2026-10-11-attention-feed-and-human-assertions-contract.md).
//
//   GET  /api/attention            list: ?user= ?role= ?project= ?since= ?limit=
//   GET  /api/attention/count      {open, by_kind} for a badge, same filters
//   POST /api/attention/{id}/act   approve | deny | answer, with the local
//                                  credential or a signed human assertion
//   events on /api/events          attention.created / .updated / .resolved
//
// Item kinds, each read from state TA already keeps:
//   draft_review    drafts in Draft or PendingReview         .ta/pr_packages/*.json
//   agent_question  `ta_ask_human` questions                 question registry + .ta/interactions/pending/
//   blocked_goal    goals in AwaitingInput with no question  .ta/goals/*.json
//   dead_letter     wake messages given up on                .ta/wake-dead-letter.jsonl
// Resolved items are kept for a week in `.ta/attention/resolved.jsonl` so a
// poller that missed the moment still sees what happened and who accepted it.
//
// The feed is READ-ONLY and carries no secrets: summaries are one sanitized
// line, dead letters name the message but never the error tail, and the
// provenance block is the capped, untrusted-data block the draft API returns.
// `act` is the only place an assertion is accepted.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use base64::Engine;
use chrono::{DateTime, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ta_changeset::draft_package::{DraftPackage, DraftStatus};
use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
use ta_goal::provenance::RequestProvenance;
use ta_goal::GoalRunState;

use crate::api::auth::CallerIdentity;
use crate::api::AppState;
use crate::assertion::{
    canonical_json, sha256_tagged, AssertionRequest, ItemFacts, VerifiedAssertion,
};
use crate::human_gate::{GatedAction, GatedCaller};

/// File of acknowledged dead-letter message ids, one JSON `{"msg_id": ...}` per line.
pub const DEAD_LETTER_ACK_FILE: &str = "wake-dead-letter-acked.jsonl";
/// Longest summary, in characters (contract section 3).
pub const MAX_SUMMARY_CHARS: usize = 280;
/// Longest item id, in characters (contract section 3).
pub const MAX_ID_CHARS: usize = 128;
/// How often the background tracker re-reads the sources.
pub const POLL_INTERVAL_SECS: u64 = 2;
/// How long a resolved item stays in the feed.
const RESOLVED_RETENTION_DAYS: i64 = 7;
/// Most resolved items read back from a project's ledger.
const RESOLVED_TAIL: usize = 500;
const DEFAULT_PAGE: usize = 100;
const MAX_PAGE: usize = 500;
/// Longest answer text `act` will relay.
const MAX_ANSWER_BYTES: usize = 16 * 1024;
const LEDGER_FILE: &str = "resolved.jsonl";
const AUDIT_FILE: &str = "audit.jsonl";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    DraftReview,
    AgentQuestion,
    BlockedGoal,
    DeadLetter,
}

impl AttentionKind {
    fn as_str(self) -> &'static str {
        match self {
            AttentionKind::DraftReview => "draft_review",
            AttentionKind::AgentQuestion => "agent_question",
            AttentionKind::BlockedGoal => "blocked_goal",
            AttentionKind::DeadLetter => "dead_letter",
        }
    }
}

/// Who an item is waiting on: `{type, value}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WaitingOn {
    /// `user`, `role` or `any_maintainer`.
    #[serde(rename = "type")]
    pub kind: String,
    pub value: Option<String>,
}

impl WaitingOn {
    fn user(id: &str) -> Self {
        Self {
            kind: "user".into(),
            value: Some(id.to_string()),
        }
    }
    #[cfg(test)]
    fn role(r: &str) -> Self {
        Self {
            kind: "role".into(),
            value: Some(r.to_string()),
        }
    }
    fn any_maintainer() -> Self {
        Self {
            kind: "any_maintainer".into(),
            value: None,
        }
    }
    /// The requester of a goal if known, otherwise any maintainer.
    fn requester_or_any(p: Option<&RequestProvenance>) -> Self {
        p.map(|p| Self::user(&p.requester.id))
            .unwrap_or_else(Self::any_maintainer)
    }
}

// ── Provenance as the feed shows it (contract section 3) ───────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedRequester {
    pub id: String,
    pub display_name: Option<String>,
    pub channel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedExcerpt {
    pub text: String,
    /// Always true: data from outside TA, never instructions.
    pub untrusted: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedChain {
    pub candidate_id: Option<String>,
    pub cos_goal_id: Option<String>,
    pub worker_goal_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedProvenance {
    pub requester: FeedRequester,
    pub excerpt: Option<FeedExcerpt>,
    pub reason: Option<String>,
    pub objective: Option<String>,
    pub chain: FeedChain,
}

impl From<&RequestProvenance> for FeedProvenance {
    fn from(p: &RequestProvenance) -> Self {
        Self {
            requester: FeedRequester {
                id: p.requester.id.clone(),
                display_name: p.requester.display_name.clone(),
                channel: p.requester.channel.clone(),
            },
            excerpt: p.message_excerpt.as_ref().map(|e| FeedExcerpt {
                text: e.text.clone(),
                untrusted: true,
                truncated: e.truncated,
            }),
            reason: p.delegation_reason.clone(),
            objective: p.objective.clone(),
            chain: FeedChain {
                candidate_id: p.ids.candidate_id.clone(),
                cos_goal_id: p.ids.cos_goal_id.clone(),
                worker_goal_id: p.ids.worker_goal_id.clone(),
            },
        }
    }
}

/// One accepted human assertion, recorded on the resolved item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceptedAssertion {
    pub jti: String,
    pub user_id: String,
    pub action: String,
    pub at: DateTime<Utc>,
    pub escalated: bool,
    pub self_approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionItem {
    /// Stable for the life of the underlying thing: `att_draft_<id>`,
    /// `att_question_<id>`, `att_goal_<id>`, `att_wake_<msg_id>`.
    pub id: String,
    pub kind: AttentionKind,
    pub project: String,
    /// `open` or `resolved`.
    pub status: String,
    /// `sha256:` over the canonical JSON of id, kind, summary, provenance, actions.
    pub item_digest: String,
    pub waiting_on: WaitingOn,
    pub summary: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub candidate_id: Option<String>,
    pub goal_id: Option<String>,
    pub draft_id: Option<String>,
    pub interaction_id: Option<String>,
    /// Null when the work did not come from an agent-side requester.
    pub provenance: Option<FeedProvenance>,
    /// What a UI may offer: `review`, `approve`, `deny`, `answer`.
    pub actions: Vec<String>,
    /// Relative paths: `review` (read the thing) and `act` (the relay endpoint).
    pub links: BTreeMap<String, String>,
    /// Seconds since `created_at`, filled in when listed.
    pub age_secs: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_assertions: Vec<AcceptedAssertion>,
}

/// The item id for `key` under `prefix`, shortened with a hash if it would
/// exceed the 128-character contract limit.
fn make_id(prefix: &str, key: &str) -> String {
    let id = format!("{prefix}{key}");
    if id.chars().count() <= MAX_ID_CHARS {
        return id;
    }
    let hash = sha256_tagged(key.as_bytes());
    format!("{prefix}{}", &hash["sha256:".len().."sha256:".len() + 40])
}

/// One line, no control characters, at most `MAX_SUMMARY_CHARS`.
pub(crate) fn one_line(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() > MAX_SUMMARY_CHARS {
        let cut: String = flat.chars().take(MAX_SUMMARY_CHARS - 3).collect();
        format!("{}...", cut.trim_end())
    } else {
        flat
    }
}

/// The value whose canonical JSON the item digest covers (contract section 3).
/// Shared with the golden-vector generator so the two cannot drift.
pub(crate) fn digest_input(
    id: &str,
    kind: AttentionKind,
    summary: &str,
    provenance: &Option<FeedProvenance>,
    actions: &[String],
) -> Value {
    json!({
        "id": id,
        "kind": kind.as_str(),
        "summary": summary,
        "provenance": provenance,
        "actions": actions,
    })
}

/// The digest the person's assertion must cover (contract section 3).
pub(crate) fn item_digest(
    id: &str,
    kind: AttentionKind,
    summary: &str,
    provenance: &Option<FeedProvenance>,
    actions: &[String],
) -> String {
    let v = digest_input(id, kind, summary, provenance, actions);
    sha256_tagged(canonical_json(&v).as_bytes())
}

struct NewItem<'a> {
    id: String,
    kind: AttentionKind,
    project: &'a str,
    waiting_on: WaitingOn,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    summary: String,
    provenance: Option<&'a RequestProvenance>,
    actions: &'a [&'a str],
    candidate_id: Option<String>,
    goal_id: Option<String>,
    draft_id: Option<String>,
    interaction_id: Option<String>,
    review_link: Option<String>,
}

fn build(n: NewItem<'_>) -> AttentionItem {
    let provenance = n.provenance.map(FeedProvenance::from);
    let summary = one_line(&n.summary);
    let actions: Vec<String> = n.actions.iter().map(|a| a.to_string()).collect();
    let mut links = BTreeMap::new();
    if let Some(l) = n.review_link {
        links.insert("review".to_string(), l);
    }
    if actions.iter().any(|a| a != "review") {
        links.insert("act".to_string(), format!("/api/attention/{}/act", n.id));
    }
    let candidate_id = n
        .candidate_id
        .or_else(|| n.provenance.and_then(|p| p.ids.candidate_id.clone()));
    AttentionItem {
        item_digest: item_digest(&n.id, n.kind, &summary, &provenance, &actions),
        id: n.id,
        kind: n.kind,
        project: n.project.to_string(),
        status: "open".into(),
        waiting_on: n.waiting_on,
        summary,
        created_at: n.created_at,
        updated_at: n.updated_at,
        candidate_id,
        goal_id: n.goal_id,
        draft_id: n.draft_id,
        interaction_id: n.interaction_id,
        provenance,
        actions,
        links,
        age_secs: 0,
        accepted_assertions: Vec::new(),
    }
}

impl AttentionItem {
    /// Digest of everything except the age, to detect `attention.updated`.
    fn fingerprint(&self) -> String {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("age_secs");
        }
        sha256_tagged(canonical_json(&v).as_bytes())
    }

    fn facts(&self) -> ItemFacts {
        ItemFacts {
            id: self.id.clone(),
            project: self.project.clone(),
            digest: self.item_digest.clone(),
            waiting_on_type: self.waiting_on.kind.clone(),
            waiting_on_value: self.waiting_on.value.clone(),
            requester_id: self.provenance.as_ref().map(|p| p.requester.id.clone()),
            agent_originated: self.provenance.is_some(),
        }
    }
}

// ── Collection ──────────────────────────────────────────────────────────────

/// A pending agent question as read from either source.
#[derive(Debug, Clone)]
struct QuestionRecord {
    interaction_id: String,
    goal_id: Option<String>,
    question: String,
    created_at: DateTime<Utc>,
}

fn load_file_questions(root: &Path) -> Vec<QuestionRecord> {
    #[derive(Deserialize)]
    struct F {
        interaction_id: String,
        goal_id: Option<String>,
        question: String,
        created_at: Option<String>,
    }
    let dir = root.join(".ta").join("interactions").join("pending");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<F>(&t).ok())
        .map(|f| QuestionRecord {
            created_at: f
                .created_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(Utc::now),
            interaction_id: f.interaction_id,
            goal_id: f.goal_id,
            question: f.question,
        })
        .collect()
}

fn goal_provenance(root: &Path, goal_id: &str) -> Option<RequestProvenance> {
    let id = uuid::Uuid::parse_str(goal_id).ok()?;
    let dir = root.join(".ta").join("goals");
    if !dir.is_dir() {
        return None;
    }
    ta_goal::GoalRunStore::new(&dir)
        .ok()?
        .get(id)
        .ok()
        .flatten()
        .and_then(|g| g.request_provenance)
}

fn modified_at(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(DateTime::<Utc>::from)
}

fn draft_items(project: &str, root: &Path, out: &mut Vec<AttentionItem>) {
    let dir = root.join(".ta").join("pr_packages");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        if entry.path().extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(pkg) = serde_json::from_str::<DraftPackage>(&text) else {
            continue;
        };
        if !matches!(pkg.status, DraftStatus::Draft | DraftStatus::PendingReview) {
            continue;
        }
        let id = pkg.package_id;
        let files = pkg.changes.artifacts.len();
        let updated = modified_at(&entry.path())
            .map(|m| m.max(pkg.created_at))
            .unwrap_or(pkg.created_at);
        out.push(build(NewItem {
            id: make_id("att_draft_", &id.to_string()),
            kind: AttentionKind::DraftReview,
            project,
            // TA raises draft reviews for any maintainer; who is shown the item
            // is the signing system's decision (contract section 9).
            waiting_on: WaitingOn::any_maintainer(),
            created_at: pkg.created_at,
            updated_at: updated,
            summary: format!(
                "Draft awaiting review: {} ({} file{} changed)",
                pkg.goal.title,
                files,
                if files == 1 { "" } else { "s" }
            ),
            provenance: pkg.request_provenance.as_ref(),
            actions: &["review", "approve", "deny"],
            candidate_id: None,
            goal_id: Some(pkg.goal.goal_id.to_string()),
            draft_id: Some(id.to_string()),
            interaction_id: None,
            review_link: Some(format!("/api/drafts/{id}")),
        }));
    }
}

fn question_items(
    project: &str,
    root: &Path,
    in_process: &[QuestionRecord],
    out: &mut Vec<AttentionItem>,
) -> Vec<String> {
    // Interaction ids already shown, so an AwaitingInput goal is not listed twice.
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut all = load_file_questions(root);
    all.extend(in_process.iter().cloned());
    for q in all {
        if seen.insert(q.interaction_id.clone(), ()).is_some() {
            continue;
        }
        let prov = q.goal_id.as_deref().and_then(|g| goal_provenance(root, g));
        let id = &q.interaction_id;
        out.push(build(NewItem {
            id: make_id("att_question_", id),
            kind: AttentionKind::AgentQuestion,
            project,
            waiting_on: WaitingOn::requester_or_any(prov.as_ref()),
            created_at: q.created_at,
            updated_at: q.created_at,
            summary: format!("Agent asks: {}", q.question),
            provenance: prov.as_ref(),
            actions: &["answer"],
            candidate_id: None,
            goal_id: q.goal_id.clone(),
            draft_id: None,
            interaction_id: Some(id.clone()),
            review_link: None,
        }));
    }
    seen.into_keys().collect()
}

fn blocked_goal_items(
    project: &str,
    root: &Path,
    question_ids: &[String],
    out: &mut Vec<AttentionItem>,
) {
    let dir = root.join(".ta").join("goals");
    if !dir.is_dir() {
        return;
    }
    let Ok(store) = ta_goal::GoalRunStore::new(&dir) else {
        return;
    };
    let Ok(goals) = store.list() else {
        return;
    };
    for g in goals {
        let (why, interaction) = match &g.state {
            GoalRunState::AwaitingInput {
                interaction_id,
                question_preview,
            } => {
                if question_ids.contains(&interaction_id.to_string()) {
                    continue; // already listed as an agent question
                }
                (
                    format!("waiting for input: {question_preview}"),
                    Some(interaction_id.to_string()),
                )
            }
            GoalRunState::Custom { tag }
                if tag == "blocked_on_person" || tag == "waiting_for_human" =>
            {
                (format!("blocked on a person ({tag})"), None)
            }
            _ => continue,
        };
        out.push(build(NewItem {
            id: make_id("att_goal_", &g.goal_run_id.to_string()),
            kind: AttentionKind::BlockedGoal,
            project,
            waiting_on: WaitingOn::requester_or_any(g.request_provenance.as_ref()),
            created_at: g.created_at,
            updated_at: g.updated_at,
            summary: format!("Goal \"{}\" is {}", g.title, why),
            provenance: g.request_provenance.as_ref(),
            actions: &["review"],
            candidate_id: None,
            goal_id: Some(g.goal_run_id.to_string()),
            draft_id: None,
            interaction_id: interaction,
            review_link: Some(format!("/api/goals/{}/output", g.goal_run_id)),
        }));
    }
}

fn dead_letter_items(project: &str, root: &Path, out: &mut Vec<AttentionItem>) {
    #[derive(Deserialize)]
    struct Rec {
        msg_id: String,
        role: String,
        attempts: u32,
        dead_lettered_at: DateTime<Utc>,
    }
    #[derive(Deserialize)]
    struct Ack {
        msg_id: String,
    }
    let ta = root.join(".ta");
    let Ok(text) = std::fs::read_to_string(crate::wake_retry::dead_letter_path(root)) else {
        return;
    };
    let acked: Vec<String> = std::fs::read_to_string(ta.join(DEAD_LETTER_ACK_FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Ack>(l).ok())
        .map(|a| a.msg_id)
        .collect();
    for rec in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Rec>(l).ok())
    {
        if acked.contains(&rec.msg_id) {
            continue;
        }
        out.push(build(NewItem {
            id: make_id("att_wake_", &rec.msg_id),
            kind: AttentionKind::DeadLetter,
            project,
            // TA's own roles (implementer, chief-of-staff) route agents; they
            // are never a human approval role, so this waits on a maintainer.
            waiting_on: WaitingOn::any_maintainer(),
            created_at: rec.dead_lettered_at,
            updated_at: rec.dead_lettered_at,
            // Never the error tail: it can quote environment or tokens.
            summary: format!(
                "Wake message {} for role {} was given up after {} attempts",
                rec.msg_id, rec.role, rec.attempts
            ),
            provenance: None,
            actions: &[],
            candidate_id: None,
            goal_id: None,
            draft_id: None,
            interaction_id: None,
            review_link: None,
        }));
    }
}

fn sort_items(items: &mut [AttentionItem]) {
    items.sort_by(|a, b| {
        a.updated_at
            .trunc_subsecs(6)
            .cmp(&b.updated_at.trunc_subsecs(6))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Everything waiting on a person in one project.
pub fn collect_project(
    project: &str,
    root: &Path,
    in_process_questions: &[crate::question_registry::PendingQuestion],
) -> Vec<AttentionItem> {
    let qs: Vec<QuestionRecord> = in_process_questions
        .iter()
        .map(|q| QuestionRecord {
            interaction_id: q.interaction_id.to_string(),
            goal_id: q.goal_id.map(|g| g.to_string()),
            question: q.question.clone(),
            created_at: q.created_at,
        })
        .collect();
    let mut out = Vec::new();
    draft_items(project, root, &mut out);
    let qids = question_items(project, root, &qs, &mut out);
    blocked_goal_items(project, root, &qids, &mut out);
    dead_letter_items(project, root, &mut out);
    sort_items(&mut out);
    out
}

fn project_name(state: &AppState) -> String {
    state
        .project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string())
}

/// (project name, root) for every project the daemon serves.
pub fn project_roots(state: &AppState) -> Vec<(String, PathBuf)> {
    let projects = state.project_registry.list();
    if projects.is_empty() {
        return vec![(project_name(state), state.project_root.clone())];
    }
    projects
        .iter()
        .map(|c| (c.name.clone(), c.path.clone()))
        .collect()
}

/// Every project's open items, unfiltered, with ages filled in.
pub async fn collect_all(state: &AppState) -> Vec<AttentionItem> {
    let pending = state.question_registry.list_pending().await;
    let now = Utc::now();
    let mut items = Vec::new();
    for (i, (name, root)) in project_roots(state).iter().enumerate() {
        // The in-process registry is daemon-wide; attribute it to the first
        // project (single-project mode has exactly one).
        let qs: &[crate::question_registry::PendingQuestion] = if i == 0 { &pending } else { &[] };
        items.extend(collect_project(name, root, qs));
    }
    for it in &mut items {
        it.age_secs = (now - it.created_at).num_seconds().max(0);
    }
    sort_items(&mut items);
    items
}

// ── Resolution ledger and audit ─────────────────────────────────────────────

/// One resolved item as stored in `.ta/attention/resolved.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerEntry {
    item: AttentionItem,
    /// `approve`, `deny`, `answer`, or `external` for a resolution TA only observed.
    action: String,
    /// What the caller was told; returned again to a repeated delivery.
    result: Value,
    /// `agent_delegated` or `direct` for an action taken through `/act`
    /// (CR-03 item 6); absent for resolutions TA only observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actor_context: Option<String>,
    /// Why the client judged the context to be an agent (delegated only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    delegated_signals: Vec<String>,
}

fn attention_dir(root: &Path) -> PathBuf {
    root.join(".ta").join("attention")
}

fn append_jsonl(path: &Path, value: &Value) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{value}")
}

fn ledger_append(root: &Path, entry: &LedgerEntry) {
    let path = attention_dir(root).join(LEDGER_FILE);
    let value = serde_json::to_value(entry).unwrap_or_default();
    if let Err(e) = append_jsonl(&path, &value) {
        tracing::warn!(
            path = %path.display(),
            item = %entry.item.id,
            error = %e,
            "could not record a resolved attention item; the action itself was done. \
             Check that the directory is writable"
        );
    }
}

fn ledger_read(root: &Path) -> Vec<LedgerEntry> {
    let path = attention_dir(root).join(LEDGER_FILE);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let cutoff = Utc::now() - chrono::Duration::days(RESOLVED_RETENTION_DAYS);
    let mut entries: Vec<LedgerEntry> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<LedgerEntry>(l).ok())
        .filter(|e| e.item.updated_at >= cutoff)
        .collect();
    if entries.len() > RESOLVED_TAIL {
        entries.drain(..entries.len() - RESOLVED_TAIL);
    }
    entries
}

fn ledger_find(roots: &[(String, PathBuf)], id: &str) -> Option<(PathBuf, LedgerEntry)> {
    roots.iter().find_map(|(_, root)| {
        ledger_read(root)
            .into_iter()
            .rev()
            .find(|e| e.item.id == id)
            .map(|e| (root.clone(), e))
    })
}

/// Resolved items of every project, newest last, with ages filled in.
pub fn collect_resolved(state: &AppState) -> Vec<AttentionItem> {
    let now = Utc::now();
    let mut items: Vec<AttentionItem> = project_roots(state)
        .iter()
        .flat_map(|(_, root)| ledger_read(root))
        .map(|e| e.item)
        .collect();
    for it in &mut items {
        it.age_secs = (now - it.created_at).num_seconds().max(0);
    }
    sort_items(&mut items);
    items
}

fn audit(root: &Path, record: Value) {
    let path = attention_dir(root).join(AUDIT_FILE);
    if let Err(e) = append_jsonl(&path, &record) {
        tracing::warn!(path = %path.display(), error = %e, "could not write the human-action audit record");
    }
    tracing::info!(audit = %record, "human action on an attention item");
}

// ── Filtering and cursors ───────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, Clone)]
pub struct AttentionQuery {
    /// Items waiting on this user (or on any maintainer, who this user may be).
    pub user: Option<String>,
    /// Items waiting on this role. Combined with `user` as OR ("me or my role").
    pub role: Option<String>,
    /// Only this project. Without it, every project.
    pub project: Option<String>,
    /// Opaque cursor from a previous `next_cursor`, or an RFC 3339 `updated_at`.
    pub since: Option<String>,
    /// Page size (default 100, at most 500).
    pub limit: Option<usize>,
}

pub fn filter(items: Vec<AttentionItem>, q: &AttentionQuery) -> Vec<AttentionItem> {
    items
        .into_iter()
        .filter(|it| q.project.as_deref().is_none_or(|p| it.project == p))
        .filter(|it| {
            if q.user.is_none() && q.role.is_none() {
                return true;
            }
            let any = it.waiting_on.kind == "any_maintainer";
            let by_user = q.user.as_deref().is_some_and(|u| {
                any || (it.waiting_on.kind == "user" && it.waiting_on.value.as_deref() == Some(u))
            });
            let by_role = q.role.as_deref().is_some_and(|r| {
                (it.waiting_on.kind == "role" && it.waiting_on.value.as_deref() == Some(r))
                    || (any && ta_credentials::human::DEFAULT_MAINTAINER_ROLES.contains(&r))
            });
            by_user || by_role
        })
        .collect()
}

fn cursor_of(it: &AttentionItem) -> String {
    let raw = format!(
        "{}|{}",
        it.updated_at
            .trunc_subsecs(6)
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        it.id
    );
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// (updated_at, id) a cursor stands for; the id is empty for a bare timestamp.
fn parse_cursor(since: &str) -> Result<(DateTime<Utc>, String), String> {
    if let Ok(raw) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(since) {
        if let Some((ts, id)) = String::from_utf8_lossy(&raw).split_once('|') {
            if let Ok(ts) = DateTime::parse_from_rfc3339(ts) {
                return Ok((ts.with_timezone(&Utc), id.to_string()));
            }
        }
    }
    DateTime::parse_from_rfc3339(since)
        .map(|ts| (ts.with_timezone(&Utc), String::new()))
        .map_err(|_| {
            format!(
                "'since' must be a next_cursor value from a previous GET /api/attention or an \
                 RFC 3339 timestamp (for example 2026-10-11T01:35:54Z); got '{since}'"
            )
        })
}

// ── Event tracker ───────────────────────────────────────────────────────────

/// What one `tick` changed (ids), for tests and logging.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickResult {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub resolved: Vec<String>,
}

struct Seen {
    fingerprint: String,
    item: AttentionItem,
}

/// Diffs successive snapshots of the feed into events. The first tick only
/// records a baseline, so a daemon restart does not replay old items as new.
#[derive(Default)]
pub struct AttentionTracker {
    seen: Mutex<Option<BTreeMap<String, Seen>>>,
}

impl AttentionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compare `current` with the previous snapshot and append one event per
    /// change to the store in `events_dir`. A vanished item emits
    /// `attention.resolved` once, is recorded in its project's resolved ledger
    /// (unless `act` already did), and is forgotten, so it cannot emit twice.
    pub fn tick(
        &self,
        current: &[AttentionItem],
        events_dir: &Path,
        roots: &[(String, PathBuf)],
    ) -> TickResult {
        let mut guard = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let mut next: BTreeMap<String, Seen> = current
            .iter()
            .map(|it| {
                (
                    it.id.clone(),
                    Seen {
                        fingerprint: it.fingerprint(),
                        item: it.clone(),
                    },
                )
            })
            .collect();
        let mut result = TickResult::default();
        if let Some(prev) = guard.as_ref() {
            let store = FsEventStore::new(events_dir);
            let emit = |ev: SessionEvent| {
                if let Err(e) = store.append(&EventEnvelope::new(ev)) {
                    tracing::warn!(
                        events_dir = %events_dir.display(),
                        error = %e,
                        "could not write an attention event; the feed endpoint still shows the item"
                    );
                }
            };
            for it in current {
                match prev.get(&it.id) {
                    None => {
                        result.created.push(it.id.clone());
                        emit(SessionEvent::AttentionCreated {
                            item: serde_json::to_value(it).unwrap_or_default(),
                        });
                    }
                    Some(old) if old.fingerprint != next[&it.id].fingerprint => {
                        result.updated.push(it.id.clone());
                        emit(SessionEvent::AttentionUpdated {
                            item: serde_json::to_value(it).unwrap_or_default(),
                        });
                    }
                    Some(_) => {}
                }
            }
            for (id, old) in prev {
                if !next.contains_key(id) {
                    result.resolved.push(id.clone());
                    if let Some((_, root)) = roots.iter().find(|(n, _)| *n == old.item.project) {
                        if ledger_find(&[(old.item.project.clone(), root.clone())], id).is_none() {
                            let mut item = old.item.clone();
                            item.status = "resolved".into();
                            item.updated_at = Utc::now();
                            ledger_append(
                                root,
                                &LedgerEntry {
                                    item,
                                    action: "external".into(),
                                    result: json!({
                                        "status": "resolved",
                                        "note": "resolved outside the relay endpoint (CLI, Studio, timeout or the underlying state moved on)"
                                    }),
                                    actor_context: None,
                                    delegated_signals: Vec::new(),
                                },
                            );
                        }
                    }
                    emit(SessionEvent::AttentionResolved {
                        item_id: id.clone(),
                        kind: old.item.kind.as_str().to_string(),
                        project: old.item.project.clone(),
                    });
                }
            }
        }
        *guard = Some(std::mem::take(&mut next));
        result
    }
}

/// Start the background tracker for the daemon's lifetime.
pub fn start(state: &Arc<AppState>, shutdown: Arc<tokio::sync::Notify>) {
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            let items = collect_all(&state).await;
            let roots = project_roots(&state);
            let result = state.attention.tick(&items, &state.events_dir, &roots);
            if !(result.created.is_empty()
                && result.updated.is_empty()
                && result.resolved.is_empty())
            {
                tracing::debug!(
                    created = result.created.len(),
                    updated = result.updated.len(),
                    resolved = result.resolved.len(),
                    "attention feed changed"
                );
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS)) => {}
                _ = shutdown.notified() => break,
            }
        }
    });
}

// ── Read routes ─────────────────────────────────────────────────────────────

fn error_response(status: StatusCode, error: String, hint: &str) -> Response {
    (status, Json(json!({ "error": error, "hint": hint }))).into_response()
}

/// `GET /api/attention`
pub async fn list(State(state): State<Arc<AppState>>, Query(q): Query<AttentionQuery>) -> Response {
    let since = match q.since.as_deref().map(parse_cursor).transpose() {
        Ok(c) => c,
        Err(msg) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                msg,
                "Pass the next_cursor of the previous response unchanged, or omit 'since' to start from the beginning.",
            )
        }
    };
    let mut items = filter(collect_all(&state).await, &q);
    items.extend(filter(collect_resolved(&state), &q));
    sort_items(&mut items);
    if let Some((ts, id)) = &since {
        items.retain(|i| (i.updated_at.trunc_subsecs(6), i.id.as_str()) > (*ts, id.as_str()));
    }
    let limit = q.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = items.last().map(cursor_of).or_else(|| q.since.clone());

    let state_dir = state.human_gate.state_dir();
    let install_id = ta_credentials::human::install_id(state_dir)
        .map_err(|e| {
            tracing::warn!(error = %e, "could not read or create the install id; the feed omits it");
        })
        .ok();
    let keys: Vec<Value> = ta_credentials::human::load_assertion_keys(state_dir)
        .unwrap_or_default()
        .iter()
        .map(key_metadata)
        .collect();
    Json(json!({
        "count": items.len(),
        "items": items,
        "has_more": has_more,
        "next_cursor": next_cursor,
        "install_id": install_id,
        "keys": keys,
    }))
    .into_response()
}

/// What the feed publishes about one registered key: enough for the signing
/// system to check a role against TA's floor at bind time. Never key material.
pub(crate) fn key_metadata(k: &ta_credentials::human::AssertionKey) -> Value {
    json!({
        "kid": k.kid,
        "fingerprint": k.fingerprint(),
        "role_order": k.role_order,
        "maintainer_roles": k.maintainer_roles,
        "escalation_min_role": k.escalation_min_role,
    })
}

/// `GET /api/attention/count`
pub async fn count(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AttentionQuery>,
) -> impl IntoResponse {
    let items = filter(collect_all(&state).await, &q);
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    for it in &items {
        *by_kind.entry(it.kind.as_str()).or_default() += 1;
    }
    Json(json!({ "open": items.len(), "by_kind": by_kind }))
}

// ── The relay endpoint ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActRequest {
    /// `approve`, `deny` or `answer`.
    pub action: String,
    /// The answer text, for `answer`.
    #[serde(default)]
    pub answer: Option<String>,
    /// Why, for `deny` (optional, shown on the denied draft).
    #[serde(default)]
    pub reason: Option<String>,
    /// The compact signed assertion of the acting human.
    #[serde(default)]
    pub assertion: Option<String>,
}

fn allowed_actions(kind: AttentionKind) -> &'static [&'static str] {
    match kind {
        AttentionKind::DraftReview => &["approve", "deny"],
        AttentionKind::AgentQuestion => &["answer"],
        AttentionKind::BlockedGoal | AttentionKind::DeadLetter => &[],
    }
}

/// Who proved they are a human.
enum Actor {
    /// The local credential (or its explicit opt-ins): no named user.
    Local(crate::human_gate::ProofKind),
    Asserted(Box<VerifiedAssertion>),
}

fn refusal(status: StatusCode, what: &str, why: &str, hint: &str, code: &str) -> Response {
    (
        status,
        Json(json!({ "error": format!("{what} refused: {why}"), "hint": hint, "reason": code })),
    )
        .into_response()
}

/// `POST /api/attention/{id}/act`
pub async fn act(
    State(state): State<Arc<AppState>>,
    AxPath(item_id): AxPath<String>,
    Extension(identity): Extension<CallerIdentity>,
    headers: HeaderMap,
    body: Result<Json<ActRequest>, JsonRejection>,
) -> Response {
    let what = format!("POST /api/attention/{item_id}/act");
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("{what} refused: the body is not a valid act request ({e})"),
                "Send {\"action\": \"approve|deny|answer\", \"answer\": \"...\", \"assertion\": \"<compact assertion>\"}; see docs/USAGE.md, 'Human-asserted actions'.",
            )
        }
    };
    if !matches!(req.action.as_str(), "approve" | "deny" | "answer") {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!("{what} refused: unknown action '{}'", req.action),
            "action must be approve, deny or answer.",
        );
    }
    if req
        .answer
        .as_deref()
        .is_some_and(|a| a.len() > MAX_ANSWER_BYTES)
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!("{what} refused: the answer is longer than {MAX_ANSWER_BYTES} bytes"),
            "Send a shorter answer.",
        );
    }

    // One act at a time: "first valid action wins" must hold under concurrent
    // delivery of two assertions for one item.
    let _guard = state.act_lock.lock().await;
    let roots = project_roots(&state);
    let now = Utc::now();
    let now_ts = now.timestamp();

    let open = collect_all(&state)
        .await
        .into_iter()
        .find(|i| i.id == item_id);
    let resolved = if open.is_none() {
        ledger_find(&roots, &item_id)
    } else {
        None
    };
    if open.is_none() && resolved.is_none() {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("{what} refused: there is no attention item '{item_id}'"),
            "List items with GET /api/attention; an item that was resolved a week ago is no longer kept.",
        );
    }

    // ── Proof of a human ────────────────────────────────────────────────────
    let gate = &state.human_gate;
    let action_kind = match req.action.as_str() {
        "approve" => GatedAction::Approve,
        "deny" => GatedAction::Deny,
        _ => GatedAction::Answer,
    };
    let caller = GatedCaller {
        is_local: identity.is_local,
        scope: identity.scope,
        method: &Method::POST,
        path: &format!("/api/attention/{item_id}/act"),
    };
    let mut actor: Option<Actor> = gate.credential_proof(&headers).map(|p| Actor::Local(p.via));
    let mut verified_token = None;
    if actor.is_none() {
        if let Some(token) = req.assertion.as_deref() {
            verified_token = Some(token);
        } else {
            match gate.decide_without_assertion(
                &what,
                action_kind,
                &headers,
                &state.daemon_config.auth,
                &caller,
            ) {
                Ok(p) => actor = Some(Actor::Local(p.via)),
                Err(r) => {
                    tracing::warn!(item = %item_id, action = %req.action, "act refused: no human proof");
                    return (
                        StatusCode::FORBIDDEN,
                        Json(json!({ "error": r.error, "hint": r.hint, "reason": "no_proof" })),
                    )
                        .into_response();
                }
            }
        }
    }

    // ── Already resolved: first valid action won; say so, act on nothing ────
    let Some(item) = open else {
        let (_, entry) = resolved.expect("checked above");
        let mut replay = false;
        if actor.is_none() {
            let token = verified_token.unwrap_or_default();
            match gate.verifier().authenticate(token) {
                Ok(a) => {
                    replay = entry
                        .item
                        .accepted_assertions
                        .iter()
                        .any(|x| x.jti == a.jti);
                }
                Err(e) => {
                    tracing::warn!(item = %item_id, reason = %e, "act refused: assertion not authentic");
                    return refusal(
                        StatusCode::FORBIDDEN,
                        &what,
                        &e.to_string(),
                        "Sign a new assertion with a registered key.",
                        "assertion_rejected",
                    );
                }
            }
        }
        return Json(json!({
            "status": "already_resolved",
            "acted": false,
            "replayed": replay,
            "item_id": item_id,
            "original_action": entry.action,
            "original_result": entry.result,
            "resolved_at": entry.item.updated_at,
            "accepted_assertions": entry.item.accepted_assertions,
        }))
        .into_response();
    };

    // ── The action must fit the item ────────────────────────────────────────
    if !allowed_actions(item.kind).contains(&req.action.as_str()) {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!(
                "{what} refused: a {} item cannot be '{}'",
                item.kind.as_str(),
                req.action
            ),
            &format!(
                "Allowed for this item: {}.",
                if allowed_actions(item.kind).is_empty() {
                    "none (read it with its review link)".to_string()
                } else {
                    allowed_actions(item.kind).join(", ")
                }
            ),
        );
    }
    if req.action == "answer" && req.answer.as_deref().is_none_or(|a| a.trim().is_empty()) {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!("{what} refused: action 'answer' needs a non-empty 'answer'"),
            "Send the answer text in the 'answer' field.",
        );
    }

    // ── Verify the assertion against this exact item ────────────────────────
    let actor = match actor {
        Some(a) => a,
        None => {
            let token = verified_token.unwrap_or_default();
            let facts = item.facts();
            let areq = AssertionRequest {
                action: &req.action,
                item: &facts,
                answer: req.answer.as_deref(),
            };
            match gate.verifier().verify(token, &areq, now_ts) {
                Ok(v) => Actor::Asserted(Box::new(v)),
                Err(e) => {
                    tracing::warn!(item = %item_id, action = %req.action, reason = %e, "act refused: assertion not accepted");
                    return refusal(
                        StatusCode::FORBIDDEN,
                        &what,
                        &e.to_string(),
                        "Fix the assertion and retry; nothing was done.",
                        assertion_code(&e),
                    );
                }
            }
        }
    };

    // ── Do it ───────────────────────────────────────────────────────────────
    let root = roots
        .iter()
        .find(|(n, _)| *n == item.project)
        .map(|(_, r)| r.clone())
        .unwrap_or_else(|| state.project_root.clone());
    // CR-03 item 6: the advisory actor-context header decides the label; the
    // named human (or the credential holder) is the delegating owner.
    let claim_proof = match &actor {
        Actor::Asserted(v) => crate::human_gate::HumanProof {
            via: crate::human_gate::ProofKind::Assertion,
            user_id: Some(v.user_id.clone()),
            email: None,
        },
        Actor::Local(kind) => crate::human_gate::HumanProof::anonymous(*kind),
    };
    let claim = crate::actor_context::claim_for(&headers, Some(&claim_proof));
    let acting_user = match &actor {
        Actor::Asserted(v) => v.user_id.clone(),
        Actor::Local(_) if claim.is_delegated() => claim.owner.clone(),
        Actor::Local(_) => "human-credential".to_string(),
    };
    let result = match perform(&state, &root, &item, &req, &acting_user, &claim).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    // ── Record it ───────────────────────────────────────────────────────────
    let mut resolved_item = item.clone();
    resolved_item.status = "resolved".into();
    resolved_item.updated_at = now;
    let accepted = match &actor {
        Actor::Asserted(v) => {
            let a = AcceptedAssertion {
                jti: v.jti.clone(),
                user_id: v.user_id.clone(),
                action: v.action.clone(),
                at: now,
                escalated: v.escalated,
                self_approved: v.self_approved,
            };
            resolved_item.accepted_assertions.push(a.clone());
            Some(a)
        }
        Actor::Local(_) => None,
    };
    ledger_append(
        &root,
        &LedgerEntry {
            item: resolved_item,
            action: req.action.clone(),
            result: result.clone(),
            actor_context: Some(claim.actor_context.to_string()),
            delegated_signals: claim.signals.clone(),
        },
    );
    crate::actor_context::note_sighting(state.human_gate.state_dir(), &claim);
    let via = match &actor {
        Actor::Local(kind) => format!("{kind:?}").to_lowercase(),
        Actor::Asserted(_) => "assertion".to_string(),
    };
    let mut record = json!({
        "at": now,
        "project": item.project,
        "item_id": item.id,
        "kind": item.kind.as_str(),
        "action": req.action,
        "via": via,
        "user_id": acting_user,
        "actor_context": claim.actor_context,
    });
    if claim.is_delegated() {
        record["delegating_owner"] = json!(claim.owner);
        record["delegated_signals"] = json!(claim.signals);
    }
    if let Actor::Asserted(v) = &actor {
        record["email"] = json!(v.email);
        record["kid"] = json!(v.kid);
        record["jti"] = json!(v.jti);
        record["role"] = json!(v.role);
        record["escalated"] = json!(v.escalated);
        if v.self_approved {
            record["self_approved"] = json!(true);
        }
    }
    audit(&root, record);

    Json(json!({
        "status": "resolved",
        "acted": true,
        "item_id": item.id,
        "action": req.action,
        "result": result,
        "accepted_assertion": accepted,
    }))
    .into_response()
}

pub(crate) fn assertion_code(e: &crate::assertion::AssertionError) -> &'static str {
    use crate::assertion::AssertionError as E;
    match e {
        E::Malformed(_) => "assertion_malformed",
        E::Rejected => "assertion_rejected",
        E::WrongAudience => "wrong_audience",
        E::Mismatch(_) => "claim_mismatch",
        E::Expired => "expired",
        E::NotYetValid => "not_yet_valid",
        E::BadLifetime => "bad_lifetime",
        E::Replayed => "replayed",
        E::NotPermitted(_) => "not_permitted",
        E::SelfApproval => "self_approval",
        E::ItemChanged => "item_changed",
        E::AnswerChanged => "answer_changed",
    }
}

/// Carry out an accepted action on the underlying draft or question.
async fn perform(
    state: &AppState,
    root: &Path,
    item: &AttentionItem,
    req: &ActRequest,
    actor: &str,
    claim: &crate::actor_context::ActorClaim,
) -> Result<Value, Response> {
    match (item.kind, req.action.as_str()) {
        (AttentionKind::DraftReview, "approve" | "deny") => {
            let draft_id = item.draft_id.as_deref().unwrap_or_default();
            let uuid = uuid::Uuid::parse_str(draft_id).map_err(|_| {
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("item {} has no valid draft id", item.id),
                    "Report this: the feed item is malformed.",
                )
            })?;
            let status = if req.action == "approve" {
                DraftStatus::Approved {
                    approved_by: actor.to_string(),
                    approved_at: Utc::now(),
                }
            } else {
                DraftStatus::Denied {
                    reason: req
                        .reason
                        .clone()
                        .unwrap_or_else(|| "denied via relay".into()),
                    denied_by: actor.to_string(),
                }
            };
            let dir = root.join(".ta").join("pr_packages");
            match crate::web::update_draft_status(&dir, uuid, status, None) {
                Ok(true) => {
                    crate::actor_context::record_draft_action(
                        &dir,
                        root,
                        state.human_gate.state_dir(),
                        claim,
                        &req.action,
                        uuid,
                        "POST /api/attention/{id}/act",
                    );
                    Ok(json!({
                    "draft_id": draft_id,
                    "status": if req.action == "approve" { "approved" } else { "denied" },
                    }))
                }
                Ok(false) => Err(error_response(
                    StatusCode::NOT_FOUND,
                    format!("draft {draft_id} for item {} is gone", item.id),
                    "Refresh the feed; the draft was removed after the item was listed.",
                )),
                Err(e) => Err(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("could not update draft {draft_id}: {e}"),
                    "Check the file permissions of .ta/pr_packages and retry.",
                )),
            }
        }
        (AttentionKind::AgentQuestion, "answer") => {
            let iid = item.interaction_id.as_deref().unwrap_or_default();
            let id = uuid::Uuid::parse_str(iid).map_err(|_| {
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("item {} has no valid interaction id", item.id),
                    "Report this: the feed item is malformed.",
                )
            })?;
            crate::api::interactions::deliver_answer(
                state,
                root,
                id,
                req.answer.clone().unwrap_or_default(),
                actor,
            )
            .await;
            Ok(json!({ "interaction_id": iid, "status": "delivered" }))
        }
        _ => Err(error_response(
            StatusCode::BAD_REQUEST,
            format!("item {} does not support '{}'", item.id, req.action),
            "See the item's actions.",
        )),
    }
}

#[cfg(test)]
mod vectors;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{build_api_router, AppState};
    use crate::assertion::testkit::{claims_for, TestSigner, KID};
    use crate::config::{DaemonConfig, TokenScope};
    use crate::human_gate::HumanGate;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use ta_events::store::EventQueryFilter;
    use tower::ServiceExt;
    use uuid::Uuid;

    /// A unique marker standing in for an error tail that must never reach the
    /// feed. Deliberately not shaped like a credential, so secret scanners do not
    /// flag the test source; the test only needs a string it can search for.
    const SECRET_TAIL: &str = "upstream error: LEAK-CHECK-SENTINEL-93f1 (never expose)";

    fn draft_json(
        id: Uuid,
        title: &str,
        status: serde_json::Value,
        prov: Option<&RequestProvenance>,
    ) -> String {
        let mut v = serde_json::json!({
            "package_version": "1.0.0",
            "package_id": id.to_string(),
            "created_at": "2026-01-01T00:00:00Z",
            "goal": {"goal_id": "aabbccdd-0000-0000-0000-000000000000", "title": title,
                     "objective": "o", "success_criteria": [], "constraints": []},
            "iteration": {"iteration_id": "i", "sequence": 1,
                          "workspace_ref": {"type": "staging_dir", "ref": "t"}},
            "agent_identity": {"agent_id": "a", "agent_type": "t", "constitution_id": "d",
                               "capability_manifest_hash": "h"},
            "summary": {"what_changed": "w", "why": "y", "impact": "n", "rollback_plan": "n",
                        "open_questions": [], "alternatives_considered": []},
            "plan": {"completed_steps": [], "next_steps": [], "decision_log": []},
            "changes": {"artifacts": [], "patch_sets": [], "pending_actions": []},
            "risk": {"risk_score": 0, "findings": [], "policy_decisions": []},
            "provenance": {"inputs": [], "tool_trace_hash": "t"},
            "review_requests": {"requested_actions": [], "reviewers": [], "required_approvals": 1},
            "signatures": {"package_hash": "t", "agent_signature": "t"},
            "status": status
        });
        if let Some(p) = prov {
            v["request_provenance"] = serde_json::to_value(p).unwrap();
        }
        v.to_string()
    }

    fn provenance(user: &str) -> RequestProvenance {
        RequestProvenance::from_json(
            &serde_json::json!({
                "requester": {"id": user, "display_name": "Alice", "channel": "slack"},
                "excerpt": "please fix it",
                "reason": "the CoS thinks it is small",
                "objective": "fix the login bug",
                "candidate_id": "wayfinder-task:1#2",
                "delegating_role": "chief-of-staff"
            })
            .to_string(),
        )
        .unwrap()
    }

    fn write_draft(
        root: &Path,
        id: Uuid,
        title: &str,
        status: serde_json::Value,
        prov: Option<&RequestProvenance>,
    ) {
        let dir = root.join(".ta").join("pr_packages");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{id}.json")),
            draft_json(id, title, status, prov),
        )
        .unwrap();
    }

    fn write_question(root: &Path, id: Uuid, goal: Option<Uuid>, q: &str) {
        let dir = root.join(".ta").join("interactions").join("pending");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::json!({
                "interaction_id": id.to_string(),
                "goal_id": goal.map(|g| g.to_string()),
                "question": q,
                "context": null, "response_hint": "freeform", "choices": [], "turn": 1,
                "created_at": Utc::now().to_rfc3339(), "timeout_secs": 60
            })
            .to_string(),
        )
        .unwrap();
    }

    fn write_dead_letter(root: &Path, msg: &str, role: &str) {
        let path = crate::wake_retry::dead_letter_path(root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let line = serde_json::json!({
            "msg_id": msg, "session": "s", "role": role, "key": "k", "attempts": 3,
            "last_error_tail": SECRET_TAIL, "payload_bytes": 10,
            "first_attempt_at": Utc::now(), "last_attempt_at": Utc::now(),
            "dead_lettered_at": Utc::now()
        });
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn write_blocked_goal(root: &Path, interaction: Uuid) -> Uuid {
        let goals = root.join(".ta").join("goals");
        let store = ta_goal::GoalRunStore::new(&goals).unwrap();
        let mut g = ta_goal::GoalRun::new(
            "Blocked goal",
            "o",
            "agent",
            root.join("w"),
            goals.join("s"),
        );
        g.state = GoalRunState::AwaitingInput {
            interaction_id: interaction,
            question_preview: "which db?".into(),
        };
        g.request_provenance = Some(provenance("bob"));
        store.save(&g).unwrap();
        g.goal_run_id
    }

    fn items(root: &Path) -> Vec<AttentionItem> {
        collect_project("proj", root, &[])
    }

    fn roots_of(root: &Path) -> Vec<(String, PathBuf)> {
        vec![("proj".to_string(), root.to_path_buf())]
    }

    #[test]
    fn lists_every_kind_with_stable_ids_actions_links_and_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let draft = Uuid::new_v4();
        let applied = Uuid::new_v4();
        let q = Uuid::new_v4();
        write_draft(
            root,
            draft,
            "Fix login",
            serde_json::json!({"status": "pending_review"}),
            Some(&provenance("alice")),
        );
        write_draft(
            root,
            applied,
            "Old",
            serde_json::json!({"status": "applied", "applied_at": "2026-01-01T00:00:00Z"}),
            None,
        );
        write_question(root, q, None, "Which database should I use?");
        let blocked = write_blocked_goal(root, Uuid::new_v4());
        write_dead_letter(root, "m-1", "implementer");

        let got = items(root);
        let ids: Vec<&str> = got.iter().map(|i| i.id.as_str()).collect();
        assert!(ids.contains(&format!("att_draft_{draft}").as_str()));
        assert!(ids.contains(&format!("att_question_{q}").as_str()));
        assert!(ids.contains(&format!("att_goal_{blocked}").as_str()));
        assert!(ids.contains(&"att_wake_m-1"));
        assert!(
            !ids.iter().any(|i| i.contains(&applied.to_string())),
            "applied drafts are not waiting"
        );
        assert_eq!(got.len(), 4);
        assert!(got.iter().all(|i| i.status == "open"
            && i.item_digest.starts_with("sha256:")
            && i.id.chars().count() <= MAX_ID_CHARS
            && i.summary.chars().count() <= MAX_SUMMARY_CHARS));

        let d = got
            .iter()
            .find(|i| i.kind == AttentionKind::DraftReview)
            .unwrap();
        assert_eq!(d.waiting_on, WaitingOn::any_maintainer());
        assert_eq!(d.draft_id.as_deref(), Some(draft.to_string().as_str()));
        assert_eq!(
            d.goal_id.as_deref(),
            Some("aabbccdd-0000-0000-0000-000000000000")
        );
        assert_eq!(d.candidate_id.as_deref(), Some("wayfinder-task:1#2"));
        assert!(d.summary.contains("Fix login"));
        assert_eq!(d.actions, ["review", "approve", "deny"]);
        assert_eq!(d.links["review"], format!("/api/drafts/{draft}"));
        assert_eq!(
            d.links["act"],
            format!("/api/attention/att_draft_{draft}/act")
        );
        let p = d.provenance.as_ref().unwrap();
        assert_eq!(p.requester.id, "alice");
        let ex = p.excerpt.as_ref().unwrap();
        assert!(ex.untrusted && !ex.truncated && ex.text == "please fix it");
        assert_eq!(p.reason.as_deref(), Some("the CoS thinks it is small"));
        assert_eq!(p.objective.as_deref(), Some("fix the login bug"));
        assert_eq!(p.chain.candidate_id.as_deref(), Some("wayfinder-task:1#2"));

        let qi = got
            .iter()
            .find(|i| i.kind == AttentionKind::AgentQuestion)
            .unwrap();
        assert_eq!(qi.waiting_on, WaitingOn::any_maintainer());
        assert_eq!(qi.interaction_id.as_deref(), Some(q.to_string().as_str()));
        assert_eq!(qi.actions, ["answer"]);

        let gi = got
            .iter()
            .find(|i| i.kind == AttentionKind::BlockedGoal)
            .unwrap();
        assert_eq!(gi.waiting_on, WaitingOn::user("bob"));
        assert_eq!(gi.goal_id.as_deref(), Some(blocked.to_string().as_str()));

        let dl = got
            .iter()
            .find(|i| i.kind == AttentionKind::DeadLetter)
            .unwrap();
        assert_eq!(
            dl.waiting_on,
            WaitingOn::any_maintainer(),
            "TA never invents a human role"
        );
        assert!(dl.actions.is_empty());
        // The JSON has the contract's field names.
        let v = serde_json::to_value(d).unwrap();
        for f in [
            "id",
            "kind",
            "project",
            "status",
            "item_digest",
            "waiting_on",
            "summary",
            "created_at",
            "updated_at",
            "candidate_id",
            "goal_id",
            "draft_id",
            "interaction_id",
            "provenance",
            "actions",
            "links",
        ] {
            assert!(v.get(f).is_some(), "missing {f}");
        }
        assert_eq!(v["waiting_on"]["type"], "any_maintainer");
        assert_eq!(v["kind"], "draft_review");
    }

    #[test]
    fn item_digest_is_sha256_of_canonical_json_of_the_five_fields() {
        let dir = tempfile::tempdir().unwrap();
        write_draft(
            dir.path(),
            Uuid::new_v4(),
            "Fix login",
            serde_json::json!({"status": "pending_review"}),
            Some(&provenance("alice")),
        );
        let it = &items(dir.path())[0];
        let as_json = serde_json::to_value(it).unwrap();
        let basis = serde_json::json!({
            "id": as_json["id"], "kind": as_json["kind"], "summary": as_json["summary"],
            "provenance": as_json["provenance"], "actions": as_json["actions"],
        });
        assert_eq!(
            it.item_digest,
            sha256_tagged(canonical_json(&basis).as_bytes())
        );
        // A different summary under the same id is a different digest.
        let d = item_digest(
            &it.id,
            it.kind,
            "something misleading",
            &it.provenance,
            &it.actions,
        );
        assert_ne!(d, it.item_digest);
        // Age and timestamps are not part of it.
        let mut later = it.clone();
        later.age_secs = 999;
        later.updated_at = Utc::now();
        assert_eq!(
            item_digest(
                &later.id,
                later.kind,
                &later.summary,
                &later.provenance,
                &later.actions
            ),
            it.item_digest
        );
    }

    #[test]
    fn ids_and_summaries_respect_the_contract_limits() {
        let dir = tempfile::tempdir().unwrap();
        write_dead_letter(dir.path(), &"m".repeat(300), "r");
        write_question(
            dir.path(),
            Uuid::new_v4(),
            None,
            &format!("line1\n\u{1b}[31mline2 {}", "x".repeat(900)),
        );
        let got = items(dir.path());
        assert_eq!(got.len(), 2);
        for it in &got {
            assert!(it.id.chars().count() <= MAX_ID_CHARS, "{}", it.id);
            assert!(it.summary.chars().count() <= MAX_SUMMARY_CHARS);
            assert!(!it.summary.contains('\n') && !it.summary.contains('\u{1b}'));
        }
        // A long id stays stable between reads.
        let again = items(dir.path());
        assert_eq!(
            got.iter().map(|i| &i.id).collect::<Vec<_>>(),
            again.iter().map(|i| &i.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn items_carry_no_secrets() {
        let dir = tempfile::tempdir().unwrap();
        write_dead_letter(dir.path(), "m-2", "implementer");
        let json = serde_json::to_string(&items(dir.path())).unwrap();
        assert!(
            !json.contains("LEAK-CHECK-SENTINEL"),
            "error tails must never reach the feed: {json}"
        );
        assert!(!json.contains("last_error_tail"));
    }

    #[test]
    fn a_goal_awaiting_the_listed_question_is_not_listed_twice() {
        let dir = tempfile::tempdir().unwrap();
        let q = Uuid::new_v4();
        write_question(dir.path(), q, None, "which db?");
        write_blocked_goal(dir.path(), q);
        let got = items(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].kind, AttentionKind::AgentQuestion);
    }

    #[test]
    fn filters_by_user_role_and_project() {
        let mk = |id: &str, project: &str, w: WaitingOn| {
            let mut it = build(NewItem {
                id: id.into(),
                kind: AttentionKind::DraftReview,
                project,
                waiting_on: w,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                summary: "s".into(),
                provenance: None,
                actions: &[],
                candidate_id: None,
                goal_id: None,
                draft_id: None,
                interaction_id: None,
                review_link: None,
            });
            it.age_secs = 0;
            it
        };
        let all = vec![
            mk("a", "p1", WaitingOn::user("alice")),
            mk("b", "p1", WaitingOn::user("bob")),
            mk("c", "p2", WaitingOn::role("reviewer")),
            mk("d", "p2", WaitingOn::any_maintainer()),
        ];
        let ids = |q: AttentionQuery| -> Vec<String> {
            filter(all.clone(), &q).into_iter().map(|i| i.id).collect()
        };
        assert_eq!(ids(AttentionQuery::default()), ["a", "b", "c", "d"]);
        assert_eq!(
            ids(AttentionQuery {
                user: Some("alice".into()),
                ..Default::default()
            }),
            ["a", "d"]
        );
        assert_eq!(
            ids(AttentionQuery {
                role: Some("reviewer".into()),
                ..Default::default()
            }),
            ["c"]
        );
        // A maintainer role also sees what waits on any maintainer.
        assert_eq!(
            ids(AttentionQuery {
                role: Some("admin".into()),
                ..Default::default()
            }),
            ["d"]
        );
        assert_eq!(
            ids(AttentionQuery {
                user: Some("bob".into()),
                role: Some("reviewer".into()),
                ..Default::default()
            }),
            ["b", "c", "d"]
        );
        assert_eq!(
            ids(AttentionQuery {
                project: Some("p1".into()),
                ..Default::default()
            }),
            ["a", "b"]
        );
        assert_eq!(
            ids(AttentionQuery {
                project: Some("p2".into()),
                user: Some("zed".into()),
                ..Default::default()
            }),
            ["d"]
        );
    }

    fn event_types(events_dir: &Path, ty: &str) -> Vec<serde_json::Value> {
        FsEventStore::new(events_dir)
            .query(&EventQueryFilter {
                event_types: vec![ty.to_string()],
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|e| serde_json::to_value(&e.payload).unwrap())
            .collect()
    }

    #[test]
    fn events_fire_for_each_kind_created_updated_and_resolved_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let events = root.join(".ta").join("events");
        let roots = roots_of(root);
        let tracker = AttentionTracker::new();

        // Baseline: pre-existing items do not replay as new.
        write_dead_letter(root, "old", "r");
        assert_eq!(
            tracker.tick(&items(root), &events, &roots),
            TickResult::default()
        );
        assert!(event_types(&events, "attention.created").is_empty());

        // One of each kind appears.
        let draft = Uuid::new_v4();
        let q = Uuid::new_v4();
        write_draft(
            root,
            draft,
            "Fix login",
            serde_json::json!({"status": "pending_review"}),
            None,
        );
        write_question(root, q, None, "which db?");
        let g = write_blocked_goal(root, Uuid::new_v4());
        write_dead_letter(root, "new", "implementer");
        let r = tracker.tick(&items(root), &events, &roots);
        let mut created = r.created.clone();
        created.sort();
        let mut want = vec![
            format!("att_draft_{draft}"),
            format!("att_question_{q}"),
            format!("att_goal_{g}"),
            "att_wake_new".to_string(),
        ];
        want.sort();
        assert_eq!(created, want);
        let created_events = event_types(&events, "attention.created");
        assert_eq!(created_events.len(), 4);
        let kinds: Vec<String> = created_events
            .iter()
            .map(|e| e["item"]["kind"].as_str().unwrap().to_string())
            .collect();
        for k in [
            "draft_review",
            "agent_question",
            "blocked_goal",
            "dead_letter",
        ] {
            assert!(
                kinds.contains(&k.to_string()),
                "missing created event for {k}"
            );
        }

        // A no-change tick emits nothing.
        assert_eq!(
            tracker.tick(&items(root), &events, &roots),
            TickResult::default()
        );

        // A change emits attention.updated.
        write_draft(
            root,
            draft,
            "Fix login properly",
            serde_json::json!({"status": "pending_review"}),
            None,
        );
        let r = tracker.tick(&items(root), &events, &roots);
        assert_eq!(r.updated, vec![format!("att_draft_{draft}")]);
        assert_eq!(event_types(&events, "attention.updated").len(), 1);

        // Resolution: draft approved, question answered or timed out (the pending
        // file is removed either way), goal moves on, dead letter acknowledged.
        write_draft(
            root,
            draft,
            "Fix login properly",
            serde_json::json!({"status": "approved", "approved_by": "alice", "approved_at": "2026-01-02T00:00:00Z"}),
            None,
        );
        std::fs::remove_file(
            root.join(".ta/interactions/pending")
                .join(format!("{q}.json")),
        )
        .unwrap();
        std::fs::remove_dir_all(root.join(".ta/goals")).unwrap();
        std::fs::write(
            root.join(".ta").join(DEAD_LETTER_ACK_FILE),
            "{\"msg_id\":\"new\"}\n",
        )
        .unwrap();
        let r = tracker.tick(&items(root), &events, &roots);
        assert_eq!(r.resolved.len(), 4, "one per kind: {:?}", r.resolved);
        let resolved = event_types(&events, "attention.resolved");
        assert_eq!(resolved.len(), 4);
        for r in &resolved {
            assert!(r["item_id"].as_str().is_some() && r["kind"].as_str().is_some());
        }
        // Each is recorded as resolved in the ledger, once.
        assert_eq!(ledger_read(root).len(), 4);

        // Exactly once: further ticks and re-reads never repeat it.
        assert_eq!(
            tracker.tick(&items(root), &events, &roots),
            TickResult::default()
        );
        assert_eq!(
            tracker.tick(&items(root), &events, &roots),
            TickResult::default()
        );
        assert_eq!(event_types(&events, "attention.resolved").len(), 4);
        assert_eq!(ledger_read(root).len(), 4);
    }

    #[test]
    fn a_question_that_times_out_resolves_like_an_answer() {
        // ta_ask_human removes its pending file on timeout (see
        // ta-mcp-gateway `ask_human_timeout_returns_message`); the feed treats
        // that as resolution.
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join(".ta").join("events");
        let roots = roots_of(dir.path());
        let tracker = AttentionTracker::new();
        tracker.tick(&items(dir.path()), &events, &roots);
        let q = Uuid::new_v4();
        write_question(dir.path(), q, None, "still there?");
        assert_eq!(
            tracker.tick(&items(dir.path()), &events, &roots).created,
            vec![format!("att_question_{q}")]
        );
        std::fs::remove_file(
            dir.path()
                .join(".ta/interactions/pending")
                .join(format!("{q}.json")),
        )
        .unwrap();
        assert_eq!(
            tracker.tick(&items(dir.path()), &events, &roots).resolved,
            vec![format!("att_question_{q}")]
        );
    }

    // ── HTTP ────────────────────────────────────────────────────────────────

    struct Rig {
        project: tempfile::TempDir,
        state_dir: tempfile::TempDir,
        state: Arc<AppState>,
        app: axum::Router,
        signer: TestSigner,
        install: String,
    }

    fn rig_with(config: DaemonConfig) -> Rig {
        let project = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let signer = TestSigner::new(5);
        signer.register(state_dir.path(), KID);
        let install = ta_credentials::human::install_id(state_dir.path()).unwrap();
        let mut state = AppState::new(project.path().to_path_buf(), config);
        state.human_gate = HumanGate::new(state_dir.path().to_path_buf());
        let state = Arc::new(state);
        Rig {
            app: build_api_router(state.clone()),
            project,
            state_dir,
            state,
            signer,
            install,
        }
    }

    fn rig() -> Rig {
        rig_with(DaemonConfig::default())
    }

    fn peer_req(
        method: &str,
        uri: &str,
        peer: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        req.extensions_mut()
            .insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
        req
    }

    async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
        send(app, peer_req("GET", uri, "127.0.0.1:50000", &[], None)).await
    }

    #[tokio::test]
    async fn feed_metadata_lists_role_metadata_for_every_key_including_non_default_order() {
        let r = rig();
        let second = TestSigner::new(9);
        ta_credentials::human::add_assertion_key_full(
            r.state_dir.path(),
            "org-2",
            &second.public_b64(),
            Some(&["lead".to_string()]),
            Some(&[
                "viewer".to_string(),
                "editor".to_string(),
                "lead".to_string(),
            ]),
            Some("editor"),
            None,
        )
        .unwrap();
        let (status, v) = get_json(&r.app, "/api/attention").await;
        assert_eq!(status, StatusCode::OK);
        let keys = v["keys"].as_array().unwrap();
        assert_eq!(keys.len(), 2);
        let org2 = keys.iter().find(|k| k["kid"] == "org-2").unwrap();
        assert_eq!(org2["role_order"], json!(["viewer", "editor", "lead"]));
        assert_eq!(org2["maintainer_roles"], json!(["lead"]));
        assert_eq!(org2["escalation_min_role"], "editor");
        assert_eq!(
            org2["fingerprint"],
            ta_credentials::human::public_key_fingerprint(&second.public_b64()).as_str()
        );
        // Exactly these fields: nothing that could carry key material.
        let mut names: Vec<&str> = org2
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "escalation_min_role",
                "fingerprint",
                "kid",
                "maintainer_roles",
                "role_order"
            ]
        );
        assert!(!v.to_string().contains(&second.public_b64()));
        assert!(!v.to_string().contains(&r.signer.public_b64()));
        // The default key is still listed with defaults.
        let org1 = keys.iter().find(|k| k["kid"] == KID).unwrap();
        assert_eq!(org1["role_order"], json!(["member", "admin", "owner"]));
    }

    impl Rig {
        fn root(&self) -> &Path {
            self.project.path()
        }

        async fn items(&self) -> Vec<AttentionItem> {
            let (_, v) = get_json(&self.app, "/api/attention").await;
            serde_json::from_value(v["items"].clone()).unwrap()
        }

        async fn item(&self, kind: AttentionKind) -> AttentionItem {
            self.items()
                .await
                .into_iter()
                .find(|i| i.kind == kind && i.status == "open")
                .expect("an open item of that kind")
        }

        fn claims(
            &self,
            item: &AttentionItem,
            action: &str,
            user: &str,
            role: &str,
            jti: &str,
        ) -> Value {
            claims_for(
                &self.install,
                action,
                &item.facts(),
                user,
                role,
                Utc::now().timestamp(),
                jti,
            )
        }

        fn sign(&self, claims: &Value) -> String {
            self.signer.sign(KID, claims)
        }

        async fn act_with(
            &self,
            id: &str,
            body: Value,
            peer: &str,
            headers: &[(&str, &str)],
        ) -> (StatusCode, Value) {
            send(
                &self.app,
                peer_req(
                    "POST",
                    &format!("/api/attention/{id}/act"),
                    peer,
                    headers,
                    Some(body),
                ),
            )
            .await
        }

        /// A relayed action: from the poller's machine, no credential.
        async fn relay(&self, id: &str, body: Value) -> (StatusCode, Value) {
            self.act_with(id, body, "127.0.0.1:50000", &[]).await
        }

        fn draft_status(&self, id: &str) -> String {
            let text = std::fs::read_to_string(
                self.root()
                    .join(".ta/pr_packages")
                    .join(format!("{id}.json")),
            )
            .unwrap();
            let v: Value = serde_json::from_str(&text).unwrap();
            v["status"]["status"]
                .as_str()
                .or_else(|| v["status"].as_str())
                .unwrap_or("")
                .to_string()
        }

        fn audit_lines(&self) -> Vec<Value> {
            std::fs::read_to_string(attention_dir(self.root()).join(AUDIT_FILE))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        }

        fn new_draft(&self, requester: &str) -> Uuid {
            let id = Uuid::new_v4();
            write_draft(
                self.root(),
                id,
                "Fix login",
                serde_json::json!({"status": "pending_review"}),
                Some(&provenance(requester)),
            );
            id
        }
    }

    #[tokio::test]
    async fn http_routes_list_filter_count_and_the_old_interaction_routes_still_work() {
        let r = rig();
        let root = r.root();
        write_draft(
            root,
            Uuid::new_v4(),
            "Fix login",
            serde_json::json!({"status": "pending_review"}),
            Some(&provenance("alice")),
        );
        write_question(root, Uuid::new_v4(), None, "which db?");
        write_dead_letter(root, "m-9", "implementer");

        let (st, v) = get_json(&r.app, "/api/attention").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["count"], 3);
        assert!(v["items"][0]["age_secs"].as_i64().unwrap() >= 0);
        assert!(v["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["id"].is_string()
                && i["kind"].is_string()
                && i["project"].is_string()
                && i["waiting_on"]["type"].is_string()
                && i["summary"].is_string()
                && i["item_digest"].is_string()
                && i["actions"].is_array()));
        // Metadata: install id, registered kids with fingerprints (never key
        // material), next_cursor.
        assert_eq!(v["install_id"], r.install.as_str());
        assert_eq!(v["keys"][0]["kid"], KID);
        assert!(v["keys"][0]["fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        assert!(!v["keys"][0].to_string().contains(&r.signer.public_b64()));
        // Role metadata (defaults for a key registered without options).
        assert_eq!(
            v["keys"][0]["role_order"],
            json!(["member", "admin", "owner"])
        );
        assert_eq!(v["keys"][0]["maintainer_roles"], json!(["admin", "owner"]));
        assert_eq!(v["keys"][0]["escalation_min_role"], "member");
        assert!(v["next_cursor"].is_string());

        // alice sees the any-maintainer items (all three here).
        let (_, v) = get_json(&r.app, "/api/attention?user=alice").await;
        assert_eq!(v["count"], 3);
        let (_, v) = get_json(&r.app, "/api/attention?role=owner").await;
        assert_eq!(v["count"], 3);
        let (_, v) = get_json(&r.app, "/api/attention?role=member").await;
        assert_eq!(v["count"], 0);
        let project = r.state.project_registry.list()[0].name.clone();
        let (_, v) = get_json(&r.app, &format!("/api/attention?project={project}")).await;
        assert_eq!(v["count"], 3);
        let (_, v) = get_json(&r.app, "/api/attention?project=nope").await;
        assert_eq!(v["count"], 0);

        let (st, v) = get_json(&r.app, "/api/attention/count").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["open"], 3);
        assert_eq!(v["by_kind"]["draft_review"], 1);
        assert_eq!(v["by_kind"]["agent_question"], 1);
        assert_eq!(v["by_kind"]["dead_letter"], 1);

        // Existing routes are untouched.
        let (st, _) = get_json(&r.app, "/api/interactions/pending").await;
        assert_eq!(st, StatusCode::OK);
    }

    #[tokio::test]
    async fn since_cursor_and_limit_page_through_the_feed() {
        let r = rig();
        for i in 0..5 {
            write_dead_letter(r.root(), &format!("m-{i}"), "r");
        }
        let (_, first) = get_json(&r.app, "/api/attention?limit=2").await;
        assert_eq!(first["count"], 2);
        assert_eq!(first["has_more"], true);
        let c1 = first["next_cursor"].as_str().unwrap().to_string();
        let (_, second) = get_json(&r.app, &format!("/api/attention?limit=2&since={c1}")).await;
        assert_eq!(second["count"], 2);
        let c2 = second["next_cursor"].as_str().unwrap().to_string();
        let (_, third) = get_json(&r.app, &format!("/api/attention?limit=2&since={c2}")).await;
        assert_eq!(third["count"], 1);
        assert_eq!(third["has_more"], false);
        let ids: Vec<String> = [&first, &second, &third]
            .iter()
            .flat_map(|p| p["items"].as_array().unwrap().iter())
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 5);
        let mut dedup = ids.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), 5, "no item repeated or skipped: {ids:?}");

        // Nothing new: the same cursor comes back and no items.
        let c3 = third["next_cursor"].as_str().unwrap().to_string();
        let (_, none) = get_json(&r.app, &format!("/api/attention?since={c3}")).await;
        assert_eq!(none["count"], 0);
        assert_eq!(none["next_cursor"], c3.as_str());

        // An RFC 3339 updated_at works as a cursor; garbage is an actionable 400.
        let (st, v) = get_json(&r.app, "/api/attention?since=2999-01-01T00:00:00Z").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["count"], 0);
        let (st, v) = get_json(&r.app, "/api/attention?since=garbage").await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("next_cursor"));
    }

    #[tokio::test]
    async fn in_process_registry_questions_appear_and_vanish_when_answered() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new(
            dir.path().to_path_buf(),
            DaemonConfig::default(),
        ));
        let id = Uuid::new_v4();
        let _rx = state
            .question_registry
            .register(crate::question_registry::PendingQuestion {
                interaction_id: id,
                goal_id: None,
                question: "in-process?".into(),
                context: None,
                response_hint: "freeform".into(),
                choices: vec![],
                turn: 1,
                created_at: Utc::now(),
                timeout_secs: Some(30),
            })
            .await;
        let got = collect_all(&state).await;
        assert!(got.iter().any(|i| i.id == format!("att_question_{id}")));
        state
            .question_registry
            .answer(
                id,
                crate::question_registry::HumanAnswer {
                    text: "yes".into(),
                    responder_id: "alice".into(),
                    answered_at: Utc::now(),
                },
            )
            .await
            .unwrap();
        assert!(collect_all(&state)
            .await
            .iter()
            .all(|i| i.id != format!("att_question_{id}")));
    }

    #[tokio::test]
    async fn attention_events_arrive_on_the_existing_events_stream() {
        use tokio_stream::StreamExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let state = Arc::new(AppState::new(root.to_path_buf(), DaemonConfig::default()));
        let roots = project_roots(&state);
        // Baseline, then a new draft appears.
        state
            .attention
            .tick(&collect_all(&state).await, &state.events_dir, &roots);
        write_draft(
            root,
            Uuid::new_v4(),
            "Fix login",
            serde_json::json!({"status": "pending_review"}),
            None,
        );
        let r = state
            .attention
            .tick(&collect_all(&state).await, &state.events_dir, &roots);
        assert_eq!(r.created.len(), 1);

        let app = build_api_router(state.clone());
        let req = peer_req(
            "GET",
            "/api/events?types=attention.created,attention.resolved",
            "127.0.0.1:50000",
            &[],
            None,
        );
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mut stream = resp.into_body().into_data_stream();
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("an event within 5s")
            .expect("stream open")
            .expect("chunk");
        let text = String::from_utf8_lossy(&chunk);
        assert!(text.contains("event: attention.created"), "{text}");
        assert!(text.contains("draft_review"), "{text}");
    }

    // ── POST /api/attention/{id}/act ───────────────────────────────────────

    #[tokio::test]
    async fn act_without_any_proof_is_refused_and_does_nothing() {
        let r = rig();
        let draft = r.new_draft("alice");
        let item = r.item(AttentionKind::DraftReview).await;
        let (st, v) = r.relay(&item.id, json!({"action": "approve"})).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert_eq!(v["reason"], "no_proof");
        assert!(v["hint"].as_str().unwrap().contains("ta login"));
        assert_eq!(r.draft_status(&draft.to_string()), "pending_review");
    }

    #[tokio::test]
    async fn act_with_the_local_credential_approves_and_is_idempotent() {
        let r = rig();
        let draft = r.new_draft("alice");
        let item = r.item(AttentionKind::DraftReview).await;
        ta_credentials::human::login(r.state_dir.path(), false).unwrap();
        let secret = ta_credentials::human::load_secret(r.state_dir.path(), false).unwrap();
        let cred = [(ta_credentials::human::CREDENTIAL_HEADER, secret.as_str())];

        let (st, v) = r
            .act_with(&item.id, json!({"action": "approve"}), "127.0.0.1:1", &cred)
            .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["acted"], true);
        assert_eq!(r.draft_status(&draft.to_string()), "approved");
        assert!(r.audit_lines().iter().any(|l| l["via"] == "credential"
            && l["action"] == "approve"
            && l["item_id"] == item.id.as_str()));

        // A repeat does not act again; it returns the original result.
        let (st, again) = r
            .act_with(&item.id, json!({"action": "deny"}), "127.0.0.1:1", &cred)
            .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(again["acted"], false);
        assert_eq!(again["status"], "already_resolved");
        assert_eq!(again["original_action"], "approve");
        assert_eq!(
            r.draft_status(&draft.to_string()),
            "approved",
            "first action wins"
        );

        // The feed now shows it resolved.
        let all = r.items().await;
        let resolved = all.iter().find(|i| i.id == item.id).unwrap();
        assert_eq!(resolved.status, "resolved");
    }

    // ── actor_context labeling on /act (CR-03 item 6) ──────────────────────

    const AGENT_HEADER: &str = "agent_delegated; reasons=CLAUDECODE%20is%20set";

    async fn act_labeled(action: &str, header: Option<&str>) -> (Rig, Uuid, Value) {
        let r = rig();
        let draft = r.new_draft("alice");
        let item = r.item(AttentionKind::DraftReview).await;
        ta_credentials::human::login(r.state_dir.path(), false).unwrap();
        let secret = ta_credentials::human::load_secret(r.state_dir.path(), false).unwrap();
        let mut headers = vec![(ta_credentials::human::CREDENTIAL_HEADER, secret.as_str())];
        if let Some(h) = header {
            headers.push((ta_credentials::human::ACTOR_CONTEXT_HEADER, h));
        }
        let body = if action == "deny" {
            json!({"action": action, "reason": "no"})
        } else {
            json!({"action": action})
        };
        let (st, v) = r.act_with(&item.id, body, "127.0.0.1:1", &headers).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        (r, draft, v)
    }

    #[tokio::test]
    async fn act_with_the_agent_header_is_labeled_everywhere() {
        let holder = ta_credentials::human::credential_holder();
        for action in ["approve", "deny"] {
            let (r, draft, _) = act_labeled(action, Some(AGENT_HEADER)).await;

            // Attention resolution record (ledger) and the human-action audit.
            let entries = ledger_read(r.root());
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].actor_context.as_deref(), Some("agent_delegated"));
            assert_eq!(entries[0].delegated_signals, vec!["CLAUDECODE is set"]);
            let rec = r.audit_lines().into_iter().next().expect("audit record");
            assert_eq!(rec["actor_context"], "agent_delegated");
            assert_eq!(rec["delegating_owner"], holder.as_str());
            assert_eq!(rec["user_id"], holder.as_str());
            assert_eq!(rec["delegated_signals"][0], "CLAUDECODE is set");

            // Draft record, approved_by / denied_by and the ta-audit log.
            let text =
                std::fs::read_to_string(r.root().join(format!(".ta/pr_packages/{draft}.json")))
                    .unwrap();
            let pkg: DraftPackage = serde_json::from_str(&text).unwrap();
            assert_eq!(pkg.action_contexts.len(), 1, "{action}");
            assert_eq!(pkg.action_contexts[0].actor_context, "agent_delegated");
            assert_eq!(pkg.action_contexts[0].delegating_owner, holder);
            match pkg.status {
                DraftStatus::Approved { approved_by, .. } => assert_eq!(approved_by, holder),
                DraftStatus::Denied { denied_by, .. } => assert_eq!(denied_by, holder),
                other => panic!("unexpected status {other:?}"),
            }
            let audit = std::fs::read_to_string(r.root().join(".ta/audit.jsonl")).unwrap();
            assert!(audit.contains("agent_delegated"), "{audit}");
            assert!(
                ta_credentials::human::last_agent_context_seen(r.state_dir.path()).is_some(),
                "sighting feeds /api/status"
            );
        }
    }

    #[tokio::test]
    async fn act_without_the_header_is_direct_and_malformed_header_is_ignored() {
        for (action, header) in [
            ("approve", None),
            ("deny", None),
            ("approve", Some("agent_delegated; reasons=%ZZ")),
            ("deny", Some("nonsense")),
        ] {
            let (r, draft, _) = act_labeled(action, header).await;
            let entries = ledger_read(r.root());
            assert_eq!(entries[0].actor_context.as_deref(), Some("direct"));
            assert!(entries[0].delegated_signals.is_empty());
            let rec = r.audit_lines().into_iter().next().unwrap();
            assert_eq!(rec["actor_context"], "direct");
            assert!(rec.get("delegating_owner").is_none());
            assert_eq!(rec["user_id"], "human-credential");
            let text =
                std::fs::read_to_string(r.root().join(format!(".ta/pr_packages/{draft}.json")))
                    .unwrap();
            let pkg: DraftPackage = serde_json::from_str(&text).unwrap();
            assert_eq!(pkg.action_contexts[0].actor_context, "direct");
            assert!(ta_credentials::human::last_agent_context_seen(r.state_dir.path()).is_none());
        }
    }

    #[tokio::test]
    async fn a_relay_only_token_cannot_approve_deny_or_answer() {
        let mut cfg = DaemonConfig::default();
        cfg.auth.require_token = true;
        let r = rig_with(cfg);
        let draft = r.new_draft("alice");
        let q = Uuid::new_v4();
        write_question(r.root(), q, None, "which db?");
        let relay = r
            .state
            .token_store
            .create(TokenScope::Relay, Some("poller".into()))
            .unwrap();
        let bearer = format!("Bearer {}", relay.token);
        let d = r.item(AttentionKind::DraftReview).await;
        let qi = r.item(AttentionKind::AgentQuestion).await;
        // From a remote address and from loopback: neither is a human.
        for peer in ["203.0.113.9:443", "127.0.0.1:50000"] {
            for (id, body) in [
                (&d.id, json!({"action": "approve"})),
                (&d.id, json!({"action": "deny"})),
                (&qi.id, json!({"action": "answer", "answer": "postgres"})),
            ] {
                let (st, v) = r
                    .act_with(id, body, peer, &[("authorization", &bearer)])
                    .await;
                assert_eq!(st, StatusCode::FORBIDDEN, "{peer}: {v}");
                assert!(
                    v["error"].as_str().unwrap().contains("relay")
                        || v["error"].as_str().unwrap().contains("proof of a human"),
                    "{v}"
                );
            }
        }
        assert_eq!(r.draft_status(&draft.to_string()), "pending_review");
        assert!(!r.root().join(".ta/interactions/answers").exists());
    }

    #[tokio::test]
    async fn a_relayed_assertion_approves_records_the_acceptance_and_audits_it() {
        let mut cfg = DaemonConfig::default();
        cfg.auth.require_token = true;
        let r = rig_with(cfg);
        let draft = r.new_draft("requester-1");
        let relay = r
            .state
            .token_store
            .create(TokenScope::Relay, Some("poller".into()))
            .unwrap();
        let bearer = format!("Bearer {}", relay.token);
        let item = r.item(AttentionKind::DraftReview).await;
        let mut c = r.claims(&item, "approve", "olivia", "owner", "jti-act-0001");
        c["escalated"] = true.into();
        c["a_claim_from_the_future"] = "ignored".into();
        let token = r.sign(&c);

        let (st, v) = r
            .act_with(
                &item.id,
                json!({"action": "approve", "assertion": token}),
                "203.0.113.9:443",
                &[("authorization", &bearer)],
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["acted"], true);
        assert_eq!(r.draft_status(&draft.to_string()), "approved");
        assert_eq!(v["accepted_assertion"]["jti"], "jti-act-0001");
        assert_eq!(v["accepted_assertion"]["escalated"], true);

        let rec = r
            .audit_lines()
            .into_iter()
            .find(|l| l["via"] == "assertion")
            .expect("audit record");
        assert_eq!(rec["user_id"], "olivia");
        assert_eq!(rec["escalated"], true);
        assert_eq!(rec["jti"], "jti-act-0001");
        assert!(rec.get("self_approved").is_none());

        // The resolved item in the feed carries accepted_assertions.
        let (_, feed) = send(
            &r.app,
            peer_req(
                "GET",
                "/api/attention",
                "203.0.113.9:443",
                &[("authorization", &bearer)],
                None,
            ),
        )
        .await;
        let resolved = feed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == item.id.as_str())
            .unwrap();
        assert_eq!(resolved["status"], "resolved");
        let acc = &resolved["accepted_assertions"][0];
        assert_eq!(acc["jti"], "jti-act-0001");
        assert_eq!(acc["user_id"], "olivia");
        assert_eq!(acc["action"], "approve");
        assert_eq!(acc["escalated"], true);
        assert_eq!(acc["self_approved"], false);
        // And it is no longer counted as open.
        let (_, cnt) = get_json(&r.app, "/api/attention/count").await;
        assert_eq!(cnt["open"], 0);
    }

    #[tokio::test]
    async fn a_repeated_delivery_returns_the_original_result_without_acting_twice() {
        let r = rig();
        let draft = r.new_draft("requester-1");
        let item = r.item(AttentionKind::DraftReview).await;
        let c = r.claims(&item, "approve", "olivia", "admin", "jti-act-0002");
        let token = r.sign(&c);
        let body = json!({"action": "approve", "assertion": token});
        let (st, first) = r.relay(&item.id, body.clone()).await;
        assert_eq!(st, StatusCode::OK, "{first}");
        assert_eq!(first["acted"], true);
        let before =
            std::fs::read_to_string(r.root().join(format!(".ta/pr_packages/{draft}.json")))
                .unwrap();

        // The same assertion again (the poller retried): original result.
        let (st, again) = r.relay(&item.id, body).await;
        assert_eq!(st, StatusCode::OK, "{again}");
        assert_eq!(again["acted"], false);
        assert_eq!(again["replayed"], true);
        assert_eq!(again["original_result"], first["result"]);
        // Nothing was written a second time.
        let after = std::fs::read_to_string(r.root().join(format!(".ta/pr_packages/{draft}.json")))
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(
            r.audit_lines()
                .iter()
                .filter(|l| l["via"] == "assertion")
                .count(),
            1
        );
        // The resolution ledger holds exactly one acceptance.
        let entries = ledger_read(r.root());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].item.accepted_assertions.len(), 1);

        // A different, valid assertion afterwards: first valid action wins.
        let c = r.claims(&item, "deny", "pat", "admin", "jti-act-0003");
        let (st, late) = r
            .relay(&item.id, json!({"action": "deny", "assertion": r.sign(&c)}))
            .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(late["acted"], false);
        assert_eq!(late["replayed"], false);
        assert_eq!(late["original_action"], "approve");
        assert_eq!(r.draft_status(&draft.to_string()), "approved");

        // But an unsigned or forged follow-up learns nothing.
        let forged = TestSigner::new(66).sign(KID, &c);
        let (st, v) = r
            .relay(&item.id, json!({"action": "deny", "assertion": forged}))
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    }

    #[tokio::test]
    async fn replaying_an_accepted_assertion_on_a_new_item_is_refused() {
        let r = rig();
        r.new_draft("requester-1");
        let item = r.item(AttentionKind::DraftReview).await;
        let c = r.claims(&item, "approve", "olivia", "admin", "jti-act-0004");
        let token = r.sign(&c);
        let (st, _) = r
            .relay(&item.id, json!({"action": "approve", "assertion": token}))
            .await;
        assert_eq!(st, StatusCode::OK);
        // A second draft, same jti and a claims set re-pointed at it: the
        // signature no longer matches, and the original cannot be reused for it.
        let d2 = r.new_draft("requester-1");
        let second = r
            .items()
            .await
            .into_iter()
            .find(|i| i.draft_id.as_deref() == Some(d2.to_string().as_str()) && i.status == "open")
            .unwrap();
        let (st, v) = r
            .relay(&second.id, json!({"action": "approve", "assertion": token}))
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(v["reason"], "claim_mismatch");
        assert_eq!(r.draft_status(&d2.to_string()), "pending_review");
    }

    #[tokio::test]
    async fn an_item_that_changed_since_it_was_shown_is_refused() {
        let r = rig();
        let draft = r.new_draft("requester-1");
        let item = r.item(AttentionKind::DraftReview).await;
        let token = r.sign(&r.claims(&item, "approve", "olivia", "admin", "jti-act-0005"));
        // The draft changes after the person looked at it.
        write_draft(
            r.root(),
            draft,
            "Delete the production database",
            json!({"status": "pending_review"}),
            Some(&provenance("requester-1")),
        );
        let (st, v) = r
            .relay(&item.id, json!({"action": "approve", "assertion": token}))
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(v["reason"], "item_changed");
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("item changed, refresh and review again"));
        assert_eq!(r.draft_status(&draft.to_string()), "pending_review");
    }

    #[tokio::test]
    async fn an_answer_cannot_be_swapped_by_the_relay() {
        let r = rig();
        let q = Uuid::new_v4();
        write_question(r.root(), q, None, "which db?");
        let item = r.item(AttentionKind::AgentQuestion).await;
        let mut c = r.claims(&item, "answer", "olivia", "admin", "jti-act-0006");
        c["answer_sha256"] = sha256_tagged(b"postgres").into();
        let token = r.sign(&c);
        let answer_file = r.root().join(format!(".ta/interactions/answers/{q}.json"));

        let (st, v) = r
            .relay(
                &item.id,
                json!({"action": "answer", "answer": "drop all tables", "assertion": token}),
            )
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(v["reason"], "answer_changed");
        assert!(!answer_file.exists(), "nothing was delivered");

        // The answer the person signed goes through.
        let (st, v) = r
            .relay(
                &item.id,
                json!({"action": "answer", "answer": "postgres", "assertion": token}),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let delivered: Value =
            serde_json::from_str(&std::fs::read_to_string(&answer_file).unwrap()).unwrap();
        assert_eq!(delivered["text"], "postgres");
        assert_eq!(delivered["responder_id"], "olivia");
    }

    #[tokio::test]
    async fn self_approval_needs_the_claim_and_is_audited_when_used() {
        let r = rig();
        let draft = r.new_draft("sam");
        let item = r.item(AttentionKind::DraftReview).await;
        // Sam requested it and signs the approval: refused by default.
        let c = r.claims(&item, "approve", "sam", "owner", "jti-act-0007");
        let (st, v) = r
            .relay(
                &item.id,
                json!({"action": "approve", "assertion": r.sign(&c)}),
            )
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(v["reason"], "self_approval");
        assert_eq!(r.draft_status(&draft.to_string()), "pending_review");

        // With self_approval_permitted (a sole approver) it goes through and is audited.
        let mut c = r.claims(&item, "approve", "sam", "owner", "jti-act-0008");
        c["self_approval_permitted"] = true.into();
        let (st, v) = r
            .relay(
                &item.id,
                json!({"action": "approve", "assertion": r.sign(&c)}),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["accepted_assertion"]["self_approved"], true);
        let rec = r
            .audit_lines()
            .into_iter()
            .find(|l| l["via"] == "assertion")
            .unwrap();
        assert_eq!(rec["self_approved"], true);
        assert_eq!(rec["user_id"], "sam");
        assert_eq!(rec["item_id"], item.id.as_str());
        assert!(rec["at"].is_string());
    }

    #[tokio::test]
    async fn the_role_floor_is_enforced_at_the_endpoint() {
        let r = rig();
        let draft = r.new_draft("requester-1");
        let item = r.item(AttentionKind::DraftReview).await;
        // A member can never approve an any_maintainer item, even if signed.
        let c = r.claims(&item, "approve", "mia", "member", "jti-act-0009");
        let (st, v) = r
            .relay(
                &item.id,
                json!({"action": "approve", "assertion": r.sign(&c)}),
            )
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(v["reason"], "not_permitted");
        assert_eq!(r.draft_status(&draft.to_string()), "pending_review");
        // An admin can.
        let c = r.claims(&item, "approve", "adam", "admin", "jti-act-0010");
        let (st, _) = r
            .relay(
                &item.id,
                json!({"action": "approve", "assertion": r.sign(&c)}),
            )
            .await;
        assert_eq!(st, StatusCode::OK);
    }

    #[tokio::test]
    async fn deny_records_the_reason_and_the_named_human() {
        let r = rig();
        let draft = r.new_draft("requester-1");
        let item = r.item(AttentionKind::DraftReview).await;
        let c = r.claims(&item, "deny", "adam", "admin", "jti-act-0011");
        let (st, v) = r
            .relay(
                &item.id,
                json!({"action": "deny", "reason": "not what I asked", "assertion": r.sign(&c)}),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let text = std::fs::read_to_string(r.root().join(format!(".ta/pr_packages/{draft}.json")))
            .unwrap();
        assert!(
            text.contains("not what I asked") && text.contains("adam"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn act_validates_the_request_with_actionable_errors() {
        let r = rig();
        r.new_draft("requester-1");
        write_dead_letter(r.root(), "m-x", "r");
        let item = r.item(AttentionKind::DraftReview).await;
        let dead = r.item(AttentionKind::DeadLetter).await;
        ta_credentials::human::login(r.state_dir.path(), false).unwrap();
        let secret = ta_credentials::human::load_secret(r.state_dir.path(), false).unwrap();
        let cred = [(ta_credentials::human::CREDENTIAL_HEADER, secret.as_str())];

        let (st, v) = r
            .act_with(
                "att_draft_nope",
                json!({"action": "approve"}),
                "127.0.0.1:1",
                &cred,
            )
            .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert!(v["hint"].as_str().unwrap().contains("GET /api/attention"));

        let (st, _) = r
            .act_with(&item.id, json!({"action": "explode"}), "127.0.0.1:1", &cred)
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, v) = r
            .act_with(
                &item.id,
                json!({"action": "answer", "answer": "x"}),
                "127.0.0.1:1",
                &cred,
            )
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("draft_review"));
        let (st, _) = r
            .act_with(&dead.id, json!({"action": "approve"}), "127.0.0.1:1", &cred)
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, v) = r
            .act_with(
                &item.id,
                json!({"action": "approve", "extra": 1}),
                "127.0.0.1:1",
                &cred,
            )
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["hint"]
            .as_str()
            .unwrap()
            .contains("Human-asserted actions"));
    }

    #[tokio::test]
    async fn an_assertion_for_another_install_or_a_forged_one_is_refused() {
        let r = rig();
        let draft = r.new_draft("requester-1");
        let item = r.item(AttentionKind::DraftReview).await;
        let mut c = r.claims(&item, "approve", "adam", "admin", "jti-act-0012");
        c["aud"] = "ta_install_somebody_else".into();
        let (st, v) = r
            .relay(
                &item.id,
                json!({"action": "approve", "assertion": r.sign(&c)}),
            )
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert_eq!(v["reason"], "wrong_audience");
        let c = r.claims(&item, "approve", "adam", "admin", "jti-act-0013");
        let forged = TestSigner::new(77).sign(KID, &c);
        let (st, v) = r
            .relay(&item.id, json!({"action": "approve", "assertion": forged}))
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert_eq!(v["reason"], "assertion_rejected");
        assert_eq!(r.draft_status(&draft.to_string()), "pending_review");
    }
}
