# Output Token Reduction: Research and Recommendation

**Status:** research, 2026-10-17. Feeds the `OutputOptimizer` slot in
`docs/superpowers/specs/2026-10-13-token-cost-optimization-and-ab-harness.md` (section 5).
**Scope:** tokens the model generates (text, tool-call arguments, edits, thinking). Not input or context compression.

Evidence labels used below: **independent** (not by the technique's author), **author** (benchmark by the author or
vendor of the technique), **claim** (a number with no method shown), **secondary** (reported by a write-up that we
could not trace to a primary source in this pass). Where a figure is only a claim, it is said so.

## 1. Why output reduction is worth only a bounded amount for TA

Our own measurement (spec section 1) says cost is context length times turns. Output was 5.3% of the cost of a long
interactive session and 24% of the largest measured phase goal (89.6k output tokens, $9.82). An independent write-up of
Caveman estimates output at 0.6% to 2.5% of a typical Claude Code bill. So:

- Maximum possible saving from a perfect 100% output cut is 5% to 24% of cost. A realistic 20% output cut is
  about 1% to 5% of total cost.
- Output tokens are priced about 5x input and are also the slowest tokens (sequential decode), so output cuts buy
  **latency** more than dollars. For `ta run` goals that matter for wall-clock time.
- There is one second-order effect that can matter more than the direct saving: every output token is later re-read
  as cached context on every following turn. A token written at turn t of N costs one output rate plus about
  (N minus t) cache-read charges. Edits and tool-call arguments that stay in the transcript are therefore worth more to
  shrink than a final chat reply. This is an inference from our spec's cost model, not a measurement.

Recommendation preview: do not expect output work to move the bill the way the context ceiling does. Build the cheap,
safe pieces, measure them, and stop.

## 2. The science: where output tokens come from

An autoregressive model emits one token per forward pass. Output cost is the sum of five sources, and each technique
attacks a different one:

| Source | Example in a coding agent | Which technique attacks it |
|---|---|---|
| Narration and pleasantries | "Sure, I'll now look at the file and..." | terse-output prompts (Caveman) |
| Reasoning / thinking | extended thinking blocks, step-by-step plans | effort and thinking-budget controls, concise-CoT prompts |
| Edit payload | retyping old lines to replace them, or rewriting a whole file | diff formats, search/replace, hash-anchored edits |
| Retries | failed edit (mismatched context), re-issued tool call | better edit formats (fewer failures), validation before apply |
| Structure overhead | JSON keys, wrappers, repeated fields | compact schemas, constrained decoding |

Two facts constrain everything:

1. **Brevity is not free in quality.** For reasoning-heavy work, the written steps are the computation. Cutting
   them can lower accuracy ("Let Me Speak Freely?", Tam et al., EMNLP 2024 industry track,
   [arXiv 2408.02442](https://arxiv.org/pdf/2408.02442): stricter format constraints degraded reasoning; looser ones
   helped; strict formats helped classification). Terse style helps most where the output was padding, least where it
   was thinking.
2. **Edit payload scales with how much unchanged text the model must re-emit.** Whole-file output is O(file length)
   regardless of edit size. A diff is O(edit size plus anchor context). Hash anchors are O(edit size plus a few tag
   tokens). That is the real, mechanical source of savings, and it is large for big files and small edits.

## 3. Technique by technique

### 3.1 Terse-output prompts (Caveman and relatives)

- **Mechanism:** an instruction (skill, system prompt fragment or output-style) suppresses preamble, hedging,
  restating the question, and trailing summaries. Affects narration only; code, commands and tool arguments are
  unchanged by design.
- **Measured:**
  - Vendor headline: up to 65% (earlier 65 to 75%) fewer output tokens. **Claim.** The original baseline had no
    brevity instruction, which inflates the result.
  - JetBrains paired A/B on Claude Code (stock arm vs Caveman forced on, SkillsBench tasks, Sonnet at low effort):
    **8.5% fewer output tokens**, described by them as far below the advertised 65%. **Independent, paired.**
    Source: [JetBrains blog](https://blog.jetbrains.com/ai/2026/07/speak-to-ai-agents-like-cavemen-tosave-tokens/)
    (could not be fetched in this pass, 403; figure taken from search excerpts). Our notes say 82 tasks; the excerpts
    said 86 of 87. Check the task count before quoting it.
  - Other third-party runs: about 12% (coding tasks) to 23% vs vanilla, but only about **4% vs a plain one-line
    brevity instruction** (Guzik three-arm test, as summarized by
    [implicator.ai](https://www.implicator.ai/caveman-claude-code-skill-cuts-output-20-your-bill-barely-notices-2)).
    **Secondary.** Implication: most of the gain comes from *any* brevity instruction, not Caveman's specific wording.
  - Bill effect on a typical Claude Code bill: about 1% to 3% (same source).
- **Quality risk:** low at default intensity (no regressions reported in 72 and 30 prompt sets, secondary);
  "ultra" intensity showed occasional instruction-following failures on complex tasks. Risk is mainly that terse
  agents omit the explanation a reviewer needs in a TA draft summary.
- **Adoption cost:** near zero. A prompt fragment injected at `ta run` (we already inject CLAUDE.md context).
- **Licence:** Caveman repo licence not verified here (marked "see repo" in the spec). We do not need it: a
  two-sentence first-party fragment is equivalent, per the 4% finding above.
- **Fits our layer:** yes, prompt layer. Strongest candidate to author ourselves.

### 3.2 Hash-anchored edits ("Hashline", oh-my-pi)

- **What it actually is:** a change to the *edit tool interface*, not a prompt. When the agent reads or greps a file,
  each line is returned with a short content hash (for example `2:f1|  return "world";`). To edit, the model cites
  those tags (line plus hash) and supplies only the new text, instead of reproducing the old text it wants to replace.
  If the file changed since the read, the hashes no longer match and the edit is rejected before corruption.
- **Which output it reduces:** the **edit payload** (no retyping of old lines) and **retries** (no
  whitespace or context mismatch failures). It does not reduce narration or thinking. The tags cost a small amount of
  *input* (read results get slightly longer).
- **Licence:** oh-my-pi is **MIT** (copyright Mario Zechner, Can Bolukbasi/Bolük, Stencil Labs), with vendored
  third-party parts under their own licences (see its `THIRD-PARTY-NOTICES.txt`). The scheme itself is simple enough to
  reimplement; the idea is not patent-encumbered as far as we found (not a legal review).
  Source: [github.com/can1357/oh-my-pi](https://github.com/can1357/oh-my-pi).
- **Measured:** the only primary benchmark is the author's
  ([The Harness Problem](https://stencil.so/blog/the-harness-problem), redirect from blog.can.ac). Setup: 16 models,
  3 runs of 180 tasks each, synthetic bugs (mechanical mutations of a random React file, fixed by a plain-English
  description), only the edit tool varies, hashline vs patch format. **Author benchmark, no significance testing.**
  Output tokens vs the patch format, per model:

  | Model | Output tokens vs patch |
  |---|---|
  | Grok 4 Fast | -61% (the headline; best case) |
  | Grok Code Fast 1 | -49% |
  | MiniMax M2.1 | -42% |
  | GLM-4.7 | -32% |
  | Kimi K2.5 | -26% |
  | Claude Sonnet 4.5 | -24% |
  | Claude Haiku 4.5 | -22% |
  | Gemini 3 Flash | -21% |
  | Grok-4.1 Fast | -20% |
  | GLM-4.5 Air | -17% |
  | GPT-5.2 Codex | **+26%** |
  | DeepSeek V3.2 | **+20%** |

  Reading it honestly: **61% is the single best model, not typical.** For Claude models the same table shows 22% to
  24% on a synthetic one-line-fix task set. The article itself attributes Grok 4 Fast's drop partly to the model
  "stopping burning tokens on retry loops", so some of the saving is retry elimination, which is model-specific.
  Two models got worse. The blog also reports the pass-rate side (Grok Code Fast 1, 6.7% to 68.3%), which is a
  different claim. Our own spec notes a 24.4% patch-failure rate for Haiku with hashline and 0% for Sonnet, so
  per-model replay is required.
- **Quality risk:** medium and model-dependent. A mis-cited tag silently edits the wrong line only if the hash
  collides (2 to 3 character hashes can collide within a file; validation of line number plus hash makes this rare).
  Failure mode is rejection, which is safe but costs a retry.
- **Adoption cost:** high for TA specifically. TA mediates writes through `ta_fs_write` and stages diffs; hashline
  needs a *read tool* that emits tags and an *edit tool* that consumes them, and both must be the tools the agent
  uses. Claude Code's built-in Edit/Read cannot be changed by a hook. It works in OMP (built in) and would work for
  TA-governed agents only through an MCP `ta_fs_edit` tool plus agent instructions to prefer it.
- **Fits our layer:** tool-format layer, yes but only for agents that use TA's MCP tools. Not available as a hook
  over Claude Code's native tools.

OMP local check: `~/.local/bin/omp` is installed (MIT upstream). No hashline setting was found in
`~/.omp/agent/config.yml` in this pass (config read only, not modified). Whether the installed build has hashline on by
default was not verified; check `omp` docs before assuming the baseline for any OMP-side comparison.

### 3.3 Diff and patch edit formats vs whole-file rewrites

- **Mechanism:** edit payload shrinks from O(file) to O(change). Formats differ in how they locate the change:
  - **Whole file:** always applies, no locating, O(file) tokens. Aider notes it "uses a lot of tokens" and limits
    editable file size ([Aider edit formats](https://aider.chat/docs/more/edit-formats.html)).
  - **Search/replace blocks:** model reproduces the old text exactly, then the new text. O(old + new). Fragile to
    whitespace and stale-memory errors; failure triggers retries (token cost returns).
  - **Unified diff:** line-number-free hunks. Aider found that switching GPT-4 Turbo from search/replace to unified
    diff raised its "laziness" benchmark from 20% to 61% (89 tasks) and cut lazy placeholder outputs from 12 tasks to
    4 ([Aider, Dec 2023](https://aider.chat/docs/unified-diffs.html)). **Author benchmark, old model.** That
    is a quality gain, mostly about the model not eliding code.
  - **V4A `apply_patch` (OpenAI/Codex):** context-anchored patch grammar the GPT-5 family was trained on. No measured
    token comparison found (secondary articles only).
  - **Hashline:** see 3.2, removes the old-text echo entirely.
- **Measured (independent):** Diff-XYZ (JetBrains Research, 1,000 real commits, apply / anti-apply / generate;
  [arXiv 2510.12487](https://arxiv.org/abs/2510.12487)) found search/replace best overall for larger models, unified
  diff reliable but behind, verbose line-marker variants poor, and little format sensitivity for small open models.
  This measures *accuracy*, not tokens. No independent, direct token-count comparison of whole-file vs diff on modern
  models was found; the savings follow from the size arithmetic, which is solid, but the percentage depends on file size
  and edit size. For a 400-line file and a 5-line change the ratio exceeds 10x; for small files it is small.
- **Quality risk:** low to medium. Diff failures are visible (patch does not apply), whole-file failures are silent
  (model drops code). The retry cost of failures can erase the saving, so measure net.
- **Adoption cost:** Claude Code and OMP already default to string-replacement edit tools (diff-style). TA gains
  little here except where an agent is configured to rewrite whole files, which can be detected from the staged
  artifact sizes. Low cost to measure, low expected gain on Claude.
- **Licence:** formats are specs, not code. Aider is Apache-2.0; the V4A grammar is described in OpenAI docs.
- **Fits our layer:** tool-format layer for TA-owned tools; policy check ("this write replaced more than N% of a file
  with near-identical content") works as a hook.

### 3.4 Structured outputs and constrained decoding

- **Mechanism:** a grammar mask over the vocabulary at each decode step guarantees the output parses. Savings come from
  eliminating prose wrappers, markdown fences, retries after parse failures, and "let me explain" text. It does not
  by itself shorten the payload, and JSON key names and quoting add tokens versus plain text.
- **Measured:** no token-saving benchmark found. The quality evidence is mixed: strict formats hurt reasoning but
  help classification ([Tam et al., arXiv 2408.02442](https://arxiv.org/pdf/2408.02442)). Mitigations: put a
  free-text `reasoning` field first, or generate natural language then convert.
- **Quality risk:** medium for reasoning outputs, low for classification, routing and extraction.
- **Adoption cost:** low where TA calls a model directly (Advisor, classifier, review summaries). Not applicable to
  a delegated agent's tool loop, which is already structured by tool schemas.
- **Licence:** provider feature; open-source options exist (Outlines, llguidance, XGrammar: licences not verified here).
- **Fits our layer:** yes for TA's own LLM calls only. Worth doing for correctness more than for tokens.

### 3.5 max_tokens, stop sequences, effort and thinking budgets

- **max_tokens:** a hard cap, not a reduction technique. Truncation mid-edit corrupts work. Use only as a runaway
  guard, with a clear error ("hit max_tokens=N on turn T, raise via config X"), per the observability mandate.
- **Stop sequences:** cut generation after a sentinel (for example a closing tag). Useful when the tail is known
  padding. Low value for tool-using agents.
- **Effort / thinking budget:** thinking tokens are output tokens and are billed as such, so this is the largest
  lever *inside* output. Anthropic's effort parameter controls overall token spend including text, tool calls and
  thinking depth (per the [LiteLLM docs summary](https://docs.litellm.ai/docs/providers/anthropic_effort);
  model support changes, check Anthropic docs). Setting low effort for mechanical goals is the
  one output knob with a plausibly large effect on a thinking-heavy model, and the JetBrains Caveman test was itself run
  at low effort. Costs: lower effort can reduce quality on hard tasks, and the "effort vs tokens" curve for our
  workloads is unmeasured.
- **Research on reasoning length:** Chain of Draft (five-word step cap) reported 68% to 92% fewer reasoning tokens
  with equal or better accuracy on some benchmarks with older models, and arithmetic was an exception
  ([arXiv 2502.18600](https://arxiv.org/abs/2502.18600), numbers via secondary write-ups). Token-budget-aware prompting
  (TALE) shows a budget set too tight makes results worse and models often overshoot an impractical budget. Treat as
  **author benchmarks on reasoning QA, not on coding agents**.
- **Adoption cost:** low. Config per goal or role (`effort`, thinking budget). The policy decision (which phases
  deserve high effort) is the real work.
- **Fits our layer:** config/launch layer, yes. This is the best fit to TA's role/persona/tier model.

### 3.6 Predicted outputs and speculative decoding

- **Mechanism:** a draft (a prediction, or a small model) proposes tokens; the large model verifies many in one
  parallel pass. Lossless in distribution. It reduces **sequential forward passes (latency)**, not tokens generated.
- **Measured:** OpenAI Predicted Outputs: Simon Willison's test was faster (3.3 s vs 5.2 s) but produced more
  completion tokens (226 vs 118) because rejected prediction tokens are billed at completion rate
  ([Willison](https://simonwillison.net/2024/Nov/4/predicted-outputs/)). So it can *raise* cost. **Independent,
  single test.**
- **Verdict:** a latency feature. Not a token-reduction technique. Reject for the `OutputOptimizer` slot. Could be
  relevant only for a future local-model path, where it is a serving choice, not ours.

### 3.7 Other techniques worth listing

| Technique | Mechanism | Evidence | Note for TA |
|---|---|---|---|
| Cap the summary/explanation the agent writes at goal end | Final report and PR text are pure narration | none measured | Cheap and safe; also affects the TA draft summary reviewers read, so keep it informative |
| Don't echo tool results back | Agents often quote file contents they just read | none measured | Prompt line: "do not quote files or command output, refer by path:line" |
| Batch tool calls / parallel calls | Fewer turns means fewer re-sent assistant turns and fewer re-read context | indirect, via our own cost model | Larger lever than any output trick, because turns multiply context cost |
| Smaller/cheaper model for mechanical steps | Output priced per model | vendor price lists | Model-tier routing is already in TA's role design |
| Fine-grained tool streaming | Streams tool args early | latency only, may yield invalid JSON | Not a token saving ([Anthropic docs](https://platform.claude.com/docs/en/agents-and-tools/tool-use/fine-grained-tool-streaming)) |
| Fix the retry loop | Detect failed edit/test loops and stop or change approach | hashline article attributes part of its saving to this | Possibly the biggest hidden win: measure retries per goal (see section 5) |

## 4. Comparison table

| Technique | Mechanism | Measured saving | Evidence strength | Quality risk | Adoption cost | Licence |
|---|---|---|---|---|---|---|
| Terse-output prompt (Caveman) | Suppress narration | 8.5% (JetBrains paired A/B); 12% to 23% other runs; 65% to 75% vendor claim | Independent paired for 8.5%; rest secondary or claim | Low (default), medium (ultra) | Very low | Caveman: not verified; first-party fragment needs none |
| Plain one-line brevity instruction | Same | about 4% less than Caveman's extra gain, i.e. most of Caveman's win | Secondary (Guzik test) | Low | Very low | n/a |
| Hashline hash-anchored edits | No old-text echo, fewer retries | -22% to -24% on Claude (Sonnet 4.5, Haiku 4.5); -61% best model; +20% to +26% on two models | Author benchmark, synthetic bugs, no significance test | Medium, model dependent | High (needs TA-owned read and edit tools) | MIT (oh-my-pi) |
| Diff / search-replace vs whole file | Payload O(change) | Size-dependent, no independent token benchmark; quality shown by Aider (20% to 61% on laziness, old model) | Author (Aider), independent for accuracy (Diff-XYZ) | Low to medium | Low (already default in Claude Code/OMP) | Aider Apache-2.0; specs free |
| Structured outputs / constrained decoding | Remove wrappers, retries | None found | Quality only (Tam et al.) | Medium for reasoning | Low, only for TA's own LLM calls | Provider feature or OSS grammar libs |
| max_tokens / stop sequences | Hard cap | n/a (a guard) | n/a | High if it truncates edits | Very low | n/a |
| Effort / thinking budget | Fewer thinking tokens | Unmeasured for TA; CoD-style prompts 68% to 92% on reasoning QA | Author, non-coding | Medium on hard tasks | Low (config) | Provider feature |
| Predicted outputs / speculative decoding | Parallel verify | Negative on cost (226 vs 118 tokens in one test); latency gain | Independent, single test | None | Low | Provider feature |
| Retry/loop detection | Stop wasted turns | Not measured independently; part of hashline's claim | Weak | Low | Medium | First-party |

## 5. Recommendation for TA's `OutputOptimizer` slot

### 5.1 Build order

1. **Measure first (no new optimizer).** Extend `ta cost report` and the golden replay to record, per goal:
   output tokens split into thinking, assistant text, and tool-call arguments (edit payload), edit-tool call counts,
   failed-edit/retry counts, and whole-file writes. This tells us which of the five sources in section 2 actually
   dominates *our* output. Without it every choice below is a guess. Cost: small; all fields exist in the transcript
   or runtime usage records.
2. **`terse` fragment (first implementation of `OutputOptimizer`, kind `hook`/prompt).** Author a short first-party
   fragment (about 60 tokens): no preamble or recap, no quoting of file or command output, answer in the fewest
   sentences that stay actionable, keep code, commands and the final draft summary complete. Do not adopt
   Caveman's persona, because the evidence says a plain brevity instruction gets most of the gain and the persona
   adds quality risk. Licence-free, config-selectable, `none` is the baseline.
3. **Effort/thinking-budget policy per role or phase kind** (config, not a new optimizer). Likely the biggest
   in-output lever for thinking-capable models; replay-test it before enabling by default.
4. **Edit-format audit rather than hashline.** Add a detector for whole-file rewrites of mostly unchanged files and for
   edit-retry loops (policy/hook layer). Only if the step 1 data shows edit payload or retries are a large share,
   prototype a hash-anchored `ta_fs_edit` MCP tool as a second `OutputOptimizer` implementation. Expect about 20% to 25%
   fewer output tokens on Claude models at best, and treat the 61% figure as marketing for planning purposes.
5. **Skip:** predicted outputs, speculative decoding, fine-grained tool streaming, strict constrained decoding for
   agent loops. Use structured outputs only in TA's own classifier/advisor calls for correctness.

Rationale: items 1 to 3 are cheap, reversible, and fit the layers TA already controls. Hashline is the technique with
the most mechanical upside, but it needs TA-owned tools and has mixed per-model results, so it must earn its place with
data.

### 5.2 Contract fit

Each implementation obeys the spec's contract: `name`, `version`, `kind`, `licence`, `available()`, and a metrics
record. For output the metrics record cannot be bytes in/out; it should carry `output_tokens_by_class`,
`edit_calls`, `edit_failures`, `whole_file_writes`, plus `lossless: false` for `terse` (it changes behaviour) and
`original_ref` pointing to the baseline replay it is compared with.

### 5.3 How to measure with golden-set replay

- Use the existing `ta golden replay <id> --set optimizers.output.use=terse --tag terse` path. `none` is baseline.
- **Selection:** 6 to 10 golden entries per spec 4.3, deliberately including (a) a read-heavy investigation (narration
  dominated), (b) a multi-file edit on large files (edit payload dominated), (c) a hard reasoning task (to catch
  quality loss). Prefer goals with objective files and a `check_command`.
- **Primary metric:** output tokens per goal (runtime-reported, not table-estimated), split by class. Secondary:
  total cost, turns, wall-clock time, edit failures.
- **Noise:** LLM runs vary run to run. The JetBrains effect (8.5%) is the size of plausible run-to-run noise on a
  single replay. Therefore: run **baseline twice** to estimate noise on each entry, and treat any output-token delta
  smaller than the baseline-versus-baseline spread as no effect. Only claim a saving if it is consistent in sign
  across entries (a simple paired sign test over 8 entries, 7 of 8 same sign, is a workable bar).
- **Quality gate:** the spec's equivalence verdict (`check_command` passes and diff similarity above threshold). Anything
  `differs` goes to human review, and the optimizer cannot be made default if any hard-reasoning entry flips from
  `equivalent` to `differs` or `failed`.
- **Per-model:** repeat for each model tier TA uses. Hashline-style results flip sign by model.
- **Decision rule:** ship an output optimizer by default only if it shows at least 10% fewer output tokens with no
  quality regression across the set. Anything smaller (1% to 3% of total cost) is not worth the surface area; keep it
  available but off.

## 6. Open items

- Confirm the JetBrains task count (82 vs 86/87) and whether it used Sonnet 5 at low effort, by reading the post
  directly (blocked by 403 in this pass).
- Verify Caveman's licence from its repo if any of its text is to be reused.
- Check whether OMP's installed build enables hashline by default, to know the baseline when replaying OMP goals.
- Confirm current Anthropic effort-parameter model support from official docs before wiring a config key.
- Find or run an independent token-count comparison of whole-file vs diff edits on current Claude models; none was
  found.
