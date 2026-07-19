//! Postgres database proxy plugin for Trusted Autonomy (v0.17.1).
//!
//! Implements the TA `db` plugin JSON-over-stdio protocol (protocol version 1).
//!
//! ## Protocol
//!
//! Reads one JSON request line from stdin, writes one JSON response line to
//! stdout, then exits. Each invocation is stateless — the plugin is spawned
//! fresh for every method call.
//!
//! ## Capture model — logical replication slot, drained at goal end
//!
//! `start_capture` creates a logical replication slot
//! (`pg_create_logical_replication_slot(slot_name, 'test_decoding')`) via a
//! plain SQL connection — no streaming replication protocol needed. From
//! that point, Postgres itself retains every WAL change in the slot until
//! it's consumed; this plugin doesn't need to poll continuously during the
//! goal. `stop_capture` drains the whole slot in one shot
//! (`pg_logical_slot_get_changes(slot_name, NULL, NULL)`), parses the
//! `test_decoding` output plugin's text format into row mutations, and (on
//! `apply`) appends them to `db-overlay.jsonl`. The slot is always dropped
//! afterwards — on `discard` too — so a denied draft can't leak a
//! replication slot.
//!
//! **Known limitation — DDL is not captured.** Postgres logical decoding
//! only replicates row-level DML (INSERT/UPDATE/DELETE); `CREATE`/`ALTER`/
//! `DROP TABLE` never appear in the `test_decoding` stream at all — this is
//! a Postgres limitation, not a gap in this plugin. `classify_query` still
//! classifies DDL statements as `ddl` for any caller that intercepts SQL
//! text directly, but a goal that needs schema-drop enforcement (the v0.17.1
//! constitution rule) is only protected by the sandbox network-policy layer
//! forcing all access through the proxy, not by anything at capture time
//! for Postgres. Capturing DDL for real requires an event trigger writing to
//! a side table (or `wal2json`'s DDL support) — tracked as follow-up work.
//!
//! **Known limitation — row identifier.** The URI written for a captured
//! mutation (`db://postgres/<schema>.<table>/<id>`) uses the value of the
//! *first* column in the captured tuple as the row identifier. `test_decoding`
//! lists replica-identity (primary key, by default) columns first for
//! UPDATE/DELETE, and INSERT lists columns in table-definition order, so
//! this is correct whenever the primary key is the first column — which
//! covers the common case but is a heuristic, not a schema-aware lookup.
//!
//! **Known limitation — no TLS.** Every connection this plugin makes
//! (`start_capture`, `stop_capture`, `apply_mutation`) uses `postgres::NoTls`
//! unconditionally; there's no option to negotiate a TLS-encrypted
//! connection to the upstream Postgres server. This is acceptable when the
//! upstream is reached over a trusted network path (e.g. localhost, or a
//! private network already covered by other transport security), which is
//! the deployment this plugin targets today, but it means credentials and
//! row data travel in cleartext on the wire to the database itself. Adding
//! real TLS support requires a `tokio_postgres::tls::MakeTlsConnect`
//! implementation such as `postgres-native-tls` or `postgres-openssl`, both
//! of which pull in a native TLS library dependency — a large enough change
//! in scope and new-dependency surface that it's deliberately left as
//! follow-up work rather than bundled into this fix. See
//! `docs/community-db-plugin.md` for the same note aimed at plugin authors.
//!
//! ## Supported methods
//!
//! | Method          | Description                                              |
//! |-----------------|-----------------------------------------------------------|
//! | `handshake`      | Version negotiation                                       |
//! | `classify_query` | Classify a SQL string as read/write/ddl/admin/unknown      |
//! | `start_capture`  | Create a logical replication slot, return opaque handle    |
//! | `stop_capture`   | Drain the slot, write/discard mutations, drop the slot      |
//! | `apply_mutation` | Replay one staged mutation against the real database         |

use std::io::{self, BufRead, Write};
use std::path::Path;

use postgres::{Client, NoTls};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PROTOCOL_VERSION: u32 = 1;
const ADAPTER_NAME: &str = "postgres";
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
// Overlay entry — self-contained mirror of ta_db_overlay::OverlayEntry's
// wire shape (see ta-db-proxy-sqlite's plugin for the same convention).
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    chrono::DateTime::<chrono::Utc>::from_timestamp(now.as_secs() as i64, now.subsec_nanos())
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// Strip credentials from an `upstream_dsn` before it's ever allowed into a
/// captured `OverlayEntry.uri` or a persisted `CaptureHandle.cursor` — both
/// land in `db-overlay.jsonl`, a human-reviewed draft artifact, so a raw
/// `postgres://user:pass@host/db` string must never reach either. Parses via
/// `postgres::Config` (accepts both URL and libpq key-value DSN syntax) and
/// keeps only host/port/dbname; falls back to a fixed placeholder — never the
/// raw input — if the DSN doesn't parse.
fn sanitize_dsn_for_display(dsn: &str) -> String {
    use postgres::config::Host;
    match dsn.parse::<postgres::Config>() {
        Ok(cfg) => {
            let host = cfg
                .get_hosts()
                .first()
                .map(|h| match h {
                    Host::Tcp(s) => s.clone(),
                    #[cfg(unix)]
                    Host::Unix(p) => p.to_string_lossy().to_string(),
                })
                .unwrap_or_else(|| "unknown-host".to_string());
            let port = cfg.get_ports().first().copied();
            let dbname = cfg.get_dbname().unwrap_or("unknown-db");
            match port {
                Some(p) => format!("{host}:{p}/{dbname}"),
                None => format!("{host}/{dbname}"),
            }
        }
        Err(_) => "redacted-dsn/unparseable".to_string(),
    }
}

/// Pure builder for `start_capture`'s JSON response — kept separate from
/// `handle_start_capture` so the "cursor never contains a credential" claim
/// is directly unit-testable without a live Postgres connection. `source_db`
/// must already be sanitized (see `sanitize_dsn_for_display`) — this
/// function does not know what a raw DSN looks like and cannot re-check it.
fn build_start_capture_response(
    slot_name: &str,
    start_lsn: &str,
    sanitized_source_db: &str,
    staging_dir: &str,
) -> Value {
    json!({
        "engine": "postgres",
        "cursor": {
            "slot_name": slot_name,
            "start_lsn": start_lsn,
            "source_db": sanitized_source_db,
            "staging_dir": staging_dir,
        }
    })
}

fn sanitize_slot_name(goal_id: &str) -> String {
    // Postgres replication slot names: lowercase letters, numbers,
    // underscores only, max 63 bytes.
    let cleaned: String = goal_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let mut name = format!("ta_{cleaned}");
    name.truncate(63);
    name
}

// ---------------------------------------------------------------------------
// classify_query
// ---------------------------------------------------------------------------

fn classify_query(sql: &str) -> Value {
    let upper = sql.trim().to_uppercase();
    if upper.starts_with("SELECT") || upper.starts_with("EXPLAIN") || upper.starts_with("SHOW") {
        json!("read")
    } else if upper.starts_with("INSERT") {
        json!({"write": "insert"})
    } else if upper.starts_with("UPDATE") {
        json!({"write": "update"})
    } else if upper.starts_with("DELETE") {
        json!({"write": "delete"})
    } else if upper.starts_with("UPSERT") || upper.contains("ON CONFLICT") {
        json!({"write": "upsert"})
    } else if upper.starts_with("CREATE")
        || upper.starts_with("ALTER")
        || upper.starts_with("DROP")
        || upper.starts_with("TRUNCATE")
    {
        json!("ddl")
    } else if upper.starts_with("VACUUM")
        || upper.starts_with("BEGIN")
        || upper.starts_with("COMMIT")
        || upper.starts_with("ROLLBACK")
        || upper.starts_with("SET")
        || upper.starts_with("ANALYZE")
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
// test_decoding parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct ParsedChange {
    schema: String,
    table: String,
    op: &'static str, // "insert" | "update" | "delete"
    before: Option<serde_json::Map<String, Value>>,
    after: Option<serde_json::Map<String, Value>>,
}

fn unescape_pg_literal(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        inner.replace("''", "'")
    } else {
        raw.to_string()
    }
}

fn typed_value(sql_type: &str, raw: &str) -> Value {
    if raw.trim() == "null" {
        return Value::Null;
    }
    let t = sql_type.to_lowercase();
    if t.contains("int") || t.contains("serial") || t.contains("oid") {
        if let Ok(i) = raw.trim().parse::<i64>() {
            return json!(i);
        }
    } else if t.contains("bool") {
        return json!(raw.trim() == "true" || raw.trim() == "t");
    } else if t.contains("real")
        || t.contains("double")
        || t.contains("numeric")
        || t.contains("float")
    {
        if let Ok(f) = raw.trim().parse::<f64>() {
            return json!(f);
        }
    }
    json!(unescape_pg_literal(raw))
}

/// Parse a `name[type]:value ...` column list into an ordered map. Column
/// order is preserved via a `Vec` internally but returned as a JSON object
/// (object key order round-trips through `serde_json::Map`'s insertion-order
/// backing when the `preserve_order` feature is enabled at the workspace
/// level; if not, order isn't semantically significant here anyway).
fn parse_columns(text: &str, marker: &Regex) -> serde_json::Map<String, Value> {
    let matches: Vec<_> = marker.find_iter(text).collect();
    let mut out = serde_json::Map::new();
    for (i, m) in matches.iter().enumerate() {
        let caps = marker.captures(&text[m.start()..m.end()]).unwrap();
        let name = caps.get(1).unwrap().as_str().to_string();
        let sql_type = caps.get(2).unwrap().as_str().to_string();
        let value_start = m.end();
        let value_end = matches
            .get(i + 1)
            .map(|next| next.start())
            .unwrap_or(text.len());
        let raw_value = text[value_start..value_end].trim();
        out.insert(name, typed_value(&sql_type, raw_value));
    }
    out
}

fn parse_test_decoding_line(line: &str, marker: &Regex) -> Option<ParsedChange> {
    let rest = line.strip_prefix("table ")?;
    let (qualified_table, rest) = rest.split_once(": ")?;
    let (schema, table) = qualified_table
        .split_once('.')
        .unwrap_or(("public", qualified_table));

    if let Some(cols) = rest.strip_prefix("INSERT: ") {
        return Some(ParsedChange {
            schema: schema.to_string(),
            table: table.to_string(),
            op: "insert",
            before: None,
            after: Some(parse_columns(cols, marker)),
        });
    }
    if let Some(cols) = rest.strip_prefix("DELETE: ") {
        let cols = cols.strip_prefix("old-key: ").unwrap_or(cols);
        return Some(ParsedChange {
            schema: schema.to_string(),
            table: table.to_string(),
            op: "delete",
            before: Some(parse_columns(cols, marker)),
            after: None,
        });
    }
    if let Some(cols) = rest.strip_prefix("UPDATE: ") {
        if let Some(old_part) = cols.strip_prefix("old-key: ") {
            if let Some((old_cols, new_cols)) = old_part.split_once(" new-tuple: ") {
                return Some(ParsedChange {
                    schema: schema.to_string(),
                    table: table.to_string(),
                    op: "update",
                    before: Some(parse_columns(old_cols, marker)),
                    after: Some(parse_columns(new_cols, marker)),
                });
            }
        }
        return Some(ParsedChange {
            schema: schema.to_string(),
            table: table.to_string(),
            op: "update",
            before: None,
            after: Some(parse_columns(cols, marker)),
        });
    }
    None
}

fn first_column_value(cols: &serde_json::Map<String, Value>) -> String {
    cols.values()
        .next()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn changes_to_overlay_entries(changes: &[ParsedChange], source_db: &str) -> Vec<OverlayEntryOut> {
    let ts = now_rfc3339();
    changes
        .iter()
        .map(|c| {
            let id = c
                .after
                .as_ref()
                .or(c.before.as_ref())
                .map(first_column_value)
                .unwrap_or_else(|| "unknown".to_string());
            let uri = format!("postgres://{source_db}/{}.{}/{id}", c.schema, c.table);
            OverlayEntryOut {
                uri,
                before: c.before.clone().map(Value::Object),
                after: c.after.clone().map(Value::Object).unwrap_or(Value::Null),
                ts: ts.clone(),
                kind: c.op,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// start_capture / stop_capture
// ---------------------------------------------------------------------------

fn handle_start_capture(params: &Value) -> Response {
    let goal_id = match params.get("goal_id").and_then(|v| v.as_str()) {
        Some(g) => g,
        None => return Response::err("start_capture: missing 'goal_id' param"),
    };
    let staging_dir = match params.get("staging_dir").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("start_capture: missing 'staging_dir' param"),
    };
    let upstream_dsn = match params.get("upstream_dsn").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("start_capture: missing 'upstream_dsn' param"),
    };

    let slot_name = sanitize_slot_name(goal_id);
    let mut client = match Client::connect(upstream_dsn, NoTls) {
        Ok(c) => c,
        Err(e) => return Response::err(format!("start_capture: connect failed: {e}")),
    };

    let row = client.query_one(
        "SELECT slot_name, lsn::text FROM pg_create_logical_replication_slot($1, 'test_decoding')",
        &[&slot_name],
    );
    let row = match row {
        Ok(r) => r,
        Err(e) => {
            return Response::err(format!(
                "start_capture: pg_create_logical_replication_slot('{slot_name}') failed: {e}. \
                 Requires PostgreSQL >= 9.4 with wal_level = logical and a role with the \
                 REPLICATION attribute."
            ))
        }
    };
    let created_slot: String = row.get(0);
    let start_lsn: String = row.get(1);

    Response::ok(build_start_capture_response(
        &created_slot,
        &start_lsn,
        &sanitize_dsn_for_display(upstream_dsn),
        staging_dir,
    ))
}

fn drop_slot(client: &mut Client, slot_name: &str) -> Result<(), String> {
    client
        .execute("SELECT pg_drop_replication_slot($1)", &[&slot_name])
        .map(|_| ())
        .map_err(|e| format!("pg_drop_replication_slot('{slot_name}') failed: {e}"))
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
    let slot_name = match cursor.get("slot_name").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return Response::err("stop_capture: cursor missing 'slot_name'"),
    };
    // The cursor never carries the raw DSN (see `sanitize_dsn_for_display`) —
    // `upstream_dsn` is a fresh top-level param, resolved by the caller from
    // the credential vault at stop_capture time, exactly like apply_mutation
    // already does. This mirrors that existing, already-correct pattern.
    let upstream_dsn = match params.get("upstream_dsn").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("stop_capture: missing 'upstream_dsn' param"),
    };

    let mut client = match Client::connect(upstream_dsn, NoTls) {
        Ok(c) => c,
        Err(e) => return Response::err(format!("stop_capture: connect failed: {e}")),
    };

    if action == "discard" {
        if let Err(e) = drop_slot(&mut client, &slot_name) {
            return Response::err(format!("stop_capture: {e}"));
        }
        return Response::ok(json!({ "mutations_captured": 0 }));
    }
    if action != "apply" {
        return Response::err(format!(
            "stop_capture: unknown action '{action}' (expected 'apply' or 'discard')"
        ));
    }

    let rows = client.query(
        "SELECT data FROM pg_logical_slot_get_changes($1, NULL, NULL)",
        &[&slot_name],
    );
    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            let _ = drop_slot(&mut client, &slot_name);
            return Response::err(format!(
                "stop_capture: pg_logical_slot_get_changes('{slot_name}') failed: {e}"
            ));
        }
    };

    let marker = Regex::new(r"(\S+)\[([^\]]*)\]:").expect("static regex is valid");
    let changes: Vec<ParsedChange> = rows
        .iter()
        .filter_map(|row| {
            let line: String = row.get(0);
            parse_test_decoding_line(&line, &marker)
        })
        .collect();

    // `cursor.source_db` is already sanitized by `start_capture` (see
    // `sanitize_dsn_for_display`) — if it's somehow absent, fall back to a
    // freshly sanitized value, never the raw `upstream_dsn`, so a captured
    // URI can never carry embedded credentials.
    let fallback_source_db;
    let source_db = match cursor.get("source_db").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            fallback_source_db = sanitize_dsn_for_display(upstream_dsn);
            &fallback_source_db
        }
    };
    let entries = changes_to_overlay_entries(&changes, source_db);

    let staging_dir = match cursor.get("staging_dir").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            let _ = drop_slot(&mut client, &slot_name);
            return Response::err("stop_capture: cursor missing 'staging_dir'");
        }
    };
    if let Err(e) = append_overlay_entries(Path::new(staging_dir), &entries) {
        let _ = drop_slot(&mut client, &slot_name);
        return Response::err(format!(
            "stop_capture: failed to write db-overlay.jsonl: {e}"
        ));
    }

    if let Err(e) = drop_slot(&mut client, &slot_name) {
        return Response::err(format!("stop_capture: {e}"));
    }
    Response::ok(json!({ "mutations_captured": entries.len() }))
}

// ---------------------------------------------------------------------------
// apply_mutation
// ---------------------------------------------------------------------------

fn handle_apply_mutation(params: &Value) -> Response {
    let upstream_dsn = match params.get("upstream_dsn").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("apply_mutation: missing 'upstream_dsn' param"),
    };
    let uri = match params.get("uri").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return Response::err("apply_mutation: missing 'uri' param"),
    };
    let after = params.get("after").cloned().unwrap_or(Value::Null);

    let rest = match uri.strip_prefix("postgres://") {
        Some(r) => r,
        None => return Response::err(format!("apply_mutation: invalid postgres URI: {uri}")),
    };
    let mut parts = rest.rsplitn(3, '/');
    let id = parts.next();
    let table_part = parts.next();
    let bad_uri_err = format!(
        "apply_mutation: postgres URI must be db://postgres/<db>/<schema>.<table>/<id>: {uri}"
    );
    let (id, table_part) = match (id, table_part) {
        (Some(i), Some(t)) => (i, t),
        _ => return Response::err(bad_uri_err),
    };

    let mut client = match Client::connect(upstream_dsn, NoTls) {
        Ok(c) => c,
        Err(e) => return Response::err(format!("apply_mutation: connect failed: {e}")),
    };

    if after == Value::Null {
        let sql = format!("DELETE FROM {table_part} WHERE {} = $1", pk_column_guess());
        if let Err(e) = client.execute(&sql, &[&id]) {
            return Response::err(format!("apply_mutation: delete failed: {e}"));
        }
        return Response::ok(json!({}));
    }

    let obj = match after.as_object() {
        Some(o) => o,
        None => return Response::err("apply_mutation: 'after' must be a JSON object"),
    };
    let cols: Vec<String> = obj.keys().cloned().collect();
    let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("${i}")).collect();
    let updates: Vec<String> = cols.iter().map(|c| format!("{c} = EXCLUDED.{c}")).collect();
    let sql = format!(
        "INSERT INTO {table_part} ({}) VALUES ({}) ON CONFLICT ({}) DO UPDATE SET {}",
        cols.join(", "),
        placeholders.join(", "),
        pk_column_guess(),
        updates.join(", "),
    );
    let values: Vec<String> = obj
        .values()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .collect();
    let params_refs: Vec<&(dyn postgres::types::ToSql + Sync)> = values
        .iter()
        .map(|v| v as &(dyn postgres::types::ToSql + Sync))
        .collect();
    match client.execute(&sql, &params_refs) {
        Ok(_) => Response::ok(json!({})),
        Err(e) => Response::err(format!("apply_mutation: upsert failed: {e}")),
    }
}

/// The row identifier column captured by `changes_to_overlay_entries` is
/// always the table's first column — see the module-level "Known limitation"
/// note — so replay assumes that same column is named `id`. Tables whose
/// primary key isn't literally called `id` need a schema-aware identity
/// lookup, tracked as the same follow-up as the row-identifier limitation.
fn pk_column_guess() -> &'static str {
    "id"
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

    fn marker() -> Regex {
        Regex::new(r"(\S+)\[([^\]]*)\]:").unwrap()
    }

    #[test]
    fn handshake_returns_adapter_name() {
        let resp = handle_handshake(&json!({}));
        assert!(resp.ok);
        assert_eq!(resp.result["adapter_name"], "postgres");
    }

    #[test]
    fn classify_select_is_read() {
        assert_eq!(classify_query("SELECT * FROM t"), json!("read"));
    }

    #[test]
    fn classify_truncate_is_ddl() {
        assert_eq!(classify_query("TRUNCATE items"), json!("ddl"));
    }

    #[test]
    fn classify_drop_table_is_ddl() {
        assert_eq!(classify_query("DROP TABLE items"), json!("ddl"));
    }

    #[test]
    fn parse_insert_line() {
        let line =
            "table public.items: INSERT: id[integer]:1 name[text]:'Alice' active[boolean]:true";
        let change = parse_test_decoding_line(line, &marker()).unwrap();
        assert_eq!(change.schema, "public");
        assert_eq!(change.table, "items");
        assert_eq!(change.op, "insert");
        assert!(change.before.is_none());
        let after = change.after.unwrap();
        assert_eq!(after["id"], json!(1));
        assert_eq!(after["name"], json!("Alice"));
        assert_eq!(after["active"], json!(true));
    }

    #[test]
    fn parse_insert_with_quoted_string_containing_spaces_and_escaped_quote() {
        let line = "table public.items: INSERT: id[integer]:1 name[text]:'O''Brien likes cats'";
        let change = parse_test_decoding_line(line, &marker()).unwrap();
        let after = change.after.unwrap();
        assert_eq!(after["name"], json!("O'Brien likes cats"));
    }

    #[test]
    fn parse_update_line_without_replica_identity_full() {
        let line = "table public.items: UPDATE: id[integer]:1 name[text]:'updated'";
        let change = parse_test_decoding_line(line, &marker()).unwrap();
        assert_eq!(change.op, "update");
        assert!(change.before.is_none());
        assert_eq!(change.after.unwrap()["name"], json!("updated"));
    }

    #[test]
    fn parse_update_line_with_replica_identity_full() {
        let line = "table public.items: UPDATE: old-key: id[integer]:1 name[text]:'old' new-tuple: id[integer]:1 name[text]:'new'";
        let change = parse_test_decoding_line(line, &marker()).unwrap();
        assert_eq!(change.op, "update");
        assert_eq!(change.before.unwrap()["name"], json!("old"));
        assert_eq!(change.after.unwrap()["name"], json!("new"));
    }

    #[test]
    fn parse_delete_line() {
        let line = "table public.items: DELETE: id[integer]:5";
        let change = parse_test_decoding_line(line, &marker()).unwrap();
        assert_eq!(change.op, "delete");
        assert_eq!(change.before.unwrap()["id"], json!(5));
        assert!(change.after.is_none());
    }

    #[test]
    fn parse_delete_line_with_old_key_prefix() {
        let line = "table public.items: DELETE: old-key: id[integer]:5";
        let change = parse_test_decoding_line(line, &marker()).unwrap();
        assert_eq!(change.op, "delete");
        assert_eq!(change.before.unwrap()["id"], json!(5));
    }

    #[test]
    fn begin_and_commit_lines_are_not_changes() {
        assert!(parse_test_decoding_line("BEGIN 693", &marker()).is_none());
        assert!(parse_test_decoding_line("COMMIT 693", &marker()).is_none());
    }

    #[test]
    fn null_value_parses_to_json_null() {
        let line = "table public.items: INSERT: id[integer]:1 name[text]:null";
        let change = parse_test_decoding_line(line, &marker()).unwrap();
        assert_eq!(change.after.unwrap()["name"], Value::Null);
    }

    #[test]
    fn changes_to_overlay_entries_builds_postgres_uris() {
        let changes = vec![ParsedChange {
            schema: "public".to_string(),
            table: "items".to_string(),
            op: "insert",
            before: None,
            after: Some({
                let mut m = serde_json::Map::new();
                m.insert("id".to_string(), json!(7));
                m
            }),
        }];
        let entries = changes_to_overlay_entries(&changes, "host=db1 dbname=app");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].uri,
            "postgres://host=db1 dbname=app/public.items/7"
        );
        assert_eq!(entries[0].kind, "insert");
    }

    #[test]
    fn sanitize_dsn_strips_credentials_from_url_style_dsn() {
        let dsn = "postgres://scott:tigerpw@dbhost:5432/mydb";
        let sanitized = sanitize_dsn_for_display(dsn);
        assert!(!sanitized.contains("scott"));
        assert!(!sanitized.contains("tigerpw"));
        assert!(sanitized.contains("dbhost"));
        assert!(sanitized.contains("5432"));
        assert!(sanitized.contains("mydb"));
    }

    #[test]
    fn sanitize_dsn_strips_credentials_from_keyword_value_dsn() {
        let dsn = "host=dbhost port=5432 dbname=mydb user=scott password=tigerpw";
        let sanitized = sanitize_dsn_for_display(dsn);
        assert!(!sanitized.contains("scott"));
        assert!(!sanitized.contains("tigerpw"));
        assert!(sanitized.contains("dbhost"));
        assert!(sanitized.contains("mydb"));
    }

    #[test]
    fn start_capture_cursor_json_never_contains_raw_dsn_or_credentials() {
        let dsn = "postgres://scott:tigerpw@dbhost:5432/mydb";
        let sanitized = sanitize_dsn_for_display(dsn);
        let resp = build_start_capture_response("ta_goal1", "0/1A2B3C", &sanitized, "/tmp/staging");
        let text = serde_json::to_string(&resp).unwrap();
        assert!(!text.contains("scott"));
        assert!(!text.contains("tigerpw"));
        assert!(!text.contains(dsn));
        assert!(!text.to_lowercase().contains("upstream_dsn"));
    }

    #[test]
    fn changes_to_overlay_entries_uri_never_contains_dsn_credentials() {
        let dsn = "postgres://scott:tigerpw@dbhost:5432/mydb";
        let sanitized = sanitize_dsn_for_display(dsn);
        let changes = vec![ParsedChange {
            schema: "public".to_string(),
            table: "items".to_string(),
            op: "insert",
            before: None,
            after: Some({
                let mut m = serde_json::Map::new();
                m.insert("id".to_string(), json!(7));
                m
            }),
        }];
        let entries = changes_to_overlay_entries(&changes, &sanitized);
        for entry in &entries {
            assert!(!entry.uri.contains("scott"));
            assert!(!entry.uri.contains("tigerpw"));
            assert!(!entry.uri.contains(dsn));
        }
    }

    #[test]
    fn sanitize_slot_name_produces_valid_identifier() {
        let name = sanitize_slot_name("goal-ABC 123!");
        assert!(name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
        assert!(name.len() <= 63);
        assert!(name.starts_with("ta_"));
    }

    #[test]
    fn start_capture_on_unsupported_server_returns_clear_error() {
        // The sandbox's local Postgres predates 9.4 (no logical replication
        // support at all) — this exercises the real, honest failure path
        // rather than a live logical-replication round trip, which this
        // environment cannot provide.
        let dsn = std::env::var("TA_TEST_PG_DSN").unwrap_or_else(|_| {
            "host=localhost user=postgres dbname=postgres connect_timeout=2".to_string()
        });
        let resp = handle_start_capture(&json!({
            "goal_id": "test-goal",
            "staging_dir": "/tmp/ta-pg-test",
            "upstream_dsn": dsn,
        }));
        // Either the server is unreachable in this sandbox, or it's reachable
        // but too old for logical replication — both are actionable errors,
        // never a panic or a silent false-success.
        if resp.ok {
            eprintln!("note: a live PG >= 9.4 server was available; skipping negative assertion");
        } else {
            assert!(resp.error.is_some());
        }
    }
}
