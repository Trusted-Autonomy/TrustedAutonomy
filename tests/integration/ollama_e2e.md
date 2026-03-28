# Ollama End-to-End Validation Checklist

This validates the full lifecycle of a local-model goal using Qwen3.5.
These tests require a live Ollama instance and are intentionally excluded
from CI (`#[ignore]`). Run manually before a release.

## Prerequisites

- Ollama installed and running (`ollama serve`)
- At least one qwen3.5 variant pulled (`ollama pull qwen3.5:4b`)
- TA CLI built from this workspace (`cargo build --release -p ta-cli`)

## Test 1 — Basic goal run

```bash
ta agent install-qwen --size 4b
ta run "Create a file called hello.txt with the content 'hello world'" \
  --agent qwen3.5-4b
```

**Expected**:
- Agent starts (sentinel `[goal started]` emitted)
- Agent creates `hello.txt`
- Goal completes, draft built
- `ta draft view --latest` shows `hello.txt` in artifact list

## Test 2 — Draft builds and applies

```bash
ta draft build --latest
ta draft approve --latest
ta draft apply --latest --dry-run
```

**Expected**: All commands succeed without error.

## Test 3 — Thinking mode injection

```bash
ta-agent-ollama --model qwen3.5:9b --thinking-mode true \
  --context-file /dev/null &
```

Check stderr: model validation output should appear, prompt contains `/think`.

## Test 4 — Auto model selection

```bash
ta-agent-ollama --model qwen3.5:auto --context-file /dev/null
```

**Expected**: prints `qwen3.5:auto → selected qwen3.5:Xb` where X is the largest installed variant.

## Test 5 — `ta doctor` with Ollama not running

Stop Ollama (`pkill ollama`), then:

```bash
ta doctor
```

**Expected**: Line `Ollama not reachable at http://localhost:11434 — start with: ollama serve` appears in output.

## Test 6 — `ta agent list --local`

With Ollama running and at least one qwen3.5 variant installed:

```bash
ta agent list --local
```

**Expected**:
- Each local agent shown with `[local]` tag
- Model tag, VRAM estimate, and download status displayed
- `downloaded` status for installed models

## Validation result

After running all tests, record:

- [ ] Test 1 passed (basic goal run)
- [ ] Test 2 passed (draft lifecycle)
- [ ] Test 3 passed (thinking mode)
- [ ] Test 4 passed (auto selection)
- [ ] Test 5 passed (doctor health check)
- [ ] Test 6 passed (agent list --local)
