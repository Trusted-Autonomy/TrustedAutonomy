# Usage metrics: plan usage and budget usage

How TA reports "how much is left" without guessing, and the field contract other tools (Meridian, Wayfinder) can rely on. User-facing how-to lives in `docs/USAGE.md`, section "Usage and budget".

## Two numbers, two names

| Field | Who defines the limit | Where the value comes from | Unknown is |
| --- | --- | --- | --- |
| `plan_usage_pct` | The provider (subscription window) | Provider-reported, via Claude Code's statusline JSON | `null` |
| `budget_used_pct` | The owner (`[budget]` in `daemon.toml`) | Computed by TA from the usage ledger | `null` |

They are never merged into one invented figure. A subscription user cares about the first, an API-key user only has the second, and a user with both wants to see which one is closer to binding. The headline (`headline_pct`, `headline_basis`) is simply the higher of the two, preferring unscoped meters and the shorter window on a tie.

Naming rationale:

- `plan_` says the provider owns the limit. `budget_` says the owner owns it. The prefix tells a reader whether the number can be trusted as authoritative or is derived.
- `_pct` fields are 0 to 100 for `plan_usage_pct` (clamped) and may exceed 100 for `budget_used_pct` (overspend is information, not an error).
- Unknown is `null` in JSON and `n/a` in string-typed exports. It is never `0`, because 0 reads as "plenty left".
- "meter" is the unit that pairs a window with a scope (`session`, `week`, `week` scoped to a model). A plan meter and a budget meter for the same window and scope are merged into one, with `source` set to `claude_code_statusline+ta_ledger`.

Alternatives considered: `plan_window_used_pct` (more exact, longer), `budget_consumed_pct` (symmetrical but awkward next to `used`), `UsageGauge` for `UsageMeter`. The current names were kept because they match the words in `/usage` and in the owner's budget vocabulary, and the type names are internal.

## Data sources

Plan usage comes from one place: the JSON Claude Code passes to the statusline command. `scripts/ta-usage-capture.py` (or `ta usage capture`) keeps `rate_limits`, `rate_limits_available` and `subscription_type`, stamps `captured_at`, and writes `plan-usage.json` atomically. The raw subset is stored and parsed at read time, so parser improvements apply to old snapshots.

Not used, on purpose: OAuth tokens, the macOS Keychain, `~/.claude` credentials, any undocumented usage endpoint. Nothing in TA reads them for this feature.

Staleness: a snapshot older than the max age (default 15 minutes, `[usage] plan_max_age_minutes` or `--max-age-minutes`) or whose `resets_at` has passed is `stale`. Stale plan values stay visible in `meters[]` but never feed `plan_usage_pct` or the headline.

Budget usage comes from the append-only ledger (`<project>/.ta/usage-ledger.jsonl` plus the global roll-up under the user usage dir). Windows are UTC: rolling 5h session, calendar day, Monday-start week, calendar month, or rolling `window_hours`.

## Provider seam

`PlanUsageSource` (in `ta-goal::usage`) returns a `PlanUsageReading` (status, plan type, captured time, parsed meters). `ClaudeCodeStatusline` is the only implementation that reads data; `NoPlanUsage` covers API-key use. Another provider adds an implementation and a capture path; the summary, CLI and exports do not change.

## Ledger coverage

Recorded with tokens and USD: finished goals, published through `publish_completion` (kind `goal`; `golden_replay` when the title marks a replay; `cos_qa` when the goal origin is chat).

Recorded as calls with `tokens_unknown: true`: the daemon shell Q&A path (`QaAgent::ask`, kind `advisor`), because `claude --print` returns no token counts.

Not covered (no token counts available, no hook added): `ask_agent` paths in the daemon, one-shot `claude --print` callers (release, style, constitution, tools), and CoS read-only chat other than goals. `ta usage` surfaces `unknown_token_records` so the gap is visible, not hidden.

## Field contract for Meridian and Wayfinder

Shared shape from `ta usage --json` and the `usage` object of `ta cost report --json`:

```json
{
  "generated_at": "2026-10-21T12:00:00Z",
  "plan_usage_pct": 42.0,
  "budget_used_pct": 17.5,
  "headline_pct": 42.0,
  "headline_basis": "plan",
  "plan": { "status": "fresh", "plan_type": "max", "captured_at": "...", "age_secs": 60, "max_age_minutes": 15, "message": null },
  "meters": [
    { "window": "week", "scope": { "kind": "all", "name": null },
      "plan_usage_pct": 42.0, "budget_used_pct": 17.5,
      "budget": { "usd": 200.0, "tokens": null },
      "used": { "usd": 35.0, "tokens": 1200000, "unknown_token_records": 3 },
      "resets_at": "...", "source": "claude_code_statusline+ta_ledger", "captured_at": "...", "stale": false }
  ],
  "ledger": { "scope": "global", "path": "...", "found": true, "records": 120, "unparsable_lines": 0, "since": null, "by_kind": {}, "by_model": {}, "total": {} },
  "notes": [],
  "hints": []
}
```

Rules for consumers:

- All additions are additive. Old readers that ignore unknown keys keep working. Old stored records (cost reports, golden replay records) load with the new fields defaulted.
- `ta cost report --json --rows` is deliberately unchanged: the Wayfinder cost relay in ta-virtual-team parses a bare array, and wrapping it would break that. Wayfinder should read `ta usage --json`, or the `usage` object in the non-rows report.
- Meridian records (`ta cost report --meridian FILE`) gain string extras `plan_usage_pct`, `budget_used_pct` (two decimals, or `n/a`) and `usage_captured_at`.
- Golden replay records gain `usage` with `before`, `after` (each `plan_usage_pct`, `budget_used_pct`, `plan_captured_at`, `taken_at`), `plan_usage_pct_delta`, `budget_used_pct_delta` and `note`.

## Limits

- A headless `claude --print` run does not refresh the statusline, so a replay launched from automation usually leaves `captured_at` unchanged. The plan delta is then `null` with a note, rather than a fabricated 0.
- The snapshot is only as fresh as the last interactive Claude Code redraw.
- Budget percentages count only what the ledger saw. Paths marked `tokens_unknown` add calls but no cost.
- Per-model budget matching is by case-insensitive substring of the recorded model name.
