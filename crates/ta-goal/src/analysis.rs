//! Language-aware static analysis configuration, finding parsers, and language detection.
//!
//! Provides per-language analysis tool config ([`AnalysisConfig`]), structured
//! [`AnalysisFinding`] output parsed from popular tools (mypy, pyright, cargo-clippy,
//! golangci-lint, eslint/tsc), and helpers for the correction-loop driver in the
//! governed workflow engine.

use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Enums ─────────────────────────────────────────────────────────────────────

/// How to handle a static analysis failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Fail the workflow step immediately with a findings table.
    #[default]
    Fail,
    /// Log findings and continue (non-blocking).
    Warn,
    /// Spawn a targeted fix agent and iterate until clean or max iterations reached.
    Agent,
}

impl std::fmt::Display for OnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OnFailure::Fail => write!(f, "fail"),
            OnFailure::Warn => write!(f, "warn"),
            OnFailure::Agent => write!(f, "agent"),
        }
    }
}

/// What to do when `max_iterations` is exhausted in the correction loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnMaxIterations {
    /// Emit a warning with remaining findings and continue.
    #[default]
    Warn,
    /// Fail the workflow step with remaining findings.
    Fail,
}

/// Severity of a static analysis finding.
///
/// Maps from tool-specific severity terminology to a common scale.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Error,
    Warning,
    Note,
    Info,
}

impl std::fmt::Display for FindingSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingSeverity::Error => write!(f, "error"),
            FindingSeverity::Warning => write!(f, "warning"),
            FindingSeverity::Note => write!(f, "note"),
            FindingSeverity::Info => write!(f, "info"),
        }
    }
}

// ── Config ────────────────────────────────────────────────────────────────────

/// Per-language static analysis configuration.
///
/// Configured in `.ta/workflow.toml` under `[analysis.<lang>]`:
///
/// ```toml
/// [analysis.python]
/// tool = "mypy"
/// args = ["--strict"]
/// on_failure = "agent"
/// max_iterations = 3
///
/// [analysis.rust]
/// tool = "cargo-clippy"
/// args = ["-D", "warnings"]
/// on_failure = "warn"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisConfig {
    /// Analysis tool to run (e.g. `"mypy"`, `"pyright"`, `"cargo-clippy"`, `"golangci-lint"`).
    pub tool: String,
    /// Extra arguments appended to the tool invocation.
    #[serde(default)]
    pub args: Vec<String>,
    /// Behavior on analysis failure. Default: `fail`.
    #[serde(default)]
    pub on_failure: OnFailure,
    /// Maximum correction-loop iterations when `on_failure = "agent"`. Default: 3.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Behavior when `max_iterations` is exhausted. Default: `warn`.
    #[serde(default)]
    pub on_max_iterations: OnMaxIterations,
}

fn default_max_iterations() -> u32 {
    3
}

// ── Language detection ────────────────────────────────────────────────────────

/// Languages that TA understands for static analysis.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Python,
    TypeScript,
    Rust,
    Go,
    Other(String),
}

impl Language {
    /// The canonical string key used in `[analysis.<key>]` TOML sections.
    pub fn as_key(&self) -> String {
        match self {
            Language::Python => "python".to_string(),
            Language::TypeScript => "typescript".to_string(),
            Language::Rust => "rust".to_string(),
            Language::Go => "go".to_string(),
            Language::Other(s) => s.clone(),
        }
    }
}

impl std::str::FromStr for Language {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "python" | "py" => Language::Python,
            "typescript" | "ts" => Language::TypeScript,
            "rust" | "rs" => Language::Rust,
            "go" | "golang" => Language::Go,
            other => Language::Other(other.to_string()),
        })
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_key())
    }
}

/// Auto-detect the primary language of a workspace from marker files.
///
/// Manual config (`[analysis.<lang>]` in `workflow.toml`) always takes precedence;
/// this is a fallback for when no explicit config is present.
///
/// Detection priority: Rust > Go > TypeScript > Python.
pub fn detect_language(workspace_root: &Path) -> Option<Language> {
    if workspace_root.join("Cargo.toml").exists() {
        return Some(Language::Rust);
    }
    if workspace_root.join("go.mod").exists() {
        return Some(Language::Go);
    }
    // TypeScript: package.json present AND at least one .ts file in top-level entries.
    if workspace_root.join("package.json").exists() {
        let has_ts = workspace_root
            .read_dir()
            .ok()
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .any(|e| e.path().extension().is_some_and(|ext| ext == "ts"))
            })
            .unwrap_or(false);
        if has_ts {
            return Some(Language::TypeScript);
        }
    }
    // Python: pyproject.toml, setup.py, or requirements.txt.
    if workspace_root.join("pyproject.toml").exists()
        || workspace_root.join("setup.py").exists()
        || workspace_root.join("requirements.txt").exists()
    {
        return Some(Language::Python);
    }
    None
}

// ── Finding ───────────────────────────────────────────────────────────────────

/// A structured finding from a static analysis tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnalysisFinding {
    /// Source file path (relative to workspace root when possible).
    pub file: String,
    /// 1-based line number. 0 when line information is unavailable.
    pub line: u32,
    /// 1-based column number. 0 when column information is unavailable.
    pub col: u32,
    /// Tool-specific error / rule code (e.g. `"E501"`, `"no-unused-vars"`).
    /// Empty string when the tool does not emit codes.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Severity level.
    pub severity: FindingSeverity,
}

impl AnalysisFinding {
    /// Format findings as a compact markdown table suitable for agent prompts.
    pub fn format_table(findings: &[AnalysisFinding]) -> String {
        let mut out = String::from("| File | Line | Col | Code | Severity | Message |\n");
        out.push_str("|------|------|-----|------|----------|---------|\n");
        for f in findings {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                f.file, f.line, f.col, f.code, f.severity, f.message
            ));
        }
        out
    }

    /// Collect the unique set of affected files, preserving first-seen order.
    pub fn affected_files(findings: &[AnalysisFinding]) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        findings
            .iter()
            .filter(|f| seen.insert(f.file.clone()))
            .map(|f| f.file.clone())
            .collect()
    }

    /// Build an objective prompt for a targeted fix agent.
    pub fn build_fix_prompt(tool: &str, lang: &Language, findings: &[AnalysisFinding]) -> String {
        let table = Self::format_table(findings);
        let files = Self::affected_files(findings);
        let files_list = files.join(", ");
        format!(
            "Fix all {tool} ({lang}) analysis findings listed below.\n\
            Only modify the flagged files ({files_list}).\n\
            Do NOT change any other files, do NOT update PLAN.md, \
            do NOT add comments or refactors unrelated to the flagged findings.\n\n\
            Findings to fix:\n\n{table}"
        )
    }
}

// ── Parsers ───────────────────────────────────────────────────────────────────

/// Parse `mypy` text output into findings.
///
/// Expected format: `path/to/file.py:42: error: Cannot find  [import-untyped]`
pub fn parse_mypy(output: &str) -> Vec<AnalysisFinding> {
    let mut findings = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        // Skip summary lines.
        if trimmed.is_empty()
            || trimmed.starts_with("Found ")
            || trimmed.starts_with("Success:")
            || trimmed.starts_with("note:")
        {
            continue;
        }
        // file:line: severity: message  [code]
        let parts: Vec<&str> = trimmed.splitn(4, ':').collect();
        if parts.len() < 3 {
            continue;
        }
        let file = parts[0].trim().to_string();
        let Ok(line_no) = parts[1].trim().parse::<u32>() else {
            continue;
        };
        let rest = parts[2..].join(":");
        let rest = rest.trim();
        // severity: message
        let (severity_str, message_rest) = if let Some(idx) = rest.find(": ") {
            (&rest[..idx], &rest[idx + 2..])
        } else {
            continue;
        };
        let severity = match severity_str.trim().to_lowercase().as_str() {
            "error" => FindingSeverity::Error,
            "warning" => FindingSeverity::Warning,
            "note" => FindingSeverity::Note,
            _ => FindingSeverity::Info,
        };
        // Extract trailing `  [error-code]`
        let (message, code) = if let Some(bracket_pos) = message_rest.rfind('[') {
            let code = message_rest[bracket_pos + 1..]
                .trim_end_matches(']')
                .to_string();
            let msg = message_rest[..bracket_pos].trim().to_string();
            (msg, code)
        } else {
            (message_rest.trim().to_string(), String::new())
        };
        findings.push(AnalysisFinding {
            file,
            line: line_no,
            col: 0,
            code,
            message,
            severity,
        });
    }
    findings
}

/// Parse `pyright --outputjson` JSON output into findings.
///
/// Pyright emits a JSON object with a `generalDiagnostics` array.
pub fn parse_pyright_json(output: &str) -> Vec<AnalysisFinding> {
    // Pyright may prefix the JSON with a shebang / version line; find the JSON object.
    let json_start = output.find('{').unwrap_or(0);
    let Ok(val): Result<serde_json::Value, _> = serde_json::from_str(&output[json_start..]) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    let diagnostics = match val
        .get("generalDiagnostics")
        .or_else(|| val.get("diagnostics"))
    {
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        _ => return findings,
    };
    for d in &diagnostics {
        let file = d["file"].as_str().unwrap_or("").to_string();
        // pyright line numbers are 0-based.
        let line = d["range"]["start"]["line"].as_u64().unwrap_or(0) as u32 + 1;
        let col = d["range"]["start"]["character"].as_u64().unwrap_or(0) as u32 + 1;
        let message = d["message"].as_str().unwrap_or("").to_string();
        let rule = d["rule"].as_str().unwrap_or("").to_string();
        let severity = match d["severity"].as_str().unwrap_or("") {
            "error" => FindingSeverity::Error,
            "warning" => FindingSeverity::Warning,
            "information" | "info" => FindingSeverity::Info,
            _ => FindingSeverity::Note,
        };
        if file.is_empty() && message.is_empty() {
            continue;
        }
        findings.push(AnalysisFinding {
            file,
            line,
            col,
            code: rule,
            message,
            severity,
        });
    }
    findings
}

/// Parse `cargo clippy --message-format json` NDJSON output into findings.
///
/// Each line is a JSON object. Only lines with `reason = "compiler-message"` matter.
pub fn parse_clippy_json(output: &str) -> Vec<AnalysisFinding> {
    let mut findings = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(val): Result<serde_json::Value, _> = serde_json::from_str(line) else {
            continue;
        };
        if val.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let msg = &val["message"];
        let level = msg["level"].as_str().unwrap_or("");
        let severity = match level {
            "error" => FindingSeverity::Error,
            "warning" => FindingSeverity::Warning,
            "note" | "help" => FindingSeverity::Note,
            _ => continue, // skip irrelevant entries (e.g. "icu")
        };
        let message = msg["message"].as_str().unwrap_or("").to_string();
        let code = msg["code"]["code"].as_str().unwrap_or("").to_string();
        // Use the primary span for file/line info.
        let spans = msg["spans"].as_array().cloned().unwrap_or_default();
        let primary = spans
            .iter()
            .find(|s| s["is_primary"].as_bool().unwrap_or(false));
        if let Some(span) = primary {
            let file = span["file_name"].as_str().unwrap_or("").to_string();
            let line = span["line_start"].as_u64().unwrap_or(0) as u32;
            let col = span["column_start"].as_u64().unwrap_or(0) as u32;
            if file.is_empty() && message.is_empty() {
                continue;
            }
            findings.push(AnalysisFinding {
                file,
                line,
                col,
                code,
                message,
                severity,
            });
        } else if !message.is_empty() {
            // No span — capture as a file-less finding.
            findings.push(AnalysisFinding {
                file: String::new(),
                line: 0,
                col: 0,
                code,
                message,
                severity,
            });
        }
    }
    findings
}

/// Parse `golangci-lint run --out-format json` output into findings.
pub fn parse_golangci_json(output: &str) -> Vec<AnalysisFinding> {
    let json_start = output.find('{').unwrap_or(0);
    let Ok(val): Result<serde_json::Value, _> = serde_json::from_str(&output[json_start..]) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    let issues = match val.get("Issues") {
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        _ => return findings,
    };
    for issue in &issues {
        let linter = issue["FromLinter"].as_str().unwrap_or("").to_string();
        let text = issue["Text"].as_str().unwrap_or("").to_string();
        let file = issue["Pos"]["Filename"].as_str().unwrap_or("").to_string();
        let line = issue["Pos"]["Line"].as_u64().unwrap_or(0) as u32;
        let col = issue["Pos"]["Column"].as_u64().unwrap_or(0) as u32;
        if text.is_empty() {
            continue;
        }
        findings.push(AnalysisFinding {
            file,
            line,
            col,
            code: linter,
            message: text,
            severity: FindingSeverity::Warning,
        });
    }
    findings
}

/// Parse eslint or tsc JSON output into findings.
///
/// eslint format: `[{"filePath": ..., "messages": [{"line": N, "column": N, "ruleId": ..., "message": ..., "severity": 1|2}]}]`
pub fn parse_eslint_json(output: &str) -> Vec<AnalysisFinding> {
    // eslint output is a JSON array; tsc doesn't produce JSON by default.
    let Ok(val): Result<serde_json::Value, _> = serde_json::from_str(output) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    let files = match val.as_array() {
        Some(arr) => arr,
        None => return findings,
    };
    for file_result in files {
        let file = file_result["filePath"].as_str().unwrap_or("").to_string();
        let messages = match file_result["messages"].as_array() {
            Some(arr) => arr,
            None => continue,
        };
        for msg in messages {
            let line = msg["line"].as_u64().unwrap_or(0) as u32;
            let col = msg["column"].as_u64().unwrap_or(0) as u32;
            let code = msg["ruleId"].as_str().unwrap_or("").to_string();
            let message = msg["message"].as_str().unwrap_or("").to_string();
            let severity = match msg["severity"].as_u64().unwrap_or(1) {
                2 => FindingSeverity::Error,
                _ => FindingSeverity::Warning,
            };
            if message.is_empty() {
                continue;
            }
            findings.push(AnalysisFinding {
                file: file.clone(),
                line,
                col,
                code,
                message,
                severity,
            });
        }
    }
    findings
}

/// Dispatch output parsing to the correct parser based on tool name.
///
/// Unknown tools: capture each non-empty line as a raw warning finding.
pub fn parse_output(tool: &str, stdout: &str) -> Vec<AnalysisFinding> {
    match tool {
        "mypy" => parse_mypy(stdout),
        "pyright" => parse_pyright_json(stdout),
        "cargo-clippy" | "clippy" => parse_clippy_json(stdout),
        "golangci-lint" => parse_golangci_json(stdout),
        "eslint" | "tsc" => parse_eslint_json(stdout),
        _ => stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(i, l)| AnalysisFinding {
                file: String::new(),
                line: i as u32 + 1,
                col: 0,
                code: String::new(),
                message: l.to_string(),
                severity: FindingSeverity::Warning,
            })
            .collect(),
    }
}

// ── Runner ────────────────────────────────────────────────────────────────────

/// Run the configured static analysis tool, parse its output, and return structured results.
///
/// Returns `(exit_success, stdout, findings)`.
pub fn run_analyzer(
    config: &AnalysisConfig,
    workspace_root: &Path,
) -> anyhow::Result<(bool, String, Vec<AnalysisFinding>)> {
    let (cmd_name, extra_args) = build_command_args(config);

    let output = std::process::Command::new(cmd_name)
        .args(&extra_args)
        .current_dir(workspace_root)
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to run '{}': {}. Is the tool installed and on PATH?",
                config.tool,
                e
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    // For clippy JSON, output is on stdout only; for others stderr may carry info.
    let parse_input = if config.tool == "cargo-clippy" || config.tool == "clippy" {
        stdout.clone()
    } else {
        let combined = format!("{}\n{}", stdout, stderr);
        combined
    };
    let _ = extra_args; // suppress unused warning

    let findings = parse_output(&config.tool, &parse_input);
    let success = output.status.success();
    Ok((success, parse_input, findings))
}

/// Build the (command, args) pair to invoke the analysis tool, injecting
/// machine-readable output flags where appropriate.
fn build_command_args(config: &AnalysisConfig) -> (&str, Vec<String>) {
    match config.tool.as_str() {
        "cargo-clippy" | "clippy" => {
            let mut args = vec!["clippy".to_string()];
            args.extend(config.args.clone());
            // Inject JSON output format if not already present.
            if !args.iter().any(|a| a.starts_with("--message-format")) {
                args.push("--message-format".to_string());
                args.push("json".to_string());
            }
            ("cargo", args)
        }
        "pyright" => {
            let mut args = config.args.clone();
            if !args.contains(&"--outputjson".to_string()) {
                args.push("--outputjson".to_string());
            }
            ("pyright", args)
        }
        "golangci-lint" => {
            let mut args = config.args.clone();
            if !args.iter().any(|a| a.starts_with("--out-format")) {
                args.push("--out-format".to_string());
                args.push("json".to_string());
            }
            ("golangci-lint", args)
        }
        other => (other, config.args.clone()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── mypy parser ───────────────────────────────────────────────────────────

    #[test]
    fn parse_mypy_basic_error() {
        let output = "src/foo.py:42: error: Cannot find implementation  [import-untyped]\n\
                      src/bar.py:7: warning: Redundant cast  [redundant-cast]\n\
                      Found 2 errors in 2 files (checked 5 source files)\n";
        let findings = parse_mypy(output);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].file, "src/foo.py");
        assert_eq!(findings[0].line, 42);
        assert_eq!(findings[0].severity, FindingSeverity::Error);
        assert_eq!(findings[0].code, "import-untyped");
        assert_eq!(findings[0].message, "Cannot find implementation");
        assert_eq!(findings[1].file, "src/bar.py");
        assert_eq!(findings[1].severity, FindingSeverity::Warning);
        assert_eq!(findings[1].code, "redundant-cast");
    }

    #[test]
    fn parse_mypy_no_code_in_message() {
        let output = "app.py:10: error: Missing return statement\n";
        let findings = parse_mypy(output);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "");
        assert_eq!(findings[0].message, "Missing return statement");
    }

    #[test]
    fn parse_mypy_skips_summary() {
        let output = "Success: no issues found in 3 source files\n";
        let findings = parse_mypy(output);
        assert!(findings.is_empty());
    }

    // ── pyright parser ────────────────────────────────────────────────────────

    #[test]
    fn parse_pyright_json_basic() {
        let output = r#"{
            "generalDiagnostics": [
                {
                    "file": "src/main.ts",
                    "range": {"start": {"line": 9, "character": 4}},
                    "message": "Type 'string' is not assignable to type 'number'",
                    "severity": "error",
                    "rule": "reportArgumentType"
                }
            ],
            "summary": {"filesAnalyzed": 1, "errorCount": 1}
        }"#;
        let findings = parse_pyright_json(output);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/main.ts");
        assert_eq!(findings[0].line, 10); // 0-based → 1-based
        assert_eq!(findings[0].col, 5);
        assert_eq!(findings[0].severity, FindingSeverity::Error);
        assert_eq!(findings[0].code, "reportArgumentType");
    }

    #[test]
    fn parse_pyright_json_empty() {
        let output = r#"{"generalDiagnostics": [], "summary": {}}"#;
        let findings = parse_pyright_json(output);
        assert!(findings.is_empty());
    }

    // ── clippy parser ─────────────────────────────────────────────────────────

    #[test]
    fn parse_clippy_json_basic() {
        let output = r#"{"reason":"compiler-message","message":{"code":{"code":"clippy::needless_return"},"level":"warning","message":"unneeded `return` statement","spans":[{"file_name":"src/lib.rs","is_primary":true,"line_start":15,"column_start":5}]}}
{"reason":"build-finished","success":false}"#;
        let findings = parse_clippy_json(output);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/lib.rs");
        assert_eq!(findings[0].line, 15);
        assert_eq!(findings[0].col, 5);
        assert_eq!(findings[0].severity, FindingSeverity::Warning);
        assert_eq!(findings[0].code, "clippy::needless_return");
        assert_eq!(findings[0].message, "unneeded `return` statement");
    }

    #[test]
    fn parse_clippy_json_skips_non_compiler_message() {
        let output = r#"{"reason":"build-script-executed","package_id":"foo 0.1.0"}
{"reason":"build-finished","success":true}"#;
        let findings = parse_clippy_json(output);
        assert!(findings.is_empty());
    }

    // ── golangci-lint parser ──────────────────────────────────────────────────

    #[test]
    fn parse_golangci_json_basic() {
        let output = r#"{
            "Issues": [
                {
                    "FromLinter": "errcheck",
                    "Text": "Error return value of `db.Close` is not checked",
                    "Pos": {"Filename": "main.go", "Line": 23, "Column": 2}
                }
            ]
        }"#;
        let findings = parse_golangci_json(output);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "main.go");
        assert_eq!(findings[0].line, 23);
        assert_eq!(findings[0].col, 2);
        assert_eq!(findings[0].code, "errcheck");
        assert_eq!(
            findings[0].message,
            "Error return value of `db.Close` is not checked"
        );
    }

    #[test]
    fn parse_golangci_json_empty_issues() {
        let output = r#"{"Issues": []}"#;
        let findings = parse_golangci_json(output);
        assert!(findings.is_empty());
    }

    // ── eslint parser ─────────────────────────────────────────────────────────

    #[test]
    fn parse_eslint_json_basic() {
        let output = r#"[
            {
                "filePath": "src/index.ts",
                "messages": [
                    {"line": 5, "column": 10, "ruleId": "no-unused-vars", "message": "'x' is defined but never used", "severity": 2},
                    {"line": 12, "column": 1, "ruleId": "eqeqeq", "message": "Expected '===' but found '=='", "severity": 1}
                ]
            }
        ]"#;
        let findings = parse_eslint_json(output);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].file, "src/index.ts");
        assert_eq!(findings[0].line, 5);
        assert_eq!(findings[0].severity, FindingSeverity::Error);
        assert_eq!(findings[0].code, "no-unused-vars");
        assert_eq!(findings[1].severity, FindingSeverity::Warning);
    }

    // ── on_failure roundtrip ──────────────────────────────────────────────────

    #[test]
    fn on_failure_serde_roundtrip() {
        let json = serde_json::to_string(&OnFailure::Agent).unwrap();
        assert_eq!(json, "\"agent\"");
        let v: OnFailure = serde_json::from_str("\"warn\"").unwrap();
        assert_eq!(v, OnFailure::Warn);
        let v: OnFailure = serde_json::from_str("\"fail\"").unwrap();
        assert_eq!(v, OnFailure::Fail);
    }

    // ── AnalysisConfig defaults ───────────────────────────────────────────────

    #[test]
    fn analysis_config_defaults() {
        let toml = r#"tool = "mypy""#;
        let cfg: AnalysisConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.tool, "mypy");
        assert!(cfg.args.is_empty());
        assert_eq!(cfg.on_failure, OnFailure::Fail);
        assert_eq!(cfg.max_iterations, 3);
        assert_eq!(cfg.on_max_iterations, OnMaxIterations::Warn);
    }

    #[test]
    fn analysis_config_full() {
        let toml = r#"
tool = "pyright"
args = ["--strict"]
on_failure = "agent"
max_iterations = 5
on_max_iterations = "fail"
"#;
        let cfg: AnalysisConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.args, vec!["--strict"]);
        assert_eq!(cfg.on_failure, OnFailure::Agent);
        assert_eq!(cfg.max_iterations, 5);
        assert_eq!(cfg.on_max_iterations, OnMaxIterations::Fail);
    }

    // ── language detection ────────────────────────────────────────────────────

    #[test]
    fn detect_language_rust() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_language(dir.path()), Some(Language::Rust));
    }

    #[test]
    fn detect_language_go() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com").unwrap();
        assert_eq!(detect_language(dir.path()), Some(Language::Go));
    }

    #[test]
    fn detect_language_python() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();
        assert_eq!(detect_language(dir.path()), Some(Language::Python));
    }

    #[test]
    fn detect_language_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_language(dir.path()), None);
    }

    // ── findings helpers ──────────────────────────────────────────────────────

    #[test]
    fn format_table_produces_markdown() {
        let findings = vec![AnalysisFinding {
            file: "foo.py".to_string(),
            line: 1,
            col: 2,
            code: "E501".to_string(),
            message: "Line too long".to_string(),
            severity: FindingSeverity::Warning,
        }];
        let table = AnalysisFinding::format_table(&findings);
        assert!(table.contains("foo.py"));
        assert!(table.contains("E501"));
        assert!(table.contains("warning"));
    }

    #[test]
    fn affected_files_deduplicates() {
        let findings = vec![
            AnalysisFinding {
                file: "a.py".to_string(),
                line: 1,
                col: 0,
                code: String::new(),
                message: "x".to_string(),
                severity: FindingSeverity::Error,
            },
            AnalysisFinding {
                file: "a.py".to_string(),
                line: 2,
                col: 0,
                code: String::new(),
                message: "y".to_string(),
                severity: FindingSeverity::Error,
            },
            AnalysisFinding {
                file: "b.py".to_string(),
                line: 1,
                col: 0,
                code: String::new(),
                message: "z".to_string(),
                severity: FindingSeverity::Warning,
            },
        ];
        let files = AnalysisFinding::affected_files(&findings);
        assert_eq!(files, vec!["a.py", "b.py"]);
    }

    // ── build_fix_prompt ──────────────────────────────────────────────────────

    #[test]
    fn build_fix_prompt_contains_tool_and_files() {
        let findings = vec![AnalysisFinding {
            file: "main.py".to_string(),
            line: 5,
            col: 0,
            code: "import-untyped".to_string(),
            message: "Missing type stub".to_string(),
            severity: FindingSeverity::Error,
        }];
        let prompt = AnalysisFinding::build_fix_prompt("mypy", &Language::Python, &findings);
        assert!(prompt.contains("mypy"));
        assert!(prompt.contains("python"));
        assert!(prompt.contains("main.py"));
        assert!(prompt.contains("PLAN.md"));
    }

    // ── Language FromStr / Display ────────────────────────────────────────────

    #[test]
    fn language_from_str_and_display() {
        use std::str::FromStr;
        assert_eq!(Language::from_str("rust").unwrap(), Language::Rust);
        assert_eq!(Language::from_str("ts").unwrap(), Language::TypeScript);
        assert_eq!(Language::from_str("py").unwrap(), Language::Python);
        assert_eq!(Language::from_str("golang").unwrap(), Language::Go);
        assert_eq!(Language::Rust.to_string(), "rust");
        assert_eq!(Language::TypeScript.to_string(), "typescript");
    }
}
