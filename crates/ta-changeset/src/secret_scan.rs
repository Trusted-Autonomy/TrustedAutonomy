// secret_scan.rs — Regex-based secret scanner for draft artifacts (v0.15.14.4).
//
// Runs at `ta draft apply` time over all text-content artifact diffs.
// Scans for credentials, API keys, private key PEM headers, and other
// high-confidence secret patterns.
//
// Behavior is controlled by `SecretScanMode` from `SecurityProfile`:
//   - `Off`   → skip entirely (explicit opt-out)
//   - `Warn`  → print findings, allow apply to continue (default for low/mid)
//   - `Block` → print findings, abort apply with remediation CTA (default for high)
//
// False-positive management: add the offending path to `.ta-secret-ignore`.
// Format: one path pattern per line, same glob syntax as `.taignore`.

use std::path::Path;

// Patterns are compiled once at module init via `std::sync::OnceLock`.
use std::sync::OnceLock;

/// A single secret finding produced by the scanner.
#[derive(Debug, Clone)]
pub struct SecretFinding {
    /// Human-readable name of the matched pattern.
    pub pattern_name: String,
    /// File path where the match was found (relative to workspace root).
    pub file_path: String,
    /// Redacted context line (the matched value is replaced with `[REDACTED]`).
    pub context: String,
}

impl std::fmt::Display for SecretFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[secret] {} in {}: {}",
            self.pattern_name, self.file_path, self.context
        )
    }
}

// ── Pattern definitions ────────────────────────────────────────────────────────

struct PatternDef {
    name: &'static str,
    /// The capture group that holds the secret value for redaction.
    pattern: &'static str,
}

static PATTERNS: &[PatternDef] = &[
    PatternDef {
        name: "AWS Access Key ID",
        pattern: r"(AKIA[0-9A-Z]{16})",
    },
    PatternDef {
        name: "GitHub Personal Access Token",
        pattern: r"(ghp_[A-Za-z0-9]{36})",
    },
    PatternDef {
        name: "Generic API Key",
        pattern: r"(?i)[Aa][Pp][Ii][_\-]?[Kk][Ee][Yy]\s*[=:]\s*([A-Za-z0-9_\-]{20,})",
    },
    PatternDef {
        name: "Private Key PEM Header",
        pattern: r"(-----BEGIN [A-Z ]*PRIVATE KEY-----)",
    },
    PatternDef {
        name: "Generic Secret Assignment",
        pattern: r#"(?i)(?:secret|password|passwd|token|credential|auth_token|access_token|refresh_token)\s*[=:]\s*["']([A-Za-z0-9+/=_\-!@#$%^&*]{12,})["']"#,
    },
];

struct CompiledPattern {
    name: &'static str,
    re: regex::Regex,
}

static COMPILED: OnceLock<Vec<CompiledPattern>> = OnceLock::new();

fn get_patterns() -> &'static Vec<CompiledPattern> {
    COMPILED.get_or_init(|| {
        PATTERNS
            .iter()
            .map(|p| CompiledPattern {
                name: p.name,
                re: regex::Regex::new(p.pattern)
                    .unwrap_or_else(|e| panic!("bad secret pattern {}: {}", p.name, e)),
            })
            .collect()
    })
}

// ── Secret-ignore file ─────────────────────────────────────────────────────────

const SECRET_IGNORE_FILE: &str = ".ta-secret-ignore";

/// Returns true if the path should be excluded from scanning based on
/// `.ta-secret-ignore` patterns in `workspace_root`.
fn is_ignored(file_path: &str, workspace_root: &Path) -> bool {
    let ignore_path = workspace_root.join(SECRET_IGNORE_FILE);
    if !ignore_path.exists() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(&ignore_path) else {
        return false;
    };
    for line in content.lines() {
        let pattern = line.trim();
        if pattern.is_empty() || pattern.starts_with('#') {
            continue;
        }
        if glob_matches(pattern, file_path) {
            return true;
        }
    }
    false
}

/// Minimal glob matcher: `*` matches within a path component, `**` matches any segments.
fn glob_matches(pattern: &str, path: &str) -> bool {
    if pattern == path {
        return true;
    }
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.splitn(2, "**").collect();
        let prefix = parts[0];
        let suffix = parts.get(1).unwrap_or(&"");
        if prefix.is_empty() {
            return path.ends_with(suffix.trim_start_matches('/'));
        }
        return path.starts_with(prefix) && path.ends_with(suffix.trim_start_matches('/'));
    }
    if pattern.contains('*') {
        // Treat * as matching a single component segment.
        let re_str = regex::escape(pattern).replace("\\*", "[^/]*");
        if let Ok(re) = regex::Regex::new(&format!("^{}$", re_str)) {
            return re.is_match(path);
        }
    }
    false
}

// ── Scanner ────────────────────────────────────────────────────────────────────

/// Scan `text` (the text content of `file_path`) for secret patterns.
/// Returns a list of findings with redacted context lines.
///
/// `workspace_root` is used to check `.ta-secret-ignore`.
pub fn scan_for_secrets(text: &str, file_path: &str, workspace_root: &Path) -> Vec<SecretFinding> {
    if is_ignored(file_path, workspace_root) {
        return vec![];
    }

    let patterns = get_patterns();
    let mut findings = Vec::new();

    for line in text.lines() {
        // Scan the raw line. Regex patterns match anywhere in the line,
        // so no prefix stripping is needed. Diff format prefixes (+/-/ )
        // are harmless since the secret patterns look for specific high-confidence strings.
        let stripped = line;

        for compiled in patterns {
            if let Some(m) = compiled.re.find(stripped) {
                let secret_val = m.as_str();
                let redacted = stripped.replacen(secret_val, "[REDACTED]", 1);
                findings.push(SecretFinding {
                    pattern_name: compiled.name.to_string(),
                    file_path: file_path.to_string(),
                    context: redacted.trim().to_string(),
                });
                // One finding per pattern per line is enough.
                break;
            }
        }
    }

    findings
}

/// Print findings to stderr and return whether any were found.
pub fn print_findings(findings: &[SecretFinding]) -> bool {
    if findings.is_empty() {
        return false;
    }
    eprintln!();
    eprintln!("┌─ Secret Scan Findings ─────────────────────────────────────");
    for f in findings {
        eprintln!(
            "│  [{pattern}] {file}",
            pattern = f.pattern_name,
            file = f.file_path
        );
        eprintln!("│    {}", f.context);
    }
    eprintln!("└────────────────────────────────────────────────────────────");
    true
}

/// Print the block CTA when `SecretScanMode::Block` aborts apply.
pub fn print_block_cta(findings: &[SecretFinding]) {
    print_findings(findings);
    eprintln!();
    eprintln!(
        "Apply blocked: {} secret finding(s) detected in draft artifacts.",
        findings.len()
    );
    eprintln!("To resolve:");
    eprintln!("  1. Remove secrets from the staged files.");
    eprintln!("  2. Or add the path to .ta-secret-ignore to exclude it from scanning.");
    eprintln!("  3. Or set [security.secrets] scan = \"warn\" to downgrade to a warning.");
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // Item 9f: secret scanner finds AWS key.
    #[test]
    fn finds_aws_key() {
        let text = "export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n";
        let root = tmp_root();
        let findings = scan_for_secrets(text, "config/env.sh", root.path());
        assert!(
            findings
                .iter()
                .any(|f| f.pattern_name.contains("AWS") && f.context.contains("[REDACTED]")),
            "expected AWS key finding, got: {findings:?}"
        );
    }

    // Item 9f: secret scanner finds GitHub PAT.
    #[test]
    fn finds_github_pat() {
        let text = "token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890\n";
        let root = tmp_root();
        let findings = scan_for_secrets(text, "src/auth.rs", root.path());
        assert!(
            findings
                .iter()
                .any(|f| f.pattern_name.contains("GitHub") && f.context.contains("[REDACTED]")),
            "expected GitHub PAT finding, got: {findings:?}"
        );
    }

    #[test]
    fn finds_private_key_pem() {
        let text = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...\n";
        let root = tmp_root();
        let findings = scan_for_secrets(text, "keys/server.pem", root.path());
        assert!(
            findings
                .iter()
                .any(|f| f.pattern_name.contains("Private Key")),
            "expected private key finding, got: {findings:?}"
        );
    }

    #[test]
    fn clean_text_produces_no_findings() {
        let text = "fn main() { println!(\"hello\"); }\n";
        let root = tmp_root();
        let findings = scan_for_secrets(text, "src/main.rs", root.path());
        assert!(findings.is_empty());
    }

    #[test]
    fn ignored_path_is_skipped() {
        let root = tmp_root();
        std::fs::write(root.path().join(".ta-secret-ignore"), "fixtures/**\n").unwrap();
        let text = "token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890\n";
        let findings = scan_for_secrets(text, "fixtures/test.sh", root.path());
        assert!(
            findings.is_empty(),
            "ignored path should produce no findings"
        );
    }
}
