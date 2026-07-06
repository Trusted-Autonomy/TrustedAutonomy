// persona.rs — `ta persona` subcommands (v0.14.20).
//
// ta persona list          — list all personas in .ta/personas/
// ta persona new <name>    — interactive wizard to create a persona
// ta persona show <name>   — show details of a specific persona

use clap::Subcommand;
use ta_goal::{PersonaCapabilities, PersonaConfig, PersonaInner, PersonaStyle};
use ta_mcp_gateway::GatewayConfig;

use super::team::validate_agent_id;

#[derive(Debug, Subcommand)]
pub enum PersonaCommands {
    /// List all agent personas in .ta/personas/.
    List,
    /// Create a new agent persona interactively.
    New {
        /// Persona name (used as filename: .ta/personas/<name>.toml).
        name: String,
        /// Short description of the persona's role.
        #[arg(long, default_value = "")]
        description: String,
        /// System prompt text (the persona's identity).
        #[arg(long, default_value = "")]
        system_prompt: String,
        /// Comma-separated list of allowed tool names (empty = no restriction).
        #[arg(long, value_delimiter = ',')]
        allowed_tools: Vec<String>,
        /// Comma-separated list of forbidden tool names.
        #[arg(long, value_delimiter = ',')]
        forbidden_tools: Vec<String>,
        /// Persona-level agent binding (v0.17.0.12.13 `Switch` action tiers).
        ///
        /// Any goal run with `--persona <name>` picks up this agent unless
        /// `--agent` overrides it. Pass "auto" to hand the choice to the
        /// supervisor's recommendation instead of a fixed agent ID.
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show details of a specific persona.
    Show {
        /// Persona name.
        name: String,
    },
    /// Set or clear the persona-level agent binding (v0.17.0.12.13).
    ///
    /// Examples:
    ///   ta persona set-agent financial-analyst claude-opus-4-8
    ///   ta persona set-agent financial-analyst auto
    SetAgent {
        /// Persona name.
        name: String,
        /// Agent ID, or "auto" to hand the choice to the supervisor.
        agent: String,
    },
}

pub fn execute(command: &PersonaCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;

    match command {
        PersonaCommands::List => {
            let personas = PersonaConfig::list_all(project_root);
            if personas.is_empty() {
                println!("No personas found in .ta/personas/");
                println!("Create one with: ta persona new <name>");
                return Ok(());
            }
            println!("{:<25} {:<45} TOOLS", "NAME", "DESCRIPTION");
            println!("{}", "-".repeat(80));
            for p in &personas {
                let tools = if p.allowed_tools.is_empty() {
                    "(all tools)".to_string()
                } else {
                    format!("allow: {}", p.allowed_tools.join(", "))
                };
                let desc_truncated = if p.description.len() > 43 {
                    format!("{}...", &p.description[..40])
                } else {
                    p.description.clone()
                };
                println!("{:<25} {:<45} {}", p.name, desc_truncated, tools);
            }
            Ok(())
        }

        PersonaCommands::New {
            name,
            description,
            system_prompt,
            allowed_tools,
            forbidden_tools,
            agent,
        } => {
            // Validate name.
            if name.contains('/') || name.contains('\\') || name.contains('.') {
                anyhow::bail!(
                    "Persona name '{}' is invalid. Use lowercase letters, numbers, and hyphens only.",
                    name
                );
            }
            if let Some(a) = agent {
                validate_agent_id(a)?;
            }

            let description = if description.is_empty() {
                // Prompt interactively if stdin is a TTY.
                if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    eprint!("Description: ");
                    let mut buf = String::new();
                    std::io::stdin().read_line(&mut buf).ok();
                    buf.trim().to_string()
                } else {
                    description.clone()
                }
            } else {
                description.clone()
            };

            let system_prompt = if system_prompt.is_empty() {
                if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    eprintln!("System prompt (press Enter twice to finish):");
                    let mut lines = Vec::new();
                    loop {
                        let mut buf = String::new();
                        std::io::stdin().read_line(&mut buf).ok();
                        let trimmed = buf.trim_end_matches('\n').to_string();
                        if trimmed.is_empty() && !lines.is_empty() {
                            break;
                        }
                        lines.push(trimmed);
                    }
                    lines.join("\n")
                } else {
                    system_prompt.clone()
                }
            } else {
                system_prompt.clone()
            };

            let persona = PersonaConfig {
                persona: PersonaInner {
                    name: name.clone(),
                    description: description.clone(),
                    system_prompt,
                    constitution: None,
                    agent: agent.clone(),
                },
                capabilities: PersonaCapabilities {
                    allowed_tools: allowed_tools.clone(),
                    forbidden_tools: forbidden_tools.clone(),
                },
                style: PersonaStyle {
                    output_format: "markdown".to_string(),
                    max_response_length: String::new(),
                },
            };

            let path = persona.save(project_root)?;
            println!("Persona '{}' saved to {}", name, path.display());
            if let Some(a) = agent {
                println!("Agent binding: {}", a);
            }
            println!("Use it with: ta run \"your goal\" --persona {}", name);
            Ok(())
        }

        PersonaCommands::Show { name } => {
            let persona = PersonaConfig::load(project_root, name)?;
            println!("Persona: {}", persona.persona.name);
            println!("Description: {}", persona.persona.description);
            if !persona.persona.system_prompt.is_empty() {
                println!("\nSystem Prompt:\n{}", persona.persona.system_prompt);
            }
            if !persona.capabilities.allowed_tools.is_empty() {
                println!(
                    "Allowed tools: {}",
                    persona.capabilities.allowed_tools.join(", ")
                );
            }
            if !persona.capabilities.forbidden_tools.is_empty() {
                println!(
                    "Forbidden tools: {}",
                    persona.capabilities.forbidden_tools.join(", ")
                );
            }
            if let Some(a) = &persona.persona.agent {
                println!("Agent binding: {}", a);
            }
            Ok(())
        }

        PersonaCommands::SetAgent { name, agent } => {
            validate_agent_id(agent)?;
            let mut persona = PersonaConfig::load(project_root, name)?;
            persona.persona.agent = Some(agent.clone());
            persona.save(project_root)?;
            println!("Persona '{}' agent binding set to '{}'.", name, agent);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_persona(name: &str) -> PersonaConfig {
        PersonaConfig {
            persona: PersonaInner {
                name: name.to_string(),
                description: "Test persona".to_string(),
                system_prompt: "You are a test persona.".to_string(),
                constitution: None,
                agent: None,
            },
            capabilities: PersonaCapabilities::default(),
            style: PersonaStyle::default(),
        }
    }

    // ── v0.17.0.12.13: `ta persona set-agent` ────────────────────────

    #[test]
    fn set_agent_updates_existing_persona() {
        let dir = tempdir().unwrap();
        sample_persona("reviewer-bot").save(dir.path()).unwrap();

        let mut persona = PersonaConfig::load(dir.path(), "reviewer-bot").unwrap();
        persona.persona.agent = Some("claude-opus-4-8".to_string());
        persona.save(dir.path()).unwrap();

        let loaded = PersonaConfig::load(dir.path(), "reviewer-bot").unwrap();
        assert_eq!(loaded.persona.agent, Some("claude-opus-4-8".to_string()));
    }

    #[test]
    fn set_agent_accepts_auto() {
        assert!(validate_agent_id("auto").is_ok());
    }

    #[test]
    fn set_agent_rejects_invalid_id() {
        assert!(validate_agent_id("").is_err());
        assert!(validate_agent_id("bad id").is_err());
    }
}
