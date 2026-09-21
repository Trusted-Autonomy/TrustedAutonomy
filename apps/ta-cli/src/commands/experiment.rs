// experiment.rs: `ta experiment` command group. Defines, starts/stops, and
// reports on generic cost experiments. See `ta_goal::experiment` for the
// underlying config format and arm-assignment logic this command group
// manages.

use std::collections::HashMap;
use std::path::Path;

use clap::Subcommand;
use ta_goal::{ExperimentConfig, GoalError, VelocityHistoryStore};
use ta_mcp_gateway::GatewayConfig;

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

#[derive(Debug, Clone, PartialEq)]
pub struct ExperimentReport {
    pub experiment_id: String,
    pub unpaired: HashMap<String, ArmStats>,
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

/// Read `.ta/velocity-history.jsonl` under `project_root`, filter to
/// `experiment_id`, and compute per-arm statistics plus the maintenance-
/// netted savings figure.
///
/// Entries whose `workflow` matches `maintenance_workflow` are counted
/// toward `maintenance_cost_usd` and excluded from the per-arm groups
/// (maintenance runs are overhead, not experiment-arm samples), regardless
/// of whether they also carry an `experiment_arm` tag.
pub fn build_report(
    project_root: &Path,
    experiment_id: &str,
    maintenance_workflow: Option<&str>,
) -> Result<ExperimentReport, GoalError> {
    let history = VelocityHistoryStore::for_project(project_root);
    let entries = history.load_all()?;

    let mut by_arm: HashMap<String, Vec<f64>> = HashMap::new();
    let mut maintenance_cost_usd = 0.0;

    for e in entries {
        if let Some(tag) = maintenance_workflow {
            if e.workflow == tag {
                maintenance_cost_usd += e.cost_usd;
                continue;
            }
        }
        if e.experiment_id.as_deref() != Some(experiment_id) {
            continue;
        }
        let Some(arm) = e.experiment_arm.clone() else {
            continue;
        };
        by_arm.entry(arm).or_default().push(e.cost_usd);
    }

    let unpaired: HashMap<String, ArmStats> = by_arm
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

    let net_savings_usd = if unpaired.len() == 2 {
        let mut arms: Vec<&ArmStats> = unpaired.values().collect();
        arms.sort_by(|a, b| a.mean_cost_usd.partial_cmp(&b.mean_cost_usd).unwrap());
        let cheaper = arms[0];
        let pricier = arms[1];
        (pricier.mean_cost_usd - cheaper.mean_cost_usd) * pricier.n as f64 - maintenance_cost_usd
    } else {
        0.0
    };

    Ok(ExperimentReport {
        experiment_id: experiment_id.to_string(),
        unpaired,
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
            if existing.iter().any(|c| &c.id != id) {
                anyhow::bail!(
                    "an experiment is already active ('{}'); only one active experiment is \
                     supported in this release. Run `ta experiment stop {}` first.",
                    existing[0].id,
                    existing[0].id
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
            let report = build_report(project_root, id, maintenance_tag.as_deref())?;
            println!("Experiment: {}", report.experiment_id);
            if report.unpaired.is_empty() {
                println!("  no velocity-history.jsonl entries found for this experiment yet.");
            }
            let mut arms: Vec<(&String, &ArmStats)> = report.unpaired.iter().collect();
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
            if report.unpaired.len() == 2 {
                println!("  net_savings_usd={:.4}", report.net_savings_usd);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_goal::{GoalOutcome, VelocityEntry, VelocityHistoryStore};
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

    #[test]
    fn report_computes_group_means_and_nets_maintenance_cost() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::for_project(dir.path());
        history
            .append(&entry("variant-on", 0.10, "feature-work"))
            .unwrap();
        history
            .append(&entry("variant-on", 0.12, "feature-work"))
            .unwrap();
        history
            .append(&entry("variant-off", 0.20, "feature-work"))
            .unwrap();
        history
            .append(&entry("variant-off", 0.18, "feature-work"))
            .unwrap();
        let mut maintenance = entry("variant-on", 0.05, "experiment-maintenance");
        maintenance.experiment_arm = None;
        history.append(&maintenance).unwrap();

        let report =
            build_report(dir.path(), "cost-test-1", Some("experiment-maintenance")).unwrap();

        assert_eq!(report.unpaired.get("variant-on").unwrap().n, 2);
        assert!((report.unpaired.get("variant-on").unwrap().mean_cost_usd - 0.11).abs() < 1e-9);
        assert_eq!(report.unpaired.get("variant-off").unwrap().n, 2);
        assert!((report.unpaired.get("variant-off").unwrap().mean_cost_usd - 0.19).abs() < 1e-9);
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

        let report = build_report(dir.path(), "some-other-experiment", None).unwrap();
        assert!(report.unpaired.is_empty());
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

        let report = build_report(dir.path(), "cost-test-1", None).unwrap();

        assert_eq!(report.unpaired.len(), 1);
        assert_eq!(report.unpaired.get("variant-on").unwrap().n, 2);
        assert!((report.unpaired.get("variant-on").unwrap().mean_cost_usd - 0.12).abs() < 1e-9);
        // Only one arm present: net savings is not computable, represented as 0.0.
        assert_eq!(report.net_savings_usd, 0.0);
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
