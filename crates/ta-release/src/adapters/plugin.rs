//! `PluginReleaseAdapter` — `ReleaseAdapter` over an external process, JSON-over-stdio.
//!
//! Same pattern as `ta-submit::ExternalVcsAdapter` (PLAN.md v0.17.4 item 3: "same pattern
//! as VCS plugins"), built on the shared `ta-plugin` transport/envelope/discovery crate
//! rather than a second hand-rolled protocol. `docs/release-design.md` §6/§8: this is what
//! lets `SteamReleaseAdapter`/`AppStoreReleaseAdapter` ship as out-of-tree plugins instead
//! of vendoring proprietary SDKs into `ta-release`.
//!
//! ## Wire protocol
//!
//! One JSON line request, one JSON line response, fresh process per call — uses the
//! canonical `ta_plugin::envelope::{PluginRequest, PluginResponse}` shape (`{"method",
//! "params"}` in, `{"ok","result"}`/`{"ok":false,"error"}` out).
//!
//! | Method      | Called from                          |
//! |-------------|---------------------------------------|
//! | `handshake` | `PluginReleaseAdapter::new` (once)    |
//! | `prepare`   | `ReleaseAdapter::prepare`              |
//! | `publish`   | `ReleaseAdapter::publish`              |
//! | `promote`   | `ReleaseAdapter::promote` (optional)   |
//! | `status`    | `ReleaseAdapter::status` (optional)    |
//! | `list`      | `ReleaseAdapter::list` (optional)      |
//!
//! `promote`/`status`/`list` are only called if the plugin's `handshake` response declares
//! the matching capability string (`"promote"`, `"status"`, `"status"` also gates `list`) —
//! a minimal plugin implementing only `prepare`/`publish` never receives those calls, same
//! spirit as `ReleaseAdapter`'s own default-implemented optional methods. Capability strings
//! prefixed `channel:` declare adapter-native channel names (`ReleaseCapabilities::custom_channel_names`),
//! e.g. `"channel:beta"` for Steam's beta branch.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use ta_plugin::envelope::{
    HandshakeParams, HandshakeResult, PluginRequest, PluginResponse, PROTOCOL_VERSION,
};
use ta_plugin::manifest::PluginManifest;

use crate::adapter::{
    Channel, PreparedRelease, ReleaseAdapter, ReleaseAsset, ReleaseCapabilities, ReleaseContext,
    ReleaseRef, ReleaseStatus,
};
use crate::error::{ReleaseError, Result};

const RELEASE_PLUGIN_KIND: &str = "release";

#[derive(Serialize)]
struct PrepareParams {
    version_or_label: String,
    channel: String,
    commits: String,
    workspace_root: String,
}

#[derive(Deserialize)]
struct PrepareResultWire {
    idempotency_key: String,
    resolved_label: String,
}

#[derive(Serialize)]
struct AssetParam {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

#[derive(Serialize)]
struct PublishParams {
    idempotency_key: String,
    resolved_label: String,
    assets: Vec<AssetParam>,
}

#[derive(Deserialize)]
struct PublishResultWire {
    external_id: String,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Serialize)]
struct PromoteParams {
    external_id: String,
    #[serde(default)]
    url: Option<String>,
    channel: String,
}

#[derive(Serialize)]
struct StatusParams {
    version: String,
}

#[derive(Deserialize)]
struct StatusResultWire {
    known: bool,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    asset_checksums: Vec<(String, String)>,
}

#[derive(Serialize)]
struct ListParams {
    limit: usize,
}

#[derive(Deserialize)]
struct ListResultWire {
    #[serde(default)]
    releases: Vec<StatusResultWire>,
}

fn status_wire_to_status(wire: StatusResultWire) -> ReleaseStatus {
    if !wire.known {
        return ReleaseStatus::Unknown;
    }
    ReleaseStatus::Known {
        channels: wire.channels.iter().map(|s| Channel::parse(s)).collect(),
        published_at: wire.published_at,
        asset_checksums: wire.asset_checksums,
    }
}

/// `ReleaseAdapter` implementation that delegates every operation to an external plugin
/// process discovered via `.ta/plugins/release/<name>/plugin.toml` (project-local) or
/// `~/.config/ta/plugins/release/<name>/plugin.toml` (user-global).
pub struct PluginReleaseAdapter {
    command: String,
    args: Vec<String>,
    work_dir: PathBuf,
    adapter_name: String,
    plugin_version: String,
    timeout: Duration,
    caps: ReleaseCapabilities,
    /// Raw capability strings from handshake — used to gate optional method calls
    /// (`promote`/`status`/`list`) the same way `ExternalVcsAdapter::has_capability` does.
    declared_capabilities: Vec<String>,
}

impl std::fmt::Debug for PluginReleaseAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginReleaseAdapter")
            .field("adapter_name", &self.adapter_name)
            .field("command", &self.command)
            .finish_non_exhaustive()
    }
}

impl PluginReleaseAdapter {
    /// Construct a new adapter and perform the initial handshake. Fails fast — mirrors
    /// `ExternalVcsAdapter::new` — so a broken plugin is caught at `resolve()` time, not
    /// mid-pipeline at `publish()`.
    pub fn new(manifest: &PluginManifest, work_dir: &Path) -> Result<Self> {
        manifest
            .validate(RELEASE_PLUGIN_KIND)
            .map_err(|e| ReleaseError::Config(e.to_string()))?;
        let timeout = manifest.timeout(30);

        let handshake_params = HandshakeParams {
            ta_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
        };
        let request = PluginRequest::new(
            "handshake",
            serde_json::to_value(&handshake_params).unwrap_or_default(),
        );
        let response = call_plugin(
            &manifest.name,
            "handshake",
            &manifest.command,
            &manifest.args,
            work_dir,
            &request,
            timeout,
        )?;
        if !response.ok {
            return Err(ReleaseError::PrepareFailed {
                adapter: manifest.name.clone(),
                reason: format!(
                    "plugin handshake failed: {}",
                    response.error.as_deref().unwrap_or("unknown error")
                ),
            });
        }

        let result: HandshakeResult = serde_json::from_value(response.result).map_err(|e| {
            ReleaseError::Config(format!(
                "release plugin '{}' returned an invalid handshake response: {e}",
                manifest.name
            ))
        })?;
        if result.protocol_version != PROTOCOL_VERSION {
            return Err(ReleaseError::Config(format!(
                "release plugin '{}' uses protocol version {} but TA requires {}. \
                 Upgrade the plugin or downgrade TA.",
                manifest.name, result.protocol_version, PROTOCOL_VERSION
            )));
        }

        let adapter_name = if result.adapter_name.is_empty() {
            manifest.name.clone()
        } else {
            result.adapter_name
        };
        let caps = ReleaseCapabilities {
            requires_semver: result.capabilities.iter().any(|c| c == "requires_semver"),
            supports_promote: result.capabilities.iter().any(|c| c == "promote"),
            supports_live_status: result.capabilities.iter().any(|c| c == "status"),
            custom_channel_names: result
                .capabilities
                .iter()
                .filter_map(|c| c.strip_prefix("channel:").map(|s| s.to_string()))
                .collect(),
        };

        tracing::info!(
            plugin = %manifest.name,
            plugin_version = %result.plugin_version,
            adapter = %adapter_name,
            "Release plugin handshake successful"
        );

        Ok(Self {
            command: manifest.command.clone(),
            args: manifest.args.clone(),
            work_dir: work_dir.to_path_buf(),
            adapter_name,
            plugin_version: result.plugin_version,
            timeout,
            caps,
            declared_capabilities: result.capabilities,
        })
    }

    pub fn plugin_version(&self) -> &str {
        &self.plugin_version
    }

    fn has_capability(&self, cap: &str) -> bool {
        self.declared_capabilities.iter().any(|c| c == cap)
    }

    fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        let request = PluginRequest::new(method, params);
        let response = call_plugin(
            &self.adapter_name,
            method,
            &self.command,
            &self.args,
            &self.work_dir,
            &request,
            self.timeout,
        )?;
        if !response.ok {
            return Err(ReleaseError::PublishFailed {
                adapter: self.adapter_name.clone(),
                reason: format!(
                    "method '{method}' failed: {}",
                    response.error.as_deref().unwrap_or("unknown error")
                ),
            });
        }
        serde_json::from_value(response.result).map_err(|e| {
            ReleaseError::Config(format!(
                "release plugin '{}' method '{method}' returned an invalid response: {e}",
                self.adapter_name
            ))
        })
    }
}

impl ReleaseAdapter for PluginReleaseAdapter {
    fn name(&self) -> &str {
        &self.adapter_name
    }

    fn capabilities(&self) -> ReleaseCapabilities {
        self.caps.clone()
    }

    fn prepare(&self, ctx: &ReleaseContext) -> Result<PreparedRelease> {
        let params = PrepareParams {
            version_or_label: ctx.version_or_label.clone(),
            channel: ctx.channel.as_str().to_string(),
            commits: ctx.commits.clone(),
            workspace_root: ctx.workspace_root.display().to_string(),
        };
        let result: PrepareResultWire =
            self.call("prepare", serde_json::to_value(&params).unwrap_or_default())?;
        Ok(PreparedRelease {
            idempotency_key: result.idempotency_key,
            resolved_label: result.resolved_label,
        })
    }

    fn publish(&self, prepared: &PreparedRelease, assets: &[ReleaseAsset]) -> Result<ReleaseRef> {
        let params = PublishParams {
            idempotency_key: prepared.idempotency_key.clone(),
            resolved_label: prepared.resolved_label.clone(),
            assets: assets
                .iter()
                .map(|a| AssetParam {
                    path: a.path.display().to_string(),
                    label: a.label.clone(),
                })
                .collect(),
        };
        let result: PublishResultWire =
            self.call("publish", serde_json::to_value(&params).unwrap_or_default())?;
        Ok(ReleaseRef {
            adapter: self.adapter_name.clone(),
            external_id: result.external_id,
            url: result.url,
        })
    }

    fn promote(&self, release_ref: &ReleaseRef, channel: &Channel) -> Result<()> {
        if !self.has_capability("promote") {
            return Err(ReleaseError::Unsupported {
                adapter: self.adapter_name.clone(),
                operation: "promote".to_string(),
            });
        }
        let params = PromoteParams {
            external_id: release_ref.external_id.clone(),
            url: release_ref.url.clone(),
            channel: channel.as_str().to_string(),
        };
        self.call::<serde_json::Value>(
            "promote",
            serde_json::to_value(&params).unwrap_or_default(),
        )?;
        Ok(())
    }

    fn status(&self, version: &str) -> Result<ReleaseStatus> {
        if !self.has_capability("status") {
            return Ok(ReleaseStatus::Unknown);
        }
        let params = StatusParams {
            version: version.to_string(),
        };
        let result: StatusResultWire =
            self.call("status", serde_json::to_value(&params).unwrap_or_default())?;
        Ok(status_wire_to_status(result))
    }

    fn list(&self, limit: usize) -> Result<Vec<ReleaseStatus>> {
        if !self.has_capability("status") {
            return Ok(Vec::new());
        }
        let params = ListParams { limit };
        let result: ListResultWire =
            self.call("list", serde_json::to_value(&params).unwrap_or_default())?;
        Ok(result
            .releases
            .into_iter()
            .map(status_wire_to_status)
            .collect())
    }
}

/// Spawn the plugin, send one JSON request, read one JSON response. Delegates
/// spawn/framing/timeout to the shared `ta_plugin::transport` crate.
fn call_plugin(
    name: &str,
    method: &str,
    command: &str,
    extra_args: &[String],
    work_dir: &Path,
    request: &PluginRequest,
    timeout: Duration,
) -> Result<PluginResponse> {
    ta_plugin::transport::call_json(
        name, method, command, extra_args, work_dir, request, timeout,
    )
    .map_err(|e| ReleaseError::SubprocessFailed {
        command: command.to_string(),
        reason: e.to_string(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn write_mock_plugin(script: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let name = format!("ta-release-mock-plugin-{pid}-{n}");
        // Use /tmp directly (tmpfs) on Linux to avoid ETXTBSY races against
        // Nix devShell's overlayfs-backed TMPDIR — same reasoning as
        // ta-submit::external_vcs_adapter's test helper.
        #[cfg(target_os = "linux")]
        let path = std::path::PathBuf::from("/tmp").join(&name);
        #[cfg(not(target_os = "linux"))]
        let path = std::env::temp_dir().join(&name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        f.sync_all().unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn mock_manifest(command: &str, capabilities: &[&str]) -> PluginManifest {
        PluginManifest {
            name: "steam".to_string(),
            version: "0.1.0".to_string(),
            kind: RELEASE_PLUGIN_KIND.to_string(),
            command: command.to_string(),
            args: vec![],
            capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
            description: None,
            timeout_secs: Some(10),
            protocol_version: None,
            min_daemon_version: None,
            source_url: None,
            staging_env: Default::default(),
        }
    }

    /// A mock "steamcmd"-backed plugin (PLAN.md v0.17.4 item 4's "Steam steamcmd mock"
    /// test) — a shell script standing in for a real `plugins/ta-release-steam/`
    /// executable, exercising the full round trip: handshake, prepare, publish,
    /// promote, status.
    fn steam_mock_script() -> String {
        r#"#!/bin/sh
read -r line
case "$line" in
  *'"method":"handshake"'*)
    echo '{"ok":true,"result":{"plugin_version":"1.0.0","protocol_version":1,"adapter_name":"steam","capabilities":["promote","status","channel:beta","channel:default"]}}'
    ;;
  *'"method":"prepare"'*)
    echo '{"ok":true,"result":{"idempotency_key":"build-42","resolved_label":"build-42"}}'
    ;;
  *'"method":"publish"'*)
    echo '{"ok":true,"result":{"external_id":"depot-9001","url":"https://store.steampowered.com/app/123"}}'
    ;;
  *'"method":"promote"'*)
    echo '{"ok":true,"result":{}}'
    ;;
  *'"method":"status"'*)
    echo '{"ok":true,"result":{"known":true,"channels":["beta"],"published_at":"2026-08-01T00:00:00Z","asset_checksums":[]}}'
    ;;
  *)
    echo '{"ok":false,"error":"unknown method"}'
    ;;
esac
"#
        .to_string()
    }

    #[test]
    fn handshake_reports_adapter_name_and_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_path = write_mock_plugin(&steam_mock_script());
        let manifest = mock_manifest(&plugin_path.display().to_string(), &[]);
        let adapter = PluginReleaseAdapter::new(&manifest, dir.path()).unwrap();
        assert_eq!(adapter.name(), "steam");
        assert_eq!(adapter.plugin_version(), "1.0.0");
        let caps = adapter.capabilities();
        assert!(caps.supports_promote);
        assert!(caps.supports_live_status);
        assert!(caps.custom_channel_names.contains(&"beta".to_string()));
        assert!(caps.custom_channel_names.contains(&"default".to_string()));
    }

    #[test]
    fn prepare_then_publish_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_path = write_mock_plugin(&steam_mock_script());
        let manifest = mock_manifest(&plugin_path.display().to_string(), &[]);
        let adapter = PluginReleaseAdapter::new(&manifest, dir.path()).unwrap();

        let ctx = ReleaseContext {
            version_or_label: "build-42".to_string(),
            channel: Channel::Custom("beta".to_string()),
            commits: "fixed physics".to_string(),
            workspace_root: dir.path().to_path_buf(),
        };
        let prepared = adapter.prepare(&ctx).unwrap();
        assert_eq!(prepared.idempotency_key, "build-42");

        let release_ref = adapter.publish(&prepared, &[]).unwrap();
        assert_eq!(release_ref.external_id, "depot-9001");
        assert_eq!(release_ref.adapter, "steam");
        assert_eq!(
            release_ref.url.as_deref(),
            Some("https://store.steampowered.com/app/123")
        );
    }

    #[test]
    fn promote_round_trips_when_capability_declared() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_path = write_mock_plugin(&steam_mock_script());
        let manifest = mock_manifest(&plugin_path.display().to_string(), &[]);
        let adapter = PluginReleaseAdapter::new(&manifest, dir.path()).unwrap();
        let release_ref = ReleaseRef {
            adapter: "steam".to_string(),
            external_id: "depot-9001".to_string(),
            url: None,
        };
        adapter
            .promote(&release_ref, &Channel::Custom("default".to_string()))
            .unwrap();
    }

    #[test]
    fn status_round_trips_when_capability_declared() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_path = write_mock_plugin(&steam_mock_script());
        let manifest = mock_manifest(&plugin_path.display().to_string(), &[]);
        let adapter = PluginReleaseAdapter::new(&manifest, dir.path()).unwrap();
        let status = adapter.status("build-42").unwrap();
        match status {
            ReleaseStatus::Known { channels, .. } => {
                assert!(channels.contains(&Channel::Custom("beta".to_string())));
            }
            ReleaseStatus::Unknown => panic!("expected Known status"),
        }
    }

    #[test]
    fn promote_without_capability_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
read -r line
echo '{"ok":true,"result":{"plugin_version":"1.0.0","protocol_version":1,"adapter_name":"minimal","capabilities":[]}}'
"#;
        let plugin_path = write_mock_plugin(script);
        let manifest = mock_manifest(&plugin_path.display().to_string(), &[]);
        let adapter = PluginReleaseAdapter::new(&manifest, dir.path()).unwrap();
        let release_ref = ReleaseRef {
            adapter: "minimal".to_string(),
            external_id: "x".to_string(),
            url: None,
        };
        let err = adapter.promote(&release_ref, &Channel::Stable).unwrap_err();
        assert!(matches!(err, ReleaseError::Unsupported { .. }));
    }

    #[test]
    fn status_without_capability_returns_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
read -r line
echo '{"ok":true,"result":{"plugin_version":"1.0.0","protocol_version":1,"adapter_name":"minimal","capabilities":[]}}'
"#;
        let plugin_path = write_mock_plugin(script);
        let manifest = mock_manifest(&plugin_path.display().to_string(), &[]);
        let adapter = PluginReleaseAdapter::new(&manifest, dir.path()).unwrap();
        assert_eq!(adapter.status("1.0.0").unwrap(), ReleaseStatus::Unknown);
        assert!(adapter.list(5).unwrap().is_empty());
    }

    #[test]
    fn handshake_protocol_mismatch_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
read -r line
echo '{"ok":true,"result":{"plugin_version":"1.0.0","protocol_version":99,"adapter_name":"bad","capabilities":[]}}'
"#;
        let plugin_path = write_mock_plugin(script);
        let manifest = mock_manifest(&plugin_path.display().to_string(), &[]);
        let err = PluginReleaseAdapter::new(&manifest, dir.path()).unwrap_err();
        assert!(err.to_string().contains("protocol version"));
    }

    #[test]
    fn handshake_failure_response_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
read -r line
echo '{"ok":false,"error":"steamcmd session invalid"}'
"#;
        let plugin_path = write_mock_plugin(script);
        let manifest = mock_manifest(&plugin_path.display().to_string(), &[]);
        let err = PluginReleaseAdapter::new(&manifest, dir.path()).unwrap_err();
        assert!(err.to_string().contains("steamcmd session invalid"));
    }

    #[test]
    fn wrong_manifest_kind_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut manifest = mock_manifest("irrelevant", &[]);
        manifest.kind = "vcs".to_string();
        let err = PluginReleaseAdapter::new(&manifest, dir.path()).unwrap_err();
        assert!(matches!(err, ReleaseError::Config(_)));
    }
}
