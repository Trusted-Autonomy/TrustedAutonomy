//! `ta cost report`: spend across TA goals and interactive agent sessions.
//!
//! The reading, deduplication, pricing and grouping live in
//! `ta_goal::cost_report`; this file finds the sources, applies the flags and
//! prints the result. Each source is optional: a missing directory is named in
//! the header, never an error.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Subcommand;
use ta_goal::cost_report::{
    build_report, parse_cost_group_by, scan_claude, scan_omp, CostFilter, CostGroupRow, CostReport,
    CostTotals, Scan,
};
use ta_goal::spend::parse_time_bound;
use ta_goal::{GoalRunStore, PriceTable};
use ta_mcp_gateway::GatewayConfig;

#[derive(Debug, Subcommand)]
pub enum CostCommands {
    /// Report spend across TA goals, Claude Code sessions and OMP sessions.
    ///
    /// Sources (each optional, auto-detected): this project's TA goals,
    /// ~/.claude/projects/**/*.jsonl and ~/.omp/agent/sessions/**/*.jsonl.
    /// Costs are `exact` when the runtime or OMP reported them and `estimated`
    /// when priced from the price table (.ta/pricing.toml or built in). A model
    /// with no price is counted in tokens and never given an invented price.
    Report {
        /// Only rows at or after this time (RFC 3339 or YYYY-MM-DD, UTC).
        #[arg(long)]
        since: Option<String>,
        /// Only rows before this time (RFC 3339 or YYYY-MM-DD, UTC).
        #[arg(long)]
        until: Option<String>,
        /// Group by a comma-separated list of: day, source, model, project,
        /// context_bucket, origin_class.
        #[arg(long, default_value = "source")]
        group_by: String,
        /// Only projects whose name contains this text (case-insensitive).
        #[arg(long)]
        project: Option<String>,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// Claude Code projects directory (default: ~/.claude/projects).
        #[arg(long, value_name = "DIR")]
        claude_dir: Option<PathBuf>,
        /// OMP sessions directory (default: ~/.omp/agent/sessions).
        #[arg(long, value_name = "DIR")]
        omp_dir: Option<PathBuf>,
    },
}

pub fn execute(cmd: &CostCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        CostCommands::Report {
            since,
            until,
            group_by,
            project,
            json,
            claude_dir,
            omp_dir,
        } => {
            let home = dirs::home_dir();
            let claude = claude_dir
                .clone()
                .or_else(|| home.as_ref().map(|h| h.join(".claude").join("projects")))
                .unwrap_or_else(|| PathBuf::from(".claude/projects"));
            let omp = omp_dir
                .clone()
                .or_else(|| {
                    home.as_ref()
                        .map(|h| h.join(".omp").join("agent").join("sessions"))
                })
                .unwrap_or_else(|| PathBuf::from(".omp/agent/sessions"));
            let report = run_report(
                config,
                since.as_deref(),
                until.as_deref(),
                group_by,
                project.as_deref(),
                &claude,
                &omp,
            )?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", render(&report));
            }
            Ok(())
        }
    }
}

/// Validate the flags, read every source and build the report.
pub fn run_report(
    config: &GatewayConfig,
    since: Option<&str>,
    until: Option<&str>,
    group_by: &str,
    project: Option<&str>,
    claude_dir: &Path,
    omp_dir: &Path,
) -> anyhow::Result<CostReport> {
    let group_by = parse_cost_group_by(group_by).map_err(anyhow::Error::msg)?;
    let bound = |name: &str, v: Option<&str>| -> anyhow::Result<_> {
        v.map(|s| parse_time_bound(s).map_err(|e| anyhow::anyhow!("--{name}: {e}")))
            .transpose()
    };
    let since = bound("since", since)?;
    let until = bound("until", until)?;
    if let (Some(s), Some(u)) = (since, until) {
        anyhow::ensure!(
            s < u,
            "--since ({s}) must be before --until ({u}). Swap them or widen the window."
        );
    }
    let filter = CostFilter {
        since,
        until,
        project: project.map(str::to_string),
    };

    let table = PriceTable::load(&config.workspace_root);
    let project_name = config
        .workspace_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let goals = if config.goals_dir.is_dir() {
        let list = GoalRunStore::new(&config.goals_dir)
            .and_then(|s| s.list())
            .with_context(|| {
                format!(
                    "could not read goal records in {}. Check the directory is readable, or run \
                     from the project root",
                    config.goals_dir.display()
                )
            })?;
        Some(
            list.into_iter()
                .map(|g| (project_name.clone(), g))
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let scans: Vec<Scan> = vec![scan_claude(claude_dir, &table), scan_omp(omp_dir, &table)];
    Ok(build_report(goals.as_deref(), scans, &group_by, &filter))
}

fn tok(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn cost_mix(t: &CostTotals) -> String {
    let mut parts = Vec::new();
    if t.exact_rows > 0 {
        parts.push(format!("{} exact", t.exact_rows));
    }
    if t.estimated_rows > 0 {
        parts.push(format!("{} est", t.estimated_rows));
    }
    if t.unpriced_rows > 0 {
        parts.push(format!("{} unpriced", t.unpriced_rows));
    }
    parts.join(", ")
}

fn table(label_header: &str, rows: &[CostGroupRow], total: Option<&CostTotals>) -> String {
    let label = |g: &CostGroupRow| {
        if g.group.is_empty() {
            "all".to_string()
        } else {
            g.group.values().cloned().collect::<Vec<_>>().join(" / ")
        }
    };
    let width = rows
        .iter()
        .map(|g| label(g).chars().count())
        .chain([label_header.chars().count(), 5])
        .max()
        .unwrap_or(5);
    let mut out = format!(
        "{:<w$}  {:>7} {:>6} {:>9} {:>9} {:>9} {:>9} {:>10}  {}\n",
        label_header,
        "turns",
        "sess",
        "input",
        "output",
        "cache-rd",
        "cache-wr",
        "usd",
        "cost",
        w = width
    );
    let line = |name: &str, t: &CostTotals| {
        format!(
            "{:<w$}  {:>7} {:>6} {:>9} {:>9} {:>9} {:>9} {:>10.2}  {}\n",
            name,
            t.rows,
            t.sessions,
            tok(t.input_tokens),
            tok(t.output_tokens),
            tok(t.cache_read_tokens),
            tok(t.cache_creation_tokens),
            t.usd,
            cost_mix(t),
            w = width
        )
    };
    for g in rows {
        out.push_str(&line(&label(g), &g.totals));
    }
    if let Some(t) = total {
        out.push_str(&line("TOTAL", t));
    }
    out
}

/// The terminal form of a report.
pub fn render(r: &CostReport) -> String {
    let mut out = String::from("Cost report\n");
    out.push_str("Sources:\n");
    match &r.goals {
        Some(g) => out.push_str(&format!(
            "  ta_goals     found: {} goal(s) in the window ({} exact, {} estimated, {} unpriced)\n",
            g.totals.goals,
            g.totals.exact_cost_goals,
            g.totals.estimated_cost_goals,
            g.totals.unpriced_goals
        )),
        None => out.push_str(
            "  ta_goals     missing: no .ta/goals directory here (run from a TA project root)\n",
        ),
    }
    for s in &r.sources {
        if s.found {
            out.push_str(&format!(
                "  {:<12} found: {} file(s), {} turn(s) kept, {} duplicate row(s) folded, {} synthetic skipped  [{}]\n",
                s.source, s.files, s.rows, s.deduplicated, s.synthetic_skipped, s.path
            ));
            if s.unparsable_lines > 0 {
                out.push_str(&format!(
                    "               {} line(s) were not valid JSON and were skipped\n",
                    s.unparsable_lines
                ));
            }
            for f in &s.unreadable_files {
                out.push_str(&format!("               unreadable, skipped: {f}\n"));
            }
        } else {
            out.push_str(&format!(
                "  {:<12} missing: {} does not exist (use --{}-dir to point elsewhere)\n",
                s.source,
                s.path,
                if s.source == "omp" { "omp" } else { "claude" }
            ));
        }
    }
    let t = &r.totals;
    out.push_str(&format!(
        "Costs: {} exact, {} estimated, {} unpriced (unpriced models count tokens, cost 0)\n",
        t.exact_rows, t.estimated_rows, t.unpriced_rows
    ));
    let window = match (r.since, r.until) {
        (None, None) => "all time".to_string(),
        (s, u) => format!(
            "{} to {}",
            s.map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "start".into()),
            u.map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "now".into())
        ),
    };
    out.push_str(&format!("Window: {window}\n\n"));
    if t.rows == 0 {
        out.push_str(
            "No usage found in the window. Check the sources above, widen --since/--until, or \
             drop --project.\n",
        );
        return out;
    }
    let header = if r.group_by.is_empty() {
        "group".to_string()
    } else {
        r.group_by.join(" / ")
    };
    out.push_str(&table(&header, &r.groups, Some(t)));
    if !r.context_buckets.is_empty() {
        out.push_str("\nBy context size (prompt tokens per turn), per source:\n");
        out.push_str(&table("source / bucket", &r.context_buckets, None));
    }
    out.push('\n');
    for n in &r.notes {
        out.push_str(&format!("Note: {n}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cfg(root: &Path) -> GatewayConfig {
        GatewayConfig::for_project(root)
    }

    fn fixture(root: &Path) -> (PathBuf, PathBuf) {
        let claude = root.join("claude");
        let omp = root.join("omp");
        fs::create_dir_all(claude.join("p")).unwrap();
        fs::create_dir_all(omp.join("p")).unwrap();
        let row = |id: &str, out: u64| {
            serde_json::json!({"type":"assistant","timestamp":"2026-10-10T01:00:00Z",
              "sessionId":"s","cwd":"/x/proj","message":{"id":id,"model":"claude-sonnet-5-5",
              "usage":{"input_tokens":1,"output_tokens":out,"cache_read_input_tokens":300000,
                       "cache_creation_input_tokens":0}}})
            .to_string()
        };
        fs::write(
            claude.join("p/a.jsonl"),
            [row("m1", 5), row("m1", 50), row("m2", 7)].join("\n"),
        )
        .unwrap();
        fs::write(
            omp.join("p/b.jsonl"),
            serde_json::json!({"type":"message","timestamp":"2026-10-10T02:00:00Z","message":{
              "role":"assistant","model":"claude-sonnet-5-5",
              "usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"cost":{"total":0.5}}}})
            .to_string(),
        )
        .unwrap();
        (claude, omp)
    }

    #[test]
    fn report_runs_on_both_formats_and_the_header_says_what_was_found() {
        let dir = tempfile::tempdir().unwrap();
        let (claude, omp) = fixture(dir.path());
        let rep = run_report(&cfg(dir.path()), None, None, "source", None, &claude, &omp).unwrap();
        assert_eq!(rep.totals.rows, 3); // m1 once, m2, one OMP turn
        assert_eq!(rep.sources[0].deduplicated, 1);
        let text = render(&rep);
        assert!(text.contains("1 duplicate row(s) folded"), "{text}");
        assert!(text.contains("ta_goals     missing"), "{text}");
        assert!(text.contains("By context size"), "{text}");
        assert!(
            text.contains("200-400k") || text.contains(">400k"),
            "{text}"
        );
        assert!(text.contains("TOTAL"), "{text}");
    }

    #[test]
    fn missing_sources_are_reported_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        let rep = run_report(
            &cfg(dir.path()),
            None,
            None,
            "",
            None,
            &dir.path().join("none1"),
            &dir.path().join("none2"),
        )
        .unwrap();
        let text = render(&rep);
        assert!(text.contains("missing"), "{text}");
        assert!(text.contains("No usage found"), "{text}");
    }

    #[test]
    fn bad_flags_fail_with_actionable_messages() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let p = dir.path();
        let e = run_report(&c, None, None, "hour", None, p, p).unwrap_err();
        assert!(e.to_string().contains("--group-by"), "{e}");
        let e = run_report(&c, Some("nope"), None, "", None, p, p).unwrap_err();
        assert!(e.to_string().contains("--since"), "{e}");
        let e = run_report(&c, Some("2026-10-12"), Some("2026-10-11"), "", None, p, p).unwrap_err();
        assert!(e.to_string().contains("before --until"), "{e}");
    }

    #[test]
    fn json_output_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let (claude, omp) = fixture(dir.path());
        let rep = run_report(&cfg(dir.path()), None, None, "day", None, &claude, &omp).unwrap();
        let back: CostReport = serde_json::from_str(&serde_json::to_string(&rep).unwrap()).unwrap();
        assert_eq!(back.totals, rep.totals);
        assert_eq!(back.groups[0].group["day"], "2026-10-10");
    }
}
