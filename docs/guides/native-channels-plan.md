# Native Channel Implementation Plan

This document outlines the phased implementation plan for native TA channel
plugins, replacing the bridge services with direct Rust implementations.

The bridge approach (WebhookChannel + Node.js bridge) works today and requires
no TA code changes. The native approach eliminates the bridge service, reduces
latency, and simplifies deployment.

---

## Current State

| Component | Status |
|---|---|
| `ReviewChannel` trait | Done |
| `SessionChannel` trait | Done |
| `ChannelFactory` trait | Done |
| `ChannelRegistry` with `default_registry()` | Done (terminal, auto-approve, webhook) |
| `.ta/config.yaml` loading | Done |
| GatewayState wiring to registry | Partial (defaults to AutoApproveChannel) |
| Bridge templates (Discord, Slack, Gmail) | Done (docs/guides + templates/channels) |

## Prerequisites

Before native channels, the gateway needs to wire `ChannelRegistry` properly:

1. `GatewayState::new()` should load `.ta/config.yaml` and resolve the
   configured review channel from the registry instead of defaulting to
   `AutoApproveChannel`
2. The `ta run` command should pass the channel config to the gateway when
   starting the MCP server

This is partially scoped in v0.9.4 (gateway refactor).

---

## Phase 1: Gateway Channel Wiring (v0.9.5 or v0.10.0)

**Goal**: Wire `ChannelRegistry` into the MCP gateway so `.ta/config.yaml`
actually controls which channel handles reviews.

1. Load `TaConfig` from `.ta/config.yaml` in `GatewayState::new()`
2. Build the `ChannelRegistry` with `default_registry()`
3. Resolve `config.channels.review.type` → `ChannelFactory` → `ReviewChannel`
4. Store the resolved channel in `GatewayState.review_channel`
5. Same for `notify` (multiple) and `escalation`
6. Fallback: if config missing or type unknown, use `TerminalChannel`
7. Tests: verify gateway uses webhook channel when config says webhook

**Files**:
- `crates/ta-mcp-gateway/src/server.rs` (or `tools/mod.rs` post-refactor)
- `crates/ta-changeset/src/channel_registry.rs` (minor: ensure `load_config` is public)

**Estimated scope**: ~100 lines of wiring code + tests

---

## Phase 2: Native Discord Channel (ta-channel-discord)

**Goal**: `DiscordChannelFactory` implementing `ChannelFactory` with direct
Discord gateway connection.

**Crate**: `crates/ta-channel-discord/`

**Dependencies**: `serenity` (Rust Discord library) or `twilight` (lower-level)

**Implementation**:
1. `DiscordReviewChannel` implements `ReviewChannel`:
   - `request_interaction()` → posts embed with buttons → awaits interaction → returns decision
   - `notify()` → posts notification embed
   - `capabilities()` → review, notify, rich_content, buttons
2. `DiscordChannelFactory` implements `ChannelFactory`:
   - `channel_type()` → `"discord"`
   - `build_review(config)` → reads `token_env`, `channel_id`, `allowed_roles` from config
   - `build_session(config)` → returns error (Discord isn't suitable for interactive sessions)
3. Register in `default_registry()` or via plugin loading

**Config**:
```yaml
channels:
  review:
    type: discord
    token_env: TA_DISCORD_TOKEN
    channel_id: "123456789"
    allowed_roles: ["reviewer"]
    allowed_users: ["user#1234"]
```

**Challenge**: `serenity` is async and wants to own the event loop.
`request_interaction()` is sync (blocking). Options:
- Run Discord client on a background tokio runtime, use a oneshot channel
  to bridge sync/async
- Use `twilight` which gives more control over the event loop

**Estimated scope**: ~400 lines + Cargo.toml + tests

---

## Phase 3: Native Slack Channel (ta-channel-slack)

**Goal**: `SlackChannelFactory` implementing `ChannelFactory` with Slack
Block Kit and Socket Mode.

**Crate**: `crates/ta-channel-slack/`

**Dependencies**: `slack-morphism` (Rust Slack client) or raw HTTP with `reqwest`

**Implementation**:
1. `SlackReviewChannel` implements `ReviewChannel`:
   - `request_interaction()` → posts Block Kit message → waits for action payload → returns decision
   - Socket Mode: connects outbound, no public URL needed
   - HTTP Mode: runs a small Axum server for the interactivity endpoint
2. `SlackChannelFactory` implements `ChannelFactory`:
   - `channel_type()` → `"slack"`
   - `build_review(config)` → reads token, channel_id, socket_mode flag

**Config**:
```yaml
channels:
  review:
    type: slack
    bot_token_env: TA_SLACK_BOT_TOKEN
    channel_id: "C0123456789"
    socket_mode: true
    app_token_env: TA_SLACK_APP_TOKEN
    allowed_users: ["U01234567"]
```

**Challenge**: Same sync/async bridge as Discord. Slack's action payloads
arrive via HTTP POST, so we need either a background HTTP server or Socket
Mode connection.

**Estimated scope**: ~500 lines (Block Kit formatting is verbose) + tests

---

## Phase 4: Native Email Channel (ta-channel-email)

**Goal**: `EmailChannelFactory` implementing `ChannelFactory` with SMTP
send + IMAP poll.

**Crate**: `crates/ta-channel-email/`

**Dependencies**: `lettre` (SMTP), `imap` or `async-imap` (IMAP)

**Implementation**:
1. `EmailReviewChannel` implements `ReviewChannel`:
   - `request_interaction()` → sends email via SMTP → polls IMAP for reply → parses APPROVE/DENY
   - Timeout: configurable, default 2 hours
   - Subject tagging: `[TA Review] {title}` with `X-TA-Request-ID` header
2. `EmailChannelFactory` implements `ChannelFactory`:
   - `channel_type()` → `"email"`
   - Supports Gmail, Outlook, any SMTP/IMAP provider

**Config**:
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

**Challenge**: Email is inherently high-latency. The blocking
`request_interaction()` call will hold a thread for potentially hours.
Consider:
- Background polling thread with condvar notification
- Configurable timeout with clear error message
- Support for multiple reviewers (first to reply wins)

**Estimated scope**: ~350 lines + tests

---

## Phase 5: Channel Plugin Loading

**Goal**: Allow third-party channel plugins without modifying TA source.

**Approach**: Dynamic library loading (`.so`/`.dylib`/`.dll`)

1. Define a C ABI entry point: `extern "C" fn ta_channel_factory() -> Box<dyn ChannelFactory>`
2. `ChannelRegistry` scans `~/.config/ta/plugins/` and `.ta/plugins/` for shared libraries
3. Each library registers one or more channel types
4. Config references plugin types: `type: my-custom-channel`

**Alternative**: Process-based plugins (like the bridge, but standardized)
- Plugin is an executable that speaks a simple JSON-over-stdio protocol
- TA spawns the process, sends requests via stdin, reads responses from stdout
- Simpler than dynamic loading, works with any language

**Estimated scope**: ~200 lines for the loading mechanism + plugin protocol spec

---

## Summary: Recommended Phase Order

| Phase | What | Depends On | Effort |
|---|---|---|---|
| 1 | Gateway channel wiring | v0.9.4 gateway refactor | Small |
| 2 | Native Discord | Phase 1 | Medium |
| 3 | Native Slack | Phase 1 | Medium |
| 4 | Native Email | Phase 1 | Medium |
| 5 | Plugin loading | Phase 1 | Medium |

Phases 2-4 are independent of each other and can be done in any order.
Phase 5 enables the community to add channels without PRs.

All phases are optional — the bridge approach works today for all three
platforms with zero Rust changes needed.
