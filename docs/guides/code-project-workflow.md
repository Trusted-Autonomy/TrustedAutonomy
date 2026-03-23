# Code Project Workflow Sample

A complete multi-stage workflow that takes a goal from plan through code review, security review, and constitution check — then automatically branches back to the agent on failure or advances to draft review on success.

---

## Overview

```
Plan ──▶ Build ──▶ ┌── Code Review ───┐
                   ├── Security Review ─┤ ──▶ Branch: pass → Draft Review
                   └── Constitution ───┘            fail → Agent Retry
```

This pattern uses three things:
- **Workflow files** (`.ta/workflows/`) — named pipelines with steps and branches
- **Agent manifests** (`.ta/agents/`) — role-specific agents that specialise each review
- **`ta workflow run`** — trigger a workflow by name with a goal as input

---

## File Layout

```
.ta/
  workflows/
    code-project.toml        # main pipeline
    review-parallel.toml     # parallel review sub-workflow
  agents/
    code-reviewer.toml       # code quality + architecture review
    security-reviewer.toml   # OWASP, secrets, CVE scan
    constitution-reviewer.toml  # constitution fit check
```

---

## Workflow: `code-project.toml`

```toml
# .ta/workflows/code-project.toml
# Main pipeline: plan → build → parallel review → branch

name = "code-project"
description = "Full code project pipeline with parallel review gate"
version = "1.0"

[[steps]]
id = "plan"
type = "goal"
title = "Plan: {input.title}"
prompt = """
Review the goal below and produce a step-by-step implementation plan.
Output a PLAN.md section (markdown checklist) and stop — do not write code.

Goal: {input.description}
"""
agent = "default"
output = "plan_output"

[[steps]]
id = "build"
type = "goal"
title = "Build: {input.title}"
prompt = """
Implement the plan below. Write production-quality code with tests.
Run `./dev cargo test --workspace` before finishing and confirm all tests pass.

Plan:
{plan_output}

Original goal: {input.description}
"""
agent = "default"
depends_on = ["plan"]
output = "build_draft_id"

[[steps]]
id = "review"
type = "workflow"
workflow = "review-parallel"
input = { draft_id = "{build_draft_id}", title = "{input.title}" }
depends_on = ["build"]
output = "review_result"

[[steps]]
id = "gate"
type = "branch"
depends_on = ["review"]

  [[steps.branches]]
  condition = "review_result.all_passed == true"
  next = "draft-review"

  [[steps.branches]]
  condition = "review_result.all_passed == false"
  next = "retry-build"

[[steps]]
id = "retry-build"
type = "goal"
title = "Fix review findings: {input.title}"
prompt = """
The parallel review found issues. Address every finding below, then re-run
`./dev cargo test --workspace` to confirm all tests pass.

Review findings:
{review_result.findings}

Original draft ID: {build_draft_id}
"""
agent = "default"
output = "retry_draft_id"
# After retry, loop back to review
next = "review"

[[steps]]
id = "draft-review"
type = "notify"
message = """
All reviews passed for "{input.title}".

Draft ID: {build_draft_id}
Run `ta draft view {build_draft_id}` to inspect, then `ta draft approve {build_draft_id}` to apply.
"""
```

---

## Sub-Workflow: `review-parallel.toml`

```toml
# .ta/workflows/review-parallel.toml
# Three parallel review agents; aggregates pass/fail

name = "review-parallel"
description = "Parallel code, security, and constitution review"
version = "1.0"

# All three steps run concurrently (no depends_on between them)
[[steps]]
id = "code-review"
type = "goal"
title = "Code review: {input.title}"
agent = "code-reviewer"
prompt = """
Review the draft below for code quality, architecture, and test coverage.
Draft ID: {input.draft_id}

Use `ta draft view {input.draft_id}` to read the diff.

Output JSON:
{ "passed": true|false, "findings": ["..."] }
"""
output = "code_result"

[[steps]]
id = "security-review"
type = "goal"
title = "Security review: {input.title}"
agent = "security-reviewer"
prompt = """
Review the draft below for security issues (OWASP Top 10, secrets, CVEs, unsafe code).
Draft ID: {input.draft_id}

Use `ta draft view {input.draft_id}` to read the diff.

Output JSON:
{ "passed": true|false, "findings": ["..."] }
"""
output = "security_result"

[[steps]]
id = "constitution-review"
type = "goal"
title = "Constitution check: {input.title}"
agent = "constitution-reviewer"
prompt = """
Evaluate the draft against the project constitution (.ta/constitution.toml or TA-CONSTITUTION.md).
Draft ID: {input.draft_id}

Use `ta draft view {input.draft_id}` to read the diff.

Output JSON:
{ "passed": true|false, "findings": ["..."] }
"""
output = "constitution_result"

# Aggregator — runs after all three complete
[[steps]]
id = "aggregate"
type = "aggregate"
depends_on = ["code-review", "security-review", "constitution-review"]
aggregate_fn = """
all_passed = code_result.passed and security_result.passed and constitution_result.passed
findings = code_result.findings + security_result.findings + constitution_result.findings
return { all_passed: all_passed, findings: findings }
"""
output = "review_result"
```

---

## Agent Manifests

### `code-reviewer.toml`

```toml
# .ta/agents/code-reviewer.toml
name = "code-reviewer"
description = "Code quality and architecture reviewer"
framework = "claude"

[system_prompt]
role = "senior engineer and code reviewer"
focus = """
- Correctness: logic errors, off-by-one, null handling
- Architecture: does the design fit the existing codebase patterns?
- Test coverage: are the happy path, error paths, and edge cases covered?
- Readability: naming, comments, unnecessary complexity
- Performance: obvious bottlenecks or excessive allocations
"""
output_format = "JSON with keys: passed (bool), findings (string[])"

[constraints]
max_tokens = 4096
no_file_writes = true   # read-only review — no staging modifications
```

### `security-reviewer.toml`

```toml
# .ta/agents/security-reviewer.toml
name = "security-reviewer"
description = "Security-focused code reviewer (OWASP, secrets, CVEs)"
framework = "claude"

[system_prompt]
role = "application security engineer"
focus = """
- OWASP Top 10: injection, broken auth, XSS, insecure deserialization, etc.
- Secrets & credentials: API keys, tokens, passwords in code or config
- Dependency CVEs: flag crates/packages with known vulnerabilities
- Unsafe Rust: `unsafe` blocks, raw pointer arithmetic, transmute
- Command injection: shell exec with user-controlled input
- Path traversal: unvalidated file paths from external input
"""
output_format = "JSON with keys: passed (bool), findings (string[])"

[constraints]
max_tokens = 4096
no_file_writes = true
```

### `constitution-reviewer.toml`

```toml
# .ta/agents/constitution-reviewer.toml
name = "constitution-reviewer"
description = "Checks draft against project constitution and design principles"
framework = "claude"

[system_prompt]
role = "technical product reviewer"
focus = """
- Does the change fit the project's stated mission and scope?
- Does it follow the architectural principles in TA-CONSTITUTION.md?
- Does it introduce tech debt the constitution explicitly prohibits?
- Does the naming, abstraction level, and API surface match project conventions?
"""
output_format = "JSON with keys: passed (bool), findings (string[])"

[constraints]
max_tokens = 2048
no_file_writes = true
```

---

## Running the Workflow

```bash
# Trigger the full pipeline for a new feature
ta workflow run code-project \
  --input title="Add ValidationLog to DraftPackage" \
  --input description="Implement ValidationLog struct in ta-changeset, populate it in run.rs after agent exit, display in ta draft view, gate ta draft approve."

# Watch progress
ta workflow status --latest

# When it reaches draft-review:
ta draft view <draft-id>
ta draft approve <draft-id>
```

---

## Triggering From an Existing Goal

If you already have a running or completed goal, you can feed its draft into the review sub-workflow directly:

```bash
# Run just the parallel review on an existing draft
ta workflow run review-parallel \
  --input title="My feature" \
  --input draft_id="3e897676"

# See aggregate result
ta workflow status --latest --json | jq '.steps.aggregate.output'
```

---

## Customising the Gate

The `review-parallel` sub-workflow can be used standalone or the gate logic can be tightened:

```toml
# Require ALL three reviews to pass (default above)
condition = "review_result.all_passed == true"

# Allow code review to be advisory only (security + constitution must pass)
condition = "review_result.security_passed == true and review_result.constitution_passed == true"

# Block on any HIGH-severity security finding, warn on others
condition = "not review_result.findings.any(f => f.severity == 'HIGH')"
```

---

## Adding More Review Types

To add a new review (e.g., performance profiling):

1. Create `.ta/agents/perf-reviewer.toml`
2. Add a step to `review-parallel.toml` (no `depends_on` — it runs in parallel)
3. Update the `aggregate` step to include the new result

---

## Notes

- The `review-parallel` sub-workflow spawns three concurrent goals — they run in parallel if the daemon has worker capacity.
- Each reviewer is read-only (`no_file_writes = true`) — they cannot modify staging.
- The retry loop in `code-project.toml` feeds review findings back to the agent as context. Cap the retry depth if needed by adding a `max_retries` field (planned for v0.14.x).
- This sample uses features that are partially pending (workflow branching, sub-workflows, `no_file_writes` constraint). Check `ta workflow --help` and PLAN.md v0.14.x sections for availability status.
