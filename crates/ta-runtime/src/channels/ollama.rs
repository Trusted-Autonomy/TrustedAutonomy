//! OllamaChannel — writes agent_context.md; queues notes for restart.

use std::path::PathBuf;

use super::{
    AgentContext, AgentContextChannel, ChannelCapabilities, ChannelType, HumanNote, NoteDelivery,
};

/// Channel adapter for Ollama agents.
///
/// - `inject_initial` → write `.ta/agent_context.md`.
/// - `inject_note` → Queued (system-prompt restart required).
pub struct OllamaChannel {
    staging_path: PathBuf,
}

impl OllamaChannel {
    pub fn new(staging_path: PathBuf) -> Self {
        OllamaChannel { staging_path }
    }

    fn context_path(&self) -> PathBuf {
        self.staging_path.join(".ta/agent_context.md")
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

impl AgentContextChannel for OllamaChannel {
    fn inject_initial(&self, ctx: &AgentContext) -> anyhow::Result<()> {
        let path = self.context_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &ctx.content)?;
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
        let path = self.context_path();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    fn channel_type(&self) -> ChannelType {
        ChannelType::Ollama
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
    fn inject_initial_writes_agent_context() {
        let dir = TempDir::new().unwrap();
        let ch = OllamaChannel::new(dir.path().to_path_buf());
        let ctx = AgentContext {
            goal_id: "g1".to_string(),
            title: "T".to_string(),
            content: "# Ollama Context\n".to_string(),
            staging_path: dir.path().to_path_buf(),
        };
        ch.inject_initial(&ctx).unwrap();
        let path = dir.path().join(".ta/agent_context.md");
        assert!(path.exists());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("# Ollama Context"));
    }

    #[test]
    fn inject_note_returns_queued() {
        let dir = TempDir::new().unwrap();
        let ch = OllamaChannel::new(dir.path().to_path_buf());
        let note = HumanNote::new("g1", "Try a different approach");
        let delivery = ch.inject_note(&note).unwrap();
        assert_eq!(delivery, NoteDelivery::Queued);
    }

    #[test]
    fn inject_persona_appends_to_context_file() {
        let dir = TempDir::new().unwrap();
        let ch = OllamaChannel::new(dir.path().to_path_buf());
        let ctx = AgentContext {
            goal_id: "g1".to_string(),
            title: "T".to_string(),
            content: "# Base\n".to_string(),
            staging_path: dir.path().to_path_buf(),
        };
        ch.inject_initial(&ctx).unwrap();
        ch.inject_persona("\n## Persona\n\nBe concise.\n").unwrap();
        let content = std::fs::read_to_string(dir.path().join(".ta/agent_context.md")).unwrap();
        assert!(content.contains("# Base"));
        assert!(content.contains("## Persona"));
    }

    #[test]
    fn inject_failure_context_appends_to_context_file() {
        let dir = TempDir::new().unwrap();
        let ch = OllamaChannel::new(dir.path().to_path_buf());
        let ctx = AgentContext {
            goal_id: "g1".to_string(),
            title: "T".to_string(),
            content: "# Base\n".to_string(),
            staging_path: dir.path().to_path_buf(),
        };
        ch.inject_initial(&ctx).unwrap();
        ch.inject_failure_context("\n## Verification Failures\n\nFix it.\n")
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join(".ta/agent_context.md")).unwrap();
        assert!(content.contains("## Verification Failures"));
    }
}
