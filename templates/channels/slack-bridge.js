#!/usr/bin/env node
/**
 * TA Slack Review Bridge
 *
 * Translates between TA's file-based WebhookChannel protocol and the
 * Slack API. Posts Block Kit messages with Approve/Deny buttons and
 * handles interactive responses.
 *
 * Environment variables:
 *   TA_SLACK_BOT_TOKEN       — Slack Bot User OAuth Token, xoxb-... (required)
 *   TA_SLACK_SIGNING_SECRET  — Slack app signing secret (required for HTTP mode)
 *   TA_SLACK_CHANNEL_ID      — Channel ID, e.g. C0123456789 (required)
 *   TA_SLACK_BRIDGE_PORT     — HTTP port for interactivity (default: 3141)
 *   TA_SLACK_APP_TOKEN       — App-level token, xapp-... (for Socket Mode)
 *   TA_EXCHANGE_DIR          — Path to .ta/channel-exchange
 *   TA_ALLOWED_REVIEWERS     — Comma-separated Slack user IDs (optional)
 *
 * Usage:
 *   npm install @slack/bolt chokidar
 *   export TA_SLACK_BOT_TOKEN=... TA_SLACK_SIGNING_SECRET=... TA_SLACK_CHANNEL_ID=...
 *   node slack-bridge.js
 *
 * For Socket Mode (no public URL needed):
 *   export TA_SLACK_APP_TOKEN=xapp-...
 *
 * See docs/guides/slack-channel.md for full setup instructions.
 */

const { App } = require('@slack/bolt');
const chokidar = require('chokidar');
const fs = require('fs');
const path = require('path');

// ── Configuration ───────────────────────────────────────────────
const SLACK_BOT_TOKEN      = process.env.TA_SLACK_BOT_TOKEN;
const SLACK_SIGNING_SECRET = process.env.TA_SLACK_SIGNING_SECRET;
const SLACK_APP_TOKEN      = process.env.TA_SLACK_APP_TOKEN;
const CHANNEL_ID           = process.env.TA_SLACK_CHANNEL_ID;
const BRIDGE_PORT          = parseInt(process.env.TA_SLACK_BRIDGE_PORT || '3141');
const EXCHANGE_DIR         = process.env.TA_EXCHANGE_DIR
                             || path.join(process.cwd(), '.ta', 'channel-exchange');
const ALLOWED_REVIEWERS    = (process.env.TA_ALLOWED_REVIEWERS || '')
  .split(',').map(s => s.trim()).filter(Boolean);

if (!SLACK_BOT_TOKEN || !CHANNEL_ID) {
  console.error('Required: TA_SLACK_BOT_TOKEN, TA_SLACK_CHANNEL_ID');
  process.exit(1);
}
if (!SLACK_APP_TOKEN && !SLACK_SIGNING_SECRET) {
  console.error('Need either TA_SLACK_APP_TOKEN (Socket Mode) or TA_SLACK_SIGNING_SECRET (HTTP)');
  process.exit(1);
}

fs.mkdirSync(EXCHANGE_DIR, { recursive: true });

// ── Slack App ───────────────────────────────────────────────────
const appConfig = {
  token: SLACK_BOT_TOKEN,
  port: BRIDGE_PORT,
};

if (SLACK_APP_TOKEN) {
  appConfig.socketMode = true;
  appConfig.appToken = SLACK_APP_TOKEN;
  console.log('[ta-slack] Using Socket Mode (no public URL needed)');
} else {
  appConfig.signingSecret = SLACK_SIGNING_SECRET;
}

const app = new App(appConfig);

/** requestId -> { ts, filePath } */
const pending = new Map();

// ── Access Check ────────────────────────────────────────────────
function isAuthorized(userId) {
  if (ALLOWED_REVIEWERS.length === 0) return true;
  return ALLOWED_REVIEWERS.includes(userId);
}

// ── File Watcher ────────────────────────────────────────────────
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
      await postReview(request, filePath);
    } catch (err) {
      console.error(`[ta-slack] Error: ${err.message}`);
    }
  });
}

// ── Post Block Kit Message ──────────────────────────────────────
async function postReview(request, filePath) {
  const requestId = request.id
    || path.basename(filePath, '.json').replace('request-', '');

  const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  if (fs.existsSync(responsePath)) return;

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
    const list = request.artifacts
      .slice(0, 15)
      .map(a => `\`${a.path || a.resource_uri || a}\``)
      .join('\n');
    blocks.push({
      type: 'section',
      text: { type: 'mrkdwn', text: `*Files Changed (${request.artifacts.length}):*\n${list}` },
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
        action_id: 'ta_approve',
        value: requestId,
      },
      {
        type: 'button',
        text: { type: 'plain_text', text: '❌ Deny' },
        style: 'danger',
        action_id: 'ta_deny',
        value: requestId,
      },
    ],
  });

  const result = await app.client.chat.postMessage({
    channel: CHANNEL_ID,
    blocks,
    text: `Draft review: ${request.title || requestId}`,
  });

  pending.set(requestId, { ts: result.ts, filePath });
  console.log(`[ta-slack] Posted review ${requestId}`);
}

// ── Approve Handler ─────────────────────────────────────────────
app.action('ta_approve', async ({ body, ack, client }) => {
  await ack();
  const requestId = body.actions[0].value;
  const userId = body.user.id;
  const user = body.user.name || userId;

  if (!isAuthorized(userId)) {
    await client.chat.postEphemeral({
      channel: body.channel.id,
      user: userId,
      text: 'Not authorized to review.',
    });
    return;
  }

  writeResponse(requestId, {
    id: requestId,
    decision: 'approved',
    approved_by: user,
    approved_at: new Date().toISOString(),
    selection: 'all',
  });

  await client.chat.update({
    channel: body.channel.id,
    ts: body.message.ts,
    blocks: [{
      type: 'section',
      text: { type: 'mrkdwn', text: `✅ *Approved* by <@${userId}>` },
    }],
    text: `Approved by ${user}`,
  });

  pending.delete(requestId);
  console.log(`[ta-slack] Approved ${requestId} by ${user}`);
});

// ── Deny Handler ────────────────────────────────────────────────
app.action('ta_deny', async ({ body, ack, client }) => {
  await ack();
  const requestId = body.actions[0].value;

  if (!isAuthorized(body.user.id)) {
    await client.chat.postEphemeral({
      channel: body.channel.id,
      user: body.user.id,
      text: 'Not authorized to review.',
    });
    return;
  }

  await client.views.open({
    trigger_id: body.trigger_id,
    view: {
      type: 'modal',
      callback_id: 'ta_deny_modal',
      private_metadata: JSON.stringify({
        requestId,
        channelId: body.channel.id,
        ts: body.message.ts,
      }),
      title: { type: 'plain_text', text: 'Deny Draft' },
      blocks: [{
        type: 'input',
        block_id: 'reason_block',
        element: {
          type: 'plain_text_input',
          action_id: 'reason',
          multiline: true,
          placeholder: { type: 'plain_text', text: 'Why is this draft being denied?' },
        },
        label: { type: 'plain_text', text: 'Reason' },
      }],
      submit: { type: 'plain_text', text: 'Deny' },
    },
  });
});

// ── Deny Modal Submit ───────────────────────────────────────────
app.view('ta_deny_modal', async ({ ack, body, view, client }) => {
  await ack();
  const { requestId, channelId, ts } = JSON.parse(view.private_metadata);
  const reason = view.state.values.reason_block.reason.value;
  const user = body.user.name || body.user.id;

  writeResponse(requestId, {
    id: requestId,
    decision: 'denied',
    denied_by: user,
    reason,
  });

  await client.chat.update({
    channel: channelId,
    ts,
    blocks: [{
      type: 'section',
      text: { type: 'mrkdwn', text: `❌ *Denied* by <@${body.user.id}>: ${reason}` },
    }],
    text: `Denied by ${user}: ${reason}`,
  });

  pending.delete(requestId);
  console.log(`[ta-slack] Denied ${requestId} by ${user}`);
});

// ── Helpers ─────────────────────────────────────────────────────
function writeResponse(requestId, response) {
  const p = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  fs.writeFileSync(p, JSON.stringify(response, null, 2));
}

// ── Start ───────────────────────────────────────────────────────
(async () => {
  await app.start();
  console.log(`[ta-slack] Bridge running on port ${BRIDGE_PORT}`);
  startWatcher();
})();
