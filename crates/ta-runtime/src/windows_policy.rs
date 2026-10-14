// windows_policy.rs — platform-neutral Windows isolation policy (v0.17.12.9)
//
// Everything in `sandbox_windows.rs` that talks to Win32 is untestable off
// Windows. The decisions that feed those calls are not, so they live here and
// run on every platform:
//
// 1. Named-pipe names and security descriptors (gateway bridge, egress relay).
// 2. Which host directories a container may read, and the ledger that lets a
//    crash be cleaned up later (`ta doctor --fix`).
// 3. Rejection of Windows path tricks in paths that become ACL grants.
// 4. Job Object limits per role class.
//
// The Win32 side reads these values and applies them; it never decides.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sandbox_launch::RoleClass;

// ── Named pipes ────────────────────────────────────────────────────────────

/// The Win32 named-pipe namespace.
pub const PIPE_NAMESPACE: &str = r"\\.\pipe\";

/// What a per-goal pipe carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipePurpose {
    /// The out-of-sandbox MCP gateway (`ta mcp-bridge` connects here).
    Gateway,
    /// The egress proxy relay (the in-container relay connects here).
    Egress,
}

impl PipePurpose {
    fn tag(self) -> &'static str {
        match self {
            PipePurpose::Gateway => "mcp",
            PipePurpose::Egress => "egress",
        }
    }
}

/// `\\.\pipe\ta-<purpose>-<goal8>-<nonce>`. The pipe namespace is global to the
/// machine, so the name carries an unguessable nonce and the server must create
/// the first instance (see `first_pipe_instance` in the Windows server code):
/// another process cannot pre-create the name to impersonate the gateway.
pub fn pipe_name(purpose: PipePurpose, goal_id: &str, nonce: &str) -> String {
    let goal: String = goal_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    let nonce: String = nonce
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(32)
        .collect();
    format!("{PIPE_NAMESPACE}ta-{}-{goal}-{nonce}", purpose.tag())
}

/// A fresh nonce for [`pipe_name`].
pub fn new_pipe_nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Is `s` a named-pipe path (case-insensitive prefix, either slash style)?
pub fn is_pipe_path(s: &str) -> bool {
    let norm = s.replace('/', "\\").to_ascii_lowercase();
    norm.starts_with(r"\\.\pipe\") && norm.len() > PIPE_NAMESPACE.len()
}

/// `S-1-<authority>-<sub>...`: digits and hyphens only, so a SID string can be
/// spliced into SDDL without any chance of injecting extra ACEs.
pub fn is_valid_sid(s: &str) -> bool {
    let mut parts = s.split('-');
    if parts.next() != Some("S") || parts.next() != Some("1") {
        return false;
    }
    let rest: Vec<&str> = parts.collect();
    !rest.is_empty()
        && rest.len() <= 15
        && rest
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 20 && p.chars().all(|c| c.is_ascii_digit()))
}

/// The security descriptor for a per-goal pipe, as SDDL.
///
/// - Protected DACL (`D:P`): nothing is inherited.
/// - Exactly two ACEs: the pipe's owner (the TA process, full control) and the
///   goal's AppContainer SID (generic read/write). There is no ACE for
///   Everyone, Anonymous, Authenticated Users or the network, so any other
///   process, including other users' and other goals' containers, is denied.
/// - A low mandatory label with no-write-up (`S:(ML;;NW;;;LW)`) so the
///   low-integrity container token is not blocked by the default medium label.
///
/// Remote clients are refused separately when the pipe is created.
pub fn pipe_sddl(container_sid: &str, owner_sid: &str) -> Result<String, String> {
    for (what, sid) in [("container", container_sid), ("owner", owner_sid)] {
        if !is_valid_sid(sid) {
            return Err(format!(
                "the {what} SID {sid:?} is not a well-formed SID (S-1-...), so the named-pipe \
                 security descriptor was not built. This is a TA bug; report it with the goal id."
            ));
        }
    }
    Ok(format!(
        "D:P(A;;GA;;;{owner_sid})(A;;GRGW;;;{container_sid})S:(ML;;NW;;;LW)"
    ))
}

// ── Windows path tricks ────────────────────────────────────────────────────

/// Why a path was refused as an ACL grant target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantPathIssue {
    NotAbsolute,
    NamespacePrefix,
    Unc,
    DriveRoot,
    ParentTraversal(String),
    AlternateStream(String),
    TrailingDotOrSpace(String),
    ShortName(String),
    DeviceName(String),
    /// The grant would expose (is, contains, or sits inside) a protected path.
    Protected(String),
}

impl std::fmt::Display for GrantPathIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantPathIssue::NotAbsolute => f.write_str("the path is not absolute (C:\\...)"),
            GrantPathIssue::NamespacePrefix => {
                f.write_str("the path uses a \\\\?\\ or \\\\.\\ namespace prefix")
            }
            GrantPathIssue::Unc => f.write_str("the path is a UNC network path"),
            GrantPathIssue::DriveRoot => f.write_str("the path is a drive root"),
            GrantPathIssue::ParentTraversal(c) => {
                write!(f, "component {c:?} is a parent traversal")
            }
            GrantPathIssue::AlternateStream(c) => {
                write!(f, "component {c:?} uses NTFS alternate-data-stream syntax")
            }
            GrantPathIssue::TrailingDotOrSpace(c) => write!(
                f,
                "component {c:?} ends in a dot or space, which Windows silently strips"
            ),
            GrantPathIssue::ShortName(c) => {
                write!(f, "component {c:?} looks like an 8.3 short name (NAME~1)")
            }
            GrantPathIssue::DeviceName(c) => {
                write!(f, "component {c:?} is a reserved Windows device name")
            }
            GrantPathIssue::Protected(p) => {
                write!(f, "it overlaps the protected path {p:?}")
            }
        }
    }
}

/// Reserved DOS device names, with or without an extension (`NUL.txt`), and the
/// superscript-digit forms (`COM¹`) that Windows also treats as devices.
/// `ta_workspace::path_safety` carries the same rule for relative paths.
pub fn is_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let digit = |c: char| matches!(c, '1'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}');
            for prefix in ["COM", "LPT"] {
                if let Some(rest) = stem.strip_prefix(prefix) {
                    let mut it = rest.chars();
                    if let (Some(c), None) = (it.next(), it.next()) {
                        if digit(c) {
                            return true;
                        }
                    }
                }
            }
            false
        }
    }
}

/// `NAME~1` or `NAME~12.EXT`: the shape of an NTFS 8.3 short name.
pub fn is_short_name(component: &str) -> bool {
    let base = component
        .trim_start_matches('.')
        .split('.')
        .next()
        .unwrap_or(component);
    match base.rsplit_once('~') {
        Some((stem, digits)) => {
            !stem.is_empty() && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Lowercase, forward-slash-free form used to compare Windows paths.
fn fold_path(p: &str) -> String {
    let mut s = p.replace('/', "\\").to_lowercase();
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

fn is_ancestor_or_equal(ancestor: &str, descendant: &str) -> bool {
    descendant == ancestor
        || descendant
            .strip_prefix(ancestor)
            .is_some_and(|rest| ancestor.ends_with('\\') || rest.starts_with('\\'))
}

/// Validate a host path before it becomes an ACE for a container SID.
///
/// `protected` lists paths the container must never be able to read (the TA
/// state directory, the real project's `.ta/`, the user's credential stores).
/// A grant is refused when it equals, contains, or lies inside one of them,
/// because an inheritable ACE on a parent exposes the protected child.
pub fn validate_grant_path(path: &str, protected: &[PathBuf]) -> Result<(), GrantPathIssue> {
    let norm = path.replace('/', "\\");
    if norm.starts_with(r"\\?\") || norm.starts_with(r"\\.\") {
        return Err(GrantPathIssue::NamespacePrefix);
    }
    if norm.starts_with(r"\\") {
        return Err(GrantPathIssue::Unc);
    }
    let bytes = norm.as_bytes();
    let has_drive =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    if !has_drive {
        return Err(GrantPathIssue::NotAbsolute);
    }
    let rest = &norm[3..];
    if rest.trim_matches('\\').is_empty() {
        return Err(GrantPathIssue::DriveRoot);
    }
    for component in rest.split('\\').filter(|c| !c.is_empty()) {
        if component == "." {
            continue;
        }
        if component == ".." {
            return Err(GrantPathIssue::ParentTraversal(component.to_string()));
        }
        if component.contains(':') {
            return Err(GrantPathIssue::AlternateStream(component.to_string()));
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(GrantPathIssue::TrailingDotOrSpace(component.to_string()));
        }
        if is_device_name(component) {
            return Err(GrantPathIssue::DeviceName(component.to_string()));
        }
        if is_short_name(component) {
            return Err(GrantPathIssue::ShortName(component.to_string()));
        }
    }
    let grant = fold_path(&norm);
    for p in protected {
        let prot = fold_path(&p.to_string_lossy());
        if prot.is_empty() {
            continue;
        }
        if is_ancestor_or_equal(&grant, &prot) || is_ancestor_or_equal(&prot, &grant) {
            return Err(GrantPathIssue::Protected(p.display().to_string()));
        }
    }
    Ok(())
}

// ── Toolchain read grants ──────────────────────────────────────────────────

/// A directory the container is allowed to read, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainGrant {
    pub path: PathBuf,
    pub reason: String,
}

/// What [`plan_toolchain_grants`] decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantPlan {
    pub grants: Vec<ToolchainGrant>,
    /// Candidates that were not granted, with the reason (shown as a warning).
    pub skipped: Vec<(PathBuf, String)>,
}

/// Per-user toolchain directories that agents commonly need and that an
/// AppContainer cannot read by default. Anything under `Program Files` or
/// `Windows` is already readable by `ALL APPLICATION PACKAGES`, so it is not
/// listed. Credential stores (`.ssh`, `.aws`, `.claude`, `.npmrc`...) are never
/// candidates.
pub fn toolchain_candidates(env: &dyn Fn(&str) -> Option<String>) -> Vec<ToolchainGrant> {
    let mut out = Vec::new();
    let mut push = |base: Option<String>, rel: &str, reason: &str| {
        if let Some(b) = base.filter(|b| !b.is_empty()) {
            out.push(ToolchainGrant {
                path: PathBuf::from(format!("{}\\{}", b.trim_end_matches(['\\', '/']), rel)),
                reason: reason.to_string(),
            });
        }
    };
    push(env("USERPROFILE"), ".cargo\\bin", "Rust toolchain binaries");
    push(
        env("USERPROFILE"),
        ".rustup\\toolchains",
        "Rust toolchain installs",
    );
    push(env("APPDATA"), "npm", "global npm packages and shims");
    push(
        env("LOCALAPPDATA"),
        "Programs\\Python",
        "per-user Python installs",
    );
    out
}

/// Decide which directories to grant read+execute on for one goal.
///
/// - `extra_dirs` are directories the launcher needs the container to read for
///   this goal only: the agent runtime's install dir and the folder holding the
///   TA helper binaries.
/// - `owner_allow_read` is the owner's `allow_read` list from the trusted policy.
/// - `exists` lets tests (and a dry run) avoid touching the disk.
///
/// Every path goes through [`validate_grant_path`]; a failing path is skipped
/// with its reason, never silently dropped and never granted.
pub fn plan_toolchain_grants(
    env: &dyn Fn(&str) -> Option<String>,
    extra_dirs: &[ToolchainGrant],
    owner_allow_read: &[String],
    protected: &[PathBuf],
    exists: &dyn Fn(&Path) -> bool,
) -> GrantPlan {
    let mut plan = GrantPlan::default();
    let mut seen = std::collections::HashSet::new();
    let mut candidates = toolchain_candidates(env);
    candidates.extend(extra_dirs.iter().cloned());
    candidates.extend(owner_allow_read.iter().map(|p| ToolchainGrant {
        path: PathBuf::from(p),
        reason: "allow_read in the owner's sandbox policy".to_string(),
    }));
    for c in candidates {
        let text = c.path.to_string_lossy().to_string();
        if !seen.insert(fold_path(&text)) {
            continue;
        }
        if let Err(issue) = validate_grant_path(&text, protected) {
            plan.skipped.push((c.path, issue.to_string()));
            continue;
        }
        if !exists(&c.path) {
            // Not installed on this machine: nothing to grant, nothing to report.
            continue;
        }
        plan.grants.push(c);
    }
    plan
}

// ── Grant ledger and the stale-ACE sweep ───────────────────────────────────

/// One ACE TA added for a container SID. Written before the ACE is applied so a
/// crash between the two leaves a record to sweep, never an unrecorded ACE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRecord {
    pub goal_id: String,
    pub container_name: String,
    pub path: String,
    pub granted_at: String,
    /// PID of the `ta` process that applied the grant (liveness check for the sweep).
    pub owner_pid: u32,
}

/// Timestamp for ledger records.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Append-only JSON-lines record of granted ACEs, kept in the TA state dir.
#[derive(Debug, Clone)]
pub struct GrantLedger {
    file: PathBuf,
}

pub const LEDGER_FILE: &str = "windows-acl-grants.jsonl";

impl GrantLedger {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            file: dir.join(LEDGER_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.file
    }

    pub fn record(&self, rec: &GrantRecord) -> Result<(), String> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                format!(
                    "cannot create {} for the ACL grant ledger: {e}. Check permissions on the \
                     TA state directory.",
                    dir.display()
                )
            })?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)
            .map_err(|e| {
                format!(
                    "cannot open the ACL grant ledger {}: {e}",
                    self.file.display()
                )
            })?;
        let line = serde_json::to_string(rec).map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| {
            format!(
                "cannot write the ACL grant ledger {}: {e}",
                self.file.display()
            )
        })
    }

    /// Every readable record. Unreadable lines are skipped (and counted by the
    /// caller through [`GrantLedger::list_with_errors`] when it needs to warn).
    pub fn list(&self) -> Vec<GrantRecord> {
        self.list_with_errors().0
    }

    pub fn list_with_errors(&self) -> (Vec<GrantRecord>, usize) {
        let Ok(text) = std::fs::read_to_string(&self.file) else {
            return (Vec::new(), 0);
        };
        let mut ok = Vec::new();
        let mut bad = 0;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<GrantRecord>(line) {
                Ok(r) => ok.push(r),
                Err(_) => bad += 1,
            }
        }
        (ok, bad)
    }

    /// Rewrite the ledger without the records `keep` rejects.
    fn retain(&self, keep: impl Fn(&GrantRecord) -> bool) -> Result<(), String> {
        let kept: Vec<GrantRecord> = self.list().into_iter().filter(|r| keep(r)).collect();
        if kept.is_empty() {
            return match std::fs::remove_file(&self.file) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!(
                    "cannot remove the ACL grant ledger {}: {e}",
                    self.file.display()
                )),
            };
        }
        let tmp = self.file.with_extension("jsonl.tmp");
        let mut body = String::new();
        for r in &kept {
            body.push_str(&serde_json::to_string(r).map_err(|e| e.to_string())?);
            body.push('\n');
        }
        std::fs::write(&tmp, body)
            .and_then(|_| std::fs::rename(&tmp, &self.file))
            .map_err(|e| {
                format!(
                    "cannot rewrite the ACL grant ledger {}: {e}",
                    self.file.display()
                )
            })
    }

    /// Drop every record for `goal_id` (after its ACEs were removed).
    pub fn forget_goal(&self, goal_id: &str) -> Result<(), String> {
        self.retain(|r| r.goal_id != goal_id)
    }

    /// Drop one record.
    pub fn forget(&self, rec: &GrantRecord) -> Result<(), String> {
        self.retain(|r| r != rec)
    }

    pub fn for_goal(&self, goal_id: &str) -> Vec<GrantRecord> {
        self.list()
            .into_iter()
            .filter(|r| r.goal_id == goal_id)
            .collect()
    }
}

/// What a sweep did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    pub revoked: Vec<GrantRecord>,
    /// Grants whose owner is still running; left alone.
    pub live: Vec<GrantRecord>,
    pub failed: Vec<(GrantRecord, String)>,
}

/// Remove the ACEs of goals whose `ta` process is gone. `owner_alive` and
/// `revoke` are injected so the logic runs (and is tested) on any platform.
/// A record is forgotten only after its ACE was removed; a failed revoke stays
/// in the ledger for the next sweep.
pub fn sweep_stale_grants(
    ledger: &GrantLedger,
    owner_alive: &dyn Fn(u32) -> bool,
    revoke: &dyn Fn(&GrantRecord) -> Result<(), String>,
) -> SweepReport {
    let mut report = SweepReport::default();
    for rec in ledger.list() {
        if owner_alive(rec.owner_pid) {
            report.live.push(rec);
            continue;
        }
        match revoke(&rec) {
            Ok(()) => {
                if let Err(e) = ledger.forget(&rec) {
                    report.failed.push((rec, e));
                } else {
                    report.revoked.push(rec);
                }
            }
            Err(e) => report.failed.push((rec, e)),
        }
    }
    report
}

// ── Job Object limits ──────────────────────────────────────────────────────

pub const JOB_LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
pub const JOB_LIMIT_JOB_MEMORY: u32 = 0x0000_0200;
pub const JOB_LIMIT_DIE_ON_UNHANDLED_EXCEPTION: u32 = 0x0000_0400;
pub const JOB_LIMIT_BREAKAWAY_OK: u32 = 0x0000_0800;
pub const JOB_LIMIT_SILENT_BREAKAWAY_OK: u32 = 0x0000_1000;
pub const JOB_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

pub const JOB_UILIMIT_HANDLES: u32 = 0x0001;
pub const JOB_UILIMIT_READCLIPBOARD: u32 = 0x0002;
pub const JOB_UILIMIT_WRITECLIPBOARD: u32 = 0x0004;
pub const JOB_UILIMIT_SYSTEMPARAMETERS: u32 = 0x0008;
pub const JOB_UILIMIT_DISPLAYSETTINGS: u32 = 0x0010;
pub const JOB_UILIMIT_GLOBALATOMS: u32 = 0x0020;
pub const JOB_UILIMIT_DESKTOP: u32 = 0x0040;
pub const JOB_UILIMIT_EXITWINDOWS: u32 = 0x0080;

/// Limits applied to the goal's Job Object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobLimits {
    /// Most processes alive in the job at once (fork-bomb guard).
    pub active_process_limit: Option<u32>,
    /// Memory the whole job may commit, in bytes.
    pub job_memory_bytes: Option<usize>,
    /// Deny clipboard, desktop switching, global hooks, system parameters.
    pub ui_restrictions: bool,
}

impl JobLimits {
    /// Defaults by role class. Workers run compilers and test runners, which
    /// fan out widely, so their process cap is generous and memory is not capped
    /// (a build that needs 8 GB must not be killed by TA). The strict classes get
    /// small caps because they have no reason to run a build.
    pub fn for_role(role: RoleClass) -> Self {
        match role {
            RoleClass::Chat => Self {
                active_process_limit: Some(16),
                job_memory_bytes: Some(2 << 30),
                ui_restrictions: true,
            },
            RoleClass::Reviewer => Self {
                active_process_limit: Some(64),
                job_memory_bytes: Some(4 << 30),
                ui_restrictions: true,
            },
            RoleClass::Worker => Self {
                active_process_limit: Some(1024),
                job_memory_bytes: None,
                ui_restrictions: true,
            },
            RoleClass::Plugin => Self {
                active_process_limit: Some(64),
                job_memory_bytes: Some(2 << 30),
                ui_restrictions: true,
            },
        }
    }

    /// `LimitFlags` for `JOBOBJECT_BASIC_LIMIT_INFORMATION`. Always kills the
    /// tree on close and never allows breakaway: a child cannot opt out of the
    /// job with `CREATE_BREAKAWAY_FROM_JOB`.
    pub fn limit_flags(&self) -> u32 {
        let mut f = JOB_LIMIT_KILL_ON_JOB_CLOSE | JOB_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        if self.active_process_limit.is_some() {
            f |= JOB_LIMIT_ACTIVE_PROCESS;
        }
        if self.job_memory_bytes.is_some() {
            f |= JOB_LIMIT_JOB_MEMORY;
        }
        f
    }

    /// `UIRestrictionsClass` for `JOBOBJECT_BASIC_UI_RESTRICTIONS` (0 = none).
    pub fn ui_flags(&self) -> u32 {
        if !self.ui_restrictions {
            return 0;
        }
        JOB_UILIMIT_HANDLES
            | JOB_UILIMIT_READCLIPBOARD
            | JOB_UILIMIT_WRITECLIPBOARD
            | JOB_UILIMIT_SYSTEMPARAMETERS
            | JOB_UILIMIT_DISPLAYSETTINGS
            | JOB_UILIMIT_GLOBALATOMS
            | JOB_UILIMIT_DESKTOP
            | JOB_UILIMIT_EXITWINDOWS
    }

    /// One line for the launch banner.
    pub fn describe(&self) -> String {
        format!(
            "job limits: {} processes, {} memory, UI restrictions {}",
            self.active_process_limit
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unlimited".into()),
            self.job_memory_bytes
                .map(|b| format!("{} MiB", b >> 20))
                .unwrap_or_else(|| "unlimited".into()),
            if self.ui_restrictions { "on" } else { "off" }
        )
    }
}

impl Default for JobLimits {
    /// Used by callers with no role (tests, the plain Job Object guard).
    fn default() -> Self {
        Self {
            active_process_limit: None,
            job_memory_bytes: None,
            ui_restrictions: false,
        }
    }
}

// ── Launch inputs carried on SandboxPolicy ─────────────────────────────────

/// AppContainer name for a goal: `ta-` plus the first 8 characters of the goal
/// id. Deterministic, so the container SID (which derives from the name alone)
/// is known before the profile exists, and the gateway pipe can be locked to it.
pub fn container_name(goal_id: &str) -> String {
    format!("ta-{}", goal_id.chars().take(8).collect::<String>())
}

/// Windows inputs to a launch that have no equivalent on other platforms.
#[derive(Debug, Clone, Default)]
pub struct WindowsLaunch {
    /// Named pipe the in-container relay connects to; the host end forwards to
    /// the egress proxy. AppContainers cannot reach loopback, so this pipe is
    /// the only way out (no loopback exemption is configured).
    pub egress_pipe: Option<String>,
    /// Job Object limits for this goal's role class.
    pub job_limits: Option<JobLimits>,
    /// TA state directory (holds the ACL grant ledger).
    pub state_dir: Option<PathBuf>,
    /// Extra directories the container must read: the agent runtime's install
    /// directory and the folder holding the TA helper binaries.
    pub extra_read_dirs: Vec<PathBuf>,
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_name_is_ta_plus_eight_chars() {
        assert_eq!(
            container_name("9c4ee279-489e-4ba5-b2cb-2694e91cdcd3"),
            "ta-9c4ee279"
        );
        assert_eq!(container_name("abc"), "ta-abc");
    }

    const CONTAINER: &str = "S-1-15-2-1111-2222-3333-4444-5555-6666-7777";
    const OWNER: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn pipe_name_has_namespace_purpose_goal_and_nonce() {
        let n = pipe_name(PipePurpose::Gateway, "9c4ee279-489e-4ba5", "abc123");
        assert_eq!(n, r"\\.\pipe\ta-mcp-9c4ee279-abc123");
        assert!(is_pipe_path(&n));
        assert!(pipe_name(PipePurpose::Egress, "g", "n").contains("ta-egress-"));
    }

    #[test]
    fn pipe_name_strips_characters_that_could_escape_the_namespace() {
        let n = pipe_name(PipePurpose::Gateway, r"..\..\x", r"a\b/c");
        assert!(n.starts_with(PIPE_NAMESPACE));
        assert!(!n[PIPE_NAMESPACE.len()..].contains(['\\', '/']));
    }

    #[test]
    fn nonces_are_unique() {
        assert_ne!(new_pipe_nonce(), new_pipe_nonce());
        assert_eq!(new_pipe_nonce().len(), 32);
    }

    #[test]
    fn pipe_paths_are_recognised_in_either_slash_style_and_case() {
        assert!(is_pipe_path(r"\\.\pipe\x"));
        assert!(is_pipe_path("//./PIPE/x"));
        assert!(!is_pipe_path(r"\\.\pipe\"));
        assert!(!is_pipe_path(r"C:\temp\mcp.sock"));
        assert!(!is_pipe_path("/tmp/mcp.sock"));
    }

    #[test]
    fn sid_validation_accepts_real_sids_and_rejects_sddl_injection() {
        assert!(is_valid_sid(CONTAINER));
        assert!(is_valid_sid(OWNER));
        assert!(is_valid_sid("S-1-15-3-1"));
        assert!(!is_valid_sid("S-1-5-21)(A;;GA;;;WD"));
        assert!(!is_valid_sid("WD"));
        assert!(!is_valid_sid("S-1-"));
        assert!(!is_valid_sid(""));
        assert!(!is_valid_sid("S-2-5-1"));
        assert!(!is_valid_sid("S-1-5--1"));
    }

    #[test]
    fn pipe_sddl_grants_only_owner_and_container() {
        let sddl = pipe_sddl(CONTAINER, OWNER).unwrap();
        assert!(sddl.starts_with("D:P"), "DACL must be protected: {sddl}");
        assert_eq!(
            sddl.matches("(A;").count(),
            2,
            "exactly two allow ACEs: {sddl}"
        );
        assert!(sddl.contains(&format!("(A;;GA;;;{OWNER})")));
        assert!(sddl.contains(&format!("(A;;GRGW;;;{CONTAINER})")));
        for broad in [";;;WD)", ";;;AU)", ";;;AN)", ";;;BU)", ";;;NU)", ";;;AC)"] {
            assert!(!sddl.contains(broad), "{broad} must not be granted: {sddl}");
        }
        assert!(sddl.ends_with("S:(ML;;NW;;;LW)"));
    }

    #[test]
    fn pipe_sddl_refuses_malformed_sids() {
        let err = pipe_sddl("S-1-5-1)(A;;GA;;;WD", OWNER).unwrap_err();
        assert!(err.contains("container"), "{err}");
        let err = pipe_sddl(CONTAINER, "").unwrap_err();
        assert!(err.contains("owner"), "{err}");
    }

    #[test]
    fn device_and_short_names() {
        for d in [
            "CON",
            "con",
            "NUL.txt",
            "Aux.tar.gz",
            "COM1",
            "lpt9",
            "COM¹",
            "CONOUT$",
        ] {
            assert!(is_device_name(d), "{d}");
        }
        for ok in [
            "console", "COM10", "COM0", "nullable", "comma", "LPT", "a.con",
        ] {
            assert!(!is_device_name(ok), "{ok}");
        }
        for s in ["PROGRA~1", "GIT~1", "claude~12.txt", ".ta~1"] {
            assert!(is_short_name(s), "{s}");
        }
        for ok in ["program files", "a~b", "~1", "foo~", "x.y~1"] {
            assert!(!is_short_name(ok), "{ok}");
        }
    }

    #[test]
    fn grant_paths_reject_every_windows_trick() {
        let none: Vec<PathBuf> = vec![];
        assert_eq!(validate_grant_path(r"C:\tools\node", &none), Ok(()));
        assert_eq!(validate_grant_path("C:/tools/node", &none), Ok(()));
        let cases: Vec<(&str, GrantPathIssue)> = vec![
            ("tools\\node", GrantPathIssue::NotAbsolute),
            (r"\\?\C:\tools", GrantPathIssue::NamespacePrefix),
            (r"\\.\pipe\x", GrantPathIssue::NamespacePrefix),
            (r"\\server\share\x", GrantPathIssue::Unc),
            (r"C:\", GrantPathIssue::DriveRoot),
            (
                r"C:\tools\..\Users",
                GrantPathIssue::ParentTraversal("..".into()),
            ),
            (
                r"C:\tools\a.txt:hidden",
                GrantPathIssue::AlternateStream("a.txt:hidden".into()),
            ),
            (
                r"C:\tools\x::$DATA",
                GrantPathIssue::AlternateStream("x::$DATA".into()),
            ),
            (
                r"C:\tools\node. ",
                GrantPathIssue::TrailingDotOrSpace("node. ".into()),
            ),
            (
                r"C:\tools\node.",
                GrantPathIssue::TrailingDotOrSpace("node.".into()),
            ),
            (
                r"C:\PROGRA~1\x",
                GrantPathIssue::ShortName("PROGRA~1".into()),
            ),
            (r"C:\tools\NUL", GrantPathIssue::DeviceName("NUL".into())),
            (
                r"C:\tools\com3.txt",
                GrantPathIssue::DeviceName("com3.txt".into()),
            ),
        ];
        for (path, want) in cases {
            assert_eq!(validate_grant_path(path, &none), Err(want), "{path}");
        }
    }

    #[test]
    fn grants_may_not_expose_protected_paths() {
        let protected = vec![
            PathBuf::from(r"C:\Users\me\AppData\Local\TrustedAutonomy"),
            PathBuf::from(r"C:\work\proj\.ta"),
        ];
        // Inside, equal, and an ancestor (inheritable ACE would expose the child).
        for bad in [
            r"C:\Users\me\AppData\Local\TrustedAutonomy\projects",
            r"C:\users\ME\appdata\local\trustedautonomy",
            r"C:\Users\me\AppData\Local",
            r"C:\Users\me",
            r"C:\work\proj",
            r"C:\work\proj\.ta\staging",
        ] {
            assert!(
                matches!(
                    validate_grant_path(bad, &protected),
                    Err(GrantPathIssue::Protected(_))
                ),
                "{bad}"
            );
        }
        // A sibling that merely shares a prefix is fine.
        assert_eq!(validate_grant_path(r"C:\work\proj2", &protected), Ok(()));
        assert_eq!(
            validate_grant_path(r"C:\Users\me\AppData\Roaming\npm", &protected),
            Ok(())
        );
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn toolchain_plan_grants_installed_dirs_and_skips_unsafe_ones() {
        let env = env_of(&[
            ("USERPROFILE", r"C:\Users\me"),
            ("APPDATA", r"C:\Users\me\AppData\Roaming"),
            ("LOCALAPPDATA", r"C:\Users\me\AppData\Local"),
        ]);
        let protected = vec![PathBuf::from(r"C:\Users\me\AppData\Local\TrustedAutonomy")];
        let exists = |p: &Path| {
            let s = p.to_string_lossy();
            s.ends_with(".cargo\\bin") || s.ends_with("npm") || s.contains("ta-install")
        };
        let extra = vec![ToolchainGrant {
            path: PathBuf::from(r"C:\Users\me\ta-install"),
            reason: "TA helper binaries".into(),
        }];
        let owner = vec![
            r"C:\data\shared".to_string(),
            r"C:\tools\..\Users".to_string(),
        ];
        let plan = plan_toolchain_grants(&env, &extra, &owner, &protected, &|p| {
            exists(p) || p.to_string_lossy().contains("shared")
        });
        let granted: Vec<String> = plan
            .grants
            .iter()
            .map(|g| g.path.display().to_string())
            .collect();
        assert!(
            granted.contains(&r"C:\Users\me\.cargo\bin".to_string()),
            "{granted:?}"
        );
        assert!(granted.contains(&r"C:\Users\me\AppData\Roaming\npm".to_string()));
        assert!(granted.contains(&r"C:\Users\me\ta-install".to_string()));
        assert!(granted.contains(&r"C:\data\shared".to_string()));
        // Not installed: neither granted nor reported.
        assert!(!granted.iter().any(|g| g.contains(".rustup")));
        // Unsafe owner entry: skipped WITH a reason.
        assert_eq!(plan.skipped.len(), 1);
        assert!(
            plan.skipped[0].1.contains("parent traversal"),
            "{:?}",
            plan.skipped
        );
    }

    #[test]
    fn toolchain_plan_never_offers_credential_stores() {
        let env = env_of(&[
            ("USERPROFILE", r"C:\Users\me"),
            ("APPDATA", r"C:\Users\me\AppData\Roaming"),
        ]);
        for c in toolchain_candidates(&env) {
            let s = c.path.to_string_lossy().to_lowercase();
            for secret in [".ssh", ".aws", ".claude", ".npmrc", ".gnupg", "credentials"] {
                assert!(!s.contains(secret), "{s} must not be a toolchain grant");
            }
        }
    }

    #[test]
    fn toolchain_plan_dedups_case_insensitively() {
        let env = env_of(&[("USERPROFILE", r"C:\Users\me")]);
        let owner = vec![r"c:\users\ME\.cargo\bin".to_string()];
        let plan = plan_toolchain_grants(&env, &[], &owner, &[], &|_| true);
        let n = plan
            .grants
            .iter()
            .filter(|g| {
                g.path
                    .to_string_lossy()
                    .to_lowercase()
                    .ends_with(".cargo\\bin")
            })
            .count();
        assert_eq!(n, 1);
    }

    fn rec(goal: &str, path: &str, pid: u32) -> GrantRecord {
        GrantRecord {
            goal_id: goal.into(),
            container_name: format!("ta-{goal}"),
            path: path.into(),
            granted_at: "2026-10-14T00:00:00Z".into(),
            owner_pid: pid,
        }
    }

    #[test]
    fn ledger_records_lists_and_forgets_by_goal() {
        let dir = tempfile::tempdir().unwrap();
        let l = GrantLedger::in_dir(dir.path());
        assert!(l.list().is_empty());
        l.record(&rec("a", r"C:\x", 1)).unwrap();
        l.record(&rec("a", r"C:\y", 1)).unwrap();
        l.record(&rec("b", r"C:\z", 2)).unwrap();
        assert_eq!(l.list().len(), 3);
        assert_eq!(l.for_goal("a").len(), 2);
        l.forget_goal("a").unwrap();
        assert_eq!(l.list(), vec![rec("b", r"C:\z", 2)]);
        l.forget_goal("b").unwrap();
        assert!(!l.path().exists(), "an empty ledger is removed");
    }

    #[test]
    fn ledger_skips_and_counts_corrupt_lines() {
        let dir = tempfile::tempdir().unwrap();
        let l = GrantLedger::in_dir(dir.path());
        l.record(&rec("a", r"C:\x", 1)).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(l.path())
            .unwrap()
            .write_all(b"{not json\n")
            .unwrap();
        let (ok, bad) = l.list_with_errors();
        assert_eq!((ok.len(), bad), (1, 1));
    }

    #[test]
    fn sweep_revokes_dead_owners_and_keeps_live_ones_and_failures() {
        let dir = tempfile::tempdir().unwrap();
        let l = GrantLedger::in_dir(dir.path());
        l.record(&rec("dead", r"C:\a", 10)).unwrap();
        l.record(&rec("dead", r"C:\bad", 10)).unwrap();
        l.record(&rec("live", r"C:\b", 20)).unwrap();
        let revoked = std::cell::RefCell::new(Vec::new());
        let report = sweep_stale_grants(&l, &|pid| pid == 20, &|r| {
            if r.path.ends_with("bad") {
                return Err("access denied".into());
            }
            revoked.borrow_mut().push(r.path.clone());
            Ok(())
        });
        assert_eq!(report.revoked.len(), 1);
        assert_eq!(report.live.len(), 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(*revoked.borrow(), vec![r"C:\a".to_string()]);
        // The failed record stays for the next sweep; the revoked one is gone.
        let left: Vec<String> = l.list().into_iter().map(|r| r.path).collect();
        assert_eq!(left, vec![r"C:\bad".to_string(), r"C:\b".to_string()]);
    }

    #[test]
    fn job_flags_always_kill_on_close_and_never_allow_breakaway() {
        for role in [
            RoleClass::Chat,
            RoleClass::Reviewer,
            RoleClass::Worker,
            RoleClass::Plugin,
        ] {
            let f = JobLimits::for_role(role).limit_flags();
            assert_ne!(f & JOB_LIMIT_KILL_ON_JOB_CLOSE, 0, "{role:?}");
            assert_eq!(f & JOB_LIMIT_BREAKAWAY_OK, 0, "{role:?}");
            assert_eq!(f & JOB_LIMIT_SILENT_BREAKAWAY_OK, 0, "{role:?}");
            assert_ne!(f & JOB_LIMIT_ACTIVE_PROCESS, 0, "{role:?}");
        }
    }

    #[test]
    fn job_limits_follow_role_strictness() {
        let chat = JobLimits::for_role(RoleClass::Chat);
        let worker = JobLimits::for_role(RoleClass::Worker);
        assert!(chat.active_process_limit < worker.active_process_limit);
        assert!(chat.job_memory_bytes.is_some());
        assert!(worker.job_memory_bytes.is_none());
        assert_eq!(worker.limit_flags() & JOB_LIMIT_JOB_MEMORY, 0);
        assert_ne!(chat.limit_flags() & JOB_LIMIT_JOB_MEMORY, 0);
    }

    #[test]
    fn ui_restrictions_cover_clipboard_desktop_and_hooks() {
        let f = JobLimits::for_role(RoleClass::Worker).ui_flags();
        for bit in [
            JOB_UILIMIT_HANDLES,
            JOB_UILIMIT_READCLIPBOARD,
            JOB_UILIMIT_WRITECLIPBOARD,
            JOB_UILIMIT_DESKTOP,
            JOB_UILIMIT_SYSTEMPARAMETERS,
        ] {
            assert_ne!(f & bit, 0);
        }
        assert_eq!(JobLimits::default().ui_flags(), 0);
        assert_eq!(
            JobLimits::default().limit_flags(),
            JOB_LIMIT_KILL_ON_JOB_CLOSE | JOB_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
        );
    }

    #[test]
    fn describe_names_every_limit() {
        let s = JobLimits::for_role(RoleClass::Chat).describe();
        assert!(s.contains("16 processes") && s.contains("2048 MiB"), "{s}");
        assert!(JobLimits::for_role(RoleClass::Worker)
            .describe()
            .contains("unlimited"));
    }
}
