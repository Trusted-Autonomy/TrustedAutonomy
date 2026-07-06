//! The canonical `method:String` / `{ok,result,error}` call/response envelope
//! (§2.2 Plugin category) — the reference shape for new Plugin-category
//! integrations. Existing VCS/messaging/social wire types keep their own
//! shapes for backward compatibility; they use `transport::call_json` with
//! their own Req/Resp types instead of this envelope directly.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRequest {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl PluginRequest {
    pub fn new(method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginResponse {
    pub ok: bool,
    #[serde(default)]
    pub result: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PluginResponse {
    pub fn success(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result,
            error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: serde_json::Value::Null,
            error: Some(msg.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeParams {
    pub ta_version: String,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResult {
    pub plugin_version: String,
    pub protocol_version: u32,
    #[serde(default)]
    pub adapter_name: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}
