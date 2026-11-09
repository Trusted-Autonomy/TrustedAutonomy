// release_set.rs — one open release per project, cut by ONE pull request to main.
//
// Without a release, every `ta draft apply --git-commit` makes its own branch and
// its own PR to main, and every PR runs the project's CI. A release set replaces
// that: `ta release-set start` opens `release/<name>`, goals commit onto it
// locally, and `ta release-set cut` pushes once and opens the single PR.
//
// This module owns the *state* only: `.ta/release-set.json`, the drift advice
// (is a new goal on track for the release goal?), the umbrella goal record and
// the text the chief-of-staff reads. The git work lives in `ta-submit`
// (routing) and `ta-cli` (`ta release-set`).
//
// State machine:  (none) --start--> Open --cut--> Cut --start--> Open (new id)
// At most one release is Open per project; a Cut record is archived under
// `.ta/release-sets/` when the next release starts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::goal_run::{GoalRun, GoalRunState, RELEASE_OPEN_TAG};
use crate::store::GoalRunStore;

/// File name of the current release record, under `<project>/.ta/`.
pub const RELEASE_STATE_FILE: &str = "release-set.json";
/// Directory (under `<project>/.ta/`) that receives cut releases when the next one starts.
pub const RELEASE_HISTORY_DIR: &str = "release-sets";
/// Every release branch is `release/<name>`.
pub const BRANCH_PREFIX: &str = "release/";
/// Version of the on-disk record.
pub const SCHEMA_VERSION: u32 = 1;

/// How long a lock file may sit before it is treated as left by a dead process.
const LOCK_STALE_AFTER: Duration = Duration::from_secs(30);
const LOCK_RETRIES: u32 = 100;
const LOCK_RETRY_SLEEP: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStatus {
    Open,
    Cut,
}

impl ReleaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Cut => "cut",
        }
    }
}

/// The goal of the release itself: what the release is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseGoalSpec {
    pub title: String,
    pub objective: String,
    /// The Wayfinder goal this release delivers, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wayfinder_goal_id: Option<String>,
    /// Plan phases that belong to the release (e.g. `v0.17.12.34`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan_phases: Vec<String>,
    /// The umbrella (macro) goal record that represents the release in `ta goal list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_run_id: Option<Uuid>,
}

/// A goal whose draft was applied (committed) onto the release branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedGoal {
    pub goal_id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
    /// Full SHA of the commit on the release branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub landed_at: DateTime<Utc>,
}

/// A goal that opted out of the release (`--hotfix`) and went to main on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotfixGoal {
    pub goal_id: Uuid,
    pub title: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// A goal the drift check found to be off the release goal. Advice only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffTrackGoal {
    pub goal_id: Uuid,
    pub title: String,
    pub reasons: Vec<String>,
    pub flagged_at: DateTime<Utc>,
}

/// The persisted release record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseSet {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    /// `release/<name>`.
    pub branch: String,
    /// The branch the release was cut from and will be merged into (usually `main`).
    pub base_branch: String,
    /// The commit on `base_branch` the release branch started from.
    pub base_commit: String,
    pub goal: ReleaseGoalSpec,
    pub status: ReleaseStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_at: Option<DateTime<Utc>>,
    /// URL printed by `gh pr create` at cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub goals_landed: Vec<LandedGoal>,
    #[serde(default)]
    pub hotfixes: Vec<HotfixGoal>,
    #[serde(default)]
    pub off_track: Vec<OffTrackGoal>,
}

/// What `ta release-set start` supplies.
#[derive(Debug, Clone)]
pub struct NewRelease {
    pub name: String,
    pub objective: String,
    pub wayfinder_goal_id: Option<String>,
    pub plan_phases: Vec<String>,
    pub base_branch: String,
    pub base_commit: String,
}

/// Release membership recorded on a goal (`GoalRun.release`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GoalReleaseInfo {
    pub release_id: String,
    pub release_branch: String,
    /// Set when the goal opted out of the release with `--hotfix`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotfix_reason: Option<String>,
    /// Why the drift check flagged the goal. Empty when it is on track.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub off_track_reasons: Vec<String>,
}

impl GoalReleaseInfo {
    pub fn is_hotfix(&self) -> bool {
        self.hotfix_reason.is_some()
    }
    pub fn is_off_track(&self) -> bool {
        !self.is_hotfix() && !self.off_track_reasons.is_empty()
    }
}

/// Check a release name is usable as `release/<name>`.
pub fn validate_name(name: &str) -> Result<()> {
    let bad = |why: &str| {
        Err(anyhow!(
            "Invalid release name '{name}': {why}. Use letters, digits, '.', '_' or '-' \
             (for example `0.18` or `q3-launch`); the branch will be `release/<name>`."
        ))
    };
    if name.is_empty() {
        return bad("it is empty");
    }
    if name.len() > 64 {
        return bad("it is longer than 64 characters");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return bad("it contains a character other than letters, digits, '.', '_' or '-'");
    }
    if name.starts_with(['.', '-']) || name.ends_with('.') || name.contains("..") {
        return bad("it starts with '.' or '-', ends with '.', or contains '..'");
    }
    if name.ends_with(".lock") {
        return bad("it ends with '.lock'");
    }
    Ok(())
}

impl ReleaseSet {
    /// A new open release record. Does not touch git or the disk.
    pub fn open_new(spec: NewRelease) -> Result<Self> {
        validate_name(&spec.name)?;
        if spec.objective.trim().is_empty() {
            bail!(
                "A release needs a goal: pass `--goal \"<what this release delivers>\"`. \
                 The release PR is titled from it and every new goal is checked against it."
            );
        }
        let short: String = spec
            .objective
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        let short = truncate_chars(&short, 72);
        let id = format!(
            "{}-{}",
            spec.name,
            &Uuid::new_v4().simple().to_string()[..8]
        );
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            id,
            branch: format!("{BRANCH_PREFIX}{}", spec.name),
            goal: ReleaseGoalSpec {
                title: format!("Release {}: {short}", spec.name),
                objective: spec.objective,
                wayfinder_goal_id: spec.wayfinder_goal_id,
                plan_phases: spec.plan_phases,
                goal_run_id: None,
            },
            name: spec.name,
            base_branch: spec.base_branch,
            base_commit: spec.base_commit,
            status: ReleaseStatus::Open,
            created_at: Utc::now(),
            cut_at: None,
            pr_url: None,
            goals_landed: Vec::new(),
            hotfixes: Vec::new(),
            off_track: Vec::new(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.status == ReleaseStatus::Open
    }

    /// Record a landed goal; replaces an earlier record of the same goal (a
    /// re-applied draft) so the list never double counts.
    pub fn record_landed(&mut self, landed: LandedGoal) {
        self.goals_landed.retain(|g| g.goal_id != landed.goal_id);
        // The off-track advice is kept: a flagged goal that lands is exactly what the
        // reviewer of the release PR needs to see.
        self.goals_landed.push(landed);
    }

    /// Record a hotfix goal; a hotfix is never off track.
    pub fn record_hotfix(&mut self, hotfix: HotfixGoal) {
        self.hotfixes.retain(|g| g.goal_id != hotfix.goal_id);
        self.off_track.retain(|g| g.goal_id != hotfix.goal_id);
        self.hotfixes.push(hotfix);
    }

    /// Record (or refresh) the drift advice for a goal. Empty `reasons` clears it.
    pub fn record_off_track(&mut self, goal_id: Uuid, title: &str, reasons: Vec<String>) {
        self.off_track.retain(|g| g.goal_id != goal_id);
        if !reasons.is_empty() {
            self.off_track.push(OffTrackGoal {
                goal_id,
                title: title.to_string(),
                reasons,
                flagged_at: Utc::now(),
            });
        }
    }

    /// Transition Open -> Cut. Refuses a release that is already cut.
    pub fn mark_cut(&mut self, pr_url: Option<String>) -> Result<()> {
        if !self.is_open() {
            bail!(
                "Release '{}' is already cut ({}). Start a new one with \
                 `ta release-set start <name> --goal \"...\"`.",
                self.name,
                self.cut_at
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "time unknown".into())
            );
        }
        self.status = ReleaseStatus::Cut;
        self.cut_at = Some(Utc::now());
        self.pr_url = pr_url;
        Ok(())
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

// ── Store ────────────────────────────────────────────────────────────────────

/// Reads and writes `<project>/.ta/release-set.json`.
#[derive(Debug, Clone)]
pub struct ReleaseSetStore {
    ta_dir: PathBuf,
}

struct StateLock(PathBuf);

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl ReleaseSetStore {
    pub fn new(project_root: &Path) -> Self {
        Self {
            ta_dir: project_root.join(".ta"),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.ta_dir.join(RELEASE_STATE_FILE)
    }

    fn lock_path(&self) -> PathBuf {
        self.ta_dir.join("release-set.lock")
    }

    fn lock(&self) -> Result<StateLock> {
        std::fs::create_dir_all(&self.ta_dir)
            .with_context(|| format!("could not create {}", self.ta_dir.display()))?;
        let path = self.lock_path();
        for _ in 0..LOCK_RETRIES {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(StateLock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > LOCK_STALE_AFTER);
                    if stale {
                        tracing::warn!(lock = %path.display(), "removing a stale release-set lock");
                        let _ = std::fs::remove_file(&path);
                    } else {
                        std::thread::sleep(LOCK_RETRY_SLEEP);
                    }
                }
                Err(e) => {
                    return Err(anyhow!(
                        "could not take the release-set lock {}: {e}. Check that .ta/ is writable.",
                        path.display()
                    ))
                }
            }
        }
        Err(anyhow!(
            "Timed out waiting for the release-set lock {}: another `ta` command is updating \
             the release. If none is running, delete that file and retry.",
            path.display()
        ))
    }

    /// The current record (open or cut). `None` when no release was ever started.
    pub fn load(&self) -> Result<Option<ReleaseSet>> {
        let path = self.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(anyhow!(
                    "could not read {}: {e}. Check the file permissions.",
                    path.display()
                ))
            }
        };
        let rs: ReleaseSet = serde_json::from_str(&text).map_err(|e| {
            anyhow!(
                "{} is not a valid release record: {e}. Goals would silently go to main without \
                 it, so TA stops here. Fix or delete the file (the release branch itself is \
                 unaffected), then retry.",
                path.display()
            )
        })?;
        Ok(Some(rs))
    }

    /// The open release, if any.
    pub fn open_release(&self) -> Result<Option<ReleaseSet>> {
        Ok(self.load()?.filter(ReleaseSet::is_open))
    }

    fn write(&self, rs: &ReleaseSet) -> Result<()> {
        let json = serde_json::to_string_pretty(rs)?;
        let mut tmp = tempfile::NamedTempFile::new_in(&self.ta_dir).with_context(|| {
            format!("could not create a temp file in {}", self.ta_dir.display())
        })?;
        std::io::Write::write_all(&mut tmp, json.as_bytes())
            .with_context(|| "could not write the release record")?;
        tmp.persist(self.path())
            .map_err(|e| anyhow!("could not replace {}: {}", self.path().display(), e.error))?;
        Ok(())
    }

    /// Open a new release. Refuses when one is already open; archives a previous cut record.
    pub fn start(&self, spec: NewRelease) -> Result<ReleaseSet> {
        let _lock = self.lock()?;
        if let Some(existing) = self.load()? {
            if existing.is_open() {
                bail!(
                    "A release is already open: '{}' on branch {} (started {}). Only one release \
                     is open per project. Finish it with `ta release-set cut`, or inspect it with \
                     `ta release-set status`.",
                    existing.name,
                    existing.branch,
                    existing.created_at.to_rfc3339()
                );
            }
            let dir = self.ta_dir.join(RELEASE_HISTORY_DIR);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
            let archived = dir.join(format!("{}.json", existing.id));
            std::fs::write(&archived, serde_json::to_string_pretty(&existing)?)
                .with_context(|| format!("could not archive to {}", archived.display()))?;
        }
        let rs = ReleaseSet::open_new(spec)?;
        self.write(&rs)?;
        Ok(rs)
    }

    /// Locked read-modify-write of the open release. Errors when none is open.
    pub fn update<T>(&self, f: impl FnOnce(&mut ReleaseSet) -> Result<T>) -> Result<T> {
        let _lock = self.lock()?;
        let Some(mut rs) = self.open_release()? else {
            bail!(
                "No release is open. Start one with `ta release-set start <name> --goal \"...\"`."
            );
        };
        let out = f(&mut rs)?;
        self.write(&rs)?;
        Ok(out)
    }

    /// Like [`Self::update`] but only when `release_id` is still the open release.
    /// A release cut while a goal was applying must not be silently modified.
    pub fn update_if_open<T>(
        &self,
        release_id: &str,
        f: impl FnOnce(&mut ReleaseSet) -> Result<T>,
    ) -> Result<T> {
        self.update(|rs| {
            if rs.id != release_id {
                bail!(
                    "The open release is '{}' ({}), not the release '{release_id}' this goal \
                     started under.",
                    rs.name,
                    rs.id
                );
            }
            f(rs)
        })
    }

    /// Write back an edited record without the open-release requirement (used by `cut`
    /// after it has verified the release itself).
    pub fn save(&self, rs: &ReleaseSet) -> Result<()> {
        let _lock = self.lock()?;
        self.write(rs)
    }
}

// ── Drift advice ─────────────────────────────────────────────────────────────

/// Verdict of the deterministic goal-vs-release check. Advice only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftAssessment {
    pub on_track: bool,
    /// Human-readable reasons. Empty when on track.
    pub reasons: Vec<String>,
}

const STOPWORDS: &[&str] = &[
    "this",
    "that",
    "with",
    "from",
    "into",
    "have",
    "will",
    "should",
    "when",
    "then",
    "than",
    "also",
    "make",
    "adds",
    "added",
    "using",
    "used",
    "uses",
    "release",
    "goal",
    "phase",
    "implement",
    "support",
    "update",
    "fixes",
    "fixed",
    "work",
    "item",
    "items",
    "ta's",
    "your",
    "their",
    "there",
    "which",
    "while",
    "after",
    "before",
    "does",
    "need",
    "needs",
    "must",
];

fn keywords(text: &str) -> std::collections::BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4 && !STOPWORDS.contains(w))
        .map(|w| {
            w.strip_suffix('s')
                .filter(|s| s.len() >= 4)
                .unwrap_or(w)
                .to_string()
        })
        .collect()
}

fn norm_phase(p: &str) -> String {
    p.trim().trim_start_matches(['v', 'V']).to_lowercase()
}

/// Is a goal (title, objective, plan phase) on track for `release`?
///
/// Deterministic, no model call. A goal is on track when its plan phase is one
/// of the release's phases, or when its title+objective share keywords with the
/// release goal (title, objective and name): at least two shared words, or one
/// when the goal text has only one distinctive word. A goal with no
/// distinctive words at all cannot be judged and is treated as on track.
pub fn assess_goal(
    release: &ReleaseSet,
    title: &str,
    objective: &str,
    plan_phase: Option<&str>,
) -> DriftAssessment {
    let mut reasons = Vec::new();

    let phase_listed = match plan_phase {
        Some(p) if !release.goal.plan_phases.is_empty() => {
            let np = norm_phase(p);
            release
                .goal
                .plan_phases
                .iter()
                .any(|rp| norm_phase(rp) == np)
        }
        _ => false,
    };
    if phase_listed {
        return DriftAssessment {
            on_track: true,
            reasons,
        };
    }
    if let Some(p) = plan_phase {
        if !release.goal.plan_phases.is_empty() {
            reasons.push(format!(
                "plan phase {p} is not one of this release's phases ({})",
                release.goal.plan_phases.join(", ")
            ));
        }
    }

    let release_words = keywords(&format!(
        "{} {} {}",
        release.goal.title, release.goal.objective, release.name
    ));
    let goal_words = keywords(&format!("{title} {objective}"));
    if goal_words.is_empty() {
        return DriftAssessment {
            on_track: true,
            reasons: Vec::new(),
        };
    }
    let shared: Vec<&String> = goal_words.intersection(&release_words).collect();
    let needed = goal_words.len().min(2);
    if shared.len() >= needed {
        return DriftAssessment {
            on_track: true,
            reasons: Vec::new(),
        };
    }
    reasons.push(if shared.is_empty() {
        format!(
            "its title/objective share no keywords with the release goal \"{}\"",
            release.goal.title
        )
    } else {
        format!(
            "its title/objective share only {} keyword(s) with the release goal \"{}\" ({})",
            shared.len(),
            release.goal.title,
            shared
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    });
    DriftAssessment {
        on_track: false,
        reasons,
    }
}

// ── Umbrella goal ────────────────────────────────────────────────────────────

/// Create the release's umbrella goal: a macro goal in `custom:release_open`.
/// It shows in `ta goal list` as `[M]` with the release's goals as sub-goals.
pub fn create_umbrella_goal(
    goals: &GoalRunStore,
    release: &ReleaseSet,
    project_root: &Path,
    store_dir: &Path,
) -> Result<GoalRun> {
    let mut g = GoalRun::new(
        release.goal.title.clone(),
        release.goal.objective.clone(),
        "release-set",
        project_root.to_path_buf(),
        store_dir.join(format!("release-{}", release.id)),
    );
    g.is_macro = true;
    g.source_dir = Some(project_root.to_path_buf());
    g.progress_note = Some(format!(
        "release {} open on {}",
        release.name, release.branch
    ));
    g.transition(GoalRunState::Custom {
        tag: RELEASE_OPEN_TAG.to_string(),
    })?;
    goals.save_with_tag(&mut g)?;
    Ok(g)
}

/// Link `goal` under the release's umbrella goal (`parent_macro_id` / `sub_goal_ids`).
/// A missing umbrella record (deleted by hand) is reported, not fatal.
pub fn attach_to_umbrella(
    goals: &GoalRunStore,
    release: &ReleaseSet,
    goal: &mut GoalRun,
) -> Result<()> {
    let Some(umbrella_id) = release.goal.goal_run_id else {
        return Ok(());
    };
    let Some(mut umbrella) = goals.get(umbrella_id)? else {
        tracing::warn!(
            release = %release.id,
            umbrella = %umbrella_id,
            "the release's umbrella goal record is missing; goal not linked under it"
        );
        return Ok(());
    };
    goal.parent_macro_id = Some(umbrella_id);
    if !umbrella.sub_goal_ids.contains(&goal.goal_run_id) {
        umbrella.sub_goal_ids.push(goal.goal_run_id);
        goals.save(&umbrella)?;
    }
    Ok(())
}

/// Complete the umbrella goal when the release is cut.
pub fn complete_umbrella(goals: &GoalRunStore, release: &ReleaseSet) -> Result<()> {
    let Some(id) = release.goal.goal_run_id else {
        return Ok(());
    };
    if let Some(mut g) = goals.get(id)? {
        if matches!(&g.state, GoalRunState::Custom { tag } if tag == RELEASE_OPEN_TAG) {
            g.transition(GoalRunState::Completed)?;
            g.progress_note = Some(format!(
                "release {} cut{}",
                release.name,
                release
                    .pr_url
                    .as_deref()
                    .map(|u| format!(": {u}"))
                    .unwrap_or_default()
            ));
            goals.save(&g)?;
        }
    }
    Ok(())
}

// ── Status JSON ──────────────────────────────────────────────────────────────

/// `ta release-set status --json`: `{ "release": { ... } }`. Stable schema; see docs/USAGE.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    pub release: StatusRelease,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusGoal {
    pub title: String,
    pub objective: String,
    pub wayfinder_goal_id: Option<String>,
    pub plan_phases: Vec<String>,
    pub goal_run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRelease {
    pub id: String,
    pub name: String,
    pub branch: String,
    /// `"open"` or `"cut"`.
    pub status: String,
    pub base_branch: String,
    pub base_commit: String,
    pub created_at: DateTime<Utc>,
    pub cut_at: Option<DateTime<Utc>>,
    pub pr_url: Option<String>,
    pub goal: StatusGoal,
    pub goals_landed: Vec<LandedGoal>,
    pub hotfixes: Vec<HotfixGoal>,
    pub off_track: Vec<OffTrackGoal>,
    /// Commits on the release branch that main does not have. `null` when git could not say.
    pub ahead: Option<u64>,
    /// Commits on main that the release branch does not have. `null` when git could not say.
    pub behind: Option<u64>,
    /// How many merges to main (so CI runs) this release costs: always 1, at cut.
    pub merges_to_main: u32,
}

impl ReleaseSet {
    pub fn status_report(&self, ahead: Option<u64>, behind: Option<u64>) -> StatusReport {
        StatusReport {
            release: StatusRelease {
                id: self.id.clone(),
                name: self.name.clone(),
                branch: self.branch.clone(),
                status: self.status.as_str().to_string(),
                base_branch: self.base_branch.clone(),
                base_commit: self.base_commit.clone(),
                created_at: self.created_at,
                cut_at: self.cut_at,
                pr_url: self.pr_url.clone(),
                goal: StatusGoal {
                    title: self.goal.title.clone(),
                    objective: self.goal.objective.clone(),
                    wayfinder_goal_id: self.goal.wayfinder_goal_id.clone(),
                    plan_phases: self.goal.plan_phases.clone(),
                    goal_run_id: self.goal.goal_run_id,
                },
                goals_landed: self.goals_landed.clone(),
                hotfixes: self.hotfixes.clone(),
                off_track: self.off_track.clone(),
                ahead,
                behind,
                merges_to_main: 1,
            },
        }
    }
}

// ── Chief-of-staff context ───────────────────────────────────────────────────

/// The release section of the chief-of-staff context, or `None` when no release is open.
///
/// A corrupt record is reported in the section rather than hidden: the CoS
/// should tell the human that goals are routing blind.
pub fn render_cos_section(project_root: &Path) -> Option<String> {
    let store = ReleaseSetStore::new(project_root);
    let rs = match store.open_release() {
        Ok(Some(rs)) => rs,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "could not read the release record for the CoS context");
            return Some(format!(
                "## Release in progress\n\nThe release record could not be read ({e}). Tell the \
                 human; new goals cannot be routed to a release branch until it is fixed.\n\n"
            ));
        }
    };
    let mut s = String::new();
    s.push_str("## Release in progress\n\n");
    s.push_str(&format!("Release goal: {}\n", rs.goal.title));
    s.push_str(&format!("Objective: {}\n", rs.goal.objective.trim()));
    if !rs.goal.plan_phases.is_empty() {
        s.push_str(&format!(
            "Plan phases: {}\n",
            rs.goal.plan_phases.join(", ")
        ));
    }
    if let Some(w) = &rs.goal.wayfinder_goal_id {
        s.push_str(&format!("Wayfinder goal: {w}\n"));
    }
    s.push_str(&format!(
        "Release branch: {} (from {} at {})\n",
        rs.branch,
        rs.base_branch,
        rs.base_commit.chars().take(12).collect::<String>()
    ));
    s.push_str(&format!(
        "Goals landed: {}; hotfixes: {}; off-track goals: {}\n",
        rs.goals_landed.len(),
        rs.hotfixes.len(),
        rs.off_track.len()
    ));
    for o in &rs.off_track {
        s.push_str(&format!(
            "- OFF TRACK: \"{}\" ({}): {}\n",
            o.title,
            o.goal_id.to_string().chars().take(8).collect::<String>(),
            o.reasons.join("; ")
        ));
    }
    s.push_str(
        "\nStanding rule for the release: goals commit onto the release branch and open NO \
         per-goal PR; `ta release-set cut` opens the one PR to main. Advise the human when a \
         goal or request drifts from the release goal (advice only: never block, never refuse). \
         Recommend `--hotfix --hotfix-reason \"...\"` only for a genuine urgent fix that must \
         reach main without the release.\n\n",
    );
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, objective: &str) -> NewRelease {
        NewRelease {
            name: name.into(),
            objective: objective.into(),
            wayfinder_goal_id: None,
            plan_phases: Vec::new(),
            base_branch: "main".into(),
            base_commit: "0123456789abcdef".into(),
        }
    }

    #[test]
    fn name_validation() {
        for ok in ["0.18", "q3-launch", "a_b.c", "R1"] {
            validate_name(ok).unwrap();
        }
        for bad in ["", "a b", "a/b", "-x", ".x", "x.", "a..b", "x.lock", "ünï"] {
            assert!(validate_name(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(validate_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn open_new_derives_branch_title_and_requires_goal() {
        let rs = ReleaseSet::open_new(spec("0.18", "Ship the billing rewrite\nmore")).unwrap();
        assert_eq!(rs.branch, "release/0.18");
        assert_eq!(rs.goal.title, "Release 0.18: Ship the billing rewrite");
        assert!(rs.is_open());
        assert!(ReleaseSet::open_new(spec("0.18", "   ")).is_err());
        assert!(ReleaseSet::open_new(spec("bad name", "x")).is_err());
    }

    #[test]
    fn start_refuses_second_open_release_and_archives_cut_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReleaseSetStore::new(dir.path());
        assert!(store.load().unwrap().is_none());
        let first = store.start(spec("one", "first release goal")).unwrap();

        let err = store.start(spec("two", "second")).unwrap_err().to_string();
        assert!(
            err.contains("already open") && err.contains("release/one"),
            "{err}"
        );
        assert!(
            err.contains("ta release-set cut"),
            "must say what to do: {err}"
        );

        store.update(|rs| rs.mark_cut(None)).unwrap();
        assert!(store.open_release().unwrap().is_none());
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.status, ReleaseStatus::Cut);
        assert!(loaded.cut_at.is_some());

        let second = store.start(spec("two", "second release goal")).unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(store.open_release().unwrap().unwrap().name, "two");
        let archived = dir
            .path()
            .join(".ta")
            .join(RELEASE_HISTORY_DIR)
            .join(format!("{}.json", first.id));
        assert!(archived.exists(), "the cut release is archived");
    }

    #[test]
    fn cut_twice_is_refused_and_update_needs_an_open_release() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReleaseSetStore::new(dir.path());
        assert!(store.update(|_| Ok(())).is_err());
        store.start(spec("one", "goal words here")).unwrap();
        store
            .update(|rs| rs.mark_cut(Some("http://pr/1".into())))
            .unwrap();
        let err = store
            .update(|rs| rs.mark_cut(None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("No release is open"), "{err}");
        let mut cut = store.load().unwrap().unwrap();
        assert_eq!(cut.pr_url.as_deref(), Some("http://pr/1"));
        assert!(cut
            .mark_cut(None)
            .unwrap_err()
            .to_string()
            .contains("already cut"));
    }

    #[test]
    fn landed_hotfix_and_off_track_records_are_idempotent() {
        let mut rs = ReleaseSet::open_new(spec("one", "goal words here")).unwrap();
        let g = Uuid::new_v4();
        rs.record_off_track(g, "t", vec!["why".into()]);
        assert_eq!(rs.off_track.len(), 1);
        rs.record_landed(LandedGoal {
            goal_id: g,
            title: "t".into(),
            plan_phase: None,
            draft_id: None,
            commit: Some("abc".into()),
            landed_at: Utc::now(),
        });
        rs.record_landed(LandedGoal {
            goal_id: g,
            title: "t".into(),
            plan_phase: None,
            draft_id: None,
            commit: Some("def".into()),
            landed_at: Utc::now(),
        });
        assert_eq!(
            rs.goals_landed.len(),
            1,
            "a re-applied draft is not double counted"
        );
        assert_eq!(rs.goals_landed[0].commit.as_deref(), Some("def"));
        assert_eq!(
            rs.off_track.len(),
            1,
            "landing keeps the off-track note: the goal is in the release now, and the advice stays visible until the cut"
        );

        let h = Uuid::new_v4();
        rs.record_off_track(h, "fix", vec!["why".into()]);
        rs.record_hotfix(HotfixGoal {
            goal_id: h,
            title: "fix".into(),
            reason: "prod down".into(),
            commit: None,
            recorded_at: Utc::now(),
        });
        assert_eq!(rs.hotfixes.len(), 1);
        assert!(
            rs.off_track.iter().all(|o| o.goal_id != h),
            "a hotfix is never off track"
        );
    }

    #[test]
    fn corrupt_state_is_an_error_not_silent_main_routing() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReleaseSetStore::new(dir.path());
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(store.path(), "{ not json").unwrap();
        let err = store.open_release().unwrap_err().to_string();
        assert!(err.contains("release-set.json") && err.contains("not a valid release record"));
    }

    fn billing_release() -> ReleaseSet {
        let mut s = spec("0.18", "Rewrite the billing invoice export pipeline");
        s.plan_phases = vec!["v0.18.1".into(), "v0.18.2".into()];
        ReleaseSet::open_new(s).unwrap()
    }

    #[test]
    fn drift_plan_phase_membership_wins() {
        let rs = billing_release();
        let a = assess_goal(&rs, "Unrelated wording", "nothing shared", Some("0.18.2"));
        assert!(
            a.on_track,
            "a listed phase is on track even without keyword overlap"
        );
        let b = assess_goal(&rs, "Unrelated wording", "nothing shared", Some("v0.99"));
        assert!(!b.on_track);
        assert!(b
            .reasons
            .iter()
            .any(|r| r.contains("v0.99") && r.contains("v0.18.1")));
    }

    #[test]
    fn drift_keyword_overlap() {
        let rs = billing_release();
        assert!(assess_goal(&rs, "Fix invoice export rounding", "", None).on_track);
        let off = assess_goal(
            &rs,
            "Redesign the landing page hero",
            "marketing copy",
            None,
        );
        assert!(!off.on_track);
        assert!(
            off.reasons[0].contains("share no keywords"),
            "{:?}",
            off.reasons
        );
        // One distinctive word in common is not enough when the goal has several.
        let weak = assess_goal(&rs, "Billing dashboard colour palette", "", None);
        assert!(!weak.on_track);
        assert!(
            weak.reasons[0].contains("only 1 keyword"),
            "{:?}",
            weak.reasons
        );
        // Nothing to judge: never flagged.
        assert!(assess_goal(&rs, "ok", "", None).on_track);
    }

    #[test]
    fn drift_is_deterministic() {
        let rs = billing_release();
        let a = assess_goal(&rs, "Redesign the landing page", "x", None);
        let b = assess_goal(&rs, "Redesign the landing page", "x", None);
        assert_eq!(a, b);
    }

    #[test]
    fn status_report_schema_is_stable() {
        let mut rs = billing_release();
        rs.goal.wayfinder_goal_id = Some("wf-1".into());
        let v = serde_json::to_value(rs.status_report(Some(2), None)).unwrap();
        let r = &v["release"];
        for key in [
            "id",
            "name",
            "branch",
            "status",
            "goal",
            "goals_landed",
            "hotfixes",
            "off_track",
            "ahead",
            "behind",
        ] {
            assert!(r.get(key).is_some(), "missing release.{key}: {v}");
        }
        assert_eq!(r["branch"], "release/0.18");
        assert_eq!(r["status"], "open");
        assert_eq!(r["goal"]["wayfinder_goal_id"], "wf-1");
        assert_eq!(r["ahead"], 2);
        assert!(r["behind"].is_null());
        assert_eq!(r["merges_to_main"], 1);
        for key in ["title", "objective", "wayfinder_goal_id"] {
            assert!(r["goal"].get(key).is_some(), "missing release.goal.{key}");
        }
    }

    #[test]
    fn umbrella_goal_is_a_macro_goal_with_linked_sub_goals() {
        let dir = tempfile::tempdir().unwrap();
        let goals = GoalRunStore::new(dir.path().join("goals")).unwrap();
        let mut rs = billing_release();
        let umbrella =
            create_umbrella_goal(&goals, &rs, dir.path(), &dir.path().join("store")).unwrap();
        assert!(umbrella.is_macro);
        assert_eq!(
            umbrella.state,
            GoalRunState::Custom {
                tag: RELEASE_OPEN_TAG.into()
            }
        );
        rs.goal.goal_run_id = Some(umbrella.goal_run_id);

        let mut sub = GoalRun::new(
            "Fix invoice export",
            "x",
            "agent",
            dir.path().into(),
            dir.path().into(),
        );
        attach_to_umbrella(&goals, &rs, &mut sub).unwrap();
        assert_eq!(sub.parent_macro_id, Some(umbrella.goal_run_id));
        let reloaded = goals.get(umbrella.goal_run_id).unwrap().unwrap();
        assert_eq!(reloaded.sub_goal_ids, vec![sub.goal_run_id]);
        // Attaching twice does not duplicate.
        attach_to_umbrella(&goals, &rs, &mut sub).unwrap();
        assert_eq!(
            goals
                .get(umbrella.goal_run_id)
                .unwrap()
                .unwrap()
                .sub_goal_ids
                .len(),
            1
        );

        complete_umbrella(&goals, &rs).unwrap();
        assert_eq!(
            goals.get(umbrella.goal_run_id).unwrap().unwrap().state,
            GoalRunState::Completed
        );
    }

    #[test]
    fn umbrella_state_transitions_are_narrow() {
        let open = GoalRunState::Custom {
            tag: RELEASE_OPEN_TAG.into(),
        };
        assert!(GoalRunState::Created.can_transition_to(&open));
        assert!(!GoalRunState::Running.can_transition_to(&open));
        assert!(open.can_transition_to(&GoalRunState::Completed));
        assert!(!open.can_transition_to(&GoalRunState::Running));
        assert!(open.can_transition_to(&GoalRunState::Failed { reason: "x".into() }));
    }

    #[test]
    fn cos_section_names_goal_branch_and_off_track_goals() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            render_cos_section(dir.path()).is_none(),
            "no release, no section"
        );
        let store = ReleaseSetStore::new(dir.path());
        store
            .start(spec("0.18", "Rewrite the billing invoice export"))
            .unwrap();
        let g = Uuid::new_v4();
        store
            .update(|rs| {
                rs.record_off_track(
                    g,
                    "Redesign landing page",
                    vec!["no shared keywords".into()],
                );
                Ok(())
            })
            .unwrap();
        let text = render_cos_section(dir.path()).unwrap();
        assert!(text.contains("Release goal: Release 0.18: Rewrite the billing invoice export"));
        assert!(text.contains("release/0.18"));
        assert!(text.contains("OFF TRACK: \"Redesign landing page\""));
        assert!(text.contains("ta release-set cut") && text.contains("--hotfix"));

        store.update(|rs| rs.mark_cut(None)).unwrap();
        assert!(
            render_cos_section(dir.path()).is_none(),
            "a cut release is not in progress"
        );
    }
}
