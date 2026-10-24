//! First-party input minimizers for the output of the commands TA itself produces and
//! feeds back to an agent: `cargo` build/test/clippy, `git` status/diff/log, and `ta`.
//!
//! Each one is a pure text transform. Every transform keeps what a person fixing a
//! failure needs (errors in full, failing tests, summaries, file locations) and removes
//! what repeats or only shows progress. Nothing is hidden: [`crate::InputOptimizer::apply`]
//! stores the full original first and returns its reference in the metrics record.
//! `lossless` is true only when the output equals the input.

use std::fmt;

/// Which command's output a minimizer understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cargo,
    Git,
    Ta,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Cargo, Kind::Git, Kind::Ta];

    /// The optimizer name this kind is registered under.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Cargo => "cargo",
            Kind::Git => "git",
            Kind::Ta => "ta",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which minimizer a shell command line belongs to. Looks past `VAR=x` prefixes, `./dev`,
/// `nix develop --command` style wrappers, `sudo`, `time` and `env`, and the path of the
/// binary, so `./dev cargo test --workspace` and `/usr/bin/git status` both resolve.
pub fn kind_for_command(command: &str) -> Option<Kind> {
    // Words that only wrap or precede the real command.
    const SKIP: [&str; 10] = [
        "./dev",
        "dev",
        "sudo",
        "time",
        "env",
        "exec",
        "nix",
        "develop",
        "--command",
        "bash",
    ];
    let words = command
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '"' || c == '\''));
    for word in words {
        let is_var_assignment = word.contains('=') && !word.starts_with('-');
        if is_var_assignment || word == "-c" || SKIP.contains(&word) {
            continue;
        }
        let base = word.rsplit('/').next().unwrap_or(word);
        return Kind::ALL.into_iter().find(|k| k.name() == base);
    }
    None
}

/// Best guess at the producing command from the text alone, for callers that have no
/// command line. `None` when nothing in the text is distinctive.
pub fn sniff(text: &str) -> Option<Kind> {
    let head: Vec<&str> = text.lines().take(400).collect();
    let has = |f: &dyn Fn(&str) -> bool| head.iter().any(|l| f(l.trim_start()));
    if has(&|l| {
        l.starts_with("Compiling ")
            || l.starts_with("Checking ")
            || l.starts_with("error[E")
            || l.starts_with("warning: unused")
            || l.starts_with("test result:")
            || l.starts_with("running ") && (l.ends_with(" tests") || l.ends_with(" test"))
    }) {
        return Some(Kind::Cargo);
    }
    if has(&|l| {
        l.starts_with("diff --git ")
            || l.starts_with("On branch ")
            || l.starts_with("Changes not staged")
            || l.starts_with("nothing to commit")
            || (l.starts_with("commit ") && l.len() >= 47)
    }) {
        return Some(Kind::Git);
    }
    if text.contains("\u{1b}[") || text.contains('\r') {
        return Some(Kind::Ta);
    }
    None
}

/// Reduce `text` with the minimizer for `kind`. The result equals `text` when there was
/// nothing to remove.
pub fn minimize(kind: Kind, text: &str) -> String {
    let cleaned = strip_terminal_noise(text);
    match kind {
        Kind::Cargo => cargo(&cleaned),
        Kind::Git => git(&cleaned),
        Kind::Ta => ta(&cleaned),
    }
}

/// Remove ANSI escape sequences, and keep only the final redraw of a `\r` progress line.
fn strip_terminal_noise(text: &str) -> String {
    if !text.contains('\u{1b}') && !text.contains('\r') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' if chars.peek() == Some(&'[') => {
                chars.next();
                // CSI: parameters, then one final byte in 0x40..=0x7e.
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    // `\r\n` is a line ending; a bare `\r` redraws the line, so only the last part survives.
    out.split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.rsplit('\r').next().unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// Keep the trailing-newline shape of the input.
fn finish(out: String, input: &str) -> String {
    if input.ends_with('\n') || out.is_empty() {
        out
    } else {
        out.trim_end_matches('\n').to_string()
    }
}

fn is_cargo_progress(line: &str) -> bool {
    let t = line.trim_start();
    [
        "Compiling ",
        "Checking ",
        "Downloading ",
        "Downloaded ",
        "Fresh ",
        "Blocking ",
        "Locking ",
        "Updating ",
        "Adding ",
        "Documenting ",
    ]
    .iter()
    .any(|p| t.starts_with(p))
        && line.starts_with(' ')
}

fn is_passing_test(line: &str) -> bool {
    line.starts_with("test ") && line.ends_with(" ... ok")
}

/// `cargo build|check|clippy|test`: progress lines and passing-test lines become one count
/// each, and a repeated warning keeps its headline and location only. Errors, failing tests,
/// panics and every summary line stay verbatim.
fn cargo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut progress = 0usize;
    let mut passed = 0usize;
    let mut seen_warnings: std::collections::HashMap<String, usize> = Default::default();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let flush = |out: &mut String, progress: &mut usize, passed: &mut usize| {
        if *progress > 0 {
            push_line(
                out,
                &format!(
                    "[{} crate progress line(s) removed: Compiling/Checking/Downloading]",
                    *progress
                ),
            );
            *progress = 0;
        }
        if *passed > 0 {
            push_line(
                out,
                &format!("[{} passing test line(s) removed: `... ok`]", *passed),
            );
            *passed = 0;
        }
    };
    while i < lines.len() {
        let line = lines[i];
        if is_cargo_progress(line) {
            progress += 1;
            i += 1;
            continue;
        }
        if is_passing_test(line) {
            passed += 1;
            i += 1;
            continue;
        }
        // A warning block runs to the next blank line. Repeats of a headline are cut down.
        if line.starts_with("warning:") && !line.contains(" generated ") {
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim().is_empty() {
                j += 1;
            }
            let count = seen_warnings.entry(line.to_string()).or_insert(0);
            *count += 1;
            flush(&mut out, &mut progress, &mut passed);
            if *count == 1 {
                for l in &lines[i..j] {
                    push_line(&mut out, l);
                }
            } else {
                push_line(&mut out, line);
                if let Some(loc) = lines[i + 1..j]
                    .iter()
                    .find(|l| l.trim_start().starts_with("-->"))
                {
                    push_line(&mut out, loc);
                }
                push_line(&mut out, "   = (same warning as above; body omitted)");
            }
            i = j;
            continue;
        }
        flush(&mut out, &mut progress, &mut passed);
        push_line(&mut out, line);
        i += 1;
    }
    flush(&mut out, &mut progress, &mut passed);
    finish(out, text)
}

/// `git status|diff|log`: usage hints, `index` lines of a diff and the unreadable tail of a
/// full commit hash are removed; paths, hunks and messages stay.
fn git(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("(use \"git ") && t.ends_with(')') {
            continue;
        }
        if line.starts_with("index ") && line.contains("..") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("commit ") {
            let hash: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if hash.len() == 40 {
                push_line(&mut out, &format!("commit {}{}", &hash[..12], &rest[40..]));
                continue;
            }
        }
        push_line(&mut out, line);
    }
    finish(out, text)
}

/// `ta ...`: consecutive identical lines and runs of blank lines collapse to one with a count.
fn ta(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<&str> = None;
    let mut repeats = 0usize;
    let flush = |out: &mut String, prev: Option<&str>, repeats: usize| {
        if let Some(p) = prev {
            push_line(out, p);
            if repeats > 0 && !p.trim().is_empty() {
                push_line(
                    out,
                    &format!("[previous line repeated {repeats} more time(s)]"),
                );
            }
        }
    };
    for line in text.lines() {
        if Some(line) == prev {
            repeats += 1;
            continue;
        }
        flush(&mut out, prev, repeats);
        prev = Some(line);
        repeats = 0;
    }
    flush(&mut out, prev, repeats);
    finish(out, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cargo_failure() -> String {
        let mut s = String::new();
        for c in ["serde", "tokio", "ta-core"] {
            s.push_str(&format!("   Compiling {c} v1.0.0\n"));
        }
        s.push_str("warning: unused variable: `a`\n --> src/a.rs:3:9\n  |\n3 |     let a = 1;\n  |         ^ help: prefix it\n\n");
        s.push_str("warning: unused variable: `a`\n --> src/b.rs:8:9\n  |\n8 |     let a = 2;\n  |         ^ help: prefix it\n\n");
        s.push_str("error[E0308]: mismatched types\n --> src/lib.rs:10:5\n  |\n10 |     1u8\n  |     ^^^ expected `u32`\n\n");
        s.push_str("error: could not compile `ta-core` (lib) due to 1 previous error\n");
        s
    }

    #[test]
    fn commands_resolve_through_wrappers_and_paths() {
        for (cmd, want) in [
            ("cargo test --workspace", Some(Kind::Cargo)),
            (
                "./dev cargo clippy --workspace -- -D warnings",
                Some(Kind::Cargo),
            ),
            ("RUST_LOG=debug /usr/bin/git status", Some(Kind::Git)),
            (
                "nix develop --command bash -c \"cargo build\"",
                Some(Kind::Cargo),
            ),
            ("ta draft view abc", Some(Kind::Ta)),
            ("npm test", None),
            ("", None),
        ] {
            assert_eq!(kind_for_command(cmd), want, "{cmd}");
        }
    }

    #[test]
    fn cargo_keeps_errors_and_summary_and_drops_progress() {
        let input = cargo_failure();
        let out = minimize(Kind::Cargo, &input);
        assert!(out.len() < input.len());
        assert!(out.contains("error[E0308]: mismatched types"));
        assert!(out.contains(" --> src/lib.rs:10:5"));
        assert!(
            out.contains("expected `u32`"),
            "error body must stay verbatim"
        );
        assert!(out.contains("error: could not compile `ta-core` (lib) due to 1 previous error"));
        assert!(!out.contains("Compiling serde"));
        assert!(out.contains("3 crate progress line(s) removed"), "{out}");
    }

    #[test]
    fn a_repeated_warning_keeps_every_location() {
        let out = minimize(Kind::Cargo, &cargo_failure());
        assert!(
            out.contains("--> src/a.rs:3:9") && out.contains("--> src/b.rs:8:9"),
            "{out}"
        );
        assert_eq!(
            out.matches("let a = ").count(),
            1,
            "second body omitted: {out}"
        );
        assert!(out.contains("same warning as above"));
    }

    #[test]
    fn cargo_test_collapses_passing_tests_but_keeps_failures_and_results() {
        let input = "running 4 tests\ntest a ... ok\ntest b ... ok\ntest c ... FAILED\ntest d ... ignored\n\nfailures:\n\n---- c stdout ----\nthread 'c' panicked at src/lib.rs:5:9:\nboom\n\nfailures:\n    c\n\ntest result: FAILED. 2 passed; 1 failed; 1 ignored\n";
        let out = minimize(Kind::Cargo, input);
        assert!(out.contains("2 passing test line(s) removed"), "{out}");
        assert!(out.contains("test c ... FAILED") && out.contains("test d ... ignored"));
        assert!(out.contains("panicked at src/lib.rs:5:9") && out.contains("boom"));
        assert!(out.contains("test result: FAILED. 2 passed; 1 failed; 1 ignored"));
        assert!(!out.contains("test a ... ok"));
    }

    #[test]
    fn output_with_nothing_to_remove_is_unchanged() {
        for (kind, text) in [
            (Kind::Cargo, "error: no such command: `nope`\n"),
            (Kind::Git, "fatal: not a git repository\n"),
            (Kind::Ta, "Goal started\nDraft built\n"),
        ] {
            assert_eq!(minimize(kind, text), text);
        }
        assert_eq!(minimize(Kind::Ta, "no newline at end"), "no newline at end");
    }

    #[test]
    fn git_status_hints_and_diff_index_lines_go_but_paths_and_hunks_stay() {
        let status = "On branch main\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n  (use \"git restore <file>...\" to discard changes in working directory)\n\tmodified:   src/a.rs\n";
        let out = minimize(Kind::Git, status);
        assert!(!out.contains("(use \"git"));
        assert!(out.contains("modified:   src/a.rs") && out.contains("On branch main"));
        let diff = "diff --git a/x b/x\nindex 1234567..89abcde 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        let out = minimize(Kind::Git, diff);
        assert!(!out.contains("index 1234567"));
        assert!(out.contains("@@ -1 +1 @@") && out.contains("-a\n+b"));
    }

    #[test]
    fn git_log_shortens_full_hashes_to_twelve() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let out = minimize(
            Kind::Git,
            &format!("commit {hash} (HEAD -> main)\nAuthor: a\n\n    msg\n"),
        );
        assert!(
            out.starts_with("commit 0123456789ab (HEAD -> main)\n"),
            "{out}"
        );
    }

    #[test]
    fn ta_output_loses_ansi_redraws_and_repeats() {
        let input =
            "\u{1b}[32mok\u{1b}[0m\nbuilding 10%\rbuilding 100%\nwaiting\nwaiting\nwaiting\ndone\n";
        let out = minimize(Kind::Ta, input);
        assert_eq!(
            out,
            "ok\nbuilding 100%\nwaiting\n[previous line repeated 2 more time(s)]\ndone\n"
        );
    }

    #[test]
    fn sniffing_recognises_each_producer_and_is_silent_otherwise() {
        assert_eq!(sniff(&cargo_failure()), Some(Kind::Cargo));
        assert_eq!(
            sniff("On branch main\nnothing to commit\n"),
            Some(Kind::Git)
        );
        assert_eq!(sniff("\u{1b}[1mhello\u{1b}[0m\n"), Some(Kind::Ta));
        assert_eq!(sniff("plain text\n"), None);
    }
}
