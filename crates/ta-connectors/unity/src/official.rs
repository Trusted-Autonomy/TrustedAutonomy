// official.rs — Backend for Unity's official com.unity.mcp-server UPM package.
//
// The official Unity MCP server listens on a TCP socket and speaks a
// newline-delimited JSON-RPC 2.0 dialect. This backend connects to that
// socket, sends requests, and parses responses.
//
// When the socket is unreachable (Editor not open or plugin not installed),
// all methods return UnityConnectorError::NotReachable so that the MCP
// gateway can return a structured "connector_not_running" response rather
// than an opaque error.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};

use crate::{
    backend::{
        BuildResult, GameObjectInfo, RenderCaptureResult, SceneInfo, TestRunResult, UnityBackend,
    },
    config::UnityConnectorConfig,
    error::UnityConnectorError,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(60);

// TODO(backend-wiring): remove when gateway tools delegate to OfficialBackend.
#[allow(dead_code)]
pub struct OfficialBackend {
    socket: String,
}

impl OfficialBackend {
    // TODO(backend-wiring): remove when gateway tools delegate to OfficialBackend.
    #[allow(dead_code)]
    pub fn new(config: &UnityConnectorConfig) -> Self {
        Self {
            socket: config.socket.clone(),
        }
    }

    /// Open a TCP connection and send a JSON-RPC request, returning the result Value.
    fn rpc(&self, method: &str, params: Value) -> Result<Value, UnityConnectorError> {
        let stream = TcpStream::connect_timeout(
            &self
                .socket
                .parse()
                .map_err(|e| UnityConnectorError::Config(format!("invalid socket: {}", e)))?,
            CONNECT_TIMEOUT,
        )
        .map_err(|e| UnityConnectorError::NotReachable(self.socket.clone(), e.to_string()))?;

        stream
            .set_read_timeout(Some(READ_TIMEOUT))
            .map_err(UnityConnectorError::Io)?;

        let mut writer = stream.try_clone().map_err(UnityConnectorError::Io)?;

        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&request)
            .map_err(|e| UnityConnectorError::Json(e.to_string()))?;
        line.push('\n');
        writer
            .write_all(line.as_bytes())
            .map_err(UnityConnectorError::Io)?;

        let mut response_line = String::new();
        BufReader::new(stream)
            .read_line(&mut response_line)
            .map_err(UnityConnectorError::Io)?;

        let response: Value = serde_json::from_str(response_line.trim())
            .map_err(|e| UnityConnectorError::Json(e.to_string()))?;

        if let Some(err) = response.get("error") {
            return Err(UnityConnectorError::Protocol(err.to_string()));
        }

        Ok(response["result"].clone())
    }
}

impl UnityBackend for OfficialBackend {
    fn name(&self) -> &str {
        "official"
    }

    fn socket_addr(&self) -> &str {
        &self.socket
    }

    fn build_trigger(
        &self,
        target: &str,
        config: Option<&str>,
    ) -> Result<BuildResult, UnityConnectorError> {
        let params = json!({
            "target": target,
            "config": config.unwrap_or("Release"),
        });
        let result = self.rpc("unity/build/trigger", params)?;

        Ok(BuildResult {
            success: result["success"].as_bool().unwrap_or(false),
            output_path: result["outputPath"].as_str().unwrap_or("").to_string(),
            log_summary: result["logSummary"]
                .as_str()
                .unwrap_or("No log available.")
                .to_string(),
        })
    }

    fn scene_query(&self, scene_path: &str) -> Result<SceneInfo, UnityConnectorError> {
        let params = json!({ "scenePath": scene_path });
        let result = self.rpc("unity/scene/query", params)?;

        let root_objects = result["rootObjects"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|obj| GameObjectInfo {
                instance_id: obj["instanceId"].as_i64().unwrap_or(0),
                name: obj["name"].as_str().unwrap_or("").to_string(),
                tag: obj["tag"].as_str().unwrap_or("Untagged").to_string(),
                layer: obj["layer"].as_i64().unwrap_or(0) as i32,
                active: obj["active"].as_bool().unwrap_or(true),
                components: obj["components"]
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .filter_map(|c| c.as_str().map(|s| s.to_string()))
                    .collect(),
                children: obj["children"]
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .filter_map(|c| c.as_i64())
                    .collect(),
            })
            .collect::<Vec<_>>();

        let total = result["totalObjects"]
            .as_u64()
            .unwrap_or(root_objects.len() as u64) as usize;

        Ok(SceneInfo {
            scene_path: result["scenePath"]
                .as_str()
                .unwrap_or(scene_path)
                .to_string(),
            root_objects,
            total_objects: total,
        })
    }

    fn test_run(&self, filter: Option<&str>) -> Result<TestRunResult, UnityConnectorError> {
        let params = json!({ "filter": filter.unwrap_or("") });
        let result = self.rpc("unity/test/run", params)?;

        let failures = result["failures"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|f| f.as_str().map(|s| s.to_string()))
            .take(10)
            .collect();

        Ok(TestRunResult {
            passed: result["passed"].as_u64().unwrap_or(0) as usize,
            failed: result["failed"].as_u64().unwrap_or(0) as usize,
            skipped: result["skipped"].as_u64().unwrap_or(0) as usize,
            failures,
            duration_secs: result["durationSecs"].as_f64().unwrap_or(0.0),
        })
    }

    fn addressables_build(&self) -> Result<BuildResult, UnityConnectorError> {
        let result = self.rpc("unity/addressables/build", json!({}))?;

        Ok(BuildResult {
            success: result["success"].as_bool().unwrap_or(false),
            output_path: result["outputPath"].as_str().unwrap_or("").to_string(),
            log_summary: result["logSummary"]
                .as_str()
                .unwrap_or("No log available.")
                .to_string(),
        })
    }

    fn render_capture(
        &self,
        camera_path: &str,
        output_path: &str,
    ) -> Result<RenderCaptureResult, UnityConnectorError> {
        let params = json!({
            "cameraPath": camera_path,
            "outputPath": output_path,
        });
        let result = self.rpc("unity/render/capture", params)?;

        Ok(RenderCaptureResult {
            output_path: result["outputPath"]
                .as_str()
                .unwrap_or(output_path)
                .to_string(),
            width: result["width"].as_u64().unwrap_or(1920) as u32,
            height: result["height"].as_u64().unwrap_or(1080) as u32,
        })
    }
}
