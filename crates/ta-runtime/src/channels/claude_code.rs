//! ClaudeCodeChannel — injects context into CLAUDE.md, polls via advisor-notes.

use std::path::PathBuf;

use super::{
    AgentContext, AgentContextChannel, ChannelCapabilities, ChannelType, HumanNote, NoteDelivery,
};

const BACKUP_SUFFIX: &str = ".ta/claude_md_original";
const NO_ORIGINAL: &str = "__TA_NO_ORIGINAL__";

/// Channel adapter for Claude Code agents.
///
/// - `inject_initial` → prepend goal context to CLAUDE.md (backup + restore pattern).
/// - `inject_note` → append to `.ta/advisor-notes/<goal-id>.md` (LivePolled).
/// - `restore` → restore CLAUDE.md from backup.
pub struct ClaudeCodeChannel {
    staging_path: PathBuf,
}

impl ClaudeCodeChannel {
    pub fn new(staging_path: PathBuf) -> Self {
        ClaudeCodeChannel { staging_path }
    }

    fn claude_md_path(&self) -> PathBuf {
        self.staging_path.join("CLAUDE.md")
    }

    fn backup_path(&self) -> PathBuf {
        self.staging_path.join(BACKUP_SUFFIX)
    }

    pub fn notes_path(&self, goal_id: &str) -> PathBuf {
        self.staging_path
            .join(".ta/advisor-notes")
            .join(format!("{}.md", goal_id))
    }
}

impl AgentContextChannel for ClaudeCodeChannel {
    fn inject_initial(&self, ctx: &AgentContext) -> anyhow::Result<()> {
        let claude_md = self.claude_md_path();
        let backup = self.backup_path();

        // If a backup already exists from a previous injection, restore from it first.
        if backup.exists() {
            let saved = std::fs::read_to_string(&backup)?;
            if saved == NO_ORIGINAL {
                if claude_md.exists() {
                    std::fs::remove_file(&claude_md)?;
                }
            } else {
                std::fs::write(&claude_md, &saved)?;
            }
        }

        // Save original content.
        let original = if claude_md.exists() {
            std::fs::read_to_string(&claude_md)?
        } else {
            NO_ORIGINAL.to_string()
        };

        // Write backup.
        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&backup, &original)?;

        // Prepend context to CLAUDE.md.
        let new_content = if original == NO_ORIGINAL {
            ctx.content.clone()
        } else {
            format!("{}\n{}", ctx.content, original)
        };

        if let Some(parent) = claude_md.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&claude_md, &new_content)?;

        Ok(())
    }

    fn inject_note(&self, note: &HumanNote) -> anyhow::Result<NoteDelivery> {
        let notes_path = self.notes_path(&note.goal_id);
        if let Some(parent) = notes_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let timestamp = note.timestamp.format("%Y-%m-%dT%H:%M:%SZ");
        let entry = format!("\n## Human note [{}]\n\n{}\n", timestamp, note.message);

        // Append to notes file.
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&notes_path)?;
        file.write_all(entry.as_bytes())?;

        Ok(NoteDelivery::LivePolled)
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            live_injection: true,
            api_push: false,
            queue_notes: true,
        }
    }

    fn restore(&self, _staging_path: &std::path::Path) -> anyhow::Result<()> {
        let claude_md = self.claude_md_path();
        let backup = self.backup_path();

        if !backup.exists() {
            return Ok(());
        }

        let saved = std::fs::read_to_string(&backup)?;
        if saved == NO_ORIGINAL {
            if claude_md.exists() {
                std::fs::remove_file(&claude_md)?;
            }
        } else {
            std::fs::write(&claude_md, &saved)?;
        }

        std::fs::remove_file(&backup)?;
        Ok(())
    }

    fn channel_type(&self) -> ChannelType {
        ChannelType::ClaudeCode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_channel(dir: &TempDir) -> ClaudeCodeChannel {
        ClaudeCodeChannel::new(dir.path().to_path_buf())
    }

    fn make_ctx(dir: &TempDir, content: &str) -> AgentContext {
        AgentContext {
            goal_id: "test-goal-1".to_string(),
            title: "Test Goal".to_string(),
            content: content.to_string(),
            staging_path: dir.path().to_path_buf(),
        }
    }

    #[test]
    fn inject_initial_writes_claude_md() {
        let dir = TempDir::new().unwrap();
        let ch = make_channel(&dir);
        let ctx = make_ctx(&dir, "# TA Context\n\nGoal: test");
        ch.inject_initial(&ctx).unwrap();
        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains("# TA Context"));
    }

    #[test]
    fn inject_initial_prepends_to_existing_claude_md() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Original\n").unwrap();
        let ch = make_channel(&dir);
        let ctx = make_ctx(&dir, "# Injected\n");
        ch.inject_initial(&ctx).unwrap();
        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(content.starts_with("# Injected\n"));
        assert!(content.contains("# Original\n"));
    }

    #[test]
    fn restore_removes_injected_content() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Original\n").unwrap();
        let ch = make_channel(&dir);
        let ctx = make_ctx(&dir, "# Injected\n");
        ch.inject_initial(&ctx).unwrap();
        ch.restore(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content, "# Original\n");
    }

    #[test]
    fn inject_note_returns_live_polled() {
        let dir = TempDir::new().unwrap();
        let ch = make_channel(&dir);
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let note = HumanNote::new("test-goal-1", "Please fix the login bug");
        let delivery = ch.inject_note(&note).unwrap();
        assert_eq!(delivery, NoteDelivery::LivePolled);
        let path = ch.notes_path("test-goal-1");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Please fix the login bug"));
    }

    #[test]
    fn capabilities_has_live_injection() {
        let dir = TempDir::new().unwrap();
        let ch = make_channel(&dir);
        let caps = ch.capabilities();
        assert!(caps.live_injection);
        assert!(!caps.api_push);
    }
}
