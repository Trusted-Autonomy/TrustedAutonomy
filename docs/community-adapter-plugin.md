# Building a Trusted Autonomy Domain-Action Adapter Plugin

This guide explains how to make a new external side effect available to agents via
`ta_external_action` (v0.17.5.3) — placing a trade, filing a support ticket, triggering a CI
pipeline, anything an agent might need to do outside TA itself. Today `ta_external_action`
ships four built-in stub types (`email`, `social_post`, `api_call`, `db_query`) that only
validate payloads — they don't perform real I/O. A domain-action adapter plugin is how you (or
a community author) add a new, *real*, executable action type without a TA core change or
recompile: an external executable plus a `plugin.toml` manifest, dropped into
`.ta/plugins/adapter/<name>/`.

This is the **Plugin** category from `docs/USAGE.md` → "Authoring a Plugin" — call/response
over newline-delimited JSON on stdin/stdout, discovered by convention, same shared
`ta-plugin` transport/manifest/discovery crate every other kind (VCS, release, db, ...) uses.
If you haven't read that section yet, start there for the general manifest schema; this doc
covers only what's specific to `adapter`-kind plugins.

## Why a plugin, not a TA core action type

`email`/`social_post`/`api_call`/`db_query` are built into `ta-actions` because they're
close to universal — nearly every project eventually wants one of them. A brokerage trade,
a support-desk ticket, a game-build trigger — these are project-specific, and often need a
proprietary SDK or credential TA's own binary has no business bundling. Pushing them out to a
plugin means your integration ships and iterates on its own schedule, independent of TA's
release cadence — the same reasoning `docs/community-release-plugin.md` gives for Steam/App
Store release adapters, applied to the action-dispatch side instead of the release side.

## Declaring the verb(s) you handle

Unlike other Plugin-category kinds, `adapter` plugins don't get a fixed set of methods per
verb — one plugin can handle several verbs (`"trade.execute"`, `"trade.cancel"`, ...), and
new verbs never require a TA core change. Declare each one as a `capabilities` entry prefixed
`verb:` — the same prefix-convention idiom the `release` kind already uses for
`"channel:<name>"` custom channel declarations:

```toml
capabilities = ["verb:trade.execute", "verb:trade.cancel"]
```

`ta_external_action` discovers every `verb:`-prefixed capability across every registered
adapter plugin and adds it to the registry alongside the four built-in types — call
`ta_external_action` with `action_type: "trade.execute"` and TA routes it to your plugin
exactly like it would route `action_type: "email"` to the built-in email stub. Discovery only
reads `plugin.toml` files; it never spawns your process just to build the list of registered
verbs, so an unrelated `ta_external_action` call (e.g. `action_type: "email"`) never pays your
plugin's startup cost.

## The two methods, over the wire

Your plugin's `plugin.toml` has `type = "adapter"`. Every call is one `{"method","params"}`
JSON line in, one `{"ok":true,"result":{...}}` or `{"ok":false,"error":"..."}` JSON line out —
fresh process per call, no persistent handshake required (unlike `release`/`vcs` plugins,
`adapter` plugins are called directly; a malformed or unimplemented method simply returns a
clear `{"ok":false,"error":...}`, which surfaces to the caller as an actionable error).

| Method | Required? | Params | Result |
|---|---|---|---|
| `risk_score` | Yes | `{"verb": "<verb>", "payload": {...}}` | `{"risk_score": 0-100, "confidence": 0.0-1.0}` |
| `execute` | Yes | `{"verb": "<verb>", "payload": {...}}` | Arbitrary JSON — whatever your action's real outcome looks like |

Both methods receive the same `verb`/`payload` shape — `verb` tells you which of your
declared verbs this call is for (useful if one plugin handles several), `payload` is exactly
what the agent passed to `ta_external_action`'s `payload` field.

### `risk_score`

```
→ {"method":"risk_score","params":{"verb":"trade.execute","payload":{"symbol":"ACME","amount":500}}}
← {"ok":true,"result":{"risk_score":25,"confidence":0.9}}
```

This is the method that makes the score *real*. Compute it from the actual payload — trade
notional size, ticket severity, blast radius, whatever "risk" means for your domain — never
return a hardcoded value. (`ta-submit::social_adapter.rs`'s `publish()` hardcodes `risk_score:
0` today; that's the anti-pattern this framework exists to avoid for new adapters.)
`risk_score` (0-100, higher is riskier) and `confidence` (0.0-1.0, how much you trust your own
score) feed directly into `ta_decision::gate::decide()` — the same gate `ta_human_verify` and
code-draft approval use — so a well-calibrated score is what actually lets safe actions
auto-execute and risky ones get held for a human.

### `execute`

```
→ {"method":"execute","params":{"verb":"trade.execute","payload":{"symbol":"ACME","side":"buy","amount":500}}}
← {"ok":true,"result":{"status":"filled","symbol":"ACME","fill_price":18.0,"quantity":27.7778}}
```

Only called after the gate below has already decided to commit — your `execute` method never
needs to re-check risk itself, and never needs to implement a review/hold path; TA already did
that before calling you. `result` is returned to the agent verbatim as `ActionOutcome::Executed
{ result }`.

## The gating pipeline your plugin participates in

`ta_external_action` doesn't call your `execute` method directly off the back of
`risk_score` — it runs the full v0.17.5.3 approval pipeline first:

1. **Budget guardrail** (if the caller supplied one) — a hard per-action or total-budget limit
   rejects the action before your `risk_score` method is even called; a soft threshold crossing
   forces the eventual decision to `Escalate` regardless of what your score says.
2. **Risk gate** — your `risk_score` result becomes a `ta_decision::DecisionInput` (with
   `verdict: Pass`), scored against `DecisionThresholds` (configurable per-verb in
   `.ta/workflow.toml`'s `[human_verify.<verb>]` table, the same config surface
   `ta_human_verify` uses) via the shared `decide()` gate — one uniform `Commit`/`Reject`/
   `Rework`/`Escalate` contract, regardless of domain.
3. **Security tier** — the calling goal's `security_tier` (resolved from the most recent
   `ta-brain::RoutingDecision`) can only *downgrade* a `Commit` to `Escalate`, never upgrade a
   worse verdict. An inferred or non-autonomous workload never gets to auto-execute your
   action, no matter how low-risk your plugin scored it.
4. Only a surviving `Commit` calls your `execute` method. Everything else is captured for
   human review — it appears in `ta draft view` under Pending Actions, with the gate's
   decision and your risk score attached, never silently dropped.

You don't implement any of this — it's the same pipeline for every registered verb from every
plugin. Your job is just an honest `risk_score` and a correct `execute`.

## Registering your plugin

Drop `plugin.toml` + your executable into `.ta/plugins/adapter/<name>/` (project-local) or
`~/.config/ta/plugins/adapter/<name>/` (user-global):

```toml
# .ta/plugins/adapter/paper-trading/plugin.toml
name = "paper-trading"
version = "0.1.0"
type = "adapter"
command = "python3"
args = ["paper_trading_adapter.py"]
capabilities = ["verb:trade.execute"]
description = "Paper-trading only, no live brokerage API"
timeout_secs = 30
```

Call `ta_external_action` with `action_type: "trade.execute"` and TA finds your plugin. An
unregistered verb — no plugin declares a matching `verb:` capability — returns a clear
`unknown action type '...' — no adapter registered for this verb` error, never a silent no-op.

## Testing without a live external dependency

You don't need a real brokerage/ticketing/CI API to validate the protocol contract or exercise
the gating pipeline. `plugins/adapter/paper-trading/` in this repo is TA's own reference
implementation — a small Python script that computes a real (non-hardcoded) risk score from
trade notional size and simulates a fill, no network call anywhere. It's what
`templates/workflows/trading-desk.yaml`'s analyst/strategist/trader persistent team session
(v0.17.5.1) dispatches to, and what TA's own test suite (`crates/ta-actions/src/plugin_action.rs`,
`crates/ta-mcp-gateway/src/tools/action.rs`) round-trips through both the auto-execute and
captured-for-review paths. Copy its shape — read one JSON line, dispatch on `method`, write one
JSON line back — as the starting point for your own adapter before wiring up a real API.
