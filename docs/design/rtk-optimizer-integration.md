# RTK Optimizer Integration: What It Is and What Remains

Status: not integrated. `rtk` is registered in the optimizer registry (`crates/ta-context-opt`) so its
licence is on record, but selecting it is refused. Owned by PLAN phase v0.17.12.24.

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
| Install for an agent | `rtk init -g --agent omp` (OMP and Claude Code integrations exist) |
| Where it acts | Bash tool calls only. It does **not** act on Read, Grep or Glob |
| Licence (`rtk-oss`) | Apache-2.0 |
| Licence (full `rtk` and `rtk-proxy`) | Apache-2.0 plus BUSL-1.1 components |
| Vendor's own caveat | Savings dilute: shell output is only one part of what the model reads |

## 2. Why it is not just "turn it on"

Three reasons, all found during the v0.17.12.21 review:

1. **TA installs no RTK hook.** Nothing in TA writes RTK's hook into an agent's config, so selecting
   `rtk` ran the goal with no optimization. The metrics record would still have claimed a result.
2. **That would corrupt the measurement.** An A/B arm using `rtk` would match the `none` arm exactly and
   conclude that RTK saves nothing, when RTK was never active. So TA now refuses `rtk` in `ta golden
   replay --set optimizers.input.use=rtk`, in config selection, and in `apply`.
3. **The figure that matters is unmeasured.** Because it only touches Bash output, RTK's real saving on
   TA's own workloads is unknown until it is measured on real goals.

## 3. Open decision for the owner (D7): licence

The full RTK edition is Apache-2.0 plus BUSL-1.1. BUSL-1.1 is free for production use but not for
offering the software as a commercial service. Decide before TA or Untollable bundles or recommends
either edition:

- **Option A (recommended):** use `rtk-oss` only (Apache-2.0). No licence question for TA or Untollable.
- **Option B:** use the full edition for its extra engine. Needs a licence review, because Untollable or
  a hosted TA offering could count as "a commercial service".

Do not bundle RTK in any release before this is decided. Trialling it locally through replay is fine.

## 4. Steps (phase v0.17.12.24, item 1)

Each step has a check that must pass before the next.

1. **Decide D7** (section 3). Check: the choice is written in the spec's decisions list.
2. **Install `rtk-oss` on one machine**, outside TA. Check: `rtk --version` runs; record the version.
3. **Prove the hook works by hand** in a scratch repo: `rtk init -g --agent <the agent TA runs>`, run an
   agent turn that calls a noisy command, and confirm the command output the model saw was shortened.
   Check: you can show before and after output for one command. If this fails, stop; the rest depends on it.
4. **Write the adapter** (input slot, hook kind). It must: install the hook only for the lifetime of one
   goal and only in that goal's staging copy; remove it afterwards even when the goal fails or is killed;
   record the installed RTK version and licence in the metrics record. Check: after a goal, the user's own
   agent config is byte-identical to before.
5. **Make `ensure_wired` accept `rtk`** only once step 4 passes, and add a test that a goal run through the
   adapter produces a metrics record with `lossless` set from real output, not a constant.
6. **Trial through replay against `none`** on a real golden entry: `ta golden replay <id> --set
   optimizers.input.use=rtk` and the same with `none`, then `ta golden report`. Check: both arms completed,
   and you have a measured token and cost difference, however small.
7. **Record the measured result** in the spec (section 5) in place of any vendor figure. Decide from the
   measurement whether RTK is worth keeping as a default.

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
decision is recorded.
