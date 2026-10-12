// rules/postures.rs: the shipped postures, as data (v0.17.12.4).
//
// The three postures are plain TOML in `crates/ta-actions/postures/` embedded
// at build time. They use the same format as the owner file, so `ta rules
// show` prints a posture's cells with the same machinery as anyone's rules.
// A posture is a starting point: the owner's own rules override it, and a
// preset or the org floor can only tighten on top.

use sha2::{Digest, Sha256};

use super::model::{normalize_text, DocKind, RulesDoc};

/// Names of the shipped postures, strictest first.
pub const NAMES: [&str; 3] = ["strict", "balanced", "open"];

/// The posture used when the owner's file names none, and when there is no
/// owner file at all.
pub const DEFAULT: &str = "strict";

const STRICT: &str = include_str!("../../postures/strict.toml");
const BALANCED: &str = include_str!("../../postures/balanced.toml");
const OPEN: &str = include_str!("../../postures/open.toml");

/// A loaded posture.
#[derive(Debug, Clone)]
pub struct PostureData {
    pub name: String,
    pub revision: u32,
    pub description: String,
    /// sha256 (hex) of the shipped text. Recorded in the signature so a later
    /// TA that ships a different table for the same posture name is noticed.
    pub digest: String,
    pub doc: RulesDoc,
}

fn shipped_text(name: &str) -> Option<&'static str> {
    match name {
        "strict" => Some(STRICT),
        "balanced" => Some(BALANCED),
        "open" => Some(OPEN),
        _ => None,
    }
}

pub fn digest_of(text: &str) -> String {
    let digest = Sha256::digest(normalize_text(text).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The shipped text of a posture, for snapshots taken at signing time.
pub fn shipped(name: &str) -> Option<&'static str> {
    shipped_text(&name.trim().to_ascii_lowercase())
}

/// Load a shipped posture by name.
pub fn load(name: &str) -> Result<PostureData, String> {
    let key = name.trim().to_ascii_lowercase();
    let Some(text) = shipped_text(&key) else {
        return Err(format!(
            "posture \"{name}\" is not one of {}. Fix the posture line in your rules file.",
            NAMES.join(", ")
        ));
    };
    from_text(&key, text)
}

/// Parse posture text (the shipped copy, or the snapshot taken when the owner
/// last signed, so `ta rules diff` can show what a newer TA would change).
pub fn from_text(key: &str, text: &str) -> Result<PostureData, String> {
    let doc = RulesDoc::parse(text, DocKind::Posture, &format!("posture {key}")).map_err(|e| {
        // A shipped posture that does not parse is a TA bug, not user error.
        format!(
            "the shipped {key} posture is invalid (this is a bug in TA, please report it): {}",
            e.join("; ")
        )
    })?;
    Ok(PostureData {
        name: key.to_string(),
        revision: doc.revision,
        description: doc.description.clone().unwrap_or_default(),
        digest: digest_of(text),
        doc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_posture_parses() {
        for name in NAMES {
            let p = load(name).unwrap();
            assert_eq!(p.name, name);
            assert!(!p.doc.rules.is_empty());
            assert_eq!(p.digest.len(), 64);
        }
    }

    #[test]
    fn unknown_posture_names_the_choices() {
        let e = load("lax").unwrap_err();
        assert!(e.contains("strict") && e.contains("balanced") && e.contains("open"));
    }

    #[test]
    fn digest_ignores_crlf() {
        assert_eq!(digest_of("a\nb\n"), digest_of("a\r\nb\r\n"));
    }
}
