pub mod claude_code;
pub mod codex;
pub mod generic_file;
pub mod ollama;

pub use claude_code::ClaudeCodeChannel;
pub use codex::CodexChannel;
pub use generic_file::GenericFileChannel;
pub use ollama::OllamaChannel;

use std::path::PathBuf;

/// How a human note was delivered to the agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NoteDelivery {
    /// Written to polling file; agent reads on next cycle.
    LivePolled,
    /// Sent via agent framework API (e.g. vscode.lm).
    ApiPushed,
    /// Stored; injected at next restart/follow-up.
    Queued,
    /// Advisor answered directly; no agent injection needed.
    Answered,
}

impl std::fmt::Display for NoteDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoteDelivery::LivePolled => write!(f, "live-polled"),
            NoteDelivery::ApiPushed => write!(f, "api-pushed"),
            NoteDelivery::Queued => write!(f, "queued"),
            NoteDelivery::Answered => write!(f, "answered"),
        }
    }
}

/// Capabilities supported by a channel.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChannelCapabilities {
    /// Can inject notes while the agent is running (live).
    pub live_injection: bool,
    /// Can push notes via framework API.
    pub api_push: bool,
    /// Can queue notes for next restart.
    pub queue_notes: bool,
}

impl ChannelCapabilities {
    pub fn live_label(&self) -> &'static str {
        if self.live_injection || self.api_push {
            "Live"
        } else {
            "Queued"
        }
    }
}

/// Context passed to inject_initial.
#[derive(Debug, Clone)]
pub struct AgentContext {
    pub goal_id: String,
    pub title: String,
    pub content: String,
    pub staging_path: PathBuf,
}

/// A human note to inject mid-run.
#[derive(Debug, Clone)]
pub struct HumanNote {
    pub goal_id: String,
    pub message: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl HumanNote {
    pub fn new(goal_id: impl Into<String>, message: impl Into<String>) -> Self {
        HumanNote {
            goal_id: goal_id.into(),
            message: message.into(),
            timestamp: chrono::Utc::now(),
        }
    }
}

/// The channel type determines which AgentContextChannel implementation is used.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelType {
    /// Claude Code: prepend to CLAUDE.md, poll via advisor-notes dir.
    #[default]
    ClaudeCode,
    /// Codex: system-prompt file; push via vscode.lm API if available.
    Codex,
    /// Ollama: write agent_context.md; queue for restart.
    Ollama,
    /// Generic: manifest-declared context_file; queue for restart.
    GenericFile,
}

impl std::fmt::Display for ChannelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelType::ClaudeCode => write!(f, "ClaudeCode"),
            ChannelType::Codex => write!(f, "Codex"),
            ChannelType::Ollama => write!(f, "Ollama"),
            ChannelType::GenericFile => write!(f, "GenericFile"),
        }
    }
}

/// Unified agent context injection and mid-run human interjection.
///
/// All injection flows through this trait. Adapters implement what they support;
/// the common path calls the trait without knowing the agent type.
pub trait AgentContextChannel: Send + Sync {
    /// Inject full context at goal start (always called).
    fn inject_initial(&self, ctx: &AgentContext) -> anyhow::Result<()>;

    /// Inject a mid-run human note. Returns how the note was handled.
    fn inject_note(&self, note: &HumanNote) -> anyhow::Result<NoteDelivery>;

    /// What delivery modes this channel supports.
    fn capabilities(&self) -> ChannelCapabilities;

    /// Restore the channel to pre-goal state (called on goal end/restore).
    fn restore(&self, staging_path: &std::path::Path) -> anyhow::Result<()>;

    /// Channel type name for display.
    fn channel_type(&self) -> ChannelType;
}

/// Build the appropriate channel for a given channel type and context file.
pub fn build_channel(
    channel_type: &ChannelType,
    staging_path: PathBuf,
    context_file: &str,
) -> Box<dyn AgentContextChannel> {
    match channel_type {
        ChannelType::ClaudeCode => Box::new(ClaudeCodeChannel::new(staging_path)),
        ChannelType::Codex => Box::new(CodexChannel::new(staging_path, context_file)),
        ChannelType::Ollama => Box::new(OllamaChannel::new(staging_path)),
        ChannelType::GenericFile => Box::new(GenericFileChannel::new(staging_path, context_file)),
    }
}
