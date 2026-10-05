# Local Decision-Model Primitive — Design

**Goal:** One reusable TA-core crate that lets any caller ask a narrow, bounded question and get
back a calibrated decision — fast and cheap — replacing ad-hoc per-project reimplementations of
the same pattern that has already independently emerged five times across two products.

**Architecture:** A thin, backend-pluggable primitive (`ask(question, context, schema) -> {result,
confidence}`), not a pipeline and not a model-hosting service. Pre-filtering (BM25/regex),
arbitration logic, and any holistic judgment stay as *calling patterns* built on top of this
primitive in each caller — the crate itself stays narrow.

**Tech stack:** New Rust crate in TrustedAutonomy, working name `ta-ask` (not `ta-decision-model`
— see §7 for why), alongside `ta-runtime`/`ta-credentials`/`ta-decision` — consumed via path
dependency by `untollable-core` (the same pattern Untollable already uses for those two crates)
and from within TA-core itself (`ta-workflow`'s reviewer nodes, and whatever assembles the
`WaveNode` list consumed by `task-graph` — never `task-graph` itself, see §4).

## Global Constraints

- The crate's own interface stays narrow: one bounded question + context in, one calibrated
  result out. It does not own pre-filtering, arbitration sequencing, or model hosting/download —
  those are caller-side or deployment-side concerns.
- Backend-pluggable from day one (`DecisionBackend` trait) — no caller depends on a specific
  model. This mirrors the "best-in-class, swappable" posture used elsewhere in this ecosystem
  (Wayfinder/VT-agnosticism is the same shape applied to a different boundary).
- Every `DecisionResponse` carries `model_id` — which backend actually answered — so production
  usage is directly comparable across model choices, not just offline eval.
- Arbitration (resolving disagreement among multiple votes/signals) is built as a *composition* of
  narrow calls through this same primitive, never as one larger holistic call. See §3.
- See `2026-10-04-local-decision-model-evaluation.md` (same `docs/superpowers/specs/` directory)
  for the model landscape, hardware-tier recommendations, and ongoing head-to-head comparison —
  that document is a living reference, updated independently of this one.

---

## 1. Why this is one crate, not five one-offs

Five independent places already want the exact same shape — question + bounded context in,
calibrated `{result, confidence}` out:

| Call site | What it asks | Backs |
|---|---|---|
| **consensus-panel reviewer** | "Does this diff look safe to commit?" → one more `ReviewerVote` | TA-core, already has a slot (`advisor_confidence_reviewer.rs`) |
| **decision-gate input** | Same shape, feeds `DecisionInput{verdict, risk_score, confidence}` into the existing deterministic `decide()` router | TA-core (currently unwired in production — an empty slot, not a live call to displace) |
| **Wayfinder CoS classifier** | "Does this message need real dispatch or is it read-only chat?" | `wayfinder/docs/superpowers/specs/2026-10-03-multi-project-vt-agnostic-coordination-design.md` §8 |
| **Untollable chat-vs-governed classifier** | Same question, different front end (terminal prompt vs. team-session role) — **Untollable's own spec already states this should share implementation, not just design, with the Wayfinder one** | `untollable-cli/docs/superpowers/specs/2026-10-03-agent-cli-shell-product-design.md` §3 |
| **semantic overlap, feeding task-graph** | "Do tasks X and Y overlap, per the brain?" — computed by the caller assembling `WaveNode`s (`ta-workflow`), which augments `impact_tags` *before* calling `plan_waves()`. `task-graph` itself never depends on or knows about this crate. See §4. | Confirmed by `trustedautonomy-46` — real risk avoided (would have broken task-graph's own "no model in the loop" identity claim as a standalone public crate) |

Building this once, in TA-core, and consuming it everywhere avoids five divergent
reimplementations of the same calibration/backend-selection/eval logic — and two of the five
(Wayfinder's classifier and Untollable's) were independently specified, by different design
processes, as needing to share one real implementation.

## 2. The interface

```rust
pub struct DecisionRequest {
    pub question: String,
    pub context: String,           // bounded — caller's job to keep it narrow, not the crate's
    pub schema: DecisionSchema,    // YesNo | Choice(Vec<String>) | Score { min: f64, max: f64 }
}

pub struct DecisionResponse {
    pub result: DecisionResult,    // matches schema: bool | chosen label | numeric score
    pub confidence: f64,           // calibrated 0.0-1.0
    pub model_id: String,          // which backend actually answered — required for eval/telemetry
    pub latency_ms: u64,
}

#[async_trait]
pub trait DecisionBackend {
    async fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecisionError>;
}
```

Backends: a native-process backend that shells out to or embeds the chosen model's own inference
package (not an `LocalOllamaBackend` — the real head-to-head run found neither candidate is in
Ollama's official library, and the actual validated path for the resolved default, Decider-4b, is
its own native Python package (`decider.infer.Decider`), not an Ollama-mediated one; naming and
exact process boundary TBD when this gets built), a cloud backend for the "Jev" option if ever
wanted as a fallback/comparison point, and a `FixtureBackend` for deterministic tests.
**Default local backend: Decider-4b**, resolved empirically via a real run on target hardware, not
a spec-sheet pick — see `2026-10-04-local-decision-model-evaluation.md` §3 for the full
methodology and numbers.

## 3. Arbitration is composition, not a bigger call

An arbiter resolving disagreement among multiple reviewer votes/signals is a sequence of narrow
`DecisionRequest` calls composed by deterministic logic, never a single call that looks at
everything at once and renders one holistic verdict. Rationale: `cinepipe-sage`'s own
`f5_cheap_signal` experiment already found that decomposing a big multi-clause judge into one
narrow question was measurably more stable (4/4 vs. 2/4 across repeated runs) than embedding the
same check in a holistic judge call. A "look at all the votes and evidence, render one verdict"
arbiter is architecturally closer to that less-stable holistic-judge shape than to the narrow
pattern already validated — so arbitration should stay built from the same narrow primitive, used
in a loop with deterministic combination logic, not elevated into a different, bigger kind of call.

**Confirmed in Decider-4b's own real source (2026-10-04)**, not assumed: its `decide()` call
already takes a *list* of narrow questions against one shared context and answers all of them in
one real forward pass — "all from one forward pass," per its own module docstring. An arbitration
sequence that needs several narrow questions about the same state (not several different states)
can issue them as one batched call to the backend instead of N separate round trips, at the
`DecisionBackend` implementation level — `DecisionRequest`/`DecisionResponse` stay single-question
at the crate's public interface (§2), but a backend is free to batch multiple in-flight requests
against it under the hood. This is an implementation optimization, not a change to the interface
or to §3's "narrow questions, not one holistic call" principle.

## 4. Semantic overlap, and why task-graph never depends on this crate

Resolved by `trustedautonomy-46`'s red-team (2026-10-04), confirmed directly against
`task-graph`'s real README and `Cargo.toml`: "semantic overlap against the brain" is a detection
signal feeding task-graph's existing deterministic ordering logic (Kahn's-sort + tag-overlap), not
an LLM making the ordering decision itself holistically — narrowly accurate, matching task-graph's
own stated objection (an LLM deciding ordering, which is non-deterministic, costly per-dispatch,
and silently wrong), which is specifically about the ordering decision, not about every signal that
ever feeds it.

But two real gaps that accurate framing alone doesn't close, both fixed by placement rather than
by task-graph changing at all:

- **task-graph is a standalone public crate** — `thiserror` is its only required dependency;
  `serde`/`serde_json` are optional, feature-gated. "Without a model in the loop" is its whole
  identity claim to anyone depending on it outside TA. If this crate became a task-graph
  dependency — even an optional feature — that claim stops being true for every standalone
  consumer, not just TA's own usage.
- **Determinism**: if the overlap detector is re-invoked fresh on every planning run, "same graph,
  same answer, every time" breaks one layer up from the ordering algorithm itself — the exact
  "silently wrong" failure mode task-graph's README warns about, just relocated rather than
  avoided.

**Resolution: task-graph gets zero new dependencies and zero awareness of this crate.** The
caller that assembles the `WaveNode` list consumed by `plan_waves()` (`ta-workflow`, or whatever
owns that assembly) uses this crate to compute and cache semantic-overlap verdicts, keyed on the
task-description pair, and writes the result into `impact_tags` *before* calling into task-graph.
Re-planning the identical graph with identical task text returns the identical cached verdict —
task-graph never sees, and never needs to see, whether a tag came from a human or a model.
Ship this gated behind an explicit opt-in on the caller side, not a default-on behavior.

## 5. Non-functional targets

Carried from Untollable's own spec (§3) rather than re-derived, since both products need the same
numbers:

| Metric | Target |
|---|---|
| Heuristic-only pre-filter (caller-side, not this crate) | p50 < 50ms, $0 |
| This crate's model-backed `decide()` call | p50 < 400ms, ≤50 output tokens |
| System-prompt cache hit rate | >95% |
| End-to-end simple classification | <2s |

## 6. Evaluation

See `2026-10-04-local-decision-model-evaluation.md` for the full methodology, numbers, and
hardware-tier recommendations — this section is a summary, not the source of truth.

**Default backend: Decider-4b, resolved empirically (2026-10-04), not by benchmark claims from
either model's own camp.** A real head-to-head ran on this machine (M1 Max, MPS): both candidates
scored 20/20 accuracy and 10/10 stability on the same labeled set. Decider-4b measured 310.7ms p50
latency (passes Untollable's <400ms target); Intern-Decision-4B measured 1371.1ms p50 (fails it by
~3.4x), because it has no MPS-optimized kernels and no path to get them (its speed-critical ops are
CUDA/Triton-only) while Decider-4b ships dedicated ones. `DecisionBackend`'s pluggability (§2)
means this remains swappable if CPU-tier or future-hardware results change the picture — this
isn't a permanent commitment, just the best-grounded choice given what's actually been measured.

## 7. Where it lives / who builds what

New crate in TrustedAutonomy, named **`ta-ask`**. Renamed from the original working name
`ta-decision-model` per `trustedautonomy-46`'s red-team: `ta-decision` is already a real,
in-use Rust identifier (`ta-workflow/Cargo.toml:33`'s dependency alias for `decision-gate`,
an explicitly *no*-model-in-the-loop crate) — `ta-decision-model` differed by one suffix word
while meaning something architecturally near-opposite. `ta-ask` is self-documenting against the
actual public API (`ask(question, context, schema) -> {result, confidence}`) and has zero
collision risk. (`ta-oracle`/`ta-judge` were considered as backups if `ask` reads too generic.)

`trustedautonomy-46` builds the crate and wires it into `ta-workflow`'s reviewer chain (and into
whichever component assembles `WaveNode`s for `task-graph`, per §4 — never into `task-graph`
itself); the TA agent also wires it into Untollable (path-dependency, same pattern as
`ta-runtime`). Wayfinder's own CoS classifier consumes it only through the TA-backed adapter,
never directly — VT-agnosticism means a non-TA VT backend simply wouldn't have this capability,
and that's an acceptable, deliberate gap, not an oversight.

## 8. Open questions

- Exact crate name is resolved (`ta-ask`, §7); nothing else open on naming.
- task-graph's semantic-overlap placement is resolved (§4) — caller-side augmentation only, zero
  task-graph dependency.
- **Default local backend: resolved empirically, 2026-10-04.** A real head-to-head ran on this
  machine (M1 Max, MPS): both Intern-Decision-4B and Decider-4b scored 20/20 accuracy and 10/10
  stability on the shared `f5_cheap_signal` labeled set — but **Decider-4b measured 342.9ms mean /
  310.7ms p50 latency, passing Untollable's <400ms p50 target; Intern-Decision-4B measured
  1619.2ms mean / 1371.1ms p50, failing it by ~3.4x.** Not incidental: `transformers` itself
  logged `causal_conv1d_fn falling back to reference PyTorch implementation` and the same for
  `chunk_gated_delta_rule` (needs Triton, no real Apple GPU path) — Intern-Decision-4B has no
  MPS-optimized kernels and no path to get them, while Decider-4b's dedicated `mps_ops.py`/
  `mps_moe.py` patches are real and working. **Default backend is now Decider-4b**, reversing the
  earlier provisional pick (§6) — full numbers and methodology in
  `2026-10-04-local-decision-model-evaluation.md` §8.
- Still genuinely open: CPU-only latency (neither model benchmarked), and full Brier/ECE
  recomputation at the deployed quantization level (the real head-to-head measured accuracy/
  stability on a 10-case set, not a full calibration-metric recomputation).
- Whether a cloud "Jev" backend is worth building at all, or whether this stays local-only —
  not yet discussed explicitly.
