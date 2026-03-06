#!/usr/bin/env node
/**
 * TA Gmail Review Bridge
 *
 * Translates between TA's file-based WebhookChannel protocol and Gmail.
 * Sends review emails and polls inbox for APPROVE/DENY replies.
 *
 * Supports two modes:
 *   1. Gmail API (OAuth2) — richer features, requires Google Cloud project
 *   2. App Password (SMTP/IMAP) — simpler setup, set TA_GMAIL_MODE=simple
 *
 * Environment variables (OAuth mode):
 *   TA_REVIEWER_EMAIL     — Email address of the reviewer (required)
 *   TA_SENDER_EMAIL       — Sender email, defaults to reviewer (optional)
 *   TA_EXCHANGE_DIR       — Path to .ta/channel-exchange
 *   TA_GMAIL_POLL_INTERVAL — Inbox poll interval in ms (default: 30000)
 *   TA_GMAIL_CREDENTIALS  — Path to OAuth credentials.json
 *
 * Environment variables (Simple mode):
 *   TA_GMAIL_MODE=simple
 *   TA_GMAIL_ADDRESS      — Gmail address (required)
 *   TA_GMAIL_APP_PASSWORD  — Gmail app password (required)
 *   TA_REVIEWER_EMAIL     — Reviewer email (defaults to TA_GMAIL_ADDRESS)
 *
 * Usage:
 *   npm install googleapis chokidar nodemailer imap-simple mailparser
 *   export TA_REVIEWER_EMAIL=you@gmail.com
 *   node gmail-bridge.js
 *
 * See docs/guides/gmail-channel.md for full setup instructions.
 */

const fs = require('fs');
const path = require('path');
const http = require('http');
const chokidar = require('chokidar');

// ── Configuration ───────────────────────────────────────────────
const MODE           = process.env.TA_GMAIL_MODE || 'oauth';
const EXCHANGE_DIR   = process.env.TA_EXCHANGE_DIR
                       || path.join(process.cwd(), '.ta', 'channel-exchange');
const POLL_INTERVAL  = parseInt(process.env.TA_GMAIL_POLL_INTERVAL || '30000');

fs.mkdirSync(EXCHANGE_DIR, { recursive: true });

/** requestId -> { threadId|subject, ... } */
const pending = new Map();

// ── Shared Helpers ──────────────────────────────────────────────
function writeResponse(requestId, response) {
  const p = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  fs.writeFileSync(p, JSON.stringify(response, null, 2));
  console.log(`[ta-gmail] ${response.decision} ${requestId}`);
}

function extractRequestId(filePath) {
  return path.basename(filePath, '.json').replace('request-', '');
}

function responsePath(requestId) {
  return path.join(EXCHANGE_DIR, `response-${requestId}.json`);
}

function buildEmailBody(request, requestId) {
  let body = `A draft is ready for your review.\n\n`;
  body += `Draft ID: ${requestId}\n`;
  if (request.summary) body += `Summary: ${request.summary}\n`;
  if (request.artifacts && request.artifacts.length > 0) {
    body += `\nFiles Changed (${request.artifacts.length}):\n`;
    request.artifacts.slice(0, 30).forEach(a => {
      body += `  - ${a.path || a.resource_uri || a}\n`;
    });
  }
  body += `\n---\n\nReply with one of:\n  APPROVE\n  DENY: <reason>\n`;
  return body;
}

function parseReply(text) {
  const fresh = text
    .split('\n')
    .filter(l => !l.startsWith('>') && !l.match(/^On .+ wrote:$/))
    .join('\n')
    .trim();
  if (!fresh) return null;

  const first = fresh.split('\n')[0].trim().toUpperCase();
  if (first.startsWith('APPROVE')) return { decision: 'approved' };
  if (first.startsWith('DENY')) {
    const reason = fresh.replace(/^DENY:?\s*/i, '').trim() || 'Denied via email';
    return { decision: 'denied', reason };
  }
  return null;
}

// ── File Watcher ────────────────────────────────────────────────
function startWatcher(sendFn) {
  const watcher = chokidar.watch(path.join(EXCHANGE_DIR, 'request-*.json'), {
    persistent: true,
    ignoreInitial: false,
    awaitWriteFinish: { stabilityThreshold: 500 },
  });
  watcher.on('add', async (filePath) => {
    try {
      const raw = JSON.parse(fs.readFileSync(filePath, 'utf-8'));
      const requestId = raw.id || extractRequestId(filePath);
      if (fs.existsSync(responsePath(requestId))) return;
      await sendFn(raw, requestId);
    } catch (err) {
      console.error(`[ta-gmail] Error: ${err.message}`);
    }
  });
}

// ════════════════════════════════════════════════════════════════
// OAuth Mode (Gmail API)
// ════════════════════════════════════════════════════════════════
async function runOAuthMode() {
  const { google } = require('googleapis');

  const CREDENTIALS_PATH = process.env.TA_GMAIL_CREDENTIALS
                           || path.join(__dirname, 'credentials.json');
  const TOKEN_PATH       = path.join(__dirname, 'token.json');
  const REVIEWER_EMAIL   = process.env.TA_REVIEWER_EMAIL;
  const SENDER_EMAIL     = process.env.TA_SENDER_EMAIL || REVIEWER_EMAIL;

  if (!REVIEWER_EMAIL) { console.error('Set TA_REVIEWER_EMAIL'); process.exit(1); }

  const SCOPES = [
    'https://www.googleapis.com/auth/gmail.send',
    'https://www.googleapis.com/auth/gmail.readonly',
    'https://www.googleapis.com/auth/gmail.modify',
  ];

  // Authorize
  const creds = JSON.parse(fs.readFileSync(CREDENTIALS_PATH, 'utf-8'));
  const { client_id, client_secret } = creds.installed || creds.web;
  const oAuth2 = new google.auth.OAuth2(client_id, client_secret, 'http://localhost:3142/oauth2callback');

  if (fs.existsSync(TOKEN_PATH)) {
    oAuth2.setCredentials(JSON.parse(fs.readFileSync(TOKEN_PATH, 'utf-8')));
  } else {
    const authUrl = oAuth2.generateAuthUrl({ access_type: 'offline', scope: SCOPES });
    console.log('[ta-gmail] Authorize:', authUrl);
    const code = await new Promise((resolve) => {
      const server = http.createServer((req, res) => {
        const c = new URL(req.url, 'http://localhost:3142').searchParams.get('code');
        if (c) { res.end('Done! Close this tab.'); server.close(); resolve(c); }
      });
      server.listen(3142);
    });
    const { tokens } = await oAuth2.getToken(code);
    oAuth2.setCredentials(tokens);
    fs.writeFileSync(TOKEN_PATH, JSON.stringify(tokens));
  }

  const gmail = google.gmail({ version: 'v1', auth: oAuth2 });

  // Send review email
  async function sendReview(request, requestId) {
    const subject = `[TA Review] ${request.title || requestId}`;
    const body = buildEmailBody(request, requestId);
    const raw = Buffer.from(
      `From: ${SENDER_EMAIL}\nTo: ${REVIEWER_EMAIL}\nSubject: ${subject}\n` +
      `Content-Type: text/plain; charset=utf-8\nX-TA-Request-ID: ${requestId}\n\n${body}`
    ).toString('base64url');

    const result = await gmail.users.messages.send({ userId: 'me', requestBody: { raw } });
    const sent = await gmail.users.messages.get({ userId: 'me', id: result.data.id });
    pending.set(requestId, { threadId: sent.data.threadId, subject });
    console.log(`[ta-gmail] Sent review ${requestId} to ${REVIEWER_EMAIL}`);
  }

  // Poll for replies
  async function pollReplies() {
    for (const [requestId, info] of pending) {
      if (fs.existsSync(responsePath(requestId))) { pending.delete(requestId); continue; }
      try {
        const thread = await gmail.users.threads.get({ userId: 'me', id: info.threadId });
        const msgs = thread.data.messages || [];
        if (msgs.length < 2) continue;

        const reply = msgs[msgs.length - 1];
        let text = '';
        const pl = reply.payload;
        if (pl.body && pl.body.data) {
          text = Buffer.from(pl.body.data, 'base64').toString('utf-8');
        } else if (pl.parts) {
          const tp = pl.parts.find(p => p.mimeType === 'text/plain');
          if (tp && tp.body && tp.body.data)
            text = Buffer.from(tp.body.data, 'base64').toString('utf-8');
        }

        const parsed = parseReply(text);
        if (!parsed) continue;

        const from = (reply.payload.headers || []).find(h => h.name.toLowerCase() === 'from');
        const reviewer = from ? from.value : 'email';

        if (parsed.decision === 'approved') {
          writeResponse(requestId, {
            id: requestId, decision: 'approved',
            approved_by: reviewer, approved_at: new Date().toISOString(), selection: 'all',
          });
        } else {
          writeResponse(requestId, {
            id: requestId, decision: 'denied', denied_by: reviewer, reason: parsed.reason,
          });
        }
        pending.delete(requestId);

        await gmail.users.messages.modify({
          userId: 'me', id: reply.id,
          requestBody: { removeLabelIds: ['UNREAD'] },
        });
      } catch (err) {
        console.error(`[ta-gmail] Poll error for ${requestId}: ${err.message}`);
      }
    }
  }

  console.log(`[ta-gmail] OAuth mode. Watching ${EXCHANGE_DIR}`);
  startWatcher(sendReview);
  setInterval(pollReplies, POLL_INTERVAL);
}

// ════════════════════════════════════════════════════════════════
// Simple Mode (SMTP/IMAP with App Password)
// ════════════════════════════════════════════════════════════════
async function runSimpleMode() {
  const nodemailer = require('nodemailer');
  const imapSimple = require('imap-simple');

  const EMAIL        = process.env.TA_GMAIL_ADDRESS;
  const APP_PASSWORD = process.env.TA_GMAIL_APP_PASSWORD;
  const REVIEWER     = process.env.TA_REVIEWER_EMAIL || EMAIL;

  if (!EMAIL || !APP_PASSWORD) {
    console.error('Set TA_GMAIL_ADDRESS and TA_GMAIL_APP_PASSWORD');
    process.exit(1);
  }

  const transporter = nodemailer.createTransport({
    service: 'gmail',
    auth: { user: EMAIL, pass: APP_PASSWORD },
  });

  const imapConfig = {
    imap: {
      user: EMAIL, password: APP_PASSWORD,
      host: 'imap.gmail.com', port: 993, tls: true, authTimeout: 10000,
    },
  };

  async function sendReview(request, requestId) {
    const subject = `[TA Review] ${request.title || requestId}`;
    const body = buildEmailBody(request, requestId);
    await transporter.sendMail({ from: EMAIL, to: REVIEWER, subject, text: body });
    pending.set(requestId, { subject });
    console.log(`[ta-gmail] Sent review ${requestId} to ${REVIEWER}`);
  }

  async function pollReplies() {
    if (pending.size === 0) return;
    try {
      const conn = await imapSimple.connect(imapConfig);
      await conn.openBox('INBOX');

      for (const [requestId, info] of pending) {
        if (fs.existsSync(responsePath(requestId))) { pending.delete(requestId); continue; }

        const results = await conn.search(
          [['SUBJECT', info.subject], ['UNSEEN']],
          { bodies: ['TEXT'], markSeen: true }
        );

        for (const msg of results) {
          const text = msg.parts.find(p => p.which === 'TEXT')?.body || '';
          const parsed = parseReply(text);
          if (!parsed) continue;

          if (parsed.decision === 'approved') {
            writeResponse(requestId, {
              id: requestId, decision: 'approved',
              approved_by: REVIEWER, approved_at: new Date().toISOString(), selection: 'all',
            });
          } else {
            writeResponse(requestId, {
              id: requestId, decision: 'denied', denied_by: REVIEWER, reason: parsed.reason,
            });
          }
          pending.delete(requestId);
        }
      }
      await conn.end();
    } catch (err) {
      console.error(`[ta-gmail] IMAP error: ${err.message}`);
    }
  }

  console.log(`[ta-gmail] Simple mode (SMTP/IMAP). Watching ${EXCHANGE_DIR}`);
  startWatcher(sendReview);
  setInterval(pollReplies, POLL_INTERVAL);
}

// ── Main ────────────────────────────────────────────────────────
if (MODE === 'simple') {
  runSimpleMode();
} else {
  runOAuthMode();
}
