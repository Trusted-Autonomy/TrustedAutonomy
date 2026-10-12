//! `ta run --origin-class / --max-tokens / --max-usd`: launch-time spend settings,
//! and the completion record a finished goal publishes.
//!
//! The flags travel into `run::execute` through a scoped guard (the same pattern
//! as `--provenance-file`, see `review_packet`), so every goal that one `ta run`
//! launches, including the sub-goals of `--phases`, sees the same settings.
//!
//! `origin_class` is accounting data only. It is set here, by the launcher, and
//! nowhere an agent can reach; see `ta_goal::origin` for the rules.

use std::cell::RefCell;
use std::path::Path;

use ta_goal::origin::{self, OriginClass};
use ta_goal::spend::{BudgetLimits, CompletionRecord};
use ta_goal::{GoalRun, GoalRunStore, PriceTable};

#[derive(Clone, Default, Debug)]
struct SpendFlags {
    origin_class: Option<OriginClass>,
    max_tokens: Option<u64>,
    max_usd: Option<f64>,
}

thread_local! {
    static CLI_SPEND: RefCell<SpendFlags> = RefCell::new(SpendFlags::default());
}

/// Holds the spend flags of one `ta run` for the duration of its `execute`.
pub struct CliSpendGuard {
    previous: SpendFlags,
}

impl CliSpendGuard {
    /// `origin_class` is the raw `--origin-class` text; it is validated here so a
    /// typo fails the launch before any staging exists.
    pub fn set(
        origin_class: Option<&str>,
        max_tokens: Option<u64>,
        max_usd: Option<f64>,
    ) -> anyhow::Result<Self> {
        let origin_class = origin_class
            .map(origin::parse_origin_class)
            .transpose()
            .map_err(anyhow::Error::msg)?;
        // Validate the ceilings now (the config defaults are merged later).
        BudgetLimits::resolve(max_tokens, max_usd, None, None).map_err(anyhow::Error::msg)?;
        let previous = CLI_SPEND.with(|c| {
            c.replace(SpendFlags {
                origin_class,
                max_tokens,
                max_usd,
            })
        });
        Ok(Self { previous })
    }
}

impl Drop for CliSpendGuard {
    fn drop(&mut self) {
        let previous = std::mem::take(&mut self.previous);
        CLI_SPEND.with(|c| *c.borrow_mut() = previous);
    }
}

/// Whether this process looks like it runs inside an agent. Advisory: it reads
/// the same marker variables the human gate uses, and an agent can unset them.
/// It is one layer; the others are that no agent-facing API accepts an origin
/// class and that a recorded class is never changed.
pub(crate) fn launched_by_agent_from(env: &dyn Fn(&str) -> Option<String>) -> bool {
    ta_credentials::human::AGENT_MARKER_VARS
        .iter()
        .any(|var| env(var).is_some_and(|v| !v.is_empty()))
}

/// Stamp the goal's origin class at creation. A class already on the record is
/// never changed. Returns the class now on the goal.
pub(crate) fn stamp_origin_class(goal: &mut GoalRun, quiet: bool) -> anyhow::Result<OriginClass> {
    let flag = CLI_SPEND.with(|c| c.borrow().origin_class);
    let inherited = origin::origin_class_from_env().map_err(anyhow::Error::msg)?;
    let by_agent = launched_by_agent_from(&|k| std::env::var(k).ok());
    stamp_with(goal, flag, inherited, by_agent, quiet)
}

pub(crate) fn stamp_with(
    goal: &mut GoalRun,
    flag: Option<OriginClass>,
    inherited: Option<OriginClass>,
    launched_by_agent: bool,
    quiet: bool,
) -> anyhow::Result<OriginClass> {
    // The flag wins over the provenance file; the environment is `inherited`.
    let requested = flag.or(goal
        .request_provenance
        .as_ref()
        .and_then(|p| p.origin_class));
    match origin::origin_class_to_stamp(goal.origin_class, requested, inherited, launched_by_agent)
    {
        Some(class) => {
            if launched_by_agent && requested.is_some_and(|r| Some(r) != inherited) {
                tracing::warn!(
                    goal_id = %goal.goal_run_id,
                    requested = ?requested,
                    recorded = %class,
                    "ignoring the requested origin class: this process runs inside an agent, \
                     and an agent cannot set the class of work it starts"
                );
                if !quiet {
                    eprintln!(
                        "[warn] Goal {}: the requested origin class was ignored because this \
                         `ta run` was started from inside an agent. Recorded: {}. The class is \
                         set by whoever launches the goal, not by an agent: run `ta run \
                         --origin-class <asked|suggested|unasked>` from your own terminal.",
                        goal.goal_run_id, class
                    );
                }
            }
            tracing::info!(goal_id = %goal.goal_run_id, origin_class = %class, "goal origin class set");
            goal.origin_class = Some(class);
            Ok(class)
        }
        None => {
            let existing = goal.effective_origin_class();
            if let Some(r) = requested.filter(|r| *r != existing) {
                tracing::warn!(
                    goal_id = %goal.goal_run_id,
                    recorded = %existing,
                    requested = %r,
                    "ignoring the requested origin class: the goal already has one and it \
                     cannot be changed after creation"
                );
                if !quiet {
                    eprintln!(
                        "[warn] Goal {} keeps its recorded origin class '{}'; the requested \
                         class '{}' was ignored (the class is fixed when the goal is created).",
                        goal.goal_run_id, existing, r
                    );
                }
            }
            Ok(existing)
        }
    }
}

/// The ceilings for this launch: the flags, else `[budget]` in
/// `.ta/workflow.toml`.
pub(crate) fn resolve_limits(project_root: &Path) -> anyhow::Result<BudgetLimits> {
    let flags = CLI_SPEND.with(|c| c.borrow().clone());
    let cfg =
        ta_submit::WorkflowConfig::load_or_default(&project_root.join(".ta").join("workflow.toml"))
            .budget;
    BudgetLimits::resolve(flags.max_tokens, flags.max_usd, cfg.max_tokens, cfg.max_usd).map_err(
        |e| {
            anyhow::anyhow!(
                "{e} (checked --max-tokens, --max-usd and [budget] in .ta/workflow.toml)"
            )
        },
    )
}

/// The price table for this project (`.ta/pricing.toml` over the built-in table).
pub(crate) fn price_table(project_root: &Path) -> PriceTable {
    PriceTable::load(project_root)
}

/// When a goal was stopped by a spend ceiling and no draft could be built from
/// it, record that in the velocity file with the `aborted_budget` outcome. A
/// goal that does get a draft reaches the normal applied/denied outcome, with
/// `aborted_budget = true` on its entry.
pub(crate) fn record_budget_abort_velocity(
    config: &ta_mcp_gateway::GatewayConfig,
    goal_id: uuid::Uuid,
    table: &PriceTable,
) {
    let Ok(Some(goal)) = GoalRunStore::new(&config.goals_dir).and_then(|s| s.get(goal_id)) else {
        return;
    };
    if goal.budget_abort.is_none() {
        return;
    }
    let entry = ta_goal::VelocityEntry::from_goal(&goal, ta_goal::GoalOutcome::AbortedBudget)
        .with_goal_spend(&goal, table);
    if let Err(e) = ta_goal::VelocityStore::for_project(&config.workspace_root).append(&entry) {
        tracing::warn!(
            goal_id = %goal_id,
            error = %e,
            "could not record the aborted_budget velocity entry; the goal record still holds \
             the abort. Check that .ta/ is writable"
        );
    }
}

/// One printable line about a goal's spend.
pub(crate) fn summary_line(r: &CompletionRecord) -> String {
    let cost = match r.cost_source {
        ta_goal::CostSource::Runtime => format!("${:.4} (reported by the agent runtime)", r.usd),
        ta_goal::CostSource::Table => {
            format!(
                "${:.4} (estimated from the price table; add .ta/pricing.toml to adjust)",
                r.usd
            )
        }
        ta_goal::CostSource::None => {
            format!(
            "no price for model {:?} (tokens recorded; add a row to .ta/pricing.toml to price it)",
            if r.model.is_empty() { "unknown" } else { &r.model }
        )
        }
    };
    format!(
        "Spend: input {} / output {} / cache read {} / cache write {} tokens; {}; origin class {}",
        r.input_tokens,
        r.output_tokens,
        r.cache_read_tokens,
        r.cache_creation_tokens,
        cost,
        r.origin_class
    )
}

/// Publish a finished goal's completion record: a `goal.completed` event on the
/// project's event store (served on `/api/events`) and a log line. Returns the
/// record so the caller can include it in the headless result line. `None` when
/// the goal record cannot be read, which is logged.
pub(crate) fn publish_completion(
    goals_dir: &Path,
    events_dir: &Path,
    goal_id: uuid::Uuid,
    quiet: bool,
) -> Option<CompletionRecord> {
    let goal = match GoalRunStore::new(goals_dir).and_then(|s| s.get(goal_id)) {
        Ok(Some(g)) => g,
        Ok(None) => {
            tracing::warn!(goal_id = %goal_id, dir = %goals_dir.display(), "cannot publish the completion record: the goal is missing from the store");
            return None;
        }
        Err(e) => {
            tracing::warn!(goal_id = %goal_id, dir = %goals_dir.display(), error = %e, "cannot publish the completion record: the goal store could not be read");
            return None;
        }
    };
    let record = CompletionRecord::from_goal(&goal);
    {
        use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
        let value = serde_json::to_value(&record).unwrap_or(serde_json::Value::Null);
        let store = FsEventStore::new(events_dir);
        if let Err(e) = store.append(&EventEnvelope::new(SessionEvent::GoalSpendRecorded {
            goal_id,
            record: value,
        })) {
            tracing::warn!(
                goal_id = %goal_id,
                dir = %events_dir.display(),
                error = %e,
                "could not append the goal.completed event; the completion record is still in \
                 `ta goal status --json`. Check that the events directory is writable"
            );
        }
    }
    tracing::info!(
        goal_id = %goal_id,
        input_tokens = record.input_tokens,
        output_tokens = record.output_tokens,
        cache_read_tokens = record.cache_read_tokens,
        cache_creation_tokens = record.cache_creation_tokens,
        usd = record.usd,
        cost_estimated = record.cost_estimated,
        origin_class = %record.origin_class,
        aborted = record.aborted,
        "goal completion record published"
    );
    if !quiet {
        println!("{}", summary_line(&record));
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn goal() -> GoalRun {
        GoalRun::new(
            "t",
            "o",
            "claude-code",
            PathBuf::from("/tmp/w"),
            PathBuf::from("/tmp/s"),
        )
    }

    #[test]
    fn default_is_unasked_and_the_flag_is_validated() {
        let mut g = goal();
        let c = stamp_with(&mut g, None, None, false, true).unwrap();
        assert_eq!(c, OriginClass::Unasked);
        assert_eq!(g.origin_class, Some(OriginClass::Unasked));
        assert!(CliSpendGuard::set(Some("requested"), None, None).is_err());
        assert!(CliSpendGuard::set(Some("Asked"), Some(10), Some(1.0)).is_ok());
        assert!(CliSpendGuard::set(None, Some(0), None).is_err());
        assert!(CliSpendGuard::set(None, None, Some(-3.0)).is_err());
    }

    #[test]
    fn flag_beats_provenance_which_beats_environment() {
        let dir = tempfile::tempdir().unwrap();
        let prov = dir.path().join("p.json");
        std::fs::write(
            &prov,
            r#"{"requester":{"id":"u","channel":"c"},"origin_class":"suggested"}"#,
        )
        .unwrap();
        let mut g = goal();
        crate::commands::review_packet::stamp_goal_with(&mut g, Some(&prov), None, true).unwrap();
        // Provenance beats the environment.
        let mut g1 = g.clone();
        let c = stamp_with(&mut g1, None, Some(OriginClass::Asked), false, true).unwrap();
        assert_eq!(c, OriginClass::Suggested);
        // The flag beats provenance.
        let mut g2 = g.clone();
        let c = stamp_with(&mut g2, Some(OriginClass::Asked), None, false, true).unwrap();
        assert_eq!(c, OriginClass::Asked);
    }

    #[test]
    fn nested_goal_inherits_the_exported_class() {
        // A goal started inside a running goal (env exported, agent markers set)
        // gets the parent's class even when it asks for a different one.
        let mut g = goal();
        let c = stamp_with(
            &mut g,
            Some(OriginClass::Asked),
            Some(OriginClass::Suggested),
            true,
            true,
        )
        .unwrap();
        assert_eq!(c, OriginClass::Suggested);
        // And with nothing exported, an agent's request still cannot raise it.
        let mut g = goal();
        let c = stamp_with(&mut g, Some(OriginClass::Asked), None, true, true).unwrap();
        assert_eq!(c, OriginClass::Unasked);
        // Outside an agent, inheritance fills in when no flag is given.
        let mut g = goal();
        let c = stamp_with(&mut g, None, Some(OriginClass::Suggested), false, true).unwrap();
        assert_eq!(c, OriginClass::Suggested);
    }

    #[test]
    fn a_recorded_class_survives_a_later_run() {
        let mut g = goal();
        stamp_with(&mut g, Some(OriginClass::Asked), None, false, true).unwrap();
        for requested in OriginClass::ALL {
            let c = stamp_with(&mut g, Some(requested), Some(requested), false, true).unwrap();
            assert_eq!(c, OriginClass::Asked, "requested {requested}");
        }
        assert_eq!(g.origin_class, Some(OriginClass::Asked));
    }

    #[test]
    fn agent_markers_are_detected() {
        let none = |_: &str| None;
        assert!(!launched_by_agent_from(&none));
        for marker in ta_credentials::human::AGENT_MARKER_VARS {
            let m = marker.to_string();
            let env = move |k: &str| (k == m).then(|| "1".to_string());
            assert!(launched_by_agent_from(&env), "{marker}");
        }
        let empty = |_: &str| Some(String::new());
        assert!(
            !launched_by_agent_from(&empty),
            "an empty marker is not set"
        );
    }

    #[test]
    fn limits_come_from_flags_then_workflow_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[budget]\nmax_tokens = 900\nmax_usd = 7.5\n",
        )
        .unwrap();
        let none = resolve_limits(dir.path()).unwrap();
        assert_eq!(none.max_tokens, Some(900));
        assert_eq!(none.max_usd, Some(7.5));
        let _g = CliSpendGuard::set(None, Some(50), None).unwrap();
        let flag = resolve_limits(dir.path()).unwrap();
        assert_eq!(flag.max_tokens, Some(50), "the flag wins");
        assert_eq!(
            flag.max_usd,
            Some(7.5),
            "the default still applies to the other ceiling"
        );
    }

    #[test]
    fn summary_line_says_where_the_cost_came_from() {
        let mut g = goal();
        g.agent_model = "qwen3.5:9b".into();
        let line = summary_line(&CompletionRecord::from_goal(&g));
        assert!(line.contains("no price for model"), "{line}");
        assert!(line.contains(".ta/pricing.toml"), "{line}");
        g.cost_source = ta_goal::CostSource::Runtime;
        g.cost_usd = 1.5;
        let line = summary_line(&CompletionRecord::from_goal(&g));
        assert!(line.contains("reported by the agent runtime"), "{line}");
    }

    #[test]
    fn publishing_writes_a_goal_completed_event_and_returns_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let goals = dir.path().join("goals");
        let events = dir.path().join("events");
        let store = GoalRunStore::new(&goals).unwrap();
        let mut g = goal();
        g.origin_class = Some(OriginClass::Asked);
        g.input_tokens = 5;
        g.cache_read_input_tokens = 99;
        store.save(&g).unwrap();
        let rec = publish_completion(&goals, &events, g.goal_run_id, true).unwrap();
        assert_eq!(rec.goal_id, g.goal_run_id);
        assert_eq!(rec.origin_class, OriginClass::Asked);
        assert_eq!(rec.cache_read_tokens, 99);
        let text: String = std::fs::read_dir(&events)
            .unwrap()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
            .collect();
        assert!(text.contains("goal.completed"), "{text}");
        assert!(text.contains("\"origin_class\":\"asked\""), "{text}");
        // A missing goal is logged, not a panic.
        assert!(publish_completion(&goals, &events, uuid::Uuid::new_v4(), true).is_none());
    }
}
