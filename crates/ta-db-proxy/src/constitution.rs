//! Wires a staged DB draft's captured mutations into `ta-actions`'
//! `db_query` constitution rules (v0.17.1 item 3): a warn rule for
//! large mutation counts, a block rule for schema-altering statements.
//!
//! `ta-actions::PolicyConstitution::check_db_mutation` is engine-agnostic —
//! it only knows about a row count and a boolean. This module supplies
//! those two DB-shaped facts by reading `ta_db_overlay::OverlayEntry`, which
//! every db proxy plugin (built-in or third-party) produces in the same
//! shape regardless of engine.

use ta_actions::{ConstitutionViolation, PolicyConstitution};
use ta_db_overlay::{OverlayEntry, OverlayEntryKind};

/// Count of row-level mutations (insert/update/delete/blob) in a staged
/// draft. DDL entries are intentionally excluded — they're governed by the
/// schema-altering-statement rule instead, not the row-count rule.
pub fn rows_modified(entries: &[OverlayEntry]) -> u64 {
    entries
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                OverlayEntryKind::Insert
                    | OverlayEntryKind::Update
                    | OverlayEntryKind::Delete
                    | OverlayEntryKind::Blob
            )
        })
        .count() as u64
}

/// Whether any DDL entry in the draft contains a schema-altering statement:
/// `DROP TABLE`, `TRUNCATE`, or `ALTER TABLE ... DROP COLUMN`. Scans both
/// `before` and `after` since a dropped table's statement is carried in
/// `after` (the SQLite plugin's convention) while some future plugin might
/// carry it in `before` instead — this check doesn't care which.
pub fn has_schema_altering_statement(entries: &[OverlayEntry]) -> bool {
    entries
        .iter()
        .filter(|e| e.kind == OverlayEntryKind::Ddl)
        .any(|e| {
            let before_sql = e.before.as_ref().and_then(|v| v.as_str());
            let after_sql = e.after.as_str();
            before_sql.is_some_and(is_schema_altering_statement)
                || after_sql.is_some_and(is_schema_altering_statement)
        })
}

fn is_schema_altering_statement(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    upper.contains("DROP TABLE")
        || upper.contains("TRUNCATE")
        || (upper.contains("ALTER TABLE") && upper.contains("DROP COLUMN"))
}

/// Check a staged DB draft's captured mutations against the constitution's
/// `db_query` rules. `allow_schema_drops` is
/// `[actions.db_query].allow_schema_drops` from `workflow.toml`.
pub fn check_draft(
    entries: &[OverlayEntry],
    constitution: &PolicyConstitution,
    allow_schema_drops: bool,
) -> Result<(), ConstitutionViolation> {
    constitution.check_db_mutation(
        rows_modified(entries),
        has_schema_altering_statement(entries),
        allow_schema_drops,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn entry(kind: OverlayEntryKind, after: serde_json::Value) -> OverlayEntry {
        OverlayEntry {
            uri: "sqlite://db/t/1".to_string(),
            before: None,
            after,
            ts: Utc::now(),
            kind,
        }
    }

    #[test]
    fn rows_modified_counts_row_kinds_not_ddl() {
        let entries = vec![
            entry(OverlayEntryKind::Insert, json!({})),
            entry(OverlayEntryKind::Update, json!({})),
            entry(OverlayEntryKind::Delete, json!(null)),
            entry(OverlayEntryKind::Blob, json!({})),
            entry(OverlayEntryKind::Ddl, json!("CREATE TABLE t (id INTEGER)")),
        ];
        assert_eq!(rows_modified(&entries), 4);
    }

    #[test]
    fn detects_drop_table() {
        let entries = vec![entry(OverlayEntryKind::Ddl, json!("DROP TABLE users"))];
        assert!(has_schema_altering_statement(&entries));
    }

    #[test]
    fn detects_truncate() {
        let entries = vec![entry(OverlayEntryKind::Ddl, json!("TRUNCATE users"))];
        assert!(has_schema_altering_statement(&entries));
    }

    #[test]
    fn detects_alter_table_drop_column() {
        let entries = vec![entry(
            OverlayEntryKind::Ddl,
            json!("ALTER TABLE users DROP COLUMN email"),
        )];
        assert!(has_schema_altering_statement(&entries));
    }

    #[test]
    fn create_table_is_not_schema_altering() {
        let entries = vec![entry(
            OverlayEntryKind::Ddl,
            json!("CREATE TABLE users (id INTEGER)"),
        )];
        assert!(!has_schema_altering_statement(&entries));
    }

    #[test]
    fn alter_table_add_column_is_not_schema_altering() {
        let entries = vec![entry(
            OverlayEntryKind::Ddl,
            json!("ALTER TABLE users ADD COLUMN nickname TEXT"),
        )];
        assert!(!has_schema_altering_statement(&entries));
    }

    #[test]
    fn check_draft_blocks_drop_table() {
        let entries = vec![entry(OverlayEntryKind::Ddl, json!("DROP TABLE users"))];
        let constitution = PolicyConstitution::load(std::path::Path::new("/nonexistent"));
        let result = check_draft(&entries, &constitution, false);
        assert!(result.is_err());
        assert!(!result.unwrap_err().is_warn);
    }

    #[test]
    fn check_draft_allows_drop_table_when_opted_in() {
        let entries = vec![entry(OverlayEntryKind::Ddl, json!("DROP TABLE users"))];
        let constitution = PolicyConstitution::load(std::path::Path::new("/nonexistent"));
        assert!(check_draft(&entries, &constitution, true).is_ok());
    }

    #[test]
    fn check_draft_warns_at_configured_threshold() {
        let entries: Vec<OverlayEntry> = (0..101)
            .map(|i| {
                let mut e = entry(OverlayEntryKind::Insert, json!({"n": i}));
                e.uri = format!("sqlite://db/t/{i}");
                e
            })
            .collect();
        let constitution = PolicyConstitution::load(std::path::Path::new("/nonexistent"));
        let result = check_draft(&entries, &constitution, false);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().is_warn,
            "over-threshold row count is a warn, not a block"
        );
    }

    #[test]
    fn check_draft_passes_small_clean_draft() {
        let entries = vec![entry(OverlayEntryKind::Insert, json!({"n": 1}))];
        let constitution = PolicyConstitution::load(std::path::Path::new("/nonexistent"));
        assert!(check_draft(&entries, &constitution, false).is_ok());
    }
}
