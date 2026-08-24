// query.rs — Phase queries, phase-ID/semver utilities, and dependency-wave
// planning over an already-parsed phase list (extracted from
// apps/ta-cli/src/commands/plan.rs, v0.17.11.1).

use crate::parse::phase_ids_match;
use crate::schema::{PlanPhase, PlanStatus};

/// Find the next actionable (pending) phase, optionally searching forward
/// from a given phase ID rather than from the start of the plan.
pub fn find_next_pending<'a>(
    phases: &'a [PlanPhase],
    after_phase: Option<&str>,
) -> Option<&'a PlanPhase> {
    if let Some(after) = after_phase {
        // Find the current phase's position and search forward from there.
        if let Some(idx) = phases.iter().position(|p| phase_ids_match(&p.id, after)) {
            // Search forward from the phase after the current one.
            if let Some(next) = phases[idx + 1..].iter().find(|p| p.status.is_actionable()) {
                return Some(next);
            }
        }
        // Phase not found or no actionable phases after it — don't fall back to
        // the beginning (which would suggest unrelated earlier phases like v0.1).
        None
    } else {
        phases.iter().find(|p| p.status.is_actionable())
    }
}

/// Find the first `InProgress` phase.
///
/// Used for status introspection, resume flows, and claim checks. Not for
/// dispatch decisions — use `find_next_pending` for those.
pub fn find_in_progress(phases: &[PlanPhase]) -> Option<&PlanPhase> {
    phases
        .iter()
        .find(|p| matches!(p.status, PlanStatus::InProgress))
}

/// Collect human-readable warnings for phases whose declared dependencies
/// are not yet `Done`.
pub fn collect_dependency_warnings(phases: &[PlanPhase]) -> Vec<String> {
    let mut warnings = Vec::new();
    for phase in phases {
        if phase.depends_on.is_empty() {
            continue;
        }
        for dep_id in &phase.depends_on {
            let dep_done = phases
                .iter()
                .any(|p| phase_ids_match(&p.id, dep_id) && p.status == PlanStatus::Done);
            if !dep_done {
                warnings.push(format!(
                    "Phase {} depends on {} which is not yet done.",
                    phase.id, dep_id,
                ));
            }
        }
    }
    warnings
}

/// Returns the binary version string at compile time.
///
/// All workspace crates share the same version via `version.workspace =
/// true`, so `ta-plan`'s own `CARGO_PKG_VERSION` is identical to `ta-cli`'s
/// — safe to read from here rather than needing it passed in.
pub fn binary_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Returns true if the phase ID is a sub-phase (has 4 or more numeric components).
///
/// A sub-phase has the form `vX.Y.Z.N` (or deeper like `vX.Y.Z.N.M`), as opposed
/// to a top-level phase `vX.Y.Z`. Non-semver IDs like "4b" or "Phase 0" are never
/// considered sub-phases.
pub fn is_sub_phase(id: &str) -> bool {
    let stripped = match id.strip_prefix('v') {
        Some(s) if s.starts_with(|c: char| c.is_ascii_digit()) => s,
        _ => return false,
    };
    stripped.split('.').count() >= 4
}

/// Returns the immediate parent phase ID for a sub-phase, or `None` if the ID
/// is not a sub-phase.
///
/// Drops only the last dot-separated component — `v0.16.0.1` → `Some("v0.16.0")`,
/// `v0.15.30.5.1` → `Some("v0.15.30.5")`.
pub fn parent_phase_id(id: &str) -> Option<String> {
    if !is_sub_phase(id) {
        return None;
    }
    let stripped = id.strip_prefix('v').unwrap_or(id);
    let parts: Vec<&str> = stripped.split('.').collect();
    Some(format!("v{}", parts[..parts.len() - 1].join(".")))
}

/// Parse a semver-style phase ID like "v0.14.3" or "v0.13.17.1" into a comparable tuple of u32s.
///
/// Only phases whose ID starts with `v` followed by digits are considered.
/// Returns `None` for non-semver IDs (e.g., "4b", "Phase 1").
pub fn parse_semver_id(id: &str) -> Option<Vec<u32>> {
    let stripped = id.strip_prefix('v')?;
    // Must start with a digit after the 'v'
    if !stripped.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let parts: Option<Vec<u32>> = stripped.split('.').map(|s| s.parse::<u32>().ok()).collect();
    parts
}

/// Maximum number of sub-phase segments (beyond `major.minor.patch`) that are
/// encoded losslessly as dot-separated semver pre-release identifiers.
const MAX_SUBPHASE_DEPTH: usize = 4;

/// Derive a stable 4-hex-char discriminator from a phase ID string.
///
/// Deterministic per phase-id (not per-commit) so the same overflowing phase ID
/// always produces the same build-metadata suffix, regardless of which commit
/// happens to perform the bump.
fn phase_id_discriminator(phase_id: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(phase_id.as_bytes());
    format!("{:x}", digest)[..4].to_string()
}

/// Convert a plan phase ID to the canonical workspace semver string.
///
/// Phase ID mapping (per CLAUDE.md version policy):
///   v0.14.22           → "0.14.22-alpha"
///   v0.14.22.1         → "0.14.22-alpha.1"
///   v0.14.22.2         → "0.14.22-alpha.2"
///   v0.15.0            → "0.15.0-alpha"
///   v0.17.0.12.9       → "0.17.0-alpha.12.9"
///   v0.17.0.12.9.1     → "0.17.0-alpha.12.9.1"
///
/// Sub-phase IDs are handled generically: any number of segments beyond
/// `major.minor.patch` are appended in order as dot-separated pre-release
/// identifiers, up to `MAX_SUBPHASE_DEPTH` of them.
///
/// Beyond `MAX_SUBPHASE_DEPTH` sub-segments, the pre-release chain is truncated
/// to the first `MAX_SUBPHASE_DEPTH` and a build-metadata suffix (`+xxxx`) is
/// appended containing a 4-hex-char discriminator derived from a stable hash of
/// the *full* phase ID.
///
/// Non-semver phase IDs (e.g., "4b", "Phase 1") return `None` — no auto-bump.
pub fn phase_id_to_semver(phase_id: &str) -> Option<String> {
    let parts = parse_semver_id(phase_id)?;
    if parts.len() < 3 {
        return None;
    }
    let (major, minor, patch) = (parts[0], parts[1], parts[2]);
    let sub_segments = &parts[3..];

    if sub_segments.is_empty() {
        return Some(format!("{}.{}.{}-alpha", major, minor, patch));
    }

    if sub_segments.len() <= MAX_SUBPHASE_DEPTH {
        let pre_release = sub_segments
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(".");
        return Some(format!(
            "{}.{}.{}-alpha.{}",
            major, minor, patch, pre_release
        ));
    }

    // Beyond MAX_SUBPHASE_DEPTH: truncate the pre-release chain and disambiguate
    // with a build-metadata discriminator derived from the full phase ID.
    let truncated = sub_segments[..MAX_SUBPHASE_DEPTH]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(".");
    let discriminator = phase_id_discriminator(phase_id);
    Some(format!(
        "{}.{}.{}-alpha.{}+{}",
        major, minor, patch, truncated, discriminator
    ))
}

/// Loudly warn (tracing + stderr) that a phase ID could not be converted to a
/// version via `phase_id_to_semver` and that the version bump was skipped.
pub fn warn_unparseable_phase_id_for_bump(phase_id: &str) {
    tracing::warn!(
        phase = %phase_id,
        "phase_id_to_semver could not parse phase ID — version bump skipped"
    );
    eprintln!(
        "[version] Warning: phase ID '{}' could not be converted to a version \
         (expected `vMAJOR.MINOR.PATCH[.SUB...]`, e.g. `v0.17.0` or `v0.17.0.12.9`) — \
         version bump skipped. If this phase should bump the version, rename it to \
         follow that format, or run ./scripts/bump-version.sh manually.",
        phase_id
    );
}

/// Check for out-of-order phases: a `Done` phase appears after a `Pending` phase
/// in document order (for phases with semver-style IDs only).
///
/// Returns deduplicated human-readable warning strings: one line per pending phase
/// showing the count of later-done phases.
pub fn check_phase_order(phases: &[PlanPhase]) -> Vec<String> {
    // Collect (index, id, status) for semver phases only.
    let semver_phases: Vec<(usize, &PlanPhase)> = phases
        .iter()
        .enumerate()
        .filter(|(_, p)| parse_semver_id(&p.id).is_some())
        .collect();

    // pending_ids_in_order: insertion-ordered list of pending phase IDs
    // pending_later_done: parallel counts of Done phases appearing after each pending phase
    let mut pending_ids_in_order: Vec<String> = Vec::new();
    let mut pending_later_done: Vec<usize> = Vec::new();

    for (_, phase) in &semver_phases {
        if phase.status == PlanStatus::Pending {
            pending_ids_in_order.push(phase.id.clone());
            pending_later_done.push(0);
        } else if phase.status == PlanStatus::Done {
            // Count this Done phase against all currently-seen Pending phases.
            for count in pending_later_done.iter_mut() {
                *count += 1;
            }
        }
    }

    // Emit one line per pending phase that has later-done violations.
    pending_ids_in_order
        .iter()
        .zip(pending_later_done.iter())
        .filter_map(|(pid, &count)| {
            if count == 0 {
                return None;
            }
            Some(format!(
                "[warn] {} is still pending — {} later phase(s) are complete (out of order)",
                pid, count
            ))
        })
        .collect()
}

/// Detect phases that have no `<!-- status: ... -->` marker in PLAN.md content.
///
/// Returns a list of phase IDs that are missing a status marker.
/// These phases parse as `Pending` due to the status-lookahead fallback,
/// which may produce false "pending" counts in `ta plan status`.
pub fn detect_missing_status_markers(content: &str) -> Vec<String> {
    use regex::Regex;

    let status_re = match Regex::new(r"<!--\s*status:\s*\w+\s*-->") {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    // Phase header patterns (same as default schema).
    let header_patterns: &[&str] = &[
        r"^###\s+(v[\d]+\.[\d]+\.[\d]+(?:\.[\d]+)?)\s+[—\-]",
        r"^##\s+Phase\s+([\w.]+)\s+[—\-]",
        r"^###\s+(v[\d]+\.[\d]+)\s+[—\-]",
    ];
    let compiled: Vec<_> = header_patterns
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

    let lines: Vec<&str> = content.lines().collect();
    let mut missing = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let mut matched_id: Option<String> = None;
        for pat in &compiled {
            if let Some(caps) = pat.captures(trimmed) {
                matched_id = caps.get(1).map(|m| m.as_str().to_string());
                break;
            }
        }
        if let Some(id) = matched_id {
            // Check if next non-empty line has a status marker.
            let next = lines.get(i + 1).map(|l| l.trim()).unwrap_or("");
            if !status_re.is_match(next) {
                missing.push(id);
            }
        }
    }

    missing
}

/// Scan PLAN.md for phases where all items are `[x]` but status marker is not `done`.
///
/// Returns `(phase_id, line_number_of_header)` pairs.
pub fn find_phases_needing_done_marker(content: &str) -> Vec<(String, usize)> {
    use regex::Regex;

    use crate::schema::PlanSchema;

    let schema = PlanSchema::default_schema();
    let phases = crate::parse::parse_plan_with_schema(content, &schema);
    let missing_markers = detect_missing_status_markers(content);
    let missing_set: std::collections::HashSet<&str> =
        missing_markers.iter().map(|s| s.as_str()).collect();

    let mut result = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    // Phase header detection (same patterns).
    let header_patterns: &[&str] = &[
        r"^###\s+(v[\d]+\.[\d]+\.[\d]+(?:\.[\d]+)?)\s+[—\-]",
        r"^##\s+Phase\s+([\w.]+)\s+[—\-]",
        r"^###\s+(v[\d]+\.[\d]+)\s+[—\-]",
    ];

    for phase in &phases {
        // Only flag if all plan items are checked.
        if phase.status == PlanStatus::Done {
            continue; // Already marked done.
        }
        if !missing_set.contains(phase.id.as_str()) && phase.status != PlanStatus::Pending {
            continue; // Has a non-done status marker — user intent.
        }
        // Find the header line for this phase.
        let header_line_idx = lines.iter().position(|l| {
            let trimmed = l.trim();
            header_patterns.iter().any(|p| {
                Regex::new(p)
                    .ok()
                    .and_then(|r| r.captures(trimmed))
                    .map(|caps| caps.get(1).map(|m| m.as_str()) == Some(phase.id.as_str()))
                    .unwrap_or(false)
            })
        });
        if let Some(idx) = header_line_idx {
            result.push((phase.id.clone(), idx + 1));
        }
    }

    result
}

/// Check whether the binary version is ahead of the last completed phase.
///
/// Returns `Some(warning)` if the binary is ahead, `None` if in sync.
///
/// Resolves "last completed phase" via [`last_completed_phase_id`] (the
/// dependency-graph-based computation) rather than its own document-position
/// scan.
pub fn check_version_sync(phases: &[PlanPhase]) -> Option<String> {
    let last_done_id = last_completed_phase_id(phases);
    let highest_phase = phases
        .iter()
        .find(|p| phase_ids_match(&p.id, &last_done_id) && p.status == PlanStatus::Done)?;
    let binary = binary_version();

    // Compare binary version vs highest sequential done phase.
    // Parse both as semver tuples. Strip pre-release suffixes from binary version.
    let binary_base = binary.split('-').next().unwrap_or(binary);
    let binary_parts = parse_semver_id(&format!("v{}", binary_base))?;
    let phase_parts = parse_semver_id(&highest_phase.id)?;

    if binary_parts > phase_parts {
        Some(format!(
            "Binary version ({}) is ahead of highest sequential completed phase ({}). \
             Consider pinning for release — see CLAUDE.md 'Public Release Process'.",
            binary, highest_phase.id,
        ))
    } else {
        None
    }
}

/// Phases that are `Pending` and whose declared dependencies are all `Done`.
pub fn next_actionable_phases(phases: &[PlanPhase]) -> Vec<&PlanPhase> {
    phases
        .iter()
        .filter(|p| p.status == PlanStatus::Pending)
        .filter(|p| {
            p.depends_on.iter().all(|dep_id| {
                phases
                    .iter()
                    .find(|d| phase_ids_match(&d.id, dep_id))
                    .is_some_and(|d| d.status == PlanStatus::Done)
            })
        })
        .collect()
}

/// Partition a candidate set of pending phases into dependency waves
/// (v0.17.0.12.34) — every phase in wave N depends only on phases in waves
/// < N (already done, by construction of the candidate set) or on other
/// candidates that come earlier in the wave order; phases within the same
/// wave declare no ordering or API-impact conflict between them and are
/// safe to run concurrently.
///
/// Read-only analysis: this does not launch anything.
///
/// Dependencies pointing at phases outside the candidate set (e.g. an
/// already-`Done` phase) are treated as already satisfied and dropped
/// before graph construction — only intra-batch ordering matters here.
pub fn candidate_waves(phases: &[PlanPhase]) -> Result<Vec<Vec<String>>, String> {
    let ids: std::collections::HashSet<&str> = phases.iter().map(|p| p.id.as_str()).collect();
    let nodes: Vec<ta_workflow::WaveNode> = phases
        .iter()
        .map(|p| {
            let deps: Vec<String> = p
                .depends_on
                .iter()
                .filter(|d| ids.contains(d.as_str()) || ids.iter().any(|id| phase_ids_match(id, d)))
                .cloned()
                .collect();
            ta_workflow::WaveNode::new(p.id.clone())
                .with_deps(deps)
                .with_impact_tags(p.api_impact.clone())
        })
        .collect();

    ta_workflow::plan_waves(&nodes).map_err(|e| e.to_string())
}

// Ascending comparator: real semver IDs compare by value; a phase with a
// non-semver ID always sorts after every semver phase.
fn by_semver(a: &&PlanPhase, b: &&PlanPhase) -> std::cmp::Ordering {
    match (parse_semver_id(&a.id), parse_semver_id(&b.id)) {
        (Some(sa), Some(sb)) => sa.cmp(&sb),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// The single primary next-actionable phase ID, if any.
///
/// Consolidates the "pick one" selection out of [`next_actionable_phases`]'s
/// result set — takes the lowest-semver phase among those whose
/// dependencies are all satisfied, preferring phases with a real semver ID
/// and falling back to the first actionable phase if none parse. Returns
/// `None` when there is no next-actionable phase.
pub fn next_actionable_phase_id(phases: &[PlanPhase]) -> Option<String> {
    let actionable = next_actionable_phases(phases);
    actionable
        .iter()
        .copied()
        .filter(|p| parse_semver_id(&p.id).is_some())
        .min_by(by_semver)
        .or_else(|| actionable.first().copied())
        .map(|p| p.id.clone())
}

/// Find the last completed phase ID, used for gap-semver generation and the
/// `ta plan status` version-check line.
///
/// Derives this from the *dependency graph*, not document position: takes the
/// lowest-semver phase from [`next_actionable_phases`] (the immediate next
/// step, via [`next_actionable_phase_id`]) and returns the highest-semver
/// phase among *that phase's own* declared dependencies — i.e. what
/// specifically had to finish for the next step to become ready.
///
/// Falls back to the highest-semver `Done` phase overall when there is no
/// next-actionable phase or it has no declared dependencies.
pub fn last_completed_phase_id(phases: &[PlanPhase]) -> String {
    let primary_id = next_actionable_phase_id(phases);
    let primary = primary_id
        .as_deref()
        .and_then(|id| phases.iter().find(|p| p.id == id));

    if let Some(phase) = primary {
        let last_dep = phase
            .depends_on
            .iter()
            .filter_map(|dep_id| phases.iter().find(|d| phase_ids_match(&d.id, dep_id)))
            .filter(|d| d.status == PlanStatus::Done)
            .max_by(by_semver);
        if let Some(dep) = last_dep {
            return dep.id.clone();
        }
    }

    phases
        .iter()
        .filter(|p| p.status == PlanStatus::Done && parse_semver_id(&p.id).is_some())
        .max_by(by_semver)
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "v0.0.0".to_string())
}
