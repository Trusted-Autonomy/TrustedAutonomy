// read_policy.rs — the read allow/deny policy of a chat-mode Chief of Staff.
//
// One compiled policy, enforced in every layer:
//
// - the gateway read tools (`ta_fs_read`, `ta_fs_list`) call [`ReadPolicy::decide`];
// - the biscuit read grant carries the compiled rules as scopes
//   ([`crate::ChatReadAccess::to_scopes`]), so the gateway rebuilds this policy from
//   the verified token alone and a later edit of `.ta/workflow.toml` cannot widen a
//   running session;
// - the OS sandbox profile is built from [`ReadPolicy::sandbox_view`] (Seatbelt allow
//   subpaths plus explicit denies, bubblewrap read-only binds and tmpfs masks).
//
// ## Semantics (documented for users in docs/USAGE.md)
//
// - Entries are an absolute path, `~/path`, a project name registered in
//   `office.yaml`, or a glob (relative paths and globs are relative to the project root).
// - The project root is always readable. An empty allow list means "project root only".
// - Deny beats allow. A deny entry inside an allowed root hides just that subtree.
// - Every path is canonicalized before it is compared: a symlink that leaves an allowed
//   root resolves outside it and is refused; `..` is refused outright.
// - The built-in hard deny list (the project's `.ta` and `.git`, the TA state directory,
//   `HOME_PROTECTED` credential stores and browser profiles, TA/VCS infrastructure
//   directories below any allowed root, and the engine's secrets patterns) can never be
//   overridden by user config.
// - Nothing here can express a write: there is no write verb in this module.

use std::path::{Component, Path, PathBuf};

use glob::{MatchOptions, Pattern};

/// Credential stores, browser profiles and shell history under `$HOME` that no agent
/// needs. This is the single definition: the sandbox profile denies them for every
/// class of launch and the chat read policy refuses them as part of its built-in hard
/// deny list. On macOS the sandbox still reads by denylist outside a chat session's
/// confined area, so reads are not described as confined there.
pub const HOME_PROTECTED: &[&str] = &[
    // TA's own per-user human state: the `ta login` credential (`credential`,
    // 0600, present when file custody holds it), its digest and the custody
    // choice. An agent that could read `credential` could present it as the
    // human (docs/security/KEY-CUSTODY.md).
    ".config/ta/human",
    // Keys, tokens and cloud credentials.
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".config/gh",
    ".config/git/credentials",
    ".git-credentials",
    ".docker",
    ".kube",
    ".netrc",
    ".npmrc",
    ".yarnrc",
    ".pypirc",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".terraform.d/credentials.tfrc.json",
    ".local/share/keyrings",
    ".password-store",
    // macOS stores.
    "Library/Keychains",
    "Library/Cookies",
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Firefox",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Microsoft Edge",
    "Library/Application Support/Arc",
    "Library/Application Support/1Password",
    "Library/Group Containers/group.com.apple.notes",
    // Linux browser profiles and shell history.
    ".config/google-chrome",
    ".config/chromium",
    ".mozilla",
    ".bash_history",
    ".zsh_history",
    ".python_history",
    ".node_repl_history",
];

/// Prefix of the policy-engine resource a chat read is checked against
/// (`fs://read/<absolute path>`).
pub const READ_RESOURCE_PREFIX: &str = "fs://read/";
/// The one `fs.read` grant a chat manifest carries when the session may read at all.
/// Which paths are readable is decided by [`ReadPolicy`], rebuilt from the biscuit.
pub const READ_RESOURCE_SCOPE: &str = "fs://read/**";

/// Maximum number of paths one glob entry may expand to for the OS sandbox view.
const MAX_GLOB_EXPANSION: usize = 2000;

pub fn filesystem_is_case_insensitive() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

/// Lexical normalization: drops `.`, resolves `..` without touching the disk.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// True when `path` equals or lies inside `base`, comparing components
/// (never string prefixes, so `.tax` is not inside `.ta`).
pub fn path_within(path: &Path, base: &Path, case_insensitive: bool) -> bool {
    let p = normalize(path);
    let b = normalize(base);
    let mut pi = p.components();
    for bc in b.components() {
        match pi.next() {
            Some(pc) => {
                let same = if case_insensitive {
                    pc.as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&bc.as_os_str().to_string_lossy())
                } else {
                    pc == bc
                };
                if !same {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

/// The policy-engine resource URI for a chat read of the absolute `path`.
pub fn read_resource(path: &Path) -> String {
    format!(
        "{READ_RESOURCE_PREFIX}{}",
        to_slash(path).trim_start_matches('/')
    )
}

/// `path` with forward slashes and without a Windows verbatim prefix.
fn to_slash(path: &Path) -> String {
    let s = path.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.replace('\\', "/")
}

/// Display form of a path in messages (no verbatim prefix).
pub fn display_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// Canonicalize `path`; when it does not exist, canonicalize its deepest existing
/// ancestor and append the missing tail (so a symlinked prefix is still resolved).
pub fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let mut missing = Vec::new();
    let mut cur = path;
    while let Some(parent) = cur.parent() {
        if let Some(name) = cur.file_name() {
            missing.push(name.to_os_string());
        }
        if let Ok(real) = std::fs::canonicalize(parent) {
            let mut out = real;
            for part in missing.iter().rev() {
                out.push(part);
            }
            return out;
        }
        cur = parent;
    }
    path.to_path_buf()
}

fn has_glob_chars(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

// ── Errors ─────────────────────────────────────────────────────────────────

/// A read-policy entry that cannot be used. The message says what was wrong, which
/// config key to change and how; a typo in an explicit widening of access must be
/// loud, never dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPolicyError(pub String);

impl std::fmt::Display for ReadPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReadPolicyError {}

// ── Environment ────────────────────────────────────────────────────────────

/// Facts entry resolution and the hard deny list are computed against.
#[derive(Debug, Clone)]
pub struct ReadEnv {
    /// The source project root (need not be canonical).
    pub project_root: PathBuf,
    /// `$HOME` / `%USERPROFILE%` of the user.
    pub home: Option<PathBuf>,
    /// The TA state directory for the project, when one exists.
    pub state_dir: Option<PathBuf>,
    /// The project registry file (`office.yaml`, or `$TA_OFFICE_CONFIG`).
    pub office_yaml: PathBuf,
}

impl ReadEnv {
    /// The environment of `project_root`: `$HOME`, `$TA_OFFICE_CONFIG` or
    /// `<root>/office.yaml`. The state directory is supplied by the caller
    /// ([`ReadEnv::with_state_dir`]) because locating it needs the credential crates.
    pub fn for_project(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            home: home_dir(),
            state_dir: None,
            office_yaml: std::env::var_os("TA_OFFICE_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| project_root.join("office.yaml")),
        }
    }

    pub fn with_state_dir(mut self, state_dir: Option<PathBuf>) -> Self {
        self.state_dir = state_dir;
        self
    }
}

// ── Rules ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// A canonical absolute file or directory; covers its whole subtree.
    Path,
    /// An absolute glob (literal prefix canonicalized); covers every path it matches
    /// and the subtree below each match.
    Glob,
}

/// One resolved allow or deny entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRule {
    pub kind: RuleKind,
    /// Canonical absolute path ([`RuleKind::Path`]) or absolute glob ([`RuleKind::Glob`]).
    pub pattern: String,
    /// The entry exactly as the owner wrote it, for explanations.
    pub source: String,
    /// The name an agent passes as `project` to read relative to this root.
    pub label: Option<String>,
    /// The entry named a project registered in `office.yaml`: its Wayfinder
    /// configuration may be read through the `ta_wayfinder_*` tools.
    pub registered: bool,
}

/// A label usable as the `project` argument: letters, digits, `.`, `_`, `-`.
/// Labels become part of a comma-joined biscuit scope list and of messages, so they
/// are restricted to characters that can never be glob syntax or separators.
pub fn is_valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 64
        && label != "."
        && label != ".."
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl ReadRule {
    fn matches(&self, canonical: &Path, ci: bool) -> bool {
        match self.kind {
            RuleKind::Path => path_within(canonical, Path::new(&self.pattern), ci),
            RuleKind::Glob => self.glob_anchor(canonical, ci).is_some(),
        }
    }

    /// For a glob: the shallowest ancestor of `canonical` (or `canonical` itself) that
    /// the pattern matches.
    fn glob_anchor(&self, canonical: &Path, ci: bool) -> Option<PathBuf> {
        let pattern = Pattern::new(&self.pattern).ok()?;
        let opts = MatchOptions {
            case_sensitive: !ci,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        let mut chain: Vec<&Path> = canonical.ancestors().collect();
        chain.reverse();
        chain.into_iter().find_map(|a| {
            let candidate = glob_candidate(a, &self.pattern);
            pattern
                .matches_with(&candidate, opts)
                .then(|| a.to_path_buf())
        })
    }

    /// The directory a match of this rule is rooted at, given the matched path: for a
    /// path rule its own root; for a glob the matched ancestor.
    fn root_for(&self, canonical: &Path, ci: bool) -> Option<PathBuf> {
        match self.kind {
            RuleKind::Path => Some(PathBuf::from(&self.pattern)),
            RuleKind::Glob => self.glob_anchor(canonical, ci),
        }
    }
}

/// The string a glob is matched against. A pattern anchored "anywhere" (`/**/x`) is
/// matched without a Windows drive prefix so it works on every host.
fn glob_candidate(path: &Path, pattern: &str) -> String {
    let s = to_slash(path);
    if pattern.starts_with("/**/") {
        let b = s.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return s[2..].to_string();
        }
    }
    s
}

/// Percent-encode everything outside `[A-Za-z0-9/._~-]` so a value can sit inside a
/// `:`- and `,`-separated biscuit scope and inside a datalog string literal.
pub fn scope_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'~' | b'-') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Inverse of [`scope_encode`]; `None` for malformed input.
pub fn scope_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl ReadRule {
    /// `<k>:<reg>:<label>:<pattern>:<source>`, every free-text field encoded.
    pub fn to_scope_fields(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            match self.kind {
                RuleKind::Path => "p",
                RuleKind::Glob => "g",
            },
            if self.registered { 1 } else { 0 },
            scope_encode(self.label.as_deref().unwrap_or("")),
            scope_encode(&self.pattern),
            scope_encode(&self.source),
        )
    }

    pub fn from_scope_fields(fields: &str) -> Option<Self> {
        let mut it = fields.split(':');
        let kind = match it.next()? {
            "p" => RuleKind::Path,
            "g" => RuleKind::Glob,
            _ => return None,
        };
        let registered = match it.next()? {
            "1" => true,
            "0" => false,
            _ => return None,
        };
        let label = scope_decode(it.next()?)?;
        let pattern = scope_decode(it.next()?)?;
        let source = scope_decode(it.next()?)?;
        if it.next().is_some() || pattern.is_empty() {
            return None;
        }
        if !label.is_empty() && !is_valid_label(&label) {
            return None;
        }
        Some(Self {
            kind,
            pattern,
            source,
            label: (!label.is_empty()).then_some(label),
            registered,
        })
    }
}

// ── Entry resolution ───────────────────────────────────────────────────────

/// Which list an entry belongs to; used in messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Allow,
    Deny,
}

impl ListKind {
    fn key(self) -> &'static str {
        match self {
            ListKind::Allow => "allow",
            ListKind::Deny => "deny",
        }
    }
}

fn registered_project_path(
    office_yaml: &Path,
    name: &str,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(office_yaml).map_err(|e| {
        format!(
            "it is not a path, and no project registry could be read at {} ({e}); a bare word \
             names a project registered in office.yaml",
            office_yaml.display()
        )
    })?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| {
        format!(
            "the project registry {} did not parse: {e}",
            office_yaml.display()
        )
    })?;
    let path = doc
        .get("projects")
        .and_then(|p| p.get(name))
        .and_then(|p| p.get("path"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| {
            format!(
                "no project named '{name}' is registered in {}",
                office_yaml.display()
            )
        })?;
    if let Some(rest) = path.strip_prefix("~/") {
        return home
            .map(|h| h.join(rest))
            .ok_or_else(|| format!("registered path '{path}' needs HOME, which is not set"));
    }
    Ok(PathBuf::from(path))
}

/// Resolve configured entries to rules. Allow entries must exist; a deny entry may name
/// a path that does not exist yet.
pub fn resolve_entries(
    entries: &[String],
    list: ListKind,
    env: &ReadEnv,
) -> Result<Vec<ReadRule>, ReadPolicyError> {
    let key = list.key();
    let main_root = canonicalize_lenient(&env.project_root);
    let hard = hard_paths(env, &main_root);
    let mut out: Vec<ReadRule> = Vec::new();
    for raw in entries {
        let entry = raw.trim();
        let err = |why: String| {
            ReadPolicyError(format!(
                "[chat.read] {key} entry '{entry}' is unusable: {why}. Fix `{key}` under \
                 [chat.read] in {}/.ta/workflow.toml (an absolute path, ~/path, a project name \
                 registered in office.yaml, or a glob).",
                env.project_root.display()
            ))
        };
        if entry.is_empty() {
            return Err(err("it is empty".to_string()));
        }
        let rule =
            resolve_one(entry, list, env, &main_root, &hard, &out).map_err(|f| err(f.message()))?;
        push_unique(&mut out, rule);
    }
    Ok(out)
}

fn push_unique(out: &mut Vec<ReadRule>, rule: ReadRule) {
    if !out
        .iter()
        .any(|r| r.kind == rule.kind && r.pattern == rule.pattern)
    {
        out.push(rule);
    }
}

// ── Layers: project file + per-machine overlay ─────────────────────────────

/// Where a list entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrySource {
    /// `[chat.read]` in the project's `.ta/workflow.toml` (shared by every install).
    Project,
    /// The per-machine overlay file (`chat-read.toml` in the TA state directory).
    Machine,
}

impl EntrySource {
    pub fn as_str(self) -> &'static str {
        match self {
            EntrySource::Project => "project",
            EntrySource::Machine => "machine",
        }
    }
}

/// What TA made of one entry on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryState {
    /// Resolved and part of the effective policy.
    Applied,
    /// TA cannot resolve it now (missing path, unregistered project name).
    Unresolved,
    /// A protected place, the whole home folder or the filesystem root: never allowed.
    Refused,
}

impl EntryState {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryState::Applied => "applied",
            EntryState::Unresolved => "unresolved",
            EntryState::Refused => "refused",
        }
    }
}

/// One entry of one list with its resolution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    pub entry: String,
    pub list: ListKind,
    pub source: EntrySource,
    pub state: EntryState,
    /// TA's reason when the state is not `Applied`.
    pub detail: Option<String>,
    /// The resolved rule when the state is `Applied`.
    pub rule: Option<ReadRule>,
}

impl ListKind {
    pub fn as_str(self) -> &'static str {
        self.key()
    }
}

/// The per-machine overlay: the two lists the poller (or `ta chat-read machine set`)
/// owns. Stored as TOML `allow = [...]` / `deny = [...]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineLists {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl MachineLists {
    /// Parse the overlay file text. `path` only names the file in the error.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ReadPolicyError> {
        toml::from_str(text).map_err(|e| {
            ReadPolicyError(format!(
                "the machine read-list file {} did not parse: {e}. Delete it or rewrite it with \
                 `ta chat-read machine set` (it holds only `allow = [...]` and `deny = [...]`).",
                path.display()
            ))
        })
    }

    /// The file contents written by `ta chat-read machine set`.
    pub fn render(&self) -> String {
        let mut out = String::from(
            "# Per-machine Chief of Staff read lists. Written by `ta chat-read machine set`\n\
             # (the Wayfinder poller). Added to `[chat.read]` of the project's\n\
             # .ta/workflow.toml; deny beats allow; the built-in protected places stay refused.\n",
        );
        for (key, list) in [("allow", &self.allow), ("deny", &self.deny)] {
            out.push_str(&format!("{key} = ["));
            for (i, e) in list.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&toml_string(e));
            }
            out.push_str("]\n");
        }
        out
    }
}

/// A TOML basic string.
fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The entries of both layers, in the order they are compiled: project first, then
/// the machine overlay.
#[derive(Debug, Clone, Copy)]
pub struct Layers<'a> {
    pub project_allow: &'a [String],
    pub project_deny: &'a [String],
    pub machine: &'a MachineLists,
}

/// Compile both layers. Effective lists are the UNION (project first); deny beats
/// allow at decision time, so a deny in either layer wins over an allow in either.
///
/// A PROJECT entry that cannot be applied is an error (the repo file is the owner's
/// explicit widening; a typo must be loud) unless `strict_project` is false, in
/// which case it is only reported. A MACHINE entry that cannot be applied is never
/// an error: it is skipped and reported (a folder the owner deleted must not stop the
/// Chief of Staff from launching, and the skipped allow can only narrow access; a
/// skipped deny is reported so the person can fix it).
pub fn compile_layers(
    env: &ReadEnv,
    layers: Layers<'_>,
    strict_project: bool,
) -> Result<(ReadPolicy, Vec<ListedEntry>), ReadPolicyError> {
    let main_root = canonicalize_lenient(&env.project_root);
    let hard = hard_paths(env, &main_root);
    let mut listed: Vec<ListedEntry> = Vec::new();
    let mut rules: [Vec<ReadRule>; 2] = [Vec::new(), Vec::new()];
    for (slot, list) in [(0usize, ListKind::Allow), (1usize, ListKind::Deny)] {
        let (project, machine) = match list {
            ListKind::Allow => (layers.project_allow, &layers.machine.allow),
            ListKind::Deny => (layers.project_deny, &layers.machine.deny),
        };
        for (source, entries) in [
            (EntrySource::Project, project),
            (EntrySource::Machine, machine.as_slice()),
        ] {
            for raw in entries {
                let entry = raw.trim();
                let outcome = if entry.is_empty() {
                    Err(ResolveFailure::Unresolved("it is empty".to_string()))
                } else {
                    resolve_one(entry, list, env, &main_root, &hard, &rules[slot])
                };
                match outcome {
                    Ok(rule) => {
                        listed.push(ListedEntry {
                            entry: entry.to_string(),
                            list,
                            source,
                            state: EntryState::Applied,
                            detail: None,
                            rule: Some(rule.clone()),
                        });
                        push_unique(&mut rules[slot], rule);
                    }
                    Err(failure) => {
                        let state = failure.state();
                        let why = failure.message();
                        if source == EntrySource::Project && strict_project {
                            let key = list.key();
                            return Err(ReadPolicyError(format!(
                                "[chat.read] {key} entry '{entry}' is unusable: {why}. Fix \
                                 `{key}` under [chat.read] in {}/.ta/workflow.toml (an absolute \
                                 path, ~/path, a project name registered in office.yaml, or a \
                                 glob).",
                                env.project_root.display()
                            )));
                        }
                        listed.push(ListedEntry {
                            entry: entry.to_string(),
                            list,
                            source,
                            state,
                            detail: Some(why),
                            rule: None,
                        });
                    }
                }
            }
        }
    }
    let [allow, deny] = rules;
    let policy = ReadPolicy::new(
        &env.project_root,
        allow,
        deny,
        env.home.as_deref(),
        env.state_dir.as_deref(),
    );
    Ok((policy, listed))
}

/// Resolve ONE entry for `list` against `env` without compiling a whole policy and
/// without writing anything. `current` are the rules already in force; an entry that
/// is already among them is not held against itself (project-name labels must be
/// unique).
pub fn resolve_single(
    entry: &str,
    list: ListKind,
    env: &ReadEnv,
    current: &[ReadRule],
) -> (EntryState, Option<String>) {
    let main_root = canonicalize_lenient(&env.project_root);
    let hard = hard_paths(env, &main_root);
    let entry = entry.trim();
    if entry.is_empty() {
        return (EntryState::Unresolved, Some("it is empty".to_string()));
    }
    let others: Vec<ReadRule> = current
        .iter()
        .filter(|r| r.source != entry)
        .cloned()
        .collect();
    match resolve_one(entry, list, env, &main_root, &hard, &others) {
        Ok(_) => (EntryState::Applied, None),
        Err(f) => (f.state(), Some(f.message())),
    }
}

/// Why one entry cannot be applied. `Refused` is a place that may never be readable
/// (the home folder as a whole, the filesystem root, a built-in protected place, `..`);
/// `Unresolved` is an entry this machine cannot resolve right now (a missing path, an
/// unregistered project name, an unusable name).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolveFailure {
    Refused(String),
    Unresolved(String),
}

impl ResolveFailure {
    fn message(self) -> String {
        match self {
            ResolveFailure::Refused(m) | ResolveFailure::Unresolved(m) => m,
        }
    }
    fn state(&self) -> EntryState {
        match self {
            ResolveFailure::Refused(_) => EntryState::Refused,
            ResolveFailure::Unresolved(_) => EntryState::Unresolved,
        }
    }
}

impl From<String> for ResolveFailure {
    fn from(m: String) -> Self {
        ResolveFailure::Unresolved(m)
    }
}

fn resolve_one(
    entry: &str,
    list: ListKind,
    env: &ReadEnv,
    main_root: &Path,
    hard: &[HardPath],
    existing: &[ReadRule],
) -> Result<ReadRule, ResolveFailure> {
    let ci = filesystem_is_case_insensitive();
    let is_path_like =
        entry.contains(['/', '\\']) || has_glob_chars(entry) || entry.starts_with('.');
    let mut registered = false;
    let expanded: PathBuf = if entry == "~" || entry.starts_with("~/") || entry.starts_with("~\\") {
        let home = env
            .home
            .as_ref()
            .ok_or_else(|| "it starts with ~ but HOME is not set".to_string())?;
        let rest = entry[1..].trim_start_matches(['/', '\\']);
        if rest.is_empty() {
            home.clone()
        } else {
            home.join(rest)
        }
    } else if entry.starts_with('~') {
        return Err("`~user` is not supported; use ~/path or an absolute path"
            .to_string()
            .into());
    } else if Path::new(entry).is_absolute() {
        PathBuf::from(entry)
    } else if is_path_like {
        env.project_root.join(entry)
    } else {
        registered = true;
        registered_project_path(&env.office_yaml, entry, env.home.as_deref())?
    };

    let text = expanded.to_string_lossy().to_string();
    if Path::new(&text)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(ResolveFailure::Refused(
            "it contains `..`, which is refused; write the real path".to_string(),
        ));
    }

    let (kind, pattern) = if has_glob_chars(&text) && !registered {
        let (pattern, literal) = glob_pattern(&text, entry)?;
        if list == ListKind::Allow {
            if let Some(h) = literal
                .as_ref()
                .and_then(|l| hard.iter().find(|h| path_within(l, &h.path, ci)))
            {
                return Err(ResolveFailure::Refused(format!(
                    "the glob starts inside {}, which can never be read (built-in hard deny)",
                    h.what
                )));
            }
        }
        (RuleKind::Glob, pattern)
    } else {
        let canonical = if list == ListKind::Allow {
            std::fs::canonicalize(&expanded)
                .map_err(|e| format!("{} cannot be resolved: {e}", expanded.display()))?
        } else {
            canonicalize_lenient(&expanded)
        };
        (RuleKind::Path, canonical.to_string_lossy().to_string())
    };

    if kind == RuleKind::Path {
        let canonical = PathBuf::from(&pattern);
        if list == ListKind::Allow {
            if canonical.parent().is_none() {
                return Err(ResolveFailure::Refused(
                    "the filesystem root cannot be allowed".to_string(),
                ));
            }
            if env
                .home
                .as_ref()
                .is_some_and(|h| std::fs::canonicalize(h).is_ok_and(|h| h == canonical))
            {
                return Err(ResolveFailure::Refused(
                    "the home directory cannot be allowed as a whole; list the folders you want \
                     read (for example ~/notes)"
                        .to_string(),
                ));
            }
            if let Some(h) = hard.iter().find(|h| path_within(&canonical, &h.path, ci)) {
                return Err(ResolveFailure::Refused(format!(
                    "{} lies inside {}, which can never be read (built-in hard deny)",
                    canonical.display(),
                    h.what
                )));
            }
        }
    }

    let label = if registered {
        Some(entry.to_string())
    } else if kind == RuleKind::Path && list == ListKind::Allow {
        Path::new(&pattern)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| is_valid_label(n))
            .map(str::to_string)
            .filter(|l| !existing.iter().any(|r| r.label.as_deref() == Some(l)))
    } else {
        None
    };
    if registered {
        let l = label.as_deref().unwrap_or_default();
        if !is_valid_label(l) {
            return Err(format!(
                "its name '{l}' is not usable as a project label (letters, digits, '.', '_' and \
                 '-' only)"
            )
            .into());
        }
        if existing.iter().any(|r| r.label.as_deref() == Some(l)) {
            return Err(format!(
                "another entry already uses the project label '{l}'; register one of them \
                 under a different name"
            )
            .into());
        }
    }
    let _ = main_root;
    Ok(ReadRule {
        kind,
        pattern,
        source: entry.to_string(),
        label,
        registered,
    })
}

/// Build the absolute glob for `text` (an absolute path containing glob characters):
/// the literal leading components are canonicalized and escaped, the rest kept as is.
/// A pattern that starts with `**/` is anchored "anywhere" (`/**/...`). Also returns
/// the canonical literal prefix (`None` for an anywhere glob).
fn glob_pattern(text: &str, entry: &str) -> Result<(String, Option<PathBuf>), ResolveFailure> {
    if entry.starts_with("**/") || entry.starts_with("**\\") {
        let rest = entry[3..].replace('\\', "/");
        Pattern::new(&rest).map_err(|e| format!("the glob is invalid: {e}"))?;
        return Ok((format!("/**/{rest}"), None));
    }
    let path = Path::new(text);
    let mut literal = PathBuf::new();
    let mut tail: Vec<String> = Vec::new();
    for c in path.components() {
        let part = c.as_os_str().to_string_lossy().to_string();
        if tail.is_empty() && !has_glob_chars(&part) {
            literal.push(c.as_os_str());
        } else {
            tail.push(part);
        }
    }
    if tail.is_empty() {
        return Err("the glob has no wildcard".to_string().into());
    }
    let literal = canonicalize_lenient(&literal);
    if literal.parent().is_none() {
        return Err(ResolveFailure::Refused(
            "a glob must start inside a real directory (not the filesystem root); anchor it, \
             for example ~/notes/**/*.md"
                .to_string(),
        ));
    }
    let mut pattern = Pattern::escape(&to_slash(&literal));
    for part in &tail {
        pattern.push('/');
        pattern.push_str(part);
    }
    Pattern::new(&pattern).map_err(|e| format!("the glob is invalid: {e}"))?;
    Ok((pattern, Some(literal)))
}

// ── Hard deny ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct HardPath {
    path: PathBuf,
    what: String,
}

fn hard_paths(env: &ReadEnv, main_root: &Path) -> Vec<HardPath> {
    let mut out: Vec<HardPath> = Vec::new();
    let mut push = |path: PathBuf, what: String| {
        let canon = canonicalize_lenient(&path);
        if canon != path {
            out.push(HardPath {
                path: canon,
                what: what.clone(),
            });
        }
        out.push(HardPath { path, what });
    };
    for root in [env.project_root.as_path(), main_root] {
        push(
            root.join(".ta"),
            "the project's .ta directory (TA state)".to_string(),
        );
        push(
            root.join(".git"),
            "the project's .git directory (VCS internals)".to_string(),
        );
    }
    if let Some(state) = &env.state_dir {
        push(state.clone(), "the TA state directory".to_string());
    }
    if let Some(home) = &env.home {
        for rel in HOME_PROTECTED {
            push(
                home.join(rel),
                format!("~/{rel} (credential store, browser profile or shell history)"),
            );
        }
    }
    out
}

// ── Decision ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCode {
    /// The path has a `..` component.
    Traversal,
    /// The path is not absolute (callers resolve relative paths first).
    NotAbsolute,
    /// The built-in hard deny list (never overridable).
    HardDeny,
    /// TA or VCS infrastructure inside an allowed root.
    Infrastructure,
    /// A deny entry of the user's config.
    UserDeny,
    /// Matched no allow entry.
    NotAllowed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: RefusalCode,
    /// Where the path really is (symlinks resolved).
    pub resolved: PathBuf,
    /// The rule responsible, as the owner wrote it, when one applies.
    pub rule: Option<String>,
    /// Actionable sentence naming the config key.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allowed {
        resolved: PathBuf,
        /// The allow entry that admitted the path, as the owner wrote it.
        rule: String,
        /// The root of that entry (the path itself for a file entry).
        root: PathBuf,
    },
    Denied(Refusal),
}

impl Verdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Verdict::Allowed { .. })
    }
}

/// The compiled read policy.
#[derive(Debug, Clone)]
pub struct ReadPolicy {
    /// Canonical project root; always allowed.
    pub project_root: PathBuf,
    /// Resolved allow rules, NOT including the project root.
    pub allow: Vec<ReadRule>,
    pub deny: Vec<ReadRule>,
    hard: Vec<HardPath>,
    ci: bool,
}

/// What the OS sandbox gets from the policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxReadView {
    /// Read-only roots (the project root, allowed paths, expansions of allow globs).
    pub read_roots: Vec<PathBuf>,
    /// Paths hidden inside those roots (user deny entries, expansions of deny globs,
    /// `.ta` and `.git` of every root).
    pub deny: Vec<PathBuf>,
    /// Entries that could not be expressed for the OS layer, each with the reason.
    pub notes: Vec<String>,
}

impl ReadPolicy {
    pub fn new(
        project_root: &Path,
        allow: Vec<ReadRule>,
        deny: Vec<ReadRule>,
        home: Option<&Path>,
        state_dir: Option<&Path>,
    ) -> Self {
        let main_root = canonicalize_lenient(project_root);
        let env = ReadEnv {
            project_root: project_root.to_path_buf(),
            home: home.map(Path::to_path_buf),
            state_dir: state_dir.map(Path::to_path_buf),
            office_yaml: PathBuf::new(),
        };
        let hard = hard_paths(&env, &main_root);
        Self {
            project_root: main_root,
            allow,
            deny,
            hard,
            ci: filesystem_is_case_insensitive(),
        }
    }

    /// Resolve config entries and compile the policy in one step.
    pub fn from_config(
        env: &ReadEnv,
        allow: &[String],
        deny: &[String],
    ) -> Result<Self, ReadPolicyError> {
        let allow = resolve_entries(allow, ListKind::Allow, env)?;
        let deny = resolve_entries(deny, ListKind::Deny, env)?;
        Ok(Self::new(
            &env.project_root,
            allow,
            deny,
            env.home.as_deref(),
            env.state_dir.as_deref(),
        ))
    }

    /// The root `project` names: `None` is the project root; a label names an allow
    /// entry. Errors list the readable labels.
    pub fn root_of_label(&self, label: Option<&str>) -> Result<PathBuf, String> {
        match label {
            None => Ok(self.project_root.clone()),
            Some(name) => self
                .allow
                .iter()
                .find(|r| r.kind == RuleKind::Path && r.label.as_deref() == Some(name))
                .map(|r| PathBuf::from(&r.pattern))
                .ok_or_else(|| {
                    let labels: Vec<&str> = self
                        .allow
                        .iter()
                        .filter(|r| r.kind == RuleKind::Path)
                        .filter_map(|r| r.label.as_deref())
                        .collect();
                    format!(
                        "project '{name}' is not readable from this session. Readable project \
                         names: {}. The owner lists readable resources under `allow` in \
                         [chat.read] of the project's .ta/workflow.toml.",
                        if labels.is_empty() {
                            "none".to_string()
                        } else {
                            labels.join(", ")
                        }
                    )
                }),
        }
    }

    /// The allow entry that is a registered project named `label`, for Wayfinder reads.
    pub fn registered_project(&self, label: &str) -> Option<PathBuf> {
        self.allow
            .iter()
            .find(|r| r.registered && r.label.as_deref() == Some(label))
            .map(|r| PathBuf::from(&r.pattern))
    }

    /// Decide whether the absolute `requested` path may be read.
    pub fn decide(&self, requested: &Path) -> Verdict {
        let shown = display_path(requested);
        if requested
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Verdict::Denied(Refusal {
                code: RefusalCode::Traversal,
                resolved: requested.to_path_buf(),
                rule: None,
                message: format!(
                    "'{shown}' is refused: it contains `..`. Pass the real path of the file."
                ),
            });
        }
        if !requested.is_absolute() {
            return Verdict::Denied(Refusal {
                code: RefusalCode::NotAbsolute,
                resolved: requested.to_path_buf(),
                rule: None,
                message: format!("'{shown}' is refused: it is not an absolute path."),
            });
        }
        let resolved = canonicalize_lenient(requested);
        let via = if normalize(requested) != resolved {
            format!(" (it resolves to '{}')", display_path(&resolved))
        } else {
            String::new()
        };

        // 1. Built-in hard deny: nothing can override it.
        for h in &self.hard {
            if path_within(&resolved, &h.path, self.ci) || path_within(requested, &h.path, self.ci)
            {
                return Verdict::Denied(Refusal {
                    code: RefusalCode::HardDeny,
                    resolved,
                    rule: Some(h.what.clone()),
                    message: format!(
                        "'{shown}'{via} is refused: it is inside {}, which a chat session never \
                         reads and no config entry can open (built-in hard deny).",
                        h.what
                    ),
                });
            }
        }

        // 2. User deny beats allow.
        for d in &self.deny {
            if d.matches(&resolved, self.ci) {
                return Verdict::Denied(Refusal {
                    code: RefusalCode::UserDeny,
                    resolved,
                    rule: Some(d.source.clone()),
                    message: format!(
                        "'{shown}'{via} is refused by the deny entry '{}' in [chat.read] deny \
                         of the project's .ta/workflow.toml. Remove that entry to make it \
                         readable.",
                        d.source
                    ),
                });
            }
        }

        // 3. Allow: the project root first, then the configured entries.
        let mut admitted: Option<(String, PathBuf)> = None;
        if path_within(&resolved, &self.project_root, self.ci) {
            admitted = Some((
                "the project root (always readable)".to_string(),
                self.project_root.clone(),
            ));
        } else if let Some(a) = self.allow.iter().find(|a| a.matches(&resolved, self.ci)) {
            if let Some(root) = a.root_for(&resolved, self.ci) {
                admitted = Some((a.source.clone(), root));
            }
        }
        let Some((rule, root)) = admitted else {
            return Verdict::Denied(Refusal {
                code: RefusalCode::NotAllowed,
                resolved,
                rule: None,
                message: format!(
                    "'{shown}'{via} is not readable from this session: it is outside the \
                     project root ({}) and matches no entry of `allow` under [chat.read] in the \
                     project's .ta/workflow.toml. Add the folder there (and re-launch the \
                     session) to let the Chief of Staff read it.",
                    display_path(&self.project_root)
                ),
            });
        };

        // 4. TA / VCS infrastructure below the admitting root.
        let infra: Option<String> = resolved.strip_prefix(&root).ok().and_then(|rel| {
            rel.components().find_map(|comp| {
                let name = comp.as_os_str().to_string_lossy().to_string();
                ta_workspace::path_safety::is_infrastructure_component(&name).then_some(name)
            })
        });
        if let Some(name) = infra {
            return Verdict::Denied(Refusal {
                code: RefusalCode::Infrastructure,
                resolved,
                rule: Some(name.clone()),
                message: format!(
                    "'{shown}'{via} is refused: it is inside TA or VCS infrastructure \
                     ('{name}'), which a chat session never reads."
                ),
            });
        }
        Verdict::Allowed {
            resolved,
            rule,
            root,
        }
    }

    /// Resolve `path` as the agent wrote it, then [`Self::decide`]. Absolute and `~/`
    /// paths stand for themselves; a relative path is relative to `base` (the project
    /// root, or the root a `project` argument named).
    pub fn decide_requested(&self, base: &Path, home: Option<&Path>, path: &str) -> Verdict {
        match self.resolve_requested(base, home, path) {
            Ok(p) => self.decide(&p),
            Err(refusal) => Verdict::Denied(refusal),
        }
    }

    /// The absolute form of an agent-supplied path (no filesystem access).
    pub fn resolve_requested(
        &self,
        base: &Path,
        home: Option<&Path>,
        path: &str,
    ) -> Result<PathBuf, Refusal> {
        let refuse = |message: String| Refusal {
            code: RefusalCode::NotAbsolute,
            resolved: PathBuf::from(path),
            rule: None,
            message,
        };
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(refuse("the path is empty.".to_string()));
        }
        if trimmed.contains('\0') {
            return Err(refuse("the path contains a NUL byte.".to_string()));
        }
        if trimmed == "~" || trimmed.starts_with("~/") || trimmed.starts_with("~\\") {
            let home = home.ok_or_else(|| {
                refuse(format!(
                    "'{trimmed}' starts with ~ but HOME is not set for this session; pass an \
                     absolute path."
                ))
            })?;
            let rest = trimmed[1..].trim_start_matches(['/', '\\']);
            return Ok(if rest.is_empty() {
                home.to_path_buf()
            } else {
                home.join(rest)
            });
        }
        if trimmed.starts_with('~') {
            return Err(refuse(format!(
                "'{trimmed}' uses `~user`, which is not supported; pass ~/path or an absolute \
                 path."
            )));
        }
        let p = Path::new(trimmed);
        Ok(if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        })
    }

    /// The OS-level view: read-only roots and the paths hidden inside them. Globs are
    /// expanded against the filesystem NOW (the sandbox sees the paths that exist at
    /// launch); the gateway re-evaluates the glob on every read.
    pub fn sandbox_view(&self) -> SandboxReadView {
        let mut view = SandboxReadView::default();
        let push_root = |v: &mut SandboxReadView, p: PathBuf| {
            if !v.read_roots.contains(&p) {
                v.read_roots.push(p);
            }
        };
        push_root(&mut view, self.project_root.clone());
        let expand = |rule: &ReadRule, notes: &mut Vec<String>| -> Vec<PathBuf> {
            match rule.kind {
                RuleKind::Path => vec![PathBuf::from(&rule.pattern)],
                RuleKind::Glob => {
                    let opts = MatchOptions {
                        case_sensitive: !self.ci,
                        require_literal_separator: true,
                        require_literal_leading_dot: false,
                    };
                    let pattern = if rule.pattern.starts_with("/**/") {
                        notes.push(format!(
                            "the 'anywhere' glob '{}' is enforced by the gateway only; the OS \
                             sandbox does not expand it",
                            rule.source
                        ));
                        return Vec::new();
                    } else {
                        rule.pattern.clone()
                    };
                    let mut found = Vec::new();
                    if let Ok(paths) = glob::glob_with(&pattern, opts) {
                        for p in paths.flatten().take(MAX_GLOB_EXPANSION + 1) {
                            found.push(canonicalize_lenient(&p));
                        }
                    }
                    if found.len() > MAX_GLOB_EXPANSION {
                        found.truncate(MAX_GLOB_EXPANSION);
                        notes.push(format!(
                            "the glob '{}' matched more than {MAX_GLOB_EXPANSION} paths; the OS \
                             sandbox covers the first {MAX_GLOB_EXPANSION}",
                            rule.source
                        ));
                    }
                    found
                }
            }
        };
        for rule in &self.allow {
            for p in expand(rule, &mut view.notes) {
                // A deny that covers the whole allowed path wins, as in `decide`.
                if self.deny.iter().any(|d| d.matches(&p, self.ci)) {
                    view.notes.push(format!(
                        "allow entry '{}' ({}) is inside a deny entry and is not granted to \
                         the OS sandbox",
                        rule.source,
                        display_path(&p)
                    ));
                    continue;
                }
                if self.hard.iter().any(|h| path_within(&p, &h.path, self.ci)) {
                    continue;
                }
                push_root(&mut view, p);
            }
        }
        for rule in &self.deny {
            for p in expand(rule, &mut view.notes) {
                if !view.deny.contains(&p) {
                    view.deny.push(p);
                }
            }
        }
        for root in view.read_roots.clone() {
            for sub in [".ta", ".git"] {
                let d = root.join(sub);
                if !view.deny.contains(&d) {
                    view.deny.push(d);
                }
            }
        }
        view
    }

    /// Hard-deny paths, for `ta chat-read list`.
    pub fn hard_deny_list(&self) -> Vec<(PathBuf, String)> {
        let mut seen: Vec<PathBuf> = Vec::new();
        let mut out = Vec::new();
        for h in &self.hard {
            if !seen.contains(&h.path) {
                seen.push(h.path.clone());
                out.push((h.path.clone(), h.what.clone()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fx {
        _tmp: tempfile::TempDir,
        base: PathBuf,
        project: PathBuf,
        home: PathBuf,
    }

    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let project = base.join("proj");
        let home = base.join("home");
        for d in [
            project.join(".ta"),
            project.join(".git"),
            project.join("src"),
            home.join("notes/private"),
            home.join("notes/public"),
            home.join(".ssh"),
            home.join("other"),
            base.join("outside"),
        ] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(project.join("src/lib.rs"), "x").unwrap();
        fs::write(project.join(".ta/workflow.toml"), "x").unwrap();
        fs::write(project.join(".git/config"), "x").unwrap();
        fs::write(home.join("notes/a.md"), "x").unwrap();
        fs::write(home.join("notes/private/b.md"), "x").unwrap();
        fs::write(home.join("notes/public/c.md"), "x").unwrap();
        fs::write(home.join(".ssh/id_ed25519"), "x").unwrap();
        fs::write(home.join("other/o.md"), "x").unwrap();
        fs::write(base.join("outside/z.md"), "x").unwrap();
        Fx {
            _tmp: tmp,
            base,
            project,
            home,
        }
    }

    fn env(f: &Fx) -> ReadEnv {
        ReadEnv {
            project_root: f.project.clone(),
            home: Some(f.home.clone()),
            state_dir: None,
            office_yaml: f.project.join("office.yaml"),
        }
    }

    fn policy(f: &Fx, allow: &[&str], deny: &[&str]) -> ReadPolicy {
        let a: Vec<String> = allow.iter().map(|s| s.to_string()).collect();
        let d: Vec<String> = deny.iter().map(|s| s.to_string()).collect();
        ReadPolicy::from_config(&env(f), &a, &d).unwrap()
    }

    #[test]
    fn empty_allow_means_project_root_only() {
        let f = fx();
        let p = policy(&f, &[], &[]);
        assert!(p.decide(&f.project.join("src/lib.rs")).is_allowed());
        let v = p.decide(&f.home.join("notes/a.md"));
        match v {
            Verdict::Denied(r) => {
                assert_eq!(r.code, RefusalCode::NotAllowed);
                assert!(r.message.contains("[chat.read]"), "{}", r.message);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn allow_tilde_dir_and_deny_subtree_inside_it() {
        let f = fx();
        let p = policy(&f, &["~/notes"], &["~/notes/private"]);
        assert!(p.decide(&f.home.join("notes/a.md")).is_allowed());
        assert!(p.decide(&f.home.join("notes/public/c.md")).is_allowed());
        match p.decide(&f.home.join("notes/private/b.md")) {
            Verdict::Denied(r) => {
                assert_eq!(r.code, RefusalCode::UserDeny);
                assert_eq!(r.rule.as_deref(), Some("~/notes/private"));
            }
            other => panic!("{other:?}"),
        }
        assert!(!p.decide(&f.home.join("other/o.md")).is_allowed());
        assert!(!p.decide(&f.base.join("outside/z.md")).is_allowed());
    }

    #[test]
    fn deny_beats_allow_for_the_same_path() {
        let f = fx();
        let p = policy(&f, &["~/notes"], &["~/notes"]);
        assert!(!p.decide(&f.home.join("notes/a.md")).is_allowed());
    }

    #[test]
    fn hard_deny_cannot_be_overridden_and_loud_at_resolution() {
        let f = fx();
        let err = ReadPolicy::from_config(&env(&f), &["~/.ssh".to_string()], &[]).unwrap_err();
        assert!(err.0.contains("hard deny"), "{err}");
        // Allowing a parent still hides the protected child.
        let err =
            ReadPolicy::from_config(&env(&f), &[f.home.display().to_string()], &[]).unwrap_err();
        assert!(err.0.contains("home directory"), "{err}");
        let p = policy(&f, &[], &[]);
        match p.decide(&f.project.join(".ta/workflow.toml")) {
            Verdict::Denied(r) => assert_eq!(r.code, RefusalCode::HardDeny),
            other => panic!("{other:?}"),
        }
        match p.decide(&f.project.join(".git/config")) {
            Verdict::Denied(r) => assert_eq!(r.code, RefusalCode::HardDeny),
            other => panic!("{other:?}"),
        }
        match p.decide(&f.home.join(".ssh/id_ed25519")) {
            Verdict::Denied(r) => assert_eq!(r.code, RefusalCode::HardDeny),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn infrastructure_below_an_allowed_root_is_refused() {
        let f = fx();
        fs::create_dir_all(f.home.join("notes/.git")).unwrap();
        fs::write(f.home.join("notes/.git/HEAD"), "x").unwrap();
        let p = policy(&f, &["~/notes"], &[]);
        match p.decide(&f.home.join("notes/.git/HEAD")) {
            Verdict::Denied(r) => assert_eq!(r.code, RefusalCode::Infrastructure),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dotdot_is_refused_even_when_it_lands_in_an_allowed_root() {
        let f = fx();
        let p = policy(&f, &["~/notes"], &[]);
        let sneaky = f.home.join("notes/../notes/a.md");
        match p.decide(&sneaky) {
            Verdict::Denied(r) => assert_eq!(r.code, RefusalCode::Traversal),
            other => panic!("{other:?}"),
        }
        let sneaky = f.project.join("src/../../outside/z.md");
        assert!(!p.decide(&sneaky).is_allowed());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_an_allowed_root_is_refused() {
        let f = fx();
        std::os::unix::fs::symlink(f.base.join("outside"), f.home.join("notes/escape")).unwrap();
        std::os::unix::fs::symlink(f.home.join("notes/a.md"), f.home.join("notes/alias.md"))
            .unwrap();
        let p = policy(&f, &["~/notes"], &[]);
        match p.decide(&f.home.join("notes/escape/z.md")) {
            Verdict::Denied(r) => {
                assert_eq!(r.code, RefusalCode::NotAllowed);
                assert!(r.message.contains("resolves to"), "{}", r.message);
            }
            other => panic!("{other:?}"),
        }
        // A link that stays inside the root is fine.
        assert!(p.decide(&f.home.join("notes/alias.md")).is_allowed());
        // A symlink into a denied subtree is judged by where it really leads.
        std::os::unix::fs::symlink(f.home.join("notes/private"), f.home.join("notes/peek"))
            .unwrap();
        let p = policy(&f, &["~/notes"], &["~/notes/private"]);
        assert!(!p.decide(&f.home.join("notes/peek/b.md")).is_allowed());
    }

    #[test]
    fn globs_allow_and_deny_with_subtrees() {
        let f = fx();
        let p = policy(&f, &["~/notes/pub*"], &["~/notes/**/b.md"]);
        assert!(p.decide(&f.home.join("notes/public/c.md")).is_allowed());
        assert!(!p.decide(&f.home.join("notes/a.md")).is_allowed());
        let p = policy(&f, &["~/notes"], &["**/b.md"]);
        assert!(!p.decide(&f.home.join("notes/private/b.md")).is_allowed());
        assert!(p.decide(&f.home.join("notes/public/c.md")).is_allowed());
        let p = policy(&f, &["~/notes"], &["~/notes/*/b.md"]);
        assert!(!p.decide(&f.home.join("notes/private/b.md")).is_allowed());
    }

    #[test]
    fn project_names_resolve_through_the_registry_and_typos_are_loud() {
        let f = fx();
        fs::write(
            f.project.join("office.yaml"),
            format!(
                "projects:\n  wayfinder:\n    path: {}\n",
                f.home.join("other").display()
            ),
        )
        .unwrap();
        let p = policy(&f, &["wayfinder"], &[]);
        assert!(p.decide(&f.home.join("other/o.md")).is_allowed());
        assert_eq!(
            p.registered_project("wayfinder").unwrap(),
            fs::canonicalize(f.home.join("other")).unwrap()
        );
        assert!(p.root_of_label(Some("wayfinder")).is_ok());
        assert!(p.root_of_label(Some("nope")).is_err());
        let err = ReadPolicy::from_config(&env(&f), &["nope".to_string()], &[]).unwrap_err();
        assert!(err.0.contains("allow") && err.0.contains("nope"), "{err}");
        let err = ReadPolicy::from_config(&env(&f), &["/definitely/not/here".to_string()], &[])
            .unwrap_err();
        assert!(err.0.contains("cannot be resolved"), "{err}");
        let err = ReadPolicy::from_config(&env(&f), &["/".to_string()], &[]).unwrap_err();
        assert!(err.0.contains("filesystem root"), "{err}");
        let err = ReadPolicy::from_config(&env(&f), &["../x".to_string()], &[]).unwrap_err();
        assert!(err.0.contains("`..`"), "{err}");
    }

    #[test]
    fn relative_requests_resolve_against_the_chosen_root() {
        let f = fx();
        let p = policy(&f, &["~/notes"], &[]);
        let v = p.decide_requested(&f.project, Some(&f.home), "src/lib.rs");
        assert!(v.is_allowed());
        let v = p.decide_requested(&f.project, Some(&f.home), "~/notes/a.md");
        assert!(v.is_allowed());
        let notes = p.root_of_label(Some("notes")).unwrap();
        assert!(p
            .decide_requested(&notes, Some(&f.home), "public/c.md")
            .is_allowed());
        let abs = f.home.join("notes/a.md").display().to_string();
        assert!(p.decide_requested(&f.project, None, &abs).is_allowed());
        assert!(!p
            .decide_requested(&f.project, None, "~/notes/a.md")
            .is_allowed());
    }

    #[test]
    fn scopes_round_trip_including_awkward_characters() {
        let rule = ReadRule {
            kind: RuleKind::Glob,
            pattern: "/Users/me/My Notes, 2025/*.md".to_string(),
            source: "~/My Notes, 2025/*.md".to_string(),
            label: Some("notes".to_string()),
            registered: true,
        };
        let fields = rule.to_scope_fields();
        assert!(!fields.contains(','));
        assert_eq!(fields.matches(':').count(), 4);
        assert_eq!(ReadRule::from_scope_fields(&fields).unwrap(), rule);
        assert!(ReadRule::from_scope_fields("p:1:x").is_none());
        assert!(ReadRule::from_scope_fields("z:1:::").is_none());
        assert_eq!(scope_decode("%zz"), None);
        let win = r"C:\Users\me\notes";
        assert_eq!(scope_decode(&scope_encode(win)).as_deref(), Some(win));
    }

    #[test]
    fn sandbox_view_lists_roots_denies_and_expanded_globs() {
        let f = fx();
        let p = policy(&f, &["~/notes", "~/other/*.md"], &["~/notes/private"]);
        let v = p.sandbox_view();
        assert!(v.read_roots.contains(&f.project));
        assert!(v.read_roots.contains(&f.home.join("notes")));
        assert!(v.read_roots.contains(&f.home.join("other/o.md")));
        assert!(v.deny.contains(&f.home.join("notes/private")));
        assert!(v.deny.contains(&f.project.join(".ta")));
        assert!(v.deny.contains(&f.home.join("notes/.git")));
    }

    #[test]
    fn read_resource_uses_forward_slashes_without_a_leading_slash() {
        assert_eq!(
            read_resource(Path::new("/Users/me/a b/c.md")),
            "fs://read/Users/me/a b/c.md"
        );
        assert_eq!(
            read_resource(Path::new(r"\\?\C:\Users\me\c.md")),
            "fs://read/C:/Users/me/c.md"
        );
    }

    #[test]
    fn path_within_compares_components() {
        assert!(path_within(Path::new("/a/b/c"), Path::new("/a/b"), false));
        assert!(!path_within(Path::new("/a/bc"), Path::new("/a/b"), false));
        assert!(path_within(Path::new("/A/B"), Path::new("/a/b"), true));
    }

    // ── layers: project file + machine overlay ─────────────────────────────

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn layered(
        f: &Fx,
        project: (&[&str], &[&str]),
        machine: (&[&str], &[&str]),
        strict: bool,
    ) -> Result<(ReadPolicy, Vec<ListedEntry>), ReadPolicyError> {
        let m = MachineLists {
            allow: strings(machine.0),
            deny: strings(machine.1),
        };
        compile_layers(
            &env(f),
            Layers {
                project_allow: &strings(project.0),
                project_deny: &strings(project.1),
                machine: &m,
            },
            strict,
        )
    }

    #[test]
    fn layers_union_and_deny_beats_allow_across_layers() {
        let f = fx();
        // project allows notes, machine denies notes/private (machine deny wins)
        let (p, _) = layered(&f, (&["~/notes"], &[]), (&[], &["~/notes/private"]), true).unwrap();
        assert!(p.decide(&f.home.join("notes/a.md")).is_allowed());
        assert!(!p.decide(&f.home.join("notes/private/b.md")).is_allowed());
        // machine allows notes, project denies notes/private (project deny wins)
        let (p, _) = layered(&f, (&[], &["~/notes/private"]), (&["~/notes"], &[]), true).unwrap();
        assert!(p.decide(&f.home.join("notes/public/c.md")).is_allowed());
        assert!(!p.decide(&f.home.join("notes/private/b.md")).is_allowed());
        // the same path allowed in one layer and denied in the other: denied
        let (p, _) = layered(&f, (&["~/notes"], &[]), (&[], &["~/notes"]), true).unwrap();
        assert!(!p.decide(&f.home.join("notes/a.md")).is_allowed());
        // union: each layer contributes its own root
        let (p, _) = layered(&f, (&["~/notes"], &[]), (&["~/other"], &[]), true).unwrap();
        assert!(p.decide(&f.home.join("notes/a.md")).is_allowed());
        assert!(p.decide(&f.home.join("other/o.md")).is_allowed());
    }

    #[test]
    fn machine_entries_that_cannot_apply_are_reported_not_fatal() {
        let f = fx();
        let (p, listed) = layered(
            &f,
            (&["~/notes"], &[]),
            (&["~/nope", "~", "~/.ssh", "/"], &[]),
            true,
        )
        .unwrap();
        let state = |e: &str| {
            listed
                .iter()
                .find(|l| l.entry == e && l.source == EntrySource::Machine)
                .unwrap()
                .clone()
        };
        let nope = state("~/nope");
        assert_eq!(nope.state, EntryState::Unresolved);
        assert!(nope.detail.unwrap().contains("cannot be resolved"));
        let home = state("~");
        assert_eq!(home.state, EntryState::Refused);
        assert!(home.detail.unwrap().contains("home directory"));
        let ssh = state("~/.ssh");
        assert_eq!(ssh.state, EntryState::Refused);
        assert!(ssh.detail.unwrap().contains("hard deny"));
        assert_eq!(state("/").state, EntryState::Refused);
        // The project entry still applies and the floor is intact.
        assert!(p.decide(&f.home.join("notes/a.md")).is_allowed());
        assert!(!p.decide(&f.home.join(".ssh/id_ed25519")).is_allowed());
        assert!(!p.decide(&f.home.join("other/o.md")).is_allowed());
    }

    #[test]
    fn a_project_entry_that_cannot_apply_is_an_error_only_when_strict() {
        let f = fx();
        let err = layered(&f, (&["~/nope"], &[]), (&[], &[]), true)
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("~/nope") && err.contains("workflow.toml"),
            "{err}"
        );
        let (_, listed) = layered(&f, (&["~/nope"], &[]), (&[], &[]), false).unwrap();
        assert_eq!(listed[0].source, EntrySource::Project);
        assert_eq!(listed[0].state, EntryState::Unresolved);
    }

    #[test]
    fn dotdot_and_globs_into_protected_places_are_refused() {
        let f = fx();
        let (_, listed) = layered(
            &f,
            (&[], &[]),
            (
                &["~/notes/../other", "~/.ssh/**/*.pub", "~/notes/**/*.md"],
                &[],
            ),
            true,
        )
        .unwrap();
        assert_eq!(listed[0].state, EntryState::Refused);
        assert!(listed[0].detail.as_deref().unwrap().contains(".."));
        assert_eq!(listed[1].state, EntryState::Refused);
        assert_eq!(listed[2].state, EntryState::Applied);
    }

    #[test]
    fn a_machine_allow_cannot_make_a_symlink_out_of_its_root_readable() {
        #[cfg(unix)]
        {
            let f = fx();
            std::os::unix::fs::symlink(f.base.join("outside"), f.home.join("notes/link")).unwrap();
            let (p, _) = layered(&f, (&[], &[]), (&["~/notes"], &[]), true).unwrap();
            assert!(p.decide(&f.home.join("notes/a.md")).is_allowed());
            assert!(!p.decide(&f.home.join("notes/link/z.md")).is_allowed());
        }
    }

    #[test]
    fn resolve_single_does_not_hold_an_entry_against_itself() {
        let f = fx();
        let reg = f.base.join("office.yaml");
        fs::write(
            &reg,
            format!(
                "projects:\n  other:\n    path: {}\n",
                f.home.join("other").display()
            ),
        )
        .unwrap();
        let mut e = env(&f);
        e.office_yaml = reg;
        let (policy, listed) = compile_layers(
            &e,
            Layers {
                project_allow: &[],
                project_deny: &[],
                machine: &MachineLists {
                    allow: strings(&["other"]),
                    deny: vec![],
                },
            },
            true,
        )
        .unwrap();
        assert_eq!(listed[0].state, EntryState::Applied);
        let (state, _) = resolve_single("other", ListKind::Allow, &e, &policy.allow);
        assert_eq!(state, EntryState::Applied);
        let (state, detail) = resolve_single("unregistered", ListKind::Allow, &e, &policy.allow);
        assert_eq!(state, EntryState::Unresolved);
        assert!(detail.unwrap().contains("unregistered"));
    }
}
