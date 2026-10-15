//! Old finished records go into a compressed bundle with an index. Never deleted silently.

use std::io::{BufRead, BufReader, Write};

use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::model::Paths;
use crate::state::append_jsonl;

/// Write `goals` and `drafts` into `.ta/archive/records-<ts>.jsonl.gz`, read the
/// bundle back to verify it, append an index line, and only then remove the
/// originals. Returns the number of records archived.
pub fn archive_records(p: &Paths, goals: &[Uuid], drafts: &[Uuid]) -> anyhow::Result<usize> {
    let mut records: Vec<(String, Uuid, std::path::PathBuf, Value)> = Vec::new();
    for id in goals {
        let path = p.goals_dir.join(format!("{id}.json"));
        if let Ok(s) = std::fs::read_to_string(&path) {
            records.push(("goal".into(), *id, path, serde_json::from_str(&s)?));
        }
    }
    for id in drafts {
        let path = p.pr_packages_dir.join(format!("{id}.json"));
        if let Ok(s) = std::fs::read_to_string(&path) {
            records.push(("draft".into(), *id, path, serde_json::from_str(&s)?));
        }
    }
    if records.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(p.archive_dir())?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let bundle = p.archive_dir().join(format!("records-{stamp}.jsonl.gz"));
    {
        let mut enc = GzEncoder::new(std::fs::File::create(&bundle)?, Compression::default());
        for (kind, id, _, rec) in &records {
            writeln!(
                enc,
                "{}",
                serde_json::to_string(&json!({"type": kind, "id": id, "record": rec}))?
            )?;
        }
        enc.finish()?;
    }
    let readback = BufReader::new(GzDecoder::new(std::fs::File::open(&bundle)?))
        .lines()
        .filter(|l| l.as_ref().is_ok_and(|s| !s.is_empty()))
        .count();
    if readback != records.len() {
        let _ = std::fs::remove_file(&bundle);
        anyhow::bail!(
            "archive bundle {} verified {readback} of {} records; originals kept, nothing removed. Re-run `ta cleanup --only archive`.",
            bundle.display(),
            records.len()
        );
    }
    for (kind, id, _, _) in &records {
        append_jsonl(
            &p.archive_dir().join("index.jsonl"),
            &json!({
                "type": kind,
                "id": id,
                "bundle": bundle.file_name().map(|n| n.to_string_lossy().to_string()),
                "archived_at": chrono::Utc::now().to_rfc3339(),
            }),
        )?;
    }
    for (_, _, path, _) in &records {
        std::fs::remove_file(path)?;
    }
    Ok(records.len())
}
