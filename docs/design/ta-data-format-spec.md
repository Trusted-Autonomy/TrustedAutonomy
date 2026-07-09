# TA Data-Format Spec

**Status**: canonical reference, published alongside `crates/ta-data-spec` (v0.17.0.12.21), 2026-07-09.
**Purpose**: the versioned JSON Schema contract for TA's five core wire types — the interface boundary that lets Studio, community trigger-configs, and community plugins build against TA without compiling Rust.
**Source of the underlying idea**: [`ta-concepts-and-architecture.md` §13.1](ta-concepts-and-architecture.md#131-repo-organization--monorepo--library-planes--data-format-specs-2026-07-05) ("Data format specs are the real interface boundary, not repo splits").
**Companion doc**: [`ta-action-reference.md`](ta-action-reference.md) — this doc is scoped to *data shapes*; that one is scoped to *actions*.

---

## Why this exists

TA stays a single Cargo workspace (§13.1) rather than splitting into per-tier repos — a split would add cross-repo schema drift and version-pinning friction without a real payoff (solo user, single release train). Instead, the boundary is enforced at the **data** level: five Rust types that already drive TA's core logic are also published as versioned JSON Schema. Anything that needs to interoperate with TA — Studio's JS, a community `.ta/triggers/<type>.toml` author, a community plugin — builds against the published schema, not against `ta-*` crate internals.

The schemas are generated directly from the real, already-`serde`-annotated Rust types via [`schemars`](https://docs.rs/schemars) (`crates/ta-data-spec`) — not a hand-maintained mirror that can silently drift from what actually gets serialized on the wire.

## The five specs

| Spec | Rust type | Crate | Schema file |
|---|---|---|---|
| `Goal` | `GoalRun` | `ta-goal` | `schema/goal.schema.json` |
| `Draft` | `DraftPackage` | `ta-changeset` | `schema/draft.schema.json` |
| `Artifact` | `Artifact` | `ta-changeset` | `schema/artifact.schema.json` |
| `TriggerEvent` | `TriggerEvent` | `ta-intake` | `schema/trigger_event.schema.json` |
| `RoutingDecision` | `RoutingDecision` | `ta-brain` | `schema/routing_decision.schema.json` |
| `Persona` | `PersonaConfig` | `ta-goal` | `schema/persona.schema.json` |

Each schema file carries:
- `$id` — a stable, versioned URL (`https://trustedautonomy.dev/schema/<name>.v<version>.schema.json`).
- `x-ta-schema-version` — the explicit version number (see below).

`crates/ta-data-spec/src/lib.rs`'s `SPECS` constant is the single source of truth for the name/version/file mapping — everything above is derived from it, not maintained by hand in two places.

## Versioning

Each spec has its own `version: u32`, independent of the workspace/crate semver — a schema and the binary that happens to ship it change on different cadences. Bump a spec's version in `ta_data_spec::SPECS` when its shape changes in a way that isn't purely additive/backward-compatible (i.e. an old serialized example would no longer deserialize). Purely additive changes (a new optional field with a serde default) don't require a version bump.

## Regenerating the schemas

```bash
cargo run -p ta-data-spec --bin gen-schema
```

Regenerates every file under `schema/` from the current Rust types. `ta-data-spec`'s `tests/schema_sync.rs` fails `cargo test --workspace` (and therefore CI) if a checked-in schema file is out of sync with what the current types would generate — run the command above and commit the result whenever one of the five types changes.

## Backward-compatibility guarantee

`ta-data-spec`'s `tests/round_trip.rs` keeps one frozen, hand-written JSON example per spec type. If a future change to a type breaks deserialization of that example — a required field renamed or removed without a compatible default — the test fails CI. This is the "a schema change that breaks an existing serialized example fails CI" guarantee.

## The Studio boundary rule

> Studio is a separately-deployable add-on against the daemon's HTTP/SSE API (served today as plain HTML/JS from `crates/ta-daemon/assets/`). **It may never special-case internal Rust types — only the versioned spec above.**

In practice, since Studio itself is JS (it can't import a Rust type even by accident), the rule is enforced one layer down, at `ta-daemon`'s own API response types (`crates/ta-daemon/src/api/*.rs`, `crates/ta-daemon/src/web.rs`):

- **Prefer a purpose-built response DTO** (e.g. `ActiveGoalSummary`, `PersonaApiEntry`, `DraftSummary`) over serializing an internal type directly. This is already the norm across the daemon's API handlers.
- **A response may embed one of the five spec types directly** (e.g. `GET /api/drafts/:id` returns the `Draft` spec type via `#[serde(flatten)]`) *only* when it also carries an explicit `schema_version` field naming which version of that spec it conforms to (via `ta_data_spec::version_of("<name>")`). Direct exposure without a version marker is exactly the "special case" this rule forbids — a later change to the internal struct would silently change the wire shape with no signal to consumers.
- Internal types that **aren't** one of the five specs (`GoalRunState`, `GoalRunStore`, etc.) may still be imported and used for handler *logic* — the rule is about what crosses the wire in a `Json<...>` response, not about what a handler is allowed to import.

**Enforcement**: `crates/ta-data-spec/tests/studio_boundary.rs` statically scans `ta-daemon`'s API response struct definitions for a spec type embedded without a sibling `schema_version` field, and fails `cargo test --workspace` if it finds one. It's a conservative text-based scan (not full Rust type-checking) — it won't catch every possible obfuscation, but it catches the straightforward case new code is most likely to introduce.

## See also

- [`ta-action-reference.md`](ta-action-reference.md) — the action/verb model these data types flow through.
- [`ta-concepts-and-architecture.md` §13.1](ta-concepts-and-architecture.md) — the design reasoning behind staying monorepo and using data-format specs as the real boundary.
