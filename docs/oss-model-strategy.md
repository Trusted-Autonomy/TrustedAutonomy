# OSS Model & Hosting Strategy — TA Virtual Teams

Prepared 2026-08-19. Grounded against live web search (not training-data recall). This space moves fast — re-verify pricing/licensing before committing spend.

**Architecture assumed**: the TA daemon runs on a lightweight PaaS (e.g. Render) and shells out to a separate GPU service running your own model server (vLLM/Ollama/custom) — not a managed model-catalog API. That framing shapes the hosting recommendation below: you want a platform built for running *your own* container/weights, not one built around a fixed catalog of hosted models.

---

## Two different problems, don't conflate them

1. **Agent/LLM models** — the models that *drive* virtual-team roles (research, PM, product, engineering, security, ops): reasoning, tool use, writing, code.
2. **World models** — models that *generate or predict visual state* (video, camera-controlled scenes): what a visual-validation/review agent needs to check generated frames for drift, hallucination, or identity consistency.

**Note on Muse Glimmer**: it is not a world model. It's Meta's 30B general-purpose agentic multimodal model (dense text decoder + a 1.9B ViT perception encoder for image *understanding*, not video generation) — built for local tool-use/reasoning workflows, not visual simulation. It's an excellent fit for category 1, and genuinely excellent on Apple Silicon specifically (Ollama's MLX engine, DFlash speculative decoding gives 1.5–1.8× speedup on M4/M5 Max). It is not a candidate for category 2.

SANA-WM is the real world-model candidate for category 2. It isn't in competition with Glimmer — they solve different problems and likely both belong in a virtual-team stack that needs both agentic roles and visual validation.

---

## Category 1 — OSS LLMs for virtual-team roles

| Role | Model | Why |
|---|---|---|
| Orchestrator / chief-of-staff routing | **GLM-5.2** | Strongest current all-rounder for agentic, long-horizon reasoning; 1M context for cross-project synthesis |
| Engineering (heavy coding) | **Kimi K2.7 Code** (if hardware allows) or **Kimi K2.6** | K2.6 benchmarks well specifically as a sub-agent/worker in multi-agent setups on well-defined parallel tasks — matches TA's worker-node model. Not recommended as top-level planner. |
| Research / PM / product / security / ops | **Gemma 4 31B**, or **GLM-5.2** if budget allows | Gemma 4 31B is the most capable model that still fits consumer/prosumer GPU hardware — best cost/capability balance for non-coding-heavy roles |
| Local-agentic / tool-use / Apple Silicon | **Muse Glimmer 30B** | Purpose-built for local agentic workflows; native image *understanding*; the strongest current option specifically for Mac hardware via MLX |
| Constrained-hardware / high-volume, lower-stakes roles | **Mistral Small 4** | Cheaper, lighter, fine for routine synthesis/status roles |

Avoid **Kimi K3** for self-hosting despite topping coding benchmarks — ~1.4TB of weights, doesn't fit any single 8-GPU node.

---

## Category 2 — World models for visual validation

| Model | Type | Params | Hardware | License | Fit |
|---|---|---|---|---|---|
| **NVIDIA SANA-WM** | True world model — 720p, 60s video generation with 6-DoF camera control | 2.6B | Single GPU (NVIDIA) | Apache 2.0 for code; **verify weight/dataset license separately before commercial use** — the code license doesn't automatically cover generated-content commercial rights | **Recommended starting point.** Lightweight, fast (36× throughput vs prior open baselines per NVIDIA's own claims), single-GPU — cheap to run as an always-on validation service rather than a heavy render job |
| **Wan 2.1 / 2.2** | Diffusion-transformer video generator | 14B (2.1) | Multi-GPU realistic for production quality | Check current Wan license terms directly — not fully verified in this pass | Use where the validation itself needs production-grade fidelity (e.g. final-pass QA), not fast iterative checks |
| Muse Glimmer | *Not applicable* — general agentic LLM, not a world model | — | — | Apache 2.0 | Don't use for this purpose |

**Recommendation**: start with **SANA-WM** for a visual-validation/review loop — fast, cheap, repeated inference is more valuable there than maximal fidelity on every check. Single-GPU footprint keeps the always-on GPU-service cost down. Reserve Wan 2.1/2.2 for cases where the validation itself needs production-grade fidelity.

**Action item before committing**: confirm SANA-WM's weight license explicitly permits commercial-product use — the code license (Apache 2.0) is confirmed permissive, but weight/dataset terms need a direct check against the NVlabs/Sana repo, not assumed from the code license.

---

## Hosting comparison — running your own model server on a GPU service

Ranked for the assumed architecture (daemon shells out to a service running your own container/weights — not a managed model catalog):

| Rank | Platform | Cost | Operational ease | Value/features | Best fit |
|---|---|---|---|---|---|
| **1** | **RunPod** | Cheapest of the group — A100 80GB ~$1.39/hr, H100 SXM ~$2.69/hr, serverless flex billed per-second | Moderate — you manage your own Docker image, but that's exactly what "bring your own setup" needs | Both serverless (scale-to-zero, good for bursty virtual-team LLM calls) *and* dedicated pods (good for an always-on world-model service) in one platform | **Best overall fit.** Full control over custom images (needed for SANA-WM/Wan/vLLM), lowest cost, flexible between spiky and steady workloads |
| **2** | **Modal** | Moderate — ~$2.50/hr A100-equivalent, ~$3.95/hr H100-equivalent, true per-second billing with scale-to-zero | **Best-in-class** — Python-native, decorate functions with GPU requirements, Modal handles scaling/containers | Excellent DX, less mature for "just run my long-lived custom model server" vs its native function-as-a-service model | Strong second choice if engineering time is scarcer than infra cost — trade some cost for much less ops burden |
| **3** | **Baseten** | Highest — dedicated H100 ~$6.50/hr; Model API tier cheaper but catalog-constrained | High — managed vLLM/TensorRT, real SLAs, production observability | Best if uptime guarantees matter to paying customers | Right choice **later**, once a hosted offering has paying customers depending on world-model uptime — premature for early iteration |
| **4** | **Fly.io** | Unclear — no solid current GPU pricing data found in this research pass | Simple mental model if you're already Fly-native elsewhere | General-purpose app platform with GPU machines bolted on, not ML-serving-specialized | Only worth it if you want the GPU service to live alongside other Fly-hosted infrastructure — verify actual current pricing before deciding, don't take this ranking on faith |
| **5** | **Together.ai** | Competitive for its catalog, but you don't control weights | Zero-ops for supported models | Great if you want a *popular* model (Llama/DeepSeek/Qwen) via API | **Not a fit for SANA-WM/Wan/custom world-model hosting** — this is a managed-catalog service, not a bring-your-own-weights host. Could be a fine complementary option for some standard LLM *roles* (research/PM) if you'd rather not run GPU infra for those at all — but keep that decision separate from the world-model hosting decision. |

**Bottom line**: RunPod for the actual world-model GPU service, given "bring your own setup" implies custom weights and cost flexibility between steady and bursty load. Revisit Baseten once uptime SLAs matter commercially. Don't route the world-model workload through Together.ai — wrong tool for a custom model.

---

## Open items / what to verify before committing spend

1. SANA-WM's weight/dataset license for commercial generated-content use (code license alone isn't sufficient confirmation).
2. Current Wan 2.1/2.2 license terms if used beyond what's already been cleared elsewhere.
3. Fly.io's actual current GPU pricing — this pass found no reliable data; don't rank it against RunPod/Modal without real numbers.
4. Whether Together.ai (or a similar managed-catalog service) is worth using for the *LLM role* workloads specifically, as a way to avoid running GPU infra for those roles at all — separate decision from the world-model hosting question above.

---

## Sources

- [The Best Open-Source LLMs in 2026 — BentoML](https://www.bentoml.com/blog/navigating-the-world-of-open-source-large-language-models)
- [Best Open Source LLMs (August 2026) — Thunder Compute](https://www.thundercompute.com/blog/best-open-source-llms)
- [10 Best Open-Source LLMs in July 2026 — Taskade](https://www.taskade.com/blog/open-source-llms)
- [Where to Buy or Rent GPUs for LLM Inference: The 2026 GPU Procurement Guide — BentoML](https://www.bentoml.com/blog/where-to-buy-or-rent-gpus-for-llm-inference)
- [Self-Hosted LLM Costs 2026 — SitePoint](https://www.sitepoint.com/self-hosted-llm-costs-2026/)
- [SANA-WM: NVIDIA's Open Source World Model for Minute-Scale Video](https://studio.aifilms.ai/blog/sana-wm-nvidia-world-model)
- [NVIDIA Introduces SANA-WM — MarkTechPost](https://www.marktechpost.com/2026/05/16/nvidia-introduces-sana-wm-a-2-6b-parameter-open-source-world-model-that-generates-minute-scale-720p-video-on-a-single-gpu/)
- [NVlabs/Sana — GitHub](https://github.com/NVlabs/SANA)
- [Video Generation Models as World Models: Efficient Paradigms, Architectures and Algorithms (arXiv)](https://arxiv.org/pdf/2603.28489)
- [RoboTrustBench: Benchmarking the Trustworthiness of Video World Models](https://arxiv.org/pdf/2606.01600)
- [Muse Glimmer from Meta Superintelligence Labs — Ollama Blog](https://ollama.com/blog/muse-glimmer)
- [SGLang Adds Day-0 Support for Muse Glimmer — LMSYS Org](https://www.lmsys.org/blog/2026-08-10-meta-muse-glimmer)
- [Meta Open-Sources Muse Glimmer — InfoQ](https://www.infoq.com/news/2026/08/meta-muse-glimmer/)
- [Custom AI Endpoint Deployment Platform Comparison: Baseten vs Modal vs RunPod vs GMI Cloud](https://www.gmicloud.ai/en/blog/custom-ai-endpoint-platform)
- [I Tested 9 Serverless GPU Providers for AI Inference in 2026 — DEV Community](https://dev.to/heckno/i-tested-9-serverless-gpu-providers-for-ai-inference-in-2026-heres-what-id-actually-use-4cf4)
- [Best Serverless GPU Platforms for AI Apps and Inference in 2026 — Koyeb](https://www.koyeb.com/blog/best-serverless-gpu-platforms-for-ai-apps-and-inference-in-2026)

---

## Provenance

Generalized for TA virtual-team use. Slated to move into the `secure-autonomy` private repo as part of the virtual-team setup in v0.17.11.
