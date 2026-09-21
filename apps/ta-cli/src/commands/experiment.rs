// experiment.rs: `ta experiment` command group. Defines, starts/stops, and
// reports on generic cost experiments. See `ta_goal::experiment` for the
// underlying config format and arm-assignment logic this command group
// manages.

use std::collections::HashMap;
use std::path::Path;

use clap::Subcommand;
use ta_goal::{
    merge_velocity_entries, ExperimentConfig, GoalError, VelocityHistoryStore, VelocityStore,
};
use ta_mcp_gateway::GatewayConfig;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum ExperimentCommands {
    /// Define and activate a cost experiment.
    Start {
        /// Experiment id.
        id: String,
        /// Fraction of eligible goals assigned an arm via the cheap,
        /// continuous unpaired-holdout mode. 0.0-1.0.
        #[arg(long)]
        holdout_fraction: f64,
        /// Fraction of eligible goals additionally run as a paired
        /// canonical-plus-shadow sample. 0.0-1.0.
        #[arg(long, default_value_t = 0.0)]
        paired_fraction: f64,
        /// Workflow tag whose cost should be netted out of the report's
        /// savings figure as maintenance overhead.
        #[arg(long)]
        maintenance_workflow: Option<String>,
        /// Arm treated as the default/canonical baseline for paired samples.
        #[arg(long)]
        canonical_arm: Option<String>,
        /// Repeatable: `--arm name=<json-object>`, e.g.
        /// `--arm variant-off='{"feature.disabled":true}'`
        #[arg(long = "arm", value_parser = parse_arm)]
        arms: Vec<(String, serde_json::Value)>,
    },
    /// Deactivate a cost experiment (its config file is removed; historical
    /// velocity-history.jsonl entries already tagged with it are untouched).
    Stop {
        /// Experiment id.
        id: String,
    },
    /// Print the delta report for an experiment.
    Report {
        /// Experiment id.
        id: String,
    },
}

fn parse_arm(s: &str) -> Result<(String, serde_json::Value), String> {
    let (name, json) = s
        .split_once('=')
        .ok_or_else(|| "expected name=<json-object>".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid JSON for arm {name}: {e}"))?;
    Ok((name.to_string(), value))
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArmStats {
    pub n: usize,
    pub mean_cost_usd: f64,
    pub stddev_cost_usd: f64,
}

/// Paired-sample (within-pair differencing) statistics. Lower variance,
/// higher-precision signal than the group-mean `by_arm` comparison, since
/// each pair holds the task fixed and only the arm's config overrides vary.
///
/// `mean_delta_usd`/`stddev_delta_usd` are computed over each complete
/// pair's `delta = shadow_cost_usd - canonical_cost_usd`: a positive value
/// means the shadow arm cost more than the canonical arm.
#[derive(Debug, Clone, PartialEq)]
pub struct PairedStats {
    pub n: usize,
    pub mean_delta_usd: f64,
    pub stddev_delta_usd: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExperimentReport {
    pub experiment_id: String,
    /// Group-mean stats per arm. Includes every feature-work entry tagged
    /// with an arm, whether or not it's also part of a paired sample.
    pub by_arm: HashMap<String, ArmStats>,
    /// Within-pair differencing stats, computed only over complete pairs
    /// (both canonical and shadow entries present). `None` when there are
    /// zero complete pairs.
    pub paired: Option<PairedStats>,
    pub maintenance_cost_usd: f64,
    /// (pricier arm mean - cheaper arm mean) x pricier arm's n, minus total
    /// maintenance cost. Only meaningful with exactly two arms present;
    /// `0.0` otherwise, which callers should treat as "not computable," not
    /// "no savings."
    pub net_savings_usd: f64,
}

fn mean_and_stddev(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    if values.len() < 2 {
        return (mean, 0.0);
    }
    let variance =
        values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    (mean, variance.sqrt())
}

/// Read both `.ta/velocity-stats.jsonl` (local, written for every terminal
/// goal including this feature's own shadow-goal auto-cancel hook) and
/// `.ta/velocity-history.jsonl` (committed, populated only once `ta draft
/// apply` migrates local entries into it) under `project_root`, merge them
/// (see `merge_velocity_entries`, the same pattern `ta stats` already uses),
/// filter to `experiment_id`, and compute per-arm statistics, paired-sample
/// statistics, plus the maintenance-netted savings figure.
///
/// Entries whose `workflow` matches `maintenance_workflow` are counted
/// toward `maintenance_cost_usd` (scoped to the feature-work entries' own
/// timestamp window, see below) and excluded from the per-arm groups
/// (maintenance runs are overhead, not experiment-arm samples), regardless
/// of whether they also carry an `experiment_arm` tag.
///
/// `canonical_arm` mirrors `ExperimentConfig::canonical_arm`: used to decide
/// which half of a paired sample is canonical vs. shadow. When `None`, the
/// alphabetically-earlier of the pair's two arm names is treated as
/// canonical, matching `assign_arm`'s own fallback logic.
pub fn build_report(
    project_root: &Path,
    experiment_id: &str,
    maintenance_workflow: Option<&str>,
    canonical_arm: Option<&str>,
) -> Result<ExperimentReport, GoalError> {
    let local_store = VelocityStore::for_project(project_root);
    let history_store = VelocityHistoryStore::for_project(project_root);
    let local = local_store.load_all()?;
    let committed = history_store.load_all()?;
    let (entries, _committed_ids) = merge_velocity_entries(local, committed);

    // Feature-work entries tagged with this experiment (excludes anything
    // tagged as the maintenance workflow, even if it also happens to carry
    // this experiment's id/arm) vs. maintenance-tagged entries, netted
    // against feature-work's own timestamp window below.
    let mut feature_entries = Vec::new();
    let mut maintenance_entries = Vec::new();

    for e in entries {
        let is_maintenance = maintenance_workflow.is_some_and(|tag| e.workflow == tag);
        if is_maintenance {
            maintenance_entries.push(e);
            continue;
        }
        if e.experiment_id.as_deref() != Some(experiment_id) {
            continue;
        }
        feature_entries.push(e);
    }

    let mut by_arm_costs: HashMap<String, Vec<f64>> = HashMap::new();
    let mut pairs: HashMap<Uuid, Vec<&ta_goal::VelocityEntry>> = HashMap::new();
    for e in &feature_entries {
        if let Some(arm) = e.experiment_arm.clone() {
            by_arm_costs.entry(arm).or_default().push(e.cost_usd);
        }
        if let Some(pair_id) = e.experiment_pair_id {
            pairs.entry(pair_id).or_default().push(e);
        }
    }

    let by_arm: HashMap<String, ArmStats> = by_arm_costs
        .iter()
        .map(|(arm, costs)| {
            let (mean, stddev) = mean_and_stddev(costs);
            (
                arm.clone(),
                ArmStats {
                    n: costs.len(),
                    mean_cost_usd: mean,
                    stddev_cost_usd: stddev,
                },
            )
        })
        .collect();

    // Paired (within-pair differencing) stats: only complete pairs (exactly
    // two entries) count. `delta = shadow_cost_usd - canonical_cost_usd`.
    let mut deltas: Vec<f64> = Vec::new();
    for members in pairs.values() {
        if members.len() != 2 {
            continue; // incomplete (or malformed) pair -- shadow hasn't finished yet
        }
        let canonical_shadow = if let Some(canonical_name) = canonical_arm {
            let canonical = members
                .iter()
                .find(|m| m.experiment_arm.as_deref() == Some(canonical_name));
            let shadow = members
                .iter()
                .find(|m| m.experiment_arm.as_deref() != Some(canonical_name));
            canonical.zip(shadow).map(|(a, b)| (*a, *b))
        } else {
            let mut sorted = members.clone();
            sorted.sort_by(|a, b| a.experiment_arm.cmp(&b.experiment_arm));
            match (sorted.first(), sorted.get(1)) {
                (Some(a), Some(b)) => Some((*a, *b)),
                _ => None,
            }
        };
        if let Some((canonical, shadow)) = canonical_shadow {
            deltas.push(shadow.cost_usd - canonical.cost_usd);
        }
    }
    let paired = if deltas.is_empty() {
        None
    } else {
        let (mean_delta_usd, stddev_delta_usd) = mean_and_stddev(&deltas);
        Some(PairedStats {
            n: deltas.len(),
            mean_delta_usd,
            stddev_delta_usd,
        })
    };

    // Maintenance cost, netted only against the entries actually inside the
    // experiment's own feature-work sample window -- not the entire
    // maintenance-tagged history, which could predate the experiment by
    // months. Zero when there's no feature-work yet to net against.
    let maintenance_cost_usd = if feature_entries.is_empty() {
        0.0
    } else {
        let earliest = feature_entries.iter().map(|e| e.started_at).min().unwrap();
        let latest = feature_entries.iter().map(|e| e.started_at).max().unwrap();
        maintenance_entries
            .iter()
            .filter(|e| e.started_at >= earliest && e.started_at <= latest)
            .map(|e| e.cost_usd)
            .sum()
    };

    let net_savings_usd = if by_arm.len() == 2 {
        let mut arms: Vec<&ArmStats> = by_arm.values().collect();
        arms.sort_by(|a, b| a.mean_cost_usd.partial_cmp(&b.mean_cost_usd).unwrap());
        let cheaper = arms[0];
        let pricier = arms[1];
        (pricier.mean_cost_usd - cheaper.mean_cost_usd) * pricier.n as f64 - maintenance_cost_usd
    } else {
        0.0
    };

    Ok(ExperimentReport {
        experiment_id: experiment_id.to_string(),
        by_arm,
        paired,
        maintenance_cost_usd,
        net_savings_usd,
    })
}

pub fn execute(cmd: &ExperimentCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    match cmd {
        ExperimentCommands::Start {
            id,
            holdout_fraction,
            paired_fraction,
            maintenance_workflow,
            canonical_arm,
            arms,
        } => {
            let existing = ExperimentConfig::list(project_root)?;
            if let Some(conflicting) = existing.iter().find(|c| &c.id != id) {
                anyhow::bail!(
                    "an experiment is already active ('{}'); only one active experiment is \
                     supported in this release. Run `ta experiment stop {}` first.",
                    conflicting.id,
                    conflicting.id
                );
            }
            if arms.len() < 2 {
                anyhow::bail!(
                    "experiment '{id}' needs at least two --arm entries to assign anything; got {}",
                    arms.len()
                );
            }
            let config = ExperimentConfig {
                id: id.clone(),
                holdout_fraction: *holdout_fraction,
                paired_fraction: *paired_fraction,
                maintenance_workflow: maintenance_workflow.clone(),
                canonical_arm: canonical_arm.clone(),
                arms: arms.iter().cloned().collect(),
            };
            config.save(project_root)?;
            println!(
                "Experiment '{id}' started: holdout_fraction={holdout_fraction}, paired_fraction={paired_fraction}, arms={}",
                config.arms.len()
            );
        }
        ExperimentCommands::Stop { id } => {
            let path =
                ta_goal::experiment::experiments_dir(project_root).join(format!("{id}.toml"));
            if path.is_file() {
                std::fs::remove_file(&path)?;
                println!("Experiment '{id}' stopped.");
            } else {
                println!("No active experiment named '{id}'.");
            }
        }
        ExperimentCommands::Report { id } => {
            let experiment_config = ExperimentConfig::load(project_root, id)?;
            let maintenance_tag = experiment_config
                .as_ref()
                .and_then(|c| c.maintenance_workflow.clone());
            let canonical_arm = experiment_config
                .as_ref()
                .and_then(|c| c.canonical_arm.clone());
            let report = build_report(
                project_root,
                id,
                maintenance_tag.as_deref(),
                canonical_arm.as_deref(),
            )?;
            println!("Experiment: {}", report.experiment_id);
            if report.by_arm.is_empty() {
                println!("  no velocity data found for this experiment yet.");
            }
            let mut arms: Vec<(&String, &ArmStats)> = report.by_arm.iter().collect();
            arms.sort_by(|a, b| a.0.cmp(b.0));
            for (arm, stats) in arms {
                println!(
                    "  {arm}: n={}, mean_cost_usd={:.4}, stddev_cost_usd={:.4}",
                    stats.n, stats.mean_cost_usd, stats.stddev_cost_usd
                );
            }
            if report.maintenance_cost_usd > 0.0 {
                println!("  maintenance_cost_usd={:.4}", report.maintenance_cost_usd);
            }
            if report.by_arm.len() == 2 {
                println!("  net_savings_usd={:.4}", report.net_savings_usd);
            }
            if let Some(paired) = &report.paired {
                if paired.n > 0 {
                    println!(
                        "  paired samples: n={}, mean_delta_usd={:.4}, stddev_delta_usd={:.4}",
                        paired.n, paired.mean_delta_usd, paired.stddev_delta_usd
                    );
                }
            }
            println!(
                "  Note: shadow goals that made no changes are not included above (they never \
                 reach a terminal state)."
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_goal::{GoalOutcome, VelocityEntry, VelocityHistoryStore, VelocityStore};
    use tempfile::tempdir;
    use uuid::Uuid;

    fn entry(arm: &str, cost_usd: f64, workflow: &str) -> VelocityEntry {
        let goal_id = Uuid::new_v4();
        VelocityEntry {
            goal_id,
            title: "t".to_string(),
            workflow: workflow.to_string(),
            agent: "claude".to_string(),
            plan_phase: None,
            outcome: GoalOutcome::Applied,
            started_at: chrono::Utc::now(),
            pr_ready_at: None,
            completed_at: Some(chrono::Utc::now()),
            build_seconds: 60,
            review_seconds: 0,
            total_seconds: 60,
            amended: false,
            follow_up_count: 0,
            rework_seconds: 0,
            denial_reason: None,
            cancel_reason: None,
            machine_id: String::new(),
            committer: None,
            input_tokens: 1000,
            output_tokens: 500,
            cost_usd,
            model: "claude-sonnet-5".to_string(),
            cost_estimated: false,
            tokens_input: Some(1000),
            tokens_output: Some(500),
            derived_title: None,
            experiment_id: Some("cost-test-1".to_string()),
            experiment_arm: Some(arm.to_string()),
            experiment_pair_id: None,
        }
    }

    fn entry_at(
        arm: &str,
        cost_usd: f64,
        workflow: &str,
        started_at: chrono::DateTime<chrono::Utc>,
    ) -> VelocityEntry {
        let mut e = entry(arm, cost_usd, workflow);
        e.started_at = started_at;
        e
    }

    fn paired_entry(arm: &str, cost_usd: f64, pair_id: Uuid) -> VelocityEntry {
        let mut e = entry(arm, cost_usd, "feature-work");
        e.experiment_pair_id = Some(pair_id);
        e
    }

    #[test]
    fn report_computes_group_means_and_nets_maintenance_cost() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        let now = chrono::Utc::now();
        history
            .append(&entry_at(
                "variant-on",
                0.10,
                "feature-work",
                now - chrono::Duration::days(3),
            ))
            .unwrap();
        history
            .append(&entry_at(
                "variant-on",
                0.12,
                "feature-work",
                now - chrono::Duration::days(2),
            ))
            .unwrap();
        history
            .append(&entry_at(
                "variant-off",
                0.20,
                "feature-work",
                now - chrono::Duration::days(1),
            ))
            .unwrap();
        history
            .append(&entry_at("variant-off", 0.18, "feature-work", now))
            .unwrap();
        // Maintenance entry timestamped inside the feature-work window
        // (defined above by the four entries' own [earliest, latest] span).
        let mut maintenance = entry_at(
            "variant-on",
            0.05,
            "experiment-maintenance",
            now - chrono::Duration::days(1) + chrono::Duration::hours(1),
        );
        maintenance.experiment_arm = None;
        history.append(&maintenance).unwrap();

        let report = build_report(
            dir.path(),
            "cost-test-1",
            Some("experiment-maintenance"),
            None,
        )
        .unwrap();

        assert_eq!(report.by_arm.get("variant-on").unwrap().n, 2);
        assert!((report.by_arm.get("variant-on").unwrap().mean_cost_usd - 0.11).abs() < 1e-9);
        assert_eq!(report.by_arm.get("variant-off").unwrap().n, 2);
        assert!((report.by_arm.get("variant-off").unwrap().mean_cost_usd - 0.19).abs() < 1e-9);
        assert!((report.maintenance_cost_usd - 0.05).abs() < 1e-9);
        // net savings: (variant-off mean - variant-on mean) per goal, minus total maintenance cost
        assert!((report.net_savings_usd - ((0.19 - 0.11) * 2.0 - 0.05)).abs() < 1e-9);
    }

    #[test]
    fn report_with_no_matching_entries_returns_empty_groups() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        history
            .append(&entry("variant-on", 0.10, "feature-work"))
            .unwrap(); // tagged for a different experiment below

        let report = build_report(dir.path(), "some-other-experiment", None, None).unwrap();
        assert!(report.by_arm.is_empty());
        assert_eq!(report.maintenance_cost_usd, 0.0);
    }

    #[test]
    fn report_with_single_arm_present_computes_no_net_savings() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        history
            .append(&entry("variant-on", 0.10, "feature-work"))
            .unwrap();
        history
            .append(&entry("variant-on", 0.14, "feature-work"))
            .unwrap();

        let report = build_report(dir.path(), "cost-test-1", None, None).unwrap();

        assert_eq!(report.by_arm.len(), 1);
        assert_eq!(report.by_arm.get("variant-on").unwrap().n, 2);
        assert!((report.by_arm.get("variant-on").unwrap().mean_cost_usd - 0.12).abs() < 1e-9);
        // Only one arm present: net savings is not computable, represented as 0.0.
        assert_eq!(report.net_savings_usd, 0.0);
    }

    #[test]
    fn report_reads_local_velocity_store_not_just_committed_history() {
        // Every terminal-goal write (including this feature's own shadow-goal
        // auto-cancel hook) lands in the local `velocity-stats.jsonl` store
        // first; entries migrate to the committed history only on `ta draft
        // apply`. A report that only reads history is systematically empty
        // for real, unapplied usage.
        let dir = tempdir().unwrap();
        let local = VelocityStore::for_project(dir.path());
        local
            .append(&entry("variant-on", 0.10, "feature-work"))
            .unwrap();
        local
            .append(&entry("variant-off", 0.20, "feature-work"))
            .unwrap();

        let report = build_report(dir.path(), "cost-test-1", None, None).unwrap();

        assert_eq!(report.by_arm.get("variant-on").unwrap().n, 1);
        assert_eq!(report.by_arm.get("variant-off").unwrap().n, 1);
    }

    #[test]
    fn report_merges_local_and_committed_without_double_counting() {
        // A goal_id present in both stores (post `ta draft apply` migration)
        // must be counted once, matching `merge_velocity_entries`'s own
        // dedup contract.
        let dir = tempdir().unwrap();
        let local = VelocityStore::for_project(dir.path());
        let history = VelocityHistoryStore::for_project(dir.path());
        let shared = entry("variant-on", 0.10, "feature-work");
        local.append(&shared).unwrap();
        history.append(&shared).unwrap();
        local
            .append(&entry("variant-on", 0.12, "feature-work"))
            .unwrap();

        let report = build_report(dir.path(), "cost-test-1", None, None).unwrap();

        assert_eq!(report.by_arm.get("variant-on").unwrap().n, 2);
    }

    #[test]
    fn paired_stats_computed_from_complete_pairs() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        let pair_1 = Uuid::new_v4();
        let pair_2 = Uuid::new_v4();
        // pair 1: canonical (variant-on) cheaper, delta = shadow - canonical = 0.05
        history
            .append(&paired_entry("variant-on", 0.10, pair_1))
            .unwrap();
        history
            .append(&paired_entry("variant-off", 0.15, pair_1))
            .unwrap();
        // pair 2: delta = 0.03
        history
            .append(&paired_entry("variant-on", 0.20, pair_2))
            .unwrap();
        history
            .append(&paired_entry("variant-off", 0.23, pair_2))
            .unwrap();

        // canonical_arm unset: alphabetically-earlier name ("variant-off" <
        // "variant-on") is treated as canonical, matching `assign_arm`'s own
        // fallback -- so delta = variant-on - variant-off here.
        let report = build_report(dir.path(), "cost-test-1", None, None).unwrap();

        let paired = report.paired.expect("expected paired stats to be present");
        assert_eq!(paired.n, 2);
        // deltas: pair1 = 0.10 - 0.15 = -0.05, pair2 = 0.20 - 0.23 = -0.03
        let expected_mean = (-0.05 + -0.03) / 2.0;
        assert!((paired.mean_delta_usd - expected_mean).abs() < 1e-9);
        assert!(paired.stddev_delta_usd >= 0.0);
    }

    #[test]
    fn paired_stats_use_explicit_canonical_arm_when_set() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        let pair_1 = Uuid::new_v4();
        history
            .append(&paired_entry("variant-on", 0.10, pair_1))
            .unwrap();
        history
            .append(&paired_entry("variant-off", 0.16, pair_1))
            .unwrap();

        // Explicit canonical_arm = "variant-on": delta = shadow - canonical
        // = variant-off - variant-on = 0.06.
        let report = build_report(dir.path(), "cost-test-1", None, Some("variant-on")).unwrap();

        let paired = report.paired.expect("expected paired stats to be present");
        assert_eq!(paired.n, 1);
        assert!((paired.mean_delta_usd - 0.06).abs() < 1e-9);
        assert_eq!(paired.stddev_delta_usd, 0.0);
    }

    #[test]
    fn incomplete_pair_excluded_from_paired_stats_but_counted_in_by_arm() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        let pair_1 = Uuid::new_v4();
        // Only the canonical half has landed -- shadow hasn't finished yet.
        history
            .append(&paired_entry("variant-on", 0.10, pair_1))
            .unwrap();

        let report = build_report(dir.path(), "cost-test-1", None, None).unwrap();

        assert!(
            report.paired.is_none(),
            "an incomplete pair must not produce paired stats"
        );
        assert_eq!(report.by_arm.get("variant-on").unwrap().n, 1);
    }

    #[test]
    fn zero_paired_entries_leaves_paired_stats_absent_without_crashing() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        history
            .append(&entry("variant-on", 0.10, "feature-work"))
            .unwrap();
        history
            .append(&entry("variant-off", 0.20, "feature-work"))
            .unwrap();

        let report = build_report(dir.path(), "cost-test-1", None, None).unwrap();

        assert!(report.paired.is_none());
        // by_arm (group-mean) view must still be fully populated.
        assert_eq!(report.by_arm.len(), 2);
    }

    #[test]
    fn maintenance_cost_scoped_to_experiment_feature_work_window() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        let now = chrono::Utc::now();
        let before_window = now - chrono::Duration::days(60);
        let during_window = now - chrono::Duration::days(5);
        let after_window = now + chrono::Duration::days(5);

        // Feature-work entries define the experiment's own [earliest, latest] window.
        history
            .append(&entry_at(
                "variant-on",
                0.10,
                "feature-work",
                now - chrono::Duration::days(10),
            ))
            .unwrap();
        history
            .append(&entry_at("variant-on", 0.12, "feature-work", now))
            .unwrap();

        // Maintenance entries: one well before the window, one inside it, one after.
        let mut before = entry_at("variant-on", 1.00, "experiment-maintenance", before_window);
        before.experiment_arm = None;
        history.append(&before).unwrap();

        let mut during = entry_at("variant-on", 0.50, "experiment-maintenance", during_window);
        during.experiment_arm = None;
        history.append(&during).unwrap();

        let mut after = entry_at("variant-on", 2.00, "experiment-maintenance", after_window);
        after.experiment_arm = None;
        history.append(&after).unwrap();

        let report = build_report(
            dir.path(),
            "cost-test-1",
            Some("experiment-maintenance"),
            None,
        )
        .unwrap();

        // Only the "during" maintenance entry falls within the feature-work
        // window; the months-old and future entries must not be netted.
        assert!((report.maintenance_cost_usd - 0.50).abs() < 1e-9);
    }

    #[test]
    fn maintenance_cost_is_zero_with_no_feature_work_entries_yet() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        let mut maintenance = entry("variant-on", 5.00, "experiment-maintenance");
        maintenance.experiment_arm = None;
        history.append(&maintenance).unwrap();

        let report = build_report(
            dir.path(),
            "cost-test-1",
            Some("experiment-maintenance"),
            None,
        )
        .unwrap();

        assert_eq!(report.maintenance_cost_usd, 0.0);
    }

    #[test]
    fn parse_arm_rejects_missing_equals() {
        assert!(parse_arm("variant-on").is_err());
    }

    #[test]
    fn parse_arm_rejects_invalid_json() {
        assert!(parse_arm("variant-on=not-json").is_err());
    }

    #[test]
    fn parse_arm_accepts_name_and_json_object() {
        let (name, value) = parse_arm(r#"variant-off={"feature.disabled":true}"#).unwrap();
        assert_eq!(name, "variant-off");
        assert_eq!(value, serde_json::json!({"feature.disabled": true}));
    }
}
