# Setting Up a Discord Review Channel for TA

This guide covers how to connect Trusted Autonomy to a Discord server so that
draft reviews, approvals, and notifications appear as Discord messages instead
of CLI prompts.

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
  ta-discord-bridge (Node.js service)
        │  reads request → posts embed to Discord channel
        │  waits for button click (Approve / Deny)
        │  writes .ta/channel-exchange/response-{id}.json
        ▼
  WebhookChannel picks up response
        │
        ▼
  Agent continues or revises
```

The bridge service translates between TA's file-based webhook protocol and
the Discord API. No Rust code changes are needed.

---

## Part 1: Discord Setup

### 1.1 Create a Discord Application

1. Go to https://discord.com/developers/applications
2. Click **New Application**, name it `TA Review Bot` (or similar)
3. Go to the **Bot** tab
4. Click **Reset Token** and copy the bot token — you'll need this later
5. Under **Privileged Gateway Intents**, enable:
   - **Message Content Intent** (needed to read approval commands)
6. Click **Save Changes**

### 1.2 Set Bot Permissions

Go to **OAuth2 → URL Generator**:

1. Under **Scopes**, select `bot` and `applications.commands`
2. Under **Bot Permissions**, select:
   - Send Messages
   - Embed Links
   - Use Slash Commands
   - Read Message History
   - Add Reactions
3. Copy the generated URL and open it in your browser
4. Select your server and authorize the bot

### 1.3 Create a Review Channel

In your Discord server:

1. Create a channel called `#ta-reviews` (or whatever you prefer)
2. Restrict the channel so only you and the bot can post
3. Copy the **channel ID**: right-click the channel → Copy Channel ID
   (enable Developer Mode in Discord Settings → Advanced if you don't see this)

### 1.4 Note Your IDs

You need three values:
- **Bot Token**: from step 1.1
- **Channel ID**: from step 1.3
- **Guild ID** (server ID): right-click your server name → Copy Server ID

---

## Part 2: Bridge Service Setup

### 2.1 Install the Bridge

```bash
# From your TA project root
mkdir -p .ta/bridges/discord
cd .ta/bridges/discord

# Initialize and install dependencies
npm init -y
npm install discord.js chokidar
```

### 2.2 Create the Bridge Script

Copy the template from `templates/channels/discord-bridge.js` in the TA repo,
or create `.ta/bridges/discord/bridge.js`:

```javascript
// See templates/channels/discord-bridge.js for the full implementation
const { Client, GatewayIntentBits, EmbedBuilder, ActionRowBuilder,
        ButtonBuilder, ButtonStyle } = require('discord.js');
const chokidar = require('chokidar');
const fs = require('fs');
const path = require('path');

// --- Configuration ---
const DISCORD_TOKEN   = process.env.TA_DISCORD_TOKEN;
const CHANNEL_ID      = process.env.TA_DISCORD_CHANNEL_ID;
const EXCHANGE_DIR    = process.env.TA_EXCHANGE_DIR
                        || path.join(process.cwd(), '.ta', 'channel-exchange');

if (!DISCORD_TOKEN || !CHANNEL_ID) {
  console.error('Set TA_DISCORD_TOKEN and TA_DISCORD_CHANNEL_ID');
  process.exit(1);
}

// Ensure exchange directory exists
fs.mkdirSync(EXCHANGE_DIR, { recursive: true });

const client = new Client({
  intents: [
    GatewayIntentBits.Guilds,
    GatewayIntentBits.GuildMessages,
    GatewayIntentBits.MessageContent,
  ],
});

// Track pending reviews: requestId -> { messageId, timeout }
const pending = new Map();

client.once('ready', () => {
  console.log(`[ta-discord] Logged in as ${client.user.tag}`);
  console.log(`[ta-discord] Watching ${EXCHANGE_DIR} for review requests`);
  startWatcher();
});

function startWatcher() {
  const watcher = chokidar.watch(path.join(EXCHANGE_DIR, 'request-*.json'), {
    persistent: true,
    ignoreInitial: false,       // pick up requests written before bridge started
    awaitWriteFinish: { stabilityThreshold: 500 },
  });

  watcher.on('add', async (filePath) => {
    try {
      const raw = fs.readFileSync(filePath, 'utf-8');
      const request = JSON.parse(raw);
      await postReviewRequest(request, filePath);
    } catch (err) {
      console.error(`[ta-discord] Error processing ${filePath}:`, err);
    }
  });
}

async function postReviewRequest(request, filePath) {
  const channel = await client.channels.fetch(CHANNEL_ID);
  const requestId = request.id || path.basename(filePath, '.json').replace('request-', '');

  // Don't double-post if response already exists
  const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  if (fs.existsSync(responsePath)) return;

  // Build the embed
  const embed = new EmbedBuilder()
    .setTitle(`📋 Draft Review: ${request.title || 'Untitled'}`)
    .setColor(0x5865F2)
    .setTimestamp();

  if (request.summary) {
    embed.setDescription(request.summary);
  }

  // Add artifact list if present
  if (request.artifacts && request.artifacts.length > 0) {
    const fileList = request.artifacts
      .slice(0, 20)
      .map(a => `\`${a.path || a.resource_uri || a}\``)
      .join('\n');
    embed.addFields({
      name: `Files Changed (${request.artifacts.length})`,
      value: fileList.substring(0, 1024),
    });
  }

  if (request.artifact_count) {
    embed.addFields({ name: 'Artifacts', value: `${request.artifact_count}`, inline: true });
  }

  // Action buttons
  const row = new ActionRowBuilder().addComponents(
    new ButtonBuilder()
      .setCustomId(`ta_approve_${requestId}`)
      .setLabel('Approve')
      .setStyle(ButtonStyle.Success)
      .setEmoji('✅'),
    new ButtonBuilder()
      .setCustomId(`ta_deny_${requestId}`)
      .setLabel('Deny')
      .setStyle(ButtonStyle.Danger)
      .setEmoji('❌'),
    new ButtonBuilder()
      .setCustomId(`ta_view_${requestId}`)
      .setLabel('View Details')
      .setStyle(ButtonStyle.Secondary)
      .setEmoji('🔍'),
  );

  const msg = await channel.send({ embeds: [embed], components: [row] });
  pending.set(requestId, { messageId: msg.id, filePath });
  console.log(`[ta-discord] Posted review request ${requestId}`);
}

// Handle button interactions
client.on('interactionCreate', async (interaction) => {
  if (!interaction.isButton()) return;

  const [action, ...idParts] = interaction.customId.split('_').slice(1);
  const requestId = idParts.join('_');
  const entry = pending.get(requestId);

  if (!entry) {
    await interaction.reply({ content: 'This review is no longer active.', ephemeral: true });
    return;
  }

  if (action === 'view') {
    // Show the full request JSON
    try {
      const raw = fs.readFileSync(entry.filePath, 'utf-8');
      const truncated = raw.substring(0, 1900);
      await interaction.reply({
        content: `\`\`\`json\n${truncated}\n\`\`\``,
        ephemeral: true,
      });
    } catch {
      await interaction.reply({ content: 'Could not read request file.', ephemeral: true });
    }
    return;
  }

  if (action === 'approve') {
    writeResponse(requestId, {
      id: requestId,
      decision: 'approved',
      approved_by: interaction.user.tag,
      approved_at: new Date().toISOString(),
      selection: 'all',
    });
    await interaction.update({
      content: `✅ **Approved** by ${interaction.user.tag}`,
      embeds: [],
      components: [],
    });
    pending.delete(requestId);
  }

  if (action === 'deny') {
    // Ask for a reason via modal
    const { ModalBuilder, TextInputBuilder, TextInputStyle,
            ActionRowBuilder: ModalRow } = require('discord.js');
    const modal = new ModalBuilder()
      .setCustomId(`ta_deny_modal_${requestId}`)
      .setTitle('Deny Draft');
    const reasonInput = new TextInputBuilder()
      .setCustomId('reason')
      .setLabel('Reason for denial')
      .setStyle(TextInputStyle.Paragraph)
      .setRequired(true);
    modal.addComponents(new ModalRow().addComponents(reasonInput));
    await interaction.showModal(modal);
  }
});

// Handle deny modal submission
client.on('interactionCreate', async (interaction) => {
  if (!interaction.isModalSubmit()) return;
  if (!interaction.customId.startsWith('ta_deny_modal_')) return;

  const requestId = interaction.customId.replace('ta_deny_modal_', '');
  const reason = interaction.fields.getTextInputValue('reason');

  writeResponse(requestId, {
    id: requestId,
    decision: 'denied',
    denied_by: interaction.user.tag,
    reason: reason,
  });

  await interaction.reply({
    content: `❌ **Denied** by ${interaction.user.tag}: ${reason}`,
  });
  pending.delete(requestId);
});

function writeResponse(requestId, response) {
  const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  fs.writeFileSync(responsePath, JSON.stringify(response, null, 2));
  console.log(`[ta-discord] Wrote response for ${requestId}: ${response.decision}`);
}

client.login(DISCORD_TOKEN);
```

### 2.3 Create Environment File

Create `.ta/bridges/discord/.env`:

```bash
TA_DISCORD_TOKEN=your-bot-token-here
TA_DISCORD_CHANNEL_ID=your-channel-id-here
TA_EXCHANGE_DIR=/path/to/your/project/.ta/channel-exchange
```

> **Security**: Add `.ta/bridges/discord/.env` to `.gitignore`. Never commit bot tokens.

---

## Part 3: TA Configuration

### 3.1 Configure the Webhook Channel

Edit (or create) `.ta/config.yaml`:

```yaml
channels:
  review:
    type: webhook
    endpoint: .ta/channel-exchange   # relative to project root
    timeout_seconds: 3600            # 1 hour default
    poll_interval_ms: 2000
  notify:
    - type: webhook
      endpoint: .ta/channel-exchange
  session:
    type: terminal                   # keep terminal for interactive sessions
  default_agent: claude-code
```

The `WebhookChannel` writes `request-{id}.json` files to the endpoint directory.
The bridge watches that directory and translates to Discord.

### 3.2 Start the Bridge

```bash
# Terminal 1: Start the Discord bridge
cd .ta/bridges/discord
source .env  # or: export $(cat .env | xargs)
node bridge.js

# Terminal 2: Start TA daemon (optional, for web UI alongside Discord)
ta daemon start

# Terminal 3: Start your agent session
ta run "implement feature X"
```

### 3.3 Verify It Works

1. Run a quick test goal:
   ```bash
   ta goal start "test discord"
   ta draft build --latest
   ta draft submit --latest
   ```
2. You should see the review embed appear in your `#ta-reviews` channel
3. Click **Approve** or **Deny** — the response flows back to TA

---

## Part 4: Production Hardening

### 4.1 Run Bridge as a System Service

Create a systemd unit (Linux) or launchd plist (macOS):

**Linux** — `/etc/systemd/system/ta-discord-bridge.service`:
```ini
[Unit]
Description=TA Discord Review Bridge
After=network.target

[Service]
Type=simple
WorkingDirectory=/path/to/project/.ta/bridges/discord
EnvironmentFile=/path/to/project/.ta/bridges/discord/.env
ExecStart=/usr/bin/node bridge.js
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

**macOS** — `~/Library/LaunchAgents/com.ta.discord-bridge.plist`:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.ta.discord-bridge</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/node</string>
    <string>/path/to/project/.ta/bridges/discord/bridge.js</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>TA_DISCORD_TOKEN</key>
    <string>your-bot-token</string>
    <key>TA_DISCORD_CHANNEL_ID</key>
    <string>your-channel-id</string>
    <key>TA_EXCHANGE_DIR</key>
    <string>/path/to/project/.ta/channel-exchange</string>
  </dict>
  <key>KeepAlive</key>
  <true/>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
```

### 4.2 Multiple Projects

Each project needs its own `TA_EXCHANGE_DIR`. You can run one bridge per
project, or a single bridge that watches multiple directories (modify the
watcher to accept a comma-separated list of paths).

### 4.3 Access Control

Discord's permission system handles who can click Approve/Deny:

- Restrict `#ta-reviews` to specific roles
- The bridge records `interaction.user.tag` in the response, so the audit
  trail shows who approved
- For stricter control, add a check in the button handler:
  ```javascript
  const ALLOWED_REVIEWERS = ['username#1234'];
  if (!ALLOWED_REVIEWERS.includes(interaction.user.tag)) {
    await interaction.reply({ content: 'Not authorized.', ephemeral: true });
    return;
  }
  ```

---

## Troubleshooting

| Problem | Solution |
|---|---|
| Bot doesn't post | Check `TA_DISCORD_TOKEN` and `TA_DISCORD_CHANNEL_ID`. Verify bot has Send Messages permission in the channel. |
| Request not picked up | Verify `TA_EXCHANGE_DIR` matches the `endpoint` in `.ta/config.yaml`. Check that request files appear in the directory. |
| Response not picked up by TA | Verify the response filename matches `response-{id}.json` where `{id}` matches the request ID. |
| Button clicks do nothing | Check bridge console for errors. Ensure bot has the `applications.commands` scope. |
| "This interaction failed" | The bridge must respond within 3 seconds. If reading large files, defer the response. |

---

## Native Channel Implementation (Future)

A native `DiscordChannelFactory` implementing TA's `ChannelFactory` trait would
eliminate the bridge service. This is planned for a future phase. The native
implementation would:

1. Create `crates/ta-channel-discord/` with `DiscordChannelFactory`
2. Implement `ReviewChannel` using `serenity` (Rust Discord library)
3. Register in `default_registry()` or via plugin loading
4. Configure directly in `.ta/config.yaml`:
   ```yaml
   channels:
     review:
       type: discord
       token_env: TA_DISCORD_TOKEN
       channel_id: "123456789"
       allowed_roles: ["reviewer"]
   ```
