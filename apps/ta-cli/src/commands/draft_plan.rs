// draft_plan.rs -- PLAN.md handling for `ta draft apply --phase` (v0.17.11.29).
//
// Apply must change exactly what the draft says. For PLAN.md that means: carry
// the draft's own checkmarks for the target phase, set the target phase's status
// marker, and leave every other byte of the file alone. The pure functions here
// are shared by the real apply paths (VCS and non-VCS) and by `--dry-run`, so a
// preview reports exactly what the real run would write.

use std::path::Path;

use ta_changeset::plan_scoped::{
    locate_phase_block, merge_phase_from_staging, phase_status_word, phase_unchecked_own_items,
    LocateError, OwnItem, ScopedMerge,
};

use super::plan::{
    parse_plan, phase_id_to_semver, record_history, update_phase_status, PlanStatus,
};

/// What the scoped merge did for one phase.
#[derive(Debug, Clone)]
pub struct PhaseMergeReport {
    pub phase: String,
    pub merge: ScopedMerge,
}

/// Carry the draft's checkmarks for each target phase onto `source`.
///
/// `source` is the PLAN.md currently on disk. Nothing outside the target phases'
/// own items changes. Returns the new content and one report per phase.
pub fn merge_draft_plan(
    source: &str,
    staging: &str,
    phase_ids: &[String],
) -> (String, Vec<PhaseMergeReport>) {
    let mut content = source.to_string();
    let mut reports = Vec::new();
    for phase in phase_ids {
        let merge = merge_phase_from_staging(&content, staging, phase);
        content = merge.content.clone();
        reports.push(PhaseMergeReport {
            phase: phase.clone(),
            merge,
        });
    }
    (content, reports)
}

/// Human-readable lines describing a scoped merge (printed by apply and dry-run).
pub fn describe_merge(report: &PhaseMergeReport) -> Vec<String> {
    let m = &report.merge;
    let p = &report.phase;
    let mut out = Vec::new();
    if let Some(e) = &m.source_problem {
        out.push(format!(
            "[plan-merge] Phase {p}: {e} in the PLAN.md on disk, so the draft's PLAN.md edits \
             were NOT applied. Check the phase id passed to --phase (`ta plan list`), then \
             re-run, or edit PLAN.md by hand."
        ));
        return out;
    }
    if let Some(e) = &m.draft_problem {
        out.push(format!(
            "[plan-merge] Phase {p}: {e} in the draft's PLAN.md, so no checkmarks were carried \
             over. PLAN.md is otherwise unchanged."
        ));
    }
    if !m.newly_checked.is_empty() {
        out.push(format!(
            "[plan-merge] Phase {p}: carried {} checkmark(s) from the draft: {}",
            m.newly_checked.len(),
            labels(&m.newly_checked)
        ));
    }
    if !m.left_unchecked.is_empty() {
        out.push(format!(
            "[plan-merge] Phase {p}: {} item(s) were not completed by the draft and stay \
             unchecked: {}. Finish them, or move them to a named future phase (Deferred Items \
             Policy), then mark the phase done.",
            m.left_unchecked.len(),
            labels(&m.left_unchecked)
        ));
    }
    if !m.unmatched_in_draft.is_empty() {
        out.push(format!(
            "[plan-merge] Phase {p}: {} checked item(s) in the draft match no item in PLAN.md \
             (reworded or removed) and were NOT applied: {}",
            m.unmatched_in_draft.len(),
            labels(&m.unmatched_in_draft)
        ));
    }
    out
}

fn labels(items: &[OwnItem]) -> String {
    items
        .iter()
        .map(OwnItem::label)
        .collect::<Vec<_>>()
        .join("; ")
}

/// How one phase ended up after the status step.
#[derive(Debug, Clone)]
pub enum PhaseResult {
    MarkedDone,
    /// Left as it was: these own items are still unchecked.
    HeldUnchecked(Vec<OwnItem>),
    /// Already `done`; nothing to write.
    AlreadyDone,
    NotFound,
    Ambiguous(usize),
}

#[derive(Debug, Clone)]
pub struct PhaseOutcome {
    pub phase: String,
    pub old_status: PlanStatus,
    pub result: PhaseResult,
}

/// Mark each target phase `done` when all of its own items are checked.
///
/// Never checks an item. A phase with unchecked own items is held and the items are
/// named, so an incomplete phase cannot be recorded as complete by apply.
pub fn finalize_phases(content: &str, phase_ids: &[String]) -> (String, Vec<PhaseOutcome>) {
    let mut content = content.to_string();
    let mut outcomes = Vec::new();
    for phase in phase_ids {
        // Read the phase's own marker rather than parsing the whole plan: the full
        // parser is quadratic on a large PLAN.md and this runs once per phase.
        let old_status = match phase_status_word(&content, phase).as_deref() {
            Some("done") => PlanStatus::Done,
            Some("in_progress") => PlanStatus::InProgress,
            Some("deferred") => PlanStatus::Deferred,
            _ => PlanStatus::Pending,
        };

        let result = match locate_phase_block(&content, phase) {
            Err(LocateError::Ambiguous(n)) => PhaseResult::Ambiguous(n),
            Err(LocateError::NotFound) => {
                // Custom plan schemas may name phases differently; fall back to the
                // schema-driven updater, which is a no-op when nothing matches.
                let updated = update_phase_status(&content, phase, PlanStatus::Done);
                if updated == content {
                    PhaseResult::NotFound
                } else {
                    content = updated;
                    PhaseResult::MarkedDone
                }
            }
            Ok(_) => {
                let unchecked = phase_unchecked_own_items(&content, phase);
                if !unchecked.is_empty() {
                    PhaseResult::HeldUnchecked(unchecked)
                } else if old_status == PlanStatus::Done {
                    PhaseResult::AlreadyDone
                } else {
                    content = update_phase_status(&content, phase, PlanStatus::Done);
                    PhaseResult::MarkedDone
                }
            }
        };
        outcomes.push(PhaseOutcome {
            phase: phase.clone(),
            old_status,
            result,
        });
    }
    (content, outcomes)
}

/// Human-readable line for an outcome (printed by apply and dry-run).
pub fn describe_outcome(o: &PhaseOutcome, dry_run: bool) -> String {
    let p = &o.phase;
    match &o.result {
        PhaseResult::MarkedDone if dry_run => format!("Would update PLAN.md: Phase {p} -> done"),
        PhaseResult::MarkedDone => format!("Updated PLAN.md: Phase {p} -> done"),
        PhaseResult::AlreadyDone => format!("[plan-update] Phase {p} is already done in PLAN.md."),
        PhaseResult::HeldUnchecked(items) => format!(
            "[plan-update] WARNING: phase {p} has {} unchecked item(s), so its status was NOT \
             set to done: {}. Apply never checks items for you. Finish them (or move them to a \
             named future phase) and then run `ta plan mark-done {p}`.",
            items.len(),
            labels(items)
        ),
        PhaseResult::NotFound => format!(
            "[plan-update] WARNING: phase {p} was not found in PLAN.md, so no status was \
             changed. Check the id with `ta plan list`."
        ),
        PhaseResult::Ambiguous(n) => format!(
            "[plan-update] WARNING: {n} headings in PLAN.md match phase {p}, so no status was \
             changed rather than guess. Remove the duplicate heading and re-run."
        ),
    }
}

/// The version apply should set once `content` reflects the completed phase(s).
///
/// This is the same computation as `ta plan expected-version` (the last phase of
/// the leading contiguous run of done phases), so apply lands on the value the
/// version check expects even when later phases are already done. Falls back to
/// the phase id's own semver only when the plan has no done semver phase.
pub fn version_after_apply(content: &str, last_phase_id: &str) -> Option<String> {
    let expected = ta_plan::expected_version_from_plan(&parse_plan(content));
    expected
        .version
        .or_else(|| phase_id_to_semver(last_phase_id))
}

/// Result of [`complete_phases_on_disk`].
pub struct PlanCompletion {
    /// PLAN.md content after the status update (what is now on disk).
    pub content: String,
    /// The last target phase (used for the version fallback and "next phase" hint).
    pub last_phase_id: String,
}

/// Mark the target phases done in `<target_dir>/PLAN.md`, record history, and write the
/// file only if something changed. Shared by the VCS and non-VCS apply paths.
pub fn complete_phases_on_disk(
    target_dir: &Path,
    phase_ids: &[String],
) -> anyhow::Result<PlanCompletion> {
    let plan_path = target_dir.join("PLAN.md");
    let original = std::fs::read_to_string(&plan_path).map_err(|e| {
        anyhow::anyhow!(
            "Could not read {} to update phase status: {}. Check the file exists and is \
             readable, then re-run `ta draft apply`.",
            plan_path.display(),
            e
        )
    })?;
    let (content, outcomes) = finalize_phases(&original, phase_ids);
    for o in &outcomes {
        let line = describe_outcome(o, false);
        if matches!(o.result, PhaseResult::MarkedDone | PhaseResult::AlreadyDone) {
            println!("{line}");
        } else {
            eprintln!("{line}");
        }
        if matches!(o.result, PhaseResult::MarkedDone) {
            if let Err(e) = record_history(target_dir, &o.phase, &o.old_status, &PlanStatus::Done) {
                tracing::warn!(phase = %o.phase, error = %e, "could not record plan history");
            }
        }
    }
    if content != original {
        std::fs::write(&plan_path, content.as_bytes()).map_err(|e| {
            anyhow::anyhow!(
                "Could not write {}: {}. The phase status was not updated; fix the \
                 permissions and re-run `ta draft apply`.",
                plan_path.display(),
                e
            )
        })?;
    }
    Ok(PlanCompletion {
        content,
        last_phase_id: phase_ids.last().cloned().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::plan::phase_ids_match;
    use super::*;

    const REAL_PLAN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../PLAN.md");

    /// A tempdir holding a copy of the repo's real PLAN.md, and its text.
    fn real_plan_copy() -> (tempfile::TempDir, String) {
        let original = std::fs::read_to_string(REAL_PLAN).expect("read the real PLAN.md");
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("PLAN.md"), &original).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("PLAN.md")).unwrap();
        (tmp, text)
    }

    fn lines(s: &str) -> Vec<&str> {
        s.split_inclusive('\n').collect()
    }

    /// Naive oracle for a phase block, deliberately independent of the production
    /// locator: the heading line whose first word is the id, through the line before
    /// the next line starting with the same number of `#` or fewer.
    fn oracle_block(doc: &str, phase: &str) -> Option<(usize, usize)> {
        let ls = lines(doc);
        let start = ls.iter().position(|l| {
            l.starts_with("### ")
                && l[4..]
                    .split_whitespace()
                    .next()
                    .map(|w| w.trim_start_matches('v'))
                    == Some(phase.trim_start_matches('v'))
        })?;
        let end = ls
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, l)| l.starts_with("### ") || l.starts_with("## ") || l.starts_with("# "))
            .map(|(i, _)| i)
            .unwrap_or(ls.len());
        Some((start, end))
    }

    /// Every line outside `[start, end)` is identical and the line count is unchanged.
    fn assert_identical_outside(before: &str, after: &str, block: (usize, usize), what: &str) {
        let (b, a) = (lines(before), lines(after));
        assert_eq!(b.len(), a.len(), "{what}: line count changed");
        for i in 0..b.len() {
            if i >= block.0 && i < block.1 {
                continue;
            }
            assert_eq!(
                b[i],
                a[i],
                "{what}: line {} outside the target block changed",
                i + 1
            );
        }
    }

    /// A draft that is as hostile as possible: every checkbox checked (human gates
    /// included), every status marker flipped to done, and every blank line removed.
    fn hostile_draft(source: &str) -> String {
        source
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                if l.trim_start().starts_with("<!-- status:") {
                    "<!-- status: done -->".to_string()
                } else {
                    l.replace("[ ]", "[x]")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Only the target block may differ, and inside it only status marker lines and
    /// `[ ]` -> `[x]` flips on item lines.
    fn assert_block_changes_are_checkbox_or_marker(
        before: &str,
        after: &str,
        block: (usize, usize),
    ) {
        let (b, a) = (lines(before), lines(after));
        for i in block.0..block.1.min(b.len()) {
            if b[i] == a[i] {
                continue;
            }
            let is_marker = b[i].trim_start().starts_with("<!-- status:")
                && a[i].trim_start().starts_with("<!-- status:");
            let is_flip = b[i].replacen("[ ]", "[x]", 1) == a[i];
            assert!(
                is_marker || is_flip,
                "line {} changed in an unexpected way:\n  before: {:?}\n  after:  {:?}",
                i + 1,
                b[i],
                a[i]
            );
        }
    }

    /// First phase in the real plan with the given status and at least two numbered
    /// items; with `all_checked`, only phases whose numbered items are all checked.
    fn pick(
        src: &str,
        phases: &[super::super::plan::PlanPhase],
        status: PlanStatus,
        all_checked: bool,
    ) -> Option<String> {
        let ls = lines(src);
        phases
            .iter()
            .filter(|p| p.status == status && p.id.starts_with('v'))
            .find(|p| {
                let Some((s, e)) = oracle_block(src, &p.id) else {
                    return false;
                };
                let items: Vec<&&str> = ls[s..e]
                    .iter()
                    .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()) && l.contains(". ["))
                    .collect();
                items.len() >= 2 && (!all_checked || items.iter().all(|l| !l.contains(". [ ]")))
            })
            .map(|p| p.id.clone())
    }

    /// The full apply-time PLAN.md transformation for one phase.
    fn run_plan_apply(source: &str, staging: &str, phase: &str) -> (String, String) {
        let ids = vec![phase.to_string()];
        let (merged, _) = merge_draft_plan(source, staging, &ids);
        let (finalized, _) = finalize_phases(&merged, &ids);
        (merged, finalized)
    }

    #[test]
    fn golden_done_phase_changes_nothing_but_nothing_to_check() {
        let (_tmp, src) = real_plan_copy();
        let phases = parse_plan(&src);
        let id = pick(&src, &phases, PlanStatus::Done, true).expect("a done phase with items");
        let (merged, finalized) = run_plan_apply(&src, &hostile_draft(&src), &id);
        let block = oracle_block(&src, &id).unwrap();
        assert_identical_outside(&src, &merged, block, "done/merged");
        assert_identical_outside(&src, &finalized, block, "done/finalized");
        assert_block_changes_are_checkbox_or_marker(&src, &finalized, block);
        // A done phase's items were already checked, so apply must be a byte-for-byte no-op.
        assert_eq!(
            finalized, src,
            "applying a draft to a done phase must change nothing"
        );
    }

    #[test]
    fn golden_in_progress_phase_only_target_block_changes() {
        let (_tmp, original) = real_plan_copy();
        let phases = parse_plan(&original);
        let id = pick(&original, &phases, PlanStatus::Pending, false)
            .expect("a pending phase with items");
        // Turn it into an in_progress phase first, as `ta run` does.
        let src = update_phase_status(&original, &id, PlanStatus::InProgress);
        assert_ne!(src, original);
        let (merged, finalized) = run_plan_apply(&src, &hostile_draft(&src), &id);
        let block = oracle_block(&src, &id).unwrap();
        assert_identical_outside(&src, &merged, block, "in_progress/merged");
        assert_identical_outside(&src, &finalized, block, "in_progress/finalized");
        assert_block_changes_are_checkbox_or_marker(&src, &finalized, block);
        // The draft checked everything in the block, so the phase is done now.
        let after = parse_plan(&finalized);
        let status = after
            .iter()
            .find(|p| phase_ids_match(&p.id, &id))
            .unwrap()
            .status
            .clone();
        assert_eq!(status, PlanStatus::Done);
    }

    #[test]
    fn golden_look_alike_headings_are_not_confused() {
        let (_tmp, src) = real_plan_copy();
        let phases = parse_plan(&src);
        // Find a phase whose id is a strict prefix of another phase's id (e.g. v0.17.12 / v0.17.12.1).
        let id = phases
            .iter()
            .filter(|p| p.status == PlanStatus::Pending)
            .find(|p| {
                phases
                    .iter()
                    .any(|q| q.id != p.id && q.id.starts_with(&format!("{}.", p.id)))
                    && oracle_block(&src, &p.id).is_some()
            })
            .map(|p| p.id.clone())
            .expect("a pending phase with a longer look-alike id");
        let (merged, finalized) = run_plan_apply(&src, &hostile_draft(&src), &id);
        let block = oracle_block(&src, &id).unwrap();
        assert_identical_outside(&src, &merged, block, "lookalike/merged");
        assert_identical_outside(&src, &finalized, block, "lookalike/finalized");
        assert_block_changes_are_checkbox_or_marker(&src, &finalized, block);
    }

    #[test]
    fn golden_partly_checked_phase_checks_only_what_the_draft_checked_and_holds_status() {
        let (_tmp, original) = real_plan_copy();
        let phases = parse_plan(&original);
        let id = pick(&original, &phases, PlanStatus::Pending, false)
            .expect("a pending phase with 2+ items");
        let src = update_phase_status(&original, &id, PlanStatus::InProgress);
        let block = oracle_block(&src, &id).unwrap();

        // Draft: copy of the source with exactly the FIRST numbered item of the phase checked.
        let mut staging_lines: Vec<String> = lines(&src).iter().map(|s| s.to_string()).collect();
        let first_item = (block.0..block.1)
            .find(|&i| {
                staging_lines[i].starts_with(|c: char| c.is_ascii_digit())
                    && staging_lines[i].contains(". [ ]")
            })
            .expect("an unchecked numbered item");
        staging_lines[first_item] = staging_lines[first_item].replacen("[ ]", "[x]", 1);
        let staging = staging_lines.concat();

        let ids = vec![id.clone()];
        let (merged, reports) = merge_draft_plan(&src, &staging, &ids);
        assert_eq!(
            reports[0].merge.newly_checked.len(),
            1,
            "exactly one item carried"
        );
        assert!(
            !reports[0].merge.left_unchecked.is_empty(),
            "the remaining items must be reported as left unchecked"
        );
        assert_identical_outside(&src, &merged, block, "partial/merged");
        assert_eq!(
            lines(&merged)[first_item],
            lines(&src)[first_item].replacen("[ ]", "[x]", 1)
        );

        let (finalized, outcomes) = finalize_phases(&merged, &ids);
        assert!(matches!(outcomes[0].result, PhaseResult::HeldUnchecked(_)));
        assert_eq!(
            finalized, merged,
            "a held phase must not change status or items"
        );
        let msg = describe_outcome(&outcomes[0], false);
        assert!(msg.contains("unchecked item(s)"), "{msg}");
        assert!(
            msg.contains("Finish them"),
            "message must say what to do: {msg}"
        );
    }

    /// Property-style: for EVERY phase in the real plan and several deterministic
    /// pseudo-random drafts, every line outside the target block is unchanged and no
    /// checkbox outside it changes state.
    #[test]
    fn property_every_line_outside_the_target_block_is_unchanged() {
        let (_tmp, src) = real_plan_copy();
        let phases = parse_plan(&src);
        assert!(phases.len() > 50);

        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        let mut checked_phases = 0;
        // Every 4th phase keeps the test fast on a PLAN.md with hundreds of phases while
        // still covering done, in_progress, pending, sub-phase and look-alike ids.
        for p in phases.iter().step_by(4) {
            let Some(block) = oracle_block(&src, &p.id) else {
                continue;
            };
            // Skip ids that appear more than once (the production code refuses those).
            let dup = lines(&src)
                .iter()
                .filter(|l| {
                    l.starts_with("### ")
                        && l[4..]
                            .split_whitespace()
                            .next()
                            .map(|w| w.trim_start_matches('v'))
                            == Some(p.id.trim_start_matches('v'))
                })
                .count();
            if dup != 1 {
                continue;
            }
            // Random draft: flip ~1 in 3 checkboxes, drop ~1 in 7 blank lines, flip some markers.
            let staging: String = lines(&src)
                .iter()
                .filter_map(|l| {
                    let r = next();
                    if l.trim().is_empty() && r % 7 == 0 {
                        return None;
                    }
                    if l.trim_start().starts_with("<!-- status:") && r % 3 == 0 {
                        return Some("<!-- status: done -->\n".to_string());
                    }
                    if r % 3 == 0 {
                        return Some(l.replace("[ ]", "[x]"));
                    }
                    Some(l.to_string())
                })
                .collect();
            let (merged, finalized) = run_plan_apply(&src, &staging, &p.id);
            assert_identical_outside(&src, &merged, block, &format!("{} merged", p.id));
            assert_identical_outside(&src, &finalized, block, &format!("{} finalized", p.id));
            assert_block_changes_are_checkbox_or_marker(&src, &finalized, block);
            checked_phases += 1;
        }
        assert!(
            checked_phases > 30,
            "property test covered only {checked_phases} phases"
        );
    }

    #[test]
    fn identical_three_way_merge_of_the_real_plan_is_byte_identical() {
        // Regression for the blank-line loss in the (non-phase) three-way merge path.
        let (_tmp, src) = real_plan_copy();
        let merged = ta_changeset::plan_merge::merge_plan_md(&src, &src, &src).merged;
        let (s, m) = (lines(&src), lines(&merged));
        let first_diff = (0..s.len().min(m.len())).find(|&i| s[i] != m[i]);
        assert_eq!(
            first_diff,
            None,
            "three-way merge of identical inputs changed line {:?}",
            first_diff.map(|i| i + 1)
        );
        assert_eq!(s.len(), m.len());
    }

    #[test]
    fn version_after_apply_uses_the_contiguous_done_version_not_the_phase_id() {
        // v1.0.1 (the phase being applied) is done, but v1.0.3 is already done too and
        // v1.0.2 is pending, so the version is held at v1.0.1, never raised past the gap.
        let plan = "\
### v1.0.0 - A\n<!-- status: done -->\n\
### v1.0.1 - B\n<!-- status: done -->\n\
### v1.0.2 - C\n<!-- status: pending -->\n\
### v1.0.3 - D\n<!-- status: done -->\n";
        assert_eq!(
            version_after_apply(plan, "v1.0.3").as_deref(),
            Some("1.0.1-alpha")
        );
        // Phase ids that ta cannot place fall back to their own semver.
        assert_eq!(
            version_after_apply("no phases", "v2.0.0").as_deref(),
            Some("2.0.0-alpha")
        );
        // Contiguous run reaches the applied phase: version is that phase's.
        let all_done = plan.replace("pending", "done");
        assert_eq!(
            version_after_apply(&all_done, "v1.0.2").as_deref(),
            Some("1.0.3-alpha")
        );
    }

    #[test]
    fn complete_phases_on_disk_writes_only_when_changed_and_records_nothing_for_held() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = "### v1.0.0 - A\n<!-- status: in_progress -->\n\n1. [ ] left\n";
        std::fs::write(tmp.path().join("PLAN.md"), plan).unwrap();
        let (_, outcomes) = finalize_phases(plan, &["v1.0.0".to_string()]);
        assert!(matches!(outcomes[0].result, PhaseResult::HeldUnchecked(_)));
        let done = complete_phases_on_disk(tmp.path(), &["v1.0.0".to_string()]).unwrap();
        assert_eq!(done.content, plan);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("PLAN.md")).unwrap(),
            plan
        );
        assert!(!tmp.path().join(".ta/plan_history.jsonl").exists());
    }
}
