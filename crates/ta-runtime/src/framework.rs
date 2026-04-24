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
