// framework_registry.rs — Agent framework registry (v0.7.7).
//
// Provides a registry of known agent frameworks with detection,
// installation metadata, and config generation. The registry is
// loaded from `frameworks.toml` with a bundled default, overridable
// at `~/.config/ta/frameworks.toml` or `.ta/frameworks.toml`.

use std::collections::BTreeMap;
use std::path::Path;

/// A single framework entry in the registry.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct FrameworkEntry {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub install: Option<InstallInstructions>,
    #[serde(default)]
    pub detect: Vec<String>,
    #[serde(default)]
    pub agent_config: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub community: bool,
}

/// Install instructions — either a single string or per-platform.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum InstallInstructions {
    Simple(String),
    PerPlatform {
        #[serde(default)]
        macos: Option<String>,
        #[serde(default)]
        linux: Option<String>,
        #[serde(default)]
        windows: Option<String>,
    },
}

impl InstallInstructions {
    /// Get the install command for the current platform.
    pub fn for_current_platform(&self) -> &str {
        match self {
            Self::Simple(cmd) => cmd,
            Self::PerPlatform {
                macos,
                linux,
                windows,
            } => {
                if cfg!(target_os = "macos") {
                    macos.as_deref().unwrap_or("See framework homepage")
                } else if cfg!(target_os = "windows") {
                    windows.as_deref().unwrap_or("See framework homepage")
                } else {
                    linux.as_deref().unwrap_or("See framework homepage")
                }
            }
        }
    }
}

/// The full framework registry: `[frameworks.<id>]` entries.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct FrameworkRegistry {
    pub frameworks: BTreeMap<String, FrameworkEntry>,
}

impl FrameworkRegistry {
    /// Load the framework registry, checking overrides in priority order:
    /// 1. `.ta/frameworks.toml` (project)
    /// 2. `~/.config/ta/frameworks.toml` (user)
    /// 3. Bundled default
    pub fn load(project_root: Option<&Path>) -> Self {
        // 1. Project override.
        if let Some(root) = project_root {
            let project_path = root.join(".ta").join("frameworks.toml");
            if let Some(reg) = Self::try_load(&project_path) {
                return reg;
            }
        }

        // 2. User override.
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            let user_path = std::path::PathBuf::from(home)
                .join(".config")
                .join("ta")
                .join("frameworks.toml");
            if let Some(reg) = Self::try_load(&user_path) {
                return reg;
            }
        }

        // 3. Bundled default.
        Self::bundled()
    }

    fn try_load(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        toml_parse(&content)
    }

    /// The bundled default registry.
    pub fn bundled() -> Self {
        toml_parse(BUNDLED_REGISTRY).expect("bundled frameworks.toml must parse")
    }

    /// List all framework IDs.
    pub fn ids(&self) -> Vec<&str> {
        self.frameworks.keys().map(|s| s.as_str()).collect()
    }

    /// Get a framework by ID.
    pub fn get(&self, id: &str) -> Option<&FrameworkEntry> {
        self.frameworks.get(id)
    }

    /// Detect which frameworks are installed (commands found on PATH).
    pub fn detect_installed(&self) -> Vec<(&str, &FrameworkEntry)> {
        self.frameworks
            .iter()
            .filter(|(_, entry)| entry.detect.iter().any(|cmd| which_exists(cmd)))
            .map(|(id, entry)| (id.as_str(), entry))
            .collect()
    }

    /// List frameworks NOT detected on PATH.
    pub fn detect_available(&self) -> Vec<(&str, &FrameworkEntry)> {
        self.frameworks
            .iter()
            .filter(|(_, entry)| {
                entry.detect.is_empty() || !entry.detect.iter().any(|cmd| which_exists(cmd))
            })
            .map(|(id, entry)| (id.as_str(), entry))
            .collect()
    }
}

/// Check if a command exists on PATH.
fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Parse TOML content into a FrameworkRegistry.
fn toml_parse(content: &str) -> Option<FrameworkRegistry> {
    // We use serde to parse TOML manually since we only have serde_yaml in deps.
    // Parse the TOML content into our struct.
    // Since we need toml parsing but don't have the toml crate, we'll use a simple
    // custom parser for our well-known format.
    parse_frameworks_toml(content)
}

/// Simple TOML parser for frameworks.toml format.
/// Handles `[frameworks.<id>]` sections with key = value pairs.
fn parse_frameworks_toml(content: &str) -> Option<FrameworkRegistry> {
    let mut frameworks = BTreeMap::new();
    let mut current_id: Option<String> = None;
    let mut current_name = String::new();
    let mut current_desc: Option<String> = None;
    let mut current_homepage: Option<String> = None;
    let mut current_install: Option<InstallInstructions> = None;
    let mut current_detect: Vec<String> = Vec::new();
    let mut current_agent_config: Option<String> = None;
    let mut current_runtime: Option<String> = None;
    let mut current_community = false;

    let flush = |frameworks: &mut BTreeMap<String, FrameworkEntry>,
                 id: &Option<String>,
                 name: &str,
                 desc: &Option<String>,
                 homepage: &Option<String>,
                 install: &Option<InstallInstructions>,
                 detect: &[String],
                 agent_config: &Option<String>,
                 runtime: &Option<String>,
                 community: bool| {
        if let Some(id) = id {
            frameworks.insert(
                id.clone(),
                FrameworkEntry {
                    name: if name.is_empty() {
                        id.clone()
                    } else {
                        name.to_string()
                    },
                    description: desc.clone(),
                    homepage: homepage.clone(),
                    install: install.clone(),
                    detect: detect.to_vec(),
                    agent_config: agent_config.clone(),
                    runtime: runtime.clone(),
                    community,
                },
            );
        }
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check for section header: [frameworks.id]
        if trimmed.starts_with("[frameworks.") && trimmed.ends_with(']') {
            // Flush previous entry.
            flush(
                &mut frameworks,
                &current_id,
                &current_name,
                &current_desc,
                &current_homepage,
                &current_install,
                &current_detect,
                &current_agent_config,
                &current_runtime,
                current_community,
            );

            let id = trimmed
                .strip_prefix("[frameworks.")
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or("")
                .to_string();
            current_id = Some(id);
            current_name = String::new();
            current_desc = None;
            current_homepage = None;
            current_install = None;
            current_detect = Vec::new();
            current_agent_config = None;
            current_runtime = None;
            current_community = false;
            continue;
        }

        // Parse key = value pairs.
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            match key {
                "name" => current_name = unquote(value),
                "description" => current_desc = Some(unquote(value)),
                "homepage" => current_homepage = Some(unquote(value)),
                "install" => {
                    if value.starts_with('"') || value.starts_with('\'') {
                        current_install = Some(InstallInstructions::Simple(unquote(value)));
                    }
                    // Per-platform install is handled by inline table parsing below.
                    if value.starts_with('{') {
                        current_install = parse_install_table(value);
                    }
                }
                "detect" => {
                    current_detect = parse_string_array(value);
                }
                "agent_config" => current_agent_config = Some(unquote(value)),
                "runtime" => current_runtime = Some(unquote(value)),
                "community" => current_community = value.trim() == "true",
                _ => {}
            }
        }
    }

    // Flush last entry.
    flush(
        &mut frameworks,
        &current_id,
        &current_name,
        &current_desc,
        &current_homepage,
        &current_install,
        &current_detect,
        &current_agent_config,
        &current_runtime,
        current_community,
    );

    if frameworks.is_empty() {
        None
    } else {
        Some(FrameworkRegistry { frameworks })
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn parse_string_array(s: &str) -> Vec<String> {
    let s = s.trim();
    if !s.starts_with('[') || !s.ends_with(']') {
        return vec![];
    }
    let inner = &s[1..s.len() - 1];
    inner
        .split(',')
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

fn parse_install_table(s: &str) -> Option<InstallInstructions> {
    let s = s.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    let mut macos = None;
    let mut linux = None;
    let mut windows = None;

    for pair in inner.split(',') {
        if let Some((key, value)) = pair.split_once('=') {
            let key = key.trim();
            let value = unquote(value);
            match key {
                "macos" => macos = Some(value),
                "linux" => linux = Some(value),
                "windows" => windows = Some(value),
                _ => {}
            }
        }
    }

    Some(InstallInstructions::PerPlatform {
        macos,
        linux,
        windows,
    })
}

/// Extract the **Goal**: description line from PLAN.md for a given phase ID.
///
/// Looks for a bold `**Goal**:` line in the phase's content block.
/// Returns None if the phase or description isn't found.
pub fn extract_phase_description(project_root: &Path, phase_id: &str) -> Option<String> {
    let plan_path = project_root.join("PLAN.md");
    let content = std::fs::read_to_string(&plan_path).ok()?;

    // Find the phase heading, then look for **Goal**: in its content.
    let lines: Vec<&str> = content.lines().collect();
    let mut in_target_phase = false;

    for line in &lines {
        let trimmed = line.trim();

        // Check if this is a heading containing our phase ID.
        if trimmed.starts_with('#') {
            if in_target_phase {
                // Hit the next heading — stop searching.
                break;
            }
            // Normalize phase ID comparison (handle v prefix).
            let norm_phase = phase_id.strip_prefix('v').unwrap_or(phase_id);
            let norm_line = trimmed.to_lowercase();
            if norm_line.contains(&format!("v{}", norm_phase))
                || norm_line.contains(&format!(" {} ", norm_phase))
                || norm_line.contains(&format!(" {}", norm_phase))
            {
                in_target_phase = true;
            }
            continue;
        }

        if in_target_phase && trimmed.starts_with("**Goal**:") {
            let desc = trimmed.strip_prefix("**Goal**:").unwrap_or(trimmed).trim();
            if !desc.is_empty() {
                return Some(desc.to_string());
            }
        }
    }

    None
}

/// Bundled frameworks.toml content.
const BUNDLED_REGISTRY: &str = r#"
[frameworks.claude-code]
name = "Claude Code"
description = "Anthropic's Claude Code CLI — interactive coding agent"
homepage = "https://docs.anthropic.com/en/docs/claude-code"
install = "npm install -g @anthropic-ai/claude-code"
detect = ["claude"]
agent_config = "claude-code.yaml"
runtime = "native-cli"

[frameworks.codex]
name = "OpenAI Codex CLI"
homepage = "https://github.com/openai/codex"
install = "npm install -g @openai/codex"
detect = ["codex"]
agent_config = "codex.yaml"
runtime = "native-cli"

[frameworks.ollama]
name = "Ollama"
description = "Local LLM runner — run models locally without cloud API keys"
homepage = "https://ollama.ai"
install = { macos = "brew install ollama", linux = "curl -fsSL https://ollama.ai/install.sh | sh" }
detect = ["ollama"]
agent_config = "ollama.yaml"
runtime = "local-llm"

[frameworks.langchain]
name = "LangChain"
description = "Python framework for LLM application development"
homepage = "https://python.langchain.com"
install = "pip install langchain langchain-cli"
detect = ["langchain"]
agent_config = "langchain.yaml"
runtime = "python"

[frameworks.langgraph]
name = "LangGraph"
description = "LangChain's framework for building stateful multi-agent workflows"
homepage = "https://langchain-ai.github.io/langgraph/"
install = "pip install langgraph langgraph-cli"
detect = ["langgraph"]
agent_config = "langgraph.yaml"
runtime = "python"

[frameworks.bmad]
name = "BMAD-METHOD"
description = "Business/Market-driven AI Development methodology"
homepage = "https://github.com/bmad-code-org/BMAD-METHOD"
install = "See https://github.com/bmad-code-org/BMAD-METHOD#installation"
detect = []
agent_config = "bmad.yaml"
runtime = "methodology"

[frameworks.claude-flow]
name = "Claude Flow"
description = "Multi-agent orchestration with MCP coordination"
homepage = "https://github.com/ruvnet/claude-flow"
install = "npm install -g claude-flow"
detect = ["claude-flow"]
agent_config = "claude-flow.yaml"
runtime = "native-cli"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_registry_parses() {
        let reg = FrameworkRegistry::bundled();
        assert!(reg.frameworks.len() >= 7);
        assert!(reg.get("claude-code").is_some());
        assert!(reg.get("codex").is_some());
        assert!(reg.get("ollama").is_some());
        assert!(reg.get("langchain").is_some());
        assert!(reg.get("langgraph").is_some());
        assert!(reg.get("bmad").is_some());
        assert!(reg.get("claude-flow").is_some());
    }

    #[test]
    fn claude_code_entry_correct() {
        let reg = FrameworkRegistry::bundled();
        let cc = reg.get("claude-code").unwrap();
        assert_eq!(cc.name, "Claude Code");
        assert_eq!(cc.detect, vec!["claude"]);
        assert_eq!(cc.agent_config.as_deref(), Some("claude-code.yaml"));
        assert_eq!(cc.runtime.as_deref(), Some("native-cli"));
    }

    #[test]
    fn ollama_per_platform_install() {
        let reg = FrameworkRegistry::bundled();
        let o = reg.get("ollama").unwrap();
        match &o.install {
            Some(InstallInstructions::PerPlatform { macos, linux, .. }) => {
                assert!(macos.as_deref().unwrap().contains("brew"));
                assert!(linux.as_deref().unwrap().contains("curl"));
            }
            other => panic!("Expected PerPlatform, got {:?}", other),
        }
    }

    #[test]
    fn simple_install_for_current_platform() {
        let inst = InstallInstructions::Simple("npm install -g foo".to_string());
        assert_eq!(inst.for_current_platform(), "npm install -g foo");
    }

    #[test]
    fn parse_string_array_works() {
        assert_eq!(
            parse_string_array(r#"["claude", "codex"]"#),
            vec!["claude", "codex"]
        );
        assert_eq!(parse_string_array("[]"), Vec::<String>::new());
    }

    #[test]
    fn unquote_strips_quotes() {
        assert_eq!(unquote(r#""hello""#), "hello");
        assert_eq!(unquote("'world'"), "world");
        assert_eq!(unquote("bare"), "bare");
    }

    #[test]
    fn bmad_has_empty_detect() {
        let reg = FrameworkRegistry::bundled();
        let b = reg.get("bmad").unwrap();
        assert!(b.detect.is_empty());
    }

    #[test]
    fn ids_returns_all() {
        let reg = FrameworkRegistry::bundled();
        let ids = reg.ids();
        assert!(ids.contains(&"claude-code"));
        assert!(ids.contains(&"ollama"));
    }

    #[test]
    fn extract_phase_description_finds_goal() {
        let dir = tempfile::tempdir().unwrap();
        let plan = r#"# Plan

### v0.7.7 — Agent Framework Registry
<!-- status: pending -->
**Goal**: Make agent frameworks a first-class extensible concept.

### v0.8.0 — Event System
<!-- status: pending -->
**Goal**: Publish stable event types.
"#;
        std::fs::write(dir.path().join("PLAN.md"), plan).unwrap();
        let desc = extract_phase_description(dir.path(), "v0.7.7");
        assert_eq!(
            desc.as_deref(),
            Some("Make agent frameworks a first-class extensible concept.")
        );
    }

    #[test]
    fn extract_phase_description_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let plan = "# Plan\n### v0.1 — Foo\n<!-- status: done -->\n";
        std::fs::write(dir.path().join("PLAN.md"), plan).unwrap();
        assert!(extract_phase_description(dir.path(), "v99.0").is_none());
    }

    #[test]
    fn load_from_project_override() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        let custom = r#"
[frameworks.custom-agent]
name = "Custom Agent"
detect = ["custom-cli"]
agent_config = "custom.yaml"
runtime = "native-cli"
"#;
        std::fs::write(ta_dir.join("frameworks.toml"), custom).unwrap();
        let reg = FrameworkRegistry::load(Some(dir.path()));
        assert!(reg.get("custom-agent").is_some());
        // Should NOT have bundled entries since project override replaces them.
        assert!(reg.get("claude-code").is_none());
    }
}
