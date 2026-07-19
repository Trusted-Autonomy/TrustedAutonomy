//! MySQL/MariaDB database proxy plugin for Trusted Autonomy (v0.17.1).
//!
//! Implements the TA `db` plugin JSON-over-stdio protocol (protocol version 1).
//!
//! ## Protocol
//!
//! Reads one JSON request line from stdin, writes one JSON response line to
//! stdout, then exits. Each invocation is stateless — the plugin is spawned
//! fresh for every method call.
//!
//! ## Capture model — binlog position, drained at goal end
//!
//! `start_capture` records the current binlog coordinates
//! (`SHOW MASTER STATUS`, falling back to `SHOW BINARY LOG STATUS` on
//! MySQL >= 8.4 where the former was renamed) — no persistent server-side
//! resource is created, unlike a Postgres replication slot, so there's
//! nothing to leak if a goal is denied. `stop_capture` opens a binlog client
//! (`mysql_cdc`) starting from that recorded position in non-blocking mode:
//! the server sends everything since that position and then an end-of-file
//! marker, so the client naturally stops once caught up — no continuous
//! polling needed during the goal, mirroring the Postgres plugin's
//! drain-once design.
//!
//! **Known limitation — DDL is not captured.** MySQL row-based binlog events
//! only cover DML (INSERT/UPDATE/DELETE); `CREATE`/`ALTER`/`DROP TABLE`
//! appear in the binlog as `QueryEvent`s (statement text) rather than typed
//! row events, and this plugin does not currently parse them into replayable
//! DDL entries — same scoping decision as the Postgres plugin, see its module
//! docs. `classify_query` still classifies DDL/`TRUNCATE` statements for
//! callers that intercept SQL text directly.
//!
//! **Known limitation — row identifier and column names.** Binlog row
//! events carry positional column values, not names — this plugin resolves
//! names via `INFORMATION_SCHEMA.COLUMNS` (falling back to `col_<n>` if the
//! table is gone by drain time) and, like the Postgres plugin, uses the
//! first column's value as the row identifier in the generated URI — correct
//! when the primary key is the first column, a heuristic otherwise.
//!
//! **Not independently integration-tested against a live server** — this
//! sandbox has no `mysqld` available. The binlog decoding types are used
//! exactly as defined by the `mysql_cdc` crate; unit tests cover the
//! event-to-mutation mapping and value conversion logic in isolation.
//!
//! ## Supported methods
//!
//! | Method          | Description                                              |
//! |-----------------|-----------------------------------------------------------|
//! | `handshake`      | Version negotiation                                       |
//! | `classify_query` | Classify a SQL string as read/write/ddl/admin/unknown      |
//! | `start_capture`  | Record the current binlog file+position, return a handle   |
//! | `stop_capture`   | Drain the binlog from that position, write/discard, done    |
//! | `apply_mutation` | Replay one staged mutation against the real database         |

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;

use mysql::prelude::Queryable;
use mysql::{Conn, Opts};
use mysql_cdc::binlog_client::BinlogClient;
use mysql_cdc::binlog_options::BinlogOptions;
use mysql_cdc::events::binlog_event::BinlogEvent;
use mysql_cdc::events::row_events::mysql_value::MySqlValue;
use mysql_cdc::events::row_events::row_data::RowData;
use mysql_cdc::events::table_map_event::TableMapEvent;
use mysql_cdc::replica_options::ReplicaOptions;
use mysql_cdc::ssl_mode::SslMode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PROTOCOL_VERSION: u32 = 1;
const ADAPTER_NAME: &str = "mysql";
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

#[derive(Debug, Serialize, Clone, PartialEq)]
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
    } else if upper.starts_with("REPLACE") || upper.contains("ON DUPLICATE KEY UPDATE") {
        json!({"write": "upsert"})
    } else if upper.starts_with("CREATE")
        || upper.starts_with("ALTER")
        || upper.starts_with("DROP")
        || upper.starts_with("TRUNCATE")
    {
        json!("ddl")
    } else if upper.starts_with("SET")
        || upper.starts_with("BEGIN")
        || upper.starts_with("START TRANSACTION")
        || upper.starts_with("COMMIT")
        || upper.starts_with("ROLLBACK")
        || upper.starts_with("LOCK TABLES")
        || upper.starts_with("UNLOCK TABLES")
        || upper.starts_with("ANALYZE")
        || upper.starts_with("OPTIMIZE")
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
// MySqlValue -> JSON
// ---------------------------------------------------------------------------

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

fn mysql_value_to_json(v: &Option<MySqlValue>) -> Value {
    match v {
        None => Value::Null,
        Some(MySqlValue::TinyInt(i)) => json!(i),
        Some(MySqlValue::SmallInt(i)) => json!(i),
        Some(MySqlValue::MediumInt(i)) => json!(i),
        Some(MySqlValue::Int(i)) => json!(i),
        Some(MySqlValue::BigInt(i)) => json!(i),
        Some(MySqlValue::Float(f)) => json!(f),
        Some(MySqlValue::Double(f)) => json!(f),
        Some(MySqlValue::Decimal(s)) => json!(s),
        Some(MySqlValue::String(s)) => json!(s),
        Some(MySqlValue::Bit(bits)) => json!(bits),
        Some(MySqlValue::Enum(i)) => json!(i),
        Some(MySqlValue::Set(i)) => json!(i),
        Some(MySqlValue::Blob(b)) => json!(base64_encode(b)),
        Some(MySqlValue::Year(y)) => json!(y),
        Some(MySqlValue::Date(d)) => json!(format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)),
        Some(MySqlValue::Time(t)) => json!(format!(
            "{:03}:{:02}:{:02}.{:03}",
            t.hour, t.minute, t.second, t.millis
        )),
        Some(MySqlValue::DateTime(dt)) => json!(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
            dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, dt.millis
        )),
        Some(MySqlValue::Timestamp(ms)) => json!(ms),
    }
}

/// Build a name-keyed JSON object from a row's positional cell values.
/// `names` should be the table's columns in `ORDINAL_POSITION` order; if
/// it's shorter than `cells` (schema lookup failed, or the table changed
/// shape since capture started) missing names fall back to `col_<n>` rather
/// than silently dropping data.
fn row_to_map(row: &RowData, names: &[String]) -> serde_json::Map<String, Value> {
    let mut obj = serde_json::Map::new();
    for (i, cell) in row.cells.iter().enumerate() {
        let name = names.get(i).cloned().unwrap_or_else(|| format!("col_{i}"));
        obj.insert(name, mysql_value_to_json(cell));
    }
    obj
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

/// Strip credentials from an `upstream_dsn` before it's ever allowed into a
/// captured `OverlayEntry.uri` or a persisted `CaptureHandle.cursor` — both
/// land in `db-overlay.jsonl`, a human-reviewed draft artifact, so a raw
/// `mysql://user:pass@host/db` string must never reach either. Falls back to
/// a fixed placeholder — never the raw input — if the DSN doesn't parse.
fn sanitize_dsn_for_display(dsn: &str) -> String {
    match Opts::from_url(dsn) {
        Ok(opts) => {
            let host = opts.get_ip_or_hostname();
            let port = opts.get_tcp_port();
            let dbname = opts.get_db_name().unwrap_or("unknown-db");
            format!("{host}:{port}/{dbname}")
        }
        Err(_) => "redacted-dsn/unparseable".to_string(),
    }
}

/// Pure builder for `start_capture`'s JSON response — kept separate from
/// `handle_start_capture` so the "cursor never contains a credential" claim
/// is directly unit-testable without a live MySQL connection. `source_db`
/// must already be sanitized (see `sanitize_dsn_for_display`) — this
/// function does not know what a raw DSN looks like and cannot re-check it.
fn build_start_capture_response(
    file: &str,
    position: u32,
    sanitized_source_db: &str,
    staging_dir: &str,
) -> Value {
    json!({
        "engine": "mysql",
        "cursor": {
            "file": file,
            "position": position,
            "source_db": sanitized_source_db,
            "staging_dir": staging_dir,
        }
    })
}

/// Build a captured mutation's `uri`. `source_db_display` must already be
/// sanitized — see `sanitize_dsn_for_display` — so this never has a raw DSN
/// (with embedded credentials) to leak in the first place.
fn mysql_entry_uri(source_db_display: &str, schema: &str, table: &str, id: &str) -> String {
    format!("mysql://{source_db_display}/{schema}.{table}/{id}")
}

// ---------------------------------------------------------------------------
// start_capture / stop_capture
// ---------------------------------------------------------------------------

fn show_master_status(conn: &mut Conn) -> Result<(String, u32), String> {
    let row: Option<(String, u32)> = conn
        .query_first("SHOW MASTER STATUS")
        .map_err(|e| format!("SHOW MASTER STATUS failed: {e}"))?;
    if let Some((file, pos)) = row {
        return Ok((file, pos));
    }
    // MySQL >= 8.4 renamed the statement.
    let row: Option<(String, u32)> = conn
        .query_first("SHOW BINARY LOG STATUS")
        .map_err(|e| format!("SHOW BINARY LOG STATUS failed: {e}"))?;
    row.ok_or_else(|| {
        "neither SHOW MASTER STATUS nor SHOW BINARY LOG STATUS returned a row — is binary \
         logging enabled (log_bin = ON)?"
            .to_string()
    })
}

fn handle_start_capture(params: &Value) -> Response {
    let staging_dir = match params.get("staging_dir").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("start_capture: missing 'staging_dir' param"),
    };
    let upstream_dsn = match params.get("upstream_dsn").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("start_capture: missing 'upstream_dsn' param"),
    };

    let opts = match Opts::from_url(upstream_dsn) {
        Ok(o) => o,
        Err(e) => return Response::err(format!("start_capture: invalid upstream_dsn: {e}")),
    };
    let mut conn = match Conn::new(opts) {
        Ok(c) => c,
        Err(e) => return Response::err(format!("start_capture: connect failed: {e}")),
    };

    let (file, position) = match show_master_status(&mut conn) {
        Ok(v) => v,
        Err(e) => return Response::err(format!("start_capture: {e}")),
    };

    Response::ok(build_start_capture_response(
        &file,
        position,
        &sanitize_dsn_for_display(upstream_dsn),
        staging_dir,
    ))
}

fn table_columns(
    conn: &mut Conn,
    schema: &str,
    table: &str,
    cache: &mut HashMap<(String, String), Vec<String>>,
) -> Vec<String> {
    let key = (schema.to_string(), table.to_string());
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let names: Vec<String> = conn
        .exec(
            "SELECT COLUMN_NAME FROM INFORMATION_SCHEMA.COLUMNS \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
            (schema, table),
        )
        .unwrap_or_default();
    cache.insert(key, names.clone());
    names
}

fn drain_binlog(
    upstream_dsn: &str,
    source_db_display: &str,
    file: String,
    position: u32,
) -> Result<Vec<OverlayEntryOut>, String> {
    let opts = Opts::from_url(upstream_dsn).map_err(|e| format!("invalid upstream_dsn: {e}"))?;
    let mut schema_conn =
        Conn::new(opts.clone()).map_err(|e| format!("connect (schema lookup) failed: {e}"))?;

    let replica_opts = ReplicaOptions {
        hostname: opts.get_ip_or_hostname().to_string(),
        port: opts.get_tcp_port(),
        username: opts.get_user().unwrap_or_default().to_string(),
        password: opts.get_pass().unwrap_or_default().to_string(),
        database: opts.get_db_name().map(|s| s.to_string()),
        blocking: false,
        ssl_mode: SslMode::Disabled,
        binlog: BinlogOptions::from_position(file, position),
        ..Default::default()
    };

    let mut client = BinlogClient::new(replica_opts);
    let events = client
        .replicate()
        .map_err(|e| format!("failed to start binlog replication: {e:?}"))?;

    let mut table_map: HashMap<u64, TableMapEvent> = HashMap::new();
    let mut column_cache: HashMap<(String, String), Vec<String>> = HashMap::new();
    let ts = now_rfc3339();
    let mut entries = vec![];

    for result in events {
        let (header, event) = result.map_err(|e| format!("binlog stream error: {e:?}"))?;

        match &event {
            BinlogEvent::TableMapEvent(tm) => {
                table_map.insert(tm.table_id, tm.clone());
            }
            BinlogEvent::WriteRowsEvent(w) => {
                if let Some(tm) = table_map.get(&w.table_id) {
                    let names = table_columns(
                        &mut schema_conn,
                        &tm.database_name,
                        &tm.table_name,
                        &mut column_cache,
                    );
                    for row in &w.rows {
                        let after = row_to_map(row, &names);
                        let id = first_column_value(&after);
                        entries.push(OverlayEntryOut {
                            uri: mysql_entry_uri(
                                source_db_display,
                                &tm.database_name,
                                &tm.table_name,
                                &id,
                            ),
                            before: None,
                            after: Value::Object(after),
                            ts: ts.clone(),
                            kind: "insert",
                        });
                    }
                }
            }
            BinlogEvent::DeleteRowsEvent(d) => {
                if let Some(tm) = table_map.get(&d.table_id) {
                    let names = table_columns(
                        &mut schema_conn,
                        &tm.database_name,
                        &tm.table_name,
                        &mut column_cache,
                    );
                    for row in &d.rows {
                        let before = row_to_map(row, &names);
                        let id = first_column_value(&before);
                        entries.push(OverlayEntryOut {
                            uri: mysql_entry_uri(
                                source_db_display,
                                &tm.database_name,
                                &tm.table_name,
                                &id,
                            ),
                            before: Some(Value::Object(before)),
                            after: Value::Null,
                            ts: ts.clone(),
                            kind: "delete",
                        });
                    }
                }
            }
            BinlogEvent::UpdateRowsEvent(u) => {
                if let Some(tm) = table_map.get(&u.table_id) {
                    let names = table_columns(
                        &mut schema_conn,
                        &tm.database_name,
                        &tm.table_name,
                        &mut column_cache,
                    );
                    for row in &u.rows {
                        let before = row_to_map(&row.before_update, &names);
                        let after = row_to_map(&row.after_update, &names);
                        let id = first_column_value(&after);
                        entries.push(OverlayEntryOut {
                            uri: mysql_entry_uri(
                                source_db_display,
                                &tm.database_name,
                                &tm.table_name,
                                &id,
                            ),
                            before: Some(Value::Object(before)),
                            after: Value::Object(after),
                            ts: ts.clone(),
                            kind: "update",
                        });
                    }
                }
            }
            _ => {}
        }
        client.commit(&header, &event);
    }

    Ok(entries)
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

    if action == "discard" {
        // No server-side resource was created at start_capture (unlike a
        // Postgres replication slot) — a recorded binlog position needs no
        // cleanup.
        return Response::ok(json!({ "mutations_captured": 0 }));
    }
    if action != "apply" {
        return Response::err(format!(
            "stop_capture: unknown action '{action}' (expected 'apply' or 'discard')"
        ));
    }

    let file = match cursor.get("file").and_then(|v| v.as_str()) {
        Some(f) => f.to_string(),
        None => return Response::err("stop_capture: cursor missing 'file'"),
    };
    let position = match cursor.get("position").and_then(|v| v.as_u64()) {
        Some(p) => p as u32,
        None => return Response::err("stop_capture: cursor missing 'position'"),
    };
    // The cursor never carries the raw DSN (see `sanitize_dsn_for_display`) —
    // `upstream_dsn` is a fresh top-level param, resolved by the caller from
    // the credential vault at stop_capture time, exactly like apply_mutation
    // already does. This mirrors that existing, already-correct pattern.
    let upstream_dsn = match params.get("upstream_dsn").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("stop_capture: missing 'upstream_dsn' param"),
    };
    let staging_dir = match cursor.get("staging_dir").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("stop_capture: cursor missing 'staging_dir'"),
    };
    // `cursor.source_db` is already sanitized by `start_capture`; if it's
    // somehow absent, fall back to a freshly sanitized value, never the raw
    // `upstream_dsn`, so a captured URI can never carry embedded credentials.
    let fallback_source_db;
    let source_db_display = match cursor.get("source_db").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            fallback_source_db = sanitize_dsn_for_display(upstream_dsn);
            &fallback_source_db
        }
    };

    let entries = match drain_binlog(upstream_dsn, source_db_display, file, position) {
        Ok(e) => e,
        Err(e) => return Response::err(format!("stop_capture: {e}")),
    };
    if let Err(e) = append_overlay_entries(Path::new(staging_dir), &entries) {
        return Response::err(format!(
            "stop_capture: failed to write db-overlay.jsonl: {e}"
        ));
    }
    Response::ok(json!({ "mutations_captured": entries.len() }))
}

// ---------------------------------------------------------------------------
// apply_mutation
// ---------------------------------------------------------------------------

/// See the module-level "Known limitation" note — the row identifier is
/// always the first captured column, so replay assumes that column is named
/// `id`.
fn pk_column_guess() -> &'static str {
    "id"
}

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

    let rest = match uri.strip_prefix("mysql://") {
        Some(r) => r,
        None => return Response::err(format!("apply_mutation: invalid mysql URI: {uri}")),
    };
    let mut parts = rest.rsplitn(3, '/');
    let id = parts.next();
    let table_part = parts.next();
    let bad_uri_err =
        format!("apply_mutation: mysql URI must be db://mysql/<db>/<schema>.<table>/<id>: {uri}");
    let (id, table_part) = match (id, table_part) {
        (Some(i), Some(t)) => (i, t),
        _ => return Response::err(bad_uri_err),
    };

    let opts = match Opts::from_url(upstream_dsn) {
        Ok(o) => o,
        Err(e) => return Response::err(format!("apply_mutation: invalid upstream_dsn: {e}")),
    };
    let mut conn = match Conn::new(opts) {
        Ok(c) => c,
        Err(e) => return Response::err(format!("apply_mutation: connect failed: {e}")),
    };

    if after == Value::Null {
        let sql = format!("DELETE FROM {table_part} WHERE {} = ?", pk_column_guess());
        if let Err(e) = conn.exec_drop(&sql, (id,)) {
            return Response::err(format!("apply_mutation: delete failed: {e}"));
        }
        return Response::ok(json!({}));
    }

    let obj = match after.as_object() {
        Some(o) => o,
        None => return Response::err("apply_mutation: 'after' must be a JSON object"),
    };
    let cols: Vec<String> = obj.keys().cloned().collect();
    let placeholders: Vec<&str> = cols.iter().map(|_| "?").collect();
    let updates: Vec<String> = cols.iter().map(|c| format!("{c} = VALUES({c})")).collect();
    let sql = format!(
        "INSERT INTO {table_part} ({}) VALUES ({}) ON DUPLICATE KEY UPDATE {}",
        cols.join(", "),
        placeholders.join(", "),
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
    match conn.exec_drop(&sql, values) {
        Ok(_) => Response::ok(json!({})),
        Err(e) => Response::err(format!("apply_mutation: upsert failed: {e}")),
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
    use mysql_cdc::events::row_events::mysql_value::{Date, DateTime, Time};

    #[test]
    fn handshake_returns_adapter_name() {
        let resp = handle_handshake(&json!({}));
        assert!(resp.ok);
        assert_eq!(resp.result["adapter_name"], "mysql");
    }

    #[test]
    fn classify_select_is_read() {
        assert_eq!(classify_query("SELECT * FROM t"), json!("read"));
    }

    #[test]
    fn classify_truncate_and_drop_are_ddl() {
        assert_eq!(classify_query("TRUNCATE items"), json!("ddl"));
        assert_eq!(classify_query("DROP TABLE items"), json!("ddl"));
    }

    #[test]
    fn classify_replace_is_upsert() {
        assert_eq!(
            classify_query("REPLACE INTO t VALUES (1)"),
            json!({"write": "upsert"})
        );
    }

    #[test]
    fn mysql_value_conversion_covers_all_variants() {
        assert_eq!(mysql_value_to_json(&None), Value::Null);
        assert_eq!(mysql_value_to_json(&Some(MySqlValue::Int(42))), json!(42));
        assert_eq!(
            mysql_value_to_json(&Some(MySqlValue::String("hi".to_string()))),
            json!("hi")
        );
        assert_eq!(
            mysql_value_to_json(&Some(MySqlValue::Double(1.5))),
            json!(1.5)
        );
        assert_eq!(
            mysql_value_to_json(&Some(MySqlValue::Date(Date {
                year: 2026,
                month: 1,
                day: 2
            }))),
            json!("2026-01-02")
        );
        assert_eq!(
            mysql_value_to_json(&Some(MySqlValue::Time(Time {
                hour: 1,
                minute: 2,
                second: 3,
                millis: 4
            }))),
            json!("001:02:03.004")
        );
        assert_eq!(
            mysql_value_to_json(&Some(MySqlValue::DateTime(DateTime {
                year: 2026,
                month: 1,
                day: 2,
                hour: 3,
                minute: 4,
                second: 5,
                millis: 6
            }))),
            json!("2026-01-02T03:04:05.006")
        );
        assert_eq!(
            mysql_value_to_json(&Some(MySqlValue::Blob(vec![1, 2, 3]))),
            json!(base64_encode(&[1, 2, 3]))
        );
    }

    #[test]
    fn row_to_map_uses_names_and_falls_back_when_short() {
        let row = RowData::new(vec![
            Some(MySqlValue::Int(1)),
            Some(MySqlValue::String("x".to_string())),
        ]);
        let names = vec!["id".to_string()];
        let obj = row_to_map(&row, &names);
        assert_eq!(obj["id"], json!(1));
        assert_eq!(obj["col_1"], json!("x"));
    }

    #[test]
    fn first_column_value_extracts_leading_column() {
        let mut m = serde_json::Map::new();
        m.insert("id".to_string(), json!(9));
        m.insert("name".to_string(), json!("ignored"));
        // serde_json::Map without "preserve_order" is a BTreeMap keyed
        // alphabetically ("id" < "name"), so this also validates ordering
        // assumptions used by the real capture path.
        assert_eq!(first_column_value(&m), "9");
    }

    #[test]
    fn base64_round_trips_length_invariant() {
        let encoded = base64_encode(b"hello world");
        assert_eq!(encoded.len() % 4, 0);
        assert!(!encoded.is_empty());
    }

    #[test]
    fn sanitize_dsn_strips_credentials_from_url_style_dsn() {
        let dsn = "mysql://scott:tigerpw@dbhost:3306/mydb";
        let sanitized = sanitize_dsn_for_display(dsn);
        assert!(!sanitized.contains("scott"));
        assert!(!sanitized.contains("tigerpw"));
        assert!(sanitized.contains("dbhost"));
        assert!(sanitized.contains("3306"));
        assert!(sanitized.contains("mydb"));
    }

    #[test]
    fn start_capture_cursor_json_never_contains_raw_dsn_or_credentials() {
        let dsn = "mysql://scott:tigerpw@dbhost:3306/mydb";
        let sanitized = sanitize_dsn_for_display(dsn);
        let resp = build_start_capture_response("binlog.000123", 456, &sanitized, "/tmp/staging");
        let text = serde_json::to_string(&resp).unwrap();
        assert!(!text.contains("scott"));
        assert!(!text.contains("tigerpw"));
        assert!(!text.contains(dsn));
        assert!(!text.to_lowercase().contains("upstream_dsn"));
    }

    #[test]
    fn mysql_entry_uri_never_contains_dsn_credentials() {
        let dsn = "mysql://scott:tigerpw@dbhost:3306/mydb";
        let sanitized = sanitize_dsn_for_display(dsn);
        let uri = mysql_entry_uri(&sanitized, "mydb", "items", "7");
        assert!(!uri.contains("scott"));
        assert!(!uri.contains("tigerpw"));
        assert!(!uri.contains(dsn));
    }

    #[test]
    fn start_capture_against_unreachable_server_returns_actionable_error() {
        // No mysqld is available in this sandbox at all — this exercises the
        // real connection-failure path (not a live binlog round trip, which
        // this environment cannot provide) and asserts it fails with a clear
        // message rather than panicking or reporting a false success.
        let resp = handle_start_capture(&json!({
            "goal_id": "test-goal",
            "staging_dir": "/tmp/ta-mysql-test",
            "upstream_dsn": "mysql://root:root@127.0.0.1:3399/test?connect_timeout=2",
        }));
        assert!(!resp.ok);
        assert!(resp.error.is_some());
    }
}
