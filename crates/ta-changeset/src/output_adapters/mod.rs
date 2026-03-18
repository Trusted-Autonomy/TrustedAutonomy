//! output_adapters — Pluggable output renderers for draft review (v0.2.3).
//!
//! Output adapters transform DraftPackage data into different formats for review:
//! - **Terminal**: Colored inline diff with tiered display (default)
//! - **Markdown**: GitHub-ready markdown with collapsible sections
//! - **JSON**: Machine-readable structured output for CI/CD
//! - **HTML**: Standalone review page with progressive disclosure

use crate::draft_package::DraftPackage;
use crate::error::ChangeSetError;

pub mod html;
pub mod json;
pub mod markdown;
pub mod terminal;

/// Output format for PR rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Terminal,
    Markdown,
    Json,
    Html,
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "terminal" => Ok(OutputFormat::Terminal),
            "markdown" | "md" => Ok(OutputFormat::Markdown),
            "json" => Ok(OutputFormat::Json),
            "html" => Ok(OutputFormat::Html),
            _ => Err(format!(
                "Invalid output format: '{}'. Valid formats: terminal, markdown, json, html",
                s
            )),
        }
    }
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Terminal => write!(f, "terminal"),
            OutputFormat::Markdown => write!(f, "markdown"),
            OutputFormat::Json => write!(f, "json"),
            OutputFormat::Html => write!(f, "html"),
        }
    }
}

/// Detail level for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailLevel {
    /// Top: One-line summaries only.
    Top,
    /// Medium: Summary + explanation (no diffs). Default.
    Medium,
    /// Full: Everything including full diffs.
    Full,
}

impl std::str::FromStr for DetailLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "top" => Ok(DetailLevel::Top),
            "medium" | "med" => Ok(DetailLevel::Medium),
            "full" => Ok(DetailLevel::Full),
            _ => Err(format!(
                "Invalid detail level: '{}'. Valid levels: top, medium, full",
                s
            )),
        }
    }
}

impl std::fmt::Display for DetailLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DetailLevel::Top => write!(f, "top"),
            DetailLevel::Medium => write!(f, "medium"),
            DetailLevel::Full => write!(f, "full"),
        }
    }
}

/// Context for rendering a PR package.
pub struct RenderContext<'a> {
    pub package: &'a DraftPackage,
    pub detail_level: DetailLevel,
    /// Optional: Filter to a specific file (show only one artifact).
    pub file_filter: Option<String>,
    /// Optional: Diff content provider (for fetching full diffs).
    pub diff_provider: Option<&'a dyn DiffProvider>,
}

/// Trait for fetching diff content.
///
/// Adapters use this to lazily load full diffs when needed (DetailLevel::Full).
pub trait DiffProvider {
    fn get_diff(&self, diff_ref: &str) -> Result<String, ChangeSetError>;
}

/// Output adapter trait — renders draft packages in different formats.
pub trait OutputAdapter {
    /// Render the draft package to a string.
    fn render(&self, ctx: &RenderContext) -> Result<String, ChangeSetError>;

    /// Adapter name (for logging/debugging).
    fn name(&self) -> &str;
}

/// Generate a sensible default summary when no explanation or rationale is provided.
pub fn default_summary<'a>(uri: &str, change_type: &crate::pr_package::ChangeType) -> &'a str {
    let path = uri.strip_prefix("fs://workspace/").unwrap_or(uri);

    // Lockfiles
    if path.ends_with("Cargo.lock")
        || path.ends_with("package-lock.json")
        || path.ends_with("yarn.lock")
        || path.ends_with("pnpm-lock.yaml")
        || path.ends_with("Gemfile.lock")
        || path.ends_with("poetry.lock")
    {
        return "lockfile updated (dependency changes)";
    }

    // Config / manifest files
    if path.ends_with("Cargo.toml")
        || path.ends_with("package.json")
        || path.ends_with("pyproject.toml")
    {
        return "project configuration updated";
    }

    // Plan / docs
    if path.ends_with("PLAN.md") || path.ends_with("CHANGELOG.md") {
        return "project documentation updated";
    }
    if path.ends_with("README.md") {
        return "readme updated";
    }

    // By change type
    match change_type {
        crate::pr_package::ChangeType::Add => "new file",
        crate::pr_package::ChangeType::Delete => "file removed",
        crate::pr_package::ChangeType::Rename => "file renamed",
        crate::pr_package::ChangeType::Modify => "modified",
    }
}

/// Get an adapter instance for the given format.
///
/// The `color` parameter controls ANSI color output for the terminal adapter.
/// It is ignored for other formats.
pub fn get_adapter(format: OutputFormat, color: bool) -> Box<dyn OutputAdapter> {
    match format {
        OutputFormat::Terminal => Box::new(terminal::TerminalAdapter::with_color(color)),
        OutputFormat::Markdown => Box::new(markdown::MarkdownAdapter::new()),
        OutputFormat::Json => Box::new(json::JsonAdapter::new()),
        OutputFormat::Html => Box::new(html::HtmlAdapter::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_from_str() {
        assert_eq!(
            "terminal".parse::<OutputFormat>().unwrap(),
            OutputFormat::Terminal
        );
        assert_eq!(
            "markdown".parse::<OutputFormat>().unwrap(),
            OutputFormat::Markdown
        );
        assert_eq!(
            "md".parse::<OutputFormat>().unwrap(),
            OutputFormat::Markdown
        );
        assert_eq!("json".parse::<OutputFormat>().unwrap(), OutputFormat::Json);
        assert_eq!("html".parse::<OutputFormat>().unwrap(), OutputFormat::Html);
        assert!("invalid".parse::<OutputFormat>().is_err());
    }

    #[test]
    fn detail_level_from_str() {
        assert_eq!("top".parse::<DetailLevel>().unwrap(), DetailLevel::Top);
        assert_eq!(
            "medium".parse::<DetailLevel>().unwrap(),
            DetailLevel::Medium
        );
        assert_eq!("med".parse::<DetailLevel>().unwrap(), DetailLevel::Medium);
        assert_eq!("full".parse::<DetailLevel>().unwrap(), DetailLevel::Full);
        assert!("invalid".parse::<DetailLevel>().is_err());
    }

    #[test]
    fn output_format_display() {
        assert_eq!(OutputFormat::Terminal.to_string(), "terminal");
        assert_eq!(OutputFormat::Markdown.to_string(), "markdown");
        assert_eq!(OutputFormat::Json.to_string(), "json");
        assert_eq!(OutputFormat::Html.to_string(), "html");
    }

    #[test]
    fn detail_level_display() {
        assert_eq!(DetailLevel::Top.to_string(), "top");
        assert_eq!(DetailLevel::Medium.to_string(), "medium");
        assert_eq!(DetailLevel::Full.to_string(), "full");
    }
}
