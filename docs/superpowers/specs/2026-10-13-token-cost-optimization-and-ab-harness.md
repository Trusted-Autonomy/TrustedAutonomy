# Token Cost Reporting, Golden-Set Replay and Swappable Optimizers

**Status:** revised 2026-10-13 after owner direction. Supersedes the earlier draft of this file (a 4-arm A/B grid with
headroom). Owner decisions applied: build S1 and S2 now; no large explicit multi-arm testing; capture a goal start
state plus prompt and replay it against candidate optimizations on a small golden set; send `ta cost report` into
Meridian; use existing open-source optimizers first (headroom allowed if it never pops shell windows) behind a
swappable abstraction for input, context and output, replacing them only with something measurably better.

## 1. What the data says (measured)

Two workloads with opposite cost shapes. Both matter.

### 1.1 Long interactive sessions (Claude Code or OMP, days long)

Source: Claude Code session `98a823ac`, 2026-10-07 to 2026-10-13, 18 MB transcript, deduplicated by message id.
Streamed rows repeat a message id (1,565 of 2,832 rows), so a naive sum overcounts by about 2.5x.

| Token class | Tokens | Share of list-price cost (Sonnet 5.5 rates, 1h cache writes) |
|---|---|---|
| Cache read | 603.0 M | 63.8% ($180.90) |
| Cache write (1h) | 14.6 M | 30.9% ($87.60) |
| Output | 0.99 M | 5.3% ($14.92) |
| Fresh input | 0.003 M | 0.0% |

Total about $283 at list price. 1,259 turns; average context **491k tokens**, median 470k, max 967k; 94% of cache-read
cost is in turns above 200k context; average output only 790 tokens per turn.
Claude Code's default auto-compact window is about 967k tokens, which is why one session reached that size.

**Driver: context length times turn count.** A token added at turn t of a session of N turns is billed about N minus t
times at the cache-read rate. Output optimization (even the Hashline claim of 61% fewer output tokens) saves about 3.2%.

### 1.2 Fresh-context goals (`ta run`, one task each)

Source: `.ta/velocity-history.jsonl`, 47 priced goals (45 table-estimated, 2 runtime-reported).

| Measure | Value |
|---|---|
| Median goal cost, 47 priced history rows | $0.76 (mostly small goals) |
| 90th percentile / max of those rows | $2.24 / $3.57 |
| Runtime-reported (exact) goals, n=3 | $0.45, $2.81, $9.82 (mean $4.36) |
| Largest, v0.17.12.5 phase goal | $9.82: 11.1 M cache-read, 0.22 M cache-write, 89.6k output |
| Mix for that goal at list rates | 60% cache read, 15% cache write, 24% output |

Cautions. Only 47 of 317 history rows carry tokens, 45 of those are table-estimated, and phase-sized goals cost far more
than the median row. The provisional Sonnet 5.5 rows are wrong in **both directions**: they estimated $5.66 against an
actual $2.81 for one goal (over) and $5.51 against an actual $9.82 for another (under). So dollar figures from the
table are not trustworthy to better than about 2x either way; token counts are the reliable measure, and
runtime-reported cost should be preferred. The earlier claim that output is about 89% of goal cost came from the
table-estimated rows and does not hold for the phase-sized goal, where cache read dominates.


### 1.3 Consequence

- The cost driver in both workloads is **context length times turn count**, not output. A phase goal reads 11.1 M
  cache tokens in one run, so it grows a large context inside a single `ta run`; the lever there is turns per goal
  and context size within the goal (smaller tool results, read supersession, a per-turn context ceiling), not only
  a fresh start.
- Interactive sessions: cap context (compaction window), reduce turns, keep tool results small.
- Small fresh-context tasks (the median row, and what Untollable mostly runs): avoid unnecessary model calls and
  keep the per-task prefix small and cacheable. Output discipline is a secondary lever (24% of the large goal).
- The workloads differ in scale, so reporting must show them side by side (section 3).

## 2. Done already (S0 and S1)

- **Status bars.** Claude Code: `~/.claude/statusline.py` registered as `statusLine`. Sums unique assistant messages
  (last row per id wins), incremental offset cache, prices 1h cache writes separately. Token totals match an independent
  sum exactly. OMP: `statusLine.rightSegments` = `session_name, token_in, token_out, cache_read, cost, context_pct`.
- **Compaction ceiling.** Claude Code `autoCompactWindow` = 200000 in `~/.claude/settings.json`. OMP
  `compaction.thresholdTokens` = 200000 (session-only until the owner confirms). OMP `supersedeReads`, `dropUseless`
  and `shellMinimizer` were already on.
- **Modelled effect of a 200k cap** on the measured session: about 66% lower cost, 11 compactions across the week.
  This is an upper bound on direction only: the model assumes compaction loses no information, which is exactly what
  the golden set (section 4) exists to test.
- **Known pricing gap.** `token_cost.rs` has one cache-write rate; Claude transcripts split 5-minute and 1-hour
  writes (`cache_creation.ephemeral_1h_input_tokens`). The 1h rate is 2x input. Under-pricing on the measured session:
  about 11.6%.

## 3. S2: `ta cost report`

One command that answers "where is the money going" across all workloads.

**Sources** (each optional, auto-detected):

1. TA goals: `.ta/goals/*.json` and `.ta/velocity-history.jsonl`, via the existing `ta_goal::spend::aggregate`.
2. Claude Code sessions: `~/.claude/projects/**/*.jsonl`.
3. OMP sessions: `~/.omp/agent/sessions/**/*.jsonl` (per-message `usage.cost.total` already computed by OMP).

**Rules that matter for correctness:**

- Deduplicate Claude assistant rows by `message.id`, last row wins. Skip `model == "<synthetic>"`.
- Price with the project `PriceTable` (`.ta/pricing.toml` override) and add a `cache_write_1h` rate. Mark a cost
  `exact` when the source reports it (OMP, runtime goals) and `estimated` otherwise. Never invent a price for an
  unknown model (existing `token_cost.rs` rule).
- Report **context-size buckets** (0 to 50k, 50 to 100k, 100 to 200k, 200 to 400k, over 400k): turns, cache-read tokens
  and cost per bucket. This is the table that exposed the interactive-session finding and it must be one command away.

**Output:** a terminal table; `--json`; `--group-by day|source|model|project|context_bucket|origin_class`;
`--since`/`--until` (reuse `parse_time_bound`); `--project`.

**Meridian export:** `ta cost report --meridian <file.jsonl>` writes one record per session or goal in Meridian's
generic JSONL shape (`id`, `title`, `timestamp`, `tokens_input`, `tokens_output`, `source`, `phase`, plus string
metadata `cost_usd`, `cost_exact`, `model`, `cache_read_tokens`, `cache_write_tokens`, `context_peak`, `workload`).
Meridian's generic loader keeps string-valued extras as metadata and uses `tokens_input` and `tokens_output` for effort.

**Meridian prerequisites found while checking (real defects, verified on data):**

1. Meridian's `claude_code` ingester sums every assistant row without deduplicating by message id. On the measured
   session that is **2.49x input and 2.69x output**. Fix: dedupe by `message.id`, last row wins.
2. It excludes `cache_read_input_tokens` ("distorts cost signal"). For interactive sessions that is 64% of cost, so
   Meridian's effort number cannot represent spend. Fix: carry cache-read as separate metadata and let the report
   choose, or add a cost-weighted effort unit.
3. `ta experiment` and `GoalRun.experiment_*` already exist (live-traffic arms via config overrides). This work reuses
   `experiment_id`/`experiment_arm` to tag replays rather than adding a parallel mechanism.

## 4. Golden set: capture a goal start state and replay it

Goal: test an optimization on identical work without re-living it, and without the cost of a multi-arm grid.

### 4.1 What to capture (per golden entry)

`GoalRun` today records `objective`, `source_dir`, `plan_phase` and a `source_snapshot` of per-file content hashes
(used for conflict detection). It does **not** record the git commit, so a goal cannot be reproduced. Add to `GoalRun`
(additive, serde default, so old records still load):

- `start_commit`: full git SHA of `HEAD` at goal start (absent when the project is not a git repo)
- `start_dirty`: bool, true when the working tree had uncommitted changes (such a goal is not exactly replayable)
- `start_branch`: branch name
- `prompt`: the exact objective text sent (already `objective`; also store the objective *file* contents when
  `--objective-file` was used, since the file may later change)
- `agent_model`, `agent_id` (already present), and the effective config overrides (`experiment_overrides`)

A golden entry is a stored file under `.ta/golden/<id>/`:

```
.ta/golden/<id>/entry.json   { id, title, start_commit, start_dirty, prompt_sha256, source_goal_id,
                               check_command?, baseline: { cost_usd, tokens{in,out,cache_read,cache_write},
                               turns, outcome, files_changed, diff_sha256, captured_at } }
.ta/golden/<id>/prompt.md    the exact prompt
.ta/golden/<id>/baseline.diff the diff the original run produced
```

### 4.2 Commands

- `ta golden capture <goal-id> [--check "<cmd>"]` turns a completed goal into an entry. Refuses entries with
  `start_dirty` unless `--allow-dirty`. Records the baseline from the goal's actual usage and diff.
- `ta golden list` shows entries with baseline cost and age.
- `ta golden replay <id> [--set key=value ...] [--tag <label>]`:
  1. makes a clean checkout of `start_commit` in a scratch worktree (never touches the user's tree),
  2. launches a normal `ta run` against it with the stored prompt, tagging the goal `experiment_id = golden:<id>`,
     `experiment_arm = <tag>`, and applying `--set` overrides through the existing `experiment_overrides` path,
  3. on completion compares to baseline: cost, tokens, turns, outcome, whether `check_command` passes, and a
     normalized diff comparison (files touched, line counts, diff similarity to `baseline.diff`).
- `ta golden report [<id>...]` prints replay results against baselines, one row per replay, with cost delta and an
  explicit **equivalence verdict**: `equivalent` (check passes and diff similarity above a threshold), `differs`
  (needs human review), or `failed`. Anything not `equivalent` is listed for human review, never auto-accepted.

### 4.3 Data gathering without a grid

- Pick 6 to 10 golden entries from real past goals, covering small bug fix, feature from a PLAN.md phase, read-heavy
  investigation, docs edit, and one multi-file change. Prefer goals that already have an objective file.
- For each candidate optimization run **one replay** against its baseline (not 3 reps of 4 arms). Look at the cost
  delta and the equivalence verdict; only repeat a replay when the result is borderline.
- Interactive-session optimizations (compaction window, prefix trim) cannot be replayed as a `ta run` goal. Test those
  with a transcript-based estimate (the section 2 model) and then by running the new setting for a week and watching
  the status bar and `ta cost report`.

## 5. Optimizer abstraction: three swappable slots, OSS first

**Owner direction (supersedes the earlier "no headroom" draft):** start with existing open-source optimizers, behind an
abstraction so each can be swapped for another OSS tool or a first-party one without touching callers. Headroom is
acceptable **provided it does not repeatedly pop up shell windows**.

### 5.1 Three slots, one contract

| Slot | What it acts on | Trait | Starter (OSS) | First-party fallback |
|---|---|---|---|---|
| Input | tool and command output, files, logs before they enter context | `InputOptimizer` | RTK (`rtk-oss`, Apache-2.0) for command output; headroom for payload compression | `ta-context-opt` minimizers |
| Context | the accumulated conversation (compaction, supersession, eviction, prefix) | `ContextOptimizer` | the agent's own compaction (`autoCompactWindow`, OMP `compaction.*`) | `ta-context-opt` window and eviction policy |
| Output | what the model writes (terse replies, edit format) | `OutputOptimizer` | Caveman-style terse-output instruction; hashline or `apply_patch` edit format | first-party prompt fragment and edit tool |

Contract for every implementation (so the golden set can compare any of them):

- `name`, `version`, `kind` (`process`, `proxy`, `hook`, `in_process`), `licence` (recorded, shown in `ta compression status`)
- `available()`: cheap probe, never starts a visible window
- `apply(input) -> output` plus **a metrics record** `{bytes_in, bytes_out, est_tokens_in, est_tokens_out, ms, lossless: bool, original_ref}`
- `original_ref` points at the full unmodified content on disk so any reduction is reversible and auditable
- selected per slot in config (`[optimizers.input] use = "rtk"`), overridable per replay (`--set optimizers.input.use=none`)
- `none` is always a valid choice and is the baseline for every comparison

The existing `[compression.plugin]` and `PromptOptimizerPluginConfig` (command, args, `proxy_base_url`, `health_endpoint`,
env) already is the `proxy` kind for the input slot. The work is to generalize it, not to replace it.

### 5.2 Candidates found (verified from their own pages, not measured by us)

| Candidate | Slot | Form | Licence | Notes |
|---|---|---|---|---|
| RTK `rtk-oss` | input | single Rust binary, hook rewrites shell commands | Apache-2.0 | Has an OMP and a Claude Code integration (`rtk init -g --agent omp`). Only fires on Bash tool calls, not Read/Grep/Glob. States itself that savings dilute: bash output is one part of input. The full `rtk` and `rtk-proxy` add BUSL-1.1 components (free for production, not for resale as a service). |
| headroom | input | Python HTTP proxy on `ANTHROPIC_BASE_URL`, Rust core | Apache-2.0 | Payload-level compression; TA already integrates it (v0.17.0.7). Not installed on this machine. Needs the window fix in 5.3. |
| Caveman | output | skill or plugin, plus an optional local proxy | see repo | Vendor headline 65 to 75% fewer output tokens; its own paired benchmark reports 8.5% fewer output tokens over 82 tasks with no detected quality difference. Use the measured figure. |
| hashline, `apply_patch` | output | edit format | n/a | A 24.4% patch-failure rate was reported for Haiku with hashline and 0% for Sonnet, so edit format is model-dependent and must be replayed per model. |
| Agent-native compaction | context | built in | n/a | Already configured (S1). |

Selection rule: an OSS candidate becomes the default for its slot only when golden-set replays show a token or cost
reduction and an `equivalent` verdict. Otherwise the slot stays `none`. A first-party implementation replaces it only
when it measurably beats it.

### 5.3 Headroom and shell windows (verified gap)

`crates/ta-daemon/src/prompt_optimizer_supervisor.rs` spawns the plugin with `tokio::process::Command` and no
window-suppression flag, and `CREATE_NO_WINDOW` does not appear anywhere in the repository. On Windows a console
child process opens a visible window, and the supervisor restarts a failing plugin with backoff (up to 5 failures in
5 minutes), so a flapping optimizer could flash windows repeatedly. Required before enabling any process-kind optimizer
on Windows:

1. spawn every optimizer with `CREATE_NO_WINDOW` (and `DETACHED_PROCESS` for daemon-kind) via
   `std::os::windows::process::CommandExt::creation_flags`, behind `#[cfg(windows)]`;
2. a test that asserts the flag is set on the constructed command (cannot assert absence of a window in CI, so assert
   the flag, and add a manual Windows smoke check to the release checklist);
3. a flap guard: after the existing `SUSPEND_FAILURE_COUNT` the optimizer is disabled for the session with one
   user-visible message, never silently restarted forever;
4. `ta compression status` shows restart count and last failure.

On macOS and Linux no window exists, so the gap is Windows-only. This machine is macOS, so the fix needs a Windows CI
or hands-on check before it is called done.

### 5.4 Where first-party code lives

A small crate `ta-context-opt` holding the traits, the registry, the metrics record, the `none` implementation, the
`original_ref` store, and first-party minimizers for what we actually produce: `cargo build/test/clippy` (collapse
repeated warnings, keep failures and the summary), `git status/diff/log`, `ta` output, large JSON, plus read
supersession and stale-result eviction for loops we control. Consumed by TA's headless path and by Untollable, which
owns its own agent loop and so can apply all three slots, including output interruption (OMP-style stream rules).

## 6. Untollable's role (owner direction)

Untollable is the owner's daily driver and a replacement for OMP and Claude Code: it relays chat to the CoS, and
executes real work as TA goals with TA as the selected agent. It therefore consumes the optimizer slots directly and is
the place where all three slots can be enforced end to end. Its separate effort estimate is in
`untollable-cli/docs/superpowers/specs/2026-10-13-daily-driver-cost-design-and-effort.md`. Its shell must reach OMP and
Claude Code parity from the start; task T0 (evaluate the TUI shell, 3 to 4 days) gates all Untollable build work, so
the optimizer integration in S5 for Untollable waits on T0's decision.

## 7. Phases

| Phase | Work | Effort (dev-days) |
|---|---|---|
| S0 | Status bars | done |
| S1 | Compaction window on both tools; levers already on | done (OMP value session-only until confirmed) |
| S2a | Goal start state capture (`start_commit`, `start_dirty`, `start_branch`, objective-file contents) | 0.5 to 1 |
| S2b | `ta cost report`: goals + Claude + OMP, dedupe, context buckets, `--json`, 1h cache-write rate | 2 to 3 |
| S2c | `ta cost report --meridian` export + Meridian ingester fixes (dedupe, cache-read) | 1 to 2 |
| S3 | Golden set: `ta golden capture/list/replay/report` | 3 to 4 |
| S4 | Capture 6 to 10 golden entries; baseline them | 0.5 to 1 (plus owner review) |
| S5a | `ta-context-opt` crate: traits, registry, metrics, `none`, `original_ref`; adapt headroom to it | 2 to 3 |
| S5b | Windows no-window spawn fix, flap guard, status output (5.3) | 1 to 2 |
| S5c | RTK adapter (input slot), trial via replay against `none` | 1 to 2 |
| S5d | First-party minimizers for cargo, git, ta output; replay against RTK | 3 to 5 |
| S6 | Output slot candidates (terse instruction, edit format) replayed per model | 1 to 2 |

Total about 15 to 25 dev-days; S2a to S3 (about 6.5 to 10) is the instrument, S5 and S6 are what it measures.

## 8. Risks and limits

- One replay is a single sample of a stochastic agent. A borderline cost delta or `differs` verdict needs a second
  replay or a human look. The report must show this and not present a single run as proof.
- Replaying with `start_dirty` goals is not exact. They are refused by default.
- Prompt cache state differs between the original run and a replay (the original may have been warm). Compare token
  counts and turns as well as dollars, and use a unique prefix nonce per replay so replays do not warm each other.
- A replay costs real money. `ta golden replay` must print an estimate and respect `--max-usd`.
- List price is not the bill. The report is a consistent comparator; check real billing on a sample.
- Quality loss from compaction or minimizing is the real risk. The equivalence verdict and human review are the control.
- Provisional Claude 5 rates remain unconfirmed. Until the owner supplies rates or `.ta/pricing.toml`, dollar figures are
  estimates and token counts are the reliable measure.

## 9. Decisions

- **D1 (resolved):** build S1 and S2 now. Done for S1.
- **D2 (resolved):** no large multi-arm grid; golden-set replay with single runs plus human review of borderline cases.
- **D3 (resolved, owner):** headroom and other OSS optimizers are fine as starters behind the section 5 abstraction.
  Headroom only after the Windows no-window fix (5.3). RTK (`rtk-oss`) is the first input candidate to trial.
- **D7 (open):** RTK full edition is Apache-2.0 plus BUSL-1.1 (free for production, not for offering as a commercial
  service). Use `rtk-oss` unless you want the engine; confirm before TA or Untollable bundles either.
- **D4 (open):** confirm OMP `compaction.thresholdTokens = 200000` should be saved.
- **D5 (open):** the Meridian ingester fixes live in the Meridian repo and change its numbers for past data; approve.
- **D6 (open):** Claude 5 price rows and the 1h cache-write rate (supply real rates or `.ta/pricing.toml`).
