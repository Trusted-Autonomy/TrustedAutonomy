#!/usr/bin/env node
/**
 * TA Discord Review Bridge
 *
 * Translates between TA's file-based WebhookChannel protocol and the
 * Discord API. Watches for request-{id}.json files, posts embeds with
 * Approve/Deny buttons, and writes response-{id}.json when a reviewer
 * clicks a button.
 *
 * Environment variables:
 *   TA_DISCORD_TOKEN       — Discord bot token (required)
 *   TA_DISCORD_CHANNEL_ID  — Channel ID for review messages (required)
 *   TA_EXCHANGE_DIR        — Path to .ta/channel-exchange (default: cwd/.ta/channel-exchange)
 *   TA_ALLOWED_REVIEWERS   — Comma-separated Discord user tags (optional, empty = anyone)
 *
 * Usage:
 *   npm install discord.js chokidar
 *   export TA_DISCORD_TOKEN=... TA_DISCORD_CHANNEL_ID=...
 *   node discord-bridge.js
 *
 * See docs/guides/discord-channel.md for full setup instructions.
 */

const {
  Client, GatewayIntentBits, EmbedBuilder, ActionRowBuilder,
  ButtonBuilder, ButtonStyle, ModalBuilder, TextInputBuilder,
  TextInputStyle, ActionRowBuilder: ModalRow,
} = require('discord.js');
const chokidar = require('chokidar');
const fs = require('fs');
const path = require('path');

// ── Configuration ───────────────────────────────────────────────
const DISCORD_TOKEN = process.env.TA_DISCORD_TOKEN;
const CHANNEL_ID    = process.env.TA_DISCORD_CHANNEL_ID;
const EXCHANGE_DIR  = process.env.TA_EXCHANGE_DIR
                      || path.join(process.cwd(), '.ta', 'channel-exchange');
const ALLOWED_REVIEWERS = (process.env.TA_ALLOWED_REVIEWERS || '')
  .split(',')
  .map(s => s.trim())
  .filter(Boolean);

if (!DISCORD_TOKEN || !CHANNEL_ID) {
  console.error('Required: TA_DISCORD_TOKEN, TA_DISCORD_CHANNEL_ID');
  process.exit(1);
}

fs.mkdirSync(EXCHANGE_DIR, { recursive: true });

// ── Discord Client ──────────────────────────────────────────────
const client = new Client({
  intents: [
    GatewayIntentBits.Guilds,
    GatewayIntentBits.GuildMessages,
    GatewayIntentBits.MessageContent,
  ],
});

/** requestId -> { messageId, filePath } */
const pending = new Map();

client.once('ready', () => {
  console.log(`[ta-discord] Logged in as ${client.user.tag}`);
  console.log(`[ta-discord] Watching ${EXCHANGE_DIR}`);
  startWatcher();
});

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
      console.error(`[ta-discord] Error: ${err.message}`);
    }
  });
}

// ── Post Review Embed ───────────────────────────────────────────
async function postReview(request, filePath) {
  const channel = await client.channels.fetch(CHANNEL_ID);
  const requestId = request.id
    || path.basename(filePath, '.json').replace('request-', '');

  const responsePath = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  if (fs.existsSync(responsePath)) return;

  const embed = new EmbedBuilder()
    .setTitle(`📋 Draft Review: ${request.title || 'Untitled'}`)
    .setColor(0x5865F2)
    .setTimestamp();

  if (request.summary) embed.setDescription(request.summary);
  if (request.artifacts && request.artifacts.length > 0) {
    const list = request.artifacts
      .slice(0, 20)
      .map(a => `\`${a.path || a.resource_uri || a}\``)
      .join('\n');
    embed.addFields({
      name: `Files Changed (${request.artifacts.length})`,
      value: list.substring(0, 1024),
    });
  }
  if (request.artifact_count) {
    embed.addFields({ name: 'Artifacts', value: `${request.artifact_count}`, inline: true });
  }

  const row = new ActionRowBuilder().addComponents(
    new ButtonBuilder()
      .setCustomId(`ta_approve_${requestId}`)
      .setLabel('Approve')
      .setStyle(ButtonStyle.Success),
    new ButtonBuilder()
      .setCustomId(`ta_deny_${requestId}`)
      .setLabel('Deny')
      .setStyle(ButtonStyle.Danger),
    new ButtonBuilder()
      .setCustomId(`ta_view_${requestId}`)
      .setLabel('View Details')
      .setStyle(ButtonStyle.Secondary),
  );

  const msg = await channel.send({ embeds: [embed], components: [row] });
  pending.set(requestId, { messageId: msg.id, filePath });
  console.log(`[ta-discord] Posted review ${requestId}`);
}

// ── Access Check ────────────────────────────────────────────────
function isAuthorized(user) {
  if (ALLOWED_REVIEWERS.length === 0) return true;
  return ALLOWED_REVIEWERS.includes(user.tag) || ALLOWED_REVIEWERS.includes(user.id);
}

// ── Button Interactions ─────────────────────────────────────────
client.on('interactionCreate', async (interaction) => {
  if (!interaction.isButton()) return;

  const parts = interaction.customId.split('_');
  const action = parts[1];
  const requestId = parts.slice(2).join('_');
  const entry = pending.get(requestId);

  if (!entry) {
    await interaction.reply({ content: 'Review no longer active.', ephemeral: true });
    return;
  }

  if (!isAuthorized(interaction.user)) {
    await interaction.reply({ content: 'Not authorized to review.', ephemeral: true });
    return;
  }

  if (action === 'view') {
    try {
      const raw = fs.readFileSync(entry.filePath, 'utf-8');
      await interaction.reply({
        content: `\`\`\`json\n${raw.substring(0, 1900)}\n\`\`\``,
        ephemeral: true,
      });
    } catch {
      await interaction.reply({ content: 'Could not read request.', ephemeral: true });
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
      embeds: [], components: [],
    });
    pending.delete(requestId);
  }

  if (action === 'deny') {
    const modal = new ModalBuilder()
      .setCustomId(`ta_deny_modal_${requestId}`)
      .setTitle('Deny Draft');
    const input = new TextInputBuilder()
      .setCustomId('reason')
      .setLabel('Reason for denial')
      .setStyle(TextInputStyle.Paragraph)
      .setRequired(true);
    modal.addComponents(new ActionRowBuilder().addComponents(input));
    await interaction.showModal(modal);
  }
});

// ── Deny Modal ──────────────────────────────────────────────────
client.on('interactionCreate', async (interaction) => {
  if (!interaction.isModalSubmit()) return;
  if (!interaction.customId.startsWith('ta_deny_modal_')) return;

  const requestId = interaction.customId.replace('ta_deny_modal_', '');
  const reason = interaction.fields.getTextInputValue('reason');

  writeResponse(requestId, {
    id: requestId,
    decision: 'denied',
    denied_by: interaction.user.tag,
    reason,
  });
  await interaction.reply({ content: `❌ **Denied** by ${interaction.user.tag}: ${reason}` });
  pending.delete(requestId);
});

// ── Helpers ─────────────────────────────────────────────────────
function writeResponse(requestId, response) {
  const p = path.join(EXCHANGE_DIR, `response-${requestId}.json`);
  fs.writeFileSync(p, JSON.stringify(response, null, 2));
  console.log(`[ta-discord] ${response.decision} ${requestId}`);
}

// ── Start ───────────────────────────────────────────────────────
client.login(DISCORD_TOKEN);
