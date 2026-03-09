# Plugin Authoring Quickstart

This guide walks you through building a TA channel plugin from scratch. By the end you will have a working plugin that delivers agent questions to an external service and integrates with the TA review workflow.

## Overview

Channel plugins are out-of-process delivery adapters. When an agent calls `ta_ask_human`, the TA daemon needs to get that question in front of a human reviewer. Built-in channels (terminal, webhook, auto-approve) cover common cases, but plugins let you deliver questions anywhere: Slack, Microsoft Teams, PagerDuty, SMS, a custom dashboard, or anything else with an API.

Plugins run as separate processes in any language -- Rust, Python, Go, TypeScript, shell scripts. TA communicates with them using one of two protocols:

| Protocol | How it works | Best for |
|----------|-------------|----------|
| **json-stdio** | TA spawns the plugin, sends a question as JSON on stdin, reads the result from stdout | Local tools, scripts, CLIs, compiled binaries |
| **http** | TA POSTs the question as JSON to a URL and reads the result from the response body | Cloud functions, webhooks, long-running services |

## How Plugins Work

The lifecycle of a single question delivery:

1. An agent calls `ta_ask_human` with a question for the human reviewer.
2. The daemon serializes the question as a `ChannelQuestion` JSON object.
3. For **json-stdio** plugins: the daemon spawns the plugin process and writes the JSON to stdin. The plugin reads it, delivers the question (posts a Slack message, sends an email, etc.), and writes a `DeliveryResult` JSON object to stdout.
4. For **http** plugins: the daemon POSTs the JSON to the plugin's `deliver_url` and reads the `DeliveryResult` from the response body.
5. The human responds through the external channel (clicks a button, replies to a thread, answers an email).
6. The response flows back to TA via `POST {callback_url}/api/interactions/{interaction_id}/respond`.

The plugin is only responsible for steps 3-4 (delivering the question and confirming delivery). Step 6 is handled by your service posting back to the TA daemon's API.

## Quick Start: Minimal Python Plugin

Here is a complete working plugin in Python. It reads a question from stdin, prints it to stderr for debugging, and returns a success result.

### Directory structure

```
plugins/my-notifier/
  channel.toml
  channel_plugin.py
```

### channel.toml

```toml
name = "my-notifier"
version = "0.1.0"
command = "python3"
args = ["-u", "channel_plugin.py"]
protocol = "json-stdio"
capabilities = ["deliver_question"]
description = "Posts agent questions to my notification service"
timeout_secs = 30
```

### channel_plugin.py

```python
#!/usr/bin/env python3
import json
import sys
import urllib.request

def deliver_question(question):
    """Post the question to a webhook and return a DeliveryResult."""
    webhook_url = "https://hooks.example.com/ta-reviews"

    payload = json.dumps({
        "text": question["question"],
        "context": question.get("context"),
        "choices": question.get("choices", []),
        "callback": f"{question['callback_url']}/api/interactions/{question['interaction_id']}/respond",
    }).encode()

    req = urllib.request.Request(
        webhook_url,
        data=payload,
        headers={"Content-Type": "application/json"},
    )

    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            body = json.loads(resp.read())
            return {
                "channel": "my-notifier",
                "delivery_id": body.get("id", "unknown"),
                "success": True,
                "error": None,
            }
    except Exception as e:
        return {
            "channel": "my-notifier",
            "delivery_id": "",
            "success": False,
            "error": str(e),
        }

def main():
    line = sys.stdin.readline().strip()
    if not line:
        json.dump(
            {"channel": "my-notifier", "delivery_id": "", "success": False,
             "error": "No input received on stdin"},
            sys.stdout,
        )
        print()
        sys.exit(1)

    try:
        question = json.loads(line)
    except json.JSONDecodeError as e:
        json.dump(
            {"channel": "my-notifier", "delivery_id": "", "success": False,
             "error": f"Invalid JSON input: {e}"},
            sys.stdout,
        )
        print()
        sys.exit(1)

    result = deliver_question(question)
    json.dump(result, sys.stdout)
    print()  # Newline after JSON

if __name__ == "__main__":
    main()
```

Test it locally:

```bash
echo '{"interaction_id":"abc-123","goal_id":"goal-1","question":"Approve this draft?","context":"Agent finished editing auth.rs","response_hint":"yes_no","choices":[],"turn":1,"callback_url":"http://localhost:4100"}' \
  | python3 plugins/my-notifier/channel_plugin.py
```

You should see a JSON result on stdout and a debug line on stderr.

## JSON Schemas

### ChannelQuestion (input -- read from stdin)

```json
{
  "interaction_id": "550e8400-e29b-41d4-a716-446655440000",
  "goal_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "question": "The draft modifies 3 files in crates/ta-daemon/. Approve?",
  "context": "Agent completed task: refactor session handling",
  "response_hint": "yes_no",
  "choices": [],
  "turn": 1,
  "callback_url": "http://localhost:4100"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `interaction_id` | string (UUID) | Unique ID for this question. Use this when posting the human's response back. |
| `goal_id` | string (UUID) | The goal this question belongs to. |
| `question` | string | The question text to show the reviewer. |
| `context` | string or null | What the agent was doing when it asked the question. |
| `response_hint` | string | One of `"freeform"`, `"yes_no"`, or `"choice"`. Tells you how to render the response UI. |
| `choices` | string[] | When `response_hint` is `"choice"`, the list of options to present. |
| `turn` | integer | Conversation turn number (starts at 1). |
| `callback_url` | string | Base URL of the TA daemon. Post responses to `{callback_url}/api/interactions/{interaction_id}/respond`. |

### DeliveryResult (output -- write to stdout)

```json
{
  "channel": "my-notifier",
  "delivery_id": "msg-550e8400",
  "success": true,
  "error": null
}
```

| Field | Type | Description |
|-------|------|-------------|
| `channel` | string | Your plugin's channel name (must match the `name` in `channel.toml`). |
| `delivery_id` | string | A channel-specific identifier for the delivered message (Slack message timestamp, email message-id, etc.). Can be empty on failure. |
| `success` | boolean | Whether the question was successfully delivered. |
| `error` | string or null | Error message if delivery failed. Null on success. |

### Human Response (POST back to TA)

When a human responds through your channel, post their answer back:

```
POST {callback_url}/api/interactions/{interaction_id}/respond
Content-Type: application/json

{
  "response": "approved"
}
```

The `response` field is a free-form string. For `yes_no` hints, use `"approved"` or `"denied"`. For `choice` hints, use the exact choice text. For `freeform` hints, pass through whatever the human typed.

## Manifest Format (channel.toml)

Every plugin needs a `channel.toml` file in its directory. This tells TA how to find and run the plugin.

### json-stdio plugin

```toml
name = "my-notifier"
version = "0.1.0"
command = "python3"
args = ["-u", "channel_plugin.py"]
protocol = "json-stdio"
capabilities = ["deliver_question"]
description = "Posts agent questions to my notification service"
timeout_secs = 30
```

### http plugin

```toml
name = "my-cloud-reviewer"
version = "0.1.0"
protocol = "http"
deliver_url = "https://my-service.example.com/ta/deliver"
auth_token_env = "MY_SERVICE_TOKEN"
capabilities = ["deliver_question"]
description = "Sends review questions to my cloud review service"
timeout_secs = 30
```

| Field | Required | Description |
|-------|----------|-------------|
| `name` | yes | Plugin name. Used in config references and logs. |
| `version` | yes | Semver version string. |
| `protocol` | yes | `"json-stdio"` or `"http"`. |
| `command` | json-stdio only | The executable to run. |
| `args` | no | Arguments to pass to the command. |
| `deliver_url` | http only | URL where TA will POST the question JSON. Must start with `http://` or `https://`. |
| `auth_token_env` | no | Name of an environment variable containing an auth token. TA reads this at startup and sends it as a `Bearer` token in the `Authorization` header for http plugins. |
| `capabilities` | yes | List of capabilities. Currently only `["deliver_question"]` is used. |
| `description` | no | Human-readable description. Shown in `ta plugin list`. |
| `timeout_secs` | no | How long TA waits for the plugin to respond before timing out. Default: 30. |
| `build_command` | no | Shell command to build the plugin from source (e.g., `"cargo build --release"`). Used by `ta plugin build`. |

## Installing Your Plugin

### From a local directory

```bash
# Install to the current project (.ta/plugins/channels/my-notifier/)
ta plugin install ./plugins/my-notifier

# Install globally (~/.config/ta/plugins/channels/my-notifier/)
ta plugin install ./plugins/my-notifier --global
```

### Validate installed plugins

```bash
# Check that all discovered plugins have valid manifests
ta plugin validate

# List all installed plugins with their protocols and status
ta plugin list
```

### Build Rust plugins from source

If your plugin is a Rust crate in the `plugins/` directory with both `Cargo.toml` and `channel.toml`:

```bash
# Build a specific plugin
ta plugin build my-notifier

# Build all plugins in plugins/
ta plugin build --all
```

This runs `cargo build --release` (or your custom `build_command`), then copies the binary and manifest to `.ta/plugins/channels/<name>/`.

### Verify channel registration

```bash
# Show all active channels, including plugins
ta config channels

# Validate that each channel can be reached
ta config channels --check
```

## Rust Plugin Template

For compiled plugins with better performance and type safety, use Rust. Here is a minimal skeleton.

### Cargo.toml

```toml
[package]
name = "ta-channel-my-notifier"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "ta-channel-my-notifier"
path = "src/main.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

### src/main.rs

```rust
use std::io::{self, BufRead, Write};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct ChannelQuestion {
    interaction_id: String,
    #[allow(dead_code)]
    goal_id: String,
    question: String,
    context: Option<String>,
    response_hint: String,
    #[serde(default)]
    choices: Vec<String>,
    #[serde(default = "default_turn")]
    turn: u32,
    callback_url: String,
}

fn default_turn() -> u32 { 1 }

#[derive(Debug, Serialize)]
struct DeliveryResult {
    channel: String,
    delivery_id: String,
    success: bool,
    error: Option<String>,
}

fn deliver(question: &ChannelQuestion) -> DeliveryResult {
    // TODO: Replace with your delivery logic.
    eprintln!(
        "[my-notifier] Delivering question {}: {}",
        question.interaction_id, question.question
    );

    DeliveryResult {
        channel: "my-notifier".into(),
        delivery_id: format!("msg-{}", question.interaction_id),
        success: true,
        error: None,
    }
}

fn main() {
    let stdin = io::stdin();
    let line = match stdin.lock().lines().next() {
        Some(Ok(line)) => line,
        _ => {
            let result = DeliveryResult {
                channel: "my-notifier".into(),
                delivery_id: String::new(),
                success: false,
                error: Some("No input received on stdin".into()),
            };
            serde_json::to_writer(io::stdout(), &result).ok();
            println!();
            std::process::exit(1);
        }
    };

    let question: ChannelQuestion = match serde_json::from_str(&line) {
        Ok(q) => q,
        Err(e) => {
            let result = DeliveryResult {
                channel: "my-notifier".into(),
                delivery_id: String::new(),
                success: false,
                error: Some(format!("Invalid JSON input: {e}")),
            };
            serde_json::to_writer(io::stdout(), &result).ok();
            println!();
            std::process::exit(1);
        }
    };

    let result = deliver(&question);
    serde_json::to_writer(io::stdout(), &result).ok();
    println!();
}
```

### channel.toml

```toml
name = "my-notifier"
version = "0.1.0"
command = "ta-channel-my-notifier"
protocol = "json-stdio"
capabilities = ["deliver_question"]
description = "My custom channel plugin"
timeout_secs = 30
```

Build and install:

```bash
ta plugin build my-notifier
```

## HTTP Callback Protocol

Use the HTTP protocol when your plugin runs as a long-lived service rather than a spawned process. This is the right choice for cloud functions, webhook endpoints, and always-on services.

### How it works

1. TA reads the plugin's `deliver_url` and optional `auth_token_env` from the manifest or daemon config.
2. When a question needs delivery, TA sends:
   ```
   POST {deliver_url}
   Content-Type: application/json
   Authorization: Bearer {token}   # only if auth_token_env is set

   { ... ChannelQuestion JSON ... }
   ```
3. Your service processes the request and returns a `DeliveryResult` in the response body.

### Example: daemon.toml inline config

```toml
[[channels.external]]
name = "pagerduty"
protocol = "http"
deliver_url = "https://my-service.example.com/ta/deliver"
auth_token_env = "TA_PAGERDUTY_TOKEN"
```

### Example: minimal HTTP handler (Python/Flask)

```python
from flask import Flask, request, jsonify

app = Flask(__name__)

@app.route("/ta/deliver", methods=["POST"])
def deliver():
    question = request.json
    # Deliver the question to your service...
    return jsonify({
        "channel": "pagerduty",
        "delivery_id": "incident-456",
        "success": True,
        "error": None,
    })
```

### Posting responses back

When the human responds through your service, forward the response to the TA daemon:

```
POST {callback_url}/api/interactions/{interaction_id}/respond
Content-Type: application/json

{"response": "approved"}
```

The `callback_url` and `interaction_id` are included in every `ChannelQuestion`, so your service has everything it needs to close the loop.

## Testing Your Plugin

### Manual stdin/stdout test

The fastest way to verify your plugin works:

```bash
echo '{"interaction_id":"test-001","goal_id":"goal-001","question":"Approve this draft?","context":"Agent edited 2 files","response_hint":"yes_no","choices":[],"turn":1,"callback_url":"http://localhost:4100"}' \
  | python3 plugins/my-notifier/channel_plugin.py
```

Expected stdout (one JSON line):

```json
{"channel": "my-notifier", "delivery_id": "msg-test-001", "success": true, "error": null}
```

### Validate the manifest

```bash
ta plugin validate
```

This checks that all discovered plugins have valid `channel.toml` files and that required fields are present for each protocol.

### Verify TA sees your plugin

```bash
ta config channels
```

Your plugin should appear in the output with its name, protocol, and capabilities.

### Test error handling

Send invalid input and confirm your plugin returns a proper error result instead of crashing:

```bash
echo 'not-json' | python3 plugins/my-notifier/channel_plugin.py
echo '' | python3 plugins/my-notifier/channel_plugin.py
```

Both should produce a `DeliveryResult` with `"success": false` and a descriptive `"error"` string.

## Configuring TA to Use Your Plugin

### Option 1: Plugin discovery (recommended)

Install the plugin and TA discovers it automatically:

```bash
ta plugin install ./plugins/my-notifier
```

Then reference it in `.ta/config.yaml`:

```yaml
channels:
  review:
    - type: terminal
    - type: my-notifier
  strategy: first_response
```

### Option 2: Inline config in daemon.toml

Skip installation entirely and register the plugin inline:

```toml
# .ta/daemon.toml

[[channels.external]]
name = "my-notifier"
command = "python3 -u /path/to/channel_plugin.py"
protocol = "json-stdio"
```

### Option 3: Set as default channel

To have all `ta_ask_human` questions go to your plugin by default:

```toml
# .ta/daemon.toml

[channels]
default_channels = ["my-notifier"]
```

### Plugin discovery order

TA resolves plugins in this order. If two plugins share the same name, the first match wins:

1. `[[channels.external]]` in `daemon.toml` (inline config)
2. `.ta/plugins/channels/*/channel.toml` (project-local installs)
3. `~/.config/ta/plugins/channels/*/channel.toml` (user-global installs)

## Starter Templates

TA ships with working plugin skeletons in three languages. Copy one to get started:

```bash
cp -r templates/channel-plugins/python plugins/my-notifier
cp -r templates/channel-plugins/node   plugins/my-notifier
cp -r templates/channel-plugins/go     plugins/my-notifier
```

Edit `channel.toml` to set your plugin name, then implement the delivery logic in the main file.

## Reference

- [USAGE.md -- Channel Plugins](USAGE.md) -- full CLI reference for `ta plugin` and `ta config channels` commands
- [plugins-architecture-guidance.md](plugins-architecture-guidance.md) -- broader plugin architecture, event hooks, and design principles
- `plugins/ta-channel-slack/` -- production Rust plugin example (Slack Block Kit integration)
- `plugins/ta-channel-discord/` -- production Rust plugin example (Discord embeds with buttons)
- `plugins/ta-channel-email/` -- production Rust plugin example (SMTP delivery with IMAP polling)
- `templates/channel-plugins/` -- starter templates for Python, Node.js, and Go
