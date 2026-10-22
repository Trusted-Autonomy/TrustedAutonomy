//! `ta usage`: how much of a plan window or an owner-set budget is used.
//!
//! The meters, budget math, staleness and headline logic live in
//! `ta_goal::usage`; the append-only ledger in `ta_goal::usage_ledger`. This
//! file reads the flags, prints the result, and implements `ta usage capture`
//! (statusline JSON on stdin to an atomically written snapshot).
//!
//! Nothing here reads OAuth tokens or the Keychain, and no endpoint is called.
//! Plan numbers come only from the statusline JSON Claude Code hands to the
//! user's own statusline command.

use std::io::Read;
use std::path::PathBuf;

use anyhow::Context;
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use ta_goal::spend::parse_time_bound;
use ta_goal::usage::{
    default_snapshot_path, gather_summary, snapshot_from_statusline, write_snapshot, PlanStatus,
    UsageMeter, UsageSummary, DEFAULT_PLAN_MAX_AGE_MINUTES,
};
use ta_goal::usage_ledger::UsageBreakdown;
use ta_mcp_gateway::GatewayConfig;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct UsageArgs {
    #[command(subcommand)]
    pub command: Option<UsageCommands>,

    /// Print the summary as JSON (stable field names, see docs/USAGE.md).
    #[arg(long)]
    pub json: bool,
    /// Aggregate this project's ledger instead of the all-projects roll-up.
    #[arg(long)]
    pub project: bool,
    /// Only ledger records at or after this time (RFC 3339 or YYYY-MM-DD, UTC)
    /// in the by-kind and by-model tables. Budgets use their own window.
    #[arg(long)]
    pub since: Option<String>,
    /// Minutes after which the plan snapshot is reported as stale
    /// (default: [usage] plan_max_age_minutes, else 15).
    #[arg(long, value_name = "MINUTES")]
    pub max_age_minutes: Option<u64>,
    /// Plan snapshot file (default: $TA_USAGE_DIR or ~/.config/ta/usage/plan-usage.json).
    #[arg(long, value_name = "FILE")]
    pub snapshot: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum UsageCommands {
    /// Read Claude Code statusline JSON on stdin and write the plan usage
    /// snapshot atomically. Prints nothing on success so it can be chained
    /// from a statusline command. Reads no credentials and calls no endpoint.
    Capture {
        /// Snapshot file to write (default: $TA_USAGE_DIR or
        /// ~/.config/ta/usage/plan-usage.json).
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
}

pub fn execute(args: &UsageArgs, config: &GatewayConfig) -> anyhow::Result<()> {
    match &args.command {
        Some(UsageCommands::Capture { out }) => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).context(
                "could not read the statusline JSON from stdin. Pipe it in, for example: \
                 echo \"$input\" | ta usage capture",
            )?;
            let path = out.clone().unwrap_or_else(default_snapshot_path);
            capture(&input, &path, Utc::now())
        }
        None => {
            let since = args
                .since
                .as_deref()
                .map(|s| parse_time_bound(s).map_err(|e| anyhow::anyhow!("--since: {e}")))
                .transpose()?;
            let summary = gather_summary(
                &config.workspace_root,
                Utc::now(),
                args.project,
                since,
                args.max_age_minutes,
                args.snapshot.as_deref(),
            );
            if args.json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print!("{}", render(&summary));
            }
            Ok(())
        }
    }
}

/// Parse statusline JSON and write the snapshot to `path`. Silent on success.
pub fn capture(input: &str, path: &std::path::Path, now: DateTime<Utc>) -> anyhow::Result<()> {
    let value: serde_json::Value = serde_json::from_str(input.trim()).map_err(|e| {
        anyhow::anyhow!(
            "stdin is not valid JSON ({e}). `ta usage capture` expects the object Claude Code \
             passes to a statusline command. Chain it as shown in docs/USAGE.md."
        )
    })?;
    anyhow::ensure!(
        value.is_object(),
        "stdin JSON is not an object. `ta usage capture` expects the statusline object Claude \
         Code passes to a statusline command."
    );
    write_snapshot(path, &snapshot_from_statusline(&value, now))
}

fn pct(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_string(), |p| format!("{p:.1}%"))
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

fn meter_label(m: &UsageMeter) -> String {
    match &m.scope {
        Some(s) => format!("{} ({})", m.window.as_str(), s.name),
        None => m.window.as_str().to_string(),
    }
}

fn used_text(m: &UsageMeter) -> String {
    let (Some(u), Some(b)) = (&m.used, &m.budget) else {
        return "-".to_string();
    };
    let mut parts = Vec::new();
    if let Some(l) = b.usd {
        parts.push(format!("${:.2} of ${l:.2}", u.usd));
    }
    if let Some(l) = b.tokens {
        parts.push(format!("{} of {} tok", tok(u.tokens), tok(l)));
    }
    parts.join(", ")
}

fn breakdown_table(title: &str, rows: &[UsageBreakdown]) -> String {
    if rows.is_empty() {
        return format!("  {title}: none\n");
    }
    let mut out = format!("  {title}:\n");
    for r in rows {
        let unknown = if r.tokens_unknown_records > 0 {
            format!("  ({} without token counts)", r.tokens_unknown_records)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "    {:<28} {:>6} call(s) {:>10} tok {:>10}{}\n",
            r.key,
            r.records,
            tok(r.tokens),
            format!("${:.2}", r.usd),
            unknown
        ));
    }
    out
}

/// The human-readable report.
pub fn render(s: &UsageSummary) -> String {
    let mut out = String::from("Usage\n");
    match &s.headline {
        Some(h) => {
            let scope = h
                .scope
                .as_ref()
                .map(|x| format!(", {}", x.name))
                .unwrap_or_default();
            out.push_str(&format!(
                "  Headline: {:.1}% ({} usage, {} window{scope})\n",
                h.pct,
                h.basis,
                h.window.as_str()
            ));
        }
        None => out.push_str("  Headline: none (nothing is configured or readable yet)\n"),
    }
    out.push_str(&format!(
        "  plan_usage_pct: {}   budget_used_pct: {}\n",
        pct(s.plan_usage_pct),
        pct(s.budget_used_pct)
    ));
    if let Some(p) = &s.plan {
        let label = match p.status {
            PlanStatus::Fresh => "fresh",
            PlanStatus::Stale => "STALE",
            PlanStatus::Missing => "no snapshot",
            PlanStatus::NotApplicable => "not applicable",
            PlanStatus::Unreadable => "unreadable",
        };
        let plan = p
            .plan_type
            .as_deref()
            .map(|t| format!(" ({t})"))
            .unwrap_or_default();
        out.push_str(&format!("  Plan snapshot{plan}: {label}. {}\n", p.message));
        if p.max_age_minutes != DEFAULT_PLAN_MAX_AGE_MINUTES {
            out.push_str(&format!(
                "  (max snapshot age: {} min)\n",
                p.max_age_minutes
            ));
        }
    }
    if !s.meters.is_empty() {
        out.push_str("\nMeters:\n");
        out.push_str(&format!(
            "  {:<22} {:>8} {:>9}  {:<26} {}\n",
            "window", "plan", "budget", "used", "resets"
        ));
        for m in &s.meters {
            let reset = m
                .resets_at
                .map(|r| r.format("%Y-%m-%d %H:%MZ").to_string())
                .unwrap_or_else(|| "rolling".to_string());
            let flag = if m.stale { "  STALE" } else { "" };
            out.push_str(&format!(
                "  {:<22} {:>8} {:>9}  {:<26} {}{}\n",
                meter_label(m),
                pct(m.plan_usage_pct),
                pct(m.budget_used_pct),
                used_text(m),
                reset,
                flag
            ));
        }
    }
    if let Some(l) = &s.ledger {
        let since = l
            .since
            .map(|t| format!(" since {}", t.format("%Y-%m-%d")))
            .unwrap_or_default();
        if l.found {
            out.push_str(&format!(
                "\nLedger ({}, {} record(s){since}, {}):\n",
                l.scope, l.records, l.path
            ));
            out.push_str(&breakdown_table("By kind", &l.by_kind));
            out.push_str(&breakdown_table("By model", &l.by_model));
            out.push_str(&format!(
                "  Total: {} tok, ${:.2}\n",
                tok(l.total.tokens),
                l.total.usd
            ));
            if l.records > 0 {
                let un = l.total.unknown_token_records;
                out.push_str(&format!(
                    "  tokens_unknown: {un} of {} call(s) ({:.0}%) have no token counts, so the totals above understate real usage by that share\n",
                    l.records,
                    100.0 * un as f64 / l.records as f64
                ));
            }
        } else {
            out.push_str(&format!(
                "\nLedger: none yet at {}. It is written when a goal finishes.\n",
                l.path
            ));
        }
    }
    for n in &s.notes {
        out.push_str(&format!("\nNote: {n}\n"));
    }
    for h in &s.hints {
        out.push_str(&format!("\nHint: {h}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;
    use ta_goal::usage::gather_summary_with;
    use ta_goal::usage_ledger::{UsageKind, UsageLedger, UsageRecord};

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 21, h, m, 0).unwrap()
    }

    fn statusline() -> serde_json::Value {
        json!({
            "cwd": "/should/not/be/kept",
            "subscription_type": "max",
            "rate_limits_available": true,
            "rate_limits": {"limits": [
                {"kind":"session","percent":42.5,"resets_at":"2026-10-21T15:00:00Z","scope":null},
                {"kind":"weekly_all","percent":61.0,"resets_at":"2026-10-26T00:00:00Z","scope":null}
            ]}
        })
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        snap: PathBuf,
        global_cfg: PathBuf,
        global_ledger: UsageLedger,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        std::fs::write(
            root.join(".ta/daemon.toml"),
            "[budget]\nwindow = \"week\"\nusd = 100.0\n",
        )
        .unwrap();
        let snap = dir.path().join("plan-usage.json");
        capture(&statusline().to_string(), &snap, at(11, 55)).unwrap();
        let global_ledger = UsageLedger::new(dir.path().join("global-ledger.jsonl"));
        for (kind, model, out_tokens, usd, unknown) in [
            (UsageKind::Goal, "claude-opus-5-5", 1000u64, 40.0, false),
            (UsageKind::Advisor, "claude", 0, 0.0, true),
        ] {
            global_ledger
                .append(&UsageRecord {
                    ts: at(9, 0),
                    kind,
                    model: model.into(),
                    output_tokens: out_tokens,
                    usd,
                    tokens_unknown: unknown,
                    project: "proj".into(),
                    ..UsageRecord::default()
                })
                .unwrap();
        }
        Fixture {
            global_cfg: dir.path().join("none.toml"),
            _dir: dir,
            root,
            snap,
            global_ledger,
        }
    }

    fn summary(f: &Fixture, now: DateTime<Utc>) -> UsageSummary {
        gather_summary_with(
            &f.root,
            now,
            false,
            None,
            None,
            Some(&f.snap),
            &f.global_cfg,
            &f.global_ledger,
        )
    }

    #[test]
    fn json_snapshot_has_the_documented_shape_and_values() {
        let f = fixture();
        let v = serde_json::to_value(summary(&f, at(12, 0))).unwrap();
        assert_eq!(v["plan_usage_pct"], json!(61.0));
        assert_eq!(v["budget_used_pct"], json!(40.0));
        assert_eq!(v["headline_pct"], json!(61.0));
        assert_eq!(v["headline_basis"], json!("plan"));
        assert_eq!(v["plan"]["status"], json!("fresh"));
        assert_eq!(v["plan"]["plan_type"], json!("max"));
        assert_eq!(v["plan"]["max_age_minutes"], json!(15));
        assert_eq!(v["plan"]["age_secs"], json!(300));
        let week = v["meters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["window"] == "week" && m["scope"].is_null())
            .unwrap();
        assert_eq!(
            week,
            &json!({
                "window": "week",
                "scope": null,
                "plan_usage_pct": 61.0,
                "budget_used_pct": 40.0,
                "budget": {"usd": 100.0, "tokens": null},
                "used": {"usd": 40.0, "tokens": 1000, "unknown_token_records": 1},
                "resets_at": "2026-10-26T00:00:00Z",
                "source": "claude_code_statusline+ta_ledger",
                "captured_at": "2026-10-21T11:55:00Z",
                "stale": false
            })
        );
        let by_kind = &v["ledger"]["by_kind"];
        assert_eq!(by_kind.as_array().unwrap().len(), 2);
        assert_eq!(v["ledger"]["scope"], json!("global"));
        assert_eq!(v["ledger"]["total"]["unknown_token_records"], json!(1));
        // Nothing from the statusline beyond plan fields reaches the snapshot.
        assert!(!std::fs::read_to_string(&f.snap)
            .unwrap()
            .contains("should/not"));
    }

    #[test]
    fn stale_snapshot_is_labelled_in_text_and_json() {
        let f = fixture();
        let s = summary(&f, at(13, 0));
        assert_eq!(s.plan_usage_pct, None);
        let text = render(&s);
        assert!(text.contains("STALE"), "{text}");
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["plan"]["status"], json!("stale"));
        assert!(v["meters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["stale"] == json!(true)));
    }

    #[test]
    fn text_report_shows_headline_meters_ledger_and_unknown_token_note() {
        let f = fixture();
        let text = render(&summary(&f, at(12, 0)));
        for want in [
            "Headline: 61.0% (plan usage",
            "plan_usage_pct: 61.0%   budget_used_pct: 40.0%",
            "Meters:",
            "week",
            "$40.00 of $100.00",
            "Ledger (global",
            "By kind:",
            "By model:",
            "without token counts",
            "could not report tokens",
            "tokens_unknown: ",
            "have no token counts",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }

    #[test]
    fn nothing_configured_prints_hints_not_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let s = gather_summary_with(
            dir.path(),
            at(12, 0),
            false,
            None,
            None,
            Some(&dir.path().join("missing.json")),
            &dir.path().join("none.toml"),
            &UsageLedger::new(dir.path().join("g.jsonl")),
        );
        let text = render(&s);
        assert!(text.contains("Headline: none"), "{text}");
        assert!(text.contains("[budget]"), "{text}");
        assert!(text.contains("ta usage capture"), "{text}");
        assert!(text.contains("Ledger: none yet"), "{text}");
    }

    #[test]
    fn capture_writes_atomically_and_rejects_bad_input_with_next_steps() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("plan-usage.json");
        capture(&statusline().to_string(), &p, at(10, 0)).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("\"captured_at\": \"2026-10-21T10:00:00Z\""),
            "{text}"
        );
        let e = capture("not json", &p, at(10, 1)).unwrap_err().to_string();
        assert!(
            e.contains("not valid JSON") && e.contains("docs/USAGE.md"),
            "{e}"
        );
        assert!(capture("[1]", &p, at(10, 1)).is_err());
        // A failed capture leaves the previous snapshot intact.
        assert!(std::fs::read_to_string(&p).unwrap().contains("10:00:00Z"));
    }

    /// Golden failure: a ledger row that cannot be parsed must be reported in
    /// the summary, never silently dropped from the totals.
    #[test]
    fn a_dropped_ledger_row_is_reported_not_silent() {
        let f = fixture();
        let mut text = std::fs::read_to_string(f.global_ledger.path()).unwrap();
        text.push_str("{ this line is torn\n");
        std::fs::write(f.global_ledger.path(), text).unwrap();
        let s = summary(&f, at(12, 0));
        assert_eq!(s.ledger.as_ref().unwrap().unparsable_lines, 1);
        assert!(render(&s).contains("could not be parsed"));
    }
}
