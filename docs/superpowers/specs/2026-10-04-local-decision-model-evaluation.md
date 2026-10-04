# Local Decision-Model Evaluation and Hardware Install Profiles

**Status:** resolved to a default, empirically, 2026-10-04. A real head-to-head ran on this
machine (M1 Max, MPS) — see §3. **Decider-4b is the recommended default** (passes the <400ms p50
latency target; Intern-Decision-4B fails it ~3.4x due to missing MPS kernels), with both scoring
identically on accuracy/stability. Open items remain (CPU-only latency, full Brier/ECE
recomputation) — see §7.

**Why this is its own document, not a section of the main decision-primitive design spec:**
this is a living reference (model landscape changes, new hardware tiers get added) rather than a
one-time architecture proposal — it should be updatable independently of the primitive's own
design doc. See `2026-10-04-local-decision-model-primitive-design.md` (TODO: create once the
primitive design itself is written up and approved) for the crate this evaluation feeds into.

**Ground rule for this document:** every specific claim (model name, parameter count, license,
benchmark number) must be sourced and verifiable, not carried over from a prior conversation
summary or accepted on confidence of phrasing alone. This applies symmetrically — a claim this
document originates must meet the same bar as a claim it's asked to verify from someone else.

## This machine's hardware (confirmed directly, 2026-10-04)

```
$ sysctl -n machdep.cpu.brand_string
Apple M1 Max
$ system_profiler SPHardwareDataType | grep -E "Chip|Memory|Model Name"
Model Name: MacBook Pro
Chip: Apple M1 Max
Memory: 32 GB
```

## 1. Intern-Decision-4B

Confirmed via the real Hugging Face model card
([internlm/Intern-Decision-4B](https://huggingface.co/internlm/Intern-Decision-4B)) and collection
page ([huggingface.co/collections/internlm/intern-decision](https://huggingface.co/collections/internlm/intern-decision)).
The real family is **Intern-Decision-0.8B, -2B, and -4B**, from InternLM.

- ~5B actual parameters (named "4B" after its base model), fine-tuned from **Qwen3.5-4B**.
- License: **Apache 2.0**, with the base model's `LICENSE-QWEN` preserved alongside it.
- Architecture: a *structured* decision model, not a chat model — takes a shared state plus a
  schema of named questions (optionally with images; it's multimodal) and returns a calibrated
  answer-distribution per question in one forward pass. Does not generate free text.
- Published benchmarks: 90.02% average across seven benchmarks (Jevbench-Easy 100%,
  Jevbench-Original 98.61%, Jevbench-Hard 73.87%, plus Typed Decision/ToolACE/AG News/WildJailBreak).
  **Brier score 0.347, Expected Calibration Error 0.065.**
- Latency: 44.16ms mean inference, benchmarked on an **RTX 4090** (not CPU, not Apple Silicon).
- GGUF: [bombdefuser-124/Intern-Decision-4B-GGUF](https://huggingface.co/bombdefuser-124/Intern-Decision-4B-GGUF),
  Q8_0 is 4.48GB, requires a `mmproj-f16.gguf` vision projector and a recent llama.cpp build with
  Qwen3.5 multimodal support. **Not in Ollama's official model library** — needs a manual
  `ollama create` + Modelfile import.
- **Caveat, from the GGUF repo itself**: Q8_0 quantization "can change calibration quality" vs.
  the original BF16 weights — relevant since calibration is this model's whole value proposition.

## 2. Decider-4b (Mapika) — a real, comparably strong alternative

Raised by the user citing specific figures not present anywhere in this document's own earlier
research. Independently verified from primary sources — real, and the specific claims check out,
with two caveats below worth being precise about.

- [huggingface.co/Mapika/decider-4b](https://huggingface.co/Mapika/decider-4b) — real model card.
  4.2B params, fine-tuned from **Qwen3.5-4B-Base** (32 layers, mixed full/gated-linear attention).
  License: **Apache 2.0**. States explicitly: "This is an independent project. It is not
  affiliated with or endorsed by TypeSafe AI." Same architecture category as Intern-Decision-4B —
  reads state + typed questions (choice/score/yes-no), returns a probability distribution in one
  forward pass, no post-hoc JSON-parsing from a general chat model.
- [github.com/Mapika/decider](https://github.com/Mapika/decider) — real, active repo: 1.1k stars,
  46 forks, 95 commits, last activity 2026-09-30. Confirms **size range 0.8B through 35B
  parameters** (to trade speed for accuracy per use case), hardware fallback chain "CUDA, else
  MPS, else CPU."
- GGUF builds: [huggingface.co/Mapika/decider-4b-GGUF](https://huggingface.co/Mapika/decider-4b-GGUF)
  — Q4_K_M (2.7GB), Q8_0 (4.5GB, the model card itself claims "same quality as the bf16 weights"
  — notably a *stronger* calibration-preservation claim than Intern-Decision-4B's own GGUF repo
  carries, though neither has been independently re-verified by us yet, see §6), BF16 (8.4GB).
  llama.cpp support is explicitly documented with real install commands.
- **Benchmark claim, verified precisely**: [JevBench](https://github.com/fstandhartinger/jevbench)
  is a real benchmark — but it's a **one-person hobby project** (maintainer fstandhartinger,
  donation-funded), explicitly "not affiliated with TypeSafe AI, though it measures their Jev
  model." Worth weighting accordingly against a vendor-run or peer-reviewed benchmark. Leaderboard
  **v1.4.2.2** shows exactly what was claimed: decider-4b v2 = **64.13** (#3), Jev 1.13.0
  (TypeSafe AI) = **63.29** (#4) — confirmed accurate for that specific snapshot.
- **Important caveat the original claim didn't include**: a *later* leaderboard version (v1.5.0)
  shows the ranking reversed — Jev 72.1 (#3) ahead of decider-4b 71.3 (#5), described elsewhere as
  decider-4b being "16 points behind on intelligence" in that version. **The 64.13/63.29
  comparison is real but may be a superseded snapshot, not decider-4b's current standing** — check
  the leaderboard's current version before treating this as settled.
- **One overstatement to correct**: "proven llama.cpp+Metal support" overstates it. Metal is a
  real, documented build option (`-DGGML_METAL=on`), but the model card explicitly says **"The
  CPU and Metal builds were not run over the regression set"** — i.e. the build path is real, but
  calibration/quality on Metal specifically (directly relevant to this M1 Max) is undocumented,
  not proven.
- One source (`jev-ai.pro/compare/jev-vs-decider`) could not be independently reached — not
  confirmed beyond a search snippet.

## 3. Intern-Decision-4B vs. Decider-4b — resolved via a real head-to-head run

Both are real, both are purpose-built and calibrated (not general chat models repurposed via
prompting), both are Apache 2.0, both are ~4-5B with GGUF builds available. Rather than deciding
from either model's own benchmark claims (§1, §2 — one self-reported, one from a volatile
third-party leaderboard), **both were actually downloaded, loaded, and run on this machine**
(Apple M1 Max, 32GB, MPS) against the real shared `f5_cheap_signal` labeled set (10 cases, 2 runs
each), using each model's own documented native inference path — **not** Ollama/GGUF (see the
note at the end of this section on why that changed).

| | Decider-4b | Intern-Decision-4B |
|---|---|---|
| Accuracy | 20/20 (100%) | 20/20 (100%) |
| Stability (identical result across 2 runs) | 10/10 | 10/10 |
| Mean latency | **342.9ms** | 1619.2ms |
| p50 latency | **310.7ms** | 1371.1ms |
| vs. Untollable's <400ms p50 target | **Passes** | **Fails, ~3.4x over** |
| Load time | ~13-19s | ~16-21s |

Both models are equally accurate and equally stable on this task — the latency gap is the whole
story, and it's architectural, not incidental. Running Intern-Decision-4B, `transformers` itself
logged, unprompted: `causal_conv1d_fn falling back to reference PyTorch implementation... much
slower`, and the same for `chunk_gated_delta_rule` (needs `flash-linear-attention`, which is
Triton-based with no real Apple GPU path). **Intern-Decision-4B has no MPS-optimized kernels and
no path to get them** — its speed-critical ops are CUDA/Triton-only. Decider-4b shipped dedicated
`mps_ops.py`/`mps_moe.py` patches that are real and working, with no fallback-kernel warnings.

**Recommendation: Decider-4b is the default.** Not because its earlier-cited benchmark edge over
Jev holds up (§2 already flagged that as a possibly-superseded snapshot) — because it's the only
one of the two that meets the actual latency requirement on the actual target hardware, measured
directly, with identical accuracy to the alternative.

**Methodology note — invocation path changed from what §1/§2 assumed.** Neither model is in
Ollama's official library, and the original plan (manual `ollama create` + Modelfile GGUF import)
turned out not to be how this comparison was actually run: both models were invoked through their
own documented native Python packages instead — Decider-4b via `decider.infer.Decider` (which
auto-selects `device="mps"` on its own), Intern-Decision-4B via its `inference.py`/`DecisionEngine`
(which required manually forcing `device="mps"` — it has no auto-detection). This is a real,
load-bearing difference in deployment shape from what §7's "Ollama-import friction" line
originally assumed — worth re-reading before building install-profile tooling around an
Ollama-centric assumption that didn't hold up in practice.

**Not yet measured, even after this real run** (ran out of scope for the time spent, not
skipped by oversight): CPU-only latency for either model, and a full Brier-score/ECE
recomputation (this run checked 10-case accuracy/stability, not the larger eval sets each
model's own published calibration numbers were computed from).

## 4. Other real candidates checked, and why most don't qualify

| Model | License | Fit for this use case |
|---|---|---|
| **JudgeLRM-3B / JudgeLRM-7B** ([arXiv:2504.00050](https://arxiv.org/abs/2504.00050)) | **MG0-2.0 (ModelGo) — confirmed NOT OSI-approved**, [still under OSI review](https://discuss.opensource.org/t/question-about-the-modelgo-license-review-status/1441) | Disqualified on license alone despite strong claimed judge-benchmark performance (7B claims beating DeepSeek-R1 by 2.79% F1) — fails the "completely open source" requirement. |
| **Gemma 4 26B-A4B** (Google, MoE, 25.2B total / 3.8B active) | **Apache 2.0** — real, recent change; earlier Gemma generations used a restrictive custom "Gemma Terms of Use," Gemma 4 (April 2026) switched to genuine Apache 2.0 | Qualifies on license, but general-purpose — not purpose-built or calibrated for decision/classification, usable as a judge only via prompting. Q4_K_M ≈15.4GB, Q8 ≈28.8GB, BF16 ≈57.7GB. |
| **SmolLM3-3B** | Apache 2.0 | Qualifies, tiny (Q4_K_M ≈1.9GB), general small reasoning model, not purpose-built for calibrated decisions. |
| **Qwen3.5-0.8B** | Apache 2.0 | Qualifies — the literal base model Intern-Decision-0.8B is fine-tuned from. |
| Prometheus 2 (8x7B), aipsy-judge-1.0 | Claimed Apache 2.0 | **Unconfirmed** — found referenced in search results but could not be independently verified against a direct primary source. Do not treat as real options until verified. |

## 5. `qwen3:14b` — real, but not purpose-built, and not a vetted choice

Real and current: Apache 2.0, 14.8B parameters, 40 layers, 32,768 native context (131,072 with
YaRN), **in Ollama's official library** (`qwen3:14b`, Q4_K_M, 9.3GB — a plain `ollama pull` works,
unlike either Intern-Decision-4B or Decider-4b above). General-purpose chat/reasoning model usable
as a judge only via prompting — **no published calibration metric** (no Brier/ECE equivalent).

**Confirmed directly by reading `f5_cheap_signal`'s own source (2026-10-04):**
`~/development/amplifiedxai/cinepipe-sage/experiments/f5_cheap_signal/predicted_proxy.py:46` and
`evaluate.py:23` both hardcode `model: str = "qwen3:14b"` as a default argument, called against
`http://localhost:11434/v1` (Ollama's default local endpoint), with **no rationale recorded
anywhere** for choosing this specific model — not in the README, not in a comment. The README's
own results (10/10 accuracy, 4/4 stable across repeated runs, vs. 2/4 for the same check embedded
in a 12-clause judge) validate the *narrow-question methodology*, not this specific model choice.
The most honest read: whatever was already pulled locally on 2026-09-07, not a deliberate pick.

## 6. Hardware-tier recommendations

**Primary target: Decider-4b**, confirmed by real measurement on the Apple Silicon tier (§3) — the
only candidate that met the latency target on the hardware actually tested. Other tiers are not
yet empirically confirmed; recommendations below are reasoned from spec sheets, not measured, and
should get the same real-run treatment §3 gave the 32GB Apple Silicon tier before being trusted.

| Tier | Recommendation | Why |
|---|---|---|
| Apple Silicon, 32GB (this machine, M1 Max) | **Decider-4b** — confirmed, measured | 310.7ms p50, passes target; Intern-Decision-4B measured 1371.1ms p50 on identical hardware, fails. Not a spec-sheet estimate — both were actually run. |
| Apple Silicon, 8-16GB | **Decider-4b (Q8_0, ~4.5GB), not yet measured on this tier specifically** | Same model recommended by extension, but this tier's actual latency hasn't been separately measured — lower unified memory may affect swap/paging behavior differently than on 32GB. Decider's smaller family members (down to 0.8B) are a same-family fallback if 4.5GB is too tight. |
| Consumer NVIDIA, 8-24GB VRAM | **Unconfirmed — needs its own real run.** Decider's MPS-specific kernel advantage (§3) is an Apple Silicon finding; on CUDA hardware Intern-Decision-4B's Triton-based ops may perform differently (better, even) since Triton targets CUDA natively. Do not assume the Apple Silicon result transfers to this tier. | 
| CPU-only / no accelerator | **Still unresolved** — neither model's CPU latency was measured in the real run (§3), only MPS. Intern-Decision-0.8B, Decider's own smaller variants, or Qwen3.5-0.8B remain the realistic fallback candidates pending an actual CPU benchmark against the <400ms p50 target. |

## 7. Before this is final

- **CPU-only latency, both models** — not measured even after the real §3 run; still needed before
  that tier's recommendation is anything more than a guess.
- **A real run on the consumer-NVIDIA tier specifically** — don't assume the Apple Silicon
  MPS-kernel result transfers to CUDA hardware; Intern-Decision-4B's Triton-based ops are built for
  CUDA, so the latency gap found on this machine may not hold, or may reverse.
- Empirically re-verify Brier/ECE (or an equivalent full calibration check) for Decider-4b at
  whatever quantization is actually deployed — the real run checked 10-case accuracy/stability,
  not the larger eval set Decider-4b's own published numbers would need to be recomputed against.
- Confirm which JevBench leaderboard version is current before citing any Decider-vs-Jev
  comparison again — v1.4.2.2 and v1.5.0 disagree on the ranking, and the real empirical result in
  §3 settled the latency question independent of that benchmark either way.
- **Deployment path needs updating from the original plan**: neither model is in Ollama's official
  library, and the real comparison in §3 used each model's own native Python inference package
  directly, not an Ollama/GGUF import — install-profile tooling should be built around that
  reality, not the originally-assumed Ollama-import path.
- `/tmp/decision-model-eval/` on this machine still holds both models' full BF16 weights (~17GB
  total) plus the eval scripts/venv from the real run — not committed anywhere, not yet cleaned
  up. Delete once nobody needs to re-run this comparison, or keep temporarily for the CPU-latency
  and Brier/ECE follow-ups above, which could reuse the same setup.
