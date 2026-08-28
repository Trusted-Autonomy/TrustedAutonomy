# Staged-Resource Conflict Detection — v0.17.11.7

> Design note, 2026-08-28. Scopes the phase discussed and red-teamed the same day: "resource (VCS, DB, files) integration into the whiteboard to reduce conflicts... agents advertising which resources they plan to modify... detect when a resource has been modified through our change staging to know a conflict exists ahead of time." This resurfaces item 7 from the original `agent-coordination-whiteboard.md` design doc, explicitly deferred out of v1 scope at the time.

## 1. What already exists (don't rebuild)

Two resource-identity systems exist today and don't talk to each other:

- **`ta-agent-whiteboard`'s presence/discovery** (`presence.rs`/`discovery.rs`): agents self-declare a `resources: Vec<String>` glob-pattern list on launch (`task-graph`'s existing `api_impact` vocabulary). `is_anyone_touching()` glob-matches a query against currently-live presence records. Explicitly, by its own doc comment: **advisory information, not enforcement** — "this answers 'is anyone touching this,' it does not block anyone from doing so." This is *declared intent*, ephemeral (TTL'd), and can be wrong, stale, or incomplete.
- **`ta-changeset`'s artifact/patchset model** (`draft_package.rs`): every `Artifact` carries a `resource_uri: String` (`"fs://workspace/src/main.rs"` — confirmed the scheme already generalizes beyond files, e.g. `"mcp://gmail/send"` appears in the same file's docs). This is *actual, evidentiary* record of what a staged-but-not-yet-applied draft touched — durable for the life of the draft, not ephemeral like presence.

`ta-agent-whiteboard` has **zero dependency on `ta-changeset` today** (confirmed via `Cargo.toml`). Building this feature means introducing that coupling deliberately, not discovering it's already half-built.

## 2. Scope for v0.17.11.7 (deliberately narrower than the original ask)

**In scope:**
- A new query — "does anyone have a *staged* (drafted, not-yet-applied) change touching any of these resource URIs" — answered against `ta-changeset`'s draft store, not against whiteboard presence. This is strictly higher-signal than `is_anyone_touching`: it reflects what was *actually* touched, not what was *declared*.
- `fs://` resources only. This is where both systems already overlap and where real collisions actually happen today (the whole reason `is_anyone_touching` and the v0.17.10.2 bug both exist).
- Stays **advisory-only**, matching the whiteboard's existing v1 philosophy. It answers a question; it does not block a goal from launching or a draft from being built.

**Explicitly out of scope, deferred:**
- **DB resources** (`db://` scheme) — real, separate work tied to `ta-db-proxy`'s own resource model (table/key-range identity), not a trivial URI-prefix addition. Needs its own design pass.
- **VCS as a distinct resource domain** — red-teamed and rejected: a file's VCS identity *is* its file path. `fs://` already covers it; inventing a parallel `git://` scheme would just create two names for the same thing.
- **Enforcement / blocking** — whether a detected conflict should serialize goals via `task-graph`'s wave scheduler (the architecturally "right" answer per the red-team discussion) is real, cross-repo work (`task-graph` is a separate OSS repo, `Trusted-Autonomy/task-graph`, consumed via git dependency). Not attempted in this phase. This phase produces the *signal*; deciding what consumes it and whether it gates anything is a follow-up.

## 3. Design

New function in `ta-agent-whiteboard` (module TBD at implementation time — likely a new `staged_conflicts.rs` sibling to `discovery.rs`, since it queries a genuinely different data source than presence and shouldn't be silently folded into `discovery.rs`'s existing "presence snapshot" framing):

```rust
pub struct StagedConflict {
    pub resource_uri: String,
    pub draft_id: String,
    pub goal_run_id: String,
}

pub fn staged_conflicts_for(
    drafts: &dyn DraftLookup,       // small trait over ta-changeset's draft store — see §4
    resource_uris: &[String],
) -> Result<Vec<StagedConflict>>
```

Glob-matched the same way `is_anyone_touching` already does (reuse that matching logic, don't reinvent it), against `resource_uri` instead of the presence-declared glob list. Pure function, easily unit-tested against an in-memory fixture of drafts — no NATS/JetStream involvement, since this reads staging state, not the whiteboard transport.

## 4. The coupling question

`ta-agent-whiteboard` gaining a dependency on `ta-changeset` is a real, deliberate architectural decision, not a detail. Two ways to do it:

- **(a) Direct dependency**: `ta-agent-whiteboard` depends on `ta-changeset` directly, calls its draft-store query APIs. Simplest, but couples a "coordination substrate" crate to a "changeset/staging" crate — two things that were previously independent by design.
- **(b) Trait-based inversion**: `ta-agent-whiteboard` defines a small `DraftLookup` trait (get pending drafts + their artifact resource_uris); the *caller* (daemon, CLI) supplies a `ta-changeset`-backed implementation. `ta-agent-whiteboard` itself never depends on `ta-changeset`.

**Recommendation: (b).** It's a few more lines, but it keeps `ta-agent-whiteboard` genuinely reusable as a coordination primitive independent of TA's specific staging implementation (the same reasoning that already justifies `WhiteboardTransport` being a trait rather than a hardcoded NATS client) — and it avoids a real risk: `ta-changeset` is a much larger, more actively-changing crate than `ta-agent-whiteboard`; a direct dependency means every `ta-changeset` change is a potential (if usually silent) `ta-agent-whiteboard` recompile/review surface.

## 5. Consumers (not built in this phase, noted for the record)

Once `staged_conflicts_for` exists, it's callable from:
- The existing advisory pre-launch check in `ta_goal_start` (`ta-mcp-gateway/src/whiteboard_check.rs`) — add staged-conflict results alongside the existing presence-based advisory info.
- A future `task-graph` wave-scheduler integration (deferred, cross-repo, not this phase).

## 6. Testing

Pure-function unit tests against fixture data (no live daemon/NATS needed): overlapping `fs://` URI is detected; non-overlapping is not; empty draft store returns empty; glob-vs-exact-path matching in both directions (mirroring the existing `is_anyone_touching` test suite's own coverage of that same edge case).
