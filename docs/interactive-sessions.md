# Interactive Session Orchestration

**Version**: v0.3.1.2+

This guide walks through the interactive session capabilities introduced in v0.3.1.2. Interactive sessions let you observe agent work, inject guidance, review drafts inline, and manage multiple concurrent sessions through a unified protocol.

---

## Table of Contents

1. [Overview](#overview)
2. [Quick Example](#quick-example)
3. [Session Lifecycle](#session-lifecycle)
4. [Managing Sessions](#managing-sessions)
5. [Multi-Session Workflows](#multi-session-workflows)
6. [Per-Agent Configuration](#per-agent-configuration)
7. [Integration with Review Sessions](#integration-with-review-sessions)
8. [Architecture: SessionChannel Protocol](#architecture-sessionchannel-protocol)
9. [Future: Multi-Platform Channels](#future-multi-platform-channels)

---

## Overview

Without `--interactive`, `ta run` launches an agent, waits for it to exit, and builds a draft. The human only sees the result.

With `--interactive`, TA wraps the agent execution in a **session** that:
- Tracks the human-agent relationship (who launched what, when, on which channel)
- Records a message log for audit and replay
- Links inline draft reviews back to the session
- Persists across CLI invocations (you can inspect sessions from a separate terminal)
- Supports multiple concurrent sessions for different goals

The session protocol is designed so that future frontends (Discord, Slack, web app) implement the same `SessionChannel` trait and get the full TA experience without changes to core logic.

---

## Quick Example

### Start an interactive session

```bash
cd your-project/
ta run "Add input validation to the API" --interactive
```

Output:
```
Goal started: a1b2c3d4-5678-9abc-def0-123456789abc
  Title:   Add input validation to the API
  Staging: /path/to/.ta/staging/a1b2c3d4-5678-9abc-def0-123456789abc

Interactive session: f9e8d7c6-5432-1fed-cba0-987654321fed
  Channel: cli:48291

Launching claude in staging workspace...
  Working dir: /path/to/.ta/staging/a1b2c3d4-5678-9abc-def0-123456789abc
  Mode: interactive (session orchestration enabled)
```

The agent launches and works normally. TA tracks the session in the background.

### Check on it from another terminal

```bash
ta session list
```

```
SESSION ID                             GOAL ID                                AGENT        STATE          ELAPSED
f9e8d7c6-5432-1fed-cba0-987654321fed   a1b2c3d4-5678-9abc-def0-123456789abc   claude-code  active         5m 23s

1 session(s).
```

### View session details

```bash
ta session show f9e8d7c6
```

```
Session:   f9e8d7c6-5432-1fed-cba0-987654321fed
Goal:      a1b2c3d4-5678-9abc-def0-123456789abc
Channel:   cli:48291
Agent:     claude-code
State:     active
Created:   2026-02-24T23:45:00Z
Updated:   2026-02-24T23:50:23Z
Elapsed:   5m 23s
```

### After the agent exits

TA builds the draft and marks the session completed:

```
Agent exited. Building draft...
Draft built: 3 artifacts, 142 lines changed

Next steps:
  ta draft list
  ta draft view <draft-id>
  ta draft approve <draft-id>
  ta draft apply <draft-id> --git-commit
  ta session list
```

```bash
ta session list --all
```

```
SESSION ID                             GOAL ID                                AGENT        STATE          ELAPSED
f9e8d7c6-5432-1fed-cba0-987654321fed   a1b2c3d4-5678-9abc-def0-123456789abc   claude-code  completed      12m 34s

1 session(s).
```

---

## Session Lifecycle

Interactive sessions follow a state machine:

```
                +-----------+
                |  Active   |
                +-----+-----+
                      |
            +---------+---------+
            |         |         |
            v         v         v
       +---------+ +-------+ +-------+
       | Paused  | | Comp- | | Abor- |
       |         | | leted | | ted   |
       +----+----+ +-------+ +-------+
            |                     ^
            +---------------------+
            (can abort from paused)
```

| State | Meaning |
|-------|---------|
| **Active** | Agent is running, human is connected |
| **Paused** | Agent suspended, session can be resumed |
| **Completed** | Agent exited successfully, draft built |
| **Aborted** | Session killed (agent crash, user abort, or launch failure) |

Transitions:
- Active -> Paused, Completed, or Aborted
- Paused -> Active (resume) or Aborted
- Completed and Aborted are terminal states

---

## Managing Sessions

### List sessions

```bash
# Active and paused sessions only (default)
ta session list

# Include completed and aborted sessions
ta session list --all
```

### Inspect a session

```bash
# Full UUID
ta session show f9e8d7c6-5432-1fed-cba0-987654321fed

# Prefix matching (any unique prefix works)
ta session show f9e8
```

The detail view shows:
- Session and goal IDs
- Channel identity (e.g., `cli:48291`)
- Agent and state
- Timestamps and elapsed time
- Associated draft IDs (if drafts were reviewed inline)
- Message log with timestamps

### Message log

Every session records a timestamped message log for audit:

```
Message log (3 messages):
------------------------------------------------------------
  [23:45:00] ta-system: Session started
  [23:50:23] ta-system: Agent output captured (142 lines)
  [23:57:45] ta-system: Agent exited, draft built
```

---

## Multi-Session Workflows

You can run multiple interactive sessions simultaneously for different goals or agents:

```bash
# Terminal 1: Feature implementation
ta run "Implement OAuth2 login" --interactive --agent claude-code

# Terminal 2: Test writing (different goal, same project)
ta run "Write integration tests for auth" --interactive --agent codex

# Terminal 3: Monitor both
ta session list
```

```
SESSION ID                             GOAL ID                                AGENT        STATE          ELAPSED
8a7b6c5d-1234-5678-9abc-def012345678   f1e2d3c4-5678-9abc-def0-123456789abc   claude-code  active         12m 34s
4b5a6c7d-9876-5432-1fed-cba098765432   a1b2c3d4-5678-9abc-def0-987654321fed   codex        active         5m 18s

2 session(s).
```

Each session has its own staging workspace (via the goal system), so there's no interference between concurrent agents.

### Sequential sessions for iterative work

```bash
# Round 1: Initial implementation
ta run "Build the search API" --interactive
# Review and partially approve...
ta draft apply <id> --approve "src/search/**" --discuss "src/config/*"

# Round 2: Address feedback with follow-up
ta run "Fix search config issues" --interactive --follow-up
# Agent receives parent goal context including discuss items
```

---

## Per-Agent Configuration

Each agent can declare interactive session settings in its YAML config file. Config files are loaded from (in priority order):

1. `.ta/agents/<agent-id>.yaml` (project override)
2. `~/.config/ta/agents/<agent-id>.yaml` (user override)
3. Built-in defaults

### Example: `.ta/agents/claude-code.yaml`

```yaml
command: claude
args_template: ["{prompt}"]
injects_context_file: true
injects_settings: true

interactive:
  enabled: true
  output_capture: pipe       # pipe (default), pty, or log
  allow_human_input: true    # enable guidance injection
  auto_exit_on: "idle_timeout: 300s"
  resume_cmd: "claude --resume {session_id}"
```

### Configuration fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Whether interactive mode is available |
| `output_capture` | string | `"pipe"` | How to capture agent output: `pipe`, `pty`, or `log` |
| `allow_human_input` | bool | `true` | Allow human message injection during execution |
| `auto_exit_on` | string | none | Auto-exit condition (e.g., `"idle_timeout: 300s"`) |
| `resume_cmd` | string | none | Command template for session resume |

### Output capture modes

| Mode | Description | Use case |
|------|-------------|----------|
| `pipe` | Standard stdout/stderr piping | Default, works with all agents |
| `pty` | Pseudo-terminal wrapping | Preserves ANSI colors, interactive prompts |
| `log` | Write-to-file only | Headless/CI runs, no terminal output |

---

## Integration with Review Sessions

Interactive sessions complement the existing review session system (from v0.3.0):

```bash
# Start an interactive run
ta run "Implement feature X" --interactive

# ... agent works and exits, draft is built ...

# Start a review session for the draft
ta draft review start <draft-id> --reviewer "alice"

# Review artifacts one by one
ta draft review next
ta draft review comment "fs://workspace/src/main.rs" "Needs error handling"
ta draft review next

# Finish review
ta draft review finish
```

The interactive session and review session are linked through the goal — `ta session show` displays associated draft IDs, and draft review commands show which goal/session produced the draft.

### Inline review (planned)

A future enhancement will allow reviewing drafts from within the interactive session, without switching to separate `ta draft review` commands. The protocol already supports this via `HumanInput::Approve` and `HumanInput::Reject` messages.

---

## Architecture: SessionChannel Protocol

The interactive session system is built on a trait-based protocol that separates the communication channel from the session logic:

```rust
/// A bidirectional channel between a human and a TA-mediated agent session.
pub trait SessionChannel: Send + Sync {
    /// Display agent output to the human (streaming).
    fn emit(&self, event: &SessionEvent) -> Result<(), SessionChannelError>;

    /// Receive human input (blocks until available or timeout).
    fn receive(&self, timeout: Duration) -> Result<Option<HumanInput>, SessionChannelError>;

    /// Channel identity (for audit trail).
    fn channel_id(&self) -> &str;
}
```

### SessionEvent (TA -> Human)

| Variant | Description |
|---------|-------------|
| `AgentOutput { stream, content }` | Agent stdout/stderr output |
| `DraftReady { draft_id, summary, artifact_count }` | Draft checkpoint ready for review |
| `GoalComplete { goal_id }` | Goal finished |
| `WaitingForInput { prompt }` | Agent needs human guidance |
| `StatusUpdate { message }` | Informational status |

### HumanInput (Human -> TA)

| Variant | Description |
|---------|-------------|
| `Message { text }` | Free-form guidance injected into agent context |
| `Approve { draft_id, artifact_uri }` | Approve a draft or specific artifact |
| `Reject { draft_id, artifact_uri, reason }` | Reject with reason |
| `Abort` | Kill the session |

### TaEvent integration

Three new event types are emitted for audit compliance:

| Event | Payload | When |
|-------|---------|------|
| `SessionStarted` | goal_id, session_id, channel_id, agent_id | `ta run --interactive` creates session |
| `SessionStateChanged` | session_id, from_state, to_state | Any state transition |
| `SessionMessage` | session_id, sender, content_preview | Human or agent message logged |

These integrate with the existing tamper-evident audit trail (ta-audit) for compliance with ISO/IEC 42001 and IEEE 7001.

---

## Future: Multi-Platform Channels

The `SessionChannel` trait is designed so that messaging platform integrations are thin adapters, not new features. Each platform maps its primitives to `SessionEvent` / `HumanInput`:

| Platform | `emit()` | `receive()` | Channel identity | Status |
|----------|----------|-------------|-----------------|--------|
| **CLI** | Terminal stdout | stdin | `cli:{pid}` | Implemented |
| **Discord** | Thread message | Thread reply | `discord:{thread_id}` | Planned |
| **Slack** | Channel message | Thread reply | `slack:{channel}:{ts}` | Planned |
| **Email** | Reply email | Incoming email | `email:{thread_id}` | Planned |
| **Web app** | WebSocket push | WebSocket message | `web:{session_id}` | Planned |

Each adapter is expected to be ~100-200 lines: authenticate, map to `SessionChannel`, route to the correct TA session. All governance (draft review, audit, policy) is handled by TA core — the channel just carries messages.

### Example: What a Discord adapter would look like

```
Human posts in Discord thread: "Fix the auth bug"
  -> Discord adapter receives message
  -> Creates InteractiveSession with channel_id: "discord:thread:123456"
  -> Launches agent via ta run
  -> Agent output streamed back to Discord thread
  -> Draft ready notification posted as embed
  -> Human replies "approve" -> HumanInput::Approve
  -> Applied, result posted to thread
```

The protocol ensures every interaction is audited, regardless of which channel it originates from.

---

## Command Reference

### `ta run --interactive`

```bash
ta run "goal title" --interactive [--agent claude-code]
```

Creates a goal with an interactive session, launches the agent, and tracks the session lifecycle. On agent exit, builds the draft and marks the session completed.

### `ta session list`

```bash
ta session list [--all]
```

Lists interactive sessions. By default shows only active/paused sessions. Use `--all` to include completed and aborted sessions.

### `ta session show`

```bash
ta session show <session-id>
```

Displays detailed session information including goal link, channel identity, state, timestamps, associated drafts, and message history. Accepts full UUID or any unique prefix.

---

## Troubleshooting

### "No active interactive sessions"

This means no sessions are in Active or Paused state. Use `ta session list --all` to see completed/aborted sessions, or start a new one with `ta run --interactive`.

### Session shows "aborted" after agent crash

If the agent process crashes (non-zero exit or launch failure), the session is automatically marked as aborted. The draft build still runs if possible. Check the session message log for details:

```bash
ta session show <id>
# Look for: [ta-system] Agent launch failed: ...
```

### Multiple sessions for the same goal

Each `ta run --interactive` invocation creates a new session, even for the same goal. This is by design — each launch represents a distinct human-agent interaction. Use `ta session list` to find the relevant session.
