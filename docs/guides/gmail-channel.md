# Setting Up a Gmail Review Channel for TA

This guide covers how to connect Trusted Autonomy to Gmail so that
draft reviews arrive as emails and you can approve or deny by replying.

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
  ta-gmail-bridge (Node.js service)
        │  reads request → sends formatted email via Gmail API
        │  polls inbox for reply (APPROVE / DENY)
        │  writes .ta/channel-exchange/response-{id}.json
        ▼
  WebhookChannel picks up response
        │
        ▼
  Agent continues or revises
```

Gmail doesn't support interactive buttons like Discord or Slack, so the
review flow uses email replies with keyword-based commands.

---

## Part 1: Gmail API Setup

### 1.1 Create a Google Cloud Project

1. Go to https://console.cloud.google.com/
2. Click **Select a project** → **New Project**
3. Name: `TA Review Bridge`, click **Create**
4. Select the new project

### 1.2 Enable the Gmail API

1. Go to **APIs & Services** → **Library**
2. Search for **Gmail API**
3. Click **Enable**

### 1.3 Create OAuth Credentials

1. Go to **APIs & Services** → **Credentials**
2. Click **Create Credentials** → **OAuth client ID**
3. If prompted, configure the OAuth consent screen:
   - User Type: **External** (or Internal if Google Workspace)
   - App name: `TA Review Bridge`
   - Add your email as a test user
4. Application type: **Desktop app**
5. Name: `TA Gmail Bridge`
6. Click **Create**
7. Download the JSON file — save it as `.ta/bridges/gmail/credentials.json`

### 1.4 Alternative: App Password (Simpler)

If you prefer not to use OAuth, you can use a Gmail App Password:

1. Go to https://myaccount.google.com/apppasswords
   (requires 2-Step Verification enabled)
2. Generate an app password for "Mail" on "Other (TA Bridge)"
3. Save the 16-character password

This approach uses SMTP/IMAP instead of the Gmail API — simpler but less
feature-rich (no labels, no rich formatting).

---

## Part 2: Bridge Service Setup

### 2.1 Install the Bridge

```bash
mkdir -p .ta/bridges/gmail
cd .ta/bridges/gmail

npm init -y
npm install googleapis chokidar nodemailer imap-simple
```

### 2.2 Create the Bridge Script

Copy from `templates/channels/gmail-bridge.js` or create `.ta/bridges/gmail/bridge.js`:

```javascript
// See templates/channels/gmail-bridge.js for the full implementation
const { google } = require('googleapis');
const chokidar = require('chokidar');
const fs = require('fs');
const path = require('path');
const http = require('http');

// --- Configuration ---
const CREDENTIALS_PATH = process.env.TA_GMAIL_CREDENTIALS
                         || path.join(__dirname, 'credentials.json');
const TOKEN_PATH       = path.join(__dirname, 'token.json');
const REVIEWER_EMAIL   = process.env.TA_REVIEWER_EMAIL;
const SENDER_EMAIL     = process.env.TA_SENDER_EMAIL || REVIEWER_EMAIL;
const EXCHANGE_DIR     = process.env.TA_EXCHANGE_DIR
                         || path.join(process.cwd(), '.ta', 'channel-exchange');
const POLL_INTERVAL    = parseInt(process.env.TA_GMAIL_POLL_INTERVAL || '30000'); // 30s

if (!REVIEWER_EMAIL) {
  console.error('Set TA_REVIEWER_EMAIL');
  process.exit(1);
}

fs.mkdirSync(EXCHANGE_DIR, { recursive: true });

// Track pending reviews: requestId -> { threadId, subject }
const pending = new Map();

// --- OAuth2 Setup ---
const SCOPES = [
  'https://www.googleapis.com/auth/gmail.send',
  'https://www.googleapis.com/auth/gmail.readonly',
  'https://www.googleapis.com/auth/gmail.modify',
];

let auth;

async function authorize() {
  const credentials = JSON.parse(fs.readFileSync(CREDENTIALS_PATH, 'utf-8'));
  const { client_id, client_secret } = credentials.installed || credentials.web;

  const oAuth2Client = new google.auth.OAuth2(
    client_id,
    client_secret,
    'http://localhost:3142/oauth2callback'
  );

  // Check for existing token
  if (fs.existsSync(TOKEN_PATH)) {
    const token = JSON.parse(fs.readFileSync(TOKEN_PATH, 'utf-8'));
    oAuth2Client.setCredentials(token);
    auth = oAuth2Client;
    return;
  }

  // Interactive OAuth flow
  const authUrl = oAuth2Client.generateAuthUrl({
    access_type: 'offline',
    scope: SCOPES,
  });
  console.log('[ta-gmail] Authorize by visiting:', authUrl);

  // Start a temporary server to receive the OAuth callback
  const code = await new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      const url = new URL(req.url, 'http://localhost:3142');
      const code = url.searchParams.get('code');
      if (code) {
        res.end('Authorization successful! You can close this tab.');
        server.close();
        resolve(code);
      }
    });
    server.listen(3142);
  });

  const { tokens } = await oAuth2Client.getToken(code);
  oAuth2Client.setCredentials(tokens);
  fs.writeFileSync(TOKEN_PATH, JSON.stringify(tokens));
  auth = oAuth2Client;
  console.log('[ta-gmail] Token saved');
}

// --- Send Review Email ---
async function sendReviewEmail(request, requestId) {
  const gmail = google.gmail({ version: 'v1', auth });

  const subject = `[TA Review] ${request.title || requestId}`;

  let body = `A draft is ready for your review.\n\n`;
  body += `**Draft ID**: ${requestId}\n`;

  if (request.summary) {
    body += `**Summary**: ${request.summary}\n`;
  }

  if (request.artifacts && request.artifacts.length > 0) {
    body += `\n**Files Changed (${request.artifacts.length})**:\n`;
    request.artifacts.slice(0, 30).forEach(a => {
      body += `  - ${a.path || a.resource_uri || a}\n`;
    });
  }

  body += `\n---\n\n`;
  body += `To respond, reply to this email with one of:\n\n`;
  body += `  APPROVE\n`;
  body += `  APPROVE all\n`;
  body += `  DENY: <your reason here>\n\n`;
  body += `The first word of your reply determines the action.\n`;

  const message = [
    `From: ${SENDER_EMAIL}`,
    `To: ${REVIEWER_EMAIL}`,
    `Subject: ${subject}`,
    `Content-Type: text/plain; charset=utf-8`,
    `X-TA-Request-ID: ${requestId}`,
    '',
    body,
  ].join('\n');

  const encodedMessage = Buffer.from(message)
    .toString('base64')
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');

  const result = await gmail.users.messages.send({
    userId: 'me',
    requestBody: { raw: encodedMessage },
  });

  // Get the thread ID for tracking replies
  const sent = await gmail.users.messages.get({
    userId: 'me',
    id: result.data.id,
  });

  pending.set(requestId, {
    threadId: sent.data.threadId,
    subject: subject,
  });

  console.log(`[ta-gmail] Sent review email for ${requestId} to ${REVIEWER_EMAIL}`);
}

// --- Poll for Replies ---
async function pollForReplies() {
  if (pending.size === 0) return;

  const gmail = google.gmail({ version: 'v1', auth });

  for (const [requestId, info] of pending) {
    // Check if already responded (by another channel or earlier poll)
    const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
    if (fs.existsSync(responsePath)) {
      pending.delete(requestId);
      continue;
    }

    try {
      // Get thread messages
      const thread = await gmail.users.threads.get({
        userId: 'me',
        id: info.threadId,
      });

      // Look for replies (skip the first message which is our request)
      const messages = thread.data.messages || [];
      if (messages.length < 2) continue;

      // Check the latest reply
      const reply = messages[messages.length - 1];

      // Decode the reply body
      let replyText = '';
      const payload = reply.payload;
      if (payload.body && payload.body.data) {
        replyText = Buffer.from(payload.body.data, 'base64').toString('utf-8');
      } else if (payload.parts) {
        for (const part of payload.parts) {
          if (part.mimeType === 'text/plain' && part.body && part.body.data) {
            replyText = Buffer.from(part.body.data, 'base64').toString('utf-8');
            break;
          }
        }
      }

      // Strip quoted text (lines starting with >)
      const freshLines = replyText
        .split('\n')
        .filter(line => !line.startsWith('>') && !line.startsWith('On '))
        .join('\n')
        .trim();

      if (!freshLines) continue;

      // Parse the decision
      const firstLine = freshLines.split('\n')[0].trim().toUpperCase();
      const fromHeader = (reply.payload.headers || [])
        .find(h => h.name.toLowerCase() === 'from');
      const reviewer = fromHeader ? fromHeader.value : 'email-reviewer';

      if (firstLine.startsWith('APPROVE')) {
        writeResponse(requestId, {
          id: requestId,
          decision: 'approved',
          approved_by: reviewer,
          approved_at: new Date().toISOString(),
          selection: 'all',
        });
        console.log(`[ta-gmail] Approved ${requestId} by ${reviewer}`);
        pending.delete(requestId);
      } else if (firstLine.startsWith('DENY')) {
        const reason = freshLines.replace(/^DENY:?\s*/i, '').trim() || 'Denied via email';
        writeResponse(requestId, {
          id: requestId,
          decision: 'denied',
          denied_by: reviewer,
          reason: reason,
        });
        console.log(`[ta-gmail] Denied ${requestId} by ${reviewer}: ${reason}`);
        pending.delete(requestId);
      }

      // Mark the thread as read
      await gmail.users.messages.modify({
        userId: 'me',
        id: reply.id,
        requestBody: { removeLabelIds: ['UNREAD'] },
      });
    } catch (err) {
      console.error(`[ta-gmail] Error checking thread for ${requestId}:`, err.message);
    }
  }
}

function writeResponse(requestId, response) {
  const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  fs.writeFileSync(responsePath, JSON.stringify(response, null, 2));
}

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
      const requestId = request.id || path.basename(filePath, '.json').replace('request-', '');

      // Skip if already responded
      const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
      if (fs.existsSync(responsePath)) return;

      await sendReviewEmail(request, requestId);
    } catch (err) {
      console.error(`[ta-gmail] Error processing ${filePath}:`, err);
    }
  });
}

// --- Main ---
(async () => {
  await authorize();
  console.log(`[ta-gmail] Authorized. Watching ${EXCHANGE_DIR}`);
  startWatcher();

  // Poll for email replies
  setInterval(pollForReplies, POLL_INTERVAL);
  console.log(`[ta-gmail] Polling inbox every ${POLL_INTERVAL / 1000}s for replies`);
})();
```

### 2.3 Create Environment File

Create `.ta/bridges/gmail/.env`:

```bash
TA_REVIEWER_EMAIL=you@gmail.com
TA_SENDER_EMAIL=you@gmail.com
TA_EXCHANGE_DIR=/path/to/your/project/.ta/channel-exchange
TA_GMAIL_POLL_INTERVAL=30000
```

> **Security**: Add `.ta/bridges/gmail/.env`, `credentials.json`, and `token.json`
> to `.gitignore`. Never commit OAuth credentials.

### 2.4 First-Time Authorization

```bash
cd .ta/bridges/gmail
source .env
node bridge.js
```

On first run, the bridge will print a URL. Open it in your browser, authorize
the app, and the OAuth token will be saved to `token.json`. Subsequent runs
use the saved token automatically.

---

## Part 3: TA Configuration

### 3.1 Configure the Webhook Channel

Edit `.ta/config.yaml`:

```yaml
channels:
  review:
    type: webhook
    endpoint: .ta/channel-exchange
    timeout_seconds: 7200           # 2 hours — email is slower
    poll_interval_ms: 5000
  notify:
    - type: webhook
      endpoint: .ta/channel-exchange
  session:
    type: terminal
  default_agent: claude-code
```

### 3.2 Start Everything

```bash
# Terminal 1: Start the Gmail bridge
cd .ta/bridges/gmail
source .env
node bridge.js

# Terminal 2: Run your agent
ta run "implement feature X" --source .
```

### 3.3 Review Flow

1. Agent submits a draft for review
2. You receive an email with the draft summary and file list
3. Reply with `APPROVE` or `DENY: reason here`
4. The bridge polls your inbox, picks up the reply, writes the response
5. TA picks up the response and the agent continues

---

## Part 4: Alternative — App Password + IMAP/SMTP

If you don't want to set up the Gmail API, use IMAP/SMTP with an App Password:

### 4.1 Install Dependencies

```bash
npm install nodemailer imap-simple mailparser chokidar
```

### 4.2 Simple SMTP/IMAP Bridge

Create `.ta/bridges/gmail/bridge-simple.js`:

```javascript
const nodemailer = require('nodemailer');
const imapSimple = require('imap-simple');
const { simpleParser } = require('mailparser');
const chokidar = require('chokidar');
const fs = require('fs');
const path = require('path');

const EMAIL          = process.env.TA_GMAIL_ADDRESS;
const APP_PASSWORD   = process.env.TA_GMAIL_APP_PASSWORD;
const REVIEWER_EMAIL = process.env.TA_REVIEWER_EMAIL || EMAIL;
const EXCHANGE_DIR   = process.env.TA_EXCHANGE_DIR;
const POLL_INTERVAL  = parseInt(process.env.TA_GMAIL_POLL_INTERVAL || '30000');

// SMTP transport
const transporter = nodemailer.createTransport({
  service: 'gmail',
  auth: { user: EMAIL, pass: APP_PASSWORD },
});

// IMAP config
const imapConfig = {
  imap: {
    user: EMAIL,
    password: APP_PASSWORD,
    host: 'imap.gmail.com',
    port: 993,
    tls: true,
    authTimeout: 10000,
  },
};

const pending = new Map();

async function sendReview(request, requestId) {
  let body = `Draft review: ${request.title || requestId}\n\n`;
  if (request.summary) body += `${request.summary}\n\n`;
  if (request.artifacts) {
    body += `Files changed (${request.artifacts.length}):\n`;
    request.artifacts.slice(0, 30).forEach(a => {
      body += `  - ${a.path || a.resource_uri || a}\n`;
    });
  }
  body += `\n---\nReply APPROVE or DENY: <reason>\n`;

  const subject = `[TA Review] ${request.title || requestId}`;
  await transporter.sendMail({
    from: EMAIL,
    to: REVIEWER_EMAIL,
    subject: subject,
    text: body,
    headers: { 'X-TA-Request-ID': requestId },
  });

  pending.set(requestId, { subject });
  console.log(`[ta-gmail] Sent review ${requestId}`);
}

async function checkReplies() {
  if (pending.size === 0) return;
  try {
    const connection = await imapSimple.connect(imapConfig);
    await connection.openBox('INBOX');

    for (const [requestId, info] of pending) {
      const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
      if (fs.existsSync(responsePath)) { pending.delete(requestId); continue; }

      const results = await connection.search(
        [['SUBJECT', info.subject], ['UNSEEN']],
        { bodies: ['TEXT'], markSeen: true }
      );

      for (const msg of results) {
        const text = msg.parts.find(p => p.which === 'TEXT')?.body || '';
        const lines = text.split('\n')
          .filter(l => !l.startsWith('>') && !l.startsWith('On '))
          .join('\n').trim();
        const first = lines.split('\n')[0].trim().toUpperCase();

        if (first.startsWith('APPROVE')) {
          fs.writeFileSync(responsePath, JSON.stringify({
            id: requestId, decision: 'approved',
            approved_by: REVIEWER_EMAIL,
            approved_at: new Date().toISOString(),
            selection: 'all',
          }, null, 2));
          pending.delete(requestId);
        } else if (first.startsWith('DENY')) {
          const reason = lines.replace(/^DENY:?\s*/i, '').trim() || 'Denied';
          fs.writeFileSync(responsePath, JSON.stringify({
            id: requestId, decision: 'denied',
            denied_by: REVIEWER_EMAIL, reason,
          }, null, 2));
          pending.delete(requestId);
        }
      }
    }
    await connection.end();
  } catch (err) {
    console.error('[ta-gmail] IMAP error:', err.message);
  }
}

// Watch + poll
fs.mkdirSync(EXCHANGE_DIR, { recursive: true });
const watcher = chokidar.watch(path.join(EXCHANGE_DIR, 'request-*.json'), {
  persistent: true, ignoreInitial: false,
  awaitWriteFinish: { stabilityThreshold: 500 },
});
watcher.on('add', async (fp) => {
  const raw = JSON.parse(fs.readFileSync(fp, 'utf-8'));
  const id = raw.id || path.basename(fp, '.json').replace('request-', '');
  if (!fs.existsSync(path.join(EXCHANGE_DIR, `response-${id}.json`))) {
    await sendReview(raw, id);
  }
});
setInterval(checkReplies, POLL_INTERVAL);
console.log('[ta-gmail] Simple bridge started');
```

Environment for simple mode:

```bash
TA_GMAIL_ADDRESS=you@gmail.com
TA_GMAIL_APP_PASSWORD=abcd efgh ijkl mnop
TA_REVIEWER_EMAIL=you@gmail.com
TA_EXCHANGE_DIR=/path/to/project/.ta/channel-exchange
TA_GMAIL_POLL_INTERVAL=30000
```

---

## Part 5: Gmail-Specific Considerations

### 5.1 Latency

Gmail polling adds 30-60 seconds of latency compared to Slack/Discord
(which are near-instant). Adjust `TA_GMAIL_POLL_INTERVAL` to trade off
between responsiveness and API quota usage. Gmail API quota is 250 units/second
for reads, which is plenty for polling every 15-30 seconds.

### 5.2 Filtering

Create a Gmail filter to keep TA reviews organized:

1. In Gmail, click the search bar → **Show search options**
2. Subject: `[TA Review]`
3. Click **Create filter**
4. Apply label: `TA/Reviews`
5. Optionally: Star it, Never send to spam

### 5.3 Multiple Reviewers

To support multiple reviewers, set `TA_REVIEWER_EMAIL` to a comma-separated
list. The first person to reply with APPROVE/DENY wins:

```bash
TA_REVIEWER_EMAIL=alice@example.com,bob@example.com
```

Modify the `sendMail` call to use `to: REVIEWER_EMAIL` (nodemailer handles
comma-separated recipients).

### 5.4 Rich HTML Emails

For better formatting, send HTML emails instead of plain text. Add a
`html` field to the `sendMail` options with tables for file lists and
styled APPROVE/DENY instruction blocks.

---

## Troubleshooting

| Problem | Solution |
|---|---|
| OAuth error on first run | Ensure you added your email as a test user in the Google Cloud Console consent screen. |
| "Token has been expired or revoked" | Delete `token.json` and re-authorize. |
| Email not received | Check spam folder. Verify `TA_REVIEWER_EMAIL` is correct. |
| Reply not detected | Ensure your reply starts with `APPROVE` or `DENY` on the first line (above any quoted text). |
| "Less secure app access" error | Use App Password method (Part 4) instead of deprecated "less secure apps". |
| Gmail API quota exceeded | Increase `TA_GMAIL_POLL_INTERVAL` to 60000 (1 minute). |

---

## Native Channel Implementation (Future)

A native `EmailChannelFactory` would implement `ReviewChannel` using the
`lettre` (SMTP) and `imap` Rust crates. Configuration:

```yaml
channels:
  review:
    type: email
    smtp_host: smtp.gmail.com
    smtp_port: 587
    imap_host: imap.gmail.com
    imap_port: 993
    username_env: TA_EMAIL_USER
    password_env: TA_EMAIL_PASSWORD
    reviewer: reviewer@example.com
    poll_interval_seconds: 30
    subject_prefix: "[TA Review]"
```
