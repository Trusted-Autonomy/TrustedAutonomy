// framework.rs — Agent framework manifest and resolution for v0.13.8.
//
// An AgentFramework defines how TA launches an agent backend.
// Built-in frameworks ship with TA; custom frameworks are TOML manifests
// discovered from well-known paths.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How goal context is injected into the agent before launch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContextInjectMode {
    /// Prepend goal context to `context_file` (backup + restore). Default.
    #[default]
    Prepend,
    /// Write context to a temp file and set `TA_GOAL_CONTEXT` env var.
    Env,
    /// Pass context file path as a flag before the prompt arg.
    Arg,
    /// Don't inject context (agent reads it via its own mechanism).
    None,
}

/// How the agent reads/writes TA shared memory.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryInjectMode {
    /// Expose ta-memory as a local MCP server (Claude Code, Codex, Claude-Flow).
    Mcp,
    /// Serialize memory entries into context_file alongside goal context.
    Context,
    /// Write memory snapshot to $TA_MEMORY_PATH before launch.
    Env,
    /// Don't inject memory.
    #[default]
    None,
}

/// Memory configuration for an agent framework.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrameworkMemoryConfig {
    /// How TA injects memory context before launch.
    #[serde(default)]
    pub inject: MemoryInjectMode,
    /// Max memory entries to inject in context mode.
    #[serde(default = "default_max_memory_entries")]
    pub max_entries: usize,
}

fn default_max_memory_entries() -> usize {
    20
}

/// An agent framework manifest — defines how TA launches a specific agent backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFrameworkManifest {
    /// Unique name (e.g., "claude-code", "codex", "qwen-coder").
    pub name: String,
    /// Version of this manifest.
    #[serde(default = "default_version")]
    pub version: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Process command to execute (must be on PATH or absolute).
    pub command: String,
    /// Arguments to pass before the prompt.
    #[serde(default)]
    pub args: Vec<String>,
    /// Stderr substring to watch for to know the agent has started.
    #[serde(default = "default_sentinel")]
    pub sentinel: String,
    /// File that goal context is prepended into (e.g., "CLAUDE.md").
    #[serde(default = "default_context_file")]
    pub context_file: String,
    /// How goal context is injected.
    #[serde(default)]
    pub context_inject: ContextInjectMode,
    /// Memory configuration.
    #[serde(default)]
    pub memory: FrameworkMemoryConfig,
    /// Whether this is a built-in framework (vs user-defined).
    #[serde(default)]
    pub builtin: bool,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

fn default_sentinel() -> String {
    "[goal started]".to_string()
}

fn default_context_file() -> String {
    "CLAUDE.md".to_string()
}

impl AgentFrameworkManifest {
    /// Returns the built-in catalog of known framework manifests.
    pub fn builtins() -> Vec<AgentFrameworkManifest> {
        vec![
            AgentFrameworkManifest {
                name: "claude-code".to_string(),
                version: "1.0.0".to_string(),
                description: "Claude Code — Anthropic's official agentic coding tool (default)"
                    .to_string(),
                command: "claude".to_string(),
                args: vec![
                    "--headless".to_string(),
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                    "--verbose".to_string(),
                ],
                sentinel: "[goal started]".to_string(),
                context_file: "CLAUDE.md".to_string(),
                context_inject: ContextInjectMode::Prepend,
                memory: FrameworkMemoryConfig {
                    inject: MemoryInjectMode::Mcp,
                    max_entries: 20,
                },
                builtin: true,
            },
            AgentFrameworkManifest {
                name: "codex".to_string(),
                version: "1.0.0".to_string(),
                description:
                    "OpenAI Codex CLI — agentic coding with GPT-4o (requires OPENAI_API_KEY)"
                        .to_string(),
                command: "codex".to_string(),
                args: vec!["--approval-mode".to_string(), "full-auto".to_string()],
                sentinel: "[goal started]".to_string(),
                context_file: "AGENTS.md".to_string(),
                context_inject: ContextInjectMode::Prepend,
                memory: FrameworkMemoryConfig {
                    inject: MemoryInjectMode::Mcp,
                    max_entries: 20,
                },
                builtin: true,
            },
            AgentFrameworkManifest {
                name: "claude-flow".to_string(),
                version: "1.0.0".to_string(),
                description: "Claude-Flow — multi-agent swarm orchestration built on Claude Code"
                    .to_string(),
                command: "claude-flow".to_string(),
                args: vec!["run".to_string()],
                sentinel: "[goal started]".to_string(),
                context_file: "CLAUDE.md".to_string(),
                context_inject: ContextInjectMode::Prepend,
                memory: FrameworkMemoryConfig {
                    inject: MemoryInjectMode::Mcp,
                    max_entries: 20,
                },
                builtin: true,
            },
            AgentFrameworkManifest {
                name: "ollama".to_string(),
                version: "1.0.0".to_string(),
                description: "Generic Ollama agent — use with --model ollama/<model-name>"
                    .to_string(),
                command: "ta-agent-ollama".to_string(),
                args: vec![],
                sentinel: "[goal started]".to_string(),
                context_file: "CLAUDE.md".to_string(),
                context_inject: ContextInjectMode::Env,
                memory: FrameworkMemoryConfig {
                    inject: MemoryInjectMode::Env,
                    max_entries: 10,
                },
                builtin: true,
            },
        ]
    }

    /// Look up a built-in framework by name.
    pub fn builtin(name: &str) -> Option<AgentFrameworkManifest> {
        Self::builtins().into_iter().find(|f| f.name == name)
    }

    /// Discover custom framework manifests from well-known paths.
    ///
    /// Search order:
    /// 1. `.ta/agents/` (project-level)
    /// 2. `~/.config/ta/agents/` (user-level)
    pub fn discover(project_root: &Path) -> Vec<AgentFrameworkManifest> {
        let mut manifests = Vec::new();
        let search_dirs = [
            project_root.join(".ta/agents"),
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("~/.config"))
                .join("ta/agents"),
        ];
        for dir in &search_dirs {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                        match std::fs::read_to_string(&path).and_then(|s| {
                            toml::from_str::<AgentFrameworkManifest>(&s).map_err(|e| {
                                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
                            })
                        }) {
                            Ok(mut manifest) => {
                                manifest.builtin = false;
                                manifests.push(manifest);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    "Skipping invalid agent framework manifest: {}",
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }
        manifests
    }

    /// Resolve a framework by name: check builtins first, then discovered.
    pub fn resolve(name: &str, project_root: &Path) -> Option<AgentFrameworkManifest> {
        if let Some(builtin) = Self::builtin(name) {
            return Some(builtin);
        }
        Self::discover(project_root)
            .into_iter()
            .find(|f| f.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn builtins_are_non_empty() {
        let builtins = AgentFrameworkManifest::builtins();
        assert!(!builtins.is_empty());
        assert!(builtins.iter().any(|f| f.name == "claude-code"));
        assert!(builtins.iter().any(|f| f.name == "codex"));
    }

    #[test]
    fn builtin_lookup_by_name() {
        let cc = AgentFrameworkManifest::builtin("claude-code").unwrap();
        assert_eq!(cc.command, "claude");
        assert!(cc.builtin);
    }

    #[test]
    fn unknown_builtin_returns_none() {
        assert!(AgentFrameworkManifest::builtin("nonexistent-agent").is_none());
    }

    #[test]
    fn discover_empty_dir() {
        let dir = tempdir().unwrap();
        let manifests = AgentFrameworkManifest::discover(dir.path());
        assert!(manifests.is_empty());
    }

    #[test]
    fn discover_reads_toml_manifest() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join(".ta/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let manifest_toml = r#"
name = "my-custom-agent"
version = "1.0.0"
description = "A custom test agent"
command = "my-agent-bin"
args = ["--headless"]
"#;
        std::fs::write(agents_dir.join("my-custom-agent.toml"), manifest_toml).unwrap();
        let discovered = AgentFrameworkManifest::discover(dir.path());
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].name, "my-custom-agent");
        assert!(!discovered[0].builtin);
    }

    #[test]
    fn resolve_builtin_found() {
        let dir = tempdir().unwrap();
        let manifest = AgentFrameworkManifest::resolve("claude-code", dir.path());
        assert!(manifest.is_some());
        assert_eq!(manifest.unwrap().name, "claude-code");
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let dir = tempdir().unwrap();
        let manifest = AgentFrameworkManifest::resolve("no-such-agent", dir.path());
        assert!(manifest.is_none());
    }
}
