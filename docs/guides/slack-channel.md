# Setting Up a Slack Review Channel for TA

This guide covers how to connect Trusted Autonomy to a Slack workspace so that
draft reviews, approvals, and notifications appear as Slack messages with
interactive Block Kit buttons.

---

## Architecture Overview

```
Agent works in staging
        │
        ▼
  TA MCP Gateway
        │  InteractionRequest (review/notify)
        ▼
  WebhookChannel (file-based)
        │  writes .ta/channel-exchange/request-{id}.json
        ▼
  ta-slack-bridge (Node.js service)
        │  reads request → posts Block Kit message to Slack channel
        │  receives button click via Slack interactivity webhook
        │  writes .ta/channel-exchange/response-{id}.json
        ▼
  WebhookChannel picks up response
        │
        ▼
  Agent continues or revises
```

Slack's interactivity model uses HTTP webhooks (unlike Discord's gateway),
so the bridge needs to run an HTTP server to receive button clicks.

---

## Part 1: Slack App Setup

### 1.1 Create a Slack App

1. Go to https://api.slack.com/apps
2. Click **Create New App** → **From scratch**
3. Name: `TA Review Bot`, pick your workspace
4. Click **Create App**

### 1.2 Configure Bot Permissions

Go to **OAuth & Permissions** → **Scopes** → **Bot Token Scopes**, add:

- `chat:write` — post messages
- `chat:write.public` — post to channels without joining
- `files:write` — upload diff attachments (optional)

### 1.3 Enable Interactivity

Go to **Interactivity & Shortcuts**:

1. Toggle **Interactivity** to **On**
2. Set **Request URL** to your bridge's public endpoint:
   - Local dev: use ngrok (`ngrok http 3141`) → `https://xxxx.ngrok.io/slack/actions`
   - Production: your server's URL + `/slack/actions`
3. Click **Save Changes**

### 1.4 Install to Workspace

Go to **Install App**:

1. Click **Install to Workspace**
2. Authorize the requested permissions
3. Copy the **Bot User OAuth Token** (`xoxb-...`)

### 1.5 Create a Review Channel

In Slack:

1. Create a channel called `#ta-reviews`
2. Invite the bot: `/invite @TA Review Bot`
3. Copy the **Channel ID**: click the channel name → scroll to bottom of the
   modal → copy the ID (starts with `C`)

### 1.6 Get the Signing Secret

Go to **Basic Information** → **App Credentials**:

- Copy the **Signing Secret** — used to verify incoming webhook requests

---

## Part 2: Bridge Service Setup

### 2.1 Install the Bridge

```bash
mkdir -p .ta/bridges/slack
cd .ta/bridges/slack

npm init -y
npm install @slack/web-api @slack/bolt chokidar
```

### 2.2 Create the Bridge Script

Copy from `templates/channels/slack-bridge.js` or create `.ta/bridges/slack/bridge.js`:

```javascript
// See templates/channels/slack-bridge.js for the full implementation
const { App } = require('@slack/bolt');
const chokidar = require('chokidar');
const fs = require('fs');
const path = require('path');

// --- Configuration ---
const SLACK_BOT_TOKEN    = process.env.TA_SLACK_BOT_TOKEN;
const SLACK_SIGNING_SECRET = process.env.TA_SLACK_SIGNING_SECRET;
const CHANNEL_ID         = process.env.TA_SLACK_CHANNEL_ID;
const BRIDGE_PORT        = parseInt(process.env.TA_SLACK_BRIDGE_PORT || '3141');
const EXCHANGE_DIR       = process.env.TA_EXCHANGE_DIR
                           || path.join(process.cwd(), '.ta', 'channel-exchange');

if (!SLACK_BOT_TOKEN || !SLACK_SIGNING_SECRET || !CHANNEL_ID) {
  console.error('Set TA_SLACK_BOT_TOKEN, TA_SLACK_SIGNING_SECRET, TA_SLACK_CHANNEL_ID');
  process.exit(1);
}

fs.mkdirSync(EXCHANGE_DIR, { recursive: true });

const app = new App({
  token: SLACK_BOT_TOKEN,
  signingSecret: SLACK_SIGNING_SECRET,
  port: BRIDGE_PORT,
});

// Track pending reviews: requestId -> { ts (Slack message timestamp) }
const pending = new Map();

// --- Watch for TA review requests ---
function startWatcher() {
  const watcher = chokidar.watch(path.join(EXCHANGE_DIR, 'request-*.json'), {
    persistent: true,
    ignoreInitial: false,
    awaitWriteFinish: { stabilityThreshold: 500 },
  });

  watcher.on('add', async (filePath) => {
    try {
      const raw = fs.readFileSync(filePath, 'utf-8');
      const request = JSON.parse(raw);
      await postReviewRequest(request, filePath);
    } catch (err) {
      console.error(`[ta-slack] Error processing ${filePath}:`, err);
    }
  });
}

async function postReviewRequest(request, filePath) {
  const requestId = request.id || path.basename(filePath, '.json').replace('request-', '');

  // Skip if already responded
  const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  if (fs.existsSync(responsePath)) return;

  // Build Block Kit message
  const blocks = [
    {
      type: 'header',
      text: { type: 'plain_text', text: `📋 Draft Review: ${request.title || 'Untitled'}` },
    },
  ];

  if (request.summary) {
    blocks.push({
      type: 'section',
      text: { type: 'mrkdwn', text: request.summary },
    });
  }

  if (request.artifacts && request.artifacts.length > 0) {
    const fileList = request.artifacts
      .slice(0, 15)
      .map(a => `\`${a.path || a.resource_uri || a}\``)
      .join('\n');
    blocks.push({
      type: 'section',
      text: {
        type: 'mrkdwn',
        text: `*Files Changed (${request.artifacts.length}):*\n${fileList}`,
      },
    });
  }

  blocks.push({ type: 'divider' });
  blocks.push({
    type: 'actions',
    elements: [
      {
        type: 'button',
        text: { type: 'plain_text', text: '✅ Approve' },
        style: 'primary',
        action_id: `ta_approve`,
        value: requestId,
      },
      {
        type: 'button',
        text: { type: 'plain_text', text: '❌ Deny' },
        style: 'danger',
        action_id: `ta_deny`,
        value: requestId,
      },
    ],
  });

  const result = await app.client.chat.postMessage({
    channel: CHANNEL_ID,
    blocks: blocks,
    text: `Draft review: ${request.title || requestId}`,  // fallback
  });

  pending.set(requestId, { ts: result.ts, filePath });
  console.log(`[ta-slack] Posted review request ${requestId}`);
}

// --- Handle Approve button ---
app.action('ta_approve', async ({ body, ack, client }) => {
  await ack();
  const requestId = body.actions[0].value;
  const user = body.user.name || body.user.id;

  writeResponse(requestId, {
    id: requestId,
    decision: 'approved',
    approved_by: user,
    approved_at: new Date().toISOString(),
    selection: 'all',
  });

  // Update the original message
  await client.chat.update({
    channel: body.channel.id,
    ts: body.message.ts,
    blocks: [
      {
        type: 'section',
        text: { type: 'mrkdwn', text: `✅ *Approved* by <@${body.user.id}>` },
      },
    ],
    text: `Approved by ${user}`,
  });

  pending.delete(requestId);
  console.log(`[ta-slack] Approved ${requestId} by ${user}`);
});

// --- Handle Deny button ---
app.action('ta_deny', async ({ body, ack, client }) => {
  await ack();
  const requestId = body.actions[0].value;

  // Open a modal for denial reason
  await client.views.open({
    trigger_id: body.trigger_id,
    view: {
      type: 'modal',
      callback_id: `ta_deny_modal`,
      private_metadata: JSON.stringify({ requestId, channelId: body.channel.id, ts: body.message.ts }),
      title: { type: 'plain_text', text: 'Deny Draft' },
      blocks: [
        {
          type: 'input',
          block_id: 'reason_block',
          element: {
            type: 'plain_text_input',
            action_id: 'reason',
            multiline: true,
            placeholder: { type: 'plain_text', text: 'Why is this draft being denied?' },
          },
          label: { type: 'plain_text', text: 'Reason' },
        },
      ],
      submit: { type: 'plain_text', text: 'Deny' },
    },
  });
});

// --- Handle deny modal submission ---
app.view('ta_deny_modal', async ({ ack, body, view, client }) => {
  await ack();
  const { requestId, channelId, ts } = JSON.parse(view.private_metadata);
  const reason = view.state.values.reason_block.reason.value;
  const user = body.user.name || body.user.id;

  writeResponse(requestId, {
    id: requestId,
    decision: 'denied',
    denied_by: user,
    reason: reason,
  });

  // Update the original message
  await client.chat.update({
    channel: channelId,
    ts: ts,
    blocks: [
      {
        type: 'section',
        text: { type: 'mrkdwn', text: `❌ *Denied* by <@${body.user.id}>: ${reason}` },
      },
    ],
    text: `Denied by ${user}: ${reason}`,
  });

  pending.delete(requestId);
  console.log(`[ta-slack] Denied ${requestId} by ${user}: ${reason}`);
});

function writeResponse(requestId, response) {
  const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  fs.writeFileSync(responsePath, JSON.stringify(response, null, 2));
}

// --- Start ---
(async () => {
  await app.start();
  console.log(`[ta-slack] Bridge running on port ${BRIDGE_PORT}`);
  console.log(`[ta-slack] Watching ${EXCHANGE_DIR} for review requests`);
  startWatcher();
})();
```

### 2.3 Create Environment File

Create `.ta/bridges/slack/.env`:

```bash
TA_SLACK_BOT_TOKEN=xoxb-your-bot-token
TA_SLACK_SIGNING_SECRET=your-signing-secret
TA_SLACK_CHANNEL_ID=C0123456789
TA_SLACK_BRIDGE_PORT=3141
TA_EXCHANGE_DIR=/path/to/your/project/.ta/channel-exchange
```

> **Security**: Add `.ta/bridges/slack/.env` to `.gitignore`. Never commit tokens.

### 2.4 Expose the Bridge (for Interactivity)

Slack sends button clicks via HTTP POST to your bridge. For local development:

```bash
# Install ngrok (https://ngrok.com)
ngrok http 3141
```

Copy the HTTPS URL (e.g., `https://abc123.ngrok.io`) and set it in your Slack
app's Interactivity settings as `https://abc123.ngrok.io/slack/events`.

For production, deploy the bridge behind a reverse proxy with a stable URL.

---

## Part 3: TA Configuration

### 3.1 Configure the Webhook Channel

Edit `.ta/config.yaml`:

```yaml
channels:
  review:
    type: webhook
    endpoint: .ta/channel-exchange
    timeout_seconds: 3600
    poll_interval_ms: 2000
  notify:
    - type: webhook
      endpoint: .ta/channel-exchange
  session:
    type: terminal
  default_agent: claude-code
```

### 3.2 Start Everything

```bash
# Terminal 1: Start ngrok (if local)
ngrok http 3141

# Terminal 2: Start the Slack bridge
cd .ta/bridges/slack
source .env
node bridge.js

# Terminal 3: Run your agent
ta run "implement feature X" --source .
```

### 3.3 Verify

1. The agent runs and eventually calls `ta_draft` with `action: "submit"`
2. A Block Kit message appears in `#ta-reviews`
3. Click **Approve** or **Deny**
4. TA picks up the response and the agent continues or revises

---

## Part 4: Production Considerations

### 4.1 Webhook URL Stability

Slack requires a stable HTTPS endpoint for interactivity. Options:

- **ngrok** with a reserved domain (paid plan)
- **Cloudflare Tunnel**: `cloudflared tunnel --url http://localhost:3141`
- **Deploy to a server**: any host with Node.js + a reverse proxy (nginx/caddy)
- **Slack Socket Mode**: eliminates the need for a public URL entirely

#### Using Socket Mode (Recommended for Solo Use)

In your Slack app settings:
1. Go to **Socket Mode** → enable it
2. Generate an **App-Level Token** with `connections:write` scope

Then modify the bridge to use socket mode:

```javascript
const app = new App({
  token: SLACK_BOT_TOKEN,
  signingSecret: SLACK_SIGNING_SECRET,
  socketMode: true,
  appToken: process.env.TA_SLACK_APP_TOKEN,  // xapp-...
});
```

Socket Mode connects outbound (no public URL needed), which is perfect for
a local development machine running TA.

### 4.2 Thread-Based Reviews

For a cleaner channel, post the initial review as a message, then use threads
for the diff details:

```javascript
// After posting the main message, add diff as a thread reply
await client.chat.postMessage({
  channel: CHANNEL_ID,
  thread_ts: result.ts,
  text: `\`\`\`\n${diffContent}\n\`\`\``,
});
```

### 4.3 Access Control

Add reviewer restrictions in the button handlers:

```javascript
const ALLOWED_REVIEWERS = ['U01234567', 'U89ABCDEF'];
if (!ALLOWED_REVIEWERS.includes(body.user.id)) {
  // Respond ephemerally
  await client.chat.postEphemeral({
    channel: body.channel.id,
    user: body.user.id,
    text: 'You are not authorized to review drafts.',
  });
  return;
}
```

---

## Troubleshooting

| Problem | Solution |
|---|---|
| Bot doesn't post | Check `TA_SLACK_BOT_TOKEN`. Verify bot is invited to the channel. Check `chat:write` scope. |
| Buttons don't work | Verify Interactivity URL matches your bridge. Check ngrok is running. Try Socket Mode instead. |
| "dispatch_failed" error | The Request URL in Slack settings doesn't match where the bridge is listening. |
| Duplicate messages | The bridge processes existing request files on startup. Add a dedup check or clear old requests. |
| Modal doesn't open | Verify `trigger_id` is being passed. Modals must open within 3 seconds of the button click. |

---

## Native Channel Implementation (Future)

A native `SlackChannelFactory` would use the `slack-morphism` Rust crate to
implement `ReviewChannel` directly, eliminating the bridge. Configuration:

```yaml
channels:
  review:
    type: slack
    bot_token_env: TA_SLACK_BOT_TOKEN
    channel_id: "C0123456789"
    socket_mode: true
    app_token_env: TA_SLACK_APP_TOKEN
```
