//! Staged-resource conflict detection (v0.17.11.7) — "has anyone already
//! staged a real change touching this resource," answered from actual
//! staged (drafted, not-yet-applied) artifacts rather than presence's
//! self-declared, ephemeral intent. Strictly higher-signal than
//! [`crate::discovery::is_anyone_touching`]: a draft's artifacts reflect
//! what was *actually* produced, not what an agent predicted it would
//! touch before starting.
//!
//! [`DraftLookup`] is a trait, not a direct `ta-changeset` dependency —
//! `ta-agent-whiteboard` stays a reusable coordination primitive
//! independent of TA's specific staging implementation, the same reasoning
//! that already justifies [`crate::transport::WhiteboardTransport`] being a
//! trait rather than a hardcoded NATS client. The caller (daemon/CLI, which
//! already depends on both crates) supplies a `ta-changeset`-backed impl.
//!
//! Scope for this phase: `fs://` resources only (matches where the two
//! systems already overlap in practice). See
//! `docs/design/staged-resource-conflict-detection.md` for what was
//! deliberately cut (DB resources, VCS as a distinct domain, `task-graph`
//! wave-scheduler enforcement) and why. Advisory-only, same as
//! `is_anyone_touching` — this answers a question, it does not block
//! anything.

use crate::error::Result;
use crate::resource_match::glob_overlap;

/// One staged-but-not-yet-applied draft's resource footprint, as seen by
/// whatever backs [`DraftLookup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDraftResources {
    pub draft_id: String,
    pub goal_run_id: String,
    /// Resource URIs the draft's artifacts touch (`"fs://workspace/..."` —
    /// this phase's scope; other schemes are simply never matched yet, not
    /// rejected, so a future scope widening needs no shape change here).
    pub resource_uris: Vec<String>,
}

/// A detected overlap between a queried resource and a staged draft's
/// actual footprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedConflict {
    /// The declared resource URI from the conflicting draft (not the query
    /// pattern) — the specific thing that's actually at risk.
    pub resource_uri: String,
    pub draft_id: String,
    pub goal_run_id: String,
}

/// Source of "what's currently staged and not yet applied" — implemented
/// by the caller against `ta-changeset`'s draft store. Returning `Err`
/// here should be rare (a real I/O/parse failure reading the store, not
/// "no drafts found," which is `Ok(vec![])`).
pub trait DraftLookup {
    /// All drafts whose status is not yet terminal in a way that means
    /// their changes are either already real (`Applied`) or no longer
    /// relevant (`Denied`/`Superseded`/`Closed`) — i.e. `Draft`,
    /// `PendingReview`, or `Approved` (approved-but-not-yet-`ta draft
    /// apply`'d changes are still staged and still a real conflict risk).
    fn pending_draft_resources(&self) -> Result<Vec<PendingDraftResources>>;
}

/// Does any currently-staged draft already touch a resource overlapping
/// `resource_uris`? Pure function over whatever [`DraftLookup`] returns —
/// no transport/network I/O, easily unit-tested against a fixture.
pub fn staged_conflicts_for(
    drafts: &dyn DraftLookup,
    resource_uris: &[String],
) -> Result<Vec<StagedConflict>> {
    if resource_uris.is_empty() {
        return Ok(Vec::new());
    }
    let pending = drafts.pending_draft_resources()?;
    let mut conflicts = Vec::new();
    for draft in &pending {
        for declared in &draft.resource_uris {
            if glob_overlap(declared, resource_uris) {
                conflicts.push(StagedConflict {
                    resource_uri: declared.clone(),
                    draft_id: draft.draft_id.clone(),
                    goal_run_id: draft.goal_run_id.clone(),
                });
            }
        }
    }
    Ok(conflicts)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureLookup {
        drafts: Vec<PendingDraftResources>,
    }

    impl DraftLookup for FixtureLookup {
        fn pending_draft_resources(&self) -> Result<Vec<PendingDraftResources>> {
            Ok(self.drafts.clone())
        }
    }

    fn draft(id: &str, goal_run_id: &str, resources: &[&str]) -> PendingDraftResources {
        PendingDraftResources {
            draft_id: id.to_string(),
            goal_run_id: goal_run_id.to_string(),
            resource_uris: resources.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn detects_overlapping_fs_uri() {
        let lookup = FixtureLookup {
            drafts: vec![draft(
                "draft-1",
                "goal-1",
                &["fs://workspace/src/auth/login.rs"],
            )],
        };

        let conflicts =
            staged_conflicts_for(&lookup, &["fs://workspace/src/auth/**".to_string()]).unwrap();

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].draft_id, "draft-1");
        assert_eq!(conflicts[0].goal_run_id, "goal-1");
        assert_eq!(
            conflicts[0].resource_uri,
            "fs://workspace/src/auth/login.rs"
        );
    }

    #[test]
    fn no_conflict_for_non_overlapping_resources() {
        let lookup = FixtureLookup {
            drafts: vec![draft(
                "draft-1",
                "goal-1",
                &["fs://workspace/docs/readme.md"],
            )],
        };

        let conflicts =
            staged_conflicts_for(&lookup, &["fs://workspace/src/**".to_string()]).unwrap();

        assert!(conflicts.is_empty());
    }

    #[test]
    fn empty_draft_store_returns_empty() {
        let lookup = FixtureLookup { drafts: vec![] };

        let conflicts =
            staged_conflicts_for(&lookup, &["fs://workspace/src/**".to_string()]).unwrap();

        assert!(conflicts.is_empty());
    }

    #[test]
    fn empty_query_returns_empty_without_consulting_lookup() {
        let lookup = FixtureLookup {
            drafts: vec![draft("draft-1", "goal-1", &["fs://workspace/src/lib.rs"])],
        };

        let conflicts = staged_conflicts_for(&lookup, &[]).unwrap();

        assert!(conflicts.is_empty());
    }

    #[test]
    fn matches_when_declared_is_the_broader_glob() {
        // The staged draft declared a broad glob; the query is a specific
        // file within it — same both-directions matching as
        // discovery::is_anyone_touching.
        let lookup = FixtureLookup {
            drafts: vec![draft("draft-1", "goal-1", &["fs://workspace/src/**"])],
        };

        let conflicts =
            staged_conflicts_for(&lookup, &["fs://workspace/src/lib.rs".to_string()]).unwrap();

        assert_eq!(conflicts.len(), 1);
    }

    #[test]
    fn multiple_drafts_each_contribute_their_own_conflicts() {
        let lookup = FixtureLookup {
            drafts: vec![
                draft("draft-1", "goal-1", &["fs://workspace/src/auth.rs"]),
                draft("draft-2", "goal-2", &["fs://workspace/src/session.rs"]),
                draft("draft-3", "goal-3", &["fs://workspace/docs/readme.md"]),
            ],
        };

        let conflicts =
            staged_conflicts_for(&lookup, &["fs://workspace/src/**".to_string()]).unwrap();

        assert_eq!(conflicts.len(), 2);
        let draft_ids: Vec<&str> = conflicts.iter().map(|c| c.draft_id.as_str()).collect();
        assert!(draft_ids.contains(&"draft-1"));
        assert!(draft_ids.contains(&"draft-2"));
    }
}
