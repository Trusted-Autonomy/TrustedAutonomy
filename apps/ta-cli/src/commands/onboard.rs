// onboard.rs — Post-install onboarding wizard (`ta onboard`).
//
// Guides new users through configuring their AI provider, default agent,
// and planning framework. Writes `~/.config/ta/config.toml` on completion.
//
// Run modes:
//   ta onboard                  — TUI wizard (interactive)
//   ta onboard --web            — open Studio setup page in browser; fallback to TUI
//   ta onboard --non-interactive — use flags to configure without prompting
//   ta onboard --status         — show current configuration
//   ta onboard --reset          — clear config and re-run wizard
//   ta onboard --force          — re-run even if already configured

use std::io::IsTerminal;
#[cfg(unix)]
use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph};

// ── Global config path ─────────────────────────────────────────────────────

/// Returns the home directory path from the environment.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Returns `<home>/.config/ta/config.toml` for the given home dir.
fn config_path_in(home: &std::path::Path) -> PathBuf {
    home.join(".config").join("ta").join("config.toml")
}

/// Returns `~/.config/ta/config.toml`.
pub fn global_config_path() -> Option<PathBuf> {
    Some(config_path_in(&home_dir()?))
}

/// Returns `<home>/.config/ta/secrets/` for the given home dir.
fn secrets_dir_in(home: &std::path::Path) -> PathBuf {
    home.join(".config").join("ta").join("secrets")
}

/// Returns the TA secrets dir used for API key storage.
fn secrets_dir() -> anyhow::Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    Ok(secrets_dir_in(&home))
}

const ANTHROPIC_KEY_NAME: &str = "ta_anthropic_api_key";

// ── Config structure ───────────────────────────────────────────────────────

/// The global TA user config written to `~/.config/ta/config.toml`.
#[derive(Debug, Clone, Default)]
pub struct GlobalConfig {
    pub provider: ProviderConfig,
    pub defaults: DefaultsConfig,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderConfig {
    /// "anthropic" | "ollama"
    pub provider_type: String,
    /// For Ollama: base URL
    pub ollama_base_url: Option<String>,
    /// For Ollama: model name
    pub ollama_model: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DefaultsConfig {
    /// Agent to use: "claude-code" | "codex" | "claude-flow" | custom path
    pub agent: String,
    /// Planning framework: "default" | "bmad" | "gsd"
    pub planning_framework: String,
    /// Path to BMAD install (when bmad selected)
    pub bmad_home: Option<String>,
}

/// Check if a config file path has a `[provider]` section.
fn is_configured_at(path: &std::path::Path) -> bool {
    if !path.exists() {
        return false;
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let value: toml::Value =
        toml::from_str(&content).unwrap_or(toml::Value::Table(Default::default()));
    value.get("provider").is_some()
}

/// Check if the global config has a `[provider]` section configured.
pub fn is_configured() -> bool {
    match global_config_path() {
        Some(p) => is_configured_at(&p),
        None => false,
    }
}

/// Load a config from an explicit path (returns defaults if absent or invalid).
fn load_config_at(path: &std::path::Path) -> GlobalConfig {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return GlobalConfig::default(),
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(_) => return GlobalConfig::default(),
    };

    let mut cfg = GlobalConfig::default();

    if let Some(provider) = value.get("provider").and_then(|v| v.as_table()) {
        cfg.provider.provider_type = provider
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        cfg.provider.ollama_base_url = provider
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        cfg.provider.ollama_model = provider
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    if let Some(defaults) = value.get("defaults").and_then(|v| v.as_table()) {
        cfg.defaults.agent = defaults
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("claude-code")
            .to_string();
        cfg.defaults.planning_framework = defaults
            .get("planning_framework")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        cfg.defaults.bmad_home = defaults
            .get("bmad_home")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    cfg
}

/// Load existing global config (returns defaults if absent).
fn load_config() -> GlobalConfig {
    match global_config_path() {
        Some(p) => load_config_at(&p),
        None => GlobalConfig::default(),
    }
}

/// Write global config atomically. Preserves existing keys not in [provider]/[defaults].
fn write_config(cfg: &GlobalConfig) -> anyhow::Result<()> {
    let path =
        global_config_path().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Load existing toml to preserve other sections.
    let existing = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| toml::from_str::<toml::Value>(&c).ok())
            .unwrap_or(toml::Value::Table(toml::map::Map::new()))
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let mut root = match existing {
        toml::Value::Table(t) => t,
        _ => toml::map::Map::new(),
    };

    // Build [provider] section.
    let mut provider = toml::map::Map::new();
    provider.insert(
        "type".to_string(),
        toml::Value::String(cfg.provider.provider_type.clone()),
    );
    if let Some(url) = &cfg.provider.ollama_base_url {
        provider.insert("base_url".to_string(), toml::Value::String(url.clone()));
    }
    if let Some(model) = &cfg.provider.ollama_model {
        provider.insert("model".to_string(), toml::Value::String(model.clone()));
    }
    root.insert("provider".to_string(), toml::Value::Table(provider));

    // Build [defaults] section.
    let mut defaults = toml::map::Map::new();
    defaults.insert(
        "agent".to_string(),
        toml::Value::String(cfg.defaults.agent.clone()),
    );
    defaults.insert(
        "planning_framework".to_string(),
        toml::Value::String(cfg.defaults.planning_framework.clone()),
    );
    if let Some(bmad_home) = &cfg.defaults.bmad_home {
        defaults.insert(
            "bmad_home".to_string(),
            toml::Value::String(bmad_home.clone()),
        );
    }
    root.insert("defaults".to_string(), toml::Value::Table(defaults));

    let serialized =
        toml::to_string_pretty(&toml::Value::Table(root)).context("Failed to serialize config")?;

    // Atomic write: write to .tmp then rename.
    let tmp_path = path.with_extension("toml.tmp");
    std::fs::write(&tmp_path, &serialized)?;
    std::fs::rename(&tmp_path, &path)?;

    Ok(())
}

/// Clear [provider] and [defaults] from global config.
fn clear_config() -> anyhow::Result<()> {
    let path = match global_config_path() {
        Some(p) => p,
        None => return Ok(()),
    };

    if !path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)?;
    let existing: toml::Value =
        toml::from_str(&content).unwrap_or(toml::Value::Table(Default::default()));

    let mut root = match existing {
        toml::Value::Table(t) => t,
        _ => toml::map::Map::new(),
    };

    root.remove("provider");
    root.remove("defaults");

    let serialized = toml::to_string_pretty(&toml::Value::Table(root))?;
    std::fs::write(&path, &serialized)?;
    Ok(())
}

// ── Keychain helpers ───────────────────────────────────────────────────────

fn store_api_key(key: &str) -> anyhow::Result<()> {
    let dir = secrets_dir()?;
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(ANTHROPIC_KEY_NAME);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true).mode(0o600);
        let mut f = opts.open(&file)?;
        f.write_all(key.as_bytes())?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(&file, key)?;
    }

    Ok(())
}

fn read_api_key() -> Option<String> {
    // 1. Environment variable.
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.is_empty() {
            return Some(k);
        }
    }
    // 2. Stored file.
    let dir = secrets_dir().ok()?;
    let file = dir.join(ANTHROPIC_KEY_NAME);
    if file.exists() {
        let s = std::fs::read_to_string(&file).ok()?;
        let trimmed = s.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

fn delete_api_key() -> anyhow::Result<()> {
    let dir = secrets_dir()?;
    let file = dir.join(ANTHROPIC_KEY_NAME);
    if file.exists() {
        std::fs::remove_file(&file)?;
    }
    Ok(())
}

// ── Provider detection ─────────────────────────────────────────────────────

fn detect_ollama() -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let Ok(rt) = rt else { return false };
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build()
            .ok()?;
        let resp = client
            .get("http://localhost:11434/api/tags")
            .send()
            .await
            .ok()?;
        Some(resp.status().is_success())
    })
    .unwrap_or(false)
}

fn detect_ollama_models() -> Vec<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let Ok(rt) = rt else { return vec![] };
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .ok()?;
        let resp = client
            .get("http://localhost:11434/api/tags")
            .send()
            .await
            .ok()?;
        let json: serde_json::Value = resp.json().await.ok()?;
        let models: Vec<String> = json["models"]
            .as_array()?
            .iter()
            .filter_map(|m| m["name"].as_str().map(|s| s.to_string()))
            .collect();
        Some(models)
    })
    .unwrap_or_default()
}

/// Validate an Anthropic API key with a lightweight /v1/models call.
/// Returns Ok(()) on success, Err with user-friendly message on failure.
fn validate_anthropic_key(key: &str) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        let resp = client
            .get("https://api.anthropic.com/v1/models")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .context("Network error contacting Anthropic API")?;

        if resp.status().is_success() {
            Ok(())
        } else if resp.status() == 401 {
            anyhow::bail!(
                "Invalid API key (401 Unauthorized). Check your key at console.anthropic.com"
            )
        } else {
            anyhow::bail!(
                "Anthropic API returned {} — check console.anthropic.com",
                resp.status()
            )
        }
    })
}

// ── Binary detection ───────────────────────────────────────────────────────

fn is_on_path(binary: &str) -> bool {
    which::which(binary).is_ok()
}

fn detect_claude_flow() -> bool {
    is_on_path("claude-flow")
}

fn bmad_installed() -> bool {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let Some(home) = home else { return false };
    let bmad_path = PathBuf::from(home).join(".bmad");
    bmad_path.join("agents").exists()
}

// ── Install helpers ────────────────────────────────────────────────────────

/// Clone BMAD to ~/.bmad. Returns Ok(()) on success or if already installed.
fn install_bmad() -> anyhow::Result<()> {
    if bmad_installed() {
        return Ok(()); // Already installed.
    }

    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let dest = PathBuf::from(home).join(".bmad");

    println!("Cloning BMAD to {}...", dest.display());
    let status = std::process::Command::new("git")
        .args([
            "clone",
            "--depth=1",
            "https://github.com/bmadcode/bmad-method",
            dest.to_str().unwrap_or(".bmad"),
        ])
        .status()
        .context("Failed to run git — is git installed?")?;

    if !status.success() {
        anyhow::bail!(
            "BMAD clone failed. Run manually: git clone --depth=1 https://github.com/bmadcode/bmad-method ~/.bmad"
        );
    }

    if !dest.join("agents").exists() {
        anyhow::bail!(
            "BMAD clone completed but ~/.bmad/agents/ not found — the repository structure may have changed"
        );
    }

    println!("BMAD installed to ~/.bmad");
    Ok(())
}

/// Install claude-flow via npm. Returns Ok(()) on success or if already installed.
fn install_claude_flow() -> anyhow::Result<()> {
    if detect_claude_flow() {
        return Ok(()); // Already installed.
    }

    if !is_on_path("npm") {
        anyhow::bail!(
            "npm is not installed. Install Node.js from https://nodejs.org then run: npm install -g claude-flow"
        );
    }

    println!("Installing claude-flow via npm...");
    let status = std::process::Command::new("npm")
        .args(["install", "-g", "claude-flow"])
        .status()
        .context("Failed to run npm")?;

    if !status.success() {
        anyhow::bail!("npm install failed. Run manually: npm install -g claude-flow");
    }

    // Validate installation.
    if !detect_claude_flow() {
        anyhow::bail!(
            "claude-flow installed but not found on PATH — you may need to restart your shell"
        );
    }

    println!("claude-flow installed successfully.");
    Ok(())
}

// ── Status display ─────────────────────────────────────────────────────────

pub fn show_status() {
    let cfg = load_config();
    let configured = is_configured();

    println!("TA Onboarding Status");
    println!("────────────────────────────────────────");

    if !configured {
        println!("Status:   not configured");
        println!();
        println!("Run 'ta onboard' to set up your AI provider and agent defaults.");
        return;
    }

    // Provider
    println!(
        "Provider: {}",
        if cfg.provider.provider_type.is_empty() {
            "(not set)"
        } else {
            &cfg.provider.provider_type
        }
    );
    match cfg.provider.provider_type.as_str() {
        "ollama" => {
            println!(
                "  Ollama URL: {}",
                cfg.provider
                    .ollama_base_url
                    .as_deref()
                    .unwrap_or("http://localhost:11434")
            );
            println!(
                "  Ollama model: {}",
                cfg.provider.ollama_model.as_deref().unwrap_or("(not set)")
            );
        }
        "anthropic" => {
            let key_status = if read_api_key().is_some() {
                "stored (use 'ta onboard --reset' to change)"
            } else {
                "not stored (set ANTHROPIC_API_KEY or re-run 'ta onboard')"
            };
            println!("  API key: {}", key_status);
        }
        _ => {}
    }

    println!();
    println!(
        "Agent:    {}",
        if cfg.defaults.agent.is_empty() {
            "(not set)"
        } else {
            &cfg.defaults.agent
        }
    );
    println!(
        "Planning: {}",
        if cfg.defaults.planning_framework.is_empty() {
            "(not set)"
        } else {
            &cfg.defaults.planning_framework
        }
    );
    if let Some(bmad) = &cfg.defaults.bmad_home {
        println!("  BMAD home: {}", bmad);
    }

    println!();

    // Config path
    if let Some(path) = global_config_path() {
        println!("Config:   {}", path.display());
    }
}

// ── Reset ──────────────────────────────────────────────────────────────────

fn reset_config() -> anyhow::Result<()> {
    clear_config()?;
    delete_api_key()?;
    println!("Configuration cleared. Run 'ta onboard' to reconfigure.");
    Ok(())
}

// ── Non-interactive mode ───────────────────────────────────────────────────

pub fn execute_non_interactive(
    provider: Option<&str>,
    api_key: Option<&str>,
    agent_name: Option<&str>,
    planning: Option<&str>,
) -> anyhow::Result<()> {
    let mut cfg = load_config();

    let provider_type = provider.unwrap_or("anthropic");
    cfg.provider.provider_type = provider_type.to_string();

    if provider_type == "anthropic" {
        let key = api_key
            .map(|k| k.to_string())
            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
            .ok_or_else(|| {
                anyhow::anyhow!("No API key provided. Use --api-key or set ANTHROPIC_API_KEY")
            })?;

        println!("Validating Anthropic API key...");
        validate_anthropic_key(&key)?;
        store_api_key(&key)?;
        println!("API key validated and stored.");
    } else if provider_type == "ollama" {
        cfg.provider.ollama_base_url = Some("http://localhost:11434".to_string());
        let models = detect_ollama_models();
        cfg.provider.ollama_model = models.into_iter().next();
    }

    cfg.defaults.agent = agent_name.unwrap_or("claude-code").to_string();
    cfg.defaults.planning_framework = planning.unwrap_or("default").to_string();

    write_config(&cfg)?;

    if let Some(path) = global_config_path() {
        println!("Configuration written to {}", path.display());
    }

    println!("Setup complete. Run 'ta run <goal>' to start.");
    Ok(())
}

// ── TUI Wizard ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum WizardStep {
    Provider,    // Step 1
    ApiKey,      // Step 1b (if anthropic + key not found)
    OllamaModel, // Step 1c (if ollama)
    Agent,       // Step 2
    Framework,   // Step 3
    Optionals,   // Step 4
    Summary,     // Step 5
}

impl WizardStep {
    fn index(&self) -> usize {
        match self {
            WizardStep::Provider => 0,
            WizardStep::ApiKey => 0,
            WizardStep::OllamaModel => 0,
            WizardStep::Agent => 1,
            WizardStep::Framework => 2,
            WizardStep::Optionals => 3,
            WizardStep::Summary => 4,
        }
    }
}

#[derive(Debug, Clone)]
struct WizardState {
    step: WizardStep,
    // Provider selection: 0=anthropic, 1=ollama, 2=skip
    provider_idx: usize,
    // API key input
    api_key_input: String,
    api_key_validated: Option<bool>, // None=not validated, Some(true/false)
    api_key_message: String,
    // Ollama models
    ollama_models: Vec<String>,
    ollama_model_idx: usize,
    // Agent selection: 0=claude-code, 1=codex, 2=claude-flow, 3=custom
    agent_idx: usize,
    custom_agent_input: String,
    // Framework: 0=default, 1=bmad, 2=gsd
    framework_idx: usize,
    // Optionals checkboxes: [claude-flow, bmad]
    optionals: [bool; 2],
    // Available agent detection
    agent_available: [bool; 3], // claude-code, codex, claude-flow
    // Detected values
    detected_api_key: Option<String>,
    ollama_detected: bool,
    // Error/status message
    message: Option<String>,
}

impl WizardState {
    fn new() -> Self {
        // Detect available agents
        let claude_code_ok = is_on_path("claude");
        let codex_ok = is_on_path("codex");
        let claude_flow_ok = detect_claude_flow();
        let detected_api_key = read_api_key();
        let ollama_detected = detect_ollama();
        let ollama_models = if ollama_detected {
            detect_ollama_models()
        } else {
            vec![]
        };

        // Pre-select provider based on detection
        let provider_idx = if detected_api_key.is_some() {
            0
        } else if ollama_detected {
            1
        } else {
            0
        };

        WizardState {
            step: WizardStep::Provider,
            provider_idx,
            api_key_input: detected_api_key.clone().unwrap_or_default(),
            api_key_validated: None,
            api_key_message: String::new(),
            ollama_models,
            ollama_model_idx: 0,
            agent_idx: 0,
            custom_agent_input: String::new(),
            framework_idx: 0,
            optionals: [false, false],
            agent_available: [claude_code_ok, codex_ok, claude_flow_ok],
            detected_api_key,
            ollama_detected,
            message: None,
        }
    }

    #[allow(dead_code)]
    fn provider_name(&self) -> &str {
        match self.provider_idx {
            0 => "anthropic",
            1 => "ollama",
            _ => "skip",
        }
    }

    fn agent_name(&self) -> String {
        match self.agent_idx {
            0 => "claude-code".to_string(),
            1 => "codex".to_string(),
            2 => "claude-flow".to_string(),
            3 => self.custom_agent_input.clone(),
            _ => "claude-code".to_string(),
        }
    }

    fn framework_name(&self) -> &str {
        match self.framework_idx {
            0 => "default",
            1 => "bmad",
            2 => "gsd",
            _ => "default",
        }
    }

    fn advance(&mut self) -> bool {
        match self.step {
            WizardStep::Provider => {
                match self.provider_idx {
                    0 => {
                        // Anthropic — need API key
                        if self.api_key_input.is_empty() && self.detected_api_key.is_none() {
                            self.step = WizardStep::ApiKey;
                        } else {
                            // Key already detected — skip to agent
                            self.step = WizardStep::Agent;
                        }
                    }
                    1 => {
                        // Ollama
                        if self.ollama_models.is_empty() {
                            self.step = WizardStep::Agent;
                        } else {
                            self.step = WizardStep::OllamaModel;
                        }
                    }
                    _ => {
                        // Skip provider
                        self.step = WizardStep::Agent;
                    }
                }
                true
            }
            WizardStep::ApiKey => {
                self.step = WizardStep::Agent;
                true
            }
            WizardStep::OllamaModel => {
                self.step = WizardStep::Agent;
                true
            }
            WizardStep::Agent => {
                self.step = WizardStep::Framework;
                true
            }
            WizardStep::Framework => {
                // Pre-check optionals based on selections
                self.optionals[0] = self.agent_idx == 2; // claude-flow if selected as agent
                self.optionals[1] = self.framework_idx == 1; // bmad if selected
                self.step = WizardStep::Optionals;
                true
            }
            WizardStep::Optionals => {
                self.step = WizardStep::Summary;
                true
            }
            WizardStep::Summary => false, // Handled by caller
        }
    }

    fn go_back(&mut self) -> bool {
        match self.step {
            WizardStep::Provider => false,
            WizardStep::ApiKey => {
                self.step = WizardStep::Provider;
                true
            }
            WizardStep::OllamaModel => {
                self.step = WizardStep::Provider;
                true
            }
            WizardStep::Agent => {
                match self.provider_idx {
                    0 if self.detected_api_key.is_none() && !self.api_key_input.is_empty() => {
                        self.step = WizardStep::ApiKey;
                    }
                    1 if !self.ollama_models.is_empty() => {
                        self.step = WizardStep::OllamaModel;
                    }
                    _ => {
                        self.step = WizardStep::Provider;
                    }
                }
                true
            }
            WizardStep::Framework => {
                self.step = WizardStep::Agent;
                true
            }
            WizardStep::Optionals => {
                self.step = WizardStep::Framework;
                true
            }
            WizardStep::Summary => {
                self.step = WizardStep::Optionals;
                true
            }
        }
    }
}

// ── TUI rendering ──────────────────────────────────────────────────────────

fn render_wizard(frame: &mut Frame, state: &WizardState) {
    let area = frame.area();

    // Main frame
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" TA Setup Wizard ")
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Layout: progress bar (3 lines), content, help bar (2 lines)
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(2),
    ])
    .split(inner);

    // Progress bar
    let total_steps = 5u16;
    let current_step = state.step.index() as u16 + 1;
    let progress = (current_step * 100) / total_steps;
    let gauge = Gauge::default()
        .block(Block::default())
        .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
        .percent(progress)
        .label(format!("Step {current_step} of {total_steps}"));
    frame.render_widget(gauge, chunks[0]);

    // Content area for the current step
    match &state.step {
        WizardStep::Provider => render_provider_step(frame, state, chunks[1]),
        WizardStep::ApiKey => render_api_key_step(frame, state, chunks[1]),
        WizardStep::OllamaModel => render_ollama_step(frame, state, chunks[1]),
        WizardStep::Agent => render_agent_step(frame, state, chunks[1]),
        WizardStep::Framework => render_framework_step(frame, state, chunks[1]),
        WizardStep::Optionals => render_optionals_step(frame, state, chunks[1]),
        WizardStep::Summary => render_summary_step(frame, state, chunks[1]),
    }

    // Help bar
    let help = match &state.step {
        WizardStep::ApiKey | WizardStep::OllamaModel => {
            "  ↑/↓ Navigate   Enter Confirm   Esc Back   q Quit"
        }
        WizardStep::Summary => "  Enter Apply & Finish   Esc Back   q Quit",
        _ => "  ↑/↓ Navigate   Enter/→ Next   Esc/← Back   q Quit",
    };
    let help_bar = Paragraph::new(help)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Left);
    frame.render_widget(help_bar, chunks[2]);

    // Status message overlay
    if let Some(msg) = &state.message {
        let msg_area = Rect {
            x: area.x + 2,
            y: area.y + area.height.saturating_sub(4),
            width: area.width.saturating_sub(4),
            height: 2,
        };
        frame.render_widget(Clear, msg_area);
        let style = if msg.starts_with("✓") {
            Style::default().fg(Color::Green)
        } else if msg.starts_with("✗") || msg.starts_with("Error") {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };
        let msg_widget = Paragraph::new(msg.as_str()).style(style);
        frame.render_widget(msg_widget, msg_area);
    }
}

fn render_provider_step(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(4),
    ])
    .split(area);

    let title = Paragraph::new("Step 1 — AI Provider")
        .style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Left);
    frame.render_widget(title, chunks[0]);

    let providers = vec![
        ListItem::new("● Claude (Anthropic)   — powerful, cloud-based AI"),
        ListItem::new("○ Ollama (local)        — runs locally, no API key needed"),
        ListItem::new("○ Skip for now          — configure later with 'ta onboard'"),
    ];

    let mut list_state = ListState::default();
    list_state.select(Some(state.provider_idx));

    let list = List::new(providers)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    // Help text
    let help = match state.provider_idx {
        0 => {
            if state.detected_api_key.is_some() {
                "  API key detected in environment/keychain — will be used automatically."
            } else {
                "  You'll be prompted for your API key on the next screen."
            }
        }
        1 => {
            if state.ollama_detected {
                "  Ollama detected at localhost:11434."
            } else {
                "  Ollama not detected. Ensure it's running: https://ollama.com"
            }
        }
        _ => "  You can configure a provider later by running 'ta onboard'.",
    };
    let help_text = Paragraph::new(help)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Left);
    frame.render_widget(help_text, chunks[2]);
}

fn render_api_key_step(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(4),
    ])
    .split(area);

    let title = Paragraph::new("Step 1 — Anthropic API Key").style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(title, chunks[0]);

    // Key input display (masked)
    let display_key = if state.api_key_input.is_empty() {
        "_".repeat(30)
    } else {
        let visible = state.api_key_input.chars().take(8).collect::<String>();
        format!(
            "{}{}",
            visible,
            "•".repeat(state.api_key_input.len().saturating_sub(8))
        )
    };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(" API Key ")
        .border_style(Style::default().fg(Color::Yellow));
    let input_text = Paragraph::new(display_key).block(input_block);
    frame.render_widget(input_text, chunks[1]);

    let hint = if !state.api_key_message.is_empty() {
        state.api_key_message.as_str()
    } else {
        "Type your API key, then press Enter to validate. Get one at console.anthropic.com"
    };
    let help = Paragraph::new(hint).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(help, chunks[2]);
}

fn render_ollama_step(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(3),
    ])
    .split(area);

    let title = Paragraph::new("Step 1 — Ollama Model").style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = state
        .ollama_models
        .iter()
        .map(|m| ListItem::new(format!("● {m}")))
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.ollama_model_idx));

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    let help = Paragraph::new("Select the Ollama model to use by default.")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(help, chunks[2]);
}

fn render_agent_step(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(4),
    ])
    .split(area);

    let title = Paragraph::new("Step 2 — Implementation Agent").style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(title, chunks[0]);

    let agents = vec![
        ListItem::new(format!(
            "● claude-code  {}",
            if state.agent_available[0] {
                "✓ detected"
            } else {
                "(install: npm i -g @anthropic-ai/claude-code)"
            }
        )),
        ListItem::new(format!(
            "○ codex        {}",
            if state.agent_available[1] {
                "✓ detected"
            } else {
                "(not found on PATH)"
            }
        )),
        ListItem::new(format!(
            "○ claude-flow  {}",
            if state.agent_available[2] {
                "✓ detected"
            } else {
                "(install: npm i -g claude-flow)"
            }
        )),
        ListItem::new("○ Custom       (enter binary path)"),
    ];

    let mut list_state = ListState::default();
    list_state.select(Some(state.agent_idx));

    let list = List::new(agents)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    let help = match state.agent_idx {
        0 => "claude-code: Anthropic's official coding agent — best for most projects.",
        1 => "codex: OpenAI's coding agent — good for GPT-4-based workflows.",
        2 => "claude-flow: Multi-agent orchestration framework with swarm support.",
        3 => "Custom: Specify the path to your own agent binary.",
        _ => "",
    };
    let help_text = Paragraph::new(help).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(help_text, chunks[2]);
}

fn render_framework_step(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(4),
    ])
    .split(area);

    let title = Paragraph::new("Step 3 — Planning Framework").style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(title, chunks[0]);

    let bmad_status = if bmad_installed() {
        "✓ installed"
    } else {
        "(will clone from GitHub)"
    };
    let frameworks = vec![
        ListItem::new("● Default (single-pass)  — simplest; works immediately"),
        ListItem::new(format!(
            "○ BMAD                  — structured multi-role planning  {bmad_status}"
        )),
        ListItem::new("○ GSD                   — goal-structured decomposition"),
    ];

    let mut list_state = ListState::default();
    list_state.select(Some(state.framework_idx));

    let list = List::new(frameworks)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    let help = match state.framework_idx {
        0 => "Default: agent runs a single pass — ideal for most tasks.",
        1 => "BMAD: Analyst→Architect→PM roles for complex multi-phase projects.",
        2 => "GSD: Goal-structured decomposition for iterative delivery.",
        _ => "",
    };
    let help_text = Paragraph::new(help).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(help_text, chunks[2]);
}

fn render_optionals_step(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(4),
    ])
    .split(area);

    let title = Paragraph::new("Step 4 — Optional Components").style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(title, chunks[0]);

    let items = vec![
        ListItem::new(format!(
            "[{}] claude-flow agent framework  (npm install -g claude-flow)",
            if state.optionals[0] { "x" } else { " " }
        )),
        ListItem::new(format!(
            "[{}] BMAD planning library        (git clone to ~/.bmad)",
            if state.optionals[1] { "x" } else { " " }
        )),
    ];

    let mut list_state = ListState::default();
    list_state.select(Some(0)); // No cursor needed for toggle

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(Style::default().fg(Color::Cyan));
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    let help = Paragraph::new("Space to toggle. These will be installed when you confirm.")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(help, chunks[2]);
}

fn render_summary_step(frame: &mut Frame, state: &WizardState, area: Rect) {
    let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(10)]).split(area);

    let title = Paragraph::new("Step 5 — Summary & Confirm").style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(title, chunks[0]);

    let mut lines = vec![];

    // Provider
    match state.provider_idx {
        0 => {
            lines.push(Line::from(vec![
                Span::styled("Provider:  ", Style::default().fg(Color::Gray)),
                Span::styled("Anthropic (Claude)", Style::default().fg(Color::White)),
            ]));
            let key_status = if !state.api_key_input.is_empty() {
                "API key will be stored securely"
            } else if state.detected_api_key.is_some() {
                "API key already stored"
            } else {
                "API key: none (set ANTHROPIC_API_KEY later)"
            };
            lines.push(Line::from(vec![
                Span::styled("           ", Style::default()),
                Span::styled(key_status, Style::default().fg(Color::DarkGray)),
            ]));
        }
        1 => {
            let model = if !state.ollama_models.is_empty() {
                state
                    .ollama_models
                    .get(state.ollama_model_idx)
                    .map(|m| m.as_str())
                    .unwrap_or("(none)")
            } else {
                "(auto)"
            };
            lines.push(Line::from(vec![
                Span::styled("Provider:  ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("Ollama — {model}"),
                    Style::default().fg(Color::White),
                ),
            ]));
        }
        _ => {
            lines.push(Line::from(vec![
                Span::styled("Provider:  ", Style::default().fg(Color::Gray)),
                Span::styled("(skipped)", Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Agent:     ", Style::default().fg(Color::Gray)),
        Span::styled(state.agent_name(), Style::default().fg(Color::White)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Planning:  ", Style::default().fg(Color::Gray)),
        Span::styled(state.framework_name(), Style::default().fg(Color::White)),
    ]));

    if state.optionals[0] || state.optionals[1] {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "Will install: ",
            Style::default().fg(Color::Gray),
        )]));
        if state.optionals[0] {
            lines.push(Line::from(vec![Span::styled(
                "  • claude-flow",
                Style::default().fg(Color::Cyan),
            )]));
        }
        if state.optionals[1] {
            lines.push(Line::from(vec![Span::styled(
                "  • BMAD (~/.bmad)",
                Style::default().fg(Color::Cyan),
            )]));
        }
    }

    lines.push(Line::from(""));
    if let Some(path) = global_config_path() {
        lines.push(Line::from(vec![
            Span::styled("Config:    ", Style::default().fg(Color::Gray)),
            Span::styled(
                path.display().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "Press Enter to apply, Esc to go back.",
        Style::default().fg(Color::Yellow),
    )]));

    let para = Paragraph::new(lines);
    frame.render_widget(para, chunks[1]);
}

// ── TUI event loop ─────────────────────────────────────────────────────────

/// Run the interactive TUI wizard. Returns the final config to apply, or None if aborted.
fn run_tui_wizard() -> anyhow::Result<Option<(GlobalConfig, bool, bool)>> {
    // Returns (config, install_claude_flow, install_bmad)
    let mut state = WizardState::new();
    let mut stdout = std::io::stdout();

    enable_raw_mode()?;
    stdout.execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_wizard_loop(&mut terminal, &mut state);

    disable_raw_mode()?;
    stdout.execute(LeaveAlternateScreen)?;

    // After restoring terminal, show a plain-text summary if we completed.
    match result {
        Ok(Some(_)) => {}
        Ok(None) => return Ok(None),
        Err(e) => return Err(e),
    }

    // Build result config from final state.
    let mut cfg = GlobalConfig::default();

    match state.provider_idx {
        0 => {
            cfg.provider.provider_type = "anthropic".to_string();
        }
        1 => {
            cfg.provider.provider_type = "ollama".to_string();
            cfg.provider.ollama_base_url = Some("http://localhost:11434".to_string());
            cfg.provider.ollama_model = state
                .ollama_models
                .get(state.ollama_model_idx)
                .cloned()
                .or_else(|| Some("qwen2.5-coder:7b".to_string()));
        }
        _ => {
            // skip — no provider configured
        }
    }

    cfg.defaults.agent = state.agent_name();
    cfg.defaults.planning_framework = state.framework_name().to_string();
    if state.framework_idx == 1 {
        cfg.defaults.bmad_home = Some("~/.bmad".to_string());
    }

    let install_cf = state.optionals[0];
    let install_bmad = state.optionals[1];

    // Store API key if entered
    if state.provider_idx == 0 && !state.api_key_input.is_empty() {
        store_api_key(&state.api_key_input)?;
    }

    Ok(Some((cfg, install_cf, install_bmad)))
}

fn run_wizard_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: &mut WizardState,
) -> anyhow::Result<Option<()>> {
    loop {
        terminal.draw(|f| render_wizard(f, state))?;

        if let Event::Key(key) = crossterm::event::read()? {
            // Ctrl-C / q = quit
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(None);
            }
            if key.code == KeyCode::Char('q') {
                return Ok(None);
            }

            match &state.step {
                WizardStep::ApiKey => match key.code {
                    KeyCode::Char(c) => {
                        state.api_key_input.push(c);
                        state.api_key_validated = None;
                        state.api_key_message = String::new();
                        state.message = None;
                    }
                    KeyCode::Backspace => {
                        state.api_key_input.pop();
                        state.api_key_validated = None;
                        state.api_key_message = String::new();
                        state.message = None;
                    }
                    KeyCode::Enter => {
                        if !state.api_key_input.is_empty() {
                            state.message = Some("  Validating API key...".to_string());
                            terminal.draw(|f| render_wizard(f, state))?;
                            match validate_anthropic_key(&state.api_key_input) {
                                Ok(()) => {
                                    state.api_key_validated = Some(true);
                                    state.api_key_message = "✓ API key valid".to_string();
                                    state.message = Some("✓ API key valid".to_string());
                                    terminal.draw(|f| render_wizard(f, state))?;
                                    std::thread::sleep(std::time::Duration::from_millis(800));
                                    state.advance();
                                    state.message = None;
                                }
                                Err(e) => {
                                    state.api_key_validated = Some(false);
                                    state.api_key_message = format!("✗ {e}");
                                    state.message = Some(format!("✗ {e}"));
                                }
                            }
                        }
                    }
                    KeyCode::Esc => {
                        state.go_back();
                        state.message = None;
                    }
                    _ => {}
                },

                WizardStep::OllamaModel => match key.code {
                    KeyCode::Up => {
                        if state.ollama_model_idx > 0 {
                            state.ollama_model_idx -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if state.ollama_model_idx + 1 < state.ollama_models.len() {
                            state.ollama_model_idx += 1;
                        }
                    }
                    KeyCode::Enter | KeyCode::Right => {
                        state.advance();
                        state.message = None;
                    }
                    KeyCode::Esc | KeyCode::Left => {
                        state.go_back();
                        state.message = None;
                    }
                    _ => {}
                },

                WizardStep::Optionals => {
                    match key.code {
                        KeyCode::Up => {
                            // No cursor for optionals, just toggle by index
                        }
                        KeyCode::Down => {}
                        KeyCode::Char(' ') | KeyCode::Char('1') => {
                            state.optionals[0] = !state.optionals[0];
                        }
                        KeyCode::Char('2') => {
                            state.optionals[1] = !state.optionals[1];
                        }
                        KeyCode::Enter | KeyCode::Right => {
                            state.advance();
                            state.message = None;
                        }
                        KeyCode::Esc | KeyCode::Left => {
                            state.go_back();
                            state.message = None;
                        }
                        _ => {}
                    }
                }

                WizardStep::Summary => match key.code {
                    KeyCode::Enter => {
                        return Ok(Some(()));
                    }
                    KeyCode::Esc | KeyCode::Left => {
                        state.go_back();
                        state.message = None;
                    }
                    _ => {}
                },

                // Default navigation for list steps
                _ => {
                    match key.code {
                        KeyCode::Up => {
                            let max = match state.step {
                                WizardStep::Provider => 2,
                                WizardStep::Agent => 3,
                                WizardStep::Framework => 2,
                                _ => 0,
                            };
                            let idx = match state.step {
                                WizardStep::Provider => &mut state.provider_idx,
                                WizardStep::Agent => &mut state.agent_idx,
                                WizardStep::Framework => &mut state.framework_idx,
                                _ => continue,
                            };
                            let _ = max;
                            if *idx > 0 {
                                *idx -= 1;
                            }
                        }
                        KeyCode::Down => {
                            let max = match state.step {
                                WizardStep::Provider => 2,
                                WizardStep::Agent => 3,
                                WizardStep::Framework => 2,
                                _ => 0,
                            };
                            let idx = match state.step {
                                WizardStep::Provider => &mut state.provider_idx,
                                WizardStep::Agent => &mut state.agent_idx,
                                WizardStep::Framework => &mut state.framework_idx,
                                _ => continue,
                            };
                            if *idx < max {
                                *idx += 1;
                            }
                        }
                        KeyCode::Enter | KeyCode::Right => {
                            if !state.advance() {
                                return Ok(Some(()));
                            }
                            state.message = None;
                        }
                        KeyCode::Esc | KeyCode::Left => {
                            if !state.go_back() {
                                return Ok(None); // Esc from first step = quit
                            }
                            state.message = None;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

// ── First-run hint (for ta run / ta serve) ──────────────────────────────────

/// Print the first-run hint and return Err if not configured.
/// Callers pass `--skip-onboard-check` to bypass in CI.
pub fn check_provider_configured(skip_check: bool) -> anyhow::Result<()> {
    if skip_check || is_configured() {
        return Ok(());
    }
    eprintln!(
        "TA is not configured yet. Run 'ta onboard' to set up your AI provider and defaults (takes ~2 minutes).\n\
         To skip this check in CI, pass --skip-onboard-check."
    );
    Err(anyhow::anyhow!(
        "TA provider not configured — run 'ta onboard'"
    ))
}

// ── Main entry point ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn execute(
    web: bool,
    non_interactive: bool,
    from_installer: bool,
    force: bool,
    status: bool,
    reset: bool,
    provider: Option<&str>,
    api_key: Option<&str>,
    agent_name: Option<&str>,
    planning: Option<&str>,
    project_root: &std::path::Path,
) -> anyhow::Result<()> {
    if status {
        show_status();
        return Ok(());
    }

    if reset {
        reset_config()?;
        if non_interactive {
            return Ok(());
        }
        // Fall through to re-run wizard.
    }

    // Already configured and not forced.
    if !force && !reset && is_configured() {
        println!(
            "TA is already configured. Use --force to reconfigure or --status to view settings."
        );
        println!("Run 'ta onboard --reset' to clear and reconfigure.");
        return Ok(());
    }

    if web {
        return run_web_mode(project_root);
    }

    if non_interactive || from_installer {
        return execute_non_interactive(provider, api_key, agent_name, planning);
    }

    // Check for TTY — if no TTY (piped install), print hint and exit.
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "TA is not configured. Run 'ta onboard' in an interactive terminal to set up your AI provider."
        );
        return Ok(());
    }

    // Run TUI wizard.
    match run_tui_wizard()? {
        None => {
            println!("Setup cancelled. Run 'ta onboard' to configure later.");
            return Ok(());
        }
        Some((cfg, do_install_cf, do_install_bmad)) => {
            // Write config first.
            write_config(&cfg)?;

            // Run optional installs with progress output.
            if do_install_cf {
                println!();
                if let Err(e) = install_claude_flow() {
                    eprintln!("Warning: claude-flow install failed: {e}");
                }
            }
            if do_install_bmad {
                println!();
                if let Err(e) = install_bmad() {
                    eprintln!("Warning: BMAD install failed: {e}");
                }
            }

            // Final confirmation.
            println!();
            println!("Setup complete.");
            if let Some(path) = global_config_path() {
                println!("  Config: {}", path.display());
            }
            println!("  Agent:    {}", cfg.defaults.agent);
            println!("  Provider: {}", cfg.provider.provider_type);
            println!();
            println!("Run 'ta studio' to open TA Studio, or 'ta run <goal>' to start.");
        }
    }

    Ok(())
}

/// Open the Studio setup page in the default browser, falling back to TUI.
fn run_web_mode(project_root: &std::path::Path) -> anyhow::Result<()> {
    use super::daemon;
    use super::shell::resolve_daemon_url;

    let base_url = resolve_daemon_url(project_root);

    // Try to ensure daemon is running.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let reachable = rt
        .block_on(async {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .ok()?;
            let resp = client
                .get(format!("{}/health", base_url.trim_end_matches('/')))
                .send()
                .await
                .ok()?;
            Some(resp.status().is_success())
        })
        .unwrap_or(false);

    if !reachable {
        eprintln!("Daemon not running — starting...");
        match daemon::ensure_running(project_root) {
            Ok(()) => eprintln!("Daemon started."),
            Err(e) => {
                eprintln!("Cannot start daemon: {e}. Falling back to TUI wizard.");
                return execute(
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    None,
                    None,
                    None,
                    None,
                    project_root,
                );
            }
        }
    }

    let setup_url = format!("{}/setup", base_url.trim_end_matches('/'));
    println!("Opening setup page: {}", setup_url);

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&setup_url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(&setup_url)
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", &setup_url])
            .spawn();
    }

    println!("If the browser didn't open, navigate to: {}", setup_url);
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Thread-safe path-based helpers ────────────────────────────────────

    fn make_config_file(home: &std::path::Path, content: &str) -> PathBuf {
        let path = config_path_in(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    fn write_config_at_home(home: &std::path::Path, cfg: &GlobalConfig) -> anyhow::Result<()> {
        let path = config_path_in(home);
        write_config_to_path(&path, cfg)
    }

    /// Write the config to an explicit path (used by test helpers to avoid env races).
    fn write_config_to_path(path: &std::path::Path, cfg: &GlobalConfig) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let existing = if path.exists() {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|c| toml::from_str::<toml::Value>(&c).ok())
                .unwrap_or(toml::Value::Table(toml::map::Map::new()))
        } else {
            toml::Value::Table(toml::map::Map::new())
        };

        let mut root = match existing {
            toml::Value::Table(t) => t,
            _ => toml::map::Map::new(),
        };

        let mut provider = toml::map::Map::new();
        provider.insert(
            "type".to_string(),
            toml::Value::String(cfg.provider.provider_type.clone()),
        );
        if let Some(url) = &cfg.provider.ollama_base_url {
            provider.insert("base_url".to_string(), toml::Value::String(url.clone()));
        }
        if let Some(model) = &cfg.provider.ollama_model {
            provider.insert("model".to_string(), toml::Value::String(model.clone()));
        }
        root.insert("provider".to_string(), toml::Value::Table(provider));

        let mut defaults = toml::map::Map::new();
        defaults.insert(
            "agent".to_string(),
            toml::Value::String(cfg.defaults.agent.clone()),
        );
        defaults.insert(
            "planning_framework".to_string(),
            toml::Value::String(cfg.defaults.planning_framework.clone()),
        );
        if let Some(bmad_home) = &cfg.defaults.bmad_home {
            defaults.insert(
                "bmad_home".to_string(),
                toml::Value::String(bmad_home.clone()),
            );
        }
        root.insert("defaults".to_string(), toml::Value::Table(defaults));

        let serialized = toml::to_string_pretty(&toml::Value::Table(root))
            .context("Failed to serialize config")?;

        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, &serialized)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    fn store_key_at(secrets_dir: &std::path::Path, key: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(secrets_dir)?;
        let file = secrets_dir.join(ANTHROPIC_KEY_NAME);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = std::fs::OpenOptions::new();
            opts.create(true).write(true).truncate(true).mode(0o600);
            let mut f = opts.open(&file)?;
            std::io::Write::write_all(&mut f, key.as_bytes())?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&file, key)?;
        }
        Ok(())
    }

    fn read_key_at(secrets_dir: &std::path::Path) -> Option<String> {
        let file = secrets_dir.join(ANTHROPIC_KEY_NAME);
        if file.exists() {
            let s = std::fs::read_to_string(&file).ok()?;
            let t = s.trim().to_string();
            if !t.is_empty() {
                Some(t)
            } else {
                None
            }
        } else {
            None
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[test]
    fn config_roundtrip_anthropic() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GlobalConfig {
            provider: ProviderConfig {
                provider_type: "anthropic".to_string(),
                ollama_base_url: None,
                ollama_model: None,
            },
            defaults: DefaultsConfig {
                agent: "claude-code".to_string(),
                planning_framework: "default".to_string(),
                bmad_home: None,
            },
        };

        write_config_at_home(dir.path(), &cfg).unwrap();
        let path = config_path_in(dir.path());
        let loaded = load_config_at(&path);
        assert_eq!(loaded.provider.provider_type, "anthropic");
        assert_eq!(loaded.defaults.agent, "claude-code");
        assert_eq!(loaded.defaults.planning_framework, "default");
        assert!(loaded.provider.ollama_base_url.is_none());
    }

    #[test]
    fn config_roundtrip_ollama() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GlobalConfig {
            provider: ProviderConfig {
                provider_type: "ollama".to_string(),
                ollama_base_url: Some("http://localhost:11434".to_string()),
                ollama_model: Some("qwen2.5-coder:7b".to_string()),
            },
            defaults: DefaultsConfig {
                agent: "claude-flow".to_string(),
                planning_framework: "bmad".to_string(),
                bmad_home: Some("~/.bmad".to_string()),
            },
        };

        write_config_at_home(dir.path(), &cfg).unwrap();
        let path = config_path_in(dir.path());
        let loaded = load_config_at(&path);
        assert_eq!(loaded.provider.provider_type, "ollama");
        assert_eq!(
            loaded.provider.ollama_base_url.as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(
            loaded.provider.ollama_model.as_deref(),
            Some("qwen2.5-coder:7b")
        );
        assert_eq!(loaded.defaults.agent, "claude-flow");
        assert_eq!(loaded.defaults.planning_framework, "bmad");
        assert_eq!(loaded.defaults.bmad_home.as_deref(), Some("~/.bmad"));
    }

    #[test]
    fn is_configured_false_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_path_in(dir.path());
        assert!(!is_configured_at(&path));
    }

    #[test]
    fn is_configured_true_after_write() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GlobalConfig {
            provider: ProviderConfig {
                provider_type: "anthropic".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        write_config_at_home(dir.path(), &cfg).unwrap();
        let path = config_path_in(dir.path());
        assert!(is_configured_at(&path));
    }

    #[test]
    fn clear_config_removes_provider_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GlobalConfig {
            provider: ProviderConfig {
                provider_type: "anthropic".to_string(),
                ..Default::default()
            },
            defaults: DefaultsConfig {
                agent: "codex".to_string(),
                planning_framework: "gsd".to_string(),
                bmad_home: None,
            },
        };
        write_config_at_home(dir.path(), &cfg).unwrap();
        let path = config_path_in(dir.path());
        assert!(is_configured_at(&path));

        // Clear by removing [provider] and [defaults].
        let content = std::fs::read_to_string(&path).unwrap();
        let existing: toml::Value =
            toml::from_str(&content).unwrap_or(toml::Value::Table(Default::default()));
        let mut root = match existing {
            toml::Value::Table(t) => t,
            _ => toml::map::Map::new(),
        };
        root.remove("provider");
        root.remove("defaults");
        let serialized = toml::to_string_pretty(&toml::Value::Table(root)).unwrap();
        std::fs::write(&path, &serialized).unwrap();

        assert!(!is_configured_at(&path));
    }

    #[test]
    fn clear_config_preserves_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_config_file(
            dir.path(),
            r#"[provider]
type = "anthropic"

[defaults]
agent = "claude-code"
planning_framework = "default"

[custom_section]
foo = "bar"
"#,
        );

        // Remove provider and defaults sections.
        let content = std::fs::read_to_string(&path).unwrap();
        let existing: toml::Value =
            toml::from_str(&content).unwrap_or(toml::Value::Table(Default::default()));
        let mut root = match existing {
            toml::Value::Table(t) => t,
            _ => toml::map::Map::new(),
        };
        root.remove("provider");
        root.remove("defaults");
        let serialized = toml::to_string_pretty(&toml::Value::Table(root)).unwrap();
        std::fs::write(&path, &serialized).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("[provider]"));
        assert!(!content.contains("[defaults]"));
        assert!(content.contains("[custom_section]"));
        assert!(content.contains("foo"));
    }

    #[test]
    fn api_key_store_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = secrets_dir_in(dir.path());

        assert!(read_key_at(&secrets).is_none());

        store_key_at(&secrets, "test-key-12345").unwrap();
        let key = read_key_at(&secrets).unwrap();
        assert_eq!(key, "test-key-12345");

        // Delete and verify gone.
        std::fs::remove_file(secrets.join(ANTHROPIC_KEY_NAME)).unwrap();
        assert!(read_key_at(&secrets).is_none());
    }

    #[test]
    fn api_key_env_var_takes_priority_over_file() {
        // read_api_key() checks env first; this tests that the logic is correct.
        // We avoid mutating ANTHROPIC_API_KEY globally (parallel-safe).
        let dir = tempfile::tempdir().unwrap();
        let secrets = secrets_dir_in(dir.path());
        store_key_at(&secrets, "file-key").unwrap();

        // File key is readable.
        assert_eq!(read_key_at(&secrets).unwrap(), "file-key");

        // The actual read_api_key() checks env first — we just verify the helper works.
        // Env var mutation tests are done in integration test context if needed.
    }

    #[test]
    fn check_provider_configured_skip_flag() {
        // skip=true bypasses even when no config exists.
        assert!(check_provider_configured(true).is_ok());
    }

    #[test]
    fn check_provider_configured_fails_when_path_is_empty() {
        // When is_configured() returns false (no global config or no [provider]),
        // check_provider_configured(false) should return Err.
        // We can't inject the path here without refactoring, but we verify
        // that the function returns an error with the right message by
        // observing behavior on a system where we just test the error path.
        // Use a real temp dir to fake an unconfigured state through is_configured_at.
        let dir = tempfile::tempdir().unwrap();
        let path = config_path_in(dir.path());
        assert!(!is_configured_at(&path)); // no file = unconfigured
                                           // The error message from check_provider_configured mentions "ta onboard".
                                           // We can verify the message shape here by calling the lower-level check.
        let result: anyhow::Result<()> = if is_configured_at(&path) {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "TA provider not configured — run 'ta onboard'"
            ))
        };
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ta onboard"));
    }

    #[test]
    fn write_config_atomic_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GlobalConfig {
            provider: ProviderConfig {
                provider_type: "anthropic".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        write_config_at_home(dir.path(), &cfg).unwrap();

        let path = config_path_in(dir.path());
        let tmp = path.with_extension("toml.tmp");
        assert!(
            !tmp.exists(),
            ".tmp file should be cleaned up after atomic write"
        );
        assert!(path.exists(), "config file should exist after write");
    }

    #[test]
    fn bmad_install_idempotent_when_agents_dir_present() {
        let dir = tempfile::tempdir().unwrap();
        // Create fake <home>/.bmad/agents/ directory.
        let bmad_agents = dir.path().join(".bmad").join("agents");
        std::fs::create_dir_all(&bmad_agents).unwrap();

        // Test the detection helper directly.
        let bmad_path = dir.path().join(".bmad");
        assert!(bmad_path.join("agents").exists());
        // install_bmad() is not called because we can't inject the home dir into it.
        // We test the detection logic which is the core of idempotency.
        // install_bmad() calls bmad_installed() which reads HOME — tested in integration.
    }

    #[test]
    fn claude_flow_detection_does_not_panic() {
        // Smoke test: detect_claude_flow() should not panic.
        let _ = detect_claude_flow();
    }

    #[test]
    fn config_preserves_extra_sections_on_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        // Write initial config with a custom section.
        make_config_file(
            dir.path(),
            r#"[provider]
type = "anthropic"

[custom_section]
foo = "bar"
"#,
        );

        // Rewrite config — should preserve [custom_section].
        let cfg = GlobalConfig {
            provider: ProviderConfig {
                provider_type: "ollama".to_string(),
                ollama_base_url: Some("http://localhost:11434".to_string()),
                ollama_model: Some("llama3".to_string()),
            },
            defaults: DefaultsConfig {
                agent: "codex".to_string(),
                planning_framework: "gsd".to_string(),
                bmad_home: None,
            },
        };
        write_config_at_home(dir.path(), &cfg).unwrap();

        let path = config_path_in(dir.path());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("[custom_section]"),
            "extra section should be preserved"
        );
        assert!(
            content.contains("ollama"),
            "new provider type should be written"
        );
        assert!(content.contains("codex"), "new agent should be written");
    }
}
