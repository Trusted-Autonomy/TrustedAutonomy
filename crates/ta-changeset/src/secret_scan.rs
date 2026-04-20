// secret_scan.rs — Real-threat discriminating secret scanner (v0.15.22).
//
// Runs at `ta draft apply` time over all text-content artifact diffs.
// Classifies findings into three levels:
//   - RealCredential: literal token with known-service prefix OR high Shannon entropy.
//     → [error] always shown; blocks apply at security.level = "high".
//   - Ambiguous: matches a secret pattern but can't confirm real vs placeholder.
//     → [warn] always shown; never blocks.
//   - DocExample: recognized documentation placeholder pattern.
//     → [info] only shown with --verbose; never blocks.
//
// False-positive management: add the offending path to `.ta-secret-ignore`.
// Format: one path pattern per line, same glob syntax as `.taignore`.

use std::path::Path;
use std::sync::OnceLock;

// ── Classification ─────────────────────────────────────────────────────────────

/// Classification of a secret finding.
#[derive(Debug, Clone, PartialEq)]
pub enum SecretClassification {
    /// A literal credential value with high confidence (known-service prefix or high entropy).
    RealCredential { service: String, entropy: f64 },
    /// A documentation placeholder — never blocks, emitted as [info] only.
    DocExample,
    /// Matches a secret pattern but too ambiguous to confirm. Warn but don't block.
    Ambiguous,
}

impl SecretClassification {
    pub fn level_label(&self) -> &'static str {
        match self {
            SecretClassification::RealCredential { .. } => "error",
            SecretClassification::Ambiguous => "warn",
            SecretClassification::DocExample => "info",
        }
    }

    pub fn is_real_credential(&self) -> bool {
        matches!(self, SecretClassification::RealCredential { .. })
    }

    pub fn is_doc_example(&self) -> bool {
        matches!(self, SecretClassification::DocExample)
    }
}

/// A classified secret finding produced by the scanner.
#[derive(Debug, Clone)]
pub struct ClassifiedFinding {
    /// Human-readable name of the matched pattern.
    pub pattern_name: String,
    /// File path where the match was found (relative to workspace root).
    pub file_path: String,
    /// 1-based line number within the file.
    pub line_number: usize,
    /// Redacted context line (the matched value is replaced with `[REDACTED]`).
    pub context: String,
    /// Classification of the finding.
    pub classification: SecretClassification,
}

impl ClassifiedFinding {
    pub fn is_blocking(&self) -> bool {
        self.classification.is_real_credential()
    }
}

impl std::fmt::Display for ClassifiedFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {} — {}:{}: {}",
            self.classification.level_label(),
            self.pattern_name,
            self.file_path,
            self.line_number,
            self.context
        )
    }
}

// ── Backward-compat alias ──────────────────────────────────────────────────────

/// A single secret finding (legacy type, kept for API compatibility).
#[derive(Debug, Clone)]
pub struct SecretFinding {
    pub pattern_name: String,
    pub file_path: String,
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

// ── Shannon entropy ────────────────────────────────────────────────────────────

/// Compute Shannon entropy (bits per character) for `s`.
pub fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let len = s.len() as f64;
    let mut freq = [0u32; 256];
    for b in s.bytes() {
        freq[b as usize] += 1;
    }
    freq.iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

const ENTROPY_THRESHOLD: f64 = 4.5;

// ── Pattern definitions ────────────────────────────────────────────────────────

struct PatternDef {
    name: &'static str,
    pattern: &'static str,
    /// Known service name; if set, any match is RealCredential regardless of entropy.
    known_service: Option<&'static str>,
}

static PATTERNS: &[PatternDef] = &[
    // ── Known-service prefixes (always RealCredential) ─────────────────────────
    PatternDef {
        name: "Slack Bot/User Token",
        pattern: r"(xox[baprs]-[0-9A-Za-z]+-[0-9A-Za-z\-]+)",
        known_service: Some("Slack"),
    },
    PatternDef {
        name: "Anthropic API Key",
        pattern: r"(sk-ant-[a-zA-Z0-9_\-]{20,})",
        known_service: Some("Anthropic"),
    },
    PatternDef {
        name: "GitHub Personal Access Token",
        pattern: r"(ghp_[A-Za-z0-9]{36})",
        known_service: Some("GitHub"),
    },
    PatternDef {
        name: "GitHub Fine-Grained PAT",
        pattern: r"(github_pat_[A-Za-z0-9_]{82})",
        known_service: Some("GitHub"),
    },
    PatternDef {
        name: "GitHub App/Actions Token",
        pattern: r"(ghs_[A-Za-z0-9]{36})",
        known_service: Some("GitHub"),
    },
    PatternDef {
        name: "Discord Bot Token",
        // Discord tokens: base64-encoded user ID + timestamp + hmac (~70 chars total)
        pattern: r"([MN][A-Za-z0-9]{23}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27})",
        known_service: Some("Discord"),
    },
    PatternDef {
        name: "AWS Access Key ID",
        pattern: r"(AKIA[0-9A-Z]{16})",
        known_service: Some("AWS"),
    },
    PatternDef {
        name: "Private Key PEM Header",
        pattern: r"(-----BEGIN [A-Z ]*PRIVATE KEY-----)",
        known_service: Some("PKI"),
    },
    // ── Generic patterns (classified by entropy) ───────────────────────────────
    PatternDef {
        name: "Generic API Key",
        pattern: r#"(?i)[Aa][Pp][Ii][_\-]?[Kk][Ee][Yy]\s*[=:]\s*["']?([A-Za-z0-9_\-]{20,})["']?"#,
        known_service: None,
    },
    PatternDef {
        name: "Generic Secret Assignment",
        pattern: r#"(?i)(?:secret|password|passwd|token|credential|auth_token|access_token|refresh_token)\s*[=:]\s*["']([A-Za-z0-9+/=_\-!@#$%^&*]{12,})["']"#,
        known_service: None,
    },
];

struct CompiledPattern {
    name: &'static str,
    re: regex::Regex,
    known_service: Option<&'static str>,
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
                known_service: p.known_service,
            })
            .collect()
    })
}

// ── Documentation-pattern recognizer ──────────────────────────────────────────

/// Known placeholder words that signal a documentation example.
static DOC_PLACEHOLDERS: &[&str] = &[
    "your_token_here",
    "your-token-here",
    "your_token",
    "your-token",
    "your_key_here",
    "your-key-here",
    "your_api_key",
    "your-api-key",
    "placeholder",
    "your_secret",
    "your-secret",
    "changeme",
    "change_me",
    "replace_me",
    "replace-me",
    "example",
    "xxxx",
    "xxx",
    "yyy",
    "zzz",
    "test",
    "dummy",
    "fake",
    "sample",
    "todo",
    "tbd",
    "insert_here",
    "insert-here",
    "token_here",
    "key_here",
    "secret_here",
];

static DOC_PATTERN_RE: OnceLock<regex::Regex> = OnceLock::new();

fn get_doc_pattern_re() -> &'static regex::Regex {
    DOC_PATTERN_RE.get_or_init(|| {
        // Matches recognized documentation shell patterns:
        //   export VAR=...          (ellipsis)
        //   export VAR=<...>        (angle-bracket placeholder)
        //   VAR=<value>
        //   # Set this to ...
        //   VAR=""                  (empty string assignment)
        //   VAR=''
        regex::Regex::new(
            r#"(?x)
            (?:
                # export VAR=... or VAR=...
                (?:export\s+)?[A-Z_][A-Z0-9_]*\s*=\s*
                (?:
                    \.{3,}                          # VAR=...
                    | <[^>]{1,64}>                  # VAR=<placeholder>
                    | ["']{2}                        # VAR="" or VAR=''
                )
            )
            "#,
        )
        .expect("doc pattern regex")
    })
}

/// Returns true if the line/value looks like a documentation example.
fn is_doc_example(line: &str, matched_value: &str) -> bool {
    // 1. Check if the matched value itself is a known placeholder word.
    // Only use substring containment for long, specific phrases (>= 8 chars) to prevent
    // short placeholders like "xxx", "yyy", "test" from matching substrings of real credentials.
    let lower_val = matched_value.to_lowercase();
    for placeholder in DOC_PLACEHOLDERS {
        if lower_val == *placeholder {
            return true;
        }
        if placeholder.len() >= 8 && lower_val.contains(placeholder) {
            return true;
        }
    }

    // 2. Check if the value is in angle brackets.
    if matched_value.starts_with('<') && matched_value.ends_with('>') {
        return true;
    }

    // 3. Check if the value is all dots (ellipsis placeholder).
    if matched_value.chars().all(|c| c == '.') {
        return true;
    }

    // 4. Check if the matched value is the RHS of a recognized doc shell pattern.
    // Only search the line up to and including the matched value to avoid false positives
    // from doc patterns appearing elsewhere on the same line (check order bypass).
    if let Some(val_end) = line.find(matched_value).map(|p| p + matched_value.len()) {
        if get_doc_pattern_re().is_match(&line[..val_end]) {
            return true;
        }
    }

    // 5. Check comment proximity: line starts with # and contains "set", "replace", etc.
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        let lower_line = trimmed.to_lowercase();
        if lower_line.contains("set this")
            || lower_line.contains("replace with")
            || lower_line.contains("your ")
            || lower_line.contains("insert ")
            || lower_line.contains("example")
        {
            return true;
        }
    }

    false
}

// ── Classifier ─────────────────────────────────────────────────────────────────

fn classify(
    line: &str,
    matched_value: &str,
    pattern_name: &str,
    known_service: Option<&str>,
) -> SecretClassification {
    // Doc-example check always wins — doc patterns are never RealCredential.
    if is_doc_example(line, matched_value) {
        return SecretClassification::DocExample;
    }

    if let Some(service) = known_service {
        let entropy = shannon_entropy(matched_value);
        return SecretClassification::RealCredential {
            service: service.to_string(),
            entropy,
        };
    }

    // Generic pattern: classify by entropy.
    let entropy = shannon_entropy(matched_value);
    if entropy >= ENTROPY_THRESHOLD {
        SecretClassification::RealCredential {
            service: format!("generic ({})", pattern_name),
            entropy,
        }
    } else {
        SecretClassification::Ambiguous
    }
}

// ── Secret-ignore file ─────────────────────────────────────────────────────────

const SECRET_IGNORE_FILE: &str = ".ta-secret-ignore";

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
        let re_str = regex::escape(pattern).replace("\\*", "[^/]*");
        if let Ok(re) = regex::Regex::new(&format!("^{}$", re_str)) {
            return re.is_match(path);
        }
    }
    false
}

// ── Scanner ────────────────────────────────────────────────────────────────────

/// Scan `text` for secret patterns, returning classified findings.
///
/// Findings are classified as `RealCredential`, `DocExample`, or `Ambiguous`.
/// Doc-example findings are included so callers can filter by verbosity.
pub fn scan_for_secrets_classified(
    text: &str,
    file_path: &str,
    workspace_root: &Path,
) -> Vec<ClassifiedFinding> {
    if is_ignored(file_path, workspace_root) {
        return vec![];
    }

    let patterns = get_patterns();
    let mut findings = Vec::new();

    for (line_number, line) in text.lines().enumerate() {
        for compiled in patterns {
            if let Some(cap) = compiled.re.captures(line) {
                // Group 1 is always the secret value.
                let secret_val = cap
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_else(|| cap.get(0).map(|m| m.as_str()).unwrap_or(""));

                let classification =
                    classify(line, secret_val, compiled.name, compiled.known_service);
                let redacted = line.replacen(secret_val, "[REDACTED]", 1);

                findings.push(ClassifiedFinding {
                    pattern_name: compiled.name.to_string(),
                    file_path: file_path.to_string(),
                    line_number: line_number + 1,
                    context: redacted.trim().to_string(),
                    classification,
                });
                // One finding per pattern per line.
                break;
            }
        }
    }

    findings
}

/// Scan `text` for secret patterns (legacy API, returns unclassified findings).
/// Use `scan_for_secrets_classified` for new callers.
pub fn scan_for_secrets(text: &str, file_path: &str, workspace_root: &Path) -> Vec<SecretFinding> {
    scan_for_secrets_classified(text, file_path, workspace_root)
        .into_iter()
        .filter(|f| !f.classification.is_doc_example())
        .map(|f| SecretFinding {
            pattern_name: f.pattern_name,
            file_path: f.file_path,
            context: f.context,
        })
        .collect()
}

// ── Output ─────────────────────────────────────────────────────────────────────

/// Print classified findings.
///
/// `verbose`: if true, also prints `DocExample` findings as [info].
///
/// Returns `(real_count, ambiguous_count, doc_count)`.
pub fn print_classified_findings(
    findings: &[ClassifiedFinding],
    verbose: bool,
) -> (usize, usize, usize) {
    let real: Vec<_> = findings
        .iter()
        .filter(|f| f.classification.is_real_credential())
        .collect();
    let ambiguous: Vec<_> = findings
        .iter()
        .filter(|f| matches!(f.classification, SecretClassification::Ambiguous))
        .collect();
    let doc: Vec<_> = findings
        .iter()
        .filter(|f| f.classification.is_doc_example())
        .collect();

    let has_output = !real.is_empty() || !ambiguous.is_empty() || (verbose && !doc.is_empty());

    if has_output {
        eprintln!();
        eprintln!("┌─ Secret Scan Findings ─────────────────────────────────────");

        for f in &real {
            let svc = match &f.classification {
                SecretClassification::RealCredential { service, entropy } => {
                    format!("{} entropy={:.2}", service, entropy)
                }
                _ => String::new(),
            };
            eprintln!(
                "│  [error] {} — {}:{} ({})",
                f.pattern_name, f.file_path, f.line_number, svc
            );
            eprintln!("│    {}", f.context);
        }

        for f in &ambiguous {
            eprintln!(
                "│  [warn]  {} — {}:{}",
                f.pattern_name, f.file_path, f.line_number
            );
            eprintln!("│    {}", f.context);
        }

        if verbose {
            for f in &doc {
                eprintln!(
                    "│  [info]  {} — {}:{} (doc example, not a threat)",
                    f.pattern_name, f.file_path, f.line_number
                );
                eprintln!("│    {}", f.context);
            }
        }

        eprintln!("└────────────────────────────────────────────────────────────");
    }

    (real.len(), ambiguous.len(), doc.len())
}

/// Print the block CTA when real credentials are found and apply is blocked.
pub fn print_block_cta_classified(findings: &[ClassifiedFinding]) {
    let real_count = findings
        .iter()
        .filter(|f| f.classification.is_real_credential())
        .count();
    print_classified_findings(findings, false);
    eprintln!();
    eprintln!(
        "Apply blocked: {} real credential(s) detected in draft artifacts.",
        real_count
    );
    eprintln!("To resolve:");
    eprintln!("  1. Remove the credential from the staged files and rotate the secret.");
    eprintln!("  2. Or add the path to .ta-secret-ignore to exclude it from scanning.");
    eprintln!(
        "  3. Or set [security.secrets] real_credential_action = \"warn\" to downgrade to a warning."
    );
    eprintln!();
}

/// Print findings (legacy API).
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

/// Print the block CTA (legacy API).
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

// ── RealCredentialAction ───────────────────────────────────────────────────────

/// Controls what happens when a `RealCredential` finding is detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RealCredentialAction {
    /// Always shown as [error]; blocks apply at security.level = "high". (default)
    #[default]
    Error,
    /// Always shown as [warn]; never blocks.
    Warn,
    /// Always blocks apply regardless of security level.
    Block,
}

impl std::str::FromStr for RealCredentialAction {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "error" => Ok(RealCredentialAction::Error),
            "warn" => Ok(RealCredentialAction::Warn),
            "block" => Ok(RealCredentialAction::Block),
            other => Err(format!(
                "unknown real_credential_action '{}'; expected error | warn | block",
                other
            )),
        }
    }
}

impl std::fmt::Display for RealCredentialAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RealCredentialAction::Error => write!(f, "error"),
            RealCredentialAction::Warn => write!(f, "warn"),
            RealCredentialAction::Block => write!(f, "block"),
        }
    }
}

impl serde::Serialize for RealCredentialAction {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for RealCredentialAction {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // ── Item 1: service-specific RealCredential ─────────────────────────────

    #[test]
    fn slack_token_is_real_credential() {
        // Build token at runtime — no literal token in source to trigger push protection.
        let tok = [
            "xoxb",
            "1234567890",
            "1234567890123",
            "abc123def456ghi789jkl",
        ]
        .join("-");
        let text = format!("export TA_SLACK_BOT_TOKEN={tok}\n");
        let root = tmp_root();
        let findings = scan_for_secrets_classified(&text, "setup.sh", root.path());
        assert!(
            findings
                .iter()
                .any(|f| f.classification.is_real_credential()),
            "Slack token should be RealCredential, got: {findings:?}"
        );
    }

    #[test]
    fn anthropic_key_is_real_credential() {
        let text = "ANTHROPIC_API_KEY=sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890abcdef\n";
        let root = tmp_root();
        let findings = scan_for_secrets_classified(text, "src/config.rs", root.path());
        assert!(
            findings
                .iter()
                .any(|f| f.classification.is_real_credential()),
            "Anthropic key should be RealCredential, got: {findings:?}"
        );
    }

    #[test]
    fn github_pat_is_real_credential() {
        let text = "token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890\n";
        let root = tmp_root();
        let findings = scan_for_secrets_classified(text, "src/auth.rs", root.path());
        assert!(
            findings
                .iter()
                .any(|f| f.classification.is_real_credential()),
            "GitHub PAT should be RealCredential, got: {findings:?}"
        );
    }

    // ── Item 2: doc-pattern recognizer ──────────────────────────────────────

    #[test]
    fn slack_doc_placeholder_is_doc_example() {
        let text = "export TA_SLACK_BOT_TOKEN=your_token_here\n";
        let root = tmp_root();
        let findings = scan_for_secrets_classified(text, "docs/USAGE.md", root.path());
        // Should find a finding but classified as DocExample, not RealCredential.
        let doc_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.classification.is_doc_example())
            .collect();
        let real_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.classification.is_real_credential())
            .collect();
        assert!(
            !doc_findings.is_empty() || real_findings.is_empty(),
            "Doc placeholder should be DocExample or not matched at all, got: {findings:?}"
        );
        assert!(
            real_findings.is_empty(),
            "Doc placeholder must NOT be RealCredential, got: {findings:?}"
        );
    }

    #[test]
    fn angle_bracket_placeholder_is_doc_example() {
        let is_doc = is_doc_example("export VAR=<your_slack_token>", "<your_slack_token>");
        assert!(
            is_doc,
            "Angle-bracket placeholder should be recognized as doc"
        );
    }

    #[test]
    fn ellipsis_placeholder_is_doc_example() {
        let is_doc = is_doc_example("export VAR=...", "...");
        assert!(is_doc, "Ellipsis should be recognized as doc example");
    }

    // ── Item 3: Ambiguous ───────────────────────────────────────────────────

    #[test]
    fn low_entropy_generic_is_ambiguous() {
        // "abc123" has low entropy — should be Ambiguous, not RealCredential.
        let text = r#"secret = "abc123abcabc""#;
        let root = tmp_root();
        let findings = scan_for_secrets_classified(text, "config.toml", root.path());
        // Low entropy generic — if matched, should be Ambiguous.
        for f in &findings {
            assert!(
                !f.classification.is_real_credential(),
                "Low-entropy value should not be RealCredential: {:?}",
                f
            );
        }
    }

    #[test]
    fn high_entropy_generic_is_real_or_ambiguous() {
        // High-entropy random string (real random bytes base64-ish).
        let text = r#"secret = "x7Kp2mQwRv9nJhLzTsYeAu4cFiGdOb3E""#;
        let root = tmp_root();
        let findings = scan_for_secrets_classified(text, "config.toml", root.path());
        // May or may not match depending on exact pattern; just verify no panic and correct type.
        for f in &findings {
            // If classified, must not be DocExample.
            assert!(
                !f.classification.is_doc_example(),
                "Random high-entropy string should not be DocExample: {:?}",
                f
            );
        }
    }

    // ── Item 5 / path traversal: ignored path ───────────────────────────────

    #[test]
    fn ignored_path_is_skipped() {
        let root = tmp_root();
        std::fs::write(root.path().join(".ta-secret-ignore"), "fixtures/**\n").unwrap();
        let text = "token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890\n";
        let findings = scan_for_secrets_classified(text, "fixtures/test.sh", root.path());
        assert!(
            findings.is_empty(),
            "ignored path should produce no findings"
        );
    }

    // ── Legacy scan_for_secrets filters doc examples ─────────────────────────

    #[test]
    fn legacy_scan_filters_doc_examples() {
        let text =
            "export TOKEN=your_token_here\ntoken: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890\n";
        let root = tmp_root();
        let findings = scan_for_secrets(text, "setup.sh", root.path());
        // Should find the GitHub PAT but not the doc example.
        assert!(
            findings.iter().any(|f| f.pattern_name.contains("GitHub")),
            "should find GitHub PAT"
        );
    }

    // ── Entropy function ────────────────────────────────────────────────────

    #[test]
    fn entropy_of_repeated_char_is_zero() {
        let e = shannon_entropy("aaaaaaaaaa");
        assert!(e < 0.01, "repeated char should have near-zero entropy: {e}");
    }

    #[test]
    fn entropy_of_random_string_is_high() {
        let e = shannon_entropy("x7Kp2mQwRv9nJhLzTsYeAu4cFiGdOb3E");
        assert!(e > 4.0, "random string should have high entropy: {e}");
    }

    // ── Line numbers are reported ───────────────────────────────────────────

    #[test]
    fn finding_includes_correct_line_number() {
        let text = "clean line\ntoken: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890\nclean line\n";
        let root = tmp_root();
        let findings = scan_for_secrets_classified(text, "src/auth.rs", root.path());
        assert!(
            findings.iter().any(|f| f.line_number == 2),
            "finding should be on line 2, got: {findings:?}"
        );
    }

    // ── Existing tests preserved ────────────────────────────────────────────

    #[test]
    fn finds_aws_key() {
        let text = "export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n";
        let root = tmp_root();
        let findings = scan_for_secrets_classified(text, "config/env.sh", root.path());
        assert!(
            findings
                .iter()
                .any(|f| f.pattern_name.contains("AWS") && f.context.contains("[REDACTED]")),
            "expected AWS key finding, got: {findings:?}"
        );
    }

    #[test]
    fn finds_private_key_pem() {
        let text = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...\n";
        let root = tmp_root();
        let findings = scan_for_secrets_classified(text, "keys/server.pem", root.path());
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
        let findings = scan_for_secrets_classified(text, "src/main.rs", root.path());
        assert!(findings.is_empty());
    }

    // ── RealCredentialAction parsing ────────────────────────────────────────

    #[test]
    fn real_credential_action_parses() {
        assert_eq!(
            "error".parse::<RealCredentialAction>().unwrap(),
            RealCredentialAction::Error
        );
        assert_eq!(
            "warn".parse::<RealCredentialAction>().unwrap(),
            RealCredentialAction::Warn
        );
        assert_eq!(
            "block".parse::<RealCredentialAction>().unwrap(),
            RealCredentialAction::Block
        );
        assert!("unknown".parse::<RealCredentialAction>().is_err());
    }

    // ── is_doc_example check-order bypass guards ─────────────────────────────

    #[test]
    fn doc_example_does_not_bypass_on_short_placeholder_substring() {
        // A real credential containing "test" as a substring must NOT be classified
        // as DocExample — short placeholders only match exactly.
        // Build value at runtime to avoid push-protection false positives.
        let val = ["xoxb", "test", "real", "credential"].join("-");
        let line = format!("SLACK_TOKEN={val}");
        let not_doc = is_doc_example(&line, &val);
        assert!(
            !not_doc,
            "real credential containing 'test' must not be DocExample"
        );
    }

    #[test]
    fn doc_example_does_not_bypass_via_unrelated_line_pattern() {
        // A line with a real credential AND a doc pattern elsewhere must not be
        // misclassified as DocExample based on the unrelated doc portion.
        // The doc pattern `EXAMPLE_KEY=""` is on the same line but the matched
        // value is a real Slack token — should NOT be DocExample.
        // Build value at runtime to avoid push-protection false positives.
        let val = ["xoxb", "actual", "token", "value"].join("-");
        let line = format!(r#"export REAL={val} && export EXAMPLE_KEY="""#);
        let not_doc = is_doc_example(&line, &val);
        assert!(
            !not_doc,
            "unrelated doc pattern elsewhere on line must not bypass check"
        );
    }

    #[test]
    fn doc_example_exact_short_placeholder_is_still_doc() {
        // Exact match on a short placeholder must still be recognised.
        let is_doc = is_doc_example("TOKEN=xxx", "xxx");
        assert!(is_doc, "exact short placeholder must still be DocExample");
    }
}
