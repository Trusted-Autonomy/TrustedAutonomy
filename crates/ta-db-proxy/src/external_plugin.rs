//! External-process `DbProxyPlugin` implementation (§2.2 Plugin category),
//! discovered via `.ta/plugins/db/<name>/plugin.toml`.

use crate::classification::QueryClass;
use crate::error::{ProxyError, Result};
use crate::plugin::{DbProxyPlugin, ProxyConfig, ProxyHandle};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// `DbProxyPlugin` that shells out to an external process for query
/// classification and mutation replay, via the shared `ta_plugin` transport.
#[derive(Debug)]
pub struct ExternalDbProxyPlugin {
    name: String,
    command: String,
    args: Vec<String>,
    timeout: Duration,
}

impl ExternalDbProxyPlugin {
    /// Resolve `name` via `.ta/plugins/db/<name>/plugin.toml` discovery.
    pub fn discover(name: &str, project_root: &Path) -> Result<Self> {
        let found = ta_plugin::find_plugin("db", name, project_root).ok_or_else(|| {
            ProxyError::Plugin(format!("no db plugin manifest found for '{name}'"))
        })?;
        Ok(Self {
            name: found.manifest.name.clone(),
            command: found
                .plugin_dir
                .join(&found.manifest.command)
                .to_string_lossy()
                .to_string(),
            args: found.manifest.args.clone(),
            timeout: found.manifest.timeout(DEFAULT_TIMEOUT_SECS),
        })
    }
}

#[derive(Serialize)]
struct ClassifyParams<'a> {
    query: &'a str,
}

#[derive(Deserialize)]
struct ClassifyResult {
    class: QueryClass,
}

#[derive(Serialize)]
struct ApplyMutationParams<'a> {
    upstream_dsn: &'a str,
    uri: &'a str,
    before: Option<&'a serde_json::Value>,
    after: &'a serde_json::Value,
    staging_dir: String,
}

impl ExternalDbProxyPlugin {
    /// Send one `{method,params}` request via the canonical `ta_plugin`
    /// envelope (the reference shape for new Plugin-category integrations —
    /// unlike VCS/messaging/social, db plugins have no pre-existing wire
    /// format to preserve) and return the parsed `result` on `ok: true`.
    fn call<Req: Serialize>(&self, method: &str, params: &Req) -> Result<serde_json::Value> {
        let params_value = serde_json::to_value(params).map_err(|e| {
            ProxyError::Plugin(format!("serialize params for method '{method}': {e}"))
        })?;
        let request = ta_plugin::PluginRequest::new(method, params_value);
        let response: ta_plugin::PluginResponse = ta_plugin::transport::call_json(
            &self.name,
            method,
            &self.command,
            &self.args,
            Path::new("."),
            &request,
            self.timeout,
        )
        .map_err(|e| {
            ProxyError::Plugin(format!(
                "db plugin '{}' method '{method}' failed: {e}",
                self.name
            ))
        })?;

        if !response.ok {
            return Err(ProxyError::Plugin(format!(
                "db plugin '{}' method '{method}' failed: {}",
                self.name,
                response
                    .error
                    .unwrap_or_else(|| "unknown error".to_string())
            )));
        }
        Ok(response.result)
    }
}

impl DbProxyPlugin for ExternalDbProxyPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn wire_protocol(&self) -> &str {
        "external"
    }

    fn start(&self, config: ProxyConfig) -> Result<Box<dyn ProxyHandle>> {
        Err(ProxyError::Plugin(format!(
            "external db plugin '{}' listener lifecycle is not yet wired to a long-lived process \
             (requested listen_addr: {}) — use classify_query/apply_mutation only until \
             v0.17.0.12.15's Channel/Listener support lands",
            self.name, config.listen_addr
        )))
    }

    fn classify_query(&self, query: &str) -> QueryClass {
        let params = ClassifyParams { query };
        match self.call("classify_query", &params) {
            Ok(result) => serde_json::from_value::<ClassifyResult>(result)
                .map(|r| r.class)
                .unwrap_or(QueryClass::Unknown),
            Err(_) => QueryClass::Unknown,
        }
    }

    fn apply_mutation(
        &self,
        upstream_dsn: &str,
        uri: &str,
        before: Option<&serde_json::Value>,
        after: &serde_json::Value,
        staging_dir: &Path,
    ) -> Result<()> {
        let params = ApplyMutationParams {
            upstream_dsn,
            uri,
            before,
            after,
            staging_dir: staging_dir.to_string_lossy().to_string(),
        };
        self.call("apply_mutation", &params)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_reports_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let err = ExternalDbProxyPlugin::discover("nonexistent", dir.path()).unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[cfg(unix)]
    #[test]
    fn discover_and_classify_query_via_mock_plugin() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join(".ta/plugins/db/mockdb");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let script_path = plugin_dir.join("mockdb-plugin.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nread -r line\necho '{\"ok\":true,\"result\":{\"class\":\"read\"}}'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();

        std::fs::write(
            plugin_dir.join("plugin.toml"),
            format!(
                "name = \"mockdb\"\ntype = \"db\"\ncommand = \"{}\"\n",
                script_path.display()
            ),
        )
        .unwrap();

        let plugin = ExternalDbProxyPlugin::discover("mockdb", dir.path()).unwrap();
        assert_eq!(plugin.classify_query("SELECT 1"), QueryClass::Read);
    }
}
