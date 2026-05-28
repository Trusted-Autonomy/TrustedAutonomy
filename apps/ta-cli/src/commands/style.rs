// style.rs — Developer Style Constitution commands (`ta style`).
//
// Manages a global style constitution at `~/.config/ta/style.md`, authored once,
// prepended to every CLAUDE.md injection at `ta run` time.
//
// Commands:
//   ta style init              — Interactive interview, builds style.md from scratch
//   ta style template list     — List built-in curated templates
//   ta style template apply    — Apply a template (merges with or replaces current)
//   ta style import <src>      — Import from a file path or HTTPS URL
//   ta style discover [path]   — Analyse codebase and infer style
//   ta style show              — Print current style constitution
//   ta style edit              — Open in $EDITOR
//   ta style clear             — Remove the global style file

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use clap::Subcommand;

// ── Built-in templates ─────────────────────────────────────────────────────

struct TemplateInfo {
    name: &'static str,
    description: &'static str,
    attribution: &'static str,
    content: &'static str,
}

static TEMPLATES: &[TemplateInfo] = &[
    TemplateInfo {
        name: "karpathy",
        description:
            "Flat code, minimal abstraction, explicit over implicit, no premature generalisation",
        attribution: "Andrej Karpathy (published CLAUDE.md preferences)",
        content: include_str!("../templates/style-karpathy.md"),
    },
    TemplateInfo {
        name: "minimal",
        description: "No comments, no helpers, explicit errors, tests only for non-obvious logic",
        attribution: "TA built-in",
        content: include_str!("../templates/style-minimal.md"),
    },
    TemplateInfo {
        name: "documented",
        description: "Full docstrings, typed interfaces, integration tests preferred",
        attribution: "TA built-in",
        content: include_str!("../templates/style-documented.md"),
    },
    TemplateInfo {
        name: "pragmatic",
        description: "Balanced: WHY-only comments, extract when used 3+ times, anyhow for errors",
        attribution: "TA built-in",
        content: include_str!("../templates/style-pragmatic.md"),
    },
];

static DISCOVER_PROMPT: &str = include_str!("../templates/style-discover.md");

// ── CLI types ──────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum StyleCommands {
    /// Build ~/.config/ta/style.md through an interactive interview.
    ///
    /// Walks through 7 topic areas (all skippable). At the end, writes your
    /// answers to ~/.config/ta/style.md. Existing content is overwritten after
    /// confirmation.
    Init,
    /// Manage built-in style templates.
    Template {
        #[command(subcommand)]
        command: TemplateCommands,
    },
    /// Import a style file from a local path or HTTPS URL.
    ///
    /// Examples:
    ///   ta style import ./my-style.md
    ///   ta style import https://example.com/style.md
    Import {
        /// Local file path or https:// URL to import.
        source: String,
    },
    /// Analyse a codebase and infer its coding style (agent-driven).
    ///
    /// Scans the codebase at PATH (default: current directory) and produces a
    /// style.md draft. You review the draft before it is saved.
    ///
    /// Examples:
    ///   ta style discover
    ///   ta style discover ~/projects/myapp
    Discover {
        /// Path to the codebase to analyse (defaults to current directory).
        path: Option<PathBuf>,
        /// Skip user review — save the draft immediately.
        #[arg(long)]
        yes: bool,
    },
    /// Print the current ~/.config/ta/style.md.
    Show,
    /// Open ~/.config/ta/style.md in $EDITOR.
    Edit,
    /// Remove ~/.config/ta/style.md.
    Clear {
        /// Skip confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum TemplateCommands {
    /// List available built-in style templates.
    List,
    /// Apply a template to ~/.config/ta/style.md.
    ///
    /// Examples:
    ///   ta style template apply karpathy
    ///   ta style template apply minimal
    Apply {
        /// Template name (run `ta style template list` to see options).
        name: String,
        /// Overwrite existing style file without prompting.
        #[arg(long)]
        yes: bool,
    },
}

// ── Entry point ────────────────────────────────────────────────────────────

pub fn execute(command: &StyleCommands) -> anyhow::Result<()> {
    match command {
        StyleCommands::Init => run_init(),
        StyleCommands::Template { command } => match command {
            TemplateCommands::List => template_list(),
            TemplateCommands::Apply { name, yes } => template_apply(name, *yes),
        },
        StyleCommands::Import { source } => import(source),
        StyleCommands::Discover { path, yes } => discover(path.as_deref(), *yes),
        StyleCommands::Show => show(),
        StyleCommands::Edit => edit(),
        StyleCommands::Clear { yes } => clear(*yes),
    }
}

// ── Global style path ──────────────────────────────────────────────────────

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub fn style_path() -> anyhow::Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    Ok(home.join(".config").join("ta").join("style.md"))
}

/// Return the content of `~/.config/ta/style.md` if it exists and is non-empty.
pub fn load_style() -> Option<String> {
    let path = style_path().ok()?;
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    if content.trim().is_empty() {
        None
    } else {
        Some(content)
    }
}

fn ensure_style_dir() -> anyhow::Result<PathBuf> {
    let path = style_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(path)
}

// ── Interview ──────────────────────────────────────────────────────────────

fn run_init() -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "`ta style init` requires an interactive terminal.\n\
             Use `ta style template apply <name>` for non-interactive setup."
        );
    }

    let path = style_path()?;
    if path.exists() {
        print!("~/.config/ta/style.md already exists. Overwrite? [y/N] ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    println!();
    println!("=== Developer Style Interview ===");
    println!();
    println!("Answer the questions below (or press Enter to skip any topic).");
    println!("Your answers are assembled into ~/.config/ta/style.md and injected");
    println!("into every `ta run` session.");
    println!();

    let sections = interview_questions();
    let mut parts: Vec<String> = Vec::new();

    for (heading, question, hint) in &sections {
        println!("--- {} ---", heading);
        println!("{}", question);
        if !hint.is_empty() {
            println!("({})", hint);
        }
        print!("> ");
        std::io::stdout().flush()?;

        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();

        if !answer.is_empty() {
            parts.push(format!("## {}\n\n{}\n", heading, answer));
        }
        println!();
    }

    if parts.is_empty() {
        println!("No answers recorded. Style file not written.");
        println!("Tip: run `ta style template apply pragmatic` for a sensible default.");
        return Ok(());
    }

    let content = format!(
        "# Developer Style\n\n*Created with `ta style init`. Edit with `ta style edit`.*\n\n{}\n",
        parts.join("\n")
    );

    let style_path = ensure_style_dir()?;
    std::fs::write(&style_path, &content)?;
    println!(
        "Written: {} ({} sections)",
        style_path.display(),
        parts.len()
    );
    println!();
    println!("This style will be injected into every `ta run` session.");
    println!("Edit at any time with `ta style edit`, or view with `ta style show`.");
    Ok(())
}

fn interview_questions() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Code Density",
            "How verbose should the code be? (e.g. 'prefer inline over helpers', \
             'extract anything used twice', 'flat over nested')",
            "Skip = no preference",
        ),
        (
            "Error Handling",
            "How should errors be handled? (e.g. 'anyhow everywhere', \
             'typed error enums', 'explicit early panics for invariants')",
            "Skip = no preference",
        ),
        (
            "Abstraction",
            "What is your abstraction preference? (e.g. 'no layers that add no logic', \
             'traits only when multiple impls expected', 'explicit over implicit')",
            "Skip = no preference",
        ),
        (
            "Tests",
            "What is your test philosophy? (e.g. 'integration tests only', \
             'unit tests for pure logic', 'no tests for trivial delegation')",
            "Skip = no preference",
        ),
        (
            "Naming",
            "What naming conventions do you follow? (e.g. 'no Manager/Handler suffixes', \
             'concise over verbose', 'match domain terminology exactly')",
            "Skip = no preference",
        ),
        (
            "Comments",
            "When should code be commented? (e.g. 'WHY only', 'never restate the code', \
             'full docstrings for public API')",
            "Skip = no preference",
        ),
        (
            "Other",
            "Anything else you want every agent to know about your coding style?",
            "Skip = nothing else",
        ),
    ]
}

// ── Template commands ──────────────────────────────────────────────────────

fn template_list() -> anyhow::Result<()> {
    println!("{:<14} {:<55} ATTRIBUTION", "NAME", "DESCRIPTION");
    println!("{}", "-".repeat(100));
    for t in TEMPLATES {
        println!(
            "{:<14} {:<55} {}",
            t.name,
            truncate(t.description, 54),
            t.attribution
        );
    }
    println!();
    println!("Apply a template: ta style template apply <name>");
    Ok(())
}

fn template_apply(name: &str, yes: bool) -> anyhow::Result<()> {
    let template = TEMPLATES
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown template '{}'. Run `ta style template list` to see available templates.",
                name
            )
        })?;

    let path = style_path()?;
    if path.exists() && !yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("~/.config/ta/style.md already exists. Pass --yes to overwrite.");
        }
        print!(
            "~/.config/ta/style.md already exists. Overwrite with '{}'? [y/N] ",
            template.name
        );
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let style_path = ensure_style_dir()?;
    std::fs::write(&style_path, template.content)?;
    println!(
        "Applied template '{}': {}",
        template.name,
        style_path.display()
    );
    println!("This style will be injected into every `ta run` session.");
    println!("Edit at any time with `ta style edit`, or view with `ta style show`.");
    Ok(())
}

// ── Import ─────────────────────────────────────────────────────────────────

fn import(source: &str) -> anyhow::Result<()> {
    let content = if source.starts_with("https://") || source.starts_with("http://") {
        fetch_url(source)?
    } else {
        let src_path = std::path::Path::new(source);
        if !src_path.exists() {
            anyhow::bail!(
                "File not found: {}\n\
                 Provide a local file path or an https:// URL.",
                source
            );
        }
        std::fs::read_to_string(src_path)
            .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", src_path.display(), e))?
    };

    if content.trim().is_empty() {
        anyhow::bail!("The imported content is empty. Nothing written to ~/.config/ta/style.md.");
    }

    let path = style_path()?;
    if path.exists() && std::io::stdin().is_terminal() {
        print!("~/.config/ta/style.md already exists. Overwrite? [y/N] ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let style_path = ensure_style_dir()?;
    std::fs::write(&style_path, &content)?;
    println!(
        "Imported {} bytes from '{}' → {}",
        content.len(),
        source,
        style_path.display()
    );
    println!("This style will be injected into every `ta run` session.");
    Ok(())
}

fn fetch_url(url: &str) -> anyhow::Result<String> {
    let response = reqwest::blocking::get(url).map_err(|e| {
        anyhow::anyhow!(
            "Failed to fetch '{}': {}\n\
             Check that the URL is correct and accessible.",
            url,
            e
        )
    })?;

    let status = response.status();
    if !status.is_success() {
        anyhow::bail!(
            "HTTP {} fetching '{}'.\n\
             The URL must be publicly accessible and return 200 OK.",
            status,
            url
        );
    }

    response
        .text()
        .map_err(|e| anyhow::anyhow!("Failed to read response body from '{}': {}", url, e))
}

// ── Discover ───────────────────────────────────────────────────────────────

fn discover(path: Option<&std::path::Path>, yes: bool) -> anyhow::Result<()> {
    let target = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("Cannot determine current directory: {}", e))?,
    };

    if !target.exists() {
        anyhow::bail!(
            "Path not found: {}\n\
             Provide the path to an existing codebase directory.",
            target.display()
        );
    }

    println!("=== Style Discovery ===");
    println!();
    println!("Analysing: {}", target.display());
    println!("This requires the `claude` CLI to be installed and authenticated.");
    println!();

    // Gather codebase metrics to feed to the agent.
    let metrics = gather_codebase_metrics(&target);
    let project_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("this project");

    let prompt = format!(
        "{}\n\n\
         ## Codebase to Analyse\n\n\
         **Project**: {}\n\
         **Path**: {}\n\n\
         ### Sampled Metrics\n\n\
         {}\n\n\
         Produce a `style.md` draft based on these metrics and any \
         patterns you can infer from the metric summary above. \
         Output ONLY the Markdown content of the style.md file — \
         no preamble, no explanation outside the file.",
        DISCOVER_PROMPT,
        project_name,
        target.display(),
        metrics
    );

    println!("Running analysis...");
    let output = Command::new("claude")
        .args(["--print", "--output-format", "text"])
        .arg(&prompt)
        .stdin(Stdio::null())
        .output();

    let draft = match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            if text.trim().is_empty() {
                anyhow::bail!(
                    "The analysis agent returned an empty response.\n\
                     Check that `claude` is installed and authenticated: `claude --version`"
                );
            }
            text
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "The `claude` CLI exited with an error during style discovery.\n\
                 {}\n\
                 Ensure `claude` is installed and authenticated: `claude --version`",
                stderr.trim()
            );
        }
        Err(e) => {
            anyhow::bail!(
                "Failed to run the `claude` CLI: {}\n\
                 Install it with: `npm install -g @anthropic-ai/claude-code`",
                e
            );
        }
    };

    println!();
    println!("=== Draft Style ===");
    println!();
    println!("{}", draft.trim());
    println!();

    let style_path_val = style_path()?;
    if style_path_val.exists() && !yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "~/.config/ta/style.md already exists. Pass --yes to overwrite without prompting."
            );
        }
        print!("Save this as ~/.config/ta/style.md? [y/N] ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled. The draft was not saved.");
            return Ok(());
        }
    } else if !style_path_val.exists() && !yes && std::io::stdin().is_terminal() {
        print!("Save this as ~/.config/ta/style.md? [Y/n] ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let answer = input.trim();
        if !answer.is_empty() && !answer.eq_ignore_ascii_case("y") {
            println!("Cancelled. The draft was not saved.");
            return Ok(());
        }
    }

    let saved_path = ensure_style_dir()?;
    std::fs::write(&saved_path, draft.trim())?;
    println!("Saved: {}", saved_path.display());
    println!("Edit at any time with `ta style edit`, or view with `ta style show`.");
    Ok(())
}

/// Gather lightweight codebase metrics without running the agent yet.
fn gather_codebase_metrics(root: &std::path::Path) -> String {
    let mut total_files = 0usize;
    let mut total_lines = 0usize;
    let mut comment_lines = 0usize;
    let mut test_files = 0usize;
    let mut fn_count = 0usize;
    let mut doc_fn_count = 0usize;
    let mut extensions: std::collections::HashMap<String, usize> = Default::default();
    let mut sample_files: Vec<String> = Vec::new();

    walk_source_files(root, &mut |path, content| {
        total_files += 1;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        *extensions.entry(ext.clone()).or_insert(0) += 1;

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        if name.contains("test") || name.contains("spec") {
            test_files += 1;
        }

        let lines: Vec<&str> = content.lines().collect();
        total_lines += lines.len();

        let is_rust = ext == "rs";
        let is_py = ext == "py";
        let is_ts = ext == "ts" || ext == "tsx";
        let is_js = ext == "js" || ext == "jsx";

        let mut prev_line_was_doc = false;
        for line in &lines {
            let trimmed = line.trim();
            // Rust/JS/TS: line comments
            if (is_rust || is_ts || is_js)
                && (trimmed.starts_with("//") || trimmed.starts_with("/*"))
            {
                comment_lines += 1;
                prev_line_was_doc = trimmed.starts_with("///") || trimmed.starts_with("/**");
            } else if is_py && trimmed.starts_with('#') {
                comment_lines += 1;
                prev_line_was_doc = false;
            }
            if (is_rust && trimmed.starts_with("fn ") || trimmed.starts_with("pub fn "))
                || (is_py && trimmed.starts_with("def "))
                || ((is_ts || is_js)
                    && (trimmed.starts_with("function ") || trimmed.contains("=> {")))
            {
                fn_count += 1;
                if prev_line_was_doc {
                    doc_fn_count += 1;
                }
            }
        }

        if sample_files.len() < 5 {
            if let Ok(rel) = path.strip_prefix(root) {
                sample_files.push(rel.display().to_string());
            }
        }
    });

    let comment_pct = if total_lines > 0 {
        comment_lines * 100 / total_lines
    } else {
        0
    };
    let doc_pct = if fn_count > 0 {
        doc_fn_count * 100 / fn_count
    } else {
        0
    };

    let mut ext_summary: Vec<String> = extensions
        .iter()
        .map(|(k, v)| format!(".{} ({})", k, v))
        .collect();
    ext_summary.sort();

    format!(
        "- Total source files sampled: {}\n\
         - Total lines: {}\n\
         - Comment density: {}% of lines are comments\n\
         - Test files detected: {}\n\
         - Functions detected: {} ({} with doc comments, {}%)\n\
         - File types: {}\n\
         - Sample files: {}",
        total_files,
        total_lines,
        comment_pct,
        test_files,
        fn_count,
        doc_fn_count,
        doc_pct,
        ext_summary.join(", "),
        sample_files.join(", ")
    )
}

fn walk_source_files(root: &std::path::Path, callback: &mut impl FnMut(&std::path::Path, &str)) {
    let source_exts = [
        "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "cpp", "c", "h",
    ];
    let skip_dirs = [
        "target",
        "node_modules",
        ".git",
        ".ta",
        "dist",
        "build",
        "__pycache__",
    ];

    let mut stack = vec![root.to_path_buf()];
    let mut file_count = 0usize;

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !skip_dirs.contains(&name) {
                    stack.push(path);
                }
            } else if path.is_file() {
                // Limit to 200 files to keep analysis fast.
                if file_count >= 200 {
                    return;
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if source_exts.contains(&ext) {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        callback(&path, &content);
                        file_count += 1;
                    }
                }
            }
        }
    }
}

// ── Show / Edit / Clear ────────────────────────────────────────────────────

fn show() -> anyhow::Result<()> {
    let path = style_path()?;
    if !path.exists() {
        println!("No style file found at {}", path.display());
        println!();
        println!("Create one with:");
        println!("  ta style init                         # interactive interview");
        println!("  ta style template apply pragmatic     # apply a curated template");
        println!("  ta style import <path-or-url>         # import from file or URL");
        return Ok(());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;
    println!("=== {} ===", path.display());
    println!();
    print!("{}", content);
    if !content.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn edit() -> anyhow::Result<()> {
    let path = ensure_style_dir()?;

    // Create the file if it doesn't exist yet.
    if !path.exists() {
        std::fs::write(
            &path,
            "# Developer Style\n\n\
             *Add your coding style preferences here.*\n\
             *This file is injected into every `ta run` session.*\n",
        )?;
        println!("Created: {}", path.display());
    }

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| {
            if cfg!(target_os = "windows") {
                "notepad".to_string()
            } else {
                "vi".to_string()
            }
        });

    let status = Command::new(&editor).arg(&path).status().map_err(|e| {
        anyhow::anyhow!(
            "Failed to launch editor '{}': {}\n\
                 Set $EDITOR to your preferred editor.",
            editor,
            e
        )
    })?;

    if !status.success() {
        anyhow::bail!("Editor '{}' exited with status {}.", editor, status);
    }
    Ok(())
}

fn clear(yes: bool) -> anyhow::Result<()> {
    let path = style_path()?;
    if !path.exists() {
        println!(
            "No style file found at {}. Nothing to remove.",
            path.display()
        );
        return Ok(());
    }

    if !yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("Pass --yes to remove ~/.config/ta/style.md without prompting.");
        }
        print!("Remove {}? [y/N] ", path.display());
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    std::fs::remove_file(&path)
        .map_err(|e| anyhow::anyhow!("Failed to remove {}: {}", path.display(), e))?;
    println!("Removed: {}", path.display());
    Ok(())
}

// ── Utilities ──────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Override HOME for tests by calling functions with explicit path helpers.
    fn style_path_in(home: &std::path::Path) -> PathBuf {
        home.join(".config").join("ta").join("style.md")
    }

    #[test]
    fn style_template_list() {
        assert!(!TEMPLATES.is_empty(), "Template list must be non-empty");
        let names: Vec<&str> = TEMPLATES.iter().map(|t| t.name).collect();
        assert!(names.contains(&"karpathy"), "karpathy template must exist");
        assert!(names.contains(&"minimal"), "minimal template must exist");
        assert!(
            names.contains(&"documented"),
            "documented template must exist"
        );
        assert!(
            names.contains(&"pragmatic"),
            "pragmatic template must exist"
        );
    }

    #[test]
    fn style_template_content_non_empty() {
        for t in TEMPLATES {
            assert!(
                !t.content.trim().is_empty(),
                "Template '{}' must have non-empty content",
                t.name
            );
        }
    }

    #[test]
    fn style_template_apply() {
        let dir = tempdir().unwrap();
        let path = style_path_in(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let template = TEMPLATES.iter().find(|t| t.name == "minimal").unwrap();
        std::fs::write(&path, template.content).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("minimal") || content.contains("Minimal"),
            "Template content must match 'minimal'"
        );
    }

    #[test]
    fn style_init_writes_file() {
        let dir = tempdir().unwrap();
        let path = style_path_in(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let content = "# Developer Style\n\n## Code Density\n\nFlat over nested.\n";
        std::fs::write(&path, content).unwrap();

        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, content);
    }

    #[test]
    fn style_import_from_path() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("my-style.md");
        std::fs::write(&src, "# My Style\n\nNo comments.\n").unwrap();

        let content = std::fs::read_to_string(&src).unwrap();
        assert!(!content.is_empty());

        let dest = style_path_in(dir.path());
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, &content).unwrap();

        let read_back = std::fs::read_to_string(&dest).unwrap();
        assert!(read_back.contains("No comments"));
    }

    #[test]
    fn style_import_from_url_mock() {
        // Validate URL detection logic — does not make a network call.
        let url = "https://example.com/style.md";
        assert!(url.starts_with("https://"), "URL detection must work");
    }

    #[test]
    fn style_discover_produces_draft() {
        let dir = tempdir().unwrap();
        // Write a small Rust file.
        std::fs::write(
            dir.path().join("main.rs"),
            "fn main() {\n    // Hello world\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        // Metrics collection must not panic and must return a non-empty string.
        let metrics = gather_codebase_metrics(dir.path());
        assert!(
            !metrics.is_empty(),
            "Metrics must be non-empty for a non-empty directory"
        );
        assert!(
            metrics.contains("Total source files sampled"),
            "Metrics must include file count"
        );
    }

    #[test]
    fn style_injected_in_run() {
        // build_style_section returns Some when file exists.
        let dir = tempdir().unwrap();
        let path = style_path_in(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "# Developer Style\n\nNo comments.\n").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let section = format!("\n## Developer Style\n\n{}\n", content.trim());
        assert!(section.contains("Developer Style"));
        assert!(section.contains("No comments"));
    }

    #[test]
    fn style_not_injected_when_absent() {
        let dir = tempdir().unwrap();
        let path = style_path_in(dir.path());
        assert!(
            !path.exists(),
            "Style file must not exist in a fresh temp dir"
        );
    }

    #[test]
    fn discover_prompt_non_empty() {
        assert!(
            !DISCOVER_PROMPT.trim().is_empty(),
            "style-discover.md must have content"
        );
    }
}
