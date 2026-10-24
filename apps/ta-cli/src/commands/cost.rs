//! `ta cost report`: spend across TA goals and interactive agent sessions.
//!
//! The reading, deduplication, pricing and grouping live in
//! `ta_goal::cost_report`; this file finds the sources, applies the flags and
//! prints the result. Each source is optional: a missing directory is named in
//! the header, never an error.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Subcommand;
use ta_goal::cost_detail::DetailTotals;
use ta_goal::cost_export::{
    meridian_command, meridian_records_with_usage, session_rows, write_jsonl,
};
use ta_goal::cost_report::{
    build_report, goal_rows, parse_cost_group_by, scan_claude, scan_omp, CostFilter, CostGroup,
    CostGroupRow, CostReport, CostRow, CostTotals, Scan,
};
use ta_goal::spend::parse_time_bound;
use ta_goal::{GoalRun, GoalRunStore, PriceTable};
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
        /// context_bucket, origin_class, goal, session_family, entrypoint.
        /// For one typed row per goal and session use `--json --rows` instead.
        #[arg(long, default_value = "source")]
        group_by: String,
        /// Only projects whose name contains this text (case-insensitive).
        #[arg(long)]
        project: Option<String>,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// With --json, print one typed row per goal and per session (a JSON
        /// array) instead of the grouped report. Same ids and token counts as
        /// --meridian. An unpriced model has `usd: null`.
        #[arg(long, requires = "json")]
        rows: bool,
        /// Claude Code projects directory (default: ~/.claude/projects).
        #[arg(long, value_name = "DIR")]
        claude_dir: Option<PathBuf>,
        /// OMP sessions directory (default: ~/.omp/agent/sessions).
        #[arg(long, value_name = "DIR")]
        omp_dir: Option<PathBuf>,
        /// Also write one record per goal or session to this Meridian JSONL
        /// file. Re-running updates records by id and never duplicates them.
        #[arg(long, value_name = "FILE.jsonl")]
        meridian: Option<PathBuf>,
        /// Also print the output-token split (thinking / text / tool-call
        /// arguments), edit and whole-file-write counts, the turns-per-session
        /// histogram, session-start overhead and short-session families.
        /// `--json` always includes them.
        #[arg(long)]
        detail: bool,
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
            rows,
            claude_dir,
            omp_dir,
            meridian,
            detail,
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
            let inputs = gather(
                config,
                since.as_deref(),
                until.as_deref(),
                group_by,
                project.as_deref(),
                &claude,
                &omp,
            )?;
            // Plan and budget usage, attached to the JSON/text report and to
            // each Meridian record. Never changes the `--rows` array.
            let usage = ta_goal::usage::gather_summary(
                &config.workspace_root,
                chrono::Utc::now(),
                false,
                inputs.filter.since,
                None,
                None,
            );
            // Export first from the same rows the report is built from, so the
            // file and the table cannot disagree.
            let exported = meridian
                .as_ref()
                .map(|path| export_meridian(&inputs, path, Some(&usage)))
                .transpose()?;
            let rows_json = if *rows {
                Some(rows_json(&inputs)?)
            } else {
                None
            };
            let mut report = inputs.into_report();
            report.usage = Some(usage);
            if let Some(text) = rows_json {
                println!("{text}");
            } else if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", render(&report));
                if *detail {
                    print!("{}", render_detail(&report));
                }
            }
            if let Some(msg) = exported {
                // stderr under --json keeps stdout parseable.
                if *json {
                    eprintln!("{msg}");
                } else {
                    println!("{msg}");
                }
            }
            Ok(())
        }
    }
}

/// Validated flags and everything read from the sources.
struct Inputs {
    goals: Option<Vec<(String, GoalRun)>>,
    scans: Vec<Scan>,
    group_by: Vec<CostGroup>,
    filter: CostFilter,
}

impl Inputs {
    fn into_report(self) -> CostReport {
        build_report(
            self.goals.as_deref(),
            self.scans,
            &self.group_by,
            &self.filter,
        )
    }
}

/// Every goal and session that passes the filters, as the typed rows array.
fn export_rows(inputs: &Inputs) -> Vec<ta_goal::cost_export::SessionRow> {
    let rows = filtered_rows(inputs);
    let kept: Vec<&CostRow> = rows.iter().collect();
    session_rows(&kept, inputs.goals.as_deref().unwrap_or(&[]))
}

/// All goal and transcript rows that pass --since/--until/--project.
fn filtered_rows(inputs: &Inputs) -> Vec<CostRow> {
    let mut rows: Vec<CostRow> = inputs.goals.as_deref().map(goal_rows).unwrap_or_default();
    for scan in &inputs.scans {
        rows.extend(scan.rows.iter().cloned());
    }
    rows.retain(|r| inputs.filter.matches(r));
    rows
}

fn rows_json(inputs: &Inputs) -> anyhow::Result<String> {
    serde_json::to_string_pretty(&export_rows(inputs))
        .context("could not serialize the cost rows to JSON. Re-run without --rows to see the grouped report")
}

/// Write the Meridian file and return the confirmation text.
fn export_meridian(
    inputs: &Inputs,
    path: &Path,
    usage: Option<&ta_goal::usage::UsageSummary>,
) -> anyhow::Result<String> {
    let rows = filtered_rows(inputs);
    let kept: Vec<&CostRow> = rows.iter().collect();
    let goals = inputs.goals.as_deref().unwrap_or(&[]);
    let records = meridian_records_with_usage(&kept, goals, usage);
    let out = write_jsonl(path, &records)?;
    tracing::info!(
        path = %path.display(),
        exported = out.exported,
        replaced = out.replaced,
        kept = out.kept,
        "cost report exported to Meridian"
    );
    let mut msg = format!(
        "Meridian export: wrote {} record(s) to {} ({} updated in place, {} other record(s) kept)",
        out.exported,
        path.display(),
        out.replaced,
        out.kept
    );
    if out.exported == 0 {
        msg.push_str(
            "\nNo goals or sessions matched, so nothing new was exported. Widen --since/--until \
             or --project, or check the sources listed above.",
        );
    }
    msg.push_str(&format!("\nRead it with: {}", meridian_command(path)));
    Ok(msg)
}

/// Validate the flags, read every source and build the report.
#[cfg(test)]
fn run_report(
    config: &GatewayConfig,
    since: Option<&str>,
    until: Option<&str>,
    group_by: &str,
    project: Option<&str>,
    claude_dir: &Path,
    omp_dir: &Path,
) -> anyhow::Result<CostReport> {
    Ok(gather(config, since, until, group_by, project, claude_dir, omp_dir)?.into_report())
}

fn gather(
    config: &GatewayConfig,
    since: Option<&str>,
    until: Option<&str>,
    group_by: &str,
    project: Option<&str>,
    claude_dir: &Path,
    omp_dir: &Path,
) -> anyhow::Result<Inputs> {
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
                .map(|mut g| {
                    ta_goal::spend::estimate_legacy_goal_cost(&mut g, &table);
                    (project_name.clone(), g)
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let scans: Vec<Scan> = vec![scan_claude(claude_dir, &table), scan_omp(omp_dir, &table)];
    Ok(Inputs {
        goals,
        scans,
        group_by,
        filter,
    })
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
    out.push_str(&format!("Window: {window}\n"));
    if let Some(u) = &r.usage {
        let p = |v: Option<f64>| v.map_or_else(|| "n/a".to_string(), |x| format!("{x:.1}%"));
        let stale = u
            .plan
            .as_ref()
            .filter(|pl| pl.status == ta_goal::usage::PlanStatus::Stale)
            .map(|_| " (plan snapshot STALE)")
            .unwrap_or("");
        out.push_str(&format!(
            "Usage: plan_usage_pct {}, budget_used_pct {}{stale}  (details: `ta usage`)\n",
            p(u.plan_usage_pct),
            p(u.budget_used_pct)
        ));
    }
    out.push('\n');
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

/// Right-align every column but the first `lead`.
fn grid(header: &[&str], lead: usize, rows: &[Vec<String>]) -> String {
    let mut width: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            width[i] = width[i].max(c.chars().count());
        }
    }
    let line = |cells: &[String]| {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i > 0 {
                s.push_str("  ");
            }
            if i < lead {
                s.push_str(&format!("{:<w$}", c, w = width[i]));
            } else {
                s.push_str(&format!("{:>w$}", c, w = width[i]));
            }
        }
        s.push('\n');
        s
    };
    let head: Vec<String> = header.iter().map(|h| h.to_string()).collect();
    let mut out = line(&head);
    for r in rows {
        out.push_str(&line(r));
    }
    out
}

fn pct1(part: u64, whole: u64) -> String {
    if whole == 0 {
        "n/a".to_string()
    } else {
        format!("{:.1}%", part as f64 / whole as f64 * 100.0)
    }
}

/// The `--detail` sections: output split, edit behaviour, session shape.
/// Printed after [`render`]; every `-est` column is an estimate.
pub fn render_detail(r: &CostReport) -> String {
    let mut out = String::from("\nDetail\n");
    let label = |g: &CostGroupRow| {
        if g.group.is_empty() {
            "all".to_string()
        } else {
            g.group.values().cloned().collect::<Vec<_>>().join(" / ")
        }
    };
    let header = if r.group_by.is_empty() {
        "group".to_string()
    } else {
        r.group_by.join(" / ")
    };
    let mut with: Vec<(String, &DetailTotals)> = r
        .groups
        .iter()
        .filter_map(|g| g.totals.detail.as_ref().map(|d| (label(g), d)))
        .collect();
    if let Some(d) = &r.totals.detail {
        with.push(("TOTAL".to_string(), d));
    }
    if with.is_empty() {
        out.push_str(
            "No transcript turns in the window, so there is no output split or edit data.\n",
        );
    } else {
        out.push_str(
            "\nOutput tokens by kind of the transcript turns (think-exact: reported by the \
             runtime; every -est column is an ESTIMATE from block length):\n",
        );
        let rows: Vec<Vec<String>> = with
            .iter()
            .map(|(l, d)| {
                let o = &d.output_split;
                let think = o.thinking_tokens_reported + o.thinking_tokens_est;
                vec![
                    l.clone(),
                    tok(d.output_tokens),
                    tok(o.thinking_tokens_reported),
                    tok(o.thinking_tokens_est),
                    tok(o.text_tokens_est),
                    tok(o.tool_arg_tokens_est),
                    tok(o.unattributed_tokens),
                    pct1(think, d.output_tokens),
                    pct1(o.tool_arg_tokens_est, d.output_tokens),
                ]
            })
            .collect();
        out.push_str(&grid(
            &[
                &header,
                "output",
                "think-exact",
                "think-est",
                "text-est",
                "args-est",
                "unattrib",
                "think%",
                "args%",
            ],
            1,
            &rows,
        ));
        out.push_str("\nEdit tool calls and whole-file writes:\n");
        let rows: Vec<Vec<String>> = with
            .iter()
            .map(|(l, d)| {
                let e = &d.edits;
                vec![
                    l.clone(),
                    e.tool_calls.to_string(),
                    e.edit_calls.to_string(),
                    e.edit_failures.to_string(),
                    pct1(e.edit_failures, e.edit_calls),
                    e.edit_retries.to_string(),
                    tok(e.edit_arg_tokens_est),
                    tok(e.failed_edit_arg_tokens_est),
                    e.file_writes.to_string(),
                    e.whole_file_overwrites.to_string(),
                    e.large_overwrites.to_string(),
                    tok(e.overwrite_bytes),
                ]
            })
            .collect();
        out.push_str(&grid(
            &[
                &header,
                "calls",
                "edits",
                "failed",
                "fail%",
                "retries",
                "edit-arg-est",
                "failed-arg-est",
                "writes",
                "overwrites",
                "large-ovw",
                "ovw-bytes",
            ],
            1,
            &rows,
        ));
    }
    match &r.session_shape {
        Some(s) if !s.sources.is_empty() => {
            out.push_str(&format!(
                "\nSessions that started in the window (short = at most {} turns):\n",
                s.short_max_turns
            ));
            let mut rows: Vec<Vec<String>> = Vec::new();
            for src in &s.sources {
                for b in &src.turns_histogram {
                    rows.push(vec![
                        src.source.clone(),
                        b.turns.clone(),
                        b.sessions.to_string(),
                        format!("{:.2}", b.usd),
                    ]);
                }
            }
            out.push_str("Turns per session:\n");
            out.push_str(&grid(&["source", "turns", "sessions", "usd"], 2, &rows));
            out.push_str("\nSession-start overhead (cache write of each session's first turn):\n");
            let mut rows: Vec<Vec<String>> = Vec::new();
            for src in &s.sources {
                for c in &src.classes {
                    let f = &c.first_turn;
                    rows.push(vec![
                        format!("{} / {}", src.source, c.class),
                        c.sessions.to_string(),
                        c.turns.to_string(),
                        format!("{:.2}", c.usd),
                        format!("{:.1}%", c.usd_share_pct),
                        tok(f.cache_write_tokens_mean),
                        tok(f.cache_write_tokens_median),
                        tok(f.cache_write_tokens_p90),
                        tok(f.cache_write_tokens_max),
                        format!("{:.1}%", f.share_of_cache_write_pct),
                        format!("{:.2}", f.usd_total),
                        format!("{:.1}%", f.usd_share_pct),
                    ]);
                }
            }
            out.push_str(&grid(
                &[
                    "source / class",
                    "sessions",
                    "turns",
                    "usd",
                    "usd%",
                    "1st-cw-mean",
                    "median",
                    "p90",
                    "max",
                    "1st-cw%",
                    "1st-turn-usd",
                    "1st-usd%",
                ],
                1,
                &rows,
            ));
            if !s.families.is_empty() {
                out.push_str(
                    "\nShort sessions by first-prompt family (a label for a known job, else \
                     other:<hash>; prompt text is never shown):\n",
                );
                let rows: Vec<Vec<String>> = s
                    .families
                    .iter()
                    .map(|f| {
                        vec![
                            format!("{} / {} / {}", f.source, f.entrypoint, f.family),
                            f.sessions.to_string(),
                            f.turns.to_string(),
                            format!("{:.2}", f.usd),
                            format!("{:.1}%", f.usd_share_of_short_pct),
                            tok(f.first_turn_cache_write_tokens_mean),
                        ]
                    })
                    .collect();
                out.push_str(&grid(
                    &[
                        "source / entrypoint / family",
                        "sessions",
                        "turns",
                        "usd",
                        "short-usd%",
                        "1st-cw-mean",
                    ],
                    1,
                    &rows,
                ));
                if s.families_folded > 0 {
                    out.push_str(&format!(
                        "({} more families folded into the (remaining) row)\n",
                        s.families_folded
                    ));
                }
            }
        }
        _ => out.push_str("\nNo transcript sessions started in the window.\n"),
    }
    out.push('\n');
    for n in &r.detail_notes {
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
    fn detail_tables_are_separate_from_the_unchanged_report_text() {
        let dir = tempfile::tempdir().unwrap();
        let (claude, omp) = fixture(dir.path());
        let rep = run_report(&cfg(dir.path()), None, None, "source", None, &claude, &omp).unwrap();
        let base = render(&rep);
        assert!(!base.contains("Output tokens by kind"), "{base}");
        let detail = render_detail(&rep);
        for want in [
            "Output tokens by kind",
            "think-exact",
            "Edit tool calls and whole-file writes",
            "Turns per session",
            "Session-start overhead",
            "ESTIMATE",
        ] {
            assert!(detail.contains(want), "missing {want:?} in:\n{detail}");
        }
        // The new --group-by values are accepted from the CLI path.
        let by = "session_family,entrypoint";
        let rep = run_report(&cfg(dir.path()), None, None, by, None, &claude, &omp).unwrap();
        assert!(rep
            .groups
            .iter()
            .all(|g| g.group.contains_key("entrypoint")));
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

    #[test]
    fn usage_fields_are_additive_in_the_report_the_text_and_meridian() {
        let dir = tempfile::tempdir().unwrap();
        let (claude, omp) = fixture(dir.path());
        let inputs = gather(&cfg(dir.path()), None, None, "source", None, &claude, &omp).unwrap();
        let usage = ta_goal::usage::UsageSummary {
            plan_usage_pct: Some(61.0),
            budget_used_pct: None,
            ..Default::default()
        };
        let out = dir.path().join("cost.jsonl");
        export_meridian(&inputs, &out, Some(&usage)).unwrap();
        let recs: Vec<serde_json::Value> = fs::read_to_string(&out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        for r in &recs {
            assert_eq!(r["plan_usage_pct"], "61.00");
            assert_eq!(r["budget_used_pct"], "n/a", "unknown is n/a, never 0");
            assert_eq!(r["usage_captured_at"], "n/a");
        }
        let mut rep = inputs.into_report();
        assert!(rep.usage.is_none());
        rep.usage = Some(usage);
        let text = render(&rep);
        assert!(
            text.contains("plan_usage_pct 61.0%, budget_used_pct n/a"),
            "{text}"
        );
        // A report saved before this field existed still loads.
        let mut v = serde_json::to_value(&rep).unwrap();
        v.as_object_mut().unwrap().remove("usage");
        let old: CostReport = serde_json::from_value(v).unwrap();
        assert!(old.usage.is_none());
    }

    #[test]
    fn meridian_export_dedupes_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let (claude, omp) = fixture(dir.path());
        let out = dir.path().join("cost.jsonl");
        let inputs = gather(&cfg(dir.path()), None, None, "source", None, &claude, &omp).unwrap();
        let msg = export_meridian(&inputs, &out, None).unwrap();
        assert!(msg.contains("2 record(s)"), "{msg}");
        assert!(
            msg.contains("meridian analyze --source jsonl --path"),
            "{msg}"
        );

        // The Claude session repeats message id m1; it counts once (5+... last row wins).
        let text = fs::read_to_string(&out).unwrap();
        let recs: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let claude_rec = recs.iter().find(|r| r["source"] == "claude_code").unwrap();
        assert_eq!(claude_rec["tokens_output"], 50 + 7);
        let omp_rec = recs.iter().find(|r| r["source"] == "omp").unwrap();
        assert_eq!(omp_rec["cost_exact"], "true");
        assert_eq!(omp_rec["cost_usd"], "0.5000");

        // Re-running leaves the file byte-identical.
        let again = gather(&cfg(dir.path()), None, None, "source", None, &claude, &omp).unwrap();
        let msg = export_meridian(&again, &out, None).unwrap();
        assert!(msg.contains("2 updated in place"), "{msg}");
        assert_eq!(fs::read_to_string(&out).unwrap(), text);
    }

    /// A project dir named `proj` holding three goals: runtime-priced (exact),
    /// table-priced (estimated) and unpriced.
    fn goal_fixture(root: &Path) -> GatewayConfig {
        use chrono::{TimeZone, Utc};
        use ta_goal::token_cost::CostSource;
        let config = cfg(root);
        fs::create_dir_all(&config.goals_dir).unwrap();
        let store = GoalRunStore::new(&config.goals_dir).unwrap();
        let make = |id: &str, title: &str, model: &str, cost: f64, src: CostSource, hour: u32| {
            let mut g = GoalRun::new(
                title,
                "obj",
                "agent",
                root.to_path_buf(),
                root.to_path_buf(),
            );
            g.goal_run_id = id.parse().unwrap();
            g.created_at = Utc.with_ymd_and_hms(2026, 10, 11, hour, 0, 0).unwrap();
            g.plan_phase = Some("v1.0".into());
            g.agent_model = model.into();
            g.input_tokens = 100;
            g.output_tokens = 20;
            g.cache_read_input_tokens = 3000;
            g.cache_creation_input_tokens = 40;
            g.cost_usd = cost;
            g.cost_source = src;
            store.save(&g).unwrap();
        };
        make(
            "00000000-0000-0000-0000-000000000001",
            "exact goal",
            "claude-sonnet-5-5",
            1.5,
            CostSource::Runtime,
            1,
        );
        make(
            "00000000-0000-0000-0000-000000000002",
            "estimated goal",
            "claude-opus-5-5",
            2.25,
            CostSource::Table,
            2,
        );
        make(
            "00000000-0000-0000-0000-000000000003",
            "unpriced goal",
            "mystery-model",
            0.0,
            CostSource::None,
            3,
        );
        config
    }

    fn proj_root(dir: &Path) -> PathBuf {
        let root = dir.join("proj");
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn rows_json_golden_shape_covers_exact_estimated_and_unpriced() {
        let dir = tempfile::tempdir().unwrap();
        let root = proj_root(dir.path());
        let config = goal_fixture(&root);
        let none = dir.path().join("none");
        let inputs = gather(&config, None, None, "source", None, &none, &none).unwrap();
        let got: serde_json::Value = serde_json::from_str(&rows_json(&inputs).unwrap()).unwrap();
        let goal = |n: u8,
                    title: &str,
                    hour: u32,
                    model: &str,
                    usd: serde_json::Value,
                    exact: bool,
                    priced: bool| {
            serde_json::json!({
                "id": format!("ta_goals:00000000-0000-0000-0000-00000000000{n}"),
                "kind": "goal",
                "goal_id": format!("00000000-0000-0000-0000-00000000000{n}"),
                "title": title,
                "phase": "v1.0",
                "project": "proj",
                "model": model,
                "started_at": format!("2026-10-11T{hour:02}:00:00+00:00"),
                "tokens_input": 100,
                "tokens_output": 20,
                "cache_read_tokens": 3000,
                "cache_write_tokens": 40,
                "usd": usd,
                "cost_exact": exact,
                "priced": priced,
            })
        };
        let want = serde_json::json!([
            goal(
                1,
                "exact goal",
                1,
                "claude-sonnet-5-5",
                serde_json::json!(1.5),
                true,
                true
            ),
            goal(
                2,
                "estimated goal",
                2,
                "claude-opus-5-5",
                serde_json::json!(2.25),
                false,
                true
            ),
            goal(
                3,
                "unpriced goal",
                3,
                "mystery-model",
                serde_json::Value::Null,
                false,
                false
            ),
        ]);
        assert_eq!(got, want);
        // Numbers are JSON numbers, never strings.
        assert!(got[0]["tokens_input"].is_u64() && got[0]["usd"].is_f64());
    }

    #[test]
    fn rows_and_meridian_share_ids_and_token_counts() {
        let dir = tempfile::tempdir().unwrap();
        let root = proj_root(dir.path());
        let config = goal_fixture(&root);
        let (claude, omp) = fixture(dir.path());
        let inputs = gather(&config, None, None, "source", None, &claude, &omp).unwrap();
        let out = dir.path().join("cost.jsonl");
        export_meridian(&inputs, &out, None).unwrap();
        let meridian: Vec<serde_json::Value> = fs::read_to_string(&out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let rows = export_rows(&inputs);
        assert_eq!(rows.len(), 5); // 3 goals, 1 Claude session, 1 OMP session
        assert_eq!(rows.len(), meridian.len());
        for r in &rows {
            let m = meridian
                .iter()
                .find(|m| m["id"] == r.id.as_str())
                .unwrap_or_else(|| panic!("row {} missing from the Meridian file", r.id));
            assert_eq!(m["tokens_input"], r.tokens_input, "{}", r.id);
            assert_eq!(m["tokens_output"], r.tokens_output, "{}", r.id);
            assert_eq!(m["cache_read_tokens"], r.cache_read_tokens.to_string());
            assert_eq!(m["cache_write_tokens"], r.cache_write_tokens.to_string());
            assert_eq!(m["cost_exact"], r.cost_exact.to_string());
        }
        // The unpriced goal carries no invented price in the rows.
        let unpriced = rows.iter().find(|r| r.title == "unpriced goal").unwrap();
        assert_eq!((unpriced.usd, unpriced.priced), (None, false));
    }

    #[test]
    fn rows_respect_filters() {
        let dir = tempfile::tempdir().unwrap();
        let root = proj_root(dir.path());
        let config = goal_fixture(&root);
        let none = dir.path().join("none");
        let g = |since: Option<&str>, project: Option<&str>| {
            export_rows(&gather(&config, since, None, "source", project, &none, &none).unwrap())
        };
        assert_eq!(g(Some("2026-10-11T02:00:00Z"), None).len(), 2);
        assert_eq!(g(None, Some("proj")).len(), 3);
        assert!(g(None, Some("other")).is_empty());
    }

    #[test]
    fn group_by_goal_gives_one_group_per_goal() {
        let dir = tempfile::tempdir().unwrap();
        let root = proj_root(dir.path());
        let config = goal_fixture(&root);
        let none = dir.path().join("none");
        let rep = run_report(&config, None, None, "goal", None, &none, &none).unwrap();
        assert_eq!(rep.groups.len(), 3);
        assert!(rep.groups.iter().all(|g| g.group.contains_key("goal")));
        let e = run_report(&config, None, None, "hour", None, &none, &none).unwrap_err();
        assert!(e.to_string().contains("--json --rows"), "{e}");
    }
}
