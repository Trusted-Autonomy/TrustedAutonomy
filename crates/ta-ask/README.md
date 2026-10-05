# ta-ask

A narrow, bounded-question decision primitive: `ask(question, context, schema) -> {result, confidence}`.

**Status: private, in-tree only, as of 2026-10-04.** Unlike `decision-gate`, `consensus-panel`, and `task-graph`, this crate has NOT been extracted to a standalone public repo. Don't assume it has been, and don't extract it without explicit direction — it's deliberately staying internal for now.

## Why this exists

Five independent places across TA-core, Wayfinder, and Untollable converged on the same shape (bounded question + context in, calibrated result out) rather than five divergent reimplementations. See `docs/superpowers/specs/2026-10-04-local-decision-model-primitive-design.md` and `docs/superpowers/specs/2026-10-04-local-decision-model-evaluation.md` (both on branch `docs/local-decision-model-primitive-design` as of this writing) for the full design and model evaluation.

## Interface is synchronous, not async

`DecisionBackend::decide` is a plain sync function, not `async fn`. This was a deliberate correction from the design doc's original sketch: `ta-workflow`'s `ReviewerNode::review()` (the first real consumer, see below) is sync with no async anywhere in its graph engine, and `reqwest`'s `blocking` feature is already enabled workspace-wide — making this sync means zero runtime-bridging code at the integration point. A future async caller can wrap a sync `ask()` call in `tokio::task::spawn_blocking`, which is the standard, safe direction for calling blocking code from async Rust (the reverse — sync code calling async — is the fragile direction).

## Backends

- `FixtureBackend`/`ErrorFixtureBackend` — deterministic, for tests. No network, no subprocess.
- `DeciderBackend` — the real backend, wrapping Decider-4b (Mapika, Apache 2.0, github.com/Mapika/decider). Spawns and keeps warm Decider's own HTTP server (`scripts/serve.sh <model> <port>`) rather than a custom stdio protocol — the model takes 13-21s to load, so a long-lived process is required; Decider ships `/health` and `/decide` endpoints specifically for this kind of integration.

### Running `DeciderBackend` for real

Install dependencies (note: **two extras are required** — `[metal]` for Apple Silicon acceleration and `[serve]` for the FastAPI/uvicorn HTTP server):

```bash
python3 -m venv /tmp/ta-ask-decider-env
source /tmp/ta-ask-decider-env/bin/activate
pip install 'decider-ai[metal,serve]'   # [metal] is the Apple Silicon MPS acceleration extra
                                        # [serve] brings in fastapi/uvicorn/httpx
git clone https://github.com/Mapika/decider /tmp/decider-repo
```

Start the server (note: model id must be the **full Hugging Face repo id**, and UVICORN must point to the venv's uvicorn binary):

```bash
cd /tmp/decider-repo
UVICORN=/tmp/ta-ask-decider-env/bin/uvicorn \
PATH="/tmp/ta-ask-decider-env/bin:$PATH" \
bash scripts/serve.sh Mapika/decider-4b 8700
```

Then `DeciderBackend::connect("http://127.0.0.1:8700", "decider-4b")`, or `DeciderBackend::spawn(...)` to have Rust own the process lifecycle itself.

**`model_id` is load-bearing for `spawn()`, not just a label.** `connect()`'s `model_id` is a free-text label only (it never leaves the process -- it's just copied into `DecisionResponse::model_id`). `spawn()`'s `model_id`, by contrast, is forwarded directly as an argument to `scripts/serve.sh`, which passes it straight to `decider.serve` as `DECIDER_MODEL` -- it must be the full Hugging Face repo id (e.g. `"Mapika/decider-4b"`, not `"decider-4b"`), or the server fails to load the model.

**`spawn()` takes an `env` parameter** (`&[(&str, &str)]`) that's passed straight through to the child process's environment. The typical use is `UVICORN`, which `scripts/serve.sh` reads to locate the `uvicorn` binary (it defaults to a repo-relative `.venv312/bin/uvicorn`, which won't exist outside the original authors' own checkout):

```rust
DeciderBackend::spawn(
    Path::new("/tmp/decider-repo/scripts/serve.sh"),
    "Mapika/decider-4b",
    8700,
    &[("UVICORN", "/tmp/ta-ask-decider-env/bin/uvicorn")],
    Duration::from_secs(60),
)?;
```

**Wire format:** The real `/decide` endpoint takes and returns JSON objects, not arrays:
- Request: `{"context": "<str>", "schema": {"<question text>": {"type": "bool"} | {"type": "choice", "options": [...]}}}`
- Response (bool question): `{"<question text>": {"noul": <0..1 prob-of-yes>, "type": "noul"}}`
  - Confidence is synthesized from the `noul` probability: `confidence = if noul > 0.5 { noul } else { 1.0 - noul }`
- Response (choice question): `{"<question text>": {"choice": "<str>", "confidence": <0..1>, "type": "choice", "probabilities": {...}}}`

## Known limitation: `DecisionSchema::Score` is not supported by `DeciderBackend`

`DeciderBackend::decide` returns `DecisionError::UnsupportedSchema` for `Score` requests. No real consumer needs it yet (both known near-term consumers are `YesNo`/`Choice`-shaped). Decider does have a richer `system_one` API that supports scored/typed answers with discrete levels, not a continuous `min..max` range — if `Score` support is ever needed, that's the API to map against, not a client-side approximation layered on `decide()`.

## Consumers

- `ta-workflow`'s `DecisionReviewerNode` (`crates/ta-workflow/src/graph/nodes/decision_reviewer.rs`) — opt-in via `NodeRegistry::register_decision_reviewer`, not part of `with_builtins()`.
- Future (not yet built, tracked separately): Wayfinder's CoS chat-vs-governed classifier, Untollable's own chat-vs-governed classifier (explicitly specified to share this implementation, not just the design, with Wayfinder's), and a semantic-overlap detection signal feeding `task-graph`'s `impact_tags` from the caller side (never a `task-graph` dependency itself).
