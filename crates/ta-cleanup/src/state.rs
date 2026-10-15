//! Files cleanup writes: questions, remembered decisions, pending interactions, audit.

use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};
use ta_audit::{AuditAction, AuditEvent, AuditLog};

use crate::model::*;

pub fn append_jsonl(path: &Path, value: &Value) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{}", serde_json::to_string(value)?)?;
    Ok(())
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

pub fn decisions_path(p: &Paths) -> std::path::PathBuf {
    p.cleanup_dir().join("decisions.jsonl")
}

pub fn questions_path(p: &Paths) -> std::path::PathBuf {
    p.cleanup_dir().join("questions.jsonl")
}

/// Record a human answer so the same situation can pre-select it next time.
pub fn record_decision(p: &Paths, f: &Finding, answer: &str) -> anyhow::Result<()> {
    append_jsonl(
        &decisions_path(p),
        &json!({
            "kind": f.kind.as_str(),
            "signature": f.signature,
            "finding_id": f.id,
            "answer": answer,
            "at": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

/// The most recent answer for the same (kind, evidence signature).
pub fn lookup_decision(p: &Paths, kind: Kind, signature: &str) -> Option<Remembered> {
    read_jsonl(&decisions_path(p))
        .into_iter()
        .rev()
        .find(|v| v["kind"] == kind.as_str() && v["signature"] == signature)
        .map(|v| Remembered {
            answer: v["answer"].as_str().unwrap_or_default().to_string(),
            at: v["at"].as_str().unwrap_or_default().to_string(),
        })
}

pub fn record_suggestion(p: &Paths, f: &Finding, s: &Suggestion) -> anyhow::Result<()> {
    append_jsonl(
        &p.cleanup_dir().join("suggestions.jsonl"),
        &json!({
            "finding_id": f.id,
            "signature": f.signature,
            "option": s.option,
            "confidence": s.confidence,
            "note": s.note,
            "at": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

fn interaction_id(f: &Finding) -> String {
    let clean: String =
        f.id.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
    format!("cleanup-{clean}")
}

/// Queue a question for a human who is not at the terminal. Idempotent per
/// finding id. Returns true when something new was written.
pub fn queue_question(p: &Paths, f: &Finding) -> anyhow::Result<bool> {
    let qpath = questions_path(p);
    let already = read_jsonl(&qpath)
        .iter()
        .any(|v| v["finding_id"] == f.id.as_str());
    let iid = interaction_id(f);
    let pending = p
        .ta_dir()
        .join("interactions/pending")
        .join(format!("{iid}.json"));
    let answered = p
        .ta_dir()
        .join("interactions/answers")
        .join(format!("{iid}.json"));
    if already && pending.exists() || answered.exists() {
        return Ok(false);
    }
    let evidence: Vec<String> = f
        .evidence
        .iter()
        .map(|e| format!("{}: {}", e.source, e.fact))
        .collect();
    let choices: Vec<String> = f
        .options
        .iter()
        .map(|o| format!("{}: {}", o.key, o.label))
        .collect();
    if !already {
        append_jsonl(
            &qpath,
            &json!({
                "finding_id": f.id,
                "kind": f.kind.as_str(),
                "title": f.title,
                "evidence": evidence,
                "reason": f.reason,
                "options": f.options.iter().map(|o| json!({"key": o.key, "label": o.label})).collect::<Vec<_>>(),
                "recommended": f.recommended,
                "remembered": f.remembered,
                "suggestion": f.suggestion,
                "signature": f.signature,
                "asked_at": chrono::Utc::now().to_rfc3339(),
            }),
        )?;
    }
    if let Some(dir) = pending.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = json!({
        "interaction_id": iid,
        "goal_id": null,
        "question": format!("Cleanup: {}. {}", f.title, f.reason),
        "context": evidence.join("\n"),
        "choices": choices,
        "response_hint": format!(
            "Answer with an option key. Recommended: {}. Or run `ta cleanup` at a terminal.",
            f.recommended.as_deref().unwrap_or("none")
        ),
        "turn": 1,
    });
    std::fs::write(&pending, serde_json::to_string_pretty(&body)?)?;
    Ok(true)
}

/// One audit line per executed action.
pub fn audit(p: &Paths, f: &Finding, decider: Decider, summary: &str) {
    let human = !matches!(decider, Decider::Rule);
    let evidence: Vec<String> = f
        .evidence
        .iter()
        .map(|e| format!("{}: {}", e.source, e.fact))
        .collect();
    let mut ev = AuditEvent::new(
        "ta-cleanup",
        if human {
            AuditAction::Approval
        } else {
            AuditAction::Apply
        },
    )
    .with_target(format!("cleanup://{}", f.id))
    .with_metadata(json!({
        "action": "cleanup",
        "finding_id": f.id,
        "kind": f.kind.as_str(),
        "evidence": evidence.join("; "),
        "verdict": f.verdict.to_string(),
        "decided_by": decider.as_str(),
        "result": summary,
    }));
    match AuditLog::open(&p.audit_log) {
        Ok(mut log) => {
            if let Err(e) = log.append(&mut ev) {
                tracing::warn!(finding = %f.id, error = %e, "cleanup: could not write audit event");
            }
        }
        Err(e) => {
            tracing::warn!(path = %p.audit_log.display(), error = %e, "cleanup: could not open audit log");
        }
    }
}
