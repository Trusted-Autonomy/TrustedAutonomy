// connector.rs — FsConnector: filesystem operations through the staging model.
//
// The FsConnector is the bridge between MCP-style tool calls (read, write, list)
// and the Trusted Autonomy staging system. Every write goes to a staging
// directory and produces a ChangeSet. Approved changes are applied to the
// real target directory via `apply()`.
//
// Flow:
//   1. Agent calls `write_patch(path, content)` → staged in temp dir, ChangeSet created
//   2. Agent calls `build_pr_package(...)` → bundles all staged changes
//   3. Human reviews and approves
//   4. Agent calls `apply(target_dir)` → copies staged files to real filesystem

use std::fs;
use std::path::Path;

use chrono::Utc;
use uuid::Uuid;

use ta_audit::{AuditAction, AuditEvent, AuditLog};
use ta_changeset::pr_package::*;
use ta_changeset::{ChangeKind, ChangeSet, CommitIntent, DiffContent};
use ta_workspace::{ChangeStore, StagingWorkspace};

use crate::error::FsConnectorError;

/// Filesystem connector — bridges MCP tool calls to staging + changeset model.
///
/// Generic over `S: ChangeStore` so we can use any storage backend
/// (JsonFileStore for MVP, SqliteStore later) without changing this code.
///
/// In Rust, `<S: ChangeStore>` means "S is a type parameter that must
/// implement the ChangeStore trait". This is similar to generics in other
/// languages but enforced at compile time (zero runtime cost).
pub struct FsConnector<S: ChangeStore> {
    /// The goal this connector is working on.
    goal_id: String,

    /// The staging workspace where files are staged before review.
    staging: StagingWorkspace,

    /// The change store for persisting changesets.
    store: S,

    /// Optional audit log for recording operations.
    audit_log: Option<AuditLog>,

    /// The agent ID performing operations (for audit events).
    agent_id: String,
}

impl<S: ChangeStore> FsConnector<S> {
    /// Create a new filesystem connector.
    ///
    /// - `goal_id`: identifies which goal this work belongs to
    /// - `staging`: the ephemeral workspace for staging files
    /// - `store`: where to persist changeset records
    /// - `agent_id`: the agent performing operations (for audit)
    pub fn new(
        goal_id: impl Into<String>,
        staging: StagingWorkspace,
        store: S,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            goal_id: goal_id.into(),
            staging,
            store,
            audit_log: None,
            agent_id: agent_id.into(),
        }
    }

    /// Attach an audit log to record operations.
    pub fn with_audit_log(mut self, log: AuditLog) -> Self {
        self.audit_log = Some(log);
        self
    }

    /// Read a file from the source filesystem (not the staging area).
    ///
    /// This reads the *original* file content from the real filesystem.
    /// If the file has been staged, use `read_staged` to see the modified version.
    pub fn read_source(
        &mut self,
        source_dir: &Path,
        relative_path: &str,
    ) -> Result<Vec<u8>, FsConnectorError> {
        // Reject path traversal attempts.
        if relative_path.contains("..") {
            return Err(FsConnectorError::PathTraversal {
                path: relative_path.to_string(),
            });
        }

        let full_path = source_dir.join(relative_path);
        let content = fs::read(&full_path).map_err(|source| FsConnectorError::IoError {
            path: full_path,
            source,
        })?;

        // Snapshot the original content so we can generate diffs later.
        self.staging
            .snapshot_original(relative_path, content.clone());

        self.log_event(
            AuditAction::ToolCall,
            &format!("fs://source/{}", relative_path),
        )?;

        Ok(content)
    }

    /// Read a file from the staging workspace.
    pub fn read_staged(&self, relative_path: &str) -> Result<Vec<u8>, FsConnectorError> {
        Ok(self.staging.read_file(relative_path)?)
    }

    /// Stage a file write — the core operation.
    ///
    /// This is the equivalent of an MCP `write_patch` tool call. The file
    /// is written to the staging directory and a ChangeSet is created to
    /// track the change. Nothing touches the real filesystem until `apply()`.
    ///
    /// Returns the ChangeSet so the caller can inspect it or include it
    /// in a PR package.
    pub fn write_patch(
        &mut self,
        relative_path: &str,
        content: &[u8],
    ) -> Result<ChangeSet, FsConnectorError> {
        // Write to staging directory.
        self.staging.write_file(relative_path, content)?;

        // Generate a diff for the changeset.
        let diff = self.staging.diff_file(relative_path)?;

        // Determine if this is a new file or a modification.
        let diff_content = match diff {
            Some(diff_text) => {
                // Check if the diff header indicates a new file.
                if diff_text.starts_with("--- /dev/null") {
                    DiffContent::CreateFile {
                        content: String::from_utf8_lossy(content).to_string(),
                    }
                } else {
                    DiffContent::UnifiedDiff { content: diff_text }
                }
            }
            None => {
                // No diff means no change — but since we were asked to write,
                // treat it as a create with the current content.
                DiffContent::CreateFile {
                    content: String::from_utf8_lossy(content).to_string(),
                }
            }
        };

        let target_uri = format!("fs://workspace/{}", relative_path);

        // Create the changeset.
        let changeset = ChangeSet::new(target_uri.clone(), ChangeKind::FsPatch, diff_content)
            .with_commit_intent(CommitIntent::RequestCommit);

        // Persist to the change store.
        self.store.save(&self.goal_id, &changeset)?;

        self.log_event(AuditAction::ToolCall, &target_uri)?;

        Ok(changeset)
    }

    /// List all files currently staged.
    pub fn list_staged(&self) -> Result<Vec<String>, FsConnectorError> {
        Ok(self.staging.list_files()?)
    }

    /// Get the diff for a specific staged file.
    pub fn diff_file(&self, relative_path: &str) -> Result<Option<String>, FsConnectorError> {
        Ok(self.staging.diff_file(relative_path)?)
    }

    /// List all changesets for this goal.
    pub fn list_changesets(&self) -> Result<Vec<ChangeSet>, FsConnectorError> {
        Ok(self.store.list(&self.goal_id)?)
    }

    /// Build a PR package from all staged changes.
    ///
    /// This bundles all changesets into a reviewable artifact. The PR package
    /// includes a summary, the changes, risk assessment, and review requests.
    ///
    /// The caller provides the high-level context (goal title, summary, etc.)
    /// and this method fills in the changes from the store.
    pub fn build_pr_package(
        &self,
        goal_title: &str,
        goal_objective: &str,
        summary_what: &str,
        summary_why: &str,
    ) -> Result<PRPackage, FsConnectorError> {
        let changesets = self.store.list(&self.goal_id)?;

        if changesets.is_empty() {
            return Err(FsConnectorError::NoStagedChanges {
                goal_id: self.goal_id.clone(),
            });
        }

        // Convert changesets to artifacts for the PR package.
        let artifacts: Vec<Artifact> = changesets
            .iter()
            .map(|cs| {
                let change_type = match &cs.diff_content {
                    DiffContent::CreateFile { .. } => ChangeType::Add,
                    DiffContent::DeleteFile => ChangeType::Delete,
                    DiffContent::UnifiedDiff { .. } => ChangeType::Modify,
                    DiffContent::BinarySummary { .. } => ChangeType::Modify,
                };
                Artifact {
                    resource_uri: cs.target_uri.clone(),
                    change_type,
                    diff_ref: cs.changeset_id.to_string(),
                    tests_run: vec![],
                    disposition: Default::default(),
                    rationale: None,
                    dependencies: vec![],
                    explanation_tiers: None,
                    comments: None,
                    amendment: None,
                    kind: None,
                }
            })
            .collect();

        let package = PRPackage {
            package_version: "1.0.0".to_string(),
            package_id: Uuid::new_v4(),
            created_at: Utc::now(),
            goal: Goal {
                goal_id: self.goal_id.clone(),
                title: goal_title.to_string(),
                objective: goal_objective.to_string(),
                success_criteria: vec![],
                constraints: vec![],
                parent_goal_title: None,
            },
            iteration: Iteration {
                iteration_id: format!("{}-iter-1", self.goal_id),
                sequence: 1,
                workspace_ref: WorkspaceRef {
                    ref_type: "staging_dir".to_string(),
                    ref_name: self.staging.staging_path().to_string_lossy().to_string(),
                    base_ref: None,
                },
            },
            agent_identity: AgentIdentity {
                agent_id: self.agent_id.clone(),
                agent_type: "fs_connector".to_string(),
                constitution_id: "default".to_string(),
                capability_manifest_hash: "not-yet-computed".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: summary_what.to_string(),
                why: summary_why.to_string(),
                impact: format!("{} file(s) affected", artifacts.len()),
                rollback_plan: "Revert staged changes".to_string(),
                open_questions: vec![],
                alternatives_considered: vec![],
            },
            plan: Plan {
                completed_steps: vec!["Staged filesystem changes".to_string()],
                next_steps: vec!["Await human review".to_string()],
                decision_log: vec![],
            },
            changes: Changes {
                artifacts,
                patch_sets: vec![],
                pending_actions: vec![],
            },
            risk: Risk {
                risk_score: 0,
                findings: vec![],
                policy_decisions: vec![],
            },
            provenance: Provenance {
                inputs: vec![],
                tool_trace_hash: "not-yet-computed".to_string(),
            },
            review_requests: ReviewRequests {
                requested_actions: vec![RequestedAction {
                    action: "apply".to_string(),
                    targets: changesets.iter().map(|cs| cs.target_uri.clone()).collect(),
                }],
                reviewers: vec!["human-reviewer".to_string()],
                required_approvals: 1,
                notes_to_reviewer: None,
            },
            signatures: Signatures {
                package_hash: "not-yet-computed".to_string(),
                agent_signature: "not-yet-computed".to_string(),
                gateway_attestation: None,
            },
            status: PRStatus::PendingReview,
            verification_warnings: vec![],
            validation_log: vec![],
            display_id: None,
            tag: None,
            vcs_status: None,
            parent_draft_id: None,
            pending_approvals: vec![],
            supervisor_review: None,
            ignored_artifacts: vec![],
            baseline_artifacts: vec![],
            agent_decision_log: vec![],
            goal_shortref: None,
            draft_seq: 0,
            plan_phase: None,
        };

        Ok(package)
    }

    /// Apply approved changes to a target directory.
    ///
    /// This copies staged files to the real filesystem. It only works after
    /// the PR package has been approved (status check is the caller's
    /// responsibility for now — the integration test will verify the flow).
    ///
    /// Returns a list of files that were applied.
    pub fn apply(&mut self, target_dir: &Path) -> Result<Vec<String>, FsConnectorError> {
        let staged_files = self.staging.list_files()?;
        let mut applied = Vec::new();

        for relative_path in &staged_files {
            let content = self.staging.read_file(relative_path)?;
            let target_path = target_dir.join(relative_path);

            // Ensure parent directories exist in the target.
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).map_err(|source| FsConnectorError::IoError {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }

            fs::write(&target_path, &content).map_err(|source| FsConnectorError::IoError {
                path: target_path,
                source,
            })?;

            applied.push(relative_path.clone());
        }

        self.log_event(
            AuditAction::Apply,
            &format!("fs://target/{}", target_dir.display()),
        )?;

        Ok(applied)
    }

    /// Get the goal ID.
    pub fn goal_id(&self) -> &str {
        &self.goal_id
    }

    /// Get the staging workspace path.
    pub fn staging_path(&self) -> &Path {
        self.staging.staging_path()
    }

    /// Log an audit event if an audit log is attached.
    fn log_event(&mut self, action: AuditAction, target_uri: &str) -> Result<(), FsConnectorError> {
        if let Some(ref mut log) = self.audit_log {
            let mut event = AuditEvent::new(&self.agent_id, action).with_target(target_uri);
            log.append(&mut event)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use ta_workspace::JsonFileStore;
    use tempfile::tempdir;

    /// Helper to create a connector with a temp staging workspace and store.
    fn setup() -> (
        FsConnector<JsonFileStore>,
        PathBuf, // staging root (for inspection)
        PathBuf, // store dir
    ) {
        let staging_root = tempdir().unwrap().keep();
        let store_dir = tempdir().unwrap().keep();

        let staging = StagingWorkspace::new("goal-1", &staging_root).unwrap();
        let store = JsonFileStore::new(&store_dir).unwrap();

        let connector = FsConnector::new("goal-1", staging, store, "test-agent");

        (connector, staging_root, store_dir)
    }

    #[test]
    fn write_patch_creates_changeset() {
        let (mut connector, _, _) = setup();

        let cs = connector
            .write_patch("hello.txt", b"Hello, world!")
            .unwrap();

        assert_eq!(cs.target_uri, "fs://workspace/hello.txt");
        assert_eq!(cs.kind, ChangeKind::FsPatch);
        assert_eq!(cs.commit_intent, CommitIntent::RequestCommit);
    }

    #[test]
    fn write_patch_stages_file() {
        let (mut connector, _, _) = setup();

        connector.write_patch("hello.txt", b"Hello!").unwrap();

        let content = connector.read_staged("hello.txt").unwrap();
        assert_eq!(content, b"Hello!");
    }

    #[test]
    fn write_patch_new_file_produces_create_diff() {
        let (mut connector, _, _) = setup();

        let cs = connector.write_patch("new.txt", b"new content").unwrap();

        match &cs.diff_content {
            DiffContent::CreateFile { content } => {
                assert_eq!(content, "new content");
            }
            other => panic!("expected CreateFile, got {:?}", other),
        }
    }

    #[test]
    fn write_patch_modified_file_produces_unified_diff() {
        let (mut connector, _, _) = setup();

        // Simulate reading an existing file first (sets up the original snapshot).
        // We can't use read_source without a real source dir, so snapshot manually.
        connector
            .staging
            .snapshot_original("file.txt", b"original line\n".to_vec());

        // Now write a modified version.
        let cs = connector
            .write_patch("file.txt", b"modified line\n")
            .unwrap();

        match &cs.diff_content {
            DiffContent::UnifiedDiff { content } => {
                assert!(content.contains("-original line"));
                assert!(content.contains("+modified line"));
            }
            other => panic!("expected UnifiedDiff, got {:?}", other),
        }
    }

    #[test]
    fn multiple_writes_accumulate_changesets() {
        let (mut connector, _, _) = setup();

        connector.write_patch("a.txt", b"aaa").unwrap();
        connector.write_patch("b.txt", b"bbb").unwrap();
        connector.write_patch("c.txt", b"ccc").unwrap();

        let changesets = connector.list_changesets().unwrap();
        assert_eq!(changesets.len(), 3);
    }

    #[test]
    fn list_staged_files() {
        let (mut connector, _, _) = setup();

        connector.write_patch("x.txt", b"x").unwrap();
        connector.write_patch("sub/y.txt", b"y").unwrap();

        let files = connector.list_staged().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"x.txt".to_string()));
        assert!(files.contains(&"sub/y.txt".to_string()));
    }

    #[test]
    fn build_pr_package_includes_all_changes() {
        let (mut connector, _, _) = setup();

        connector.write_patch("file1.txt", b"content 1").unwrap();
        connector.write_patch("file2.txt", b"content 2").unwrap();

        let pkg = connector
            .build_pr_package(
                "Test Goal",
                "Testing the connector",
                "Created two files",
                "To verify PR package building",
            )
            .unwrap();

        assert_eq!(pkg.goal.goal_id, "goal-1");
        assert_eq!(pkg.changes.artifacts.len(), 2);
        assert_eq!(pkg.status, PRStatus::PendingReview);

        // Verify artifacts have correct URIs.
        let uris: Vec<&str> = pkg
            .changes
            .artifacts
            .iter()
            .map(|a| a.resource_uri.as_str())
            .collect();
        assert!(uris.contains(&"fs://workspace/file1.txt"));
        assert!(uris.contains(&"fs://workspace/file2.txt"));
    }

    #[test]
    fn build_pr_package_fails_with_no_changes() {
        let (connector, _, _) = setup();

        let result = connector.build_pr_package("Goal", "Obj", "What", "Why");
        assert!(matches!(
            result,
            Err(FsConnectorError::NoStagedChanges { .. })
        ));
    }

    #[test]
    fn apply_copies_files_to_target() {
        let (mut connector, _, _) = setup();

        connector.write_patch("hello.txt", b"Hello!").unwrap();
        connector
            .write_patch("sub/nested.txt", b"Nested content")
            .unwrap();

        // Create a target directory and apply.
        let target = tempdir().unwrap();
        let applied = connector.apply(target.path()).unwrap();

        assert_eq!(applied.len(), 2);

        // Verify files exist in target.
        let content1 = fs::read(target.path().join("hello.txt")).unwrap();
        assert_eq!(content1, b"Hello!");

        let content2 = fs::read(target.path().join("sub/nested.txt")).unwrap();
        assert_eq!(content2, b"Nested content");
    }

    #[test]
    fn read_source_snapshots_original() {
        let (mut connector, _, _) = setup();

        // Create a "source" directory with a file.
        let source = tempdir().unwrap();
        fs::write(source.path().join("existing.txt"), b"original content").unwrap();

        // Read from source — this snapshots the original.
        let content = connector
            .read_source(source.path(), "existing.txt")
            .unwrap();
        assert_eq!(content, b"original content");

        // Now write a modified version.
        let cs = connector
            .write_patch("existing.txt", b"modified content")
            .unwrap();

        // The diff should show the change from original to modified.
        match &cs.diff_content {
            DiffContent::UnifiedDiff { content } => {
                assert!(content.contains("-original content"));
                assert!(content.contains("+modified content"));
            }
            other => panic!("expected UnifiedDiff, got {:?}", other),
        }
    }

    #[test]
    fn connector_with_audit_log() {
        let (mut connector, _, _) = setup();

        // Attach an audit log.
        let audit_dir = tempdir().unwrap();
        let audit_path = audit_dir.path().join("audit.jsonl");
        let log = AuditLog::open(&audit_path).unwrap();
        connector = connector.with_audit_log(log);

        // Write a file — should create an audit event.
        connector.write_patch("test.txt", b"data").unwrap();

        // Verify audit log has an entry.
        let events = AuditLog::read_all(&audit_path).unwrap();
        assert!(!events.is_empty());
        assert_eq!(events[0].action, AuditAction::ToolCall);
    }
}
