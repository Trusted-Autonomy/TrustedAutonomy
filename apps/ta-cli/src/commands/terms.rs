// terms.rs — First-run terms acceptance gate.
//
// On first run of a mutating command (ta init, ta run, ta goal start), TA
// prompts the user to review and accept the terms of use. Acceptance state is
// stored at ~/.config/ta/accepted_terms as JSON. The record binds the
// acceptance to the exact text: `terms_hash` (first 16 hex chars of the
// SHA-256, the original field, kept so an older `ta` still reads the file),
// plus `document_version` (TERMS_VERSION) and `document_sha256` (the full
// SHA-256). When the text changes (new binary with updated terms.txt), or its
// TERMS_VERSION is bumped, the user is prompted once more.
//
// Records written before documents were versioned carry only `terms_hash`.
// They stay valid while that hash matches the current text (as before) and
// `ta terms-status --json` marks them `legacy_record`.
//
// Read-only commands (ta plan list, ta draft view, ta goal list, ta stats, …)
// never gate on terms acceptance.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use ta_workspace::{TermsState, TermsStatusReport, TERMS_STATUS_SCHEMA_VERSION};

/// The terms of use text, embedded at compile time from terms.txt.
const TERMS: &str = include_str!("../terms.txt");

/// Version label of the terms document (not the `ta` version). Bump it in the
/// same change that edits `terms.txt`; a test pins the label to the text's
/// SHA-256 so one cannot change without the other. Bumping it makes every
/// recorded acceptance stale, so everyone is asked again.
pub const TERMS_VERSION: &str = "1";

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

/// Lowercase hex SHA-256 of `text`, byte for byte.
fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex_encode(&hasher.finalize())
}

/// Full SHA-256 (64 hex chars) of the embedded terms text.
fn terms_sha256() -> String {
    sha256_hex(TERMS)
}

/// First 16 hex chars of the SHA-256, the value stored in `terms_hash`.
fn terms_hash() -> String {
    terms_sha256()[..16].to_string()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TermsAcceptance {
    accepted: bool,
    terms_hash: String,
    accepted_at: String,
    /// The `ta` version that recorded the acceptance.
    version: String,
    /// `TERMS_VERSION` at acceptance. Absent in records from before documents were versioned.
    #[serde(default)]
    document_version: Option<String>,
    /// Full SHA-256 of the terms text at acceptance. Absent in older records.
    #[serde(default)]
    document_sha256: Option<String>,
}

/// Whether a recorded acceptance covers the terms this `ta` embeds.
#[derive(Debug, PartialEq, Eq)]
enum Standing {
    /// Covers the current text. `legacy` when the record has no full hash.
    Current { legacy: bool },
    /// Does not cover the current text, or was never an acceptance.
    Stale,
}

fn standing(acceptance: &TermsAcceptance) -> Standing {
    if !acceptance.accepted || acceptance.terms_hash != terms_hash() {
        return Standing::Stale;
    }
    match &acceptance.document_sha256 {
        None => Standing::Current { legacy: true },
        Some(full) => {
            let version_moved = acceptance
                .document_version
                .as_deref()
                .is_some_and(|v| v != TERMS_VERSION);
            if *full != terms_sha256() || version_moved {
                Standing::Stale
            } else {
                Standing::Current { legacy: false }
            }
        }
    }
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

    if matches!(standing(&acceptance), Standing::Stale) {
        return Err(anyhow::anyhow!(
            "terms have been updated since last acceptance"
        ));
    }

    Ok(())
}

/// Print terms-related text. Normally stdout; with `json` it goes to stderr so
/// stdout carries only the command's JSON document.
fn say(json: bool, text: &str) {
    if json {
        eprint!("{text}");
        let _ = std::io::stderr().flush();
    } else {
        print!("{text}");
        let _ = std::io::stdout().flush();
    }
}

/// Ensure terms are accepted before a mutating operation.
///
/// - If already accepted: returns `Ok(())` immediately (fast path).
/// - If not accepted and stdin is an interactive terminal: displays terms and
///   prompts the user. On acceptance, records it and returns `Ok(())`.
/// - If not accepted and stdin is **not** a terminal (CI / headless): prints a
///   clear error message directing the user to `ta accept-terms --yes`.
///
/// `json` is true when the command about to run prints a JSON document on
/// stdout (`ta init --json`): the prompt then goes to stderr.
pub fn ensure_accepted(json: bool) -> anyhow::Result<()> {
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
    prompt_and_accept_to(json)
}

/// Display the terms and prompt the user for interactive acceptance.
pub fn prompt_and_accept() -> anyhow::Result<()> {
    prompt_and_accept_to(false)
}

fn prompt_and_accept_to(json: bool) -> anyhow::Result<()> {
    say(json, &format!("{}\n", TERMS));
    say(
        json,
        "─────────────────────────────────────────────────────\n",
    );
    say(json, "Do you accept these terms? [y/N] ");

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
    say(json, "\nTerms accepted. You can now use ta.\n");
    Ok(())
}

/// Accept terms non-interactively (for CI / scripted usage with --yes).
/// With `json`, the terms text and confirmation go to stderr.
pub fn accept_non_interactive(json: bool) -> anyhow::Result<()> {
    say(json, &format!("{}\n", TERMS));
    say(
        json,
        "─────────────────────────────────────────────────────\n",
    );
    record_acceptance()?;
    say(json, "Terms accepted (non-interactive).\n");
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
        document_version: Some(TERMS_VERSION.to_string()),
        document_sha256: Some(terms_sha256()),
    };

    let json = serde_json::to_string_pretty(&acceptance)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// The terms document this binary embeds: its version label and the SHA-256
/// of its exact text. `ta capabilities` reports `terms_status` from this.
pub fn document_facts() -> (&'static str, String) {
    (TERMS_VERSION, terms_sha256())
}

const NEXT_STEP: &str = "Run `ta accept-terms` to review and accept, or `ta accept-terms --yes` \
                         to accept without a prompt.";

/// Build the `ta terms-status --json` document from the acceptance record at
/// `path` (None when the home directory is unknown).
fn status_report(path: Option<&Path>) -> anyhow::Result<TermsStatusReport> {
    let mut report = TermsStatusReport {
        schema_version: TERMS_STATUS_SCHEMA_VERSION,
        status: TermsState::Unavailable,
        document_version: TERMS_VERSION.to_string(),
        document_sha256: terms_sha256(),
        accepted_document_version: None,
        accepted_document_sha256: None,
        accepted_at: None,
        accepted_by_ta_version: None,
        legacy_record: false,
        record_path: path.map(|p| p.display().to_string()),
        next_step: None,
    };
    let Some(path) = path else {
        report.next_step = Some(
            "Set HOME (or USERPROFILE on Windows) so TA can find ~/.config/ta/accepted_terms."
                .to_string(),
        );
        return Ok(report);
    };
    if !path.exists() {
        report.status = TermsState::NotAccepted;
        report.next_step = Some(NEXT_STEP.to_string());
        return Ok(report);
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(
            "Cannot read the terms acceptance record {} ({}). Check its permissions, or delete it \
             and run `ta accept-terms`.",
            path.display(),
            e
        )
    })?;
    let acceptance: TermsAcceptance = serde_json::from_str(&content).map_err(|e| {
        anyhow::anyhow!(
            "The terms acceptance record {} is not valid ({}). Delete it and run `ta accept-terms`.",
            path.display(),
            e
        )
    })?;

    report.accepted_at = Some(acceptance.accepted_at.clone());
    report.accepted_by_ta_version = Some(acceptance.version.clone());
    report.accepted_document_version = acceptance.document_version.clone();
    report.accepted_document_sha256 = acceptance.document_sha256.clone();
    match standing(&acceptance) {
        Standing::Current { legacy } => {
            report.status = TermsState::Current;
            report.legacy_record = legacy;
        }
        Standing::Stale => {
            report.status = if acceptance.accepted {
                TermsState::Stale
            } else {
                TermsState::NotAccepted
            };
            report.next_step = Some(NEXT_STEP.to_string());
        }
    }
    Ok(report)
}

/// Show the current acceptance status: the human lines, or with `json` one
/// `TermsStatusReport` document on stdout.
pub fn show_status(json: bool) -> anyhow::Result<()> {
    let report = status_report(terms_path().as_deref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print!("{}", status_text(&report));
    Ok(())
}

/// The human `ta terms-status` text. The `Status: current` line and the
/// `Version:` line (the `ta` version that accepted) are read by the shell
/// installer; keep them as they are.
fn status_text(report: &TermsStatusReport) -> String {
    let at = report.accepted_at.as_deref().unwrap_or_default();
    match report.status {
        TermsState::Unavailable => "Cannot determine terms file location.\n".to_string(),
        TermsState::NotAccepted if report.accepted_at.is_none() => {
            "Terms have not been accepted yet.\nRun `ta accept-terms` to review and accept.\n"
                .to_string()
        }
        TermsState::Current => format!(
            "Terms accepted: {}\nVersion: {}\nStatus: current\n",
            at,
            report.accepted_by_ta_version.as_deref().unwrap_or_default()
        ),
        TermsState::Stale | TermsState::NotAccepted => format!(
            "Terms were accepted for a previous version.\nLast accepted: {}\nRun `ta accept-terms` to accept the updated terms.\n",
            at
        ),
    }
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
            document_version: None,
            document_sha256: None,
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
            document_version: None,
            document_sha256: None,
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
            document_version: None,
            document_sha256: None,
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

    /// Editing `terms.txt` must come with a `TERMS_VERSION` bump: the pair is
    /// pinned here, so changing either alone fails this test. After a
    /// deliberate edit, bump `TERMS_VERSION`, then update both constants below.
    #[test]
    fn terms_text_and_version_are_pinned_together() {
        const PINNED_VERSION: &str = "1";
        const PINNED_SHA256: &str =
            "4ab92aa7b55fe8cc00f6ed6f0343f6e98a2300894d0c8b660a1292f450a9f3a7";
        assert_eq!(
            (TERMS_VERSION, terms_sha256().as_str()),
            (PINNED_VERSION, PINNED_SHA256),
            "terms.txt or TERMS_VERSION changed. Bump TERMS_VERSION in commands/terms.rs for \
             any edit to terms.txt, then update PINNED_VERSION and PINNED_SHA256 here"
        );
    }

    #[test]
    fn full_hash_is_sha256_of_the_exact_text_and_changes_with_it() {
        let full = terms_sha256();
        assert_eq!(full.len(), 64);
        assert!(full
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        // The legacy 16-hex field is the prefix of the full hash.
        assert!(full.starts_with(&terms_hash()));
        // Known vector: SHA-256("abc").
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // One changed character changes the hash.
        let edited = TERMS.replacen("Trusted Autonomy", "Trusted Autonomy!", 1);
        assert_ne!(sha256_hex(&edited), full);
        // A trailing newline is part of the text.
        assert_ne!(sha256_hex(&format!("{TERMS}\n")), full);
    }

    fn write_record(dir: &Path, acceptance: &TermsAcceptance) -> PathBuf {
        let path = dir.join("accepted_terms");
        std::fs::write(&path, serde_json::to_string_pretty(acceptance).unwrap()).unwrap();
        path
    }

    fn fresh_acceptance() -> TermsAcceptance {
        TermsAcceptance {
            accepted: true,
            terms_hash: terms_hash(),
            accepted_at: "2026-10-01T00:00:00Z".to_string(),
            version: "0.17.12-alpha.5".to_string(),
            document_version: Some(TERMS_VERSION.to_string()),
            document_sha256: Some(terms_sha256()),
        }
    }

    #[test]
    fn status_report_without_a_record_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let r = status_report(Some(&dir.path().join("accepted_terms"))).unwrap();
        assert_eq!(r.status, TermsState::NotAccepted);
        assert_eq!(r.document_version, TERMS_VERSION);
        assert_eq!(r.document_sha256, terms_sha256());
        assert!(r.accepted_at.is_none() && r.accepted_document_sha256.is_none());
        assert!(r.next_step.as_deref().unwrap().contains("ta accept-terms"));
    }

    #[test]
    fn status_report_without_a_home_is_unavailable() {
        let r = status_report(None).unwrap();
        assert_eq!(r.status, TermsState::Unavailable);
        assert!(r.record_path.is_none());
    }

    #[test]
    fn status_report_current_carries_the_accepted_version_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_record(dir.path(), &fresh_acceptance());
        let r = status_report(Some(&path)).unwrap();
        assert_eq!(r.status, TermsState::Current);
        assert!(!r.legacy_record);
        assert_eq!(
            r.accepted_document_sha256.as_deref(),
            Some(terms_sha256().as_str())
        );
        assert_eq!(r.accepted_document_version.as_deref(), Some(TERMS_VERSION));
        assert_eq!(r.accepted_by_ta_version.as_deref(), Some("0.17.12-alpha.5"));
        assert!(r.next_step.is_none());
    }

    #[test]
    fn changed_text_makes_an_acceptance_stale() {
        // An acceptance recorded for different text (full hash and prefix both differ).
        let other = sha256_hex("an earlier edition of the terms");
        let mut a = fresh_acceptance();
        a.terms_hash = other[..16].to_string();
        a.document_sha256 = Some(other);
        let dir = tempfile::tempdir().unwrap();
        let path = write_record(dir.path(), &a);
        let r = status_report(Some(&path)).unwrap();
        assert_eq!(r.status, TermsState::Stale);
        assert_eq!(r.accepted_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert!(r.next_step.is_some());
    }

    #[test]
    fn a_version_bump_alone_makes_an_acceptance_stale() {
        let mut a = fresh_acceptance();
        a.document_version = Some("0".to_string());
        assert_eq!(standing(&a), Standing::Stale);
    }

    #[test]
    fn a_full_hash_mismatch_with_a_matching_prefix_is_stale() {
        let mut a = fresh_acceptance();
        a.document_sha256 = Some("0".repeat(64));
        assert_eq!(standing(&a), Standing::Stale);
    }

    #[test]
    fn a_record_from_before_documents_were_versioned_stays_current_and_is_flagged() {
        // Exactly what `ta` wrote before: four fields, no document_* fields.
        let legacy = format!(
            r#"{{"accepted":true,"terms_hash":"{}","accepted_at":"2026-04-01T00:00:00Z","version":"0.15.5-alpha"}}"#,
            terms_hash()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted_terms");
        std::fs::write(&path, legacy).unwrap();
        let r = status_report(Some(&path)).unwrap();
        assert_eq!(r.status, TermsState::Current);
        assert!(r.legacy_record);
        assert!(r.accepted_document_sha256.is_none());
        assert!(r.accepted_document_version.is_none());
        assert_eq!(r.accepted_by_ta_version.as_deref(), Some("0.15.5-alpha"));
    }

    #[test]
    fn a_legacy_record_for_other_text_is_stale() {
        let legacy = r#"{"accepted":true,"terms_hash":"0000000000000000","accepted_at":"2026-04-01T00:00:00Z","version":"0.15.5-alpha"}"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted_terms");
        std::fs::write(&path, legacy).unwrap();
        let r = status_report(Some(&path)).unwrap();
        assert_eq!(r.status, TermsState::Stale);
        assert!(!r.legacy_record);
    }

    #[test]
    fn an_unreadable_record_is_an_error_that_says_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted_terms");
        std::fs::write(&path, "not json").unwrap();
        let err = status_report(Some(&path)).unwrap_err().to_string();
        assert!(
            err.contains("accepted_terms") && err.contains("ta accept-terms"),
            "{err}"
        );
    }

    #[test]
    fn recorded_acceptance_binds_version_and_full_hash_and_reads_back_current() {
        let _lock = HOME_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let orig_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let recorded = record_acceptance();
        let report = status_report(terms_path().as_deref());
        let gate = check_accepted();
        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        recorded.unwrap();
        gate.unwrap();
        let r = report.unwrap();
        assert_eq!(r.status, TermsState::Current);
        assert_eq!(r.accepted_document_version.as_deref(), Some(TERMS_VERSION));
        assert_eq!(
            r.accepted_document_sha256.as_deref(),
            Some(terms_sha256().as_str())
        );
        assert!(!r.legacy_record);
    }

    /// The shell installer reads these lines; they must not move.
    #[test]
    fn human_status_lines_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_record(dir.path(), &fresh_acceptance());
        let current = status_text(&status_report(Some(&path)).unwrap());
        assert_eq!(
            current,
            "Terms accepted: 2026-10-01T00:00:00Z\nVersion: 0.17.12-alpha.5\nStatus: current\n"
        );
        let none = status_text(&status_report(Some(&dir.path().join("missing"))).unwrap());
        assert_eq!(
            none,
            "Terms have not been accepted yet.\nRun `ta accept-terms` to review and accept.\n"
        );
        let mut stale = fresh_acceptance();
        stale.terms_hash = "0000000000000000".to_string();
        let path = write_record(dir.path(), &stale);
        assert_eq!(
            status_text(&status_report(Some(&path)).unwrap()),
            "Terms were accepted for a previous version.\nLast accepted: 2026-10-01T00:00:00Z\nRun `ta accept-terms` to accept the updated terms.\n"
        );
    }
}
