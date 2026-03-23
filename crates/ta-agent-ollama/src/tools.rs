// tools.rs — Tool implementations for ta-agent-ollama (v0.13.16 item 2).
//
// Tool set:
//   bash_exec    — Execute a shell command in the working directory
//   file_read    — Read a file (UTF-8)
//   file_write   — Write/overwrite a file (UTF-8)
//   file_list    — List directory contents (optionally recursive)
//   web_fetch    — Fetch a URL and return the response body
//   memory_read  — Read a memory entry from the snapshot
//   memory_write — Queue a memory entry for writing on exit
//   memory_search — Search snapshot for entries matching a query

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Result;
use serde_json::Value;

use crate::memory::MemoryBridge;

/// The full tool set available to the agent.
pub struct ToolSet {
    workdir: PathBuf,
    memory: MemoryBridge,
    http_client: reqwest::Client,
}

impl ToolSet {
    pub fn new(workdir: PathBuf, memory: MemoryBridge) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("ta-agent-ollama/0.13.16")
            .build()
            .unwrap_or_default();
        Self {
            workdir,
            memory,
            http_client,
        }
    }

    /// Return JSON schema definitions for all tools (for the tools array in chat requests).
    pub fn definitions(&self) -> Vec<Value> {
        vec![
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "bash_exec",
                    "description": "Execute a shell command in the working directory. Returns stdout, stderr, and exit code.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": {
                                "type": "string",
                                "description": "Shell command to execute."
                            },
                            "timeout_secs": {
                                "type": "integer",
                                "description": "Timeout in seconds (default: 60)."
                            }
                        },
                        "required": ["command"]
                    }
                }
            }),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "file_read",
                    "description": "Read the contents of a file as UTF-8 text.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "File path (relative to working directory or absolute)."
                            }
                        },
                        "required": ["path"]
                    }
                }
            }),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "file_write",
                    "description": "Write content to a file, creating parent directories as needed.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "File path to write."
                            },
                            "content": {
                                "type": "string",
                                "description": "UTF-8 content to write."
                            }
                        },
                        "required": ["path", "content"]
                    }
                }
            }),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "file_list",
                    "description": "List files and directories at the given path.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Directory path to list (default: working directory)."
                            },
                            "recursive": {
                                "type": "boolean",
                                "description": "List recursively (default: false)."
                            }
                        },
                        "required": []
                    }
                }
            }),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "web_fetch",
                    "description": "Fetch a URL and return the response body as text.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "url": {
                                "type": "string",
                                "description": "URL to fetch."
                            }
                        },
                        "required": ["url"]
                    }
                }
            }),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "memory_read",
                    "description": "Read a memory entry by key from the TA memory snapshot.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "key": {
                                "type": "string",
                                "description": "Memory key (e.g., 'arch:module-map:my-service')."
                            }
                        },
                        "required": ["key"]
                    }
                }
            }),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "memory_write",
                    "description": "Write a memory entry to be persisted after this agent run.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "key": {
                                "type": "string",
                                "description": "Memory key (e.g., 'arch:module-map:my-service')."
                            },
                            "value": {
                                "type": "string",
                                "description": "Value to store."
                            },
                            "tags": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Tags for categorisation (e.g., ['architecture', 'api'])."
                            },
                            "category": {
                                "type": "string",
                                "description": "Category: architecture, convention, state, preference, history, negative_path."
                            }
                        },
                        "required": ["key", "value"]
                    }
                }
            }),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "memory_search",
                    "description": "Search the TA memory snapshot for entries matching a query.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "Search query (keyword or phrase)."
                            }
                        },
                        "required": ["query"]
                    }
                }
            }),
        ]
    }

    /// Return plain-text descriptions of all tools (for CoT fallback mode).
    pub fn text_descriptions(&self) -> String {
        "\
bash_exec(command: str, timeout_secs?: int)
  Execute a shell command. Returns {stdout, stderr, exit_code}.

file_read(path: str)
  Read file contents. Returns {content: str}.

file_write(path: str, content: str)
  Write content to a file. Returns {success: bool}.

file_list(path?: str, recursive?: bool)
  List directory entries. Returns {entries: [str]}.

web_fetch(url: str)
  Fetch a URL. Returns {content: str, status: int}.

memory_read(key: str)
  Read a memory entry. Returns {value: str | null}.

memory_write(key: str, value: str, tags?: [str], category?: str)
  Write a memory entry. Returns {success: bool}.

memory_search(query: str)
  Search memory. Returns {results: [{key: str, value: str}]}.
"
        .to_string()
    }

    /// Dispatch a tool call by name.
    pub async fn call(&self, name: &str, args: &Value) -> Result<Value> {
        match name {
            "bash_exec" => self.bash_exec(args).await,
            "file_read" => self.file_read(args),
            "file_write" => self.file_write(args),
            "file_list" => self.file_list(args),
            "web_fetch" => self.web_fetch(args).await,
            "memory_read" => self.memory_read(args),
            "memory_write" => self.memory_write(args),
            "memory_search" => self.memory_search(args),
            _ => Ok(serde_json::json!({
                "error": format!("Unknown tool: '{}'. Available: bash_exec, file_read, file_write, file_list, web_fetch, memory_read, memory_write, memory_search", name)
            })),
        }
    }

    /// Flush memory entries to the exit-file.
    pub fn flush_memory(&self) -> Result<()> {
        self.memory.flush()
    }

    // ── Tool implementations ──────────────────────────────────────────────

    async fn bash_exec(&self, args: &Value) -> Result<Value> {
        let command = args
            .get("command")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow::anyhow!("bash_exec requires 'command' argument"))?;

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|t| t.as_u64())
            .unwrap_or(60);

        #[cfg(windows)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/c", command]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", command]);
            c
        };

        cmd.current_dir(&self.workdir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), cmd.output())
                .await
                .map_err(|_| {
                    anyhow::anyhow!("Command timed out after {}s: {}", timeout_secs, command)
                })?
                .map_err(|e| anyhow::anyhow!("Failed to execute command '{}': {}", command, e))?;

        Ok(serde_json::json!({
            "stdout": String::from_utf8_lossy(&output.stdout).to_string(),
            "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
            "exit_code": output.status.code().unwrap_or(-1)
        }))
    }

    fn file_read(&self, args: &Value) -> Result<Value> {
        let path = args
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow::anyhow!("file_read requires 'path' argument"))?;

        let full_path = resolve_path(&self.workdir, path);

        match std::fs::read_to_string(&full_path) {
            Ok(content) => Ok(serde_json::json!({ "content": content })),
            Err(e) => Ok(serde_json::json!({
                "error": format!("Cannot read {}: {}", full_path.display(), e)
            })),
        }
    }

    fn file_write(&self, args: &Value) -> Result<Value> {
        let path = args
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow::anyhow!("file_write requires 'path' argument"))?;

        let content = args
            .get("content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow::anyhow!("file_write requires 'content' argument"))?;

        let full_path = resolve_path(&self.workdir, path);

        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!(
                    "Cannot create directories for {}: {}",
                    full_path.display(),
                    e
                )
            })?;
        }

        std::fs::write(&full_path, content)
            .map_err(|e| anyhow::anyhow!("Cannot write {}: {}", full_path.display(), e))?;

        Ok(serde_json::json!({ "success": true, "path": full_path.display().to_string() }))
    }

    fn file_list(&self, args: &Value) -> Result<Value> {
        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");

        let recursive = args
            .get("recursive")
            .and_then(|r| r.as_bool())
            .unwrap_or(false);

        let full_path = resolve_path(&self.workdir, path);

        let entries = list_dir(&full_path, recursive, 0)?;
        Ok(serde_json::json!({ "entries": entries }))
    }

    async fn web_fetch(&self, args: &Value) -> Result<Value> {
        let url = args
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| anyhow::anyhow!("web_fetch requires 'url' argument"))?;

        match self.http_client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let text = resp
                    .text()
                    .await
                    .unwrap_or_else(|e| format!("(failed to read body: {})", e));
                // Truncate large responses.
                let truncated = if text.len() > 50_000 {
                    format!("{}... [truncated to 50KB]", &text[..50_000])
                } else {
                    text
                };
                Ok(serde_json::json!({ "content": truncated, "status": status }))
            }
            Err(e) => Ok(serde_json::json!({
                "error": format!("Failed to fetch {}: {}", url, e)
            })),
        }
    }

    fn memory_read(&self, args: &Value) -> Result<Value> {
        let key = args
            .get("key")
            .and_then(|k| k.as_str())
            .ok_or_else(|| anyhow::anyhow!("memory_read requires 'key' argument"))?;

        match self.memory.read(key) {
            Some(value) => Ok(serde_json::json!({ "value": value })),
            None => Ok(
                serde_json::json!({ "value": null, "note": "Key not found in memory snapshot." }),
            ),
        }
    }

    fn memory_write(&self, args: &Value) -> Result<Value> {
        let key = args
            .get("key")
            .and_then(|k| k.as_str())
            .ok_or_else(|| anyhow::anyhow!("memory_write requires 'key' argument"))?;
        let value = args
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("memory_write requires 'value' argument"))?;

        let tags = args
            .get("tags")
            .map(MemoryBridge::parse_tags)
            .unwrap_or_default();

        let category = args
            .get("category")
            .and_then(|c| c.as_str())
            .map(String::from);

        self.memory
            .write(key.to_string(), value.to_string(), tags, category);

        Ok(serde_json::json!({ "success": true }))
    }

    fn memory_search(&self, args: &Value) -> Result<Value> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or_else(|| anyhow::anyhow!("memory_search requires 'query' argument"))?;

        let results: Vec<Value> = self
            .memory
            .search(query)
            .into_iter()
            .map(|(k, v)| serde_json::json!({ "key": k, "value": v }))
            .collect();

        Ok(serde_json::json!({ "results": results, "count": results.len() }))
    }
}

/// Resolve a path relative to workdir (or return as-is if absolute).
fn resolve_path(workdir: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        workdir.join(p)
    }
}

/// List directory entries, optionally recursively.
fn list_dir(dir: &Path, recursive: bool, depth: usize) -> Result<Vec<String>> {
    if depth > 10 {
        return Ok(vec!["[max depth reached]".to_string()]);
    }

    let entries = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("Cannot list {}: {}", dir.display(), e))?;

    let mut result = Vec::new();
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();

    for path in paths {
        let display = path.display().to_string();
        result.push(display);
        if recursive && path.is_dir() {
            let children = list_dir(&path, true, depth + 1)?;
            result.extend(children);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryBridge;
    use tempfile::tempdir;

    fn make_tools(workdir: &Path) -> ToolSet {
        ToolSet::new(workdir.to_path_buf(), MemoryBridge::new(None))
    }

    #[test]
    fn file_write_and_read() {
        let dir = tempdir().unwrap();
        let tools = make_tools(dir.path());

        let write_result = tools
            .file_write(&serde_json::json!({"path": "hello.txt", "content": "world"}))
            .unwrap();
        assert_eq!(write_result["success"], true);

        let read_result = tools
            .file_read(&serde_json::json!({"path": "hello.txt"}))
            .unwrap();
        assert_eq!(read_result["content"], "world");
    }

    #[test]
    fn file_read_missing_returns_error_json() {
        let dir = tempdir().unwrap();
        let tools = make_tools(dir.path());
        let result = tools
            .file_read(&serde_json::json!({"path": "nonexistent.txt"}))
            .unwrap();
        assert!(result.get("error").is_some());
    }

    #[test]
    fn file_list_basic() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        let tools = make_tools(dir.path());
        let result = tools.file_list(&serde_json::json!({"path": "."})).unwrap();
        let entries = result["entries"].as_array().unwrap();
        assert!(entries.len() >= 2);
    }

    #[test]
    fn file_write_creates_subdirectory() {
        let dir = tempdir().unwrap();
        let tools = make_tools(dir.path());
        let result = tools
            .file_write(&serde_json::json!({
                "path": "sub/dir/file.txt",
                "content": "nested"
            }))
            .unwrap();
        assert_eq!(result["success"], true);
        let content = std::fs::read_to_string(dir.path().join("sub/dir/file.txt")).unwrap();
        assert_eq!(content, "nested");
    }

    #[test]
    fn tool_definitions_non_empty() {
        let dir = tempdir().unwrap();
        let tools = make_tools(dir.path());
        let defs = tools.definitions();
        assert_eq!(defs.len(), 8);
        let names: Vec<_> = defs
            .iter()
            .filter_map(|d| d["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"bash_exec"));
        assert!(names.contains(&"memory_write"));
    }

    #[tokio::test]
    async fn bash_exec_echo() {
        let dir = tempdir().unwrap();
        let tools = make_tools(dir.path());
        let result = tools
            .call(
                "bash_exec",
                &serde_json::json!({"command": "echo hello-world"}),
            )
            .await
            .unwrap();
        assert!(result["stdout"]
            .as_str()
            .unwrap_or("")
            .contains("hello-world"));
        assert_eq!(result["exit_code"], 0);
    }

    #[tokio::test]
    async fn bash_exec_exit_code() {
        let dir = tempdir().unwrap();
        let tools = make_tools(dir.path());
        #[cfg(windows)]
        let cmd = "exit /b 42";
        #[cfg(not(windows))]
        let cmd = "exit 42";
        let result = tools
            .call("bash_exec", &serde_json::json!({"command": cmd}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 42);
    }

    #[tokio::test]
    async fn unknown_tool_returns_error_json() {
        let dir = tempdir().unwrap();
        let tools = make_tools(dir.path());
        let result = tools
            .call("nonexistent_tool", &serde_json::json!({}))
            .await
            .unwrap();
        assert!(result["error"].as_str().unwrap().contains("Unknown tool"));
    }

    #[tokio::test]
    async fn memory_write_then_flush() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("out.json");
        let bridge = MemoryBridge::new(Some(&out));
        let tools = ToolSet::new(dir.path().to_path_buf(), bridge);

        tools
            .call(
                "memory_write",
                &serde_json::json!({
                    "key": "test:key",
                    "value": "test value",
                    "tags": ["test"]
                }),
            )
            .await
            .unwrap();

        tools.flush_memory().unwrap();
        assert!(out.exists());
        let content: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(content[0]["key"], "test:key");
    }
}
