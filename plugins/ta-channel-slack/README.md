# ta-channel-slack

Slack channel plugin for [Trusted Autonomy](https://github.com/Trusted-Autonomy/TrustedAutonomy).

> **Send-only starter** — this plugin delivers agent questions to Slack as Block Kit messages with interactive buttons. Inbound handling (slash commands, button callbacks, Socket Mode) is not yet implemented; responses must be posted back to the TA daemon via the HTTP API. Full inbound support is planned for a future release.

## What it does

- Posts `ta_ask_human` requests to a Slack channel as rich Block Kit messages
- Renders yes/no, multiple-choice, and freeform prompts with buttons
- Long context is posted as a thread reply to keep the channel readable
- Returns the Slack message timestamp as the `delivery_id` for traceability

## What it does NOT do (yet)

- Slash commands (`/ta status`, `/ta goal list`) — requires Socket Mode or an HTTP interaction endpoint
- Button-click callbacks — Slack requires an HTTPS interaction endpoint; this is planned for a future release
- Inbound message listening

For now, reviewers see the question in Slack and must respond via the TA web UI or another channel.

## Quick install (recommended)

Declare the plugin in your project's `.ta/project.toml`:

```toml
[plugins.slack]
type    = "channel"
version = ">=0.1.0"
source  = "registry:ta-channel-slack"
env_vars = ["TA_SLACK_BOT_TOKEN", "TA_SLACK_CHANNEL_ID"]
```

Then run:

```bash
ta setup resolve
```

TA downloads the pre-built binary for your platform, installs it to `.ta/plugins/channels/slack/`, and checks that the required env vars are set.

## Manual install

### 1. Create a Slack app

1. Go to [api.slack.com/apps](https://api.slack.com/apps) → **Create New App → From scratch**.
2. Under **OAuth & Permissions**, add the `chat:write` bot scope.
3. Click **Install to Workspace** and copy the **Bot User OAuth Token** (`xoxb-...`).
4. In Slack, invite the bot to your target channel: `/invite @YourBotName`.
5. Right-click the channel name → **Copy Link** to find the channel ID in the URL (the `C0...` part), or use the Slack API.

### 2. Set environment variables

```bash
export TA_SLACK_BOT_TOKEN="xoxb-your-bot-token"
export TA_SLACK_CHANNEL_ID="C01ABC23DEF"

# Optional: restrict which Slack users can respond via the HTTP API
export TA_SLACK_ALLOWED_USERS="U01ABC,U02DEF"
```

### 3. Download or build the binary

**Download** (pre-built):

```bash
# macOS (Apple Silicon)
curl -LO https://github.com/Trusted-Autonomy/ta-channel-slack/releases/latest/download/ta-channel-slack-0.1.0-aarch64-apple-darwin.tar.gz
tar xzf ta-channel-slack-0.1.0-aarch64-apple-darwin.tar.gz

# Linux x86_64
curl -LO https://github.com/Trusted-Autonomy/ta-channel-slack/releases/latest/download/ta-channel-slack-0.1.0-x86_64-unknown-linux-musl.tar.gz
tar xzf ta-channel-slack-0.1.0-x86_64-unknown-linux-musl.tar.gz
```

**Build from source**:

```bash
cargo build --release
```

### 4. Install the plugin

```bash
mkdir -p .ta/plugins/channels/slack
cp target/release/ta-channel-slack .ta/plugins/channels/slack/
cp channel.toml .ta/plugins/channels/slack/
```

Or use TA's plugin command:

```bash
ta plugin install .
```

### 5. Configure the daemon

```toml
# .ta/daemon.toml
[channels]
default_channels = ["slack"]
```

### 6. Verify

```bash
ta plugin validate
ta config channels
```

`ta config channels` should list `slack` with the `json-stdio` protocol.

## Environment variables

| Variable | Required | Description |
|---|---|---|
| `TA_SLACK_BOT_TOKEN` | Yes | Slack Bot User OAuth Token (`xoxb-...`) |
| `TA_SLACK_CHANNEL_ID` | Yes | Target Slack channel ID (`C01...`) |
| `TA_SLACK_ALLOWED_USERS` | No | Comma-separated Slack user IDs allowed to respond via the HTTP API |

## daemon.toml snippet

```toml
[channels]
default_channels = ["slack"]

[[channels.external]]
name     = "slack"
command  = "/path/to/ta-channel-slack"
protocol = "json-stdio"
timeout_secs = 30
```

## How responses work (send-only mode)

Because inbound callbacks are not yet implemented, reviewers respond through one of:

1. **TA web UI** — open `http://localhost:7700` and approve/deny from there.
2. **Another channel** — configure a second channel (e.g., `terminal`) alongside Slack.
3. **HTTP API** — POST directly to the daemon:
   ```bash
   curl -X POST http://localhost:7700/api/interactions/{interaction_id}/respond \
     -H "Content-Type: application/json" \
     -H "X-TA-Human-Credential: $TA_HUMAN_CREDENTIAL" \
     -d '{"answer": "approved"}'
   ```
   The daemon only accepts answers from a caller that presents the human credential created by `ta login` (use `ta login --file` so a script can read it). Without it the call is refused with a message that says to run `ta login`.

The `interaction_id` is included in every Slack message as part of the embed footer.

## Roadmap

- **Inbound callbacks** (button clicks, Socket Mode) — planned for a future release
- **Slash commands** (`/ta status`, `/ta goal list`) — planned for a future release

## Troubleshooting

**Bot doesn't post messages**: Check `TA_SLACK_BOT_TOKEN` and `TA_SLACK_CHANNEL_ID`. Ensure the bot is invited to the channel and has the `chat:write` scope.

**`ta plugin validate` fails**: Run `ta plugin logs slack` to see stderr output from the plugin process.

**`channel_not_found` error**: The bot is not a member of the channel. Run `/invite @YourBotName` in Slack.

## License

Apache-2.0 — see [LICENSE](https://github.com/Trusted-Autonomy/TrustedAutonomy/blob/main/LICENSE).
