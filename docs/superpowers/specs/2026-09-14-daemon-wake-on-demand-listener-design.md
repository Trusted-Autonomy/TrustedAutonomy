# Daemon-Hosted Wake-On-Demand Listener (v0.17.11.10)

**Status**: proposed, 2026-09-14. Resolves a gap found red-teaming `ta-virtual-team`'s Wayfinder-dispatch design doc (§3-§10): chief-of-staff's cadence for consuming the external-intake stream was implicitly bound to `team_session.rs`'s fixed round-robin rotation, unreconciled with that design's own "a few seconds feels live enough" latency goal. This is very plausibly the same underlying problem the now-superseded §11 (daemon-native chat/meeting-notes intake, `ta-virtual-team` PR #4) was reaching for — the difference here is a **generic, key-routed** capability serving any topic/source, not a bespoke path per intake type.

## Grounding

- **`team_session.rs`'s real driver** (`run_team_session`, `crates/ta-daemon/src/team_session.rs:627-683`) is a fixed round-robin: `current_stage_index % state.stages.len()`, advancing immediately (`sleep_secs = 0`) on success. A role's turn comes once per full rotation through every other role — there is no "wake this specific role now" path today.
- **The daemon already owns a shared, long-lived `WhiteboardTransport` instance** (`AppState.whiteboard_transport`, v0.17.11.8) — the same justification applies here: the daemon is the one genuinely long-lived process per project, unlike any individual agent's throwaway `ta run` subprocess. A watcher for wake-up events belongs where v0.17.11.8 already put shared coordination state, not in a private repo's own process.
- **`ta-agent-whiteboard`'s transport already has what a watcher needs**: `stream_append`/`stream_read_next`/`stream_ack` for durable message delivery, `kv_put`/`kv_list` with TTL for registration records — the exact primitives `presence.rs` and the planned `topics.rs`/`registration.rs` (`ta-virtual-team`'s Phase 3) already build on. No `ta-agent-whiteboard` changes needed.
- **§9.5 of the Wayfinder-dispatch design doc left "can chief-of-staff invocations for different candidates run concurrently" as an open, unresolved question** — a daemon-owned, single-flight-by-construction trigger resolves this as a side effect (see Concurrency below), not as separate follow-up work.

## Scope decision (per direction given)

Build the generic mechanism now; wire up exactly one listener (chief-of-staff) now. Do not build a second listener, a UI, or cross-listener coordination speculatively — extend when a second real use case (e.g. a security-engineer urgent-review listener) actually shows up.

## Design

### 1. `WakeListener` registration (new, `ta-daemon`)

```rust
pub struct WakeListenerRegistration {
    pub role: TeamRole,           // which .ta/team.toml role to launch
    pub keys: Vec<String>,        // routing keys this role wakes for (e.g. "external-intake")
    pub team_session_id: String,  // which team session owns this registration
}
```

Stored via the daemon's existing `WhiteboardTransport` KV primitives (`kv_put`/`kv_list`, TTL-refreshed), mirroring `presence.rs`'s existing liveness pattern exactly — not a new storage mechanism. A registration is **not** a subscription to one hardcoded stream; `keys` is a list specifically so a future second listener (or CoS listening to more than one key later) doesn't need a schema change — this is the concrete extensibility point the "design to scale" direction asked for. For now: exactly one registration exists, `{ role: chief-of-staff, keys: ["external-intake"] }`.

### 2. Daemon-resident watcher (new, `ta-daemon`)

A background task, started once at daemon startup alongside the existing shared `WhiteboardTransport` init (v0.17.11.8) — this **is** the "always-on" property: the watcher itself is always running as part of the daemon's own lifecycle, even though each triggered execution is still an ordinary bounded `ta run` subprocess (reusing existing goal machinery — no new agent-execution mechanism, same principle the Wayfinder-dispatch design already established for chief-of-staff's own invocation).

For each registered key with a live (non-expired) listener registration: watch the corresponding stream for new messages. On a new message:

1. **Single-flight check**: is a goal already running for this `(role, team_session_id)` pair? Reuses `ta-goal`'s existing `GoalRunState` tracking (`Running` vs. terminal states) — not a new PID-tracking mechanism. If yes, do nothing — the message stays durable and unacknowledged on the stream; the already-running invocation (or the next one) will pick it up via `stream_read_next`.
2. If not already running: launch `ta run` for that role. **Implementation note**: `build_ta_run_args` is written for the rotation's per-stage-index context and isn't a drop-in call here — this needs a small extraction of its reusable "launch this role's goal" core into a function callable outside the rotation state machine, not a literal reuse of the function as-is. The context/payload (the triggering message) is injected the same way rotation stages already receive context today (`render_session_context`'s existing mechanism) — no new context-passing mechanism, just a new caller of it.
3. **After the invocation completes**: re-check stream depth for that key. If nonzero (new messages arrived mid-run), launch again immediately. If zero, go back to watching. This directly resolves §9.5's open concurrency question — a listener role can never have two invocations in flight for the same key, by construction, without needing a separate coordination mechanism.

### 3. Relationship to `team_session.rs`'s round-robin

A role registered as a wake-on-demand listener is **excluded from the ordinary rotation** — it would be redundant and wasteful to both wake it on demand and give it a fixed round-robin turn. Rather than overload `TeamSessionStageConfig` with an opt-out flag (conflating "is a rotation stage" with "isn't one"), add a **separate, sibling list** to the session config: `#[serde(default)] wake_on_demand_listeners: Vec<WakeListenerConfig>` (session-level, alongside `stages`, not embedded inside a stage entry). `stages` stays exactly what it is today — pure rotation, unchanged, zero ambiguity about what's in the round-robin. Empty/absent `wake_on_demand_listeners` preserves today's behavior exactly (backward-compatible, same additive-field pattern as `model_tier`/`handles_tags`, v0.17.11.6).

### 4. What `ta-virtual-team`'s Phase 3-5 still owns — and still needs building

Unchanged by this design: the external-intake stream itself, and the `candidate`/`outcome` wire schema (§8.5), still live in the private repo, built on `ta-agent-whiteboard`'s public transport — this design doesn't move that. What moves to `ta-daemon`: *who decides when to launch chief-of-staff's process* — previously implicit/unresolved (bound to rotation), now an explicit, generic, daemon-owned mechanism. `ta-virtual-team`'s Phase 5 ("wire intake end to end") now has a real answer for "how does chief-of-staff actually wake up," which it didn't before.

**This spec assumes messages already arrive on the registered stream key — it does not build the thing that puts them there.** That's Phase 4/5's poller (Wayfinder REST client → `candidate` translation → stream publish), still 100% unbuilt as of 2026-09-14. Confirmed with the Wayfinder-side session (which shipped its own half, PR #163, `triage-intake` tasks with embedded triage instructions): the full chain is **Wayfinder task → poller (missing) → external-intake stream → this wake-listener → chief-of-staff**. Two of those three pieces exist; the middle one — the actual poller — doesn't yet. Don't treat this spec as closing that gap; it only closes "how does the daemon react once something is on the stream."

**Integration detail for whoever builds that poller** (flagged by the Wayfinder-side session, worth deciding now): Wayfinder's `triage-intake` task descriptions (PR #163) already embed the full "You are the chief-of-staff... triage this..." instructions verbatim — Wayfinder had no way to know the daemon would separately inject its own framing via `render_session_context`. If the poller forwards Wayfinder's whole task `description` field as `candidate.description`, chief-of-staff would see the triage instructions twice (once from Wayfinder's task, once from the daemon's own context-building). The poller should extract just the raw content (the chat text / meeting notes) for the candidate message, not Wayfinder's instructional wrapper — daemon-side context-building should be the single source of "here's how to triage this."

## Explicitly out of scope

- A second wake-on-demand listener (e.g. security-engineer) — the mechanism supports it (multi-key `WakeListenerRegistration`, per-role single-flight), but nothing wires one up until a real use case exists.
- Any cross-listener coordination or priority between multiple simultaneously-registered listeners — not needed with exactly one.
- A UI for registrations — `kv_list` is enough to inspect state for now, same as `presence.rs`'s existing bar.
- Changing `topics.rs`/`registration.rs`'s ownership or the `candidate`/`outcome` schema — unchanged, still `ta-virtual-team`'s Phase 3/§8.5.

## Tests

- Registration round-trips through the daemon's KV store, TTL-expires like `presence.rs`'s existing pattern.
- Single-flight: two messages published in quick succession before the first invocation completes produce exactly one `ta run` launch, then a second launch after completion (not a launch per message).
- A role listed in `wake_on_demand_listeners` never appears in `stages` and is never touched by `stage_index`'s rotation — construction-level test on a session with one rotation role + one wake-on-demand listener, confirming `run_one_cycle` only ever advances through the rotation role.
- Backward compatibility: an existing `team.toml`/session config with no `wake_on_demand_listeners` field behaves identically to today (existing `team_session.rs` test suite must still pass unchanged).
