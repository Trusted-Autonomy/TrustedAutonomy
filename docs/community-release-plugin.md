# Building a Trusted Autonomy Release Adapter Plugin

This guide explains how to add a new publish target to `ta release` (v0.17.4) — Steam and
an App Store are the motivating examples, not special cases. A community author (or you,
adding a new platform) writes exactly the same kind of package the built-in `github`,
`remote-file`, and `youtube` adapters are: something that satisfies the `ReleaseAdapter`
contract. The difference is *where the code lives* — a plugin is an external executable,
a `plugin.toml` manifest, dropped into `.ta/plugins/release/<name>/`. No TA core change,
no recompile.

This is the **Plugin** category from `docs/USAGE.md` → "Authoring a Plugin" — call/response
over newline-delimited JSON on stdin/stdout, discovered by convention. If you haven't read
that section yet, start there for the shared manifest schema and wire envelope; this doc
covers only what's specific to `release`-kind plugins.

## Why plugins, not native adapters, for Steam/App Store

`YouTubeReleaseAdapter` ships native in `ta-release` because the YouTube Data API v3 is a
plain REST API — no proprietary SDK to vendor. Steam and the App Store are different: both
require a licensed platform SDK (Steamworks SDK; App Store Connect / `altool`) that TA's own
binary cannot bundle or redistribute. Pushing them out to a plugin process means TA's own
release cadence never has to track a third-party SDK's license or version churn — the same
reasoning `docs/release-design.md` §6 and §8 apply, and the same shape v0.17.5.3's
domain-action adapters use for "no live external API dependency in TA's own test suite."

## The `ReleaseAdapter` contract, over the wire

Your plugin's `plugin.toml` has `type = "release"`. Every call is one `{"method","params"}`
JSON line in, one `{"ok":true,"result":{...}}` or `{"ok":false,"error":"..."}` JSON line out
— fresh process per call (see `docs/USAGE.md`'s wire protocol section for the exact framing).

| Method | Required? | Maps to |
|---|---|---|
| `handshake` | Yes — first call, once, at adapter resolution | Report `plugin_version`, `protocol_version`, `adapter_name`, `capabilities` |
| `prepare` | Yes | `ReleaseAdapter::prepare` |
| `publish` | Yes | `ReleaseAdapter::publish` |
| `promote` | Only if declared in `capabilities` | `ReleaseAdapter::promote` |
| `status` | Only if declared in `capabilities` (also gates `list`) | `ReleaseAdapter::status` / `ReleaseAdapter::list` |

A minimal plugin implementing only `handshake`/`prepare`/`publish` is legal — `ta release
promote`/`status`/`list` simply report the adapter as unsupported/unknown for those
operations, the same graceful-degradation the built-in adapters' default trait methods give
you (`docs/release-design.md` §3).

### `handshake`

```
→ {"method":"handshake","params":{"ta_version":"0.17.4-alpha","protocol_version":1}}
← {"ok":true,"result":{
     "plugin_version":"1.0.0",
     "protocol_version":1,
     "adapter_name":"steam",
     "capabilities":["promote","status","channel:beta","channel:default"]
   }}
```

`capabilities` does double duty:
- `"promote"` — declares `promote` is implemented; omit it and `ta release promote` returns
  `ReleaseError::Unsupported` without ever calling your plugin.
- `"status"` — declares `status`/`list` are implemented; omit it and both return
  `Unknown`/empty locally, no call made.
- `"channel:<name>"` — declares an adapter-native channel name beyond the four standard ones
  (`draft`/`rc`/`stable`/`lts`), surfaced as `ReleaseCapabilities::custom_channel_names`. Steam
  declares `"channel:beta"` and `"channel:default"` for its beta/default depot branches.
- `"requires_semver"` — declares your adapter needs a semver `version_or_label` (most content
  and game-build adapters omit this and accept arbitrary labels).

### `prepare`

```
→ {"method":"prepare","params":{
     "version_or_label":"build-42",
     "channel":"beta",
     "commits":"fixed physics\nfixed lighting",
     "workspace_root":"/path/to/project"
   }}
← {"ok":true,"result":{"idempotency_key":"build-42","resolved_label":"build-42"}}
```

Do your preflight here — verify a `steamcmd` session, resolve a depot/branch mapping —
and fail loudly (`{"ok":false,"error":"..."}`) before anything touches the target platform.
`idempotency_key` is round-tripped back to you unmodified at `publish`; put whatever your
platform needs to detect "this was already published" in it.

### `publish`

```
→ {"method":"publish","params":{
     "idempotency_key":"build-42",
     "resolved_label":"build-42",
     "assets":[{"path":"/path/to/build.zip","label":null}]
   }}
← {"ok":true,"result":{"external_id":"depot-9001","url":"https://store.steampowered.com/app/123"}}
```

`external_id` and `url` become `ReleaseRef.external_id`/`.url` — `external_id` is what a
later `promote`/`status` call receives back, so make it whatever handle your platform needs
to identify this specific published artifact (a Steam depot build ID, an App Store Connect
build number).

### `promote` (optional)

```
→ {"method":"promote","params":{
     "external_id":"depot-9001",
     "url":"https://store.steampowered.com/app/123",
     "channel":"default"
   }}
← {"ok":true,"result":{}}
```

Move an already-published build to a different channel without rebuilding — for Steam,
`steamcmd` branch promotion (copy from `beta` to `default`), no new depot upload.

### `status` / `list` (optional)

```
→ {"method":"status","params":{"version":"build-42"}}
← {"ok":true,"result":{
     "known":true,
     "channels":["beta"],
     "published_at":"2026-08-01T00:00:00Z",
     "asset_checksums":[]
   }}

→ {"method":"list","params":{"limit":10}}
← {"ok":true,"result":{"releases":[ /* same shape as status's result, one per release */ ]}}
```

`"known":false` (or omitting `status` from `capabilities` entirely) means "adapter has no
live query path" — the caller falls back to `.ta/release-history.json`.

## Registering your plugin

Drop `plugin.toml` + your executable into `.ta/plugins/release/<name>/` (project-local) or
`~/.config/ta/plugins/release/<name>/` (user-global):

```toml
# .ta/plugins/release/steam/plugin.toml
name = "steam"
type = "release"
command = "ta-release-steam"
capabilities = ["promote", "status", "channel:beta", "channel:default"]
description = "Steamworks SDK depot push"
timeout_secs = 120
```

Resolution follows the same scheme convention as the built-in adapters (`docs/release-design.md`
§6): a `publish_url` scheme with no built-in match is looked up as a plugin *name* directly —
`publish_url = "steam://app/12345"` in `.release.toml`'s `[release]` table resolves to a
plugin named `steam`, no separate scheme-mapping file needed. `--adapter steam` does the same
lookup by name. Run `ta release adapters` to see both the built-in adapters and every plugin
TA discovered.

## Testing without a real platform SDK

You don't need a live Steamworks/App Store Connect session to validate the protocol contract.
A shell script that reads one line and echoes a canned response round-trips through
`PluginReleaseAdapter` exactly like a real binary — see the `steam_mock_script` test fixture
in `crates/ta-release/src/adapters/plugin.rs` for the pattern. Use it to verify your
`plugin.toml` and wire shapes before writing real SDK integration code.
