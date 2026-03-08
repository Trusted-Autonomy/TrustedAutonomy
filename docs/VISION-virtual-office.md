# Virtual Office & Meta-Agent Orchestration — Vision

> How TA fits into a world of autonomous agent teams, event-driven workflows,
> and human-in-the-loop approval across email, Slack, Discord, and beyond.

---

## The Core Insight

**TA is the safety substrate, not the orchestrator.**

TA's unique value is the staging -> review -> approve -> apply loop. It mediates
*every* agent action against the real world. The orchestration of *which* agents
run, *when* they run, and *what* roles they play is a separate concern that
lives above TA.

```
┌─────────────────────────────────────────────────────────┐
│                    Event Sources                        │
│  Email  │  Slack  │  Discord  │  Cron  │  Webhooks     │
└────────────────────────┬────────────────────────────────┘
                         │ triggers
┌────────────────────────▼────────────────────────────────┐
│               Orchestration Layer                       │
│                                                         │
│  "Virtual Office Manager"                               │
│  - Role registry (who does what)                        │
│  - Plan execution (step through phases)                 │
│  - Event routing (which role handles which trigger)     │
│  - Agent spawning + lifecycle                           │
│                                                         │
│  Implementations:                                       │
│    Claude Flow  │  clawdbot  │  Custom  │  n8n/etc.     │
└────────────────────────┬────────────────────────────────┘
                         │ spawns agents with goals
┌────────────────────────▼────────────────────────────────┐
│                TA Mediation Layer                        │
│                                                         │
│  ta run → staging copy → agent works → ta pr build      │
│  - Every action mediated through staging                │
│  - Every change requires human approval                 │
│  - Audit trail for all agent activity                   │
│  - Policy engine controls capabilities per agent/role   │
│  - Connectors scope what each agent can touch           │
│    (fs, email, drive, slack, db, etc.)                  │
└────────────────────────┬────────────────────────────────┘
                         │ PR ready → fire event
┌────────────────────────▼────────────────────────────────┐
│              Notification / Approval UX                  │
│                                                         │
│  - CLI:     ta pr view/approve (current)                │
│  - Email:   PR summary → reply "approve" / "reject"    │
│  - Slack:   PR card with approve/reject buttons         │
│  - Discord: same pattern via clawdbot                   │
│  - Web UI:  localhost → LAN → cloud (Phase 8)          │
│  - Mobile:  responsive web (Phase 8)                    │
└─────────────────────────────────────────────────────────┘
```

---

## Where Each Project Fits (No Duplication)

| Concern | Owner | TA's Role |
|---|---|---|
| **Safety mediation** (staging, review, apply) | TA | This IS TA |
| **Agent coordination** (spawn, monitor, retry) | Claude Flow / orchestrator | TA is a tool the orchestrator calls |
| **Discord bot interface** | clawdbot | Calls `ta run`, surfaces PRs in Discord |
| **Slack/Email notifications** | TA notification connectors | TA fires events, connectors deliver |
| **Plan execution** (step through phases) | Orchestrator reads TA's PLAN.md | TA tracks plan state, orchestrator drives |
| **Role definitions** (PM, dev, assistant) | Orchestrator config | TA enforces per-role policies |
| **Capability scoping** (what agent X can do) | TA policy engine | Already built (Phase 1) |
| **Event-driven triggers** | TA event system + external | TA fires lifecycle events, external systems trigger goals |

### The Boundary Rule

> If it's about **what agents can do and whether humans approve** → TA.
> If it's about **which agents to run and when** → Orchestrator.
> If it's about **how humans interact** → Notification/UX layer.

---

## TA as a Tool (The Integration Surface)

The orchestrator doesn't need TA internals. It needs a clean API:

```bash
# Programmatic: orchestrator creates a goal and gets a PR back
ta run "Draft reply to urgent email from Alice" \
  --agent claude-code \
  --source ./email-workspace \
  --phase email-triage \
  --no-launch                    # just set up staging

# Then the orchestrator launches the agent itself,
# or lets ta launch it:
ta run "Draft reply" --source ./email-workspace

# Orchestrator polls or gets notified when PR is ready:
ta pr list --goal <id> --json    # machine-readable output

# Human approves (via CLI, Slack, email, etc.):
ta pr approve <id>

# Orchestrator picks up next step:
ta pr apply <id>
```

### What TA Needs to Expose

1. **JSON output mode** (`--json` on all commands) for programmatic consumption
2. **Event hooks** (already designed in `plugins-architecture-guidance.md`):
   - `on_pr_generated` → trigger notification
   - `on_pr_approved` → trigger next plan step
   - `on_goal_completed` → update plan, spawn next goal
3. **Webhook/callback** endpoint: POST to a URL on state transitions
4. **Non-interactive approval API**: approve/reject via token (for Slack buttons, email replies)

---

## Virtual Office: Role-Based Agent Teams

A "virtual office" is a set of **roles** with purpose, triggers, and TA scope.

### Role Definition (Conceptual Schema)

```yaml
# ~/.config/ta/offices/dev-team.yaml
office: dev-team
description: "Development team for Trusted Autonomy"

roles:
  project-manager:
    purpose: "Drive plan execution, create goals for next phases"
    triggers:
      - event: on_pr_applied        # previous phase done → start next
      - schedule: "0 9 * * MON"     # weekly check-in
    agent: claude-code
    capabilities:
      - "fs://workspace/PLAN.md"    # can read/update plan
      - "fs://workspace/docs/**"    # can update docs
    notification_channel: slack:#dev-team

  developer:
    purpose: "Implement features and fixes per plan phases"
    triggers:
      - event: goal_assigned         # PM creates a goal for this role
    agent: claude-code
    capabilities:
      - "fs://workspace/**"          # full codebase access
    notification_channel: slack:#dev-prs

  email-assistant:
    purpose: "Monitor inbox, draft responses for review"
    triggers:
      - event: new_email             # external: email webhook
      - schedule: "*/15 * * * *"     # poll every 15 min
    agent: claude-code
    capabilities:
      - "gmail://inbox/read"
      - "gmail://drafts/write"       # can draft, not send
    notification_channel: email:me@example.com

  social-monitor:
    purpose: "Monitor mentions, draft responses"
    triggers:
      - event: new_mention           # external: social webhook
    agent: claude-code
    capabilities:
      - "twitter://mentions/read"
      - "twitter://replies/draft"
    notification_channel: discord:#social
```

### How a Role Executes

1. **Trigger fires** (cron, webhook, event from TA)
2. **Orchestrator** receives trigger, looks up role config
3. **Orchestrator calls TA**: `ta run "<goal>" --agent <agent> --source <workspace>`
4. **Agent works** in TA staging with scoped capabilities
5. **TA builds PR** when agent exits
6. **TA fires `on_pr_generated`** event
7. **Notification connector** sends PR summary to configured channel
8. **Human reviews** via that channel (Slack button, email reply, CLI)
9. **TA fires `on_pr_approved`** event
10. **Orchestrator** picks up next step (apply, spawn next role, etc.)

---

## Fluid UX: Approval Wherever You Are

The current UX is CLI-only. The fluid version lets humans approve from anywhere:

### Email Flow
```
From: ta-notifications@mysetup.local
Subject: [TA] PR ready: "Draft reply to Alice re: Q3 budget"

3 files changed in email-workspace:
  + drafts/reply-alice-q3.md (new draft)
  ~ .ta/change_summary.json

Agent summary: "Drafted a concise reply confirming Q3 budget
numbers and requesting the updated forecast spreadsheet."

Reply with:
  APPROVE  — send the draft
  REJECT   — discard
  DISCUSS  — I'll review in detail later
```

### Slack Flow
```
┌──────────────────────────────────────────┐
│ TA PR Ready                              │
│                                          │
│ "Draft reply to Alice re: Q3 budget"     │
│ 3 files changed                          │
│                                          │
│ Agent: "Drafted a concise reply..."      │
│                                          │
│ [Approve] [Reject] [View Details]        │
└──────────────────────────────────────────┘
```

### What TA Needs for This

These are **notification connectors** — same pattern as the fs connector but for
notification delivery and approval ingestion:

- `ta-connector-email`: Send PR summaries, parse reply for approve/reject
- `ta-connector-slack`: Post PR cards, handle button callbacks
- `ta-connector-discord`: Post PR embeds, handle reactions/commands

Each connector is bidirectional:
- **Outbound**: PR summary formatted for the channel
- **Inbound**: Parse approval/rejection from channel interaction

---

## Two Concrete Scenarios

### Scenario 1: Email Assistant (Simple)

```
[Gmail webhook] "New email from Alice"
       │
       ▼
[Orchestrator] Looks up email-assistant role
       │
       ▼
[ta run "Triage and draft reply to Alice's email"
  --agent claude-code --source ./email-workspace]
       │
       ▼
[Agent reads email via gmail connector (staging),
 drafts reply in staging, exits]
       │
       ▼
[ta pr build → PR with draft reply]
       │
       ▼
[on_pr_generated → email notification to me]
       │
       ▼
[I reply "APPROVE" from my phone]
       │
       ▼
[ta pr apply → gmail connector sends the draft]
```

**Timeline**: Email arrives → draft ready in ~2 min → I approve from phone → sent.
**Safety**: I see exactly what will be sent. Nothing goes out without my approval.

### Scenario 2: Project Manager (Plan-Driven Development)

```
[on_pr_applied for Phase 4c]
       │
       ▼
[Orchestrator] PM role wakes up, reads PLAN.md
  "Phase 4c done. Next pending: Phase v0.1"
       │
       ▼
[ta run "Implement Phase v0.1 requirements"
  --agent claude-code --phase v0.1]
       │
       ▼
[Agent works on v0.1 tasks in staging]
       │
       ▼
[ta pr build → PR with v0.1 changes]
       │
       ▼
[on_pr_generated → Slack notification to #dev-prs]
       │
       ▼
[I review in Slack, tap "View Details" for specific files,
 approve src/** but discuss config changes]
       │
       ▼
[ta pr apply --approve "src/**" --discuss "*.toml"
 → partial apply, discussion items flagged]
```

**Timeline**: Phase completes → next phase auto-starts → I review when convenient.
**Safety**: Selective approval. I can accept code but hold back config changes.

---

## Implementation Phases (Where This Lives in PLAN.md)

### Already Built (TA Core)
- Staging/overlay mediation (Phase 3)
- Policy engine with capability scoping (Phase 1)
- Plan tracking with auto-progression (Phase 4a.1)
- Selective approval with dependency warnings (Phase 4b-4c)
- Plugin event hooks (designed, not yet implemented)

### Phase 6 — First External Connector (Already in PLAN.md)
Pick one: Gmail, Drive, or DB. Proves the pattern for non-filesystem connectors.
The email assistant scenario becomes possible here.

### New: Phase 9 — Event System & Orchestration API
<!-- status: pending -->
- `--json` output on all CLI commands for programmatic use
- Event hook execution (call webhooks/scripts on state transitions)
- `ta events listen` — stream events for external consumers
- Stable event schema (JSON) matching `plugins-architecture-guidance.md`
- Non-interactive approval API (token-based, for Slack/email bots)

### New: Phase 10 — Notification Connectors
<!-- status: pending -->
- `ta-connector-notify-email`: SMTP-based PR summaries + reply parsing
- `ta-connector-notify-slack`: Slack app with Block Kit PR cards + button handlers
- `ta-connector-notify-discord`: Discord bot with embed PR summaries + reaction handlers
- Bidirectional: outbound notifications + inbound approval
- Unified config: `notification_channel` in role definitions

### New: Phase 11 — Virtual Office Runtime
<!-- status: pending -->
- Role definition schema (YAML, as sketched above)
- Trigger system: cron scheduler + webhook receiver + TA event listener
- Office manager daemon: reads role configs, routes triggers, calls `ta run`
- `ta office start/stop/status` commands
- Role-scoped TA policies (auto-generated from role capabilities)
- Integration with Claude Flow (office manager as a Flow coordinator)

### Relationship to Existing Tools

```
┌─────────────────────────────────────────────┐
│ Virtual Office = Role Config + Trigger Glue │
│                                             │
│ Uses:                                       │
│   TA ........... safety mediation           │
│   Claude Flow .. agent coordination         │
│   clawdbot ..... Discord UX                 │
│   n8n/Temporal . workflow automation         │
│                                             │
│ Doesn't duplicate any of these.             │
│ Composes them with a role/trigger layer.    │
└─────────────────────────────────────────────┘
```

---

## Open Questions

1. **Should the office manager be a TA daemon or a separate process?**
   Leaning separate — TA stays focused on mediation. The office manager is a
   thin orchestration layer that calls `ta run` and listens for events.

2. **Where do role definitions live?**
   Options: `~/.config/ta/offices/`, `.ta/office.yaml` in project root, or
   a separate repo. Project-scoped makes sense for dev teams; global for
   personal assistants.

3. **How does Claude Flow interact?**
   Claude Flow could BE the office manager — it already does agent coordination.
   TA provides the safety layer. Flow provides the orchestration. The role YAML
   becomes a Flow configuration that happens to use TA for every action.

4. **What about multi-tenant / multi-human approval?**
   Current TA is single-user. Virtual office might need: "Alice approves email
   drafts, Bob approves code PRs." This is a policy engine extension (Phase 5
   already covers agent setup proposals — extend to human approval routing).

5. **Cost/latency of the loop?**
   Email arrives → agent drafts reply → human approves → sent. If this takes
   30 seconds for the agent + instant notification, it's viable for async
   communication. Not viable for real-time chat without pre-approved templates.

---

## Summary

| Layer | What | Who Builds It | Status |
|---|---|---|---|
| TA Core | Staging, review, apply, audit | This project | Phases 0-4c done |
| Event System | Hooks, webhooks, JSON API | TA (Phase 9) | Planned |
| Notification | Email/Slack/Discord PR delivery | TA connectors (Phase 10) | Planned |
| External Connectors | Gmail, Drive, DB staging | TA (Phase 6) | Planned |
| Orchestration | Role routing, plan stepping | Claude Flow / custom (Phase 11) | Planned |
| Virtual Office | Role defs, triggers, glue | Thin layer on top (Phase 11) | Vision |

**TA's job**: Make it safe for agents to act in the real world.
**Orchestrator's job**: Decide which agents act and when.
**Notification's job**: Let humans approve from wherever they are.

None of these duplicate each other. They compose.
