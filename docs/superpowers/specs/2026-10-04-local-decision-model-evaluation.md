# Local Decision-Model Evaluation and Hardware Install Profiles

**Status:** two real, viable, purpose-built candidates identified and verified from primary
sources (Intern-Decision-4B, Decider-4b). Not yet narrowed to one — see §3 for why, and §6 for
what's needed before this is final.

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

## 3. Intern-Decision-4B vs. Decider-4b — not yet resolved to one

Both are real, both are purpose-built and calibrated (not general chat models repurposed via
prompting), both are Apache 2.0, both are ~4-5B with GGUF builds available. Neither should be
treated as the settled winner yet:

- Intern-Decision-4B's benchmark numbers come from its own model card (self-reported by InternLM).
  Decider-4b's come from a third-party hobby benchmark whose own maintainer isn't affiliated with
  either model's creator or with TypeSafe AI — arguably more independent, but also less rigorous
  (one maintainer, donation-funded) and, per the versioning issue above, apparently volatile across
  leaderboard revisions.
- Decider-4b's GGUF repo claims *stronger* quantization-calibration preservation (Q8_0 "same
  quality as bf16") than Intern-Decision-4B's own GGUF repo claims for itself (Q8_0 "can change
  calibration quality") — but neither claim has been independently re-measured by us (see §6).
  Take this as a reason to verify both, not as a reason to prefer one yet.
- Decider-4b's documented 0.8B-35B range gives more install-profile flexibility across hardware
  tiers from one family; Intern-Decision has a narrower 0.8B/2B/4B range.

**Recommendation: evaluate both directly, same labeled set, same hardware, before picking one** —
not a judgment call to make from secondhand benchmark claims alone, especially given how much the
Decider/Jev comparison apparently moved between leaderboard versions.

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

**Primary targets: Intern-Decision-4B and Decider-4b**, for every tier that can run a ~4-5B model
— both purpose-built and calibrated, pending the direct head-to-head comparison in §3.

| Tier | Recommendation | Why |
|---|---|---|
| Apple Silicon, 32GB (this machine, M1 Max) | **Intern-Decision-4B or Decider-4b** (Q8_0, ~4.5GB either way) | Both fit trivially. Gemma 4 26B-A4B (Q4_K_M, ~15.4GB) also fits as a general-purpose comparison point, not a primary recommendation. Metal-specific calibration is unverified for both (Decider's card says so explicitly; Intern-Decision's Metal performance isn't independently sourced either) — needs real measurement before trusting either on this machine's GPU path. |
| Apple Silicon, 8-16GB | **Intern-Decision-4B or Decider-4b** (Q8_0, ~4.5GB) | Still fits; Gemma 4 does not. SmolLM3-3B/Qwen3.5-0.8B as lighter fallbacks; Decider's smaller family members (down to 0.8B) are a same-family fallback option Intern-Decision also has at 0.8B/2B. |
| Consumer NVIDIA, 8-24GB VRAM | **Intern-Decision-4B or Decider-4b** | Both fit any card in range; Gemma 4 needs 16GB+. Decider's documented range up to 35B gives more headroom on higher-VRAM cards if more accuracy is wanted at the cost of speed. |
| CPU-only / no accelerator | **Unresolved — needs real benchmarking for both.** Intern-Decision-4B's only published latency figure is GPU-measured (RTX 4090); Decider-4b's repo documents a CPU fallback path but doesn't publish CPU latency either. Intern-Decision-0.8B, Decider's own smaller variants, or Qwen3.5-0.8B are the realistic fallback candidates pending an actual CPU benchmark against the <400ms p50 target from Untollable's own spec. |

## 7. Before this is final

- **Run a direct head-to-head**: Intern-Decision-4B vs. Decider-4b, same labeled eval set, same
  hardware (this M1 Max), before picking a default — don't inherit either model's self-reported or
  third-party-leaderboard numbers as the deciding factor given §3's findings.
- Empirically re-verify Brier/ECE (or an equivalent calibration check) at whatever quantization is
  actually deployed, for whichever model is chosen — both GGUF repos make quantization-calibration
  claims that haven't been independently re-measured.
- Benchmark CPU-only latency directly for both candidates rather than assuming either fails or
  passes the <400ms target.
- Benchmark Metal specifically on this hardware — neither model's card documents this despite
  Metal being the real acceleration path on an M1 Max.
- Confirm which JevBench leaderboard version is current before citing any Decider-vs-Jev
  comparison again — v1.4.2.2 and v1.5.0 disagree on the ranking.
- The Ollama-import friction (manual Modelfile, not a plain `pull`) applies to both
  Intern-Decision-4B and Decider-4b — needs to be a documented, scripted step in whatever "local
  install profile" tooling gets built.
