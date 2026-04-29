//! GenericFileChannel — writes to manifest-declared context_file; queues notes.

use std::path::PathBuf;

use super::{
    AgentContext, AgentContextChannel, ChannelCapabilities, ChannelType, HumanNote, NoteDelivery,
};

/// Channel adapter for generic file-based agent injection.
///
/// - `inject_initial` → write to manifest-declared `context_file`.
/// - `inject_note` → Queued.
pub struct GenericFileChannel {
    staging_path: PathBuf,
    context_file: String,
}

impl GenericFileChannel {
    pub fn new(staging_path: PathBuf, context_file: impl Into<String>) -> Self {
        GenericFileChannel {
            staging_path,
            context_file: context_file.into(),
        }
    }

    fn context_path(&self) -> PathBuf {
        self.staging_path.join(&self.context_file)
    }

    fn backup_path(&self) -> PathBuf {
        self.staging_path
            .join(".ta")
            .join(format!("{}.backup", self.context_file.replace('/', "_")))
    }

    fn notes_dir(&self) -> PathBuf {
        self.staging_path.join(".ta/advisor-notes")
    }

    fn append_to_context_file(&self, section: &str) -> anyhow::Result<()> {
        let path = self.context_path();
        if path.exists() {
            let existing = std::fs::read_to_string(&path)?;
            std::fs::write(&path, format!("{}{}", existing, section))?;
        }
        Ok(())
    }
}

impl AgentContextChannel for GenericFileChannel {
    fn inject_initial(&self, ctx: &AgentContext) -> anyhow::Result<()> {
        let ctx_path = self.context_path();
        let backup = self.backup_path();

        // Backup original if present.
        let original = if ctx_path.exists() {
            std::fs::read_to_string(&ctx_path)?
        } else {
            String::new()
        };

        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&backup, &original)?;

        let new_content = if original.is_empty() {
            ctx.content.clone()
        } else {
            format!("{}\n{}", ctx.content, original)
        };

        if let Some(parent) = ctx_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&ctx_path, &new_content)?;
        Ok(())
    }

    fn inject_note(&self, note: &HumanNote) -> anyhow::Result<NoteDelivery> {
        let notes_dir = self.notes_dir();
        std::fs::create_dir_all(&notes_dir)?;
        let path = notes_dir.join(format!("{}-queued.md", note.goal_id));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let ts = note.timestamp.format("%Y-%m-%dT%H:%M:%SZ");
        writeln!(file, "\n## Queued note [{}]\n\n{}", ts, note.message)?;
        Ok(NoteDelivery::Queued)
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            live_injection: false,
            api_push: false,
            queue_notes: true,
        }
    }

    fn restore(&self, _staging_path: &std::path::Path) -> anyhow::Result<()> {
        let ctx_path = self.context_path();
        let backup = self.backup_path();

        if backup.exists() {
            let original = std::fs::read_to_string(&backup)?;
            if original.is_empty() {
                if ctx_path.exists() {
                    std::fs::remove_file(&ctx_path)?;
                }
            } else {
                std::fs::write(&ctx_path, &original)?;
            }
            std::fs::remove_file(&backup)?;
        }
        Ok(())
    }

    fn channel_type(&self) -> ChannelType {
        ChannelType::GenericFile
    }

    fn inject_persona(&self, persona_section: &str) -> anyhow::Result<()> {
        self.append_to_context_file(persona_section)
    }

    fn inject_work_plan(&self, plan_section: &str) -> anyhow::Result<()> {
        self.append_to_context_file(plan_section)
    }

    fn inject_failure_context(&self, failure_context: &str) -> anyhow::Result<()> {
        self.append_to_context_file(failure_context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn inject_initial_writes_to_declared_file() {
        let dir = TempDir::new().unwrap();
        let ch = GenericFileChannel::new(dir.path().to_path_buf(), "context.md");
        let ctx = AgentContext {
            goal_id: "g1".to_string(),
            title: "T".to_string(),
            content: "# Generic Context\n".to_string(),
            staging_path: dir.path().to_path_buf(),
        };
        ch.inject_initial(&ctx).unwrap();
        let path = dir.path().join("context.md");
        assert!(path.exists());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("# Generic Context"));
    }

    #[test]
    fn inject_note_returns_queued() {
        let dir = TempDir::new().unwrap();
        let ch = GenericFileChannel::new(dir.path().to_path_buf(), "context.md");
        let note = HumanNote::new("g1", "Update the approach");
        let delivery = ch.inject_note(&note).unwrap();
        assert_eq!(delivery, NoteDelivery::Queued);
    }

    #[test]
    fn inject_persona_appends_to_context_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("context.md"), "# Base\n").unwrap();
        let ch = GenericFileChannel::new(dir.path().to_path_buf(), "context.md");
        ch.inject_persona("\n## Persona\n\nBe concise.\n").unwrap();
        let content = std::fs::read_to_string(dir.path().join("context.md")).unwrap();
        assert!(content.starts_with("# Base\n"));
        assert!(content.contains("## Persona"));
    }

    #[test]
    fn inject_failure_context_appends_to_context_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("context.md"), "# Base\n").unwrap();
        let ch = GenericFileChannel::new(dir.path().to_path_buf(), "context.md");
        ch.inject_failure_context("\n## Verification Failures\n\nFix it.\n")
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("context.md")).unwrap();
        assert!(content.contains("## Verification Failures"));
    }

    #[test]
    fn restore_brings_back_original() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("context.md"), "# Original\n").unwrap();
        let ch = GenericFileChannel::new(dir.path().to_path_buf(), "context.md");
        let ctx = AgentContext {
            goal_id: "g1".to_string(),
            title: "T".to_string(),
            content: "# Injected\n".to_string(),
            staging_path: dir.path().to_path_buf(),
        };
        ch.inject_initial(&ctx).unwrap();
        ch.restore(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join("context.md")).unwrap();
        assert_eq!(content, "# Original\n");
    }
}
