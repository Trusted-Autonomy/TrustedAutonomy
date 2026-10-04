# Local Decision-Model Evaluation and Hardware Install Profiles

**Status:** core research resolved (2026-10-04). Two real open items remain before this is
final: CPU-only latency is unbenchmarked, and quantized-model calibration hasn't been
re-verified against the published BF16 numbers. See §5.

**Why this is its own document, not a section of the main decision-primitive design spec:**
this is a living reference (model landscape changes, new hardware tiers get added) rather than a
one-time architecture proposal — it should be updatable independently of the primitive's own
design doc. See `2026-10-04-local-decision-model-primitive-design.md` (TODO: create once the
primitive design itself is written up and approved) for the crate this evaluation feeds into.

**Ground rule for this document:** every specific claim (model name, parameter count, license,
benchmark number) must be sourced and verifiable, not carried over from a prior conversation
summary. The immediate reason this document exists: a model referred to as "Intern-Decision-4B"
in an earlier part of this session was never written to a persistent file — only a compacted
AI-generated summary preserved it — and its actual size was briefly in question (4B vs 14B).
That class of loss is exactly what this document exists to prevent going forward.

## This machine's hardware (confirmed directly, 2026-10-04)

```
$ sysctl -n machdep.cpu.brand_string
Apple M1 Max
$ system_profiler SPHardwareDataType | grep -E "Chip|Memory|Model Name"
Model Name: MacBook Pro
Chip: Apple M1 Max
Memory: 32 GB
```

## 1. The Intern-Decision naming/size discrepancy — resolved

**No "14B" variant exists.** Confirmed via the real Hugging Face model card
([internlm/Intern-Decision-4B](https://huggingface.co/internlm/Intern-Decision-4B)) and the real
collection page ([huggingface.co/collections/internlm/intern-decision](https://huggingface.co/collections/internlm/intern-decision)):
the actual family is **Intern-Decision-0.8B, -2B, and -4B**, from InternLM. The earlier session's
"4B" figure holds up under independent re-verification from a primary source — it wasn't a
compaction artifact after all. Where "14B" came from is unclear; it doesn't match any real model
in this family or anything else found in this research.

**Intern-Decision-4B, verified specs:**
- ~5B actual parameters (named "4B" after its base model), fine-tuned from **Qwen3.5-4B**.
- License: **Apache 2.0**, with the base model's `LICENSE-QWEN` preserved alongside it.
- Architecture: a *structured* decision model, not a chat model — takes a shared state plus a
  schema of named questions (optionally with images; it's multimodal) and returns a calibrated
  answer-distribution per question in one forward pass. It does not generate free text. This is
  the closest real match to "Jev" (TypeSafe AI's cloud decision model) found anywhere in this
  research.
- Published benchmarks: 90.02% average across seven benchmarks (Jevbench-Easy 100%,
  Jevbench-Original 98.61%, Jevbench-Hard 73.87%, plus Typed Decision/ToolACE/AG News/WildJailBreak).
  **Brier score 0.347, Expected Calibration Error 0.065** — real calibration metrics, which no
  other candidate below has published.
- Latency: 44.16ms mean inference, benchmarked on an **RTX 4090** (not CPU, not Apple Silicon —
  no independently-sourced figure for either exists yet).
- GGUF quantization exists ([bombdefuser-124/Intern-Decision-4B-GGUF](https://huggingface.co/bombdefuser-124/Intern-Decision-4B-GGUF)):
  Q8_0 is 4.48GB, requires a `mmproj-f16.gguf` vision projector and a recent llama.cpp build with
  Qwen3.5 multimodal support. **Not in Ollama's official model library** — deploying via Ollama
  needs a manual `ollama create` + Modelfile import, not a plain `ollama pull`. This is real
  friction any "local install profile" tooling needs to handle explicitly.
- **Real caveat, from the GGUF repo itself**: Q8_0 quantization "can change calibration quality"
  vs. the original BF16 weights. Since calibration is this model's entire value proposition (not
  raw accuracy), this needs empirical re-verification at whatever quantization level is actually
  deployed — don't assume the published Brier/ECE numbers hold at Q8_0 without checking.

## 2. Other real candidates checked, and why most don't qualify

| Model | License | Fit for this use case |
|---|---|---|
| **JudgeLRM-3B / JudgeLRM-7B** ([arXiv:2504.00050](https://arxiv.org/abs/2504.00050)) | **MG0-2.0 (ModelGo) — confirmed NOT OSI-approved**, [still under OSI review](https://discuss.opensource.org/t/question-about-the-modelgo-license-review-status/1441) | Disqualified on license alone despite strong claimed judge-benchmark performance (7B claims beating DeepSeek-R1 by 2.79% F1) — fails the "completely open source" requirement. |
| **Gemma 4 26B-A4B** (Google, MoE, 25.2B total / 3.8B active) | **Apache 2.0** — real, recent change; earlier Gemma generations used a restrictive custom "Gemma Terms of Use," Gemma 4 (April 2026) switched to genuine Apache 2.0 | Qualifies on license, but general-purpose — not purpose-built or calibrated for decision/classification, usable as a judge only via prompting (same shape as the `qwen3:14b` approach below, not a structured-decision model like Intern-Decision). Q4_K_M ≈15.4GB, Q8 ≈28.8GB, BF16 ≈57.7GB. |
| **SmolLM3-3B** | Apache 2.0 | Qualifies, tiny (Q4_K_M ≈1.9GB), general small reasoning model, not purpose-built for calibrated decisions. |
| **Qwen3.5-0.8B** | Apache 2.0 | Qualifies — notably the literal base model Intern-Decision-0.8B is fine-tuned from, so relevant both standalone and as that family's foundation. |
| Prometheus 2 (8x7B), aipsy-judge-1.0 | Claimed Apache 2.0 | **Unconfirmed** — found referenced in search results but could not be independently verified against a direct primary source. Do not treat as real options until verified. |

## 3. `qwen3:14b` — real, but not purpose-built, and not a vetted choice

Real and current: Apache 2.0, 14.8B parameters, 40 layers, 32,768 native context (131,072 with
YaRN), **in Ollama's official library** (`qwen3:14b`, Q4_K_M, 9.3GB — a plain `ollama pull` works,
unlike Intern-Decision-4B). It's a **general-purpose chat/reasoning model** usable as a judge only
via prompting (the "narrow single-question proxy call" pattern `f5_cheap_signal` used) — it has
**no published calibration metric** (no Brier score/ECE equivalent exists for it, unlike
Intern-Decision-4B's real numbers).

**Confirmed directly by reading `f5_cheap_signal`'s own source (2026-10-04):**
`~/development/amplifiedxai/cinepipe-sage/experiments/f5_cheap_signal/predicted_proxy.py:46` and
`evaluate.py:23` both hardcode `model: str = "qwen3:14b"` as a default argument, called against
`http://localhost:11434/v1` (Ollama's default local endpoint). **No rationale for choosing this
specific model is recorded anywhere in the experiment** — not in the README, not in a comment.
The README documents real, genuine evaluation results for *this experiment's own method* (10/10
accuracy, 4/4 stable across repeated runs, vs. 2/4 for the same check embedded in a 12-clause
judge — a real, validated finding worth keeping) — but there is no evidence `qwen3:14b` itself was
compared against alternatives before being used. The most honest read: it was whatever model
happened to already be pulled locally when the experiment was built on 2026-09-07, not a
deliberate best-in-class pick.

**Conclusion: `qwen3:14b` is a real, working option whose underlying narrow-question methodology
is sound and reusable — but it is not a vetted recommendation, and Intern-Decision-4B is
architecturally the better fit for calibrated decision-making specifically.**

## 4. Hardware-tier recommendations

**Primary target: Intern-Decision-4B**, for every hardware tier that can run it — it's the only
candidate purpose-built and empirically calibrated for exactly this job, not a general model
repurposed via prompting.

| Tier | Recommendation | Why |
|---|---|---|
| Apple Silicon, 32GB (this machine, M1 Max) | **Intern-Decision-4B** (Q8_0, 4.48GB) | Fits trivially; purpose-built and calibrated. Gemma 4 26B-A4B (Q4_K_M, ~15.4GB) also fits and could serve as a general-purpose comparison point, but isn't the primary recommendation. |
| Apple Silicon, 8-16GB | **Intern-Decision-4B** (Q8_0, 4.48GB) | Still fits; Gemma 4 does not (~14.4GB+ needed, leaves nothing for the OS). SmolLM3-3B/Qwen3.5-0.8B as lighter fallbacks. |
| Consumer NVIDIA, 8-24GB VRAM | **Intern-Decision-4B** | Fits any card in range; Gemma 4 needs 16GB+. |
| CPU-only / no accelerator | **Unresolved — needs real benchmarking.** Intern-Decision-4B's only published latency figure is GPU-measured (RTX 4090); CPU latency is unverified and likely materially slower. Intern-Decision-0.8B or Qwen3.5-0.8B are the realistic fallback candidates for this tier pending an actual CPU benchmark against the <400ms p50 target from Untollable's own spec. |

## 5. Before this is final

- Empirically re-verify Brier/ECE at whatever quantization is actually deployed, since Q8_0 is
  flagged as possibly degrading calibration from the published BF16 numbers.
- Benchmark CPU-only latency directly rather than assuming it fails the <400ms target.
- The Ollama-import friction (manual Modelfile + vision projector, not a plain `pull`) needs to be
  a documented, scripted step in whatever "local install profile" tooling gets built, not a manual
  one-off.
