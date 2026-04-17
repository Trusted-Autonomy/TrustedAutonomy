// persona.rs — Agent persona configuration for TA-mediated goals (v0.14.20).
//
// Personas define *who* the agent acts as: system prompt, behavioral rules,
// and tool restrictions. Stored in `.ta/personas/<name>.toml`.
// Applied with `ta run "title" --persona financial-analyst`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Capabilities section of a persona config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonaCapabilities {
    /// Tool names the agent may use. Empty = no restriction.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Tool names the agent may NOT use.
    #[serde(default)]
    pub forbidden_tools: Vec<String>,
}

/// Style/output preferences for a persona.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonaStyle {
    /// Preferred output format (e.g., "markdown", "json", "plain").
    #[serde(default)]
    pub output_format: String,
    /// Suggested max response length (e.g., "2000 words").
    #[serde(default)]
    pub max_response_length: String,
}

/// Inner [persona] table in the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaInner {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    /// Path to an optional constitution file to extend.
    #[serde(default)]
    pub constitution: Option<String>,
}

/// Full persona config loaded from `.ta/personas/<name>.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaConfig {
    pub persona: PersonaInner,
    #[serde(default)]
    pub capabilities: PersonaCapabilities,
    #[serde(default)]
    pub style: PersonaStyle,
}

/// Summary for API/CLI listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaSummary {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub forbidden_tools: Vec<String>,
}

impl PersonaConfig {
    /// Load a persona by name from `.ta/personas/<name>.toml`.
    pub fn load(project_root: &Path, name: &str) -> anyhow::Result<Self> {
        let path = project_root
            .join(".ta")
            .join("personas")
            .join(format!("{}.toml", name));
        let text = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "Could not read persona '{}' at {}: {}",
                name,
                path.display(),
                e
            )
        })?;
        let cfg: PersonaConfig = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("Invalid persona config '{}': {}", name, e))?;
        Ok(cfg)
    }

    /// List all personas in `.ta/personas/`.
    pub fn list_all(project_root: &Path) -> Vec<PersonaSummary> {
        let dir = project_root.join(".ta").join("personas");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut summaries = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(cfg) = toml::from_str::<PersonaConfig>(&text) else {
                continue;
            };
            summaries.push(cfg.to_summary());
        }
        summaries.sort_by_key(|s| s.name.clone());
        summaries
    }

    /// Save this persona to `.ta/personas/<name>.toml`.
    pub fn save(&self, project_root: &Path) -> anyhow::Result<PathBuf> {
        let dir = project_root.join(".ta").join("personas");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.toml", self.persona.name));
        let text = toml::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("Could not serialize persona: {}", e))?;
        std::fs::write(&path, text)?;
        Ok(path)
    }

    /// Generate the CLAUDE.md section injected for this persona.
    pub fn to_claude_md_section(&self) -> String {
        let mut out = String::new();
        out.push_str("\n## Agent Persona\n\n");
        out.push_str(&format!(
            "**Persona:** {} — {}\n\n",
            self.persona.name, self.persona.description
        ));
        if !self.persona.system_prompt.is_empty() {
            out.push_str("### Role\n\n");
            out.push_str(&self.persona.system_prompt);
            out.push('\n');
        }
        if !self.capabilities.allowed_tools.is_empty() {
            out.push_str(&format!(
                "\n**Allowed tools:** {}\n",
                self.capabilities.allowed_tools.join(", ")
            ));
        }
        if !self.capabilities.forbidden_tools.is_empty() {
            out.push_str(&format!(
                "**Forbidden tools:** {}\n",
                self.capabilities.forbidden_tools.join(", ")
            ));
        }
        if !self.style.output_format.is_empty() {
            out.push_str(&format!(
                "**Output format:** {}\n",
                self.style.output_format
            ));
        }
        out
    }

    pub fn to_summary(&self) -> PersonaSummary {
        PersonaSummary {
            name: self.persona.name.clone(),
            description: self.persona.description.clone(),
            allowed_tools: self.capabilities.allowed_tools.clone(),
            forbidden_tools: self.capabilities.forbidden_tools.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_persona() -> PersonaConfig {
        PersonaConfig {
            persona: PersonaInner {
                name: "financial-analyst".to_string(),
                description: "Analyzes financial data".to_string(),
                system_prompt: "You are a financial analyst.".to_string(),
                constitution: None,
            },
            capabilities: PersonaCapabilities {
                allowed_tools: vec!["read".to_string(), "bash".to_string()],
                forbidden_tools: vec!["write".to_string()],
            },
            style: PersonaStyle {
                output_format: "markdown".to_string(),
                max_response_length: "2000 words".to_string(),
            },
        }
    }

    #[test]
    fn persona_save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let persona = sample_persona();
        let path = persona.save(dir.path()).unwrap();
        assert!(path.exists());

        let loaded = PersonaConfig::load(dir.path(), "financial-analyst").unwrap();
        assert_eq!(loaded.persona.name, "financial-analyst");
        assert_eq!(loaded.capabilities.allowed_tools, vec!["read", "bash"]);
    }

    #[test]
    fn persona_list_all_returns_saved_persona() {
        let dir = tempdir().unwrap();
        let persona = sample_persona();
        persona.save(dir.path()).unwrap();

        let list = PersonaConfig::list_all(dir.path());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "financial-analyst");
    }

    #[test]
    fn persona_to_claude_md_section_includes_prompt() {
        let persona = sample_persona();
        let section = persona.to_claude_md_section();
        assert!(section.contains("financial-analyst"));
        assert!(section.contains("You are a financial analyst."));
        assert!(section.contains("Forbidden tools:"));
    }

    #[test]
    fn persona_list_all_empty_if_no_dir() {
        let dir = tempdir().unwrap();
        let list = PersonaConfig::list_all(dir.path());
        assert!(list.is_empty());
    }
}
