# TA v0.6 — User Workflow Guide

> A realistic walkthrough of building a product with TA after v0.6 ships.
> Written for someone who has never used TA before.

---

## What you're building

You're building **Mealplan** — a Rust web service that generates weekly meal plans. It has a REST API, a Postgres database, and you want an AI agent to do the implementation while you retain full control over what ships.

This guide walks through the real workflow, start to finish.

---

## 1. Install and initialize

```bash
# Install TA
cargo install ta-cli

# Initialize TA in your project
cd ~/projects/mealplan
ta init
```

This creates a `.ta/` directory:

```
.ta/
  config.yaml       # mediators, channels, agent defaults
  policy.yaml       # what agents can and cannot do
  audit.jsonl       # append-only event log (auto-created)
```

---

## 2. Configure your project

### Register your agent framework

```yaml
# agents/claude-code.yaml
name: claude-code
command: claude
args: ["--project-root", "{workspace_path}"]
shell: bash
```

This tells TA how to launch Claude Code. You can register multiple frameworks — Claude Code, Codex, Claude Flow, a LangGraph script — and choose which one to use per goal.

### Set project policy

```yaml
# .ta/policy.yaml
defaults:
  enforcement: checkpoint    # agent works freely, human reviews at draft
  auto_approve:
    read_only: true          # reads never need approval

schemes:
  fs:
    approval_required: [apply, delete]

escalation:
  - new_dependency
  - security_sensitive
  - budget_exceeded

budget:
  max_tokens_per_goal: 500000
```

This says: the agent can read anything, but file writes are held for your review. New dependencies and security-sensitive changes trigger escalation. Budget caps token spend per goal.

### Link credentials (optional, for later when you add database access)

```bash
ta credentials store pg-dev \
  --type connection_string \
  --value "postgres://dev:secret@localhost/mealplan_dev"
```

The agent never sees the raw connection string. TA brokers a scoped session token.

---

## 3. Give the agent your first goal

```bash
ta run "Set up the project skeleton: Cargo workspace, axum web server, sqlx for Postgres, basic health check endpoint" \
  --source . \
  --agent claude-code
```

What happens:

1. TA copies your project to an isolated staging workspace (`.ta/staging/<id>/`)
2. TA injects a `CLAUDE.md` into the staging copy with your policy, memory context, and instructions to write a `change_summary.json` with rationale before exiting
3. TA launches Claude Code as a subprocess against the staging workspace
4. Claude Code works — reads files, creates files, runs commands. **TA is invisible to it.**
5. Claude Code finishes and exits (process terminates)
6. TA diffs the staging workspace against your source
7. TA builds a draft from the diff and notifies you

The agent never calls TA. It doesn't know TA exists. It just works in a directory and exits. TA observes, diffs, and captures.

---

## 4. Review the draft

TA built a draft from the agent's work. Review it:

```bash
ta draft view
```

```
Draft — "Set up project skeleton"
Status: PendingReview
Agent: claude-code
Files changed: 8

  ✎ Cargo.toml                            — Added workspace members
  + crates/mealplan-api/Cargo.toml         — New crate with axum, sqlx, tokio deps
  + crates/mealplan-api/src/main.rs        — Axum server on :3000 with health route
  + crates/mealplan-api/src/routes/mod.rs   — Route module
  + crates/mealplan-api/src/routes/health.rs — GET /health returns 200
  + migrations/001_initial_schema.sql       — Creates meals, plans, plan_items tables
  + .sqlx/                                  — SQLx offline query data
  + docker-compose.yaml                     — Postgres 16 dev container

Rationale:
  Approach: Axum + SQLx for async Postgres, modular crate layout
  Alternatives considered:
    - Actix-web — heavier, more middleware ecosystem, but overkill here
    - Warp — filter-based routing is harder to read for CRUD
  Tradeoffs: Axum is lighter, composes with Tower, good sqlx integration

Escalation: new_dependency
  axum 0.7.9, sqlx 0.8.3, tokio 1.41.0
```

No IDs to remember. `ta draft view` always shows the latest draft. The rationale section shows you *why* the agent made the choices it did.

### Drill into a specific file

```bash
ta draft view --file migrations/001_initial_schema.sql
```

```sql
CREATE TABLE meals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    cuisine TEXT,
    prep_time_minutes INT,
    calories INT,
    created_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    week_start DATE NOT NULL,
    created_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE plan_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id UUID REFERENCES plans(id),
    meal_id UUID REFERENCES meals(id),
    day_of_week INT CHECK (day_of_week BETWEEN 0 AND 6),
    meal_type TEXT CHECK (meal_type IN ('breakfast', 'lunch', 'dinner'))
);
```

---

## 5. Reject with feedback — conversational continuity

You like the structure but want a `dietary_tags` column on meals. You don't approve — you reject with feedback:

```bash
ta draft reject "The meals table needs a dietary_tags TEXT[] column for filtering (vegan, gluten-free, etc.). Add it to the migration and update any query code that touches meals."
```

What happens:

1. TA records your feedback
2. TA relaunches Claude Code with a new `CLAUDE.md` that includes:
   - The original goal
   - What the agent did last time (the full change summary)
   - Your rejection reason and feedback
   - Memory from the previous iteration
3. Claude Code picks up where it left off — it knows what it built, knows what you want changed, and revises

From your perspective, this is one conversation. You said "build this," the agent proposed something, you said "change the schema," the agent revises. You never re-explain the project, re-state the goal, or manage session IDs.

When the agent exits again, TA diffs and builds a new draft:

```bash
ta draft view
```

```
Draft — "Set up project skeleton (revision 2)"
Status: PendingReview
Files changed: 8
  ✎ migrations/001_initial_schema.sql — Added dietary_tags TEXT[] column to meals
  (7 other files carried forward from revision 1)

Context: Revision after human feedback —
  "The meals table needs a dietary_tags TEXT[] column..."
```

This time it looks right. Approve:

```bash
ta draft approve
ta draft apply
```

Changes land in your real project directory. Staging workspace is cleaned up.

---

## 6. Next goal — memory carries forward

Start the next piece of work:

```bash
ta run "Implement CRUD endpoints for meals: POST, GET, GET by id, PUT, DELETE. Use sqlx." \
  --source . \
  --agent claude-code
```

TA launches a new session. But the agent receives context from the previous session through TA's memory module:

- The schema (including `dietary_tags` — it knows about your revision)
- The project structure (Cargo workspace layout, route organization)
- Conventions you established (what you approved, what you changed)

The agent doesn't start from scratch. It continues building on the foundation you already approved.

When it finishes, review and approve as before:

```bash
ta draft view
ta draft approve
ta draft apply
```

---

## 7. Reject a specific file, approve the rest

The next goal is the meal plan generation algorithm. The agent finishes, you review:

```bash
ta draft view
```

```
Draft — "Meal plan generation algorithm"
Files changed: 4
  + crates/mealplan-api/src/routes/generate.rs — POST /generate endpoint
  + crates/mealplan-api/src/planner.rs          — Greedy algorithm: fills week by calorie target
  ✎ crates/mealplan-api/src/routes/mod.rs       — Added generate route
  ✎ crates/mealplan-api/src/main.rs             — Registered generate routes

Rationale:
  Approach: Greedy fill — iterate days, pick meals matching dietary tags
    that bring daily calories closest to target
  Alternatives considered:
    - Constraint solver (OR-tools) — powerful but heavy dependency
    - Random selection with retry — simple but poor meal variety
  Tradeoffs: Greedy is fast and predictable but may not optimize variety
```

You like the endpoint and routing changes, but the greedy algorithm is too simple. You want the constraint solver approach. Reject with specific feedback:

```bash
ta draft reject "The endpoint structure and routing are good — keep those. But the greedy planner is too simple. Use a constraint-based approach: define variety, nutrition, and dietary constraints and use a backtracking solver. No need for OR-tools — a simple Rust backtracking solver is fine."
```

TA relaunches with your feedback. The agent knows to keep the routing but rewrite the planner. Next draft:

```bash
ta draft view
```

```
Draft — "Meal plan generation algorithm (revision 2)"
Files changed: 2
  ✎ crates/mealplan-api/src/planner.rs   — Backtracking constraint solver
  + crates/mealplan-api/src/constraints.rs — Variety, nutrition, dietary constraint types

  (generate.rs, routes/mod.rs, main.rs unchanged from revision 1)

Rationale:
  Approach: Backtracking solver with constraint propagation
  Change from revision 1: Human requested constraint-based approach
    instead of greedy. Kept endpoint structure per feedback.
```

Approve and apply.

---

## 8. Switch agent frameworks between goals

The implementation is done. Now you want a code review from a different perspective. Start a new goal with a different agent:

```bash
ta run "Review the codebase for security issues, error handling gaps, and SQL injection risks. Write a report to REVIEW.md." \
  --source . \
  --agent codex
```

TA launches Codex instead of Claude Code. Codex gets the same memory (project context, schema, conventions) through its own context injection. It works in a staging copy, exits, TA builds a draft.

```bash
ta draft view
```

```
Draft — "Security review"
Files changed: 1
  + REVIEW.md — Security review report: 2 findings, 3 recommendations

Rationale:
  Approach: Static analysis of all SQL queries, input validation paths,
    and error handling patterns
  Findings:
    - planner.rs: meal_id passed to SQL without parameterization (line 47)
    - generate.rs: missing input validation on calorie_target (accepts negative)
```

You approve the review, then fix the issues with another Claude Code goal:

```bash
ta draft approve
ta draft apply

ta run "Fix the two security issues identified in REVIEW.md: parameterize the SQL query in planner.rs and add input validation for calorie_target in generate.rs." \
  --source . \
  --agent claude-code
```

The agent sees the review report in the project files and in memory. It knows exactly what to fix.

---

## 9. Use an orchestrator for complex goals

For a goal that needs multiple agents coordinating internally (coder + reviewer + supervisor loop), use an orchestrator:

```yaml
# agents/claude-flow.yaml
name: claude-flow
command: claude-flow
args: ["--workflow", "feature-dev", "--project", "{workspace_path}"]
```

```bash
ta run "Add a subscription system: users can subscribe to weekly meal plan delivery via email. Include the API endpoints, email templates, and scheduling logic." \
  --source . \
  --agent claude-flow
```

TA launches Claude Flow as a subprocess — just like it launches Claude Code or Codex. Claude Flow internally coordinates a coder agent and a reviewer agent, iterates until its supervisor is satisfied, then exits.

TA doesn't know or care about the internal coordination. It sees one process, one staging workspace, one diff when the process exits.

```bash
ta draft view
```

The draft shows the final result of however many internal iterations Claude Flow ran. You review the output, not the internal process.

---

## 10. Policy in action

### Escalation — real-time notification

When the agent does something policy flags, you see it in the event stream and in the draft:

```
Draft — "Add email notifications"
Files changed: 5

Escalations:
  ⚠ new_dependency — lettre 0.11 (email sending crate)
  ⚠ security_sensitive — SMTP credentials referenced in config

  Review these carefully before approving.
```

### Budget enforcement

If an agent exceeds its token budget, TA terminates the process:

```bash
ta draft view
```

```
Draft — "Implement recommendation engine" (INCOMPLETE)
Status: BudgetExceeded
Budget: 500,000 / 500,000 tokens

The agent was stopped at the budget limit. Partial changes are in the draft.
You can:
  ta draft approve    — apply partial changes
  ta draft reject     — discard and try a different approach
  ta run "..." --budget 750000  — retry with higher budget
```

### Policy cascade for production

When deploying, tighten policy:

```yaml
# .ta/workflows/deploy.yaml
defaults:
  enforcement: supervised
escalation:
  - any_external_call
```

```bash
ta run "Generate Dockerfile and K8s manifests for production" \
  --source . \
  --agent claude-code \
  --workflow deploy
```

The workflow policy layers on top of your project policy. It can only add restrictions, never remove them.

---

## 11. The human control plane

While an agent is running, you have commands it cannot see:

```bash
ta session status        # what's the agent doing right now?
ta session pause         # pause agent execution
ta session resume        # resume after pause
ta audit trail           # what happened across all sessions
ta context list          # agent memory (what it learned)
ta context search "schema decisions"  # semantic memory search
```

These go through TA's human control plane. The agent talks to a different endpoint (the MCP gateway). The agent cannot see, intercept, or influence your commands. This is the safety boundary.

---

## 12. End of day — everything persists

After a day of work:

```
Goals completed: 5
  1. ✅ Project skeleton (2 iterations — added dietary_tags)
  2. ✅ CRUD endpoints
  3. ✅ Meal plan generator (2 iterations — switched to constraint solver)
  4. ✅ Security review (Codex)
  5. ✅ Security fixes

Drafts: 8 submitted, 7 approved, 1 partial (budget)
Files changed: 19
Audit events: logged in .ta/audit.jsonl
Memory: persisted for next session
```

Tomorrow, any goal you start inherits memory from today. The agent knows your project, your conventions, and your preferences.

---

## The conversation loop

This is the core experience. Every goal follows the same loop:

```
You: "Build X"
  │
  TA launches agent → agent works → agent exits
  TA diffs → builds draft → notifies you
  │
  ├─ You: ta draft view        → see what changed and why
  │
  ├─ You: ta draft approve     → changes land in your project
  │       ta draft apply          done. start next goal.
  │
  └─ You: ta draft reject      → "Change Y, keep Z, try approach W"
          "feedback here"
          │
          TA relaunches agent with full context + your feedback
          Agent revises → exits → TA builds new draft
          │
          └─ (loop continues until you approve or abandon)
```

From your perspective, each goal is a conversation:
- You say what you want
- The agent proposes a solution with rationale
- You approve, or you say what to change
- The agent revises with full context of the conversation so far
- No IDs to track, no sessions to manage, no context to re-explain

TA handles the stitching — memory, context injection, staging, diffing, drafts, policy. You just talk about the work.

---

## Concept summary

| Concept | What it means |
|---|---|
| **Goal** | A unit of work: "build X." One agent, one goal at a time. |
| **Draft** | The agent's proposed changes, captured by TA when the agent exits. Held for your review. |
| **Reject + feedback** | You say what to change. TA relaunches the agent with full context. Feels like a conversation. |
| **Memory** | What the agent learned — persists across goals and sessions. Next agent picks up where the last left off. |
| **Policy** | YAML rules for what agents can do. Layers stack (project → workflow → agent) and can only tighten. |
| **Escalation** | Policy-triggered flags shown in the draft. New dependencies, security issues, budget warnings. |
| **Human control plane** | Your commands (`ta session status`, `ta draft view`, etc.) — invisible to the agent. The safety boundary. |
| **Agent framework** | Claude Code, Codex, Claude Flow, LangGraph — TA launches any of them as a subprocess. Orchestrators are just another framework. |
| **Audit trail** | Hash-chained log of every action, decision, and review across all sessions. |

---

## What's different from before v0.6

| Before (v0.5) | After (v0.6) |
|---|---|
| `ta run` → agent exits → manually run `ta draft build` | Agent exits → TA auto-diffs and builds draft |
| Reject means start over from scratch | Reject with feedback → agent relaunches with full context |
| No visibility into agent reasoning | `change_summary.json` with rationale, alternatives, tradeoffs |
| File-only mediation | Any resource via `ResourceMediator` trait (files, APIs, databases) |
| Scattered policy across code | `.ta/policy.yaml` with cascade: project → workflow → agent (tighten only) |
| No session commands while agent runs | Human control plane: `ta session status/pause/resume` |
| Each goal is isolated | Memory carries context across goals and sessions |
