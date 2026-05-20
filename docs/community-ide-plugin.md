# Building a Trusted Autonomy IDE Plugin

This guide explains how to integrate Trusted Autonomy into any IDE. The TA daemon exposes a
stable HTTP REST API on `http://127.0.0.1:7700` — no changes to TA core required.

The VS Code and JetBrains plugins in this repository implement exactly the same calls documented
here. Use them as reference implementations.

## Overview

A TA plugin needs to do four things:

1. **Connect** — check the daemon is running with `GET /health`
2. **Display** — list active goals and pending drafts for the current project
3. **Act** — start goals, approve or deny drafts
4. **React** — listen on the SSE event stream for real-time goal/draft updates

---

## REST API Reference

All endpoints are on `http://127.0.0.1:7700` by default. Users can configure a custom URL.

### Authentication

Optional. If the user configures a bearer token, include it as:
```
Authorization: Bearer <token>
```
For localhost-only setups (the default), no token is required.

---

### `GET /health`

Check daemon health. Safe to call unauthenticated.

**Response**
```json
{
  "status": "ok",
  "version": "0.16.1-alpha",
  "timestamp": "2026-05-20T12:34:56Z",
  "plugins": ["discord", "slack"]
}
```

| Field | Description |
|---|---|
| `status` | `"ok"` if healthy |
| `version` | TA daemon version |
| `plugins` | Loaded channel plugins |

---

### `GET /api/status`

Get the current project status including active goals.

**Response**
```json
{
  "project": "my-project",
  "version": "0.16.1-alpha",
  "daemon_version": "0.16.1-alpha",
  "active_agents": [
    {
      "agent_id": "abc123",
      "goal_id": "7df02c4b-...",
      "tag": "feature/auth",
      "title": "Add JWT authentication",
      "state": "running",
      "running_secs": 142,
      "active": true,
      "vcs_state": "feature/auth",
      "process_health": "ok"
    }
  ],
  "pending_drafts": 1,
  "active_goals": 1,
  "total_goals": 42
}
```

**Goal states** (in order of lifecycle):

| State | Meaning |
|---|---|
| `running` | Agent is actively working |
| `pr_ready` | Draft is ready for review |
| `under_review` | Draft has been viewed |
| `approved` | Draft approved, applying changes |
| `applied` | Changes applied to project |
| `failed` | Agent encountered an error |
| `denied` | Draft was denied by reviewer |
| `completed` | Goal lifecycle complete |

---

### `GET /api/drafts`

List all drafts (across all statuses).

**Response** — array of draft summaries:
```json
[
  {
    "package_id": "d8f3a2b1-...",
    "title": "Add JWT authentication",
    "status": "pending_review",
    "created_at": "2026-05-20T12:34:56Z",
    "artifact_count": 5,
    "goal_id": "7df02c4b-..."
  }
]
```

**Draft statuses**: `draft`, `pending_review`, `approved`, `denied`, `applied`, `superseded`, `closed`

Filter to actionable drafts by excluding terminal statuses: `applied`, `superseded`, `closed`, `denied`.

---

### `GET /api/drafts/{id}`

Get full draft details including the list of changed files.

**Response**
```json
{
  "package_id": "d8f3a2b1-...",
  "created_at": "2026-05-20T12:34:56Z",
  "goal": {
    "goal_id": "7df02c4b-...",
    "title": "Add JWT authentication",
    "objective": "Implement RS256 JWT validation middleware"
  },
  "summary": {
    "what_changed": "Added JWT middleware with RS256 verification",
    "why": "Security audit required token-based auth",
    "impact": "All API endpoints now require valid JWT"
  },
  "changes": {
    "artifacts": [
      {
        "resource_uri": "fs://workspace/src/auth/middleware.rs",
        "change_type": "modified",
        "diff_ref": "sha256:abc123...",
        "rationale": "Core JWT validation logic"
      }
    ]
  },
  "status": "pending_review",
  "plan_phase": "v0.16.1",
  "display_id": "d8f3a2b1"
}
```

**Artifact `resource_uri` scheme**: `fs://workspace/<relative-path>` — strip the `fs://workspace/`
prefix to get the relative path for display.

---

### `POST /api/drafts/{id}/approve`

Approve a draft and apply its changes to the project.

**Request body**: `{}` (empty JSON object)

**Response**
```json
{
  "package_id": "d8f3a2b1-...",
  "status": "applied",
  "message": "Draft applied successfully — 5 files changed"
}
```

---

### `POST /api/drafts/{id}/deny`

Deny a draft with a reason. The reason is injected into the agent's context on the next follow-up.

**Request body**
```json
{
  "reason": "The validation logic is incorrect — see inline comments"
}
```

**Response**
```json
{
  "package_id": "d8f3a2b1-...",
  "status": "denied",
  "message": "Draft denied"
}
```

---

### `POST /api/cmd`

Run a TA CLI command on the daemon. Used to start goals from the IDE.

**Request body**
```json
{
  "command": "ta run \"Add input validation to the login form\""
}
```

**Response**
```json
{
  "exit_code": 0,
  "stdout": "Goal started: Add input validation to the login form\nGoal ID: 7df02c4b-...",
  "stderr": "",
  "background_key": null
}
```

Check `exit_code === 0` for success. Display `stderr || stdout` on failure.

---

## SSE Event Stream

The daemon pushes real-time events via Server-Sent Events (SSE).

### `GET /api/events`

**Query parameters**:
- `since` — ISO 8601 timestamp; only return events after this time (for reconnect)
- `types` — comma-separated event type filter (e.g. `draft_ready,goal_state_changed`)

**Response**: `text/event-stream` — standard SSE format

```
event: draft_ready
data: {"event_type":"draft_ready","timestamp":"2026-05-20T12:35:10Z","payload":{"draft_title":"Add JWT authentication","package_id":"d8f3a2b1-..."}}

event: goal_state_changed
data: {"event_type":"goal_state_changed","timestamp":"2026-05-20T12:35:15Z","payload":{"goal_id":"7df02c4b-...","title":"Add JWT authentication","state":"applied"}}
```

### Event types

| Event type | Trigger | Key payload fields |
|---|---|---|
| `goal_state_changed` | Goal transitions to a new state | `goal_id`, `title`, `state` |
| `draft_ready` | Agent finishes and draft is ready to review | `package_id`, `draft_title` |
| `draft_approved` | Draft approved and applied | `package_id`, `draft_title` |
| `draft_denied` | Draft denied by reviewer | `package_id`, `draft_title` |
| `goal_failed` | Agent encountered a fatal error | `goal_id`, `title`, `message` |

### Reconnect strategy

1. Track the `timestamp` field from each event
2. On disconnect, reconnect with `?since=<last-timestamp>` to resume without duplicates
3. Back off exponentially (start at 5s, cap at 60s)

---

## Recommended UX Patterns

### Command palette / quick actions

Expose these commands:
- **Start Goal** — input prompt for goal description, optional plan phase, then `POST /api/cmd`
- **Approve Draft** — picker of pending drafts, confirmation dialog, then `POST /api/drafts/{id}/approve`
- **Deny Draft** — picker of pending drafts, reason input, then `POST /api/drafts/{id}/deny`
- **Open Shell** — open `{daemonUrl}/shell` in the system browser

### Status indicator

Show daemon health in the status bar / status line:
- Connected + active goals: `TA: 2 running`
- Connected + idle: `TA: ready`
- Daemon offline: `TA: offline`

Poll `/health` every 15 seconds to keep the indicator current.

### Tool panel / sidebar

Two views work well:
- **Goals** — list of `active_agents` with state and duration, polling `/api/status`
- **Drafts** — list of active (non-terminal) drafts, polling `/api/drafts`

Refresh both views on SSE events.

### Notifications

Show a balloon/toast notification for:
- `draft_ready` → "Draft ready: {title}" with an "Approve" quick action
- `goal_state_changed` with `state == "failed"` → "Goal failed: {title}" with a link to the shell
- `draft_approved` → "Changes applied: {title}"

---

## Minimal Working Example (TypeScript)

This example targets Zed, Cursor, or any editor with a TypeScript extension API.
It demonstrates all four integration points.

```typescript
const DAEMON_URL = "http://127.0.0.1:7700";

// 1. Health check
async function checkHealth(): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/health`);
    const data = await res.json();
    return data.status === "ok";
  } catch {
    return false;
  }
}

// 2. List pending drafts
async function listDrafts() {
  const res = await fetch(`${DAEMON_URL}/api/drafts`);
  const drafts = await res.json();
  return drafts.filter(
    (d) => !["applied", "superseded", "closed", "denied"].includes(d.status)
  );
}

// 3. Start a goal
async function startGoal(description: string) {
  const res = await fetch(`${DAEMON_URL}/api/cmd`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ command: `ta run "${description.replace(/"/g, '\\"')}"` }),
  });
  return await res.json(); // { exit_code, stdout, stderr }
}

// 4. Approve a draft
async function approveDraft(packageId: string) {
  const res = await fetch(`${DAEMON_URL}/api/drafts/${packageId}/approve`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: "{}",
  });
  return await res.json(); // { package_id, status, message }
}

// 5. Listen for real-time events (SSE)
function subscribeToEvents(onEvent: (type: string, payload: unknown) => void) {
  let lastTimestamp = "";

  const connect = () => {
    const url = `${DAEMON_URL}/api/events?types=draft_ready,goal_state_changed,goal_failed${
      lastTimestamp ? `&since=${lastTimestamp}` : ""
    }`;
    const es = new EventSource(url);

    es.onmessage = (e) => {
      const data = JSON.parse(e.data);
      if (data.timestamp) lastTimestamp = data.timestamp;
      onEvent(e.type || data.event_type, data.payload);
    };

    es.onerror = () => {
      es.close();
      setTimeout(connect, 15_000); // reconnect after 15s
    };

    return () => es.close(); // call to stop
  };

  return connect();
}
```

---

## Configuration

Ask the user to configure:

| Setting | Default | Description |
|---|---|---|
| Daemon URL | `http://127.0.0.1:7700` | TA daemon address |
| API token | `""` | Bearer token (leave empty for localhost) |
| Poll interval | `15` | Seconds between panel refreshes |

---

## Publishing Your Plugin

Community plugins are welcome! If you build a TA integration for your IDE:

1. Use the REST API above — no changes to TA core needed
2. Document the minimum TA version your plugin requires (check `health.version`)
3. Open a PR or issue on [trustedautonomy/ta](https://github.com/trustedautonomy/ta) to add your plugin to the README
