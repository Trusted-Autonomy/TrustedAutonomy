//! Scan: look at `.ta/`, the source tree and the VCS, and give every finding a
//! verdict and a reason. Pure rules, no model call, no changes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use ta_changeset::draft_package::{ChangeType, DraftPackage, DraftStatus};
use ta_goal::{GoalRun, GoalRunState, GoalRunStore};
use uuid::Uuid;

use crate::model::*;

pub fn short(id: &Uuid) -> String {
    id.to_string()[..8].to_string()
}

/// Hash of the facts that decide a verdict. Ids, titles and ages are left out
/// so the same *situation* is recognised the next time it appears.
pub fn signature(kind: Kind, facts: &[(&str, String)]) -> String {
    let mut sorted: Vec<String> = facts.iter().map(|(k, v)| format!("{k}={v}")).collect();
    sorted.sort();
    let mut h = Sha256::new();
    h.update(kind.as_str().as_bytes());
    for s in sorted {
        h.update(b"\n");
        h.update(s.as_bytes());
    }
    let hex = format!("{:x}", h.finalize());
    hex[..16].to_string()
}

pub fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            total += dir_size(&e.path());
        }
    }
    total
}

pub fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn is_terminal(s: &GoalRunState) -> bool {
    matches!(
        s,
        GoalRunState::Applied
            | GoalRunState::Merged
            | GoalRunState::Completed
            | GoalRunState::Closed { .. }
            | GoalRunState::Failed { .. }
    )
}

fn landed(s: &GoalRunState) -> bool {
    matches!(
        s,
        GoalRunState::Applied
            | GoalRunState::Merged
            | GoalRunState::Completed
            | GoalRunState::Closed {
                applied_externally_ref: Some(_),
                ..
            }
    )
}

fn in_review(s: &GoalRunState) -> bool {
    matches!(
        s,
        GoalRunState::PrReady | GoalRunState::UnderReview | GoalRunState::Approved { .. }
    )
}

fn draft_final(s: &DraftStatus) -> bool {
    matches!(
        s,
        DraftStatus::Applied { .. }
            | DraftStatus::Denied { .. }
            | DraftStatus::Closed { .. }
            | DraftStatus::Superseded { .. }
    )
}

fn draft_live(s: &DraftStatus) -> bool {
    matches!(s, DraftStatus::Approved { .. } | DraftStatus::PendingReview)
}

fn state_class(s: &GoalRunState) -> &'static str {
    if landed(s) {
        "landed"
    } else if is_terminal(s) {
        "abandoned"
    } else {
        "live"
    }
}

/// PR number (or URL) recorded on a draft, if any.
pub fn review_ref(pkg: &DraftPackage) -> Option<String> {
    let v = pkg.vcs_status.as_ref()?;
    if let Some(id) = v.review_id.as_deref().filter(|s| !s.trim().is_empty()) {
        return Some(id.trim().trim_start_matches('#').to_string());
    }
    v.review_url
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
}

fn pr_label(r: &str) -> String {
    if r.chars().all(|c| c.is_ascii_digit()) {
        format!("PR #{r}")
    } else {
        r.to_string()
    }
}

pub(crate) struct Scan<'a> {
    pub e: &'a Engine<'a>,
    pub goals: Vec<GoalRun>,
    pub drafts: Vec<DraftPackage>,
    probes: RefCell<HashMap<String, ReviewState>>,
}

impl<'a> Scan<'a> {
    fn new(e: &'a Engine<'a>) -> anyhow::Result<Self> {
        let goals = if e.paths.goals_dir.exists() {
            GoalRunStore::new(&e.paths.goals_dir)?.list()?
        } else {
            Vec::new()
        };
        Ok(Scan {
            e,
            goals,
            drafts: load_drafts(&e.paths.pr_packages_dir),
            probes: RefCell::new(HashMap::new()),
        })
    }

    fn probe(&self, r: &str) -> ReviewState {
        if let Some(s) = self.probes.borrow().get(r) {
            return s.clone();
        }
        let s = self.e.truth.review_state(r);
        self.probes.borrow_mut().insert(r.to_string(), s.clone());
        s
    }

    fn goal_for(&self, pkg: &DraftPackage) -> Option<&GoalRun> {
        self.goals
            .iter()
            .find(|g| g.pr_package_id == Some(pkg.package_id))
            .or_else(|| {
                Uuid::parse_str(&pkg.goal.goal_id)
                    .ok()
                    .and_then(|id| self.goals.iter().find(|g| g.goal_run_id == id))
            })
    }

    fn draft_of(&self, g: &GoalRun) -> Option<&DraftPackage> {
        let id = g.pr_package_id?;
        self.drafts.iter().find(|p| p.package_id == id)
    }

    /// Another goal that landed `phase`.
    fn phase_landed_by(&self, phase: &str, not: Option<Uuid>) -> Option<&GoalRun> {
        self.goals.iter().find(|g| {
            Some(g.goal_run_id) != not
                && g.plan_phase
                    .as_deref()
                    .is_some_and(|p| p.trim() == phase.trim())
                && landed(&g.state)
        })
    }
}

pub fn load_drafts(dir: &Path) -> Vec<DraftPackage> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") {
            match std::fs::read_to_string(&p)
                .map_err(anyhow::Error::from)
                .and_then(|s| serde_json::from_str::<DraftPackage>(&s).map_err(Into::into))
            {
                Ok(pkg) => out.push(pkg),
                Err(err) => {
                    tracing::warn!(path = %p.display(), error = %err, "cleanup: skipping unreadable draft record");
                }
            }
        }
    }
    out
}

/// Staging against source, for the files a draft changes.
#[derive(Debug, PartialEq, Eq)]
enum SourceCmp {
    /// Every checked file already matches source: nothing is left to apply.
    Equal(usize),
    Differs {
        same: usize,
        total: usize,
    },
    NoStaging,
    NoArtifacts,
}

fn compare_source(s: &Scan, goal: Option<&GoalRun>, pkg: &DraftPackage) -> SourceCmp {
    let staging = goal
        .map(|g| g.workspace_path.clone())
        .filter(|p| p.exists())
        .unwrap_or_else(|| s.e.paths.staging_dir.join(&pkg.goal.goal_id));
    if !staging.is_dir() {
        return SourceCmp::NoStaging;
    }
    let (mut same, mut total) = (0usize, 0usize);
    for a in &pkg.changes.artifacts {
        let Some(rel) = a.resource_uri.strip_prefix("fs://workspace/") else {
            continue;
        };
        // PLAN.md is merged by the apply step, never copied, so it never matches.
        if rel == "PLAN.md" || rel.starts_with(".ta/") {
            continue;
        }
        total += 1;
        let src = s.e.paths.root.join(rel);
        let stg = staging.join(rel);
        let eq = match a.change_type {
            ChangeType::Delete => !src.exists(),
            ChangeType::Rename => false,
            _ => match (std::fs::read(&stg), std::fs::read(&src)) {
                (Ok(x), Ok(y)) => x == y,
                _ => false,
            },
        };
        if eq {
            same += 1;
        }
    }
    if total == 0 {
        SourceCmp::NoArtifacts
    } else if same == total {
        SourceCmp::Equal(total)
    } else {
        SourceCmp::Differs { same, total }
    }
}

pub fn scan(e: &Engine, only: Option<&[Category]>) -> anyhow::Result<Vec<Finding>> {
    let s = Scan::new(e)?;
    let want = |c: Category| only.is_none_or(|o| o.contains(&c));
    let mut out = Vec::new();
    if want(Category::Drafts) {
        stuck_drafts(&s, &mut out);
        stale_labels(&s, &mut out);
    }
    if want(Category::Goals) {
        stuck_goals(&s, &mut out);
    }
    if want(Category::Staging) {
        staging(&s, &mut out);
    }
    if want(Category::Locks) {
        locks(&s, &mut out);
    }
    if want(Category::Archive) {
        archive(&s, &mut out);
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn mk(
    kind: Kind,
    id: String,
    title: String,
    evidence: Vec<Evidence>,
    verdict: Verdict,
    reason: impl Into<String>,
    action: Action,
    facts: &[(&str, String)],
) -> Finding {
    Finding {
        kind,
        id,
        title,
        evidence,
        verdict,
        reason: reason.into(),
        action,
        options: Vec::new(),
        recommended: None,
        signature: signature(kind, facts),
        bytes: 0,
        manual_command: None,
        remembered: None,
        suggestion: None,
    }
}

fn stuck_drafts(s: &Scan, out: &mut Vec<Finding>) {
    let cutoff = Utc::now() - Duration::days(s.e.settings.stale_days as i64);
    for pkg in &s.drafts {
        if !draft_live(&pkg.status) {
            continue;
        }
        // Age is the safety margin for work that may still be in flight. A draft whose goal
        // has already finished has nothing in flight, so it is judged on the evidence at any
        // age; a young draft with a live or unknown goal is left alone.
        let goal_finished = s
            .goal_for(pkg)
            .is_some_and(|g| state_class(&g.state) != "live");
        if pkg.created_at > cutoff && !goal_finished {
            continue;
        }
        let sh = short(&pkg.package_id);
        let goal = s.goal_for(pkg);
        let age = (Utc::now() - pkg.created_at).num_days();
        let status = pkg.status.to_string();
        let mut ev = vec![Evidence::new(
            "draft",
            format!(
                "{} for {} day(s), {} file(s) changed, titled \"{}\"",
                status,
                age,
                pkg.changes.artifacts.len(),
                pkg.goal.title
            ),
        )];

        // 1. Goal state.
        let class = match goal {
            Some(g) => {
                ev.push(Evidence::new(
                    "goal",
                    format!("goal {} is {}", g.display_tag(), g.state),
                ));
                state_class(&g.state)
            }
            None => {
                ev.push(Evidence::new(
                    "goal",
                    "no goal record exists for this draft",
                ));
                "missing"
            }
        };

        // 2. Plan phase.
        let phase = pkg
            .plan_phase
            .clone()
            .or_else(|| goal.and_then(|g| g.plan_phase.clone()));
        let mut phase_state = "none";
        let mut superseding: Option<&GoalRun> = None;
        if let Some(ph) = &phase {
            let own = goal.map(|g| g.goal_run_id);
            superseding = s.phase_landed_by(ph, own);
            if superseding.is_some() || s.e.truth.phase_done(ph) {
                phase_state = "done";
                match superseding {
                    Some(o) => ev.push(Evidence::new(
                        "plan",
                        format!("phase {ph} is done, landed by goal {}", o.display_tag()),
                    )),
                    None => ev.push(Evidence::new(
                        "plan",
                        format!("phase {ph} is marked done in PLAN.md"),
                    )),
                }
            } else {
                phase_state = "open";
                ev.push(Evidence::new("plan", format!("phase {ph} is not done")));
            }
        }

        // 3. VCS.
        let rref = review_ref(pkg);
        let probe = rref.as_ref().map(|r| s.probe(r));
        let vcs_class = match (&rref, &probe) {
            (None, _) => {
                ev.push(Evidence::new("vcs", "the draft records no branch or PR"));
                "none"
            }
            (Some(r), Some(ReviewState::Merged)) => {
                ev.push(Evidence::new("vcs", format!("{} is MERGED", pr_label(r))));
                "merged"
            }
            (Some(r), Some(ReviewState::Closed)) => {
                ev.push(Evidence::new(
                    "vcs",
                    format!("{} was CLOSED without merging", pr_label(r)),
                ));
                "closed"
            }
            (Some(r), Some(ReviewState::Open)) => {
                ev.push(Evidence::new(
                    "vcs",
                    format!("{} is still open", pr_label(r)),
                ));
                "open"
            }
            (Some(r), Some(ReviewState::Unknown(why))) => {
                ev.push(Evidence::new(
                    "vcs",
                    format!(
                        "could not look up {} ({why}); treated as unknown",
                        pr_label(r)
                    ),
                ));
                "unknown"
            }
            _ => "none",
        };

        // 4. Staging against source.
        let cmp = compare_source(s, goal, pkg);
        let cmp_class = match &cmp {
            SourceCmp::Equal(n) => {
                ev.push(Evidence::new(
                    "source",
                    format!(
                        "all {n} changed file(s) already match your project: nothing left to apply"
                    ),
                ));
                "equal"
            }
            SourceCmp::Differs { same, total } => {
                ev.push(Evidence::new(
                    "source",
                    format!("{same} of {total} changed file(s) match your project; the rest are not applied"),
                ));
                "differs"
            }
            SourceCmp::NoStaging => {
                ev.push(Evidence::new(
                    "source",
                    "staging is gone, so the changes cannot be compared",
                ));
                "no_staging"
            }
            SourceCmp::NoArtifacts => {
                ev.push(Evidence::new(
                    "source",
                    "the draft lists no file changes to compare",
                ));
                "no_artifacts"
            }
        };

        let facts = [
            ("draft", status.clone()),
            ("goal", class.to_string()),
            ("phase", phase_state.to_string()),
            ("vcs", vcs_class.to_string()),
            ("source", cmp_class.to_string()),
        ];
        let title = format!("draft {sh} \"{}\"", pkg.goal.title);
        let close = |reason: &str, ext: Option<String>| Action::CloseDraft {
            draft_id: pkg.package_id,
            reason: reason.to_string(),
            applied_externally_ref: ext,
        };
        let id = format!("draft:{sh}");

        let mut f = if vcs_class == "merged" {
            let r = rref.clone().unwrap_or_default();
            mk(
                Kind::StuckDraft,
                id,
                title,
                ev,
                Verdict::AutoSafe,
                format!(
                    "already_landed: {} is merged, so this draft's change shipped through the PR",
                    pr_label(&r)
                ),
                close(
                    "already landed via merged PR (closed by ta cleanup)",
                    Some(pr_label(&r)),
                ),
                &facts,
            )
        } else if cmp_class == "equal" {
            mk(
                Kind::StuckDraft,
                id,
                title,
                ev,
                Verdict::AutoSafe,
                "already_landed: every file this draft changes already matches your project",
                close(
                    "changes already present in source (closed by ta cleanup)",
                    Some("matches-source".to_string()),
                ),
                &facts,
            )
        } else if phase_state == "done"
            && (superseding.is_some() || goal.is_none() || class != "live")
        {
            let (v, why) = if class == "live" {
                (Verdict::Propose, "its goal is still live")
            } else {
                (Verdict::AutoSafe, "its goal is already finished")
            };
            mk(
                Kind::StuckDraft,
                id,
                title,
                ev,
                v,
                format!("superseded_by_phase_done: the phase is already done and {why}"),
                close(
                    "superseded: plan phase already done (closed by ta cleanup)",
                    None,
                ),
                &facts,
            )
        } else if class == "landed" {
            mk(
                Kind::StuckDraft,
                id,
                title,
                ev,
                Verdict::Propose,
                "the goal record says its work landed, but the draft was never marked applied",
                close(
                    "goal already landed (closed by ta cleanup)",
                    Some("goal-landed".to_string()),
                ),
                &facts,
            )
        } else if vcs_class == "closed" && class != "live" {
            mk(
                Kind::StuckDraft,
                id,
                title,
                ev,
                Verdict::Propose,
                "abandoned: its PR was closed without merging and its goal is finished",
                close("abandoned: PR closed unmerged (closed by ta cleanup)", None),
                &facts,
            )
        } else if class == "abandoned" && vcs_class == "none" {
            mk(
                Kind::StuckDraft,
                id,
                title,
                ev,
                Verdict::Propose,
                "abandoned: its goal was closed or failed and the draft has no branch or PR",
                close(
                    "abandoned: goal finished without applying (closed by ta cleanup)",
                    None,
                ),
                &facts,
            )
        } else if vcs_class == "open" {
            mk(
                Kind::StuckDraft,
                id,
                title,
                ev,
                Verdict::Skip,
                "its PR is still open, so review continues there",
                Action::None,
                &facts,
            )
        } else {
            // Evidence conflicts or is missing: never a destructive default.
            let why = match (class, vcs_class) {
                ("live", _) => "its goal is still live and its changes are not in your project",
                (_, "unknown") => "the VCS could not be asked, so the PR state is unknown",
                _ => "the evidence does not settle whether this work is wanted",
            };
            let mut f = mk(
                Kind::StuckDraft,
                id,
                title,
                ev,
                Verdict::Ask,
                format!("unknown: {why}"),
                Action::None,
                &facts,
            );
            f.options = vec![
                AnswerOption {
                    key: "keep".into(),
                    label: format!("Keep it and decide later (apply with `ta draft apply {sh}`)"),
                    action: Action::None,
                },
                AnswerOption {
                    key: "abandon".into(),
                    label: "Close it as abandoned (the changes are not wanted)".into(),
                    action: close("abandoned: closed by a person via ta cleanup", None),
                },
                AnswerOption {
                    key: "landed".into(),
                    label: "Close it as already landed (the change shipped another way)".into(),
                    action: close(
                        "landed outside TA: confirmed by a person via ta cleanup",
                        Some("confirmed-by-human".to_string()),
                    ),
                },
            ];
            f.recommended = Some("keep".into());
            f
        };
        f.manual_command = Some(match f.verdict {
            Verdict::Ask => {
                format!("ta draft apply {sh}   |   ta draft close {sh} --reason \"...\"")
            }
            _ => format!("ta draft close {sh} --reason \"...\""),
        });
        out.push(f);
    }
}

fn stale_labels(s: &Scan, out: &mut Vec<Finding>) {
    for pkg in &s.drafts {
        let Some(v) = pkg.vcs_status.as_ref() else {
            continue;
        };
        if !matches!(
            v.review_state.as_deref(),
            Some("open") | Some("draft") | None
        ) {
            continue;
        }
        let Some(r) = review_ref(pkg) else { continue };
        let new_state = match s.probe(&r) {
            ReviewState::Merged => "merged",
            ReviewState::Closed => "closed",
            _ => continue,
        };
        let sh = short(&pkg.package_id);
        out.push(mk(
            Kind::StaleVcsLabel,
            format!("label:{sh}"),
            format!("draft {sh} shows {} as open", pr_label(&r)),
            vec![
                Evidence::new(
                    "draft",
                    format!(
                        "stored review state is {}",
                        v.review_state.as_deref().unwrap_or("unset")
                    ),
                ),
                Evidence::new("vcs", format!("{} is {}", pr_label(&r), new_state.to_uppercase())),
            ],
            Verdict::AutoSafe,
            format!("the label is out of date: the PR is {new_state}; refreshing it changes only what `ta list draft` shows"),
            Action::RefreshVcsLabel {
                draft_id: pkg.package_id,
                new_state: new_state.to_string(),
                review_ref: r,
            },
            &[("stored", v.review_state.clone().unwrap_or_default()), ("actual", new_state.to_string())],
        ));
    }
}

fn stuck_goals(s: &Scan, out: &mut Vec<Finding>) {
    let cutoff = Utc::now() - Duration::days(s.e.settings.stale_days as i64);
    for g in &s.goals {
        if !in_review(&g.state) || g.updated_at > cutoff {
            continue;
        }
        let sh = short(&g.goal_run_id);
        let pkg = s.draft_of(g);
        let mut ev = vec![Evidence::new(
            "goal",
            format!(
                "goal {} has been {} since {}",
                g.display_tag(),
                g.state,
                g.updated_at.format("%Y-%m-%d")
            ),
        )];
        let title = format!("goal {} \"{}\"", g.display_tag(), g.title);
        let id = format!("goal:{sh}");
        let set = |state: &str, reason: &str, ext: Option<String>| Action::SetGoalState {
            goal_id: g.goal_run_id,
            state: state.to_string(),
            reason: reason.to_string(),
            applied_externally_ref: ext,
        };
        let mut f = match pkg.map(|p| &p.status) {
            Some(DraftStatus::Applied { .. }) => {
                ev.push(Evidence::new("draft", "its draft was applied"));
                mk(
                    Kind::StuckGoal,
                    id,
                    title,
                    ev,
                    Verdict::AutoSafe,
                    "the draft was applied, so the goal is applied too",
                    set("applied", "", None),
                    &[("draft", "applied".into())],
                )
            }
            Some(DraftStatus::Closed {
                reason,
                applied_externally_ref,
                ..
            }) => {
                ev.push(Evidence::new("draft", "its draft was closed"));
                mk(
                    Kind::StuckGoal,
                    id,
                    title,
                    ev,
                    Verdict::AutoSafe,
                    "the draft was closed, so the goal follows it",
                    set(
                        "closed",
                        reason.as_deref().unwrap_or("draft closed"),
                        applied_externally_ref.clone(),
                    ),
                    &[("draft", "closed".into())],
                )
            }
            Some(st @ (DraftStatus::Denied { .. } | DraftStatus::Superseded { .. })) => {
                ev.push(Evidence::new("draft", format!("its draft is {st}")));
                mk(
                    Kind::StuckGoal,
                    id,
                    title,
                    ev,
                    Verdict::Propose,
                    format!("its draft is {st}; the goal has nothing left to wait for"),
                    set("closed", &format!("draft {st}"), None),
                    &[("draft", st.to_string())],
                )
            }
            Some(
                DraftStatus::Approved { .. } | DraftStatus::PendingReview | DraftStatus::Draft,
            ) => {
                // The draft finding decides; the goal follows on the next pass.
                continue;
            }
            None => {
                ev.push(Evidence::new(
                    "draft",
                    "no draft record is linked to this goal",
                ));
                let done = g
                    .plan_phase
                    .as_deref()
                    .and_then(|p| s.phase_landed_by(p, Some(g.goal_run_id)).map(|o| (p, o)));
                if let Some((p, o)) = done {
                    ev.push(Evidence::new(
                        "plan",
                        format!("phase {p} was landed by goal {}", o.display_tag()),
                    ));
                    mk(
                        Kind::StuckGoal,
                        id,
                        title,
                        ev,
                        Verdict::Propose,
                        "the phase is already done by another goal and this goal has no draft",
                        set(
                            "closed",
                            &format!("phase {p} already completed by another goal"),
                            Some(format!("phase-{p}-done")),
                        ),
                        &[("draft", "none".into()), ("phase", "done".into())],
                    )
                } else {
                    let mut f = mk(
                        Kind::StuckGoal,
                        id,
                        title,
                        ev,
                        Verdict::Ask,
                        "unknown: no draft and the phase is not done elsewhere",
                        Action::None,
                        &[("draft", "none".into()), ("phase", "open".into())],
                    );
                    f.options = vec![
                        AnswerOption {
                            key: "keep".into(),
                            label: "Keep the goal".into(),
                            action: Action::None,
                        },
                        AnswerOption {
                            key: "close".into(),
                            label: "Close the goal as abandoned".into(),
                            action: set(
                                "closed",
                                "abandoned: closed by a person via ta cleanup",
                                None,
                            ),
                        },
                    ];
                    f.recommended = Some("keep".into());
                    f
                }
            }
        };
        f.manual_command = Some(format!("ta goal show {sh}"));
        out.push(f);
    }
}

fn staging(s: &Scan, out: &mut Vec<Finding>) {
    let Ok(rd) = std::fs::read_dir(&s.e.paths.staging_dir) else {
        return;
    };
    let cutoff = Utc::now() - Duration::days(s.e.settings.stale_days as i64);
    let mut dirs: Vec<_> = rd.flatten().filter(|e| e.path().is_dir()).collect();
    dirs.sort_by_key(|e| e.file_name());
    for entry in dirs {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let goal = Uuid::parse_str(&name)
            .ok()
            .and_then(|id| s.goals.iter().find(|g| g.goal_run_id == id))
            .or_else(|| s.goals.iter().find(|g| g.workspace_path == path));
        let size = dir_size(&path);
        let sh: String = name.chars().take(8).collect();
        let id = format!("staging:{sh}");
        let title = format!("staging {sh} ({})", human_bytes(size));
        let mut ev = vec![Evidence::new(
            "staging",
            format!("{} uses {}", path.display(), human_bytes(size)),
        )];
        let rm = Action::RemoveDir { path: path.clone() };
        let target = path.join("target");

        let mut f = match goal {
            None => {
                let mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(chrono::DateTime::<Utc>::from);
                ev.push(Evidence::new(
                    "goal",
                    "no goal record owns this directory (orphan)",
                ));
                let old = mtime.is_none_or(|t| t <= cutoff);
                if old {
                    mk(
                        Kind::Staging,
                        id,
                        title,
                        ev,
                        Verdict::Propose,
                        "orphan: no goal record exists, so no draft can refer to this copy",
                        rm,
                        &[("goal", "missing".into())],
                    )
                } else {
                    mk(
                        Kind::Staging,
                        id,
                        title,
                        ev,
                        Verdict::Skip,
                        format!(
                            "orphan, but younger than {} day(s)",
                            s.e.settings.stale_days
                        ),
                        Action::None,
                        &[("goal", "missing".into()), ("age", "young".into())],
                    )
                }
            }
            Some(g) => {
                ev.push(Evidence::new(
                    "goal",
                    format!("goal {} is {}", g.display_tag(), g.state),
                ));
                let pkg = s.draft_of(g);
                if let Some(p) = pkg {
                    ev.push(Evidence::new("draft", format!("its draft is {}", p.status)));
                }
                let draft_class = match pkg.map(|p| &p.status) {
                    None => "none",
                    Some(st) if draft_final(st) => "final",
                    Some(_) => "live",
                };
                let facts = [
                    ("goal", state_class(&g.state).to_string()),
                    ("draft", draft_class.to_string()),
                ];
                let old = g.updated_at <= cutoff;
                if !is_terminal(&g.state) && !in_review(&g.state) {
                    mk(
                        Kind::Staging,
                        id,
                        title,
                        ev,
                        Verdict::Skip,
                        "kept: the goal is still active",
                        Action::None,
                        &facts,
                    )
                } else if draft_class == "live" {
                    mk(Kind::Staging, id, title, ev, Verdict::Skip,
                        "kept: its draft is unreviewed or approved but not applied, so this may be the only copy of the work",
                        Action::None, &facts)
                } else if in_review(&g.state) && draft_class != "final" {
                    mk(
                        Kind::Staging,
                        id,
                        title,
                        ev,
                        Verdict::Skip,
                        "kept: the goal is awaiting review",
                        Action::None,
                        &facts,
                    )
                } else if !old {
                    mk(
                        Kind::Staging,
                        id,
                        title,
                        ev,
                        Verdict::Skip,
                        format!(
                            "kept: finished less than {} day(s) ago",
                            s.e.settings.stale_days
                        ),
                        Action::None,
                        &facts,
                    )
                } else if matches!(g.state, GoalRunState::Failed { .. }) && draft_class == "none" {
                    mk(
                        Kind::Staging,
                        id,
                        title,
                        ev,
                        Verdict::Propose,
                        "the goal failed before building a draft; this holds its partial work",
                        rm,
                        &facts,
                    )
                } else {
                    mk(Kind::Staging, id, title, ev, Verdict::AutoSafe,
                        "the goal is finished and its draft is settled, so the staging copy is only a leftover",
                        rm, &facts)
                }
            }
        };
        f.bytes = size;
        f.manual_command = Some(format!("rm -r {}", path.display()));
        let removing = f.verdict != Verdict::Skip;
        out.push(f);

        // A build directory inside a copy that stays is rebuildable.
        let active = goal.is_some_and(|g| !is_terminal(&g.state) && !in_review(&g.state));
        if !removing && !active && target.is_dir() {
            let tsize = dir_size(&target);
            let mut t = mk(
                Kind::TargetDir,
                format!("target:{sh}"),
                format!("build output in staging {sh} ({})", human_bytes(tsize)),
                vec![Evidence::new(
                    "staging",
                    format!("{} uses {}", target.display(), human_bytes(tsize)),
                )],
                Verdict::AutoSafe,
                "build output can be rebuilt and no agent is running in this copy",
                Action::RemoveDir { path: target },
                &[("kind", "target".into())],
            );
            t.bytes = tsize;
            out.push(t);
        }
    }
}

fn locks(s: &Scan, out: &mut Vec<Finding>) {
    let root = &s.e.paths.root;
    let alive = |p: u32| s.e.truth.process_alive(p);

    match ta_lifecycle::apply_lock_state(root, &alive) {
        ta_lifecycle::ApplyLockState::Absent => {}
        ta_lifecycle::ApplyLockState::Live { pid, draft_id } => out.push(mk(
            Kind::ApplyLock,
            "lock:apply".into(),
            ".ta/apply.lock".into(),
            vec![Evidence::new(
                "lock",
                format!("held by running process {pid} applying draft {draft_id}"),
            )],
            Verdict::Skip,
            "kept: a `ta draft apply` is in progress",
            Action::None,
            &[("lock", "live".into())],
        )),
        ta_lifecycle::ApplyLockState::Stale { pid } => out.push(mk(
            Kind::ApplyLock,
            "lock:apply".into(),
            ".ta/apply.lock".into(),
            vec![Evidence::new(
                "lock",
                format!("process {pid} that took the lock is not running"),
            )],
            Verdict::AutoSafe,
            "the lock's owner is dead, so it only blocks the next apply",
            Action::RemoveFile {
                path: root.join(".ta/apply.lock"),
            },
            &[("lock", "stale".into())],
        )),
        ta_lifecycle::ApplyLockState::Unreadable(why) => {
            let mut f = mk(
                Kind::ApplyLock,
                "lock:apply".into(),
                ".ta/apply.lock".into(),
                vec![Evidence::new(
                    "lock",
                    format!("cannot read the lock: {why}"),
                )],
                Verdict::Ask,
                "unknown: the lock cannot be read, so its owner cannot be checked",
                Action::None,
                &[("lock", "unreadable".into())],
            );
            f.options = vec![
                AnswerOption {
                    key: "keep".into(),
                    label: "Keep it".into(),
                    action: Action::None,
                },
                AnswerOption {
                    key: "remove".into(),
                    label: "Remove it (no apply is running)".into(),
                    action: Action::RemoveFile {
                        path: root.join(".ta/apply.lock"),
                    },
                },
            ];
            f.recommended = Some("keep".into());
            out.push(f);
        }
    }

    if let Ok(rd) = std::fs::read_dir(s.e.paths.ta_dir()) {
        let mut pids: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "pid"))
            .collect();
        pids.sort();
        for p in pids {
            let Some(pid) = std::fs::read_to_string(&p)
                .ok()
                .and_then(|c| c.trim().parse::<u32>().ok())
            else {
                continue;
            };
            if alive(pid) {
                continue;
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            out.push(mk(
                Kind::PidFile,
                format!("pid:{name}"),
                format!(".ta/{name}"),
                vec![Evidence::new(
                    "pid",
                    format!("process {pid} is not running"),
                )],
                Verdict::AutoSafe,
                "the process that wrote this pid file is gone",
                Action::RemoveFile { path: p },
                &[("pid", "dead".into())],
            ));
        }
    }

    let dec = root.join(".ta-decisions.json");
    if dec.is_file() {
        let mut f = mk(
            Kind::LeftoverDecisions,
            "decisions:root".into(),
            ".ta-decisions.json in the project root".into(),
            vec![Evidence::new(
                "file",
                "it belongs inside a goal's staging copy; its presence here means an agent ran against the real project",
            )],
            Verdict::Propose,
            "a leaked agent artifact; delete it after a glance, it is not part of your project",
            Action::RemoveFile { path: dec.clone() },
            &[("file", "leftover".into())],
        );
        f.manual_command = Some(format!("rm {}", dec.display()));
        out.push(f);
    }

    for phase in s.e.truth.phase_claims() {
        out.push(mk(
            Kind::PhaseClaim,
            format!("claim:{phase}"),
            format!("daemon claim on phase {phase}"),
            vec![Evidence::new("daemon", format!("holds an in-memory claim on {phase} but no live goal owns it"))],
            Verdict::AutoSafe,
            "the claim blocks `ta run --phase` for a phase nobody is working on; PLAN.md stays the real gate",
            Action::ReleasePhaseClaim { phase },
            &[("claim", "no_live_goal".into())],
        ));
    }
}

fn archive(s: &Scan, out: &mut Vec<Finding>) {
    if !s.e.settings.archive {
        return;
    }
    let cutoff = Utc::now() - Duration::days(s.e.settings.retention_days as i64);
    let mut goals = Vec::new();
    let mut drafts = Vec::new();
    let mut claimed = std::collections::HashSet::new();
    for g in &s.goals {
        if !is_terminal(&g.state) || g.updated_at > cutoff {
            continue;
        }
        if g.workspace_path.exists()
            || s.e
                .paths
                .staging_dir
                .join(g.goal_run_id.to_string())
                .exists()
        {
            continue;
        }
        let pkg = s.draft_of(g);
        if pkg.is_some_and(|p| !draft_final(&p.status) || p.created_at > cutoff) {
            continue;
        }
        goals.push(g.goal_run_id);
        if let Some(p) = pkg {
            drafts.push(p.package_id);
            claimed.insert(p.package_id);
        }
    }
    for p in &s.drafts {
        if claimed.contains(&p.package_id) || !draft_final(&p.status) || p.created_at > cutoff {
            continue;
        }
        if s.goal_for(p).is_none() {
            drafts.push(p.package_id);
        }
    }
    if goals.is_empty() && drafts.is_empty() {
        return;
    }
    let mut f = mk(
        Kind::Archive,
        "archive:records".into(),
        format!(
            "{} goal and {} draft record(s) older than {} days",
            goals.len(),
            drafts.len(),
            s.e.settings.retention_days
        ),
        vec![Evidence::new(
            "records",
            format!(
                "finished, settled and without staging, older than {} days",
                s.e.settings.retention_days
            ),
        )],
        Verdict::AutoSafe,
        "moved into a compressed bundle under .ta/archive/ with an index; nothing is deleted",
        Action::ArchiveRecords { goals, drafts },
        &[("archive", "old_records".into())],
    );
    f.manual_command = Some("ta cleanup --only archive".into());
    out.push(f);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_ignores_order_and_is_stable() {
        let a = signature(Kind::StuckDraft, &[("a", "1".into()), ("b", "2".into())]);
        let b = signature(Kind::StuckDraft, &[("b", "2".into()), ("a", "1".into())]);
        assert_eq!(a, b);
        assert_ne!(
            a,
            signature(Kind::Staging, &[("a", "1".into()), ("b", "2".into())])
        );
    }

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(10), "10 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
    }
}
