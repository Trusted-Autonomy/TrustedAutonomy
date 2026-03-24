//! # ta-changeset
//!
//! The universal "staged mutation" data model for Trusted Autonomy.
//!
//! A [`ChangeSet`] represents any pending change — a file patch, email draft,
//! DB mutation, or social media post. All changes are collected (staged) by
//! default and bundled into a [`DraftPackage`] for human review.
//!
//! The data model aligns with `schema/draft_package.schema.json`.

pub mod changeset;
pub mod channel_registry;
pub mod diff;
pub mod diff_handlers;
pub mod draft_package;
pub mod error;
pub mod explanation;
pub mod interaction;
pub mod interactive_session_store;
pub mod multi_channel;
pub mod output_adapters;
pub mod plugin;
pub mod plugin_resolver;
pub mod pr_package;
pub mod project_manifest;
pub mod registry_client;
pub mod review_channel;
pub mod review_session;
pub mod review_session_store;
pub mod session_channel;
pub mod sources;
pub mod supervisor;
pub mod supervisor_review;
pub mod terminal_channel;
pub mod uri_pattern;
pub mod webhook_channel;

pub use changeset::{ChangeKind, ChangeSet, CommitIntent};
pub use channel_registry::{
    ChannelCapabilitySet, ChannelFactory, ChannelRegistry, ChannelRouteConfig,
    ChannelRoutingConfig, EscalationRouteConfig, NotifyRouteConfig, ReviewRouteConfig, TaConfig,
};
pub use diff::DiffContent;
pub use diff_handlers::{DiffHandlerError, DiffHandlersConfig, HandlerRule};
pub use draft_package::{
    ActionKind, ApprovalRecord, DesignAlternative, DraftPackage, DraftStatus, ExplanationTiers,
    IgnoredArtifact, PendingAction, ValidationEntry, VcsTrackingInfo,
};
pub use error::ChangeSetError;
pub use explanation::ExplanationSidecar;
pub use interaction::{
    ChannelCapabilities, Decision, InteractionKind, InteractionRequest, InteractionResponse,
    Notification, NotificationLevel, Urgency,
};
pub use interactive_session_store::InteractiveSessionStore;
pub use multi_channel::{MultiChannelStrategy, MultiReviewChannel};
pub use output_adapters::{DetailLevel, OutputAdapter, OutputFormat, RenderContext};
pub use review_channel::{build_channel, ReviewChannel, ReviewChannelConfig, ReviewChannelError};
pub use review_session::{
    ArtifactReview, Comment, CommentThread, DispositionCounts, ReviewReasoning, ReviewSession,
    ReviewState, SessionNote,
};
pub use review_session_store::ReviewSessionStore;
pub use session_channel::{
    HumanInput, InteractiveConfig, InteractiveSession, InteractiveSessionState, OutputStream,
    SessionChannel, SessionChannelError, SessionEvent, SessionMessage,
};
pub use sources::{
    CachedItem, ExternalSource, LockEntry, Lockfile, PackageManifest, SourceCache, SourceError,
};
pub use supervisor::{
    DependencyGraph, SupervisorAgent, ValidationError, ValidationResult, ValidationWarning,
};
pub use supervisor_review::{
    build_supervisor_prompt, fallback_supervisor_review, invoke_supervisor_agent,
    load_constitution, SupervisorReview, SupervisorRunConfig, SupervisorVerdict,
};
pub use terminal_channel::{AutoApproveChannel, TerminalChannel, TerminalSessionChannel};
pub use uri_pattern::{filter_uris, matches_uri};
pub use webhook_channel::WebhookChannel;

// Backwards compatibility: export old names as aliases
pub use draft_package::DraftPackage as PRPackage;
pub use draft_package::DraftStatus as PRStatus;
