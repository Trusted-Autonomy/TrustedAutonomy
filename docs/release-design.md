# `ta release` Design Review — v0.17.2

**Status**: Signed off. This document is what v0.17.3 (core + `GitHubReleaseAdapter` +
`RemoteFileReleaseAdapter`) and v0.17.4 (`YouTubeReleaseAdapter`, Steam, Homebrew, plugin
protocol) implement against. Design only — no code in this phase.

**Scope**: finalize the `ta release` command surface, the `ReleaseAdapter` trait, the channel
model, versioning rules for code vs. non-code artifacts, and the migration path off today's
pipeline-YAML system.

## 1. Why this exists

Today's `ta release` (`apps/ta-cli/src/commands/release.rs`, ~5,100 lines) is a single-purpose
tool: it executes a YAML-defined step pipeline (`run` shell steps, `agent` steps, a handful of
built-in step kinds like `generate_notes`/`constitution_check`/`update_release_toml`) that always
ends the same way — a git tag, a GitHub Actions dispatch (`ta release dispatch`), and a `gh
release create`. Every concept in it — "version", "publish", "channel", "promote" — is implicitly
GitHub-and-git shaped:

- **Version** is always a semver string or a plan-phase ID normalized to semver
  (`normalize_version`/`VersionPolicy`). There is no path for "episode-3" or "turntable-v2-final".
- **Publish** happens by pushing a git tag and letting `.github/workflows/release.yml` build and
  upload assets. There's no abstraction a content pipeline or a game studio's Steam depot push
  could implement instead.
- **Channel** is not a first-class concept. It's approximated by two booleans plus two strings in
  `.release.toml` — `prerelease`, `stable_release_tag`, `last_release_tag`, `nightly_tag` — and by
  which of three ad-hoc code paths ran (`ta release run` for a normal tag, `ta release dispatch`
  for a label tag, `.github/workflows/nightly.yml` for the rolling nightly). "Promote an RC to
  stable" isn't a command; it's re-running the whole pipeline with a different label and hoping
  the diff is empty.

None of this is wrong for what it was built for — TA's own binary release. It becomes wrong the
moment a second release *kind* shows up: a content creator publishing a video, a game studio
pushing a Steam build, an enterprise SecureAutonomy deployment shipping to an S3 bucket instead of
GitHub. v0.17.2 exists to design the abstraction before v0.17.3 writes code against it, per the
Deferred Items Policy's spirit — get the trait shape right once instead of reworking three adapter
implementations later.

## 2. Command surface (final)

```
ta release run <phase-or-version> [--label <label>] [--channel <channel>] [--adapter <name>]
ta release promote <tag-or-ref> --to <channel>
ta release status [<tag-or-ref>]
ta release list [--channel <channel>] [--limit N]
ta release adapters
```

Answering the plan's open questions directly:

- **Yes** — `run`, `promote`, `status`, `list`, `adapters` are the five subcommands `ta release`
  exposes going forward.
- **Yes** — RC → stable promotion is one command: `ta release promote v0.14.16-rc.1 --to stable`.
  No rebuild, no re-tag, no re-run of the publish pipeline — this is exactly `ReleaseAdapter::promote`
  (§3), which for `GitHubReleaseAdapter` means flipping the `prerelease` flag and `--latest` on an
  existing release, not creating a new one.

### Retained from today, unchanged in shape

`ta release show`, `ta release init`, `ta release config`, and `ta release validate` stay as
**pipeline-management** commands — they operate on the YAML pipeline (what shell/agent steps run
before publish: version bump, changelog, constitution check, build). The `ReleaseAdapter`
abstraction only replaces the *publish* half of the pipeline (today's implicit "tag + push +
dispatch GitHub Actions" ending), not the pre-publish orchestration. A `PublishStep` (new
`PipelineStep` variant, see §5) becomes the pipeline's last step and is the only place the adapter
is invoked. `ta release run` keeps orchestrating the pipeline exactly as it does today; the new
`--channel`/`--adapter` flags only affect what happens at the `PublishStep`.

### New: `ta release status`

Absorbs the "is this version published, on which channels" job that today requires reading
`.release.toml` by hand or running `gh release view`. Per the plan's command-simplification
principle #1 ("all release state queryable via `ta release status` — no separate `ta plan status`
needed for version info"), this becomes the canonical answer for "what shipped." Calls
`ReleaseAdapter::status(version) -> ReleaseStatus` (§3).

### New: `ta release list`

Lists recent releases across channels — thin wrapper reading `.ta/release-history.json` (already
written today by `record_release_history`) enriched with live channel state from
`ReleaseAdapter::status` where available; falls back to the local history file alone when the
adapter doesn't implement live lookup (e.g. a `RemoteFileReleaseAdapter` target that doesn't
support listing).

### New: `ta release adapters`

Lists registered adapters (built-in + discovered plugins per §6) and which `publish_url` schemes
they claim. Diagnostic command — "why did `s3://...` resolve to X" is a real support question once
third-party adapters exist.

### Deprecated: `ta release dispatch`, `ta release validate-tag`

`dispatch` becomes an alias for `run --channel <inferred-from-prerelease-flag>` with a deprecation
warning; `validate-tag`'s dry-run-precondition-check role folds into `ta release validate` (which
already exists for the pipeline) plus a `--dry-run` flag on `run`/`promote`. See §7 for the full
migration mapping — nothing is removed in v0.17.3; removal is a later, separately-announced phase.

### Conversational UX

Per the plan's principle #2, `ta shell`'s advisor layer maps natural language onto this surface
using the same `ta-brain::route()`/intent-classification path already used elsewhere (see
v0.17.7.4's phase-range parsing for the general pattern) — "release this as an RC" → `ta release
run <version> --channel rc`; "promote the last RC to stable" → resolves the most recent `rc`-channel
entry from `ta release list` and calls `ta release promote <that-ref> --to stable`. This is advisor
plumbing, not a new core concept, so it is **not** a v0.17.3 item — it lands whenever the advisor
layer picks it up (natural fit alongside v0.17.7.4, not blocking core adapter work).

## 3. The `ReleaseAdapter` trait

Modeled directly on `ta-submit::SourceAdapter` (`crates/ta-submit/src/adapter.rs`) — object-safe
trait, `Send + Sync`, default-implemented methods for optional capabilities so a minimal adapter
(one method, `publish`) is legal and everything else degrades gracefully rather than panicking.

```rust
// crates/ta-release/src/adapter.rs (target for v0.17.3 item 1)

pub trait ReleaseAdapter: Send + Sync {
    /// Adapter display name (for CLI output, `ta release adapters`, error messages).
    fn name(&self) -> &str;

    /// Static capability flags — drives CLI validation before any adapter method runs.
    /// See `ReleaseCapabilities` below.
    fn capabilities(&self) -> ReleaseCapabilities;

    /// Stage a release: build/collect assets, resolve the final version/label,
    /// run adapter-specific preflight (e.g. GitHub: verify `gh` auth; S3: verify
    /// bucket write access). Does not publish anything externally yet — a failed
    /// `prepare` leaves no visible trace on the target platform.
    ///
    /// GitHub: validates repo/auth, resolves the tag name, does NOT create the
    ///         (draft) release yet — that's `publish`, to keep the draft-first
    ///         two-step (see v0.17.3 item 5's "avoid immutable release race").
    /// S3/SFTP (RemoteFileReleaseAdapter): validates target reachability + write perms.
    /// YouTube: validates OAuth token freshness, resolves channel ID.
    /// Steam: validates steamcmd session, resolves depot/branch mapping.
    fn prepare(&self, ctx: &ReleaseContext) -> Result<PreparedRelease>;

    /// Publish a prepared release with its assets. Idempotent where the underlying
    /// platform allows it (calling twice with the same `PreparedRelease.idempotency_key`
    /// should not create a duplicate release — GitHub: same tag; S3: same manifest checksum).
    ///
    /// GitHub: draft-first — create draft release, upload assets, then publish
    ///         (edit draft=false). Prevents the "assets uploading while release is
    ///         already public" race the current pipeline is exposed to.
    /// RemoteFile: copies assets to `publish_url` + writes `manifest.json`
    ///             (version, checksums, channel, timestamp) alongside them.
    /// YouTube: uploads the video artifact, sets title/description from release notes.
    /// Steam: `steamcmd` depot push to the branch mapped from `ctx.channel`.
    fn publish(&self, prepared: &PreparedRelease, assets: &[ReleaseAsset]) -> Result<ReleaseRef>;

    /// Move an already-published release to a different channel without rebuilding
    /// or re-uploading. The core operation `ta release promote` calls.
    ///
    /// GitHub: PATCH the release — flip `prerelease`, set/unset `--latest`.
    /// RemoteFile: rewrite `manifest.json`'s `channel` field at a stable "current"
    ///             path/symlink (adapter-defined; e.g. `s3://bucket/releases/stable/`).
    /// YouTube: change video visibility (unlisted → public).
    /// Steam: `steamcmd` branch promotion (copy build from `beta` to `default`) —
    ///        no new depot upload.
    /// Default: returns `Err` — not every adapter supports post-hoc promotion
    ///          (e.g. a one-shot webhook-based `ServiceReleaseAdapter` might not).
    fn promote(&self, release_ref: &ReleaseRef, channel: Channel) -> Result<()> {
        Err(ReleaseError::Unsupported {
            adapter: self.name().to_string(),
            operation: "promote",
        })
    }

    /// Query current publish state for a version. Powers `ta release status`/`list`.
    ///
    /// GitHub: `gh release view` — channels derived from prerelease flag + whether
    ///         it's the `--latest` release.
    /// RemoteFile: reads `manifest.json` from the target if present.
    /// Default: returns `ReleaseStatus::Unknown` — adapter has no live query path;
    ///          caller falls back to local `.ta/release-history.json`.
    fn status(&self, version: &str) -> Result<ReleaseStatus> {
        Ok(ReleaseStatus::Unknown)
    }

    /// List recent releases this adapter knows about, most recent first.
    /// Default: empty — caller falls back to local history file only.
    fn list(&self, limit: usize) -> Result<Vec<ReleaseStatus>> {
        Ok(Vec::new())
    }
}

/// Static, adapter-declared capability flags — checked by the CLI *before* invoking
/// any adapter method, so an unsupported operation fails fast with a clear message
/// instead of a runtime `Unsupported` error surfacing mid-pipeline.
pub struct ReleaseCapabilities {
    /// If true, `ta release run` rejects non-semver labels for this adapter
    /// (GitHub, RemoteFile-for-code: true. YouTube, RemoteFile-for-content, Steam: false).
    pub requires_semver: bool,
    /// If true, `promote` has a real implementation (not the default `Unsupported`).
    pub supports_promote: bool,
    /// If true, `status`/`list` do a live remote query rather than returning `Unknown`/empty.
    pub supports_live_status: bool,
    /// Channel names this adapter recognizes beyond the four standard ones (§4) —
    /// e.g. Steam's `beta`/`default` branch names, surfaced in `ta release adapters`
    /// and used for channel-name validation at `run`/`promote` time.
    pub custom_channel_names: Vec<String>,
}
```

Two deliberate deviations from the plan's initial method sketch (`publish(prepared, assets) →
ReleaseRef`, `promote`, `status`), recorded here rather than silently:

1. **Added `prepare`.** The plan's sketch takes `prepared` as `publish`'s input but never defines
   what produces it. `prepare` is that step — it exists so preflight validation (auth, bucket
   permissions, OAuth token freshness) happens and fails loudly *before* any external side effect,
   matching the existing pipeline's separation between validation steps and the actual publish
   step. Without it, a failed `publish` could leave a half-created GitHub draft or a partial S3
   upload with no clean "did this fail before or after touching the target platform" answer.
2. **`promote`/`status`/`list` are default-implemented, not required.** `SourceAdapter` uses this
   pattern extensively (`sync_upstream`, `check_review`, `merge_review` all default to
   no-op/`None`) precisely because not every backend supports every operation, and a content
   pipeline's minimal adapter (just `publish`) shouldn't be forced to implement four stub methods
   that always return "not supported."

### Supporting types (sketch — full definitions land with v0.17.3 item 1)

```rust
pub struct ReleaseContext {
    pub version_or_label: String,   // semver OR arbitrary label (§4)
    pub channel: Channel,
    pub commits: String,            // for release-notes generation
    pub workspace_root: PathBuf,
}

pub struct PreparedRelease {
    pub idempotency_key: String,    // adapter-chosen; e.g. GitHub tag, S3 manifest checksum
    pub resolved_label: String,
    // ... adapter-opaque staging metadata
}

pub struct ReleaseAsset {
    pub path: PathBuf,
    pub label: Option<String>,      // display name, e.g. "ta-linux-x86_64.tar.gz"
}

pub struct ReleaseRef {
    pub adapter: String,
    pub external_id: String,        // e.g. GitHub release ID, S3 manifest URL
    pub url: Option<String>,        // human-followable link, when the platform has one
}

pub enum ReleaseStatus {
    Unknown,
    Known {
        channels: Vec<Channel>,
        published_at: Option<String>,
        asset_checksums: Vec<(String, String)>,
    },
}
```

## 4. Channel model and lifecycle

**Decision**: one small `Channel` enum with four standard variants, plus an adapter-declared
escape hatch (`ReleaseCapabilities::custom_channel_names`) for platform-native names that don't
map cleanly (Steam's `beta`/`default` branches).

```rust
pub enum Channel {
    Draft,    // not externally visible / private
    Rc,       // pre-release, externally visible, not "latest"
    Stable,   // externally visible, "latest"
    Lts,      // stable + a long-term-support marker (adapter-specific meaning; GitHub: a
              // dedicated tag/release pinned as non-superseded by future --latest moves)
    Custom(String),  // adapter-native channel name (e.g. "beta" for Steam) — validated
                     // against that adapter's `custom_channel_names` at command time
}
```

Lifecycle is `Draft → Rc → Stable → Lts`, monotonic by default (no built-in "demote"; an adapter
that needs to walk a release back to `Rc` does so via a fresh `promote` call to `Rc` — the trait
doesn't forbid moving backward, it just isn't the expected flow and the CLI doesn't build a
shortcut for it).

### Mapping onto today's mechanisms (no behavior change, same primitives)

| Channel | `GitHubReleaseAdapter` | `RemoteFileReleaseAdapter` | Content (`custom_channel_names`) | Game (`custom_channel_names`) |
|---|---|---|---|---|
| Draft | `gh release create --draft` | not copied to `publish_url` yet | `Custom("draft")` — unlisted/private | `Custom("internal")` — internal test branch |
| Rc | `prerelease=true`, no `--latest` | copied, `manifest.json.channel="rc"` | `Custom("review")` — unlisted, review link shared | `Custom("beta")` — beta branch |
| Stable | `prerelease=false`, `--latest` | copied, `channel="stable"`, updates a stable pointer/symlink | `Custom("published")` — public | `Custom("default")` — default branch |
| Lts | `prerelease=false`, pinned (not superseded by later `--latest` moves) | separate `channel="lts"` retention path | n/a (not a meaningful content concept — adapters may ignore) | n/a |

`nightly` (today's rolling `nightly.yml` tag) is **not** promoted to a standard `Channel` variant
— it's a scheduling concept (rebuilt from `main` on a timer), not a promotion target. It continues
to exist as a `Custom("nightly")` channel specifically for `GitHubReleaseAdapter`, sharing the
`promote`-free "always freshly published, never promoted into" behavior it has today.

### Use cases (from the plan, resolved against this model)

| Use case | Artifact | Adapter | Channel flow |
|---|---|---|---|
| SecureAutonomy (enterprise binary) | Signed installer | `RemoteFileReleaseAdapter` (S3) | `rc` → staging bucket path; `stable` → prod bucket path |
| Content creator (Wan2.1 video) | Video file | `YouTubeReleaseAdapter` | `Custom("draft")` → unlisted; `stable` → public |
| Game studio (UE5 build) | Depot build | `SteamReleaseAdapter` | `Custom("beta")` → beta branch; `stable` → default branch |

## 5. Versioning rules for code vs. content artifacts

**Decision**: `version_or_label` is a free-form string at the trait level; whether it must parse
as semver is an adapter capability (`ReleaseCapabilities::requires_semver`), not a core-enforced
rule. This directly answers the plan's three versioning questions:

- **Does `ta release run` require semver, or accept arbitrary labels?** Adapter-dependent.
  `GitHubReleaseAdapter` and `RemoteFileReleaseAdapter`-for-code keep today's `normalize_version`
  behavior (semver or plan-phase-ID, validated with today's regex) because TA's own release
  history, `.release.toml`, and `Cargo.toml` version-sync tooling all assume semver. A
  content/game adapter sets `requires_semver = false` and the CLI accepts `"episode-3"`,
  `"turntable-v2-final"`, or anything else verbatim.
- **What does "version" mean for content pipelines?** A project-internal label, chosen by the
  user — never a date stamp imposed by TA. If `--label` is omitted for a `requires_semver = false`
  adapter, `ta release run` does **not** silently invent one (e.g. from a timestamp); it errors
  with an actionable message ("this adapter doesn't use semver — pass `--label <name>`"),
  consistent with the Observability Mandate's "never fail silently, tell the user what to do."
  This is a deliberate rejection of "default to a date stamp" — a generated label is exactly the
  kind of silent, hard-to-search-for identifier the project's error-message conventions exist to
  avoid.
- **How does the channel model map to content?** Via `custom_channel_names`, per §4's table —
  content doesn't get special-cased channel logic, it gets adapter-declared channel *names* layered
  onto the same four-variant lifecycle.

`release.toml`'s `[release]` schema (v0.17.3 item 7) gains one field to carry this:

```toml
[release]
publish_url = "s3://my-bucket/releases"   # adapter inferred from scheme (§6)
default_channel = "stable"
version_files = ["Cargo.toml"]            # empty/omitted for non-semver adapters
changelog_cmd = ""                        # optional
```

No new `requires_semver` field is needed in `release.toml` itself — that's a property of the
*adapter*, declared in Rust (or, for plugin adapters, in `plugin.toml`), not something a project
config can override. A project can't make GitHub accept non-semver tags by editing a config file;
the constraint is real and belongs on the adapter.

## 6. Adapter discovery (URL-scheme registry)

Modeled on the `ta-db-proxy` registry (`crates/ta-db-proxy/src/registry.rs`) — the exact pattern
already proven for exactly this "core has zero awareness of which specific backends exist"
requirement:

```toml
[release]
# publish_url = "s3://my-bucket/releases"                 → RemoteFileReleaseAdapter
# publish_url = "sftp://host/path"                         → RemoteFileReleaseAdapter
# publish_url = "https://deploy.example.com/webhook"       → ServiceReleaseAdapter (v0.17.4+)
# publish_url = "youtube://channel/UCxxxx"                 → YouTubeReleaseAdapter
# publish_url = "steam://app/<appid>"                      → SteamReleaseAdapter
# (no publish_url; git remote present)                     → GitHubReleaseAdapter (default)
```

`ReleaseAdapterRegistry::resolve(publish_url: Option<&str>) -> Result<Box<dyn ReleaseAdapter>>`
checks, in order: (1) `--adapter <name>` CLI override, (2) `publish_url` scheme against built-in
adapters, (3) `publish_url` scheme against discovered plugin adapters (`.ta/plugins/release/*/`,
same manifest+JSON-over-stdio pattern as `docs/community-db-plugin.md`/`community-ide-plugin.md`),
(4) no `publish_url` at all → `GitHubReleaseAdapter` if a git remote is configured, else error.

### Resolving a real conflict found during this review

The plan's "Built-in adapters to implement in v0.17.1" list (now v0.17.3/17.4) includes
`SteamReleaseAdapter` and `AppStoreReleaseAdapter` as if they were ordinary in-tree Rust adapters
like `GitHubReleaseAdapter`. v0.17.4 item 3 separately introduces an external-process **plugin**
protocol specifically because "adding a new domain action requires a TA core code change" is the
problem it's solving, and its own item list only ships `YouTubeReleaseAdapter` natively — Steam and
App Store are conspicuously absent from v0.17.4's native item list despite being named in v0.17.2's
"built-in adapters" section.

**Resolution**: `SteamReleaseAdapter` and `AppStoreReleaseAdapter` ship as **plugins**, not
in-tree Rust, and v0.17.2's "built-in" language for them is corrected here. Rationale: both require
a proprietary, licensed platform SDK (Steamworks SDK; App Store Connect / `altool`) that TA's own
binary cannot vendor or redistribute — bundling either into `ta-release`'s own crate would tie
TA's release cadence to SDK license/version churn neither TA nor most users need. This is the same
reasoning v0.17.5.3 already applies to domain adapters generally ("no live external API dependency
in TA's own test suite"). `YouTubeReleaseAdapter` stays native because the YouTube Data API v3 is a
plain REST API with no proprietary SDK dependency — no reason to push it out to a plugin process.
Concretely: v0.17.3 ships `GitHubReleaseAdapter` + `RemoteFileReleaseAdapter` native; v0.17.4 ships
`YouTubeReleaseAdapter` native plus the plugin protocol itself (already item 3), and Steam/App
Store become the plugin protocol's first real-world example implementations (reference plugins
under `plugins/ta-release-steam/`, `plugins/ta-release-appstore/`, docs modeled on
`docs/community-db-plugin.md`) rather than new v0.17.4 Rust items.

## 7. Migration path

Nothing is removed in v0.17.3. The existing pipeline-YAML system (`ReleasePipeline`,
`PipelineStep`, `.ta/release.yaml`) is retained wholesale for pre-publish orchestration; only the
publish *ending* changes shape.

| Today | v0.17.3+ | Notes |
|---|---|---|
| `ta release run <version>` ends with an implicit git-tag-and-push, GitHub Actions does the rest | `ta release run <version> [--channel <ch>]` — pipeline unchanged, last step becomes `PublishStep` calling `ReleaseAdapter::publish` | Default adapter (no `publish_url` configured) resolves to `GitHubReleaseAdapter`, preserving today's exact behavior — this is the zero-config compatibility path |
| `ta release run <version> --label <tag> --prerelease` | `ta release run <version> --channel rc` (or `stable`) | `--label`/`--prerelease` continue to work as adapter-specific pass-through flags on `GitHubReleaseAdapter`, not removed |
| `ta release dispatch <tag> [--prerelease]` | `ta release run <version> --channel <ch>` (preferred) or `ta release dispatch <tag>` (kept as a deprecated alias, prints a pointer to `run`) | `dispatch`'s CI-wait/build-first checks fold into `prepare()`'s preflight step |
| `ta release validate-tag <tag>` | `ta release run <version> --dry-run` or `ta release validate <version>` | `--dry-run` becomes adapter-aware: calls `prepare()` only, never `publish()` |
| Re-running the whole pipeline to "promote" an RC to stable | `ta release promote <tag> --to stable` | No rebuild — this is the core UX win of the new model |
| Manual `.release.toml` edits for `stable_release_tag`/`last_release_tag` | Still happens (via `update_release_toml` pipeline step), but is now understood as **local bookkeeping the adapter's `status()` can supersede** once `supports_live_status = true` | `.release.toml` is not removed — GitHub-adapter installs without live-status support still need it |

**Removal is explicitly out of scope for this design.** `dispatch`/`validate-tag` get a deprecation
warning in v0.17.3, not a removal date — per this project's own Deferred Items Policy, an actual
removal needs its own phase with a stated timeline, not a silent drop.

## 8. What's deliberately deferred (not decided here)

- **Adapter plugin manifest schema** (`plugin.toml` fields for `type = "release"`) — v0.17.4 item
  3's job, should mirror `docs/community-db-plugin.md`'s five-method table shape but for
  `prepare`/`publish`/`promote`/`status`/`list`.
- **`ServiceReleaseAdapter`** (generic webhook target, named in the plan's URL-scheme example but
  never given its own item) — not scoped to any phase yet. Flagging here so it isn't silently
  dropped: revisit when a concrete webhook-based use case shows up, likely alongside
  `RemoteFileReleaseAdapter` in v0.17.3 if trivial, otherwise v0.17.4.
- **Homebrew tap auto-update** (v0.17.4 item 2) is not a `ReleaseAdapter` at all — it's a
  post-publish side effect of `GitHubReleaseAdapter` reaching `stable` (opening a PR in a separate
  tap repo), not a distinct publish target. No trait change needed; it hooks in as a
  `GitHubReleaseAdapter`-specific `promote`/`publish` follow-up action, implemented directly in
  v0.17.4 item 2 as scoped.
- **Advisor natural-language mapping** ("release this as an RC") — depends on `ta-brain::route()`
  wiring, not on the adapter trait; no blocking dependency on v0.17.3, can land opportunistically.

## 9. Deliverable checklist (per PLAN.md v0.17.2)

- [x] Final `ta release` command surface — §2.
- [x] `ReleaseAdapter` trait definition (Rust trait sketch) — §3.
- [x] Channel model and lifecycle (draft → rc → stable → lts) — §4.
- [x] Versioning rules for code vs. content artifacts — §5.
- [x] Migration path from `ta release dispatch` / manual tagging — §7.
