// security.rs — Security Level Profiles for Trusted Autonomy (v0.15.14.4).
//
// Defines a tiered security model with three preset levels (Low / Mid / High)
// that each set a named bundle of defaults. Individual settings always override
// the level preset. This gives solo developers a frictionless default, teams
// a sensible hardened baseline, and regulated projects a documented high-assurance
// posture without jumping to the full SA (OCI/gVisor/TPM) ceiling.
//
// Design:
// - Level sets defaults only — every control can be overridden individually.
// - Escalation (setting a control to stricter than the level default) is silent.
// - Demotion (setting a control to weaker than the level default) logs a warning.
// - Constitution / supervisor checks and secret scanning are always on at all levels.
//   What changes per level is the *consequence* (warn vs block vs block+auto-follow-up).

use serde::{Deserialize, Serialize};

// ── Level ──────────────────────────────────────────────────────────────────────

/// Named security preset levels.
///
/// ```toml
/// [security]
/// level = "mid"   # "low" | "mid" | "high"
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SecurityLevel {
    /// Frictionless default for solo developers (today's implicit behavior).
    #[default]
    Low,
    /// Sensible hardened baseline for teams and startups.
    Mid,
    /// High-assurance posture for regulated projects.
    High,
}

impl std::fmt::Display for SecurityLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecurityLevel::Low => write!(f, "low"),
            SecurityLevel::Mid => write!(f, "mid"),
            SecurityLevel::High => write!(f, "high"),
        }
    }
}

impl std::str::FromStr for SecurityLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "low" => Ok(SecurityLevel::Low),
            "mid" => Ok(SecurityLevel::Mid),
            "high" => Ok(SecurityLevel::High),
            other => Err(format!(
                "unknown security level '{}' — expected 'low', 'mid', or 'high'",
                other
            )),
        }
    }
}

// ── ConstitutionBlockMode ──────────────────────────────────────────────────────

/// How constitution (supervisor) violations affect the draft lifecycle.
///
/// At every level the supervisor is always on. The mode controls the consequence
/// when the supervisor returns a `Fail` finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConstitutionBlockMode {
    /// Log the finding but allow the draft to proceed. (Default for `low` and `mid`.)
    #[default]
    Warn,
    /// Fail the draft build and require manual intervention before applying.
    Block,
    /// Block the draft and automatically spawn a `--follow-up` goal so the agent
    /// can correct the violation. (Default for `high`.)
    BlockAndFollowUp,
}

impl std::fmt::Display for ConstitutionBlockMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConstitutionBlockMode::Warn => write!(f, "warn"),
            ConstitutionBlockMode::Block => write!(f, "block"),
            ConstitutionBlockMode::BlockAndFollowUp => write!(f, "block_and_follow_up"),
        }
    }
}

// ── SecretScanMode ─────────────────────────────────────────────────────────────

/// How secret scanning affects draft apply.
///
/// Scanning runs at every level over draft artifact text content.
/// The mode controls what happens when a pattern matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SecretScanMode {
    /// Disable scanning entirely. Must be set explicitly; never a level default.
    Off,
    /// Print findings and continue. (Default for `low` and `mid`.)
    #[default]
    Warn,
    /// Print findings and abort apply. (Default for `high`.)
    Block,
}

impl std::fmt::Display for SecretScanMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretScanMode::Off => write!(f, "off"),
            SecretScanMode::Warn => write!(f, "warn"),
            SecretScanMode::Block => write!(f, "block"),
        }
    }
}

impl std::str::FromStr for SecretScanMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "off" => Ok(SecretScanMode::Off),
            "warn" => Ok(SecretScanMode::Warn),
            "block" => Ok(SecretScanMode::Block),
            other => Err(format!(
                "unknown secret scan mode '{}' — expected 'off', 'warn', or 'block'",
                other
            )),
        }
    }
}

// ── AuditMode ─────────────────────────────────────────────────────────────────

/// Audit trail integrity mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuditMode {
    /// Local JSONL only, no hash chain. (Not used as a level default.)
    Local,
    /// SHA-256 hash chain linking successive audit entries. (Default for `mid`.)
    #[default]
    HashChain,
    /// HMAC-SHA256 signed hash chain with a per-project key. (Default for `high`.)
    SignedHashChain,
}

impl std::fmt::Display for AuditMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditMode::Local => write!(f, "local"),
            AuditMode::HashChain => write!(f, "hash_chain"),
            AuditMode::SignedHashChain => write!(f, "signed_hash_chain"),
        }
    }
}

// ── SecurityProfile ────────────────────────────────────────────────────────────

/// The fully resolved security configuration for a goal run.
///
/// Built by [`SecurityProfile::from_level`] which merges a level preset
/// with any explicit overrides from `workflow.toml`. Every field here is
/// a concrete decision — no `Option<T>` means "apply the level default".
#[derive(Debug, Clone)]
pub struct SecurityProfile {
    /// The source level (retained for display and demotion warnings).
    pub level: SecurityLevel,

    /// Whether process sandboxing is enabled for the agent.
    pub sandbox_enabled: bool,

    /// Additional forbidden tool patterns beyond the built-in list.
    /// For `mid`, populated with `DEFAULT_MID_FORBIDDEN_TOOLS`.
    pub forbidden_tool_patterns: Vec<String>,

    /// Whether `ta draft apply` requires prior `ta draft approve`. Locked `true`
    /// for `high` regardless of `[draft] approval_required` override.
    pub approval_required: bool,

    /// Audit trail integrity mode.
    pub audit_mode: AuditMode,

    /// Constitution / supervisor violation consequence.
    pub constitution_block_mode: ConstitutionBlockMode,

    /// Secret scanning consequence.
    pub secret_scan_mode: SecretScanMode,

    /// Whether WebSearch is permitted. Disabled for `high` by default.
    pub web_search_enabled: bool,
}

/// Sensible forbidden patterns added for `mid` and `high` levels.
/// These prevent the most common dangerous Bash patterns.
pub const DEFAULT_MID_FORBIDDEN_TOOLS: &[&str] = &[
    "Bash(*rm -rf*)",
    "Bash(*sudo *)",
    "Bash(*curl * | bash*)",
    "Bash(*curl * | sh*)",
    "Bash(*wget * -O- * | sh*)",
    "Bash(*wget * -O- * | bash*)",
];

impl SecurityProfile {
    /// Build a `SecurityProfile` from a level and explicit overrides.
    ///
    /// `overrides` is the `[security]` section from `workflow.toml`. Any field
    /// set there wins over the level preset. If an override weakens a `high`-level
    /// control, a warning is emitted via `tracing::warn`.
    pub fn from_level(level: SecurityLevel, overrides: &SecurityOverrides) -> Self {
        // Start with level-preset defaults.
        let (
            mut sandbox_enabled,
            mut approval_required,
            mut audit_mode,
            mut constitution_block_mode,
            mut secret_scan_mode,
            mut web_search_enabled,
        ) = match level {
            SecurityLevel::Low => (
                false,
                false,
                AuditMode::HashChain,
                ConstitutionBlockMode::Warn,
                SecretScanMode::Warn,
                true,
            ),
            SecurityLevel::Mid => (
                true,
                false,
                AuditMode::HashChain,
                ConstitutionBlockMode::Warn,
                SecretScanMode::Warn,
                true,
            ),
            SecurityLevel::High => (
                true,
                true,
                AuditMode::SignedHashChain,
                ConstitutionBlockMode::BlockAndFollowUp,
                SecretScanMode::Block,
                false,
            ),
        };

        // Apply overrides, emitting demotion warnings for high-level controls.
        if let Some(v) = overrides.sandbox_enabled {
            if level == SecurityLevel::High && !v && sandbox_enabled {
                tracing::warn!(
                    "[warn] security.level=high but sandbox.enabled=false — sandbox override active. \
                     High security requires process isolation."
                );
            }
            sandbox_enabled = v;
        }

        if let Some(v) = overrides.approval_required {
            if level == SecurityLevel::High && !v {
                tracing::warn!(
                    "[warn] security.level=high but draft.approval_required=false override active. \
                     High security locks approval_required=true for audit trail integrity."
                );
                // High mode locks approval_required=true regardless of override.
            } else {
                approval_required = v;
            }
        }

        if let Some(ref m) = overrides.audit_mode {
            audit_mode = *m;
        }

        if let Some(ref m) = overrides.constitution_block_mode {
            if level == SecurityLevel::High
                && *m == ConstitutionBlockMode::Warn
                && constitution_block_mode != ConstitutionBlockMode::Warn
            {
                tracing::warn!(
                    "[warn] security.level=high but constitution_block_mode=warn override active. \
                     High security blocks on constitution violations by default."
                );
            }
            constitution_block_mode = *m;
        }

        if let Some(ref m) = overrides.secret_scan_mode {
            if level == SecurityLevel::High && *m == SecretScanMode::Off {
                tracing::warn!(
                    "[warn] security.level=high but security.secrets.scan=off override active. \
                     High security blocks on secret findings by default."
                );
            }
            secret_scan_mode = *m;
        }

        if let Some(v) = overrides.web_search_enabled {
            web_search_enabled = v;
        }

        // Merge forbidden tool patterns: start with level preset, then add extras.
        let mut forbidden_tool_patterns: Vec<String> = match level {
            SecurityLevel::Low => vec![],
            SecurityLevel::Mid | SecurityLevel::High => DEFAULT_MID_FORBIDDEN_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        for extra in &overrides.extra_forbidden_tools {
            if !forbidden_tool_patterns.contains(extra) {
                forbidden_tool_patterns.push(extra.clone());
            }
        }

        Self {
            level,
            sandbox_enabled,
            forbidden_tool_patterns,
            approval_required,
            audit_mode,
            constitution_block_mode,
            secret_scan_mode,
            web_search_enabled,
        }
    }

    /// Returns a one-line badge string for display (e.g., `[mid]`).
    pub fn badge(&self) -> String {
        match self.level {
            SecurityLevel::Low => String::new(),
            SecurityLevel::Mid => "[mid]".to_string(),
            SecurityLevel::High => "[high]".to_string(),
        }
    }
}

// ── SecurityOverrides ─────────────────────────────────────────────────────────

/// Explicit per-project overrides from the `[security]` section of `workflow.toml`.
///
/// All fields are `Option<T>` — absent means "use the level preset".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityOverrides {
    /// Override sandbox enabled/disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_enabled: Option<bool>,

    /// Override approval_required (ignored for `high` level — always locked true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_required: Option<bool>,

    /// Override audit integrity mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_mode: Option<AuditMode>,

    /// Override constitution violation consequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constitution_block_mode: Option<ConstitutionBlockMode>,

    /// Override secret scan consequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_scan_mode: Option<SecretScanMode>,

    /// Override WebSearch availability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_enabled: Option<bool>,

    /// Additional forbidden tool patterns to add on top of the level preset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_forbidden_tools: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_overrides() -> SecurityOverrides {
        SecurityOverrides::default()
    }

    // Item 9a: SecurityProfile::from_level applies correct defaults per level.
    #[test]
    fn low_defaults() {
        let p = SecurityProfile::from_level(SecurityLevel::Low, &no_overrides());
        assert!(!p.sandbox_enabled);
        assert!(!p.approval_required);
        assert_eq!(p.audit_mode, AuditMode::HashChain);
        assert_eq!(p.constitution_block_mode, ConstitutionBlockMode::Warn);
        assert_eq!(p.secret_scan_mode, SecretScanMode::Warn);
        assert!(p.web_search_enabled);
        assert!(p.forbidden_tool_patterns.is_empty());
    }

    #[test]
    fn mid_defaults() {
        let p = SecurityProfile::from_level(SecurityLevel::Mid, &no_overrides());
        assert!(p.sandbox_enabled);
        assert!(!p.approval_required);
        assert_eq!(p.audit_mode, AuditMode::HashChain);
        assert_eq!(p.constitution_block_mode, ConstitutionBlockMode::Warn);
        assert_eq!(p.secret_scan_mode, SecretScanMode::Warn);
        assert!(p.web_search_enabled);
        // Item 9e: mid forbidden tool patterns block rm -rf and sudo
        assert!(p
            .forbidden_tool_patterns
            .contains(&"Bash(*rm -rf*)".to_string()));
        assert!(p
            .forbidden_tool_patterns
            .contains(&"Bash(*sudo *)".to_string()));
    }

    #[test]
    fn high_defaults() {
        let p = SecurityProfile::from_level(SecurityLevel::High, &no_overrides());
        assert!(p.sandbox_enabled);
        assert!(p.approval_required);
        assert_eq!(p.audit_mode, AuditMode::SignedHashChain);
        assert_eq!(
            p.constitution_block_mode,
            ConstitutionBlockMode::BlockAndFollowUp
        );
        assert_eq!(p.secret_scan_mode, SecretScanMode::Block);
        assert!(!p.web_search_enabled);
        assert!(p
            .forbidden_tool_patterns
            .contains(&"Bash(*rm -rf*)".to_string()));
    }

    // Item 9b: override wins over preset.
    #[test]
    fn override_wins_over_preset() {
        let overrides = SecurityOverrides {
            secret_scan_mode: Some(SecretScanMode::Off),
            ..Default::default()
        };
        let p = SecurityProfile::from_level(SecurityLevel::Low, &overrides);
        assert_eq!(p.secret_scan_mode, SecretScanMode::Off);
    }

    #[test]
    fn extra_forbidden_tools_merged() {
        let overrides = SecurityOverrides {
            extra_forbidden_tools: vec!["Bash(*aws*)".to_string()],
            ..Default::default()
        };
        let p = SecurityProfile::from_level(SecurityLevel::Mid, &overrides);
        assert!(p
            .forbidden_tool_patterns
            .contains(&"Bash(*aws*)".to_string()));
        // Mid preset patterns still present.
        assert!(p
            .forbidden_tool_patterns
            .contains(&"Bash(*rm -rf*)".to_string()));
    }

    #[test]
    fn high_approval_required_locked() {
        // High level always locks approval_required=true even if overridden.
        let overrides = SecurityOverrides {
            approval_required: Some(false),
            ..Default::default()
        };
        let p = SecurityProfile::from_level(SecurityLevel::High, &overrides);
        // High mode ignores the false override.
        assert!(p.approval_required);
    }

    #[test]
    fn level_display() {
        assert_eq!(SecurityLevel::Low.to_string(), "low");
        assert_eq!(SecurityLevel::Mid.to_string(), "mid");
        assert_eq!(SecurityLevel::High.to_string(), "high");
    }

    #[test]
    fn level_parse() {
        assert_eq!("low".parse::<SecurityLevel>().unwrap(), SecurityLevel::Low);
        assert_eq!("mid".parse::<SecurityLevel>().unwrap(), SecurityLevel::Mid);
        assert_eq!(
            "high".parse::<SecurityLevel>().unwrap(),
            SecurityLevel::High
        );
        assert!("other".parse::<SecurityLevel>().is_err());
    }

    #[test]
    fn badge() {
        assert_eq!(
            SecurityProfile::from_level(SecurityLevel::Low, &no_overrides()).badge(),
            ""
        );
        assert_eq!(
            SecurityProfile::from_level(SecurityLevel::Mid, &no_overrides()).badge(),
            "[mid]"
        );
        assert_eq!(
            SecurityProfile::from_level(SecurityLevel::High, &no_overrides()).badge(),
            "[high]"
        );
    }
}
