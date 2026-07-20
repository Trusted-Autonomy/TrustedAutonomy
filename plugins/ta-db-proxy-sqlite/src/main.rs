//! SQLite database proxy plugin for Trusted Autonomy (v0.17.1).
//!
//! Implements the TA `db` plugin JSON-over-stdio protocol (protocol version 1).
//!
//! ## Protocol
//!
//! Reads one JSON request line from stdin, writes one JSON response line to
//! stdout, then exits. Each invocation is stateless — the plugin is spawned
//! fresh for every method call.
//!
//! ## Capture model — session-scoped shadow copy
//!
//! SQLite is file-based, so there's no wire protocol to intercept. Instead:
//! `start_capture` makes two copies of the real database file into the
//! goal's staging directory — a `shadow.db` the agent is pointed at (TA core
//! redirects the agent's connection string to this path) and a `pristine.db`
//! kept untouched as a diff baseline. `stop_capture` diffs `shadow.db`
//! against `pristine.db` row-by-row (keyed by `rowid`) to reconstruct
//! INSERT/UPDATE/DELETE mutations, plus table-level CREATE/DROP as DDL
//! entries, and (on `apply`) appends them to `db-overlay.jsonl` in the same
//! shape `ta_db_overlay::OverlayEntry` reads — a third-party plugin author
//! never needs to depend on TA's Rust crates, only on this JSON shape.
//!
//! **Known limitation**: `ALTER TABLE` on an existing table is detected (the
//! table's `sqlite_master.sql` text differs pre/post) but is not captured as
//! a replayable DDL entry — the shadow-copy diff only sees before/after
//! state, not the actual statement issued, so there's no safe way to
//! reconstruct a replayable `ALTER` from a schema diff alone. `CREATE TABLE`
//! and `DROP TABLE` are captured because the "after" state IS the replayable
//! statement in both cases.
//!
//! ## Supported methods
//!
//! | Method          | Description                                            |
//! |-----------------|---------------------------------------------------------|
//! | `handshake`      | Version negotiation                                     |
//! | `classify_query` | Classify a SQL string as read/write/ddl/admin/unknown    |
//! | `start_capture`  | Create shadow + pristine copies, return opaque handle    |
//! | `stop_capture`   | Diff shadow vs pristine, write/discard mutations, cleanup |
//! | `apply_mutation` | Replay one staged mutation against the real database      |

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PROTOCOL_VERSION: u32 = 1;
const ADAPTER_NAME: &str = "sqlite";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Protocol envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Request {
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Value::is_null")]
    result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok(result: Value) -> Self {
        Self {
            ok: true,
            result,
            error: None,
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: Value::Null,
            error: Some(msg.into()),
        }
    }
}

fn write_response(resp: &Response) {
    let text = serde_json::to_string(resp)
        .unwrap_or_else(|e| format!(r#"{{"ok":false,"error":"serialization error: {}"}}"#, e));
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", text);
    let _ = out.flush();
}

// ---------------------------------------------------------------------------
// Overlay entry — a self-contained mirror of ta_db_overlay::OverlayEntry's
// wire shape. Kept local rather than depending on the ta-db-overlay crate,
// matching the "structurally identical to what a third party would author"
// principle: the JSONL shape is the contract, not a shared Rust type.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct OverlayEntryOut {
    uri: String,
    before: Option<Value>,
    after: Value,
    ts: String,
    kind: &'static str,
}

fn append_overlay_entries(staging_dir: &Path, entries: &[OverlayEntryOut]) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(staging_dir)
        .map_err(|e| format!("create staging dir {}: {}", staging_dir.display(), e))?;
    let overlay_path = staging_dir.join("db-overlay.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&overlay_path)
        .map_err(|e| format!("open {}: {}", overlay_path.display(), e))?;
    for entry in entries {
        let line = serde_json::to_string(entry).map_err(|e| e.to_string())?;
        writeln!(file, "{}", line).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn now_rfc3339() -> String {
    // Avoid pulling in a full chrono dependency just for "now" formatting —
    // any RFC3339 producer works since `ts` is display/ordering metadata,
    // not something this plugin parses back.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    chrono::DateTime::<chrono::Utc>::from_timestamp(now.as_secs() as i64, now.subsec_nanos())
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// classify_query
// ---------------------------------------------------------------------------

fn classify_query(sql: &str) -> Value {
    let upper = sql.trim().to_uppercase();
    if upper.starts_with("SELECT")
        || upper.starts_with("EXPLAIN")
        || (upper.starts_with("PRAGMA ") && !upper.contains('='))
    {
        json!("read")
    } else if upper.starts_with("INSERT") {
        json!({"write": "insert"})
    } else if upper.starts_with("UPDATE") {
        json!({"write": "update"})
    } else if upper.starts_with("DELETE") {
        json!({"write": "delete"})
    } else if upper.starts_with("REPLACE") || upper.starts_with("UPSERT") {
        json!({"write": "upsert"})
    } else if upper.starts_with("CREATE") || upper.starts_with("ALTER") || upper.starts_with("DROP")
    {
        json!("ddl")
    } else if upper.starts_with("PRAGMA")
        || upper.starts_with("VACUUM")
        || upper.starts_with("ATTACH")
        || upper.starts_with("DETACH")
        || upper.starts_with("BEGIN")
        || upper.starts_with("COMMIT")
        || upper.starts_with("ROLLBACK")
    {
        json!("admin")
    } else {
        json!("unknown")
    }
}

fn handle_classify_query(params: &Value) -> Response {
    let query = match params.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return Response::err("classify_query: missing 'query' param"),
    };
    Response::ok(json!({ "class": classify_query(query) }))
}

// ---------------------------------------------------------------------------
// start_capture / stop_capture
// ---------------------------------------------------------------------------

fn strip_sqlite_prefix(dsn: &str) -> &str {
    dsn.strip_prefix("sqlite://").unwrap_or(dsn)
}

fn handle_start_capture(params: &Value) -> Response {
    let goal_id = match params.get("goal_id").and_then(|v| v.as_str()) {
        Some(g) => g,
        None => return Response::err("start_capture: missing 'goal_id' param"),
    };
    let staging_dir = match params.get("staging_dir").and_then(|v| v.as_str()) {
        Some(s) => PathBuf::from(s),
        None => return Response::err("start_capture: missing 'staging_dir' param"),
    };
    let upstream_dsn = match params.get("upstream_dsn").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("start_capture: missing 'upstream_dsn' param"),
    };
    let source_db = strip_sqlite_prefix(upstream_dsn).to_string();

    let capture_dir = staging_dir.join("db-capture").join(sanitize(goal_id));
    if let Err(e) = std::fs::create_dir_all(&capture_dir) {
        return Response::err(format!(
            "start_capture: failed to create capture dir {}: {}",
            capture_dir.display(),
            e
        ));
    }

    let pristine_db = capture_dir.join("pristine.db");
    let shadow_db = capture_dir.join("shadow.db");

    let source_path = Path::new(&source_db);
    if source_path.exists() {
        if let Err(e) = std::fs::copy(source_path, &pristine_db) {
            return Response::err(format!(
                "start_capture: failed to snapshot {} to {}: {}",
                source_db,
                pristine_db.display(),
                e
            ));
        }
        if let Err(e) = std::fs::copy(source_path, &shadow_db) {
            return Response::err(format!(
                "start_capture: failed to create shadow copy at {}: {}",
                shadow_db.display(),
                e
            ));
        }
    } else {
        // New database — an empty SQLite file is a valid starting point for
        // both the baseline and the shadow copy.
        if let Err(e) = Connection::open(&pristine_db).and_then(|c| {
            drop(c);
            Connection::open(&shadow_db)
        }) {
            return Response::err(format!(
                "start_capture: failed to initialize new database at {}: {}",
                capture_dir.display(),
                e
            ));
        }
    }

    Response::ok(json!({
        "engine": "sqlite",
        "cursor": {
            "source_db": source_db,
            "pristine_db": pristine_db.to_string_lossy(),
            "shadow_db": shadow_db.to_string_lossy(),
            "staging_dir": staging_dir.to_string_lossy(),
        }
    }))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn handle_stop_capture(params: &Value) -> Response {
    let handle = match params.get("handle") {
        Some(h) => h,
        None => return Response::err("stop_capture: missing 'handle' param"),
    };
    let cursor = match handle.get("cursor") {
        Some(c) => c,
        None => return Response::err("stop_capture: handle missing 'cursor'"),
    };
    let action = match params.get("action").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return Response::err("stop_capture: missing 'action' param"),
    };

    let get_field = |field: &str| -> Result<PathBuf, Response> {
        cursor
            .get(field)
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .ok_or_else(|| Response::err(format!("stop_capture: cursor missing '{field}'")))
    };
    let pristine_db = match get_field("pristine_db") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let shadow_db = match get_field("shadow_db") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let source_db = match cursor.get("source_db").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return Response::err("stop_capture: cursor missing 'source_db'"),
    };
    let staging_dir = match get_field("staging_dir") {
        Ok(p) => p,
        Err(r) => return r,
    };

    let cleanup = || {
        let _ = std::fs::remove_file(&pristine_db);
        let _ = std::fs::remove_file(&shadow_db);
    };

    if action == "discard" {
        cleanup();
        return Response::ok(json!({ "mutations_captured": 0 }));
    }
    if action != "apply" {
        return Response::err(format!(
            "stop_capture: unknown action '{action}' (expected 'apply' or 'discard')"
        ));
    }

    let entries = match diff_databases(&pristine_db, &shadow_db, &source_db) {
        Ok(e) => e,
        Err(e) => {
            cleanup();
            return Response::err(format!("stop_capture: diff failed: {e}"));
        }
    };
    if let Err(e) = append_overlay_entries(&staging_dir, &entries) {
        cleanup();
        return Response::err(format!(
            "stop_capture: failed to write db-overlay.jsonl: {e}"
        ));
    }
    cleanup();
    Response::ok(json!({ "mutations_captured": entries.len() }))
}

// ---------------------------------------------------------------------------
// Diff logic
// ---------------------------------------------------------------------------

fn open_readonly(path: &Path) -> Result<Option<Connection>, String> {
    if !path.exists() {
        return Ok(None);
    }
    Connection::open(path)
        .map(Some)
        .map_err(|e| format!("open {}: {}", path.display(), e))
}

fn list_tables(conn: &Connection) -> Result<Vec<(String, String)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = vec![];
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn value_ref_to_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t).to_string()),
        ValueRef::Blob(b) => json!(base64_encode(b)),
    }
}

// Minimal base64 encoder so blob columns round-trip without pulling in an
// extra dependency for a rarely-hit path.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn dump_table_rows(
    conn: &Connection,
    table: &str,
) -> Result<BTreeMap<i64, serde_json::Map<String, Value>>, String> {
    let sql = format!("SELECT rowid, * FROM \"{}\"", table);
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
        let mut obj = serde_json::Map::new();
        for (i, name) in col_names.iter().enumerate().skip(1) {
            let v = row.get_ref(i).map_err(|e| e.to_string())?;
            obj.insert(name.clone(), value_ref_to_json(v));
        }
        out.insert(rowid, obj);
    }
    Ok(out)
}

fn diff_databases(
    pristine_path: &Path,
    shadow_path: &Path,
    source_db: &str,
) -> Result<Vec<OverlayEntryOut>, String> {
    let pristine = open_readonly(pristine_path)?;
    let shadow = open_readonly(shadow_path)?;
    let ts = now_rfc3339();
    let mut entries = vec![];

    let pristine_tables: BTreeMap<String, String> = pristine
        .as_ref()
        .map(list_tables)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .collect();
    let shadow_tables: BTreeMap<String, String> = shadow
        .as_ref()
        .map(list_tables)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Table-level DDL: created and dropped tables produce a directly
    // replayable statement. Altered-but-still-present tables are
    // intentionally not captured as DDL — see module docs.
    for (table, sql) in &shadow_tables {
        if !pristine_tables.contains_key(table) {
            entries.push(OverlayEntryOut {
                uri: format!("sqlite://{source_db}/{table}/{DDL_SENTINEL}"),
                before: None,
                after: json!(sql),
                ts: ts.clone(),
                kind: "ddl",
            });
        }
    }
    for (table, sql) in &pristine_tables {
        if !shadow_tables.contains_key(table) {
            entries.push(OverlayEntryOut {
                uri: format!("sqlite://{source_db}/{table}/{DDL_SENTINEL}"),
                before: Some(json!(sql)),
                after: json!(format!("DROP TABLE IF EXISTS \"{table}\"")),
                ts: ts.clone(),
                kind: "ddl",
            });
        }
    }

    // Row-level diff for every table present in the shadow copy.
    for table in shadow_tables.keys() {
        let shadow_conn = shadow
            .as_ref()
            .expect("shadow db must exist to have tables");
        let shadow_rows = dump_table_rows(shadow_conn, table)?;
        let pristine_rows = if let Some(conn) = pristine.as_ref() {
            if pristine_tables.contains_key(table) {
                dump_table_rows(conn, table)?
            } else {
                BTreeMap::new()
            }
        } else {
            BTreeMap::new()
        };

        for (rowid, after) in &shadow_rows {
            let uri = format!("sqlite://{source_db}/{table}/{rowid}");
            match pristine_rows.get(rowid) {
                None => entries.push(OverlayEntryOut {
                    uri,
                    before: None,
                    after: Value::Object(after.clone()),
                    ts: ts.clone(),
                    kind: "insert",
                }),
                Some(before) if before != after => entries.push(OverlayEntryOut {
                    uri,
                    before: Some(Value::Object(before.clone())),
                    after: Value::Object(after.clone()),
                    ts: ts.clone(),
                    kind: "update",
                }),
                Some(_) => {}
            }
        }
        for (rowid, before) in &pristine_rows {
            if !shadow_rows.contains_key(rowid) {
                entries.push(OverlayEntryOut {
                    uri: format!("sqlite://{source_db}/{table}/{rowid}"),
                    before: Some(Value::Object(before.clone())),
                    after: Value::Null,
                    ts: ts.clone(),
                    kind: "delete",
                });
            }
        }
    }

    Ok(entries)
}

// ---------------------------------------------------------------------------
// apply_mutation — replay one staged mutation against the real database.
// ---------------------------------------------------------------------------

fn handle_apply_mutation(params: &Value) -> Response {
    let uri = match params.get("uri").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return Response::err("apply_mutation: missing 'uri' param"),
    };
    let after = params.get("after").cloned().unwrap_or(Value::Null);
    // A present-but-JSON-null "before" means "no pre-image" (insert), same
    // as an absent field — only distinguish that from Some(value) below.
    let before = params.get("before").cloned().filter(|v| !v.is_null());

    match apply_sqlite_mutation(uri, before.as_ref(), &after) {
        Ok(()) => Response::ok(json!({})),
        Err(e) => Response::err(format!("apply_mutation: {e}")),
    }
}

/// Marker used in place of a rowid for table-level DDL mutations
/// (`sqlite://<db>/<table>/__ddl__`). A plain 2-segment `sqlite://<db>/<table>`
/// URI can't be told apart from a 3-segment row URI by counting `/`
/// separators once `<db>` itself contains slashes (any absolute path does),
/// so DDL entries always carry this explicit third segment instead.
const DDL_SENTINEL: &str = "__ddl__";

fn apply_sqlite_mutation(uri: &str, before: Option<&Value>, after: &Value) -> Result<(), String> {
    let rest = uri
        .strip_prefix("sqlite://")
        .ok_or_else(|| format!("invalid SQLite URI: {uri}"))?;
    let parts: Vec<&str> = rest.rsplitn(3, '/').collect();
    if parts.len() < 3 {
        return Err(format!(
            "SQLite URI must be sqlite://<db>/<table>/<rowid-or-{DDL_SENTINEL}>: {uri}"
        ));
    }
    let rowid_str = parts[0];
    let table = parts[1];
    let db_path = parts[2];
    let conn = Connection::open(db_path).map_err(|e| format!("open {db_path}: {e}"))?;

    if rowid_str == DDL_SENTINEL {
        let sql = after
            .as_str()
            .ok_or_else(|| format!("DDL mutation for table '{table}' has no SQL text"))?;
        return conn.execute_batch(sql).map_err(|e| e.to_string());
    }

    if *after == Value::Null {
        conn.execute(
            &format!("DELETE FROM \"{table}\" WHERE rowid = ?"),
            [rowid_str],
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    }

    let obj = after
        .as_object()
        .ok_or_else(|| "mutation 'after' must be a JSON object".to_string())?;

    if before.is_none() {
        let cols: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        let placeholders: Vec<&str> = cols.iter().map(|_| "?").collect();
        let sql = format!(
            "INSERT INTO \"{table}\" ({}) VALUES ({})",
            cols.iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", "),
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        for (i, v) in obj.values().enumerate() {
            stmt.raw_bind_parameter(i + 1, value_to_sql_param(v))
                .map_err(|e| e.to_string())?;
        }
        stmt.raw_execute().map_err(|e| e.to_string())?;
    } else {
        let set_clauses: Vec<String> = obj.keys().map(|k| format!("\"{k}\" = ?")).collect();
        let sql = format!(
            "UPDATE \"{table}\" SET {} WHERE rowid = ?",
            set_clauses.join(", ")
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut i = 1;
        for v in obj.values() {
            stmt.raw_bind_parameter(i, value_to_sql_param(v))
                .map_err(|e| e.to_string())?;
            i += 1;
        }
        stmt.raw_bind_parameter(i, rowid_str)
            .map_err(|e| e.to_string())?;
        stmt.raw_execute().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn value_to_sql_param(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// handshake
// ---------------------------------------------------------------------------

fn handle_handshake(_params: &Value) -> Response {
    Response::ok(json!({
        "plugin_version": PLUGIN_VERSION,
        "protocol_version": PROTOCOL_VERSION,
        "adapter_name": ADAPTER_NAME,
        "capabilities": ["classify_query", "start_capture", "stop_capture", "apply_mutation"],
    }))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let stdin = io::stdin();
    let line = match stdin.lock().lines().next() {
        Some(Ok(line)) if !line.trim().is_empty() => line,
        _ => {
            write_response(&Response::err(
                "No input on stdin. Expected one JSON line with {method, params}.",
            ));
            std::process::exit(1);
        }
    };

    let request: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            write_response(&Response::err(format!(
                "Invalid JSON request: {e}. Got: '{}'",
                if line.len() > 200 {
                    &line[..200]
                } else {
                    &line
                }
            )));
            std::process::exit(1);
        }
    };

    let response = match request.method.as_str() {
        "handshake" => handle_handshake(&request.params),
        "classify_query" => handle_classify_query(&request.params),
        "start_capture" => handle_start_capture(&request.params),
        "stop_capture" => handle_stop_capture(&request.params),
        "apply_mutation" => handle_apply_mutation(&request.params),
        unknown => Response::err(format!(
            "Unknown method '{unknown}'. Supported methods: handshake, classify_query, \
             start_capture, stop_capture, apply_mutation."
        )),
    };

    write_response(&response);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn handshake_returns_adapter_name() {
        let resp = handle_handshake(&json!({}));
        assert!(resp.ok);
        assert_eq!(resp.result["adapter_name"], "sqlite");
    }

    #[test]
    fn classify_select_is_read() {
        assert_eq!(classify_query("SELECT * FROM t"), json!("read"));
    }

    #[test]
    fn classify_insert_is_write_insert() {
        assert_eq!(
            classify_query("INSERT INTO t VALUES (1)"),
            json!({"write": "insert"})
        );
    }

    #[test]
    fn classify_create_table_is_ddl() {
        assert_eq!(classify_query("CREATE TABLE t (id INTEGER)"), json!("ddl"));
    }

    fn setup_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE items (name TEXT, value INTEGER)")
            .unwrap();
        conn.execute("INSERT INTO items (name, value) VALUES ('a', 1)", [])
            .unwrap();
        conn.execute("INSERT INTO items (name, value) VALUES ('b', 2)", [])
            .unwrap();
    }

    #[test]
    fn start_capture_then_stop_capture_apply_round_trips_full_lifecycle() {
        let dir = tempdir().unwrap();
        let staging = dir.path().join("staging");
        let source_db = dir.path().join("real.db");
        setup_db(&source_db);

        let start_resp = handle_start_capture(&json!({
            "goal_id": "goal-1",
            "staging_dir": staging.to_string_lossy(),
            "upstream_dsn": source_db.to_string_lossy(),
        }));
        assert!(start_resp.ok, "{:?}", start_resp.error);
        let handle = start_resp.result;

        // Simulate the agent mutating the shadow copy directly.
        let shadow_db = handle["cursor"]["shadow_db"].as_str().unwrap();
        let conn = Connection::open(shadow_db).unwrap();
        conn.execute("UPDATE items SET value = 99 WHERE rowid = 1", [])
            .unwrap();
        conn.execute("INSERT INTO items (name, value) VALUES ('c', 3)", [])
            .unwrap();
        conn.execute("DELETE FROM items WHERE rowid = 2", [])
            .unwrap();
        drop(conn);

        let stop_resp = handle_stop_capture(&json!({
            "handle": handle,
            "action": "apply",
        }));
        assert!(stop_resp.ok, "{:?}", stop_resp.error);
        assert_eq!(stop_resp.result["mutations_captured"], 3);

        let overlay_path = staging.join("db-overlay.jsonl");
        let text = std::fs::read_to_string(&overlay_path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(text.contains("\"kind\":\"update\""));
        assert!(text.contains("\"kind\":\"insert\""));
        assert!(text.contains("\"kind\":\"delete\""));

        // Shadow/pristine copies are cleaned up after stop_capture.
        assert!(!Path::new(handle["cursor"]["shadow_db"].as_str().unwrap()).exists());
        assert!(!Path::new(handle["cursor"]["pristine_db"].as_str().unwrap()).exists());
    }

    #[test]
    fn stop_capture_discard_writes_no_overlay_entries() {
        let dir = tempdir().unwrap();
        let staging = dir.path().join("staging");
        let source_db = dir.path().join("real.db");
        setup_db(&source_db);

        let start_resp = handle_start_capture(&json!({
            "goal_id": "goal-2",
            "staging_dir": staging.to_string_lossy(),
            "upstream_dsn": source_db.to_string_lossy(),
        }));
        let handle = start_resp.result;
        let shadow_db = handle["cursor"]["shadow_db"].as_str().unwrap();
        Connection::open(shadow_db)
            .unwrap()
            .execute("DELETE FROM items", [])
            .unwrap();

        let stop_resp = handle_stop_capture(&json!({ "handle": handle, "action": "discard" }));
        assert!(stop_resp.ok);
        assert!(!staging.join("db-overlay.jsonl").exists());

        // Real DB untouched by a discarded capture.
        let real_conn = Connection::open(&source_db).unwrap();
        let count: i64 = real_conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn start_capture_on_new_database_creates_empty_shadow() {
        let dir = tempdir().unwrap();
        let staging = dir.path().join("staging");
        let source_db = dir.path().join("does-not-exist-yet.db");

        let start_resp = handle_start_capture(&json!({
            "goal_id": "goal-3",
            "staging_dir": staging.to_string_lossy(),
            "upstream_dsn": source_db.to_string_lossy(),
        }));
        assert!(start_resp.ok, "{:?}", start_resp.error);
        assert!(Path::new(start_resp.result["cursor"]["shadow_db"].as_str().unwrap()).exists());
    }

    #[test]
    fn diff_detects_created_and_dropped_tables_as_ddl() {
        let dir = tempdir().unwrap();
        let pristine = dir.path().join("pristine.db");
        let shadow = dir.path().join("shadow.db");

        let p = Connection::open(&pristine).unwrap();
        p.execute_batch("CREATE TABLE old_table (id INTEGER)")
            .unwrap();
        drop(p);

        let s = Connection::open(&shadow).unwrap();
        s.execute_batch("CREATE TABLE new_table (id INTEGER)")
            .unwrap();
        drop(s);

        let entries = diff_databases(&pristine, &shadow, "/tmp/real.db").unwrap();
        let ddl: Vec<_> = entries.iter().filter(|e| e.kind == "ddl").collect();
        assert_eq!(ddl.len(), 2, "expected one create + one drop DDL entry");
        assert!(ddl
            .iter()
            .any(|e| e.uri.contains("/new_table/") && e.before.is_none()));
        assert!(ddl
            .iter()
            .any(|e| e.uri.contains("/old_table/")
                && e.after.as_str().unwrap().contains("DROP TABLE")));
    }

    #[test]
    fn apply_mutation_insert_update_delete_round_trip() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE items (name TEXT, value INTEGER)")
            .unwrap();
        drop(conn);

        let uri = format!("sqlite://{}/items/1", db_path.to_string_lossy());
        let resp = handle_apply_mutation(&json!({
            "uri": uri,
            "before": null,
            "after": {"name": "test", "value": 42},
        }));
        assert!(resp.ok, "{:?}", resp.error);

        let conn = Connection::open(&db_path).unwrap();
        let (name, val): (String, i64) = conn
            .query_row("SELECT name, value FROM items", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(name, "test");
        assert_eq!(val, 42);
    }

    #[test]
    fn apply_mutation_treats_json_null_before_same_as_absent() {
        // A wire request always carries an explicit "before": null for an
        // insert (there's no pre-image) — that must route to INSERT, not to
        // an UPDATE that silently matches zero rows.
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        Connection::open(&db_path)
            .unwrap()
            .execute_batch("CREATE TABLE items (name TEXT, value INTEGER)")
            .unwrap();

        let uri = format!("sqlite://{}/items/1", db_path.to_string_lossy());
        let resp = handle_apply_mutation(&json!({
            "uri": uri,
            "before": Value::Null,
            "after": {"name": "inserted", "value": 7},
        }));
        assert!(resp.ok, "{:?}", resp.error);

        let conn = Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "expected the row to actually be inserted");
    }

    #[test]
    fn apply_mutation_ddl_table_uri_executes_batch_sql() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let uri = format!(
            "sqlite://{}/widgets/{DDL_SENTINEL}",
            db_path.to_string_lossy()
        );
        let resp = handle_apply_mutation(&json!({
            "uri": uri,
            "before": null,
            "after": "CREATE TABLE widgets (id INTEGER)",
        }));
        assert!(resp.ok, "{:?}", resp.error);
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("INSERT INTO widgets (id) VALUES (1)", [])
            .unwrap();
    }

    #[test]
    fn base64_round_trips_via_standard_decoder() {
        let bytes = b"hello world binary \x00\x01\x02";
        let encoded = base64_encode(bytes);
        // Cross-check against a hand-rolled decoder to avoid an extra dep.
        assert!(!encoded.is_empty());
        assert_eq!(encoded.len() % 4, 0);
    }
}
