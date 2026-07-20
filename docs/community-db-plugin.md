# Building a Trusted Autonomy Database Proxy Plugin

This guide explains how to add a new database backend to Trusted Autonomy's governed database
proxy (v0.17.1) — Postgres, MySQL, and SQLite ship as ordinary examples of this, not as special
cases. A community author (or you, adding support for a fourth engine) writes exactly the same
kind of package: a `plugin.toml` manifest plus an executable, dropped into
`.ta/plugins/db/<name>/`. No TA core change, no recompile.

This is the **Plugin** category from `docs/USAGE.md` → "Authoring a Plugin" — call/response over
newline-delimited JSON on stdin/stdout, discovered by convention. If you haven't read that section
yet, start there for the shared manifest schema and wire envelope; this doc covers only what's
specific to `db`-kind plugins.

## Why this exists

TA's database governance model requires that an agent working inside a goal can never reach the
real database directly — every read and write goes through a mediating proxy, and every mutation
is captured for human review before it's replayed against the real database. That guarantee is
enforced two ways, and both matter to you as a plugin author:

1. **Network policy.** For any goal referencing a `db://` URI, `ta-sandbox`'s `NetworkPolicy` is
   configured via `NetworkPolicy::for_db_proxy(upstream_host, upstream_port, proxy_addr)` — it
   denies direct egress to the real database's host:port and allows only your plugin's own local
   address. The agent's environment is never handed the real DSN.
2. **Credential vault.** The real connection string is resolved exclusively by TA core from
   `ta-credentials`'s vault and passed to your plugin as `upstream_dsn` in each request — your
   plugin sees it, the agent never does.

Together, these mean an agent has no network path to the real database that doesn't pass through
your plugin. This is the same "one uniform contract, agent literally cannot reach around it"
property the sandbox already guarantees for filesystem and command execution.

## The five methods

Your plugin's `plugin.toml` has `type = "db"`. It must implement these methods, each a
`{method, params}` request in, a `{ok, result}` or `{ok:false, error}` response out — one process
spawn per call (see `docs/USAGE.md`'s wire protocol section for the exact framing):

| Method | When it's called | Purpose |
|---|---|---|
| `handshake` | Plugin discovery/health check | Report `plugin_version`, `protocol_version`, `adapter_name`, `capabilities` |
| `classify_query` | Any time TA needs to know if a SQL string is a read, write, DDL, or admin statement | Advisory classification, used for policy decisions |
| `start_capture` | Goal start, before the agent gets any DB access | Begin change-capture, return an opaque `CaptureHandle` |
| `stop_capture` | Goal apply (`action: "apply"`) or deny (`action: "discard"`) | End change-capture, write mutations for review (or discard them), release any capture resource |
| `apply_mutation` | Once per staged mutation, at `ta draft apply` | Replay one reviewed mutation against the real database |

### `classify_query`

```
→ {"method":"classify_query","params":{"query":"DROP TABLE users"}}
← {"ok":true,"result":{"class":"ddl"}}
```

`class` is one of: `"read"`, `{"write":"insert"|"update"|"delete"|"upsert"}`, `"ddl"`, `"admin"`,
`"unknown"`. This mirrors `ta_db_proxy::QueryClass`'s JSON shape exactly — match it even though
your plugin has no Rust dependency on that crate; it's a data contract, not a shared type.

### `start_capture`

```
→ {"method":"start_capture","params":{
     "goal_id":"7df02c4b-...",
     "staging_dir":"/path/to/.ta/staging/<goal>",
     "upstream_dsn":"<resolved from the credential vault>"
   }}
← {"ok":true,"result":{"engine":"my-db","cursor":{ /* whatever your plugin needs later */ }}}
```

`cursor` is opaque to TA core — round-tripped back to you unmodified at `stop_capture`. Put
whatever your engine needs to resume: a replication slot name, a recorded log position, a
shadow-copy path. Nothing outside your own plugin ever inspects it.

Design your capture mechanism around one constraint: **your plugin is spawned fresh for every
call** — there's no long-lived process to hold a streaming connection open for the duration of the
goal. The bundled plugins solve this by making the underlying database do the retention: Postgres
creates a logical replication slot (the WAL is retained server-side until consumed — no
continuous polling needed), MySQL records a binlog position (nothing to hold open at all), SQLite
makes a shadow-copy snapshot. Pick whatever fits your engine's native change-capture primitive.

### `stop_capture`

```
→ {"method":"stop_capture","params":{
     "upstream_dsn":"<resolved fresh from the credential vault for this call>",
     "handle":{"engine":"my-db","cursor":{...}},
     "action":"apply"   // or "discard"
   }}
← {"ok":true,"result":{"mutations_captured":3}}
```

`upstream_dsn` is resolved by the caller for this call specifically — the same way it is for
`start_capture` and `apply_mutation` — **not** sourced from your `cursor`. If your engine needs to
reconnect to drain captured changes (a replication slot, a binlog position, ...), use this
argument; never store the DSN in `cursor` and read it back here (see "Never let `upstream_dsn`
leak into anything persisted or reviewer-facing" below).

On `"discard"`: throw the captured changes away, release any resource you created in
`start_capture` (drop a replication slot, delete a shadow file — whatever it is, release it here
too, not only on `"apply"`; a denied draft that leaks your capture resource is a real bug).

On `"apply"`: convert captured changes into the same JSON-line shape
`ta_db_overlay::OverlayEntry` reads, and **append** them to `<staging_dir>/db-overlay.jsonl`:

```json
{"uri":"my-db://<db>/<table>/<id>","before":null,"after":{"col":"value"},"ts":"2026-07-19T00:00:00Z","kind":"insert"}
```

| Field | Type | Notes |
|---|---|---|
| `uri` | string | Your own scheme, e.g. `my-db://<connection-identifier>/<table>/<row-id>` — a sanitized identifier, never the raw `upstream_dsn` (see below) |
| `before` | object or `null` | Pre-image; `null` for inserts |
| `after` | object or `null` | Post-image; `null` for deletes |
| `ts` | string | RFC3339 timestamp |
| `kind` | string | `"insert"`, `"update"`, `"delete"`, or `"ddl"` |

This file is append-only and consumer-side dedupes by `uri` (last write per URI wins) — you don't
need to read or rewrite it, just append your new entries.

### `apply_mutation`

```
→ {"method":"apply_mutation","params":{
     "upstream_dsn":"...",
     "uri":"my-db://mydb/users/42",
     "before":{"name":"old"},
     "after":{"name":"new"},
     "staging_dir":"..."
   }}
← {"ok":true,"result":{}}
```

Called once per reviewed mutation, in the order `DraftOverlay::list_mutations()` returns them.
Replay it against `upstream_dsn`. `after == null` means delete; `before == null` means insert;
otherwise it's an update (or, if your `uri` scheme encodes a DDL sentinel the way the SQLite
plugin does, a schema statement to execute as-is).

## Known limitations worth designing around

The bundled plugins are honest about two gaps rather than papering over them — you'll likely hit
the same ones:

- **DDL capture.** Both logical replication (Postgres) and row-based binlog events (MySQL) only
  carry DML. `CREATE`/`ALTER`/`DROP TABLE` don't appear in either stream at all — that's a
  property of the underlying replication mechanism, not a gap you can close by trying harder in
  the decoder. If your engine's native change-capture has the same property, say so in your
  plugin's docs rather than silently dropping schema changes.
- **Row identifier.** Change-capture streams typically give you column values positionally, not
  by primary-key name. The bundled plugins use "the first captured column" as a heuristic row
  identifier for the generated URI — correct when the PK is genuinely first, a heuristic
  otherwise. A schema-aware lookup would fix this properly; it's a reasonable place to improve on
  the reference implementations.
- **No TLS on the bundled Postgres plugin.** `ta-db-proxy-postgres` connects to the upstream
  server with `postgres::NoTls` unconditionally — there's no option to negotiate an encrypted
  connection. This is a known, documented limitation, not an oversight: real TLS support needs a
  `tokio_postgres::tls::MakeTlsConnect` implementation (`postgres-native-tls` or
  `postgres-openssl`), and both pull in a native TLS library as a new dependency. If your engine's
  driver supports TLS more cheaply than that, add it — just don't let credentials leak in the
  process (see the next point).

## Never let `upstream_dsn` leak into anything persisted or reviewer-facing

`upstream_dsn` carries embedded credentials (`postgres://user:pass@host/db`) — treat it as a
secret the whole time your plugin holds it, not just at connection time. Concretely:

- **`OverlayEntry.uri`** (written to `db-overlay.jsonl`, a human-reviewed draft artifact) must
  never contain the raw DSN. Build your `uri` from a sanitized connection identifier — host, port,
  database name only, credentials stripped — the same way `sanitize_dsn_for_display` does in both
  bundled network-backed plugins (`ta-db-proxy-postgres`, `ta-db-proxy-mysql`).
- **`cursor`** (the opaque value your `start_capture` returns, which TA core persists to disk
  alongside goal state) must never contain the raw DSN either, even though it's "opaque" to TA
  core — opaque doesn't mean encrypted, it means TA core won't parse it, but a human with
  filesystem access to goal state can still read it. If your plugin needs the DSN again at
  `stop_capture` time to reconnect, don't round-trip it through `cursor` — `stop_capture`'s wire
  params include a fresh `upstream_dsn`, resolved by the caller from the credential vault for that
  call, exactly like `apply_mutation`'s already does. Put a sanitized identifier in `cursor` if you
  need one for display or URI-building later, never the raw DSN.

This was a real defect caught in supervisor review of the bundled plugins during v0.17.1
development — both shipped with the raw DSN embedded in captured URIs and the persisted cursor
before the fix. Don't repeat it in a third-party plugin.

## Registering your plugin

Drop `plugin.toml` + your executable into `.ta/plugins/db/<name>/` (project-local) or
`~/.config/ta/plugins/db/<name>/` (user-global). Then map a URI scheme to it in
`.ta/db-adapters.toml`:

```toml
[[adapter]]
scheme = "my-db"
plugin = "my-db"
```

`db://my-db/...` URIs now resolve to your plugin — `postgres`/`mysql`/`sqlite` are pre-registered
defaults in the same registry, not special-cased core logic; your entry works identically to
theirs.

## Worked reference

The three bundled plugins are real, complete implementations to read end-to-end:

| Plugin | Path | Capture mechanism |
|---|---|---|
| `sqlite` | `plugins/ta-db-proxy-sqlite/` | Shadow-copy diff (file-based, no server-side resource) |
| `postgres` | `plugins/ta-db-proxy-postgres/` | Logical replication slot, drained once at `stop_capture` |
| `mysql` | `plugins/ta-db-proxy-mysql/` | Recorded binlog position, drained once at `stop_capture` |

Each is a standalone Cargo package (its own `[workspace]`, not a member of TA's workspace) —
structurally identical to what a third party ships, just bundled by default. `cargo build
--release` inside any of them produces the executable `plugin.toml` points at.

## Testing without a real database

You don't need a live server to validate the protocol contract. A shell script that reads one
line and echoes a canned response round-trips through `ExternalDbProxyPlugin` exactly like a real
binary — see `third_party_plugin_round_trips_full_capture_lifecycle` in
`crates/ta-db-proxy/src/external_plugin.rs` for the pattern. Use it to verify your `plugin.toml`
and wire shapes before writing real database logic.
