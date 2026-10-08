//! Developer-local settings read from a gitignored `.env.local` at the project
//! root. Today this is only the code-signing identity (`TA_CODESIGN_IDENTITY`),
//! so a developer can name their own local certificate without exporting an
//! environment variable in every shell. See `.env.local.example`.
//!
//! Lookup order for each setting: real environment variable, then
//! `<project_root>/.env.local`, then the built-in default. The file is parsed,
//! never executed: only simple `KEY=value` lines (optional single or double
//! quotes, `#` comments) are read.

use std::path::Path;

/// Name of the per-developer, gitignored settings file at the project root.
pub const ENV_LOCAL_FILE: &str = ".env.local";

/// Default local code-signing identity (the certificate name that
/// `install_local.sh` documents creating in Keychain Access).
pub const DEFAULT_CODESIGN_IDENTITY: &str = "Trusted Autonomy Local Dev";

/// Reads `key` from the contents of an `.env.local` file. The last matching
/// line wins. Returns `None` when the key is absent or empty.
pub fn parse_env_local(contents: &str, key: &str) -> Option<String> {
    let mut found = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(v);
        found = Some(v.to_string());
    }
    found.filter(|v| !v.is_empty())
}

/// The local code-signing identity: `TA_CODESIGN_IDENTITY` from the
/// environment, else from `<project_root>/.env.local`, else the default.
pub fn codesign_identity(project_root: &Path) -> String {
    if let Ok(v) = std::env::var("TA_CODESIGN_IDENTITY") {
        if !v.is_empty() {
            return v;
        }
    }
    std::fs::read_to_string(project_root.join(ENV_LOCAL_FILE))
        .ok()
        .and_then(|c| parse_env_local(&c, "TA_CODESIGN_IDENTITY"))
        .unwrap_or_else(|| DEFAULT_CODESIGN_IDENTITY.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_quoted_export_and_comment_forms() {
        let f = "# comment\n\nOTHER=x\nTA_CODESIGN_IDENTITY=Plain Name\n";
        assert_eq!(
            parse_env_local(f, "TA_CODESIGN_IDENTITY").as_deref(),
            Some("Plain Name")
        );
        let f = "export TA_CODESIGN_IDENTITY=\"My Dev Cert\"\n";
        assert_eq!(
            parse_env_local(f, "TA_CODESIGN_IDENTITY").as_deref(),
            Some("My Dev Cert")
        );
        let f = "TA_CODESIGN_IDENTITY='Single Quoted'\n";
        assert_eq!(
            parse_env_local(f, "TA_CODESIGN_IDENTITY").as_deref(),
            Some("Single Quoted")
        );
    }

    #[test]
    fn last_value_wins_and_empty_or_missing_is_none() {
        let f = "TA_CODESIGN_IDENTITY=a\nTA_CODESIGN_IDENTITY=b\n";
        assert_eq!(
            parse_env_local(f, "TA_CODESIGN_IDENTITY").as_deref(),
            Some("b")
        );
        assert_eq!(
            parse_env_local("TA_CODESIGN_IDENTITY=\n", "TA_CODESIGN_IDENTITY"),
            None
        );
        assert_eq!(parse_env_local("OTHER=1\n", "TA_CODESIGN_IDENTITY"), None);
        // A key that merely contains the name is not a match.
        assert_eq!(
            parse_env_local("XTA_CODESIGN_IDENTITY=z\n", "TA_CODESIGN_IDENTITY"),
            None
        );
    }

    #[test]
    fn identity_falls_back_to_file_then_default() {
        // Only meaningful when the environment variable is unset; the CI and
        // dev shells do not set it, and a set value is covered by the first
        // branch of `codesign_identity`.
        if std::env::var("TA_CODESIGN_IDENTITY").is_ok() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(codesign_identity(dir.path()), DEFAULT_CODESIGN_IDENTITY);
        std::fs::write(
            dir.path().join(ENV_LOCAL_FILE),
            "TA_CODESIGN_IDENTITY=Somebody Else Dev\n",
        )
        .unwrap();
        assert_eq!(codesign_identity(dir.path()), "Somebody Else Dev");
    }
}
