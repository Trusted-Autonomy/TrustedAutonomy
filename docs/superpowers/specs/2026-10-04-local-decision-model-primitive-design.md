# Local Decision-Model Primitive — Design

**Goal:** One reusable TA-core crate that lets any caller ask a narrow, bounded question and get
back a calibrated decision — fast and cheap — replacing ad-hoc per-project reimplementations of
the same pattern that has already independently emerged five times across two products.

**Architecture:** A thin, backend-pluggable primitive (`ask(question, context, schema) -> {result,
confidence}`), not a pipeline and not a model-hosting service. Pre-filtering (BM25/regex),
arbitration logic, and any holistic judgment stay as *calling patterns* built on top of this
primitive in each caller — the crate itself stays narrow.

**Tech stack:** New Rust crate in TrustedAutonomy, alongside `ta-runtime`/`ta-credentials`/
`ta-decision` — consumed via path dependency by `untollable-core` (the same pattern Untollable
already uses for those two crates) and from within TA-core itself (`ta-workflow`'s reviewer nodes,
`task-graph`).

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
| **task-graph semantic overlap** | "Do tasks X and Y overlap, per the brain?" — a detection *signal* fed into existing deterministic ordering logic, not an LLM making the ordering decision itself | Provisional — pending confirmation this doesn't cut against task-graph's own stated anti-holistic-ordering design stance |

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

Backends: `LocalOllamaBackend` (talks to any Ollama-compatible local server), a cloud backend for
the "Jev" option if ever wanted as a fallback/comparison point, and a `FixtureBackend` for
deterministic tests. Which local model(s) back `LocalOllamaBackend` by default is governed by the
evaluation doc, not fixed here — as of this writing, two real candidates (Intern-Decision-4B,
Decider-4b) are undergoing direct head-to-head comparison on this hardware before either becomes
the default (see that doc's §3/§7).

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

## 4. task-graph (provisional)

"Semantic overlap against the brain, with or without CoS" is a detection signal feeding task-graph's
existing deterministic ordering logic — a different claim than task-graph's README argues against
(an LLM making the ordering decision itself holistically). Treated as provisional pending
`trustedautonomy-46`'s confirmation this doesn't cut against their own stated design stance for
that crate — not assumed resolved by this document.

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

See `2026-10-04-local-decision-model-evaluation.md` for the full, continuously-updated model
landscape and hardware-tier recommendations. Summary as of this writing: two real, verified,
purpose-built, Apache-2.0 candidates (Intern-Decision-4B, Decider-4b) are undergoing a direct
head-to-head comparison on this machine (same labeled set, same hardware).

**Provisional default while that comparison runs: Intern-Decision-4B.** Because
`DecisionBackend` is pluggable (§2), committing to a provisional default costs nothing — it's
swappable with no caller-visible change once the head-to-head concludes. Shipping with no default
at all has a real cost (nothing to point callers at today), so this design picks one now rather
than waiting on a comparison that's already in flight, not an open-ended one. Intern-Decision-4B
is the provisional pick specifically because its calibration evidence (Brier score, ECE) is
self-reported by its own creator but real and measured; Decider-4b's cited benchmark edge came
from a third-party hobby leaderboard whose own later version reversed the ranking — weaker
grounds to default to it ahead of the real comparison's results.

## 7. Where it lives / who builds what

New crate in TrustedAutonomy (working name `ta-decision-model` — deliberately distinct from the
existing `ta-decision`/`decision-gate` pure-routing crate so the two are never confused).
`trustedautonomy-46` builds the crate and wires it into `ta-workflow`'s reviewer chain; the TA
agent also wires it into Untollable (path-dependency, same pattern as `ta-runtime`). Wayfinder's
own CoS classifier consumes it only through the TA-backed adapter, never directly — VT-agnosticism
means a non-TA VT backend simply wouldn't have this capability, and that's an acceptable, deliberate
gap, not an oversight.

## 8. Open questions

- task-graph's semantic-overlap use case (§4) — pending `trustedautonomy-46` confirmation.
- Which model becomes the default local backend — pending the head-to-head comparison
  (`2026-10-04-local-decision-model-evaluation.md` §3/§7).
- Exact crate name (`ta-decision-model` is a working name only).
- Whether a cloud "Jev" backend is worth building at all, or whether this stays local-only —
  not yet discussed explicitly.
