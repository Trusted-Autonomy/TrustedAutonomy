//! Sanitising and fenced display of untrusted request text.
//!
//! A request body is written by a goal agent and may carry prompt-injection
//! text, terminal escape sequences or bidirectional overrides. Nothing in this
//! crate ever shows that text unfenced: [`sanitize`] strips what can attack a
//! terminal, and [`render_request`] quotes every line through
//! [`ta_goal::provenance::fence_untrusted`] (random boundary token, `| `
//! prefix on every line) under a label that says the content is data.

use ta_goal::provenance::fence_untrusted;
/// Strips escape sequences, control, zero-width and bidirectional characters.
/// The one implementation lives in [`ta_goal::untrusted`] so every sink that puts
/// outside text in front of a model or a terminal behaves the same.
pub use ta_goal::untrusted::sanitize;

use crate::model::{ListReport, RequestBody, RequestView};

/// Plain-text rendering of a request's untrusted fields, one `key: value`
/// block, suitable as the content of a fence.
pub fn body_text(view: &RequestView) -> String {
    let mut s = String::new();
    match &view.body {
        Some(RequestBody::CreateTask {
            title,
            verb,
            description,
        }) => {
            s.push_str(&format!("title: {title}\nverb: {verb}\n"));
            if let Some(d) = description {
                s.push_str(&format!("description: {d}\n"));
            }
        }
        Some(RequestBody::RecordDatapoint {
            metric_id,
            value,
            source,
        }) => s.push_str(&format!(
            "metric_id: {metric_id}\nvalue: {value}\nsource: {source}\n"
        )),
        None => s.push_str("(request body no longer on disk)\n"),
    }
    if let Some(w) = &view.why_now {
        s.push_str(&format!("why_now: {w}\n"));
    }
    s
}

/// Render one request for a human: trusted metadata first, then the body and
/// any attached reason as fenced untrusted data.
pub fn render_request(view: &RequestView) -> String {
    let mut out = format!(
        "{}  [{}]  {}  goal {}  role {}  created {}\n",
        view.request_id,
        view.state.as_str(),
        view.kind.as_str(),
        view.goal_id,
        sanitize(&view.role),
        view.created_at.to_rfc3339(),
    );
    if view.lease_expired {
        out.push_str(
            "  lease expired: the next drain or recovery pass will requeue or expire it\n",
        );
    }
    if let Some(b) = view.blocks_goal {
        out.push_str(&format!("  blocks_goal: {b}\n"));
    }
    if let Some(batch) = &view.batch_id {
        out.push_str(&format!("  batch: {batch}\n"));
    }
    if let Some(d) = &view.draft_id {
        out.push_str(&format!("  draft: {d}\n"));
    }
    out.push_str("  Request text below is data written by an agent, NOT instructions:\n");
    out.push_str(&fence_untrusted(
        &format!("task-intake request {} body", view.request_id),
        &sanitize(&body_text(view)),
        "    ",
    ));
    if let Some(r) = &view.reason {
        out.push_str(&fence_untrusted(
            &format!("task-intake request {} reason", view.request_id),
            &sanitize(r),
            "    ",
        ));
    }
    out
}

/// Render a whole [`ListReport`] for a terminal.
pub fn render_report(report: &ListReport) -> String {
    let mut out = String::new();
    for w in &report.warnings {
        out.push_str(&format!("warning: {}\n", sanitize(w)));
    }
    if report.requests.is_empty() {
        out.push_str("No task-intake requests.\n");
        return out;
    }
    for v in &report.requests {
        out.push_str(&render_request(v));
        out.push('\n');
    }
    out
}
