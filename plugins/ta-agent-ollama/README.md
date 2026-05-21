# ta-agent-ollama

Trusted Autonomy agent plugin for local models via Ollama (or any OpenAI-compatible endpoint).

Implements a full tool-use loop that lets TA drive any local model as an autonomous agent — reading files, running commands, writing changes — without any cloud API key.

## Prerequisites

- [Ollama](https://ollama.ai) installed and running
- A supported model pulled (see hardware table below)
- TA v0.14.9 or later

## Install

```bash
# Via ta plugin (recommended)
ta plugin install github:trustedautonomy/ta-agent-ollama

# Via cargo
cargo install ta-agent-ollama
```

## Quick start

```bash
# Pull a model and install the agent profile
ta agent install-qwen --size 9b

# Run a goal with the local model
ta run "Fix the authentication bug" --agent qwen3.5-9b
```

## Supported models

| Profile | Model | Min VRAM / RAM | Target hardware |
|---|---|---|---|
| `qwen3.5-4b` | `qwen3.5:4b` | 4 GB VRAM / 16 GB RAM | M1 Mac (base), RTX 3060 |
| `qwen3.5-9b` | `qwen3.5:9b` | 8 GB VRAM / 16 GB RAM | M1 Pro/Max, RTX 3080 |
| `qwen3.5-27b` | `qwen3.5:27b` | 20 GB VRAM / 32 GB RAM | M2 Max/Ultra, RTX 4090 |

Other OpenAI-compatible models (phi4-mini, llama3.1:8b, qwen2.5-coder:7b, etc.) work with `ta agent framework-new --model ollama/<model>`.

## Thinking mode

Qwen3.x models support chain-of-thought reasoning. The bundled profiles configure it automatically:

- `qwen3.5-4b`: thinking **off** — direct responses, stays within context limits
- `qwen3.5-9b`: thinking **on** — better results on complex reasoning tasks
- `qwen3.5-27b`: thinking **on** — significantly enhanced reasoning on hard problems

Override with `--thinking-mode true|false` in the agent profile's `args` list.

## Automatic model selection

```bash
ta-agent-ollama --model qwen3.5:auto
```

Selects the largest installed Qwen3.5 variant (27b > 9b > 4b).

## Troubleshooting

**Ollama not running**: `ollama serve`

**Model not found**: `ollama pull qwen3.5:9b`

**Function calling not working**: Use a model with native tool support (`qwen3.5:*`, `phi4-mini`, `llama3.1:8b`). The agent falls back to Chain-of-Thought mode automatically but results are less reliable.

**Diagnose**: `ta agent doctor qwen3.5-9b`

## Migrating from the monorepo build

If you previously used `ta-agent-ollama` from the TA monorepo, migrate to this standalone plugin:

```bash
ta agent migrate ollama
```

This detects your existing configuration, installs the plugin, and updates profile paths.
