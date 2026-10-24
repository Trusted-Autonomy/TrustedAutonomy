//! Sanitising and fenced display of untrusted request text.
//!
//! A request body is written by a goal agent and may carry prompt-injection
//! text, terminal escape sequences or bidirectional overrides. Nothing in this
//! crate ever shows that text unfenced: [`sanitize`] strips what can attack a
//! terminal, and [`render_request`] quotes every line through
//! [`ta_goal::provenance::fence_untrusted`] (random boundary token, `| `
//! prefix on every line) under a label that says the content is data.

use ta_goal::provenance::fence_untrusted;

use crate::model::{ListReport, RequestBody, RequestView};

/// Strip everything that is not safe to print: ANSI/OSC escape sequences, C0
/// and C1 control characters (keeping `\n` and `\t`), zero-width characters,
/// bidirectional overrides/isolates and line or paragraph separators.
pub fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => skip_escape(&mut chars),
            '\u{9b}' => skip_csi(&mut chars),
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}' => {}
            c => out.push(c),
        }
    }
    out
}

fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    // Parameter and intermediate bytes, then one final byte in 0x40..=0x7e.
    for c in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&c) {
            break;
        }
    }
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            skip_csi(chars);
        }
        // OSC / DCS / PM / APC: runs to BEL or ST (ESC \).
        Some(']') | Some('P') | Some('^') | Some('_') => {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '\u{7}' {
                    break;
                }
                if c == '\u{1b}' {
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                    }
                    break;
                }
            }
        }
        // Two-byte escape.
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_osc_and_controls_but_keeps_newline_and_tab() {
        let s = "a\u{1b}[31mred\u{1b}[0m\u{1b}]0;title\u{7}b\u{7}\rc\nd\te\u{0}\u{9b}31mf";
        assert_eq!(sanitize(s), "aredbc\nd\tef");
    }

    #[test]
    fn strips_bidi_and_zero_width() {
        let s = "safe\u{202e}evil\u{2066}x\u{2069}\u{200b}\u{feff}\u{2028}end";
        assert_eq!(sanitize(s), "safeevilxend");
    }

    #[test]
    fn unterminated_escape_at_end_does_not_panic() {
        assert_eq!(sanitize("x\u{1b}"), "x");
        assert_eq!(sanitize("x\u{1b}[12"), "x");
        assert_eq!(sanitize("x\u{1b}]no terminator"), "x");
    }
}
