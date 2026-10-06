// terms.rs — First-run terms acceptance gate.
//
// On first run of a mutating command (ta init, ta run, ta goal start), TA
// prompts the user to review and accept the terms of use. Acceptance state is
// stored at ~/.config/ta/accepted_terms as JSON, keyed by a SHA-256 hash of
// the terms text. When terms change (new binary with updated terms.txt), the
// user is prompted once more.
//
// Read-only commands (ta plan list, ta draft view, ta goal list, ta stats, …)
// never gate on terms acceptance.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

/// The terms of use text, embedded at compile time from terms.txt.
const TERMS: &str = include_str!("../terms.txt");

/// Where acceptance state is stored.
fn terms_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("ta")
            .join("accepted_terms"),
    )
}

/// Compute SHA-256 hash of the terms text (first 16 hex chars for brevity).
fn terms_hash() -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(TERMS.as_bytes());
    let result = hasher.finalize();
    hex_encode(&result[..8])
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TermsAcceptance {
    accepted: bool,
    terms_hash: String,
    accepted_at: String,
    version: String,
}

/// Check if terms have been accepted for the current terms version.
/// Returns `Ok(())` if accepted, `Err` if not yet accepted or if the terms
/// have changed since the last acceptance.
pub fn check_accepted() -> anyhow::Result<()> {
    let path = match terms_path() {
        Some(p) => p,
        None => return Ok(()), // Can't determine home dir — skip check.
    };

    if !path.exists() {
        return Err(anyhow::anyhow!("terms not yet accepted"));
    }

    let content = std::fs::read_to_string(&path)?;
    let acceptance: TermsAcceptance = serde_json::from_str(&content)?;

    if !acceptance.accepted || acceptance.terms_hash != terms_hash() {
        return Err(anyhow::anyhow!(
            "terms have been updated since last acceptance"
        ));
    }

    Ok(())
}

/// Ensure terms are accepted before a mutating operation.
///
/// - If already accepted: returns `Ok(())` immediately (fast path).
/// - If not accepted and stdin is an interactive terminal: displays terms and
///   prompts the user. On acceptance, records it and returns `Ok(())`.
/// - If not accepted and stdin is **not** a terminal (CI / headless): prints a
///   clear error message directing the user to `ta accept-terms --yes`.
pub fn ensure_accepted() -> anyhow::Result<()> {
    if check_accepted().is_ok() {
        return Ok(());
    }

    if !std::io::stdin().is_terminal() {
        return Err(anyhow::anyhow!(
            "TA terms of use have not been accepted.\n\
             \n\
             This environment is non-interactive (CI/headless). To pre-accept the terms:\n\
             \n\
             \x20 ta accept-terms --yes\n\
             \n\
             View the terms first with:  ta view-terms"
        ));
    }

    // Interactive terminal — show terms and prompt.
    prompt_and_accept()
}

/// Display the terms and prompt the user for interactive acceptance.
pub fn prompt_and_accept() -> anyhow::Result<()> {
    println!("{}", TERMS);
    println!("─────────────────────────────────────────────────────");
    print!("Do you accept these terms? [y/N] ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    let answer = input.trim().to_lowercase();
    if answer != "y" && answer != "yes" {
        return Err(anyhow::anyhow!(
            "Terms not accepted. Cannot continue without accepting the terms of use.\n\
             Run `ta accept-terms` to review and accept, or `ta view-terms` to read them."
        ));
    }

    record_acceptance()?;
    println!("\nTerms accepted. You can now use ta.");
    Ok(())
}

/// Accept terms non-interactively (for CI / scripted usage with --yes).
pub fn accept_non_interactive() -> anyhow::Result<()> {
    println!("{}", TERMS);
    println!("─────────────────────────────────────────────────────");
    record_acceptance()?;
    println!("Terms accepted (non-interactive).");
    Ok(())
}

/// Write the acceptance record to disk.
fn record_acceptance() -> anyhow::Result<()> {
    let path = terms_path()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory for terms storage"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let acceptance = TermsAcceptance {
        accepted: true,
        terms_hash: terms_hash(),
        accepted_at: chrono::Utc::now().to_rfc3339(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let json = serde_json::to_string_pretty(&acceptance)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Show the current acceptance status.
pub fn show_status() -> anyhow::Result<()> {
    let path = match terms_path() {
        Some(p) => p,
        None => {
            println!("Cannot determine terms file location.");
            return Ok(());
        }
    };

    if !path.exists() {
        println!("Terms have not been accepted yet.");
        println!("Run `ta accept-terms` to review and accept.");
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)?;
    let acceptance: TermsAcceptance = serde_json::from_str(&content)?;
    let current_hash = terms_hash();

    if acceptance.terms_hash == current_hash {
        println!("Terms accepted: {}", acceptance.accepted_at);
        println!("Version: {}", acceptance.version);
        println!("Status: current");
    } else {
        println!("Terms were accepted for a previous version.");
        println!("Last accepted: {}", acceptance.accepted_at);
        println!("Run `ta accept-terms` to accept the updated terms.");
    }

    Ok(())
}

/// View the full terms text.
pub fn view_terms() {
    println!("{}", TERMS);
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Finding 4 (final whole-branch review): shared across every
    // `commands::*` test module (this one and `run.rs`) so HOME-mutating
    // tests serialize against each other regardless of which module they
    // live in -- two independently-declared `Mutex` statics in the same test
    // binary cannot do that, since neither lock knows about the other. See
    // `crate::commands::test_support` for the full rationale.
    use crate::commands::test_support::HOME_ENV_LOCK as HOME_MUTEX;

    #[test]
    fn terms_hash_is_stable() {
        let h1 = terms_hash();
        let h2 = terms_hash();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16); // 8 bytes = 16 hex chars
    }

    #[test]
    fn terms_text_is_not_empty() {
        assert!(!TERMS.is_empty());
        assert!(TERMS.contains("Trusted Autonomy"));
        assert!(TERMS.contains("WITHOUT WARRANTY"));
        assert!(TERMS.len() > 200, "terms should be substantive");
    }

    #[test]
    fn acceptance_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted_terms");

        let acceptance = TermsAcceptance {
            accepted: true,
            terms_hash: terms_hash(),
            accepted_at: "2026-04-01T00:00:00Z".to_string(),
            version: "0.15.5-alpha".to_string(),
        };

        let json = serde_json::to_string_pretty(&acceptance).unwrap();
        std::fs::write(&path, &json).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let loaded: TermsAcceptance = serde_json::from_str(&content).unwrap();
        assert!(loaded.accepted);
        assert_eq!(loaded.terms_hash, terms_hash());
    }

    #[test]
    fn check_accepted_returns_err_when_no_file() {
        let _lock = HOME_MUTEX.lock().unwrap();
        // Point HOME at an empty temp dir so no accepted_terms file exists.
        let dir = tempfile::tempdir().unwrap();
        let orig_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let result = check_accepted();

        // Restore HOME before asserting (so other tests aren't affected).
        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert!(
            result.is_err(),
            "expected Err when no acceptance file exists"
        );
    }

    #[test]
    fn check_accepted_returns_err_on_stale_hash() {
        let _lock = HOME_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join(".config").join("ta");
        std::fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("accepted_terms");

        // Write acceptance with a wrong hash.
        let acceptance = TermsAcceptance {
            accepted: true,
            terms_hash: "0000000000000000".to_string(), // stale
            accepted_at: "2026-01-01T00:00:00Z".to_string(),
            version: "0.1.0-alpha".to_string(),
        };
        let json = serde_json::to_string_pretty(&acceptance).unwrap();
        std::fs::write(&path, &json).unwrap();

        let orig_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let result = check_accepted();
        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert!(result.is_err(), "expected Err on stale hash");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("updated"),
            "error should mention updated terms: {}",
            msg
        );
    }

    #[test]
    fn check_accepted_returns_ok_with_valid_acceptance() {
        let _lock = HOME_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join(".config").join("ta");
        std::fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("accepted_terms");

        let acceptance = TermsAcceptance {
            accepted: true,
            terms_hash: terms_hash(),
            accepted_at: "2026-04-01T00:00:00Z".to_string(),
            version: "0.15.5-alpha".to_string(),
        };
        let json = serde_json::to_string_pretty(&acceptance).unwrap();
        std::fs::write(&path, &json).unwrap();

        let orig_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let result = check_accepted();
        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert!(result.is_ok(), "expected Ok with valid acceptance file");
    }

    #[test]
    fn record_acceptance_writes_correct_file() {
        let _lock = HOME_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let orig_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let result = record_acceptance();

        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert!(result.is_ok());

        let path = dir.path().join(".config").join("ta").join("accepted_terms");
        assert!(path.exists(), "accepted_terms file should be written");

        let content = std::fs::read_to_string(&path).unwrap();
        let loaded: TermsAcceptance = serde_json::from_str(&content).unwrap();
        assert!(loaded.accepted);
        assert_eq!(loaded.terms_hash, terms_hash());
        assert!(!loaded.accepted_at.is_empty());
        assert!(!loaded.version.is_empty());
    }
}
