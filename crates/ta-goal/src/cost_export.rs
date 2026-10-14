//! Export a cost report to Meridian's generic JSONL format (`ta cost report --meridian`).
//!
//! One record per TA goal or per interactive session, in the shape Meridian's
//! generic loader reads: `id`, `title`, `timestamp`, `tokens_input`,
//! `tokens_output`, `source`, `phase`. Every other value is a string, because the
//! loader keeps only string-valued extras as metadata.
//!
//! `tokens_input` is uncached input only. Cache reads are 64% of interactive
//! cost, so they are carried as separate metadata (`cache_read_tokens`,
//! `cache_write_tokens`) instead of being folded into the effort score.
//!
//! Rows are the already-deduplicated [`CostRow`]s, so a Claude session whose
//! transcript repeats a `message.id` exports once. Writing is atomic and
//! idempotent by record `id`: re-running replaces a record in place, never
//! appends a second copy, and records already in the file that this run did not
//! produce are kept.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde_json::{json, Map, Value};

use crate::cost_report::{CostKind, CostRow, CostSourceKind};
use crate::GoalRun;

/// Result of writing the file, for the confirmation line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    /// Records produced by this run.
    pub exported: usize,
    /// Of those, how many replaced a record already in the file.
    pub replaced: usize,
    /// Records already in the file that this run did not touch.
    pub kept: usize,
}

/// The record id: stable across runs so a re-export replaces rather than duplicates.
pub fn record_id(source: CostSourceKind, session: &str) -> String {
    format!("{}:{}", source.as_str(), session)
}

fn short(s: &str) -> &str {
    let end = s.char_indices().nth(8).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
}

/// Build one Meridian record per goal or session in `rows`, ordered by id.
///
/// `goals` supplies each goal's title and plan phase; a transcript session is
/// titled from its project and source since the transcript has no title.
pub fn meridian_records(rows: &[&CostRow], goals: &[(String, GoalRun)]) -> Vec<Value> {
    let goal_by_id: HashMap<String, &GoalRun> = goals
        .iter()
        .map(|(_, g)| (g.goal_run_id.to_string(), g))
        .collect();

    let mut by_session: BTreeMap<(CostSourceKind, String), Vec<&CostRow>> = BTreeMap::new();
    for r in rows {
        by_session
            .entry((r.source, r.session.clone()))
            .or_default()
            .push(r);
    }

    by_session
        .into_iter()
        .map(|((source, session), rows)| {
            let (mut input, mut output, mut cache_read, mut cache_write) = (0u64, 0u64, 0u64, 0u64);
            let mut usd = 0.0;
            let mut all_exact = true;
            let mut peak: Option<u64> = None;
            let mut by_model: BTreeMap<&str, u64> = BTreeMap::new();
            for r in &rows {
                input = input.saturating_add(r.usage.input);
                output = output.saturating_add(r.usage.output);
                cache_read = cache_read.saturating_add(r.usage.cache_read);
                cache_write = cache_write.saturating_add(r.usage.cache_creation);
                usd += r.usd;
                all_exact &= r.kind == CostKind::Exact;
                peak = peak.max(r.context_tokens);
                *by_model.entry(r.model.as_str()).or_default() += r.usage.total();
            }
            // The model that carried the most tokens; ties go to the name that sorts first.
            let model = by_model
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                .map(|(m, _)| if m.is_empty() { "unknown" } else { m })
                .unwrap_or("unknown");
            let ts = rows.iter().filter_map(|r| r.ts).min();
            let goal = goal_by_id
                .get(&session)
                .filter(|_| source == CostSourceKind::Goals);
            let project = &rows[0].project;
            let title = match goal {
                Some(g) if !g.title.trim().is_empty() => g.title.clone(),
                _ => format!("{project}: {} session {}", source.as_str(), short(&session)),
            };

            let mut rec = Map::new();
            rec.insert("id".into(), json!(record_id(source, &session)));
            rec.insert("title".into(), json!(title));
            if let Some(t) = ts {
                rec.insert("timestamp".into(), json!(t.to_rfc3339()));
            }
            rec.insert("tokens_input".into(), json!(input));
            rec.insert("tokens_output".into(), json!(output));
            rec.insert("source".into(), json!(source.as_str()));
            if let Some(phase) = goal.and_then(|g| g.plan_phase.as_ref()) {
                rec.insert("phase".into(), json!(phase));
            }
            rec.insert("cost_usd".into(), json!(format!("{usd:.4}")));
            rec.insert("cost_exact".into(), json!(all_exact.to_string()));
            rec.insert("model".into(), json!(model));
            rec.insert("cache_read_tokens".into(), json!(cache_read.to_string()));
            rec.insert("cache_write_tokens".into(), json!(cache_write.to_string()));
            // A goal is many turns with no per-turn prompt size recorded.
            rec.insert(
                "context_peak".into(),
                json!(peak.map_or_else(|| "n/a".to_string(), |p| p.to_string())),
            );
            rec.insert(
                "workload".into(),
                json!(if source == CostSourceKind::Goals {
                    "goal"
                } else {
                    "interactive"
                }),
            );
            Value::Object(rec)
        })
        .collect()
}

/// Write `records` to `path`, merging by `id` with whatever is already there.
///
/// Existing records with an id this run produced are replaced in place, other
/// existing lines are kept as they are, and new records are appended. The file
/// is written to a temporary file in the same directory and renamed over
/// `path`, so a crash never leaves a half-written file. A line in an existing
/// file that is not valid JSON is an error (the file is left untouched) rather
/// than silently dropped.
pub fn write_jsonl(path: &Path, records: &[Value]) -> anyhow::Result<ExportOutcome> {
    let mut lines: Vec<String> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    match std::fs::read_to_string(path) {
        Ok(text) => {
            for (n, line) in text.lines().enumerate() {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                // Meridian's loader skips `//` comment lines; keep them.
                if !t.starts_with("//") {
                    let v: Value = serde_json::from_str(t).with_context(|| {
                        format!(
                            "{} line {} is not valid JSON, so the file was left untouched. Fix \
                             or remove that line, or export to a different --meridian path",
                            path.display(),
                            n + 1
                        )
                    })?;
                    if let Some(id) = v.get("id").and_then(Value::as_str) {
                        index.insert(id.to_string(), lines.len());
                    }
                }
                lines.push(t.to_string());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "could not read the existing {}. Check it is a readable file, or export to a \
                     different --meridian path",
                    path.display()
                )
            })
        }
    }

    let mut replaced = 0;
    for rec in records {
        let line = serde_json::to_string(rec)?;
        match rec.get("id").and_then(Value::as_str) {
            Some(id) => match index.get(id) {
                Some(&i) => {
                    lines[i] = line;
                    replaced += 1;
                }
                None => {
                    index.insert(id.to_string(), lines.len());
                    lines.push(line);
                }
            },
            None => lines.push(line),
        }
    }

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| {
        format!(
            "could not create {}. Check the --meridian path is writable",
            dir.display()
        )
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).with_context(|| {
        format!(
            "could not create a temporary file in {}. Check the directory is writable",
            dir.display()
        )
    })?;
    for l in &lines {
        writeln!(tmp, "{l}")?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| {
        anyhow::anyhow!(
            "could not move the finished export into place at {}: {}. The records are in {}; \
             copy it over by hand or export to a different --meridian path",
            path.display(),
            e.error,
            e.file.path().display()
        )
    })?;

    Ok(ExportOutcome {
        exported: records.len(),
        replaced,
        kept: lines.len() - records.len(),
    })
}

/// The command that reads the exported file.
pub fn meridian_command(path: &Path) -> String {
    let p = path.display().to_string();
    if p.contains(' ') {
        format!("meridian analyze --source jsonl --path \"{p}\"")
    } else {
        format!("meridian analyze --source jsonl --path {p}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_cost::TokenUsage;
    use chrono::{TimeZone, Utc};

    fn row(session: &str, model: &str, out: u64, ctx: u64) -> CostRow {
        CostRow {
            source: CostSourceKind::Claude,
            session: session.into(),
            ts: Some(Utc.with_ymd_and_hms(2026, 10, 10, 1, 0, 0).unwrap()),
            project: "proj".into(),
            model: model.into(),
            usage: TokenUsage {
                input: 1,
                output: out,
                cache_read: ctx,
                cache_creation: 10,
            },
            usd: 1.25,
            kind: CostKind::Estimated,
            context_tokens: Some(ctx + 11),
            origin_class: None,
        }
    }

    /// The shape of Meridian's `GenericRecord` loader: `title` required, the
    /// rest optional, string-valued extras kept as metadata.
    #[derive(serde::Deserialize)]
    struct MeridianRecord {
        id: Option<String>,
        #[allow(dead_code)]
        title: String,
        timestamp: Option<String>,
        tokens_input: Option<u64>,
        tokens_output: Option<u64>,
        source: Option<String>,
        #[serde(flatten)]
        extra: HashMap<String, Value>,
    }

    fn meta(r: &MeridianRecord) -> HashMap<String, String> {
        r.extra
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect()
    }

    #[test]
    fn one_record_per_session_with_string_metadata() {
        let rows = [
            row("s1", "claude-sonnet-5-5", 5, 1000),
            row("s1", "claude-sonnet-5-5", 7, 5000),
            row("s2", "claude-opus-5-5", 3, 10),
        ];
        let refs: Vec<&CostRow> = rows.iter().collect();
        let recs = meridian_records(&refs, &[]);
        assert_eq!(recs.len(), 2);

        let r: MeridianRecord = serde_json::from_value(recs[0].clone()).unwrap();
        assert_eq!(r.id.as_deref(), Some("claude_code:s1"));
        assert_eq!(r.source.as_deref(), Some("claude_code"));
        assert_eq!(r.tokens_input, Some(2));
        assert_eq!(r.tokens_output, Some(12));
        assert!(r
            .timestamp
            .as_ref()
            .unwrap()
            .starts_with("2026-10-10T01:00:00"));
        let m = meta(&r);
        assert_eq!(m["cost_usd"], "2.5000");
        assert_eq!(m["cost_exact"], "false");
        assert_eq!(m["model"], "claude-sonnet-5-5");
        assert_eq!(m["cache_read_tokens"], "6000");
        assert_eq!(m["cache_write_tokens"], "20");
        assert_eq!(m["context_peak"], "5011");
        assert_eq!(m["workload"], "interactive");
    }

    #[test]
    fn exact_only_when_every_row_is_exact() {
        let mut a = row("s", "m", 1, 1);
        let mut b = row("s", "m", 1, 1);
        a.kind = CostKind::Exact;
        b.kind = CostKind::Exact;
        let recs = meridian_records(&[&a, &b], &[]);
        assert_eq!(recs[0]["cost_exact"], "true");
        b.kind = CostKind::Unpriced;
        let recs = meridian_records(&[&a, &b], &[]);
        assert_eq!(recs[0]["cost_exact"], "false");
    }

    #[test]
    fn rerun_replaces_by_id_and_keeps_other_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out").join("cost.jsonl");
        let rows = [row("s1", "m", 5, 100), row("s2", "m", 5, 100)];
        let refs: Vec<&CostRow> = rows.iter().collect();
        let recs = meridian_records(&refs, &[]);

        let first = write_jsonl(&path, &recs).unwrap();
        assert_eq!((first.exported, first.replaced, first.kept), (2, 0, 0));
        let second = write_jsonl(&path, &recs).unwrap();
        assert_eq!((second.exported, second.replaced, second.kept), (2, 2, 0));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);

        // A foreign record and a comment survive; a changed session updates in place.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("// note\n{\"id\":\"other\",\"title\":\"mine\"}\n");
        std::fs::write(&path, text).unwrap();
        let changed = [row("s1", "m", 99, 100)];
        let refs: Vec<&CostRow> = changed.iter().collect();
        let out = write_jsonl(&path, &meridian_records(&refs, &[])).unwrap();
        assert_eq!((out.exported, out.replaced, out.kept), (1, 1, 3));
        let lines: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("claude_code:s1") && lines[0].contains("\"tokens_output\":99"));
        assert!(lines[1].contains("claude_code:s2"));
        assert_eq!(lines[2], "// note");
        assert!(lines[3].contains("\"other\""));
    }

    #[test]
    fn corrupt_existing_file_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cost.jsonl");
        std::fs::write(&path, "not json\n").unwrap();
        let rows = [row("s1", "m", 5, 100)];
        let refs: Vec<&CostRow> = rows.iter().collect();
        let e = write_jsonl(&path, &meridian_records(&refs, &[])).unwrap_err();
        assert!(e.to_string().contains("line 1"), "{e}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json\n");
    }

    #[test]
    fn command_quotes_paths_with_spaces() {
        assert_eq!(
            meridian_command(Path::new("a/b.jsonl")),
            "meridian analyze --source jsonl --path a/b.jsonl"
        );
        assert!(meridian_command(Path::new("a b/c.jsonl")).ends_with("\"a b/c.jsonl\""));
    }
}
