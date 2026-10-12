// reversibility.rs: reversibility classes for outside actions (v0.17.12.3).
//
// Every change a draft makes is reversible, compensable or irrecoverable. The
// class is DATA declared by the connector, never a guess made by TA. A
// connector that declares nothing is irrecoverable: the safe default is the one
// that makes the reviewer, the queue and `ta show actions` treat the action as
// something that cannot be taken back.
//
// A connector declares three optional top-level keys in its `plugin.toml`:
//
//   reversibility = "compensable"        # reversible | compensable | irrecoverable
//   undo          = "slack.delete"       # verb that counters the action (compensable)
//   idempotency   = "key"                # key | natural | none
//
// Authority: docs/superpowers/specs/2026-10-07-atomic-apply-and-action-queue-design.md.

use serde::{Deserialize, Serialize};

/// How completely an action can be taken back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Reversibility {
    /// Can be undone completely, leaving no trace.
    Reversible,
    /// Can be countered by an opposite action, but the first action still happened.
    Compensable,
    /// Cannot be undone.
    Irrecoverable,
}

impl Reversibility {
    /// Parses a manifest value. The set of classes is closed on purpose: an
    /// unknown value returns `None` and every caller must treat that as
    /// irrecoverable (and say so loudly), never guess a friendlier class.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "reversible" => Some(Self::Reversible),
            "compensable" => Some(Self::Compensable),
            "irrecoverable" => Some(Self::Irrecoverable),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Reversible => "reversible",
            Self::Compensable => "compensable",
            Self::Irrecoverable => "irrecoverable",
        }
    }
}

impl std::fmt::Display for Reversibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// How a connector protects against a repeated send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    /// The remote accepts a caller-supplied idempotency key (TA passes draft id + action id).
    Key,
    /// Repeating the call has the same effect as making it once (an upsert).
    Natural,
    /// A repeat produces a second effect. The default.
    #[default]
    None,
}

impl Idempotency {
    /// Parses a manifest value. Closed on purpose, like [`Reversibility`]:
    /// `None` means unknown, and callers fall back to `Idempotency::None`
    /// (a repeat is assumed to produce a second effect) with a loud warning.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "key" => Some(Self::Key),
            "natural" => Some(Self::Natural),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Natural => "natural",
            Self::None => "none",
        }
    }
}

/// What a connector declared about its own actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub reversibility: Reversibility,
    /// Verb that counters the action. Present only for compensable actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
    #[serde(default)]
    pub idempotency: Idempotency,
    /// Why this class applies, in one plain sentence (shown to the reviewer).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl Declaration {
    /// What an undeclared connector gets.
    pub fn undeclared() -> Self {
        Self {
            reversibility: Reversibility::Irrecoverable,
            undo: None,
            idempotency: Idempotency::None,
            note: "the connector declares no reversibility, so TA treats it as irrecoverable"
                .into(),
        }
    }

    /// Reads the declaration from the text of a `plugin.toml`. Never fails:
    /// anything missing, malformed or self-contradictory collapses to the
    /// irrecoverable default, with the reason in `note` so `ta show actions`
    /// can say why. `source` names the manifest in log lines.
    pub fn from_manifest_text(text: &str, source: &str) -> Self {
        let table: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(manifest = %source, error = %e,
                    "connector manifest is not valid TOML; treating its actions as irrecoverable");
                return Self::undeclared();
            }
        };
        let Some(raw) = table.get("reversibility") else {
            return Self::undeclared();
        };
        let Some(reversibility) = raw.as_str().and_then(Reversibility::parse) else {
            tracing::warn!(connector = %source, value = %raw,
                "unknown reversibility value; accepted values are reversible, compensable, \
                 irrecoverable. Treating the connector's actions as irrecoverable");
            let mut d = Self::undeclared();
            d.note = format!(
                "{source} declares reversibility = {raw}, which is not one of reversible, \
                 compensable, irrecoverable; treated as irrecoverable"
            );
            return d;
        };
        let undo = table
            .get("undo")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let mut unknown_idempotency = None;
        let idempotency = match table.get("idempotency") {
            None => Idempotency::None,
            Some(v) => match v.as_str().and_then(Idempotency::parse) {
                Some(i) => i,
                None => {
                    tracing::warn!(connector = %source, value = %v,
                        "unknown idempotency value; accepted values are key, natural, none. \
                         Using none (a repeat is assumed to produce a second effect)");
                    unknown_idempotency = Some(v.to_string());
                    Idempotency::None
                }
            },
        };
        // A compensable action with no recorded undo cannot be compensated.
        if reversibility == Reversibility::Compensable && undo.is_none() {
            tracing::warn!(manifest = %source,
                "reversibility = \"compensable\" needs an `undo` verb; treating as irrecoverable");
            let mut d = Self::undeclared();
            d.note = format!(
                "{source} declares compensable but names no `undo` verb; treated as irrecoverable. \
                 Add undo = \"<verb>\" to the manifest."
            );
            return d;
        }
        Self {
            reversibility,
            undo: if reversibility == Reversibility::Compensable {
                undo
            } else {
                None
            },
            idempotency,
            note: unknown_idempotency
                .map(|v| {
                    format!(
                        "{source} declares idempotency = {v}, which is not one of key, natural, \
                         none; treated as none"
                    )
                })
                .unwrap_or_default(),
        }
    }
}

/// One row of the built-in reversibility table (design doc section 3).
#[derive(Debug, Clone, Copy)]
pub struct MediumRow {
    pub medium: &'static str,
    pub supplier: &'static str,
    pub class: Reversibility,
    pub undo: &'static str,
}

/// The built-in table. Connectors may declare a different class for their own
/// verbs; this table is the reference the reviewer is shown for the media TA
/// itself understands.
pub const MEDIA: &[MediumRow] = &[
    MediumRow {
        medium: "project files",
        supplier: "local filesystem",
        class: Reversibility::Reversible,
        undo: "restore from the apply journal",
    },
    MediumRow {
        medium: "local git commit",
        supplier: "git",
        class: Reversibility::Reversible,
        undo: "reset to the previous commit while unpushed",
    },
    MediumRow {
        medium: "push, PR",
        supplier: "GitHub, GitLab",
        class: Reversibility::Compensable,
        undo: "close the PR or push a revert (never force-push)",
    },
    MediumRow {
        medium: "task or wiki change",
        supplier: "Wayfinder",
        class: Reversibility::Compensable,
        undo: "restore the prior value recorded at apply",
    },
    MediumRow {
        medium: "database changes in a transaction",
        supplier: "PostgreSQL",
        class: Reversibility::Reversible,
        undo: "roll back before commit, inverse statements after",
    },
    MediumRow {
        medium: "database schema change",
        supplier: "PostgreSQL",
        class: Reversibility::Reversible,
        undo: "transactional DDL, roll back inside the transaction",
    },
    MediumRow {
        medium: "database schema change",
        supplier: "MySQL",
        class: Reversibility::Irrecoverable,
        undo: "none: DDL commits implicitly",
    },
    MediumRow {
        medium: "object storage",
        supplier: "S3 with versioning",
        class: Reversibility::Reversible,
        undo: "restore the previous version",
    },
    MediumRow {
        medium: "object storage",
        supplier: "S3 without versioning",
        class: Reversibility::Irrecoverable,
        undo: "none",
    },
    MediumRow {
        medium: "email",
        supplier: "SMTP, Gmail, SES",
        class: Reversibility::Irrecoverable,
        undo: "none: recall is unreliable and never promised",
    },
    MediumRow {
        medium: "chat message",
        supplier: "Slack, Discord",
        class: Reversibility::Compensable,
        undo: "delete the message (people may have seen it)",
    },
    MediumRow {
        medium: "social post",
        supplier: "X, LinkedIn",
        class: Reversibility::Compensable,
        undo: "delete the post (copies remain)",
    },
    MediumRow {
        medium: "calendar invite",
        supplier: "Google, Microsoft",
        class: Reversibility::Compensable,
        undo: "cancel the event (invitees are notified)",
    },
    MediumRow {
        medium: "payment",
        supplier: "Stripe",
        class: Reversibility::Compensable,
        undo: "refund or void (fees remain)",
    },
    MediumRow {
        medium: "cloud resource created",
        supplier: "AWS, GCP, Azure",
        class: Reversibility::Compensable,
        undo: "delete it (cost already incurred)",
    },
    MediumRow {
        medium: "cloud resource deleted",
        supplier: "AWS, GCP, Azure",
        class: Reversibility::Irrecoverable,
        undo: "none unless soft delete or versioning",
    },
    MediumRow {
        medium: "deployment",
        supplier: "any CD system",
        class: Reversibility::Compensable,
        undo: "roll back to the previous release",
    },
    MediumRow {
        medium: "package publish",
        supplier: "npm, crates.io",
        class: Reversibility::Irrecoverable,
        undo: "yank hides it; the version number is burned",
    },
    MediumRow {
        medium: "generic REST call",
        supplier: "any",
        class: Reversibility::Irrecoverable,
        undo: "per connector; default irrecoverable",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_reversibility_is_irrecoverable_and_names_connector_value_and_choices() {
        let d = Declaration::from_manifest_text("reversibility = \"undoable\"\n", "acme.toml");
        assert_eq!(d.reversibility, Reversibility::Irrecoverable);
        for needle in [
            "acme.toml",
            "undoable",
            "reversible, compensable, irrecoverable",
        ] {
            assert!(d.note.contains(needle), "{needle} missing from: {}", d.note);
        }
        assert!(Reversibility::parse("undoable").is_none());
    }

    #[test]
    fn unknown_idempotency_falls_back_to_none_and_says_so() {
        let d = Declaration::from_manifest_text(
            "reversibility = \"reversible\"\nidempotency = \"maybe\"\n",
            "acme.toml",
        );
        assert_eq!(d.idempotency, Idempotency::None);
        for needle in ["acme.toml", "maybe", "key, natural, none"] {
            assert!(d.note.contains(needle), "{needle} missing from: {}", d.note);
        }
        assert!(Idempotency::parse("maybe").is_none());
    }

    #[test]
    fn undeclared_is_irrecoverable() {
        let d = Declaration::from_manifest_text("name = \"x\"\ntype = \"adapter\"\n", "x");
        assert_eq!(d.reversibility, Reversibility::Irrecoverable);
        assert_eq!(d.idempotency, Idempotency::None);
        assert!(d.undo.is_none());
    }

    #[test]
    fn compensable_with_undo_and_key_parses() {
        let d = Declaration::from_manifest_text(
            "reversibility = \"compensable\"\nundo = \"slack.delete\"\nidempotency = \"key\"\n",
            "slack",
        );
        assert_eq!(d.reversibility, Reversibility::Compensable);
        assert_eq!(d.undo.as_deref(), Some("slack.delete"));
        assert_eq!(d.idempotency, Idempotency::Key);
    }

    #[test]
    fn compensable_without_undo_collapses_to_irrecoverable_with_reason() {
        let d = Declaration::from_manifest_text("reversibility = \"compensable\"\n", "slack");
        assert_eq!(d.reversibility, Reversibility::Irrecoverable);
        assert!(
            d.note.contains("undo"),
            "note must say what to add: {}",
            d.note
        );
    }

    #[test]
    fn garbage_class_and_garbage_toml_collapse_to_irrecoverable() {
        let d = Declaration::from_manifest_text("reversibility = \"mostly\"\n", "m");
        assert_eq!(d.reversibility, Reversibility::Irrecoverable);
        assert!(d.note.contains("mostly"));
        let d = Declaration::from_manifest_text("= = =", "m");
        assert_eq!(d.reversibility, Reversibility::Irrecoverable);
    }

    #[test]
    fn undo_is_dropped_for_non_compensable() {
        let d =
            Declaration::from_manifest_text("reversibility = \"reversible\"\nundo = \"x\"\n", "m");
        assert_eq!(d.reversibility, Reversibility::Reversible);
        assert!(d.undo.is_none());
    }

    #[test]
    fn media_table_matches_the_design_doc_for_key_rows() {
        let find = |m: &str, s: &str| {
            MEDIA
                .iter()
                .find(|r| r.medium == m && r.supplier == s)
                .unwrap_or_else(|| panic!("missing row {m}/{s}"))
                .class
        };
        assert_eq!(
            find("project files", "local filesystem"),
            Reversibility::Reversible
        );
        assert_eq!(
            find("email", "SMTP, Gmail, SES"),
            Reversibility::Irrecoverable
        );
        assert_eq!(
            find("chat message", "Slack, Discord"),
            Reversibility::Compensable
        );
        assert_eq!(
            find("database schema change", "MySQL"),
            Reversibility::Irrecoverable
        );
        assert_eq!(
            find("database schema change", "PostgreSQL"),
            Reversibility::Reversible
        );
        assert_eq!(
            find("package publish", "npm, crates.io"),
            Reversibility::Irrecoverable
        );
    }
}
