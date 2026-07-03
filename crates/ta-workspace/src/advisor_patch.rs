// advisor_patch.rs — Queues Advisor-triggered direct writes to shared files
// (PLAN.md, CLAUDE.md, Cargo.toml, memory/*.md) while a goal is running,
// instead of writing them straight to disk (v0.17.0.12.7).
//
// Problem: the Advisor dashboard can write directly to shared files (e.g.
// `add_plan_phase`) while a `ta run` goal is staging its own edits to the
// same files. Those direct writes race the eventual `ta draft apply` —
// whichever side applies last silently wins.
//
// Fix: when any goal is in flight, queue the write as a patch under
// `.ta/advisor-patches/<timestamp>-<slug>.patch` instead of writing directly.
// `ta draft apply` (`apps/ta-cli/src/commands/draft.rs`) replays queued
// patches after its own shared-file merge step, 3-way merging against
// whatever the file looks like by then.
//
// Lives in `ta-workspace` (rather than `ta-daemon`, which writes patches, or
// `ta-cli`, which replays them) because both `ta-daemon` and `ta-cli` are
// separate binaries with no library target of their own — this crate is the
// lowest-level place both already depend on.

use std::fs;
use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use ta_goal::{GoalRunState, GoalRunStore};

/// A queued advisor write, persisted as JSON at
/// `.ta/advisor-patches/<queued_at>-<slug>.patch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvisorPatch {
    /// Relative path from the project root (e.g. "PLAN.md").
    pub path: String,
    /// Base64-encoded content of the file at queue time (empty string if the
    /// file didn't exist yet).
    pub old_content_b64: String,
    /// Base64-encoded content the Advisor wanted to write.
    pub new_content_b64: String,
    /// Human-readable description of the write (e.g. "add plan phase v0.18.0").
    pub description: String,
    /// When the patch was queued (seconds since UNIX epoch).
    pub queued_at: u64,
}

impl AdvisorPatch {
    pub fn old_content(&self) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.old_content_b64)
            .unwrap_or_default()
    }

    pub fn new_content(&self) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.new_content_b64)
            .unwrap_or_default()
    }
}

/// Returns true when any goal is not yet in a terminal state — i.e. a goal
/// whose eventual `ta draft apply` could still race a direct write to a
/// shared file.
pub fn has_active_goal(store: &GoalRunStore) -> bool {
    store
        .list()
        .map(|goals| {
            goals.iter().any(|g| {
                !matches!(
                    g.state,
                    GoalRunState::Applied
                        | GoalRunState::Merged
                        | GoalRunState::Completed
                        | GoalRunState::Failed { .. }
                )
            })
        })
        .unwrap_or(false)
}

/// Convenience wrapper: opens the `GoalRunStore` for `project_root` and
/// checks for an active goal. Returns `false` (safe to write directly) when
/// the store can't be opened (e.g. `.ta/goals` doesn't exist yet).
pub fn has_active_goal_in_project(project_root: &Path) -> bool {
    let goals_dir = project_root.join(".ta").join("goals");
    match GoalRunStore::new(&goals_dir) {
        Ok(store) => has_active_goal(&store),
        Err(_) => false,
    }
}

/// Directory where queued advisor patches live, relative to the project root.
pub fn patches_dir(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".ta").join("advisor-patches")
}

/// Write `new_content` to `project_root.join(rel_path)`, or — when
/// `is_goal_active()` is true — queue it as a patch under
/// `.ta/advisor-patches/` instead of touching the file directly.
pub fn queue_or_write(
    project_root: &Path,
    rel_path: &str,
    new_content: &[u8],
    description: &str,
    is_goal_active: impl FnOnce() -> bool,
) -> std::io::Result<()> {
    let target_path = project_root.join(rel_path);

    if !is_goal_active() {
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        return fs::write(&target_path, new_content);
    }

    let old_content = fs::read(&target_path).unwrap_or_default();
    let queued_at = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let patch = AdvisorPatch {
        path: rel_path.to_string(),
        old_content_b64: base64::engine::general_purpose::STANDARD.encode(old_content),
        new_content_b64: base64::engine::general_purpose::STANDARD.encode(new_content),
        description: description.to_string(),
        queued_at,
    };

    let dir = patches_dir(project_root);
    fs::create_dir_all(&dir)?;
    let slug = slugify(description);
    let patch_path = dir.join(format!("{}-{}.patch", queued_at, slug));
    let json = serde_json::to_string_pretty(&patch)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(&patch_path, json)
}

/// Filesystem-safe slug for a patch filename: lowercase, non-alphanumeric
/// runs collapsed to a single `-`, capped at 40 chars.
fn slugify(description: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in description.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug = if slug.len() > 40 { &slug[..40] } else { slug };
    if slug.is_empty() {
        "patch".to_string()
    } else {
        slug.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn queue_or_write_writes_directly_when_no_active_goal() {
        let dir = TempDir::new().unwrap();
        queue_or_write(
            dir.path(),
            "PLAN.md",
            b"new plan content",
            "update plan",
            || false,
        )
        .unwrap();

        let content = fs::read_to_string(dir.path().join("PLAN.md")).unwrap();
        assert_eq!(content, "new plan content");
        assert!(
            !dir.path().join(".ta/advisor-patches").exists(),
            "no patch should be queued when no goal is active"
        );
    }

    #[test]
    fn queue_or_write_queues_patch_when_goal_active() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("PLAN.md"), "original plan").unwrap();

        queue_or_write(
            dir.path(),
            "PLAN.md",
            b"advisor-written plan",
            "add plan phase v0.18.0",
            || true,
        )
        .unwrap();

        // The file on disk must be untouched.
        let content = fs::read_to_string(dir.path().join("PLAN.md")).unwrap();
        assert_eq!(content, "original plan");

        let patches_dir = dir.path().join(".ta").join("advisor-patches");
        let entries: Vec<_> = fs::read_dir(&patches_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "expected exactly one queued patch");
    }

    #[test]
    fn queue_or_write_patch_file_is_valid_json_with_expected_fields() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("PLAN.md"), "original plan").unwrap();

        queue_or_write(
            dir.path(),
            "PLAN.md",
            b"advisor-written plan",
            "add plan phase v0.18.0",
            || true,
        )
        .unwrap();

        let patches_dir = dir.path().join(".ta").join("advisor-patches");
        let entry = fs::read_dir(&patches_dir).unwrap().next().unwrap().unwrap();
        let content = fs::read_to_string(entry.path()).unwrap();
        let patch: AdvisorPatch = serde_json::from_str(&content).unwrap();

        assert_eq!(patch.path, "PLAN.md");
        assert_eq!(patch.old_content(), b"original plan");
        assert_eq!(patch.new_content(), b"advisor-written plan");
        assert_eq!(patch.description, "add plan phase v0.18.0");
    }

    #[test]
    fn queue_or_write_handles_missing_original_file() {
        // A shared file that didn't exist yet (e.g. memory/notes.md created
        // for the first time) must still queue cleanly with an empty old_content.
        let dir = TempDir::new().unwrap();

        queue_or_write(
            dir.path(),
            "memory/notes.md",
            b"first note",
            "add note",
            || true,
        )
        .unwrap();

        let patches_dir = dir.path().join(".ta").join("advisor-patches");
        let entry = fs::read_dir(&patches_dir).unwrap().next().unwrap().unwrap();
        let content = fs::read_to_string(entry.path()).unwrap();
        let patch: AdvisorPatch = serde_json::from_str(&content).unwrap();

        assert_eq!(patch.path, "memory/notes.md");
        assert!(patch.old_content().is_empty());
        assert_eq!(patch.new_content(), b"first note");
    }

    #[test]
    fn slugify_collapses_and_truncates() {
        assert_eq!(
            slugify("Add Plan Phase v0.18.0!!"),
            "add-plan-phase-v0-18-0"
        );
        assert_eq!(slugify(""), "patch");
        assert_eq!(slugify("   "), "patch");
    }
}
