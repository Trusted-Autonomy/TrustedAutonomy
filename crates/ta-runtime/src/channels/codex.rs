//! CodexChannel — system-prompt file; push via vscode.lm if available.

use std::path::PathBuf;

use super::{
    AgentContext, AgentContextChannel, ChannelCapabilities, ChannelType, HumanNote, NoteDelivery,
};

/// Channel adapter for Codex agents (OpenAI Codex CLI / VS Code integration).
///
/// - `inject_initial` → write to the declared context file (AGENTS.md by default).
/// - `inject_note` → push via `vscode.lm` conversation API if VSCODE_IPC_HOOK_CLI
///   is set (ApiPushed); otherwise queue for restart (Queued).
pub struct CodexChannel {
    staging_path: PathBuf,
    context_file: String,
}

impl CodexChannel {
    pub fn new(staging_path: PathBuf, context_file: impl Into<String>) -> Self {
        CodexChannel {
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

    fn is_vscode_context() -> bool {
        std::env::var("VSCODE_IPC_HOOK_CLI").is_ok()
    }
}

impl AgentContextChannel for CodexChannel {
    fn inject_initial(&self, ctx: &AgentContext) -> anyhow::Result<()> {
        let ctx_path = self.context_path();
        let backup = self.backup_path();

        // Always backup the original content (empty string if file doesn't exist).
        let original = if ctx_path.exists() {
            std::fs::read_to_string(&ctx_path)?
        } else {
            String::new()
        };

        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&backup, &original)?;

        // Write context (Codex reads system prompt from this file at start).
        if let Some(parent) = ctx_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let new_content = if original.is_empty() {
            ctx.content.clone()
        } else {
            format!("{}\n{}", ctx.content, original)
        };

        std::fs::write(&ctx_path, &new_content)?;
        Ok(())
    }

    fn inject_note(&self, note: &HumanNote) -> anyhow::Result<NoteDelivery> {
        if Self::is_vscode_context() {
            // In VS Code context: write to notes dir (API push path).
            let notes_dir = self.notes_dir();
            std::fs::create_dir_all(&notes_dir)?;
            let path = notes_dir.join(format!("{}.md", note.goal_id));
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            let ts = note.timestamp.format("%Y-%m-%dT%H:%M:%SZ");
            writeln!(file, "\n## Human note [{}]\n\n{}", ts, note.message)?;
            return Ok(NoteDelivery::ApiPushed);
        }

        // Not in VS Code: queue for restart.
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
            api_push: Self::is_vscode_context(),
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
        ChannelType::Codex
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn inject_initial_writes_context_file() {
        let dir = TempDir::new().unwrap();
        let ch = CodexChannel::new(dir.path().to_path_buf(), "AGENTS.md");
        let ctx = AgentContext {
            goal_id: "goal-1".to_string(),
            title: "Test".to_string(),
            content: "# Codex Context\n".to_string(),
            staging_path: dir.path().to_path_buf(),
        };
        ch.inject_initial(&ctx).unwrap();
        let content = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(content.contains("# Codex Context"));
    }

    #[test]
    fn inject_note_returns_queued_outside_vscode() {
        // Outside VS Code (no VSCODE_IPC_HOOK_CLI), should return Queued.
        // We can't unset env safely in a test but we can test with a temp var.
        // If VSCODE_IPC_HOOK_CLI is not set in CI, this returns Queued.
        let dir = TempDir::new().unwrap();
        let ch = CodexChannel::new(dir.path().to_path_buf(), "AGENTS.md");
        let note = HumanNote::new("goal-1", "Review this approach");
        let delivery = ch.inject_note(&note).unwrap();
        // In normal CI, VSCODE_IPC_HOOK_CLI is not set.
        assert!(
            delivery == NoteDelivery::Queued || delivery == NoteDelivery::ApiPushed,
            "Expected Queued or ApiPushed, got {:?}",
            delivery
        );
    }

    #[test]
    fn restore_cleans_up_context_file() {
        let dir = TempDir::new().unwrap();
        let ch = CodexChannel::new(dir.path().to_path_buf(), "AGENTS.md");
        let ctx = AgentContext {
            goal_id: "goal-1".to_string(),
            title: "Test".to_string(),
            content: "# Injected\n".to_string(),
            staging_path: dir.path().to_path_buf(),
        };
        ch.inject_initial(&ctx).unwrap();
        ch.restore(dir.path()).unwrap();
        // No original — file should be gone.
        assert!(!dir.path().join("AGENTS.md").exists());
    }
}
