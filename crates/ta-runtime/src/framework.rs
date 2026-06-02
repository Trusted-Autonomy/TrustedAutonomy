// framework.rs — Agent framework manifest, resolution, and dispatch for v0.13.8.
//
// An AgentFramework defines how TA launches an agent backend.
// Built-in frameworks ship with TA; custom frameworks are TOML manifests
// discovered from well-known paths.
//
// ## Architecture
//
// ```text
// ta run --agent qwen-coder
//         │
//         ▼
// AgentFrameworkManifest::resolve("qwen-coder", project_root)
//         │
//         ▼
// framework_to_command() → (command, args, env)
// context_injector()     → inject goal context before launch
// memory_bridge_mode()   → select MCP / context / env / none
// ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auth_spec::{AgentAuthSpec, AuthMethodSpec};

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
    /// Only inject entries with these tags (empty = all entries).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Only inject entries updated within this many days (0 = no filter).
    #[serde(default)]
    pub recency_days: u32,
}

fn default_max_memory_entries() -> usize {
    20
}

/// Context files injected into the agent's context file at goal start (v0.16.3).
///
/// Paths are resolved relative to the project root for `.ta/`-prefixed paths,
/// relative to the home directory for `~/`-prefixed paths, or as absolute paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentContextConfig {
    /// Markdown files whose contents are appended to the agent's CLAUDE.md block.
    #[serde(default)]
    pub files: Vec<String>,
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
    /// Authentication specification for this framework.
    /// Built-ins populate this in code; custom manifests declare `[auth]` in YAML.
    /// Defaults to no-auth-required when absent from a custom manifest.
    #[serde(default)]
    pub auth: AgentAuthSpec,
    /// Agent context channel type for unified injection (v0.15.28).
    /// Selects which AgentContextChannel implementation is used.
    #[serde(default)]
    pub channel_type: crate::channels::ChannelType,
    /// Context files to inject into the agent's context file at goal start (v0.16.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentContextConfig>,
    /// Base manifest to inherit from (v0.16.3). Single-level only — chains are rejected.
    /// Resolved to an absolute path: `~/`-prefixed → home dir, absolute → as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherit: Option<String>,
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
        // Shared auth spec for claude-code and claude-flow (both use Anthropic credentials).
        let claude_auth = AgentAuthSpec {
            required: true,
            methods: vec![
                AuthMethodSpec::EnvVar {
                    name: "ANTHROPIC_API_KEY".to_string(),
                    label: "API key".to_string(),
                    setup_hint: "export ANTHROPIC_API_KEY=sk-ant-...".to_string(),
                    required: true,
                },
                AuthMethodSpec::SessionFile {
                    config_dir_unix: "~/.config/claude/".to_string(),
                    config_dir_windows: String::new(),
                    check_cmd: "claude auth status".to_string(),
                    label: "subscription session".to_string(),
                    setup_hint: "claude auth login".to_string(),
                },
            ],
        };

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
                    ..Default::default()
                },
                builtin: true,
                auth: claude_auth.clone(),
                channel_type: crate::channels::ChannelType::ClaudeCode,
                context: None,
                inherit: None,
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
                    ..Default::default()
                },
                builtin: true,
                auth: AgentAuthSpec {
                    required: true,
                    methods: vec![AuthMethodSpec::EnvVar {
                        name: "OPENAI_API_KEY".to_string(),
                        label: "API key".to_string(),
                        setup_hint: "export OPENAI_API_KEY=sk-...".to_string(),
                        required: true,
                    }],
                },
                channel_type: crate::channels::ChannelType::Codex,
                context: None,
                inherit: None,
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
                    ..Default::default()
                },
                builtin: true,
                // claude-flow delegates to Claude — same auth requirements.
                auth: claude_auth,
                channel_type: crate::channels::ChannelType::ClaudeCode,
                context: None,
                inherit: None,
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
                    ..Default::default()
                },
                builtin: true,
                channel_type: crate::channels::ChannelType::Ollama,
                // Ollama: local service check only; no upstream creds by default.
                // Users who proxy a remote provider add upstream_auth to .ta/agents/ollama.yaml.
                auth: AgentAuthSpec {
                    required: false,
                    methods: vec![AuthMethodSpec::LocalService {
                        url_env_var: "OLLAMA_HOST".to_string(),
                        default_url: "http://localhost:11434".to_string(),
                        health_endpoint: "/api/tags".to_string(),
                        service_auth: vec![AuthMethodSpec::EnvVar {
                            name: "OLLAMA_API_KEY".to_string(),
                            label: "service API key".to_string(),
                            setup_hint:
                                "export OLLAMA_API_KEY=<your-key>  (only if access control is enabled)"
                                    .to_string(),
                            required: false,
                        }],
                        upstream_auth: vec![],
                        required: false,
                    }],
                },
                context: None,
                inherit: None,
            },
        ]
    }

    /// Look up a built-in framework by name.
    pub fn builtin(name: &str) -> Option<AgentFrameworkManifest> {
        Self::builtins().into_iter().find(|f| f.name == name)
    }

    /// Resolve a `~/`- or absolute inherit path to an absolute `PathBuf`.
    fn resolve_inherit_path(path_str: &str) -> PathBuf {
        if let Some(rest) = path_str.strip_prefix("~/") {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            home.join(rest)
        } else {
            PathBuf::from(path_str)
        }
    }

    /// Resolve a context file path relative to `project_root`.
    ///
    /// - `.ta/`-prefixed → `<project_root>/<path>`
    /// - `~/`-prefixed   → `<home>/<rest>`
    /// - absolute        → as-is
    pub fn resolve_context_file_path(path_str: &str, project_root: &Path) -> PathBuf {
        if let Some(rest) = path_str.strip_prefix("~/") {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            home.join(rest)
        } else if Path::new(path_str).is_absolute() {
            PathBuf::from(path_str)
        } else {
            project_root.join(path_str)
        }
    }

    /// Load a manifest from a TOML or YAML file.
    fn load_from_file(path: &Path) -> std::io::Result<AgentFrameworkManifest> {
        let content = std::fs::read_to_string(path)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext == "yaml" || ext == "yml" {
            serde_yaml::from_str::<AgentFrameworkManifest>(&content)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        } else {
            toml::from_str::<AgentFrameworkManifest>(&content)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }
    }

    /// Apply inheritance: load the base manifest (if `inherit` is set) and merge.
    ///
    /// Fields set by the inheriting manifest win. `context.files` are concatenated
    /// (base files first). Returns an error if the base manifest also has `inherit`
    /// (chains are not allowed).
    pub fn with_inheritance_applied(mut self) -> Result<AgentFrameworkManifest, String> {
        let inherit_path_str = match self.inherit.take() {
            Some(p) => p,
            None => return Ok(self),
        };

        let base_path = Self::resolve_inherit_path(&inherit_path_str);
        let mut base = Self::load_from_file(&base_path).map_err(|e| {
            format!(
                "Failed to load base manifest '{}': {}",
                base_path.display(),
                e
            )
        })?;

        if base.inherit.is_some() {
            return Err(format!(
                "Manifest inheritance chains are not allowed. Base manifest '{}' also has `inherit`.",
                base_path.display()
            ));
        }

        // Merge: inheriting manifest fields win; context.files are concatenated base-first.
        if self.version == default_version() && base.version != default_version() {
            self.version = base.version;
        }
        if self.description.is_empty() {
            self.description = base.description;
        }
        if self.sentinel == default_sentinel() && base.sentinel != default_sentinel() {
            self.sentinel = base.sentinel;
        }
        if self.context_file == default_context_file()
            && base.context_file != default_context_file()
        {
            self.context_file = base.context_file;
        }
        if matches!(self.context_inject, ContextInjectMode::Prepend)
            && !matches!(base.context_inject, ContextInjectMode::Prepend)
        {
            self.context_inject = base.context_inject;
        }
        if self.memory.inject == MemoryInjectMode::None
            && base.memory.inject != MemoryInjectMode::None
        {
            self.memory = base.memory.clone();
        }
        if self.auth.methods.is_empty() {
            self.auth = base.auth;
        }

        // Merge context.files: base files first, then project files.
        let base_files = base.context.take().map(|c| c.files).unwrap_or_default();
        let project_files = self.context.take().map(|c| c.files).unwrap_or_default();
        if !base_files.is_empty() || !project_files.is_empty() {
            let mut merged = base_files;
            merged.extend(project_files);
            self.context = Some(AgentContextConfig { files: merged });
        }

        Ok(self)
    }

    /// Collect the resolved context file paths for this manifest.
    ///
    /// Missing files are skipped (with a warn log); this is intentional per spec.
    pub fn resolved_context_files(&self, project_root: &Path) -> Vec<(String, PathBuf)> {
        self.context
            .as_ref()
            .map(|c| &c.files)
            .map(|files| {
                files
                    .iter()
                    .map(|f| (f.clone(), Self::resolve_context_file_path(f, project_root)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Extract the model name from `--model <value>` in `args`, if present.
    pub fn extract_model(&self) -> Option<&str> {
        let mut iter = self.args.iter();
        while let Some(arg) = iter.next() {
            if arg == "--model" {
                return iter.next().map(|s| s.as_str());
            }
        }
        None
    }

    /// Discover custom framework manifests from well-known paths.
    ///
    /// Search order:
    /// 1. `.ta/agents/` (project-level)
    /// 2. `~/.config/ta/agents/` (user-level)
    ///
    /// Canonical format is YAML (`.yaml`). TOML (`.toml`) is supported for
    /// backwards compatibility with user-provided project-local manifests.
    /// When both `<name>.yaml` and `<name>.toml` exist, YAML takes precedence.
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
                // Collect all entries first so we can de-duplicate by stem (YAML wins over TOML).
                let mut by_stem: std::collections::HashMap<String, PathBuf> =
                    std::collections::HashMap::new();
                for entry in entries.flatten() {
                    let path = entry.path();
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_string();
                    if ext != "yaml" && ext != "yml" && ext != "toml" {
                        continue;
                    }
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    // YAML takes precedence: only insert TOML if no YAML exists for this stem.
                    if ext == "yaml" || ext == "yml" {
                        by_stem.insert(stem, path);
                    } else {
                        by_stem.entry(stem).or_insert(path);
                    }
                }

                for path in by_stem.values() {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    let result = std::fs::read_to_string(path).and_then(|s| {
                        if ext == "yaml" || ext == "yml" {
                            serde_yaml::from_str::<AgentFrameworkManifest>(&s).map_err(|e| {
                                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
                            })
                        } else {
                            toml::from_str::<AgentFrameworkManifest>(&s).map_err(|e| {
                                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
                            })
                        }
                    });
                    match result {
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

// ── AgentFramework trait (v0.13.8 item 2) ──────────────────────────────────
//
// Trait abstraction over agent backends. Each framework backend implements
// this to provide polymorphic dispatch. The default implementation is
// `ManifestBackedFramework` which reads an `AgentFrameworkManifest` TOML.

/// Core abstraction over an agent backend.
///
/// Implement this to provide a new agent backend. The default implementation
/// is `ManifestBackedFramework` which reads from an `AgentFrameworkManifest`.
pub trait AgentFramework: Send + Sync {
    /// Unique name of this framework (e.g., "claude-code", "codex").
    fn name(&self) -> &str;
    /// Return the underlying manifest.
    fn manifest(&self) -> &AgentFrameworkManifest;
    /// Build the (command, args) to use when spawning the agent.
    /// Returns the command binary and a list of arguments to prepend before the prompt.
    fn build_command(&self) -> (&str, &[String]) {
        let m = self.manifest();
        (&m.command, &m.args)
    }
    /// How context is injected into this framework.
    fn context_inject_mode(&self) -> &ContextInjectMode {
        &self.manifest().context_inject
    }
    /// Memory configuration for this framework.
    fn memory_config(&self) -> &FrameworkMemoryConfig {
        &self.manifest().memory
    }
}

/// Default framework implementation backed by an `AgentFrameworkManifest`.
#[derive(Debug, Clone)]
pub struct ManifestBackedFramework {
    manifest: AgentFrameworkManifest,
}

impl ManifestBackedFramework {
    pub fn new(manifest: AgentFrameworkManifest) -> Self {
        Self { manifest }
    }
}

impl AgentFramework for ManifestBackedFramework {
    fn name(&self) -> &str {
        &self.manifest.name
    }
    fn manifest(&self) -> &AgentFrameworkManifest {
        &self.manifest
    }
}

// ── ContextInjector (v0.13.8 item 8) ───────────────────────────────────────
//
// Handles the various modes for injecting goal context before agent launch.
//
// Prepend: backup + prepend to context_file (existing behaviour for Claude Code).
// Env:     write context to a temp file; return TA_GOAL_CONTEXT env var.
// Arg:     write context to a temp file; return (flag, path) to prepend as args.
// None:    no injection.

/// Result of env/arg-mode context injection.
pub struct ContextInjectionResult {
    /// Environment variables to add to the agent process.
    pub env_vars: HashMap<String, String>,
    /// Extra args to prepend before the prompt (flag + path for Arg mode).
    pub extra_args: Vec<String>,
    /// Path to the temp context file, if one was written (must be kept alive
    /// until agent exits — caller is responsible for cleanup).
    pub context_file: Option<PathBuf>,
}

/// Inject context in Env mode: write to `.ta/goal_context.md` in staging dir
/// and return the `TA_GOAL_CONTEXT` env var pointing to it.
pub fn inject_context_env(
    staging_dir: &Path,
    context: &str,
) -> std::io::Result<ContextInjectionResult> {
    let ta_dir = staging_dir.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;
    let ctx_path = ta_dir.join("goal_context.md");
    std::fs::write(&ctx_path, context)?;
    let mut env_vars = HashMap::new();
    env_vars.insert(
        "TA_GOAL_CONTEXT".to_string(),
        ctx_path.display().to_string(),
    );
    Ok(ContextInjectionResult {
        env_vars,
        extra_args: Vec::new(),
        context_file: Some(ctx_path),
    })
}

/// Inject context in Arg mode: write to `.ta/goal_context.md` and return
/// `["--context", "<path>"]` to prepend to agent args.
pub fn inject_context_arg(
    staging_dir: &Path,
    context: &str,
    flag: &str,
) -> std::io::Result<ContextInjectionResult> {
    let ta_dir = staging_dir.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;
    let ctx_path = ta_dir.join("goal_context.md");
    std::fs::write(&ctx_path, context)?;
    let extra_args = vec![flag.to_string(), ctx_path.display().to_string()];
    Ok(ContextInjectionResult {
        env_vars: HashMap::new(),
        extra_args,
        context_file: Some(ctx_path),
    })
}

/// Build a map of `agent_name → [workflow_names]` by scanning `.ta/workflows/*.toml` (v0.16.3).
///
/// Looks for `agent = "<name>"` in any TOML section. Returns an empty map when
/// the workflows directory does not exist.
pub fn build_workflow_agent_index(project_root: &Path) -> HashMap<String, Vec<String>> {
    let workflows_dir = project_root.join(".ta").join("workflows");
    let mut index: HashMap<String, Vec<String>> = HashMap::new();

    if !workflows_dir.is_dir() {
        return index;
    }

    if let Ok(entries) = std::fs::read_dir(&workflows_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "toml" {
                continue;
            }
            let wf_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(value) = toml::from_str::<toml::Value>(&content) {
                    collect_agent_refs(&value, &wf_name, &mut index);
                }
            }
        }
    }

    index
}

/// Recursively collect `agent = "<name>"` values from a TOML value tree.
fn collect_agent_refs(
    value: &toml::Value,
    workflow_name: &str,
    index: &mut HashMap<String, Vec<String>>,
) {
    match value {
        toml::Value::Table(map) => {
            if let Some(toml::Value::String(agent)) = map.get("agent") {
                if !agent.is_empty() && agent != "builtin" {
                    index
                        .entry(agent.clone())
                        .or_default()
                        .push(workflow_name.to_string());
                }
            }
            for v in map.values() {
                collect_agent_refs(v, workflow_name, index);
            }
        }
        toml::Value::Array(arr) => {
            for v in arr {
                collect_agent_refs(v, workflow_name, index);
            }
        }
        _ => {}
    }
}

/// Set the TA_MEMORY_OUT path in the agent's environment.
/// The agent writes new memory entries to this file on exit; TA ingests them.
pub fn inject_memory_out_env(staging_dir: &Path) -> (String, String) {
    let out_path = staging_dir.join(".ta").join("memory_out.json");
    ("TA_MEMORY_OUT".to_string(), out_path.display().to_string())
}

/// Build TA_MEMORY_PATH env var: path to a snapshot file TA writes before launch.
pub fn inject_memory_snapshot_env(staging_dir: &Path) -> (String, String) {
    let snap_path = staging_dir.join(".ta").join("memory_snapshot.md");
    (
        "TA_MEMORY_PATH".to_string(),
        snap_path.display().to_string(),
    )
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
    fn discover_reads_yaml_manifest() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join(".ta/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let manifest_yaml = r#"
name: my-yaml-agent
version: "1.0.0"
description: "A custom YAML test agent"
command: my-yaml-agent-bin
args:
  - "--headless"
"#;
        std::fs::write(agents_dir.join("my-yaml-agent.yaml"), manifest_yaml).unwrap();
        let discovered = AgentFrameworkManifest::discover(dir.path());
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].name, "my-yaml-agent");
        assert_eq!(discovered[0].command, "my-yaml-agent-bin");
        assert!(!discovered[0].builtin);
    }

    #[test]
    fn discover_yaml_takes_precedence_over_toml() {
        // When both <name>.yaml and <name>.toml exist, YAML wins.
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join(".ta/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let yaml = "name: priority-agent\ncommand: from-yaml\n";
        let toml = "name = \"priority-agent\"\ncommand = \"from-toml\"\n";
        std::fs::write(agents_dir.join("priority-agent.yaml"), yaml).unwrap();
        std::fs::write(agents_dir.join("priority-agent.toml"), toml).unwrap();
        let discovered = AgentFrameworkManifest::discover(dir.path());
        // Should discover exactly one manifest (YAML wins).
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].command, "from-yaml");
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

    #[test]
    fn resolve_yaml_custom_manifest() {
        // A YAML manifest in .ta/agents/ should be discoverable via resolve().
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join(".ta/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let yaml = "name: custom-yaml-fw\ncommand: custom-bin\n";
        std::fs::write(agents_dir.join("custom-yaml-fw.yaml"), yaml).unwrap();
        let manifest = AgentFrameworkManifest::resolve("custom-yaml-fw", dir.path());
        assert!(manifest.is_some());
        assert_eq!(manifest.unwrap().command, "custom-bin");
    }

    #[test]
    fn ollama_builtin_manifest_yaml_roundtrips() {
        let ollama = AgentFrameworkManifest::builtin("ollama").unwrap();
        let yaml = serde_yaml::to_string(&ollama).unwrap();
        let back: AgentFrameworkManifest = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.name, "ollama");
        // Ollama auth is optional by design.
        assert!(!back.auth.required);
        // service_auth carries the OLLAMA_API_KEY optional env var.
        let local_svc = back.auth.methods.first().expect("should have a method");
        if let crate::auth_spec::AuthMethodSpec::LocalService {
            service_auth,
            upstream_auth,
            required,
            ..
        } = local_svc
        {
            assert!(!required, "ollama LocalService should be required=false");
            assert_eq!(service_auth.len(), 1);
            assert!(
                upstream_auth.is_empty(),
                "built-in ollama has no upstream_auth"
            );
        } else {
            panic!("expected LocalService method in ollama manifest");
        }
    }

    #[test]
    fn custom_manifest_with_upstream_auth_roundtrips() {
        let yaml = r#"
name: custom-ollama
command: ta-agent-ollama
auth:
  required: false
  methods:
    - type: local_service
      default_url: "http://localhost:11434"
      health_endpoint: "/api/tags"
      service_auth:
        - type: env_var
          name: OLLAMA_API_KEY
          required: false
      upstream_auth:
        - type: env_var
          name: OPENAI_API_KEY
          required: true
"#;
        let manifest: AgentFrameworkManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(manifest.name, "custom-ollama");
        assert!(!manifest.auth.required);
        let method = manifest.auth.methods.first().expect("should have a method");
        if let crate::auth_spec::AuthMethodSpec::LocalService {
            service_auth,
            upstream_auth,
            ..
        } = method
        {
            assert_eq!(service_auth.len(), 1);
            assert_eq!(upstream_auth.len(), 1);
            if let crate::auth_spec::AuthMethodSpec::EnvVar { name, required, .. } =
                upstream_auth.first().unwrap()
            {
                assert_eq!(name, "OPENAI_API_KEY");
                assert!(required);
            } else {
                panic!("expected EnvVar in upstream_auth");
            }
        } else {
            panic!("expected LocalService method");
        }
    }

    // ── v0.16.3 context.files and inherit tests ─────────────────────────────

    #[test]
    fn context_files_project_relative_resolved() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let path = ".ta/constitutions/style.md";
        let resolved = AgentFrameworkManifest::resolve_context_file_path(path, project_root);
        assert_eq!(resolved, project_root.join(".ta/constitutions/style.md"));
    }

    #[test]
    fn context_files_absolute_resolved() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let abs = "/tmp/shared-style.md";
        let resolved = AgentFrameworkManifest::resolve_context_file_path(abs, project_root);
        assert_eq!(resolved, std::path::PathBuf::from("/tmp/shared-style.md"));
    }

    #[test]
    fn resolved_context_files_empty_when_no_context() {
        let dir = tempdir().unwrap();
        let manifest = AgentFrameworkManifest {
            name: "test".to_string(),
            command: "cmd".to_string(),
            context: None,
            ..AgentFrameworkManifest::builtin("claude-code").unwrap()
        };
        let files = manifest.resolved_context_files(dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn resolved_context_files_returns_project_relative_paths() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let manifest = AgentFrameworkManifest {
            name: "test".to_string(),
            command: "cmd".to_string(),
            context: Some(AgentContextConfig {
                files: vec![
                    ".ta/constitutions/style.md".to_string(),
                    "/tmp/absolute.md".to_string(),
                ],
            }),
            ..AgentFrameworkManifest::builtin("claude-code").unwrap()
        };
        let files = manifest.resolved_context_files(project_root);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].1, project_root.join(".ta/constitutions/style.md"));
        assert_eq!(files[1].1, std::path::PathBuf::from("/tmp/absolute.md"));
    }

    #[test]
    fn inherit_merges_context_files_base_first() {
        let dir = tempdir().unwrap();

        // Write base manifest.
        let base_toml = r#"
name = "base-coder"
command = "claude"
[context]
files = ["~/.config/ta/base-style.md"]
"#;
        let base_path = dir.path().join("base-coder.toml");
        std::fs::write(&base_path, base_toml).unwrap();

        // Inheriting manifest.
        let manifest = AgentFrameworkManifest {
            name: "my-coder".to_string(),
            command: "claude".to_string(),
            inherit: Some(base_path.display().to_string()),
            context: Some(AgentContextConfig {
                files: vec![".ta/constitutions/project-style.md".to_string()],
            }),
            ..AgentFrameworkManifest::builtin("claude-code").unwrap()
        };

        let merged = manifest.with_inheritance_applied().unwrap();
        let files = &merged.context.as_ref().unwrap().files;
        assert_eq!(files.len(), 2, "should have base + project file");
        assert_eq!(files[0], "~/.config/ta/base-style.md", "base file first");
        assert_eq!(
            files[1], ".ta/constitutions/project-style.md",
            "project file second"
        );
    }

    #[test]
    fn inherit_chain_rejected() {
        let dir = tempdir().unwrap();

        // Base manifest that also has inherit — should be rejected.
        let base_toml = r#"
name = "base"
command = "claude"
inherit = "/some/other-base.toml"
"#;
        let base_path = dir.path().join("base.toml");
        std::fs::write(&base_path, base_toml).unwrap();

        let manifest = AgentFrameworkManifest {
            name: "child".to_string(),
            command: "claude".to_string(),
            inherit: Some(base_path.display().to_string()),
            context: None,
            ..AgentFrameworkManifest::builtin("claude-code").unwrap()
        };

        let result = manifest.with_inheritance_applied();
        assert!(result.is_err(), "chain should be rejected");
        assert!(result.unwrap_err().contains("chains are not allowed"));
    }

    #[test]
    fn inherit_missing_base_returns_error() {
        let manifest = AgentFrameworkManifest {
            name: "my-coder".to_string(),
            command: "claude".to_string(),
            inherit: Some("/nonexistent/base.toml".to_string()),
            context: None,
            ..AgentFrameworkManifest::builtin("claude-code").unwrap()
        };

        let result = manifest.with_inheritance_applied();
        assert!(result.is_err(), "missing base should return error");
    }

    #[test]
    fn extract_model_from_args() {
        let manifest = AgentFrameworkManifest {
            name: "qwen".to_string(),
            command: "ta-agent-ollama".to_string(),
            args: vec![
                "--model".to_string(),
                "qwen3.5:9b".to_string(),
                "--base-url".to_string(),
                "http://localhost:11434".to_string(),
            ],
            ..AgentFrameworkManifest::builtin("claude-code").unwrap()
        };
        assert_eq!(manifest.extract_model(), Some("qwen3.5:9b"));
    }

    #[test]
    fn extract_model_absent_when_no_flag() {
        let manifest = AgentFrameworkManifest::builtin("claude-code").unwrap();
        assert_eq!(manifest.extract_model(), None);
    }

    #[test]
    fn builtin_manifests_all_have_auth() {
        for m in AgentFrameworkManifest::builtins() {
            // All built-in manifests should have at least one auth method OR be non-required.
            // claude-code, codex, claude-flow require auth; ollama does not.
            if m.name == "ollama" {
                assert!(!m.auth.required, "ollama auth should be optional");
            } else {
                assert!(m.auth.required, "built-in '{}' should require auth", m.name);
                assert!(
                    !m.auth.methods.is_empty(),
                    "built-in '{}' should have at least one auth method",
                    m.name
                );
            }
        }
    }
}
