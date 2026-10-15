# RTK Optimizer Integration: What It Is and What Remains

Status: adapter built and tested against a fake `rtk`; NOT yet trialled with a real `rtk-oss` (steps 2, 3, 6
and 7 below). D7 is decided: Option A, `rtk-oss` only. Owned by PLAN phase v0.17.12.24.

The adapter is generic. `crates/ta-context-opt/src/hook.rs` defines `HookSpec` (data), `HookInstaller` and
an orphan sweep for any hook-kind optimizer. `rtk-oss` is one `HookSpec` shipped in `Registry::builtin()`
and overridable under `[optimizers.input.rtk]`. The runner (`run.rs::install_input_hook`) has no RTK-specific
code.

Source for everything below: `docs/superpowers/specs/2026-10-13-token-cost-optimization-and-ab-harness.md`
section 5 and decision D7. Where this document quotes a vendor figure, it is the vendor's claim, not a
TA measurement.

## 1. What RTK is

RTK (the `rtk-oss` build) is a single Rust binary that sits between an AI coding agent and the shell.
When the agent runs a shell command (for example `cargo test` or `git log`), RTK's hook rewrites the
command so its output is shortened before it reaches the model. The goal is fewer input tokens for the
same task, because long command output is a large part of what an agent reads.

It fits the **input** slot of TA's three optimizer slots (input, context, output).

What the spec records about it:

| Property | Value |
|---|---|
| Form | One Rust binary plus a hook installed into the agent's config |
| Install for an agent | A `PreToolUse` hook entry in the agent's `settings.json` that runs `rtk hook claude`. `rtk init -g --agent claude` writes it, but into the user's own `~/.claude`, so TA never runs it |
| Where it acts | Bash tool calls only. It does **not** act on Read, Grep or Glob |
| Licence (`rtk-oss`) | Apache-2.0 |
| Licence (full `rtk` and `rtk-proxy`) | Apache-2.0 plus BUSL-1.1 components |
| Vendor's own caveat | Savings dilute: shell output is only one part of what the model reads |

## 2. Why it is not just "turn it on"

Three reasons, found during the v0.17.12.21 review. The first two are resolved by this phase.

1. **TA used to install no RTK hook.** Selecting `rtk` ran the goal with no optimization while the
   metrics record still claimed a result. Now TA writes the hook into the goal's staging copy, proves it
   is active with `rtk hook check`, and if it cannot prove it the goal runs unoptimised and says so.
2. **An unwired arm corrupts a measurement.** An A/B arm using `rtk` would match the `none` arm exactly
   and conclude RTK saves nothing. `ensure_wired` still refuses `rtk` unless the binary, version,
   edition and spec are all in order, so a replay arm can no longer be silently inactive.
3. **The figure that matters is a whole-goal token saving.** RTK only touches Bash output, so the
   saving on a real goal is smaller than the per-command figures in section 4. That stays unmeasured
   until a real RTK-assisted goal is replayed against `none` (step 6).

## 3. Decision D7: licence (DECIDED, Option A)

The full RTK edition is Apache-2.0 plus BUSL-1.1. BUSL-1.1 is free for production use but not for
offering the software as a commercial service. Decide before TA or Untollable bundles or recommends
either edition:

- **Option A (recommended):** use `rtk-oss` only (Apache-2.0). No licence question for TA or Untollable.
- **Option B:** use the full edition for its extra engine. Needs a licence review, because Untollable or
  a hosted TA offering could count as "a commercial service".

**Decided: Option A, `rtk-oss` only (Apache-2.0).** TA detects the edition and refuses the full edition unless `allow_busl = true` is set for a local experiment (recorded in the goal's record). TA never downloads or installs RTK; it only detects and configures. Do not bundle RTK in any release.

## 4. Steps (phase v0.17.12.24, item 1)

Each step has a check that must pass before the next.

1. **Decide D7**: DONE (section 3). The spec's decisions list is outside this repo; record it there.
2. **Install `rtk-oss` on one machine**, outside TA: DONE. `rtk-oss` 1.0.1 (Apache-2.0) installed into a throwaway directory with an isolated `HOME`.
3. **Prove the hook works by hand** in a scratch repo: DONE. `rtk hook check "git status"` prints `rtk git status` (exit 0); `rtk hook check "echo hi"` prints `No rewrite for: echo hi` (exit 1); piping a Bash tool call into `rtk hook claude` returns an `updatedInput` rewrite. See "What was proven against the real binary" below.
4. **Write the adapter**: DONE and proven against the real binary. TA writes the single documented hook entry into the goal's staging `.claude/settings.json` itself (`SettingsHook`), merged so existing hooks and keys survive, idempotent, refusing a settings file that is not JSON rather than overwriting it. It runs `rtk hook check "git status"` to verify, with `HOME` pointed at a scratch directory so the user's real config is unreachable. Cleanup restores the declared `touches` paths byte for byte. A manifest under `.ta/compression/installs/` lets the next start sweep an install left by a crashed or killed TA. Version, edition, licence and `allow_busl` go in `.ta/compression/hook-<goal-id>.json`. A test runs the real `rtk-oss` binary through the installer: active, edition `Oss`, staging restored byte-identical, user home untouched.
5. **`ensure_wired` accepts `rtk`** only when the spec is present, the binary is found, the version meets the minimum and the edition is acceptable: DONE and tested (flips from refusing to accepting). `lossless` is computed from real before/after text (`measure_pair`) rather than a constant: DONE. Limitation: a hook runs inside the agent, so TA never sees the before/after text of a goal; `measure_pair` is available to whatever captures both, and the goal record carries version, edition and activity. Real token numbers come from `ta cost report`.
6. **Trial through replay against `none`** on a real golden entry: NOT DONE. `ta golden replay <id> --set optimizers.input.use=rtk` goes through the same install path as config selection, but a real agent goal has not yet been replayed with RTK active, so there is no end to end token delta from a goal. It needs a real RTK-assisted goal run, which costs money, so it waits for the owner.
7. **Record the measured result**: PARTLY DONE. Byte savings of the command output, measured with the real binary in this repo, are in the table below. These are output sizes only. The token saving of a whole goal is not measured (step 6), because shell output is only part of what the model reads.

### What was proven against the real binary (rtk-oss 1.0.1, 2026-10)

- `rtk --version` prints `rtk x.y.z`. The edition is NOT printed by `--version`. The Apache-2.0 edition omits the `pipeline`, `methods` and `tasks` subcommands, so TA reads `rtk --help` and treats any of those words as the full edition. If none appears and `init` does appear, it is `oss`; otherwise unknown, and refused.
- The agent name for Claude Code is `claude`. `--agent claude-code` is rejected. Valid values: claude, cursor, trae, windsurf, cline, kilocode, antigravity, kimi, pi, hermes, droid, vibe, omp.
- A project-scope `rtk init --agent claude --auto-patch` writes only `CLAUDE.md` and `.rtk/filters.toml` and reports "hooks: registered globally": it does NOT install the hook. `rtk init -g --agent claude --auto-patch` writes `~/.claude/settings.json`, `RTK.md` and `CLAUDE.md`, which is the user's home. So TA does not run `rtk init` at all and writes the one documented entry itself, then verifies it with `rtk hook check`.
- The hook entry is `{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}`.

| Command run in this repo | Bytes before | Bytes after | Saved |
|---|---|---|---|
| `git log -n 40` | 120591 | 8934 | 93% |
| `ls -la crates` | 2325 | 700 | 70% |
| `git status` | 100 | 49 | 51% |
| `git diff HEAD~3 --stat` | unchanged | unchanged | 0% |

## 5. Related remaining work in the same phase

- **Windows no-window smoke run (item 2):** needs a real Windows build and the manual check in
  `docs/RELEASING.md`.
- **First-party minimizers (item 3):** `cargo`, `git` and `ta` output minimizers that call `apply`, to
  replay against RTK and `none`. Nothing in the repo calls `apply` outside tests today.
- **Failure history across restart (item 4):** the supervisor deletes `.ta/compression/status.json` at
  startup, hiding why an optimizer was disabled.

## 6. What "done" looks like

`ta golden replay <id> --set optimizers.input.use=rtk` runs a real RTK-assisted goal, `ta golden report`
shows a measured delta against `none`, the user's agent config is untouched afterwards, and the licence
decision is recorded. Today: everything is done and tested except steps 6 and 7 as a whole-goal measurement, which need a real RTK-assisted goal run.
