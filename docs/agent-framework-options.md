# Local Inference Backend Options for `ta-agent-ollama`

> Research spike (v0.13.16) — Ollama vs llama.cpp server vs vLLM vs LM Studio.
> Focus: API compatibility, tool-calling support, macOS/Linux support, startup time,
> model availability.

---

## Summary Comparison

| Backend | API compat | Tool calling | macOS | Linux | Startup | Best for |
|---------|-----------|-------------|-------|-------|---------|----------|
| **Ollama** | Full OpenAI compat | ✅ (≥0.3.6) | ✅ native | ✅ | ~2–5 s | Most users (recommended default) |
| **llama.cpp server** | Full OpenAI compat | ✅ (grammar-based) | ✅ native | ✅ | ~1–3 s | Edge, embedded, custom quantization |
| **vLLM** | Full OpenAI compat | ✅ (most models) | ⚠️ Linux-preferred | ✅ GPU | ~10–30 s | GPU clusters, high throughput |
| **LM Studio** | Full OpenAI compat | ✅ (model-dependent) | ✅ native | ✅ (CLI only) | ~5–10 s | Desktop GUI + API dual use |

---

## Ollama

**URL**: https://ollama.com
**API base**: `http://localhost:11434`
**OpenAI endpoint**: `/v1/chat/completions`, `/v1/models`

### Tool calling

Ollama supports OpenAI-compatible function calling as of version 0.3.6 (July 2024).
Models must declare tool support in their model card. Verified working:

- `qwen2.5-coder:7b` — full parallel tool calls, excellent coding performance
- `qwen2.5-coder:14b` — best-in-class for code; requires ≥16 GB VRAM / RAM
- `phi4-mini` — lightweight (3.8B), solid tool calling on Apple Silicon
- `llama3.1:8b` — good general tool use; slower than Qwen on code tasks
- `llama3.2:3b` — fast, limited tool precision
- `mistral:7b-instruct-v0.3` — reliable tool calling with Ollama's grammar enforcement
- `deepseek-coder-v2:16b` — strong code agent; high memory requirement

### Startup time

```
$ time ollama run qwen2.5-coder:7b --nowordwrap ""
  ~ 2–4 seconds (model already pulled; Metal on Apple M-series)
  ~ 4–8 seconds (first run; CUDA on Linux GPU)
```

### macOS support

First-class. Native Metal acceleration via llama.cpp backend. DMG installer.
No CUDA required. Models download to `~/.ollama/models/`.

### Configuration

```toml
# ~/.config/ta/agents/qwen-coder.toml
name = "qwen-coder"
version = "1.0.0"
command = "ta-agent-ollama"
args = ["--model", "qwen2.5-coder:7b", "--base-url", "http://localhost:11434"]
description = "Qwen 2.5 Coder 7B via Ollama"
context_inject = "env"

[memory]
inject = "env"
write_back = "exit-file"
max_entries = 10
```

### Pros
- Easiest installation (`brew install ollama` or one-click installer)
- Automatic model management (`ollama pull <model>`)
- Native macOS/Linux/Windows support
- Actively maintained; model library with 100+ models

### Cons
- Daemon process required (`ollama serve`)
- Model downloads can be large (4–40 GB)
- Some newer models not yet supported

---

## llama.cpp server (`llama-server`)

**URL**: https://github.com/ggerganov/llama.cpp
**API base**: `http://localhost:8080`
**OpenAI endpoint**: `/v1/chat/completions`, `/v1/models`

### Tool calling

Full JSON-schema grammar enforcement via GBNF. Does not depend on the model
having been trained for function calling — the grammar forces valid JSON output.
Reliable even on models that don't have native tool-call training.

Requires `--jinja` flag for full OpenAI-compatible tool schemas:

```bash
llama-server -m qwen2.5-coder-7b-Q4_K_M.gguf \
  --port 8080 --ctx-size 8192 --jinja \
  --parallel 1
```

### Startup time

~1–3 seconds (model already in RAM). Initial model load: ~5–15 seconds depending
on model size and storage speed.

### macOS support

Excellent. Metal acceleration via Accelerate framework. Compiled natively.
Available via Homebrew: `brew install llama.cpp`.

### Configuration for ta-agent-ollama

```toml
# ~/.config/ta/agents/llama-cpp.toml
name = "llama-cpp"
version = "1.0.0"
command = "ta-agent-ollama"
args = ["--model", "qwen2.5-coder-7b", "--base-url", "http://localhost:8080"]
description = "Local model via llama.cpp server"
```

### Pros
- Lowest memory overhead (pure C++ inference)
- Offline, no daemon required (can start/stop per goal)
- Best support for custom GGUF quantizations
- GBNF grammar enforcement makes tool calling robust on any model

### Cons
- Manual model download and conversion (`.gguf` format)
- No built-in model library / management
- CLI-heavy setup vs Ollama's developer-friendly UX

---

## vLLM

**URL**: https://vllm.ai
**API base**: `http://localhost:8000`
**OpenAI endpoint**: `/v1/chat/completions`, `/v1/models`

### Tool calling

Full OpenAI-compatible tool calling for models with native function-call training
(Qwen2.5, Llama3.1, Mistral, Gemma2). Requires `--enable-prefix-caching` for
performance; `--guided-decoding-backend outlines` for grammar-based enforcement.

### Startup time

~10–30 seconds (model compilation and GPU kernel loading). Not suitable for
interactive use where startup latency matters. Best for batch workflows.

### macOS support

Limited. vLLM is primarily designed for NVIDIA/CUDA GPU environments.
Experimental CPU support exists but is slow. Not recommended for macOS deployments.

### Configuration for ta-agent-ollama

```toml
# ~/.config/ta/agents/vllm.toml
name = "vllm"
command = "ta-agent-ollama"
args = ["--model", "Qwen/Qwen2.5-Coder-7B-Instruct",
        "--base-url", "http://localhost:8000"]
```

### Pros
- Highest throughput for GPU clusters (PagedAttention, continuous batching)
- Supports the widest range of HuggingFace model architectures
- Full OpenAI API compatibility

### Cons
- Linux + NVIDIA GPU primary target
- High startup latency
- Large infrastructure footprint
- Not practical for per-developer use

---

## LM Studio

**URL**: https://lmstudio.ai
**API base**: `http://localhost:1234`
**OpenAI endpoint**: `/v1/chat/completions`, `/v1/models`

### Tool calling

Supported for models with native tool-call training. Uses llama.cpp internally.
GUI shows active tool calls; JSON mode enforced. Works well with Qwen2.5-Coder,
Phi-4, and Mistral series.

### Startup time

GUI startup: ~5–10 seconds.
API server (local server mode): starts with the app; model load ~3–8 seconds.

### macOS support

First-class native macOS app. Metal acceleration. Model library browser built-in.

### Configuration for ta-agent-ollama

```toml
# ~/.config/ta/agents/lm-studio.toml
name = "lm-studio"
command = "ta-agent-ollama"
args = ["--model", "qwen2.5-coder-7b",
        "--base-url", "http://localhost:1234"]
description = "Local model via LM Studio API server"
```

### Pros
- Best GUI experience for model exploration and testing
- Easy model download from HuggingFace
- Works as both development tool and headless API server

### Cons
- GUI app required (no headless install)
- API server must be manually started in the app before using
- Less suitable for CI/server environments

---

## Recommendation Matrix

| Use case | Recommended backend |
|----------|-------------------|
| Developer laptop (macOS/Linux), first-time setup | **Ollama** |
| Custom quantized models, minimal memory overhead | **llama.cpp server** |
| GPU cluster, batch processing, high throughput | **vLLM** |
| GUI-first exploration + occasional API use | **LM Studio** |
| CI/CD pipeline local inference | **llama.cpp server** or **Ollama** |

---

## Model Validation Matrix

These models were evaluated for function-calling support via `ta-agent-ollama`'s
startup probe. Results recorded against Ollama 0.6.x and llama.cpp server March 2026.

| Model | Size | Ollama | llama.cpp | Tool calling | Notes |
|-------|------|--------|-----------|-------------|-------|
| `qwen2.5-coder:7b` | 4.7 GB | ✅ | ✅ | ✅ | **Recommended for most users** |
| `qwen2.5-coder:14b` | 9.0 GB | ✅ | ✅ | ✅ | Best coding quality |
| `phi4-mini` | 2.5 GB | ✅ | ✅ | ✅ | Fast, good quality on Apple Silicon |
| `llama3.1:8b` | 4.7 GB | ✅ | ✅ | ✅ | General purpose |
| `llama3.2:3b` | 2.0 GB | ✅ | ✅ | ⚠️ | Occasional formatting errors |
| `kimi-k2.5` | varies | ✅ | N/A | ✅ | Via Kimi API (remote) |
| `deepseek-coder-v2:16b` | 9.0 GB | ✅ | ✅ | ✅ | Strong code; high memory |
| `mistral:7b-instruct-v0.3` | 4.1 GB | ✅ | ✅ | ✅ | Reliable, fast |
| `gemma2:9b` | 5.4 GB | ✅ | ✅ | ⚠️ | Limited tool precision |

**Legend**: ✅ Working, ⚠️ Partial/unreliable, ❌ Not supported

---

## Using ta-agent-ollama

Quick start with Ollama (recommended):

```bash
# 1. Install Ollama
brew install ollama        # macOS
# curl -fsSL https://ollama.com/install.sh | sh  # Linux

# 2. Pull a model
ollama pull qwen2.5-coder:7b

# 3. Generate a framework manifest
ta agent framework-new --model ollama/qwen2.5-coder:7b

# 4. Run a goal with the local model
ta run "refactor the auth module" --model ollama/qwen2.5-coder:7b

# Or use the named framework
ta run "refactor the auth module" --agent qwen-coder
```

For llama.cpp server:

```bash
# 1. Install
brew install llama.cpp

# 2. Download a GGUF model
# (e.g., from https://huggingface.co/bartowski/Qwen2.5-Coder-7B-Instruct-GGUF)

# 3. Start the server
llama-server -m ~/models/qwen2.5-coder-7b-Q4_K_M.gguf \
  --port 8080 --ctx-size 8192 --jinja

# 4. Run a goal
ta run "refactor the auth module" \
  --agent ta-agent-ollama \
  -- --model qwen2.5-coder-7b --base-url http://localhost:8080
```
