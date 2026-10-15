//! Generic lifecycle for hook-kind optimizers (a hook installed into an agent's config).
//!
//! Nothing here knows about RTK. A hook optimizer is a [`HookSpec`] (data): the binary and its
//! minimum version, the command lines that install, verify and uninstall, the edition it must be,
//! and the staging paths the installer is allowed to touch. `rtk-oss` is one spec supplied by
//! `Registry::builtin()` and overridable from `[optimizers.input.rtk]`.
//!
//! [`HookInstaller::install`] puts the hook into ONE goal's staging copy and returns a
//! [`HookInstall`] guard. The guard restores every declared path byte-for-byte on drop, so a
//! failed goal cleans up. A goal that is killed, or a TA crash, never runs `Drop`, so each install
//! also writes a manifest under `.ta/compression/installs/` that [`sweep_orphans`] replays on the
//! next start.
//!
//! The installer runs the optimizer's binary with `HOME` (and the Windows and XDG equivalents)
//! pointed at an empty scratch directory. A hook installer that ignores project scope and writes
//! to the user's home therefore writes into scratch, is detected, and the install is refused. The
//! user's real agent config is never reachable from the installer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::{estimate_tokens, OptMetrics, OriginalStore};

/// Where install manifests live, relative to the project root.
pub const INSTALLS_DIR: &str = ".ta/compression/installs";

/// A hook entry TA writes into an agent settings file inside the staging copy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsHook {
    /// Staging-relative settings file (`.claude/settings.json`).
    pub file: String,
    /// Hook event (`PreToolUse`).
    pub event: String,
    /// Tool matcher (`Bash`).
    pub matcher: String,
    /// Command the agent runs for the hook.
    pub command: String,
}

/// Everything TA needs to manage one hook optimizer. Data, not code.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HookSpec {
    /// Binary looked up on PATH.
    pub binary: String,
    /// Minimum acceptable `--version`, dotted numbers (`0.23.0`).
    pub min_version: String,
    /// Edition TA accepts without opt-in (`oss`).
    pub edition: String,
    /// Licence string recorded in the metrics record.
    pub licence: String,
    /// Arguments (after the binary) that install the hook in the current directory (project scope).
    pub install: Vec<String>,
    /// Alternative to `install` for agents whose hook is a settings-file entry: TA merges this
    /// one entry into the staging copy's settings file instead of running a command. Used when
    /// the optimizer's own installer can only write to the user's global config.
    pub settings_hook: Option<SettingsHook>,
    /// Arguments that report whether the hook is active; exit 0 means active.
    pub verify: Vec<String>,
    /// Arguments that remove the hook. Best effort: restoring `touches` is the guarantee.
    pub uninstall: Vec<String>,
    /// Staging-relative files and directories the installer may create or change.
    pub touches: Vec<String>,
    /// Arguments printed to identify the edition (default `--help`).
    pub edition_probe: Vec<String>,
    /// Text in the probe output that means the full (BUSL) edition.
    pub full_markers: Vec<String>,
    /// Text that must appear in the probe output for the oss verdict to be trusted. Guards
    /// against a probe that failed or printed something unrelated.
    pub sanity_marker: String,
    /// Agents the install line is written for (empty = any). A goal for another agent runs
    /// unoptimised and says so, rather than receiving a hook meant for a different agent.
    pub agents: Vec<String>,
    /// Allow the full edition. Local experiments only; recorded in the metrics record.
    pub allow_busl: bool,
}

impl HookSpec {
    /// The shipped spec for `rtk-oss`.
    ///
    /// Confirmed from RTK's docs (rtk-ai.app, 2026-10): `rtk --version` prints `rtk x.y.z`;
    /// `rtk init --show` reports hook status; `--auto-patch` skips the settings.json prompt; both
    /// editions install a binary called `rtk`; the Apache-2.0 edition omits the `pipeline`,
    /// `methods` and `tasks` subcommands. NOT confirmed: the Claude Code project-scope install
    /// line (the docs show `-g` for the hook and a bare `rtk init` that writes CLAUDE.md only) and
    /// an uninstall verb. Both are replaceable from `[optimizers.input.rtk]`; a wrong install line
    /// fails verification, and the goal then runs unoptimised and says so.
    pub fn rtk_oss() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        Self {
            binary: "rtk".into(),
            min_version: "0.23.0".into(),
            edition: "oss".into(),
            licence: "Apache-2.0 (rtk-oss)".into(),
            // Verified by hand against rtk-oss 1.0.1 (2026-10-15): project-scope `rtk init` writes
            // only CLAUDE.md and .rtk/filters.toml and does NOT install the hook ("hooks:
            // registered globally"); `--agent claude-code` is rejected (the value is `claude`).
            // The hook is a PreToolUse entry whose command is `rtk hook claude`, so TA writes
            // that one entry into the staging copy's `.claude/settings.json` itself and never
            // runs `rtk init -g`, which would write to the user's ~/.claude.
            install: Vec::new(),
            settings_hook: Some(SettingsHook {
                file: ".claude/settings.json".into(),
                event: "PreToolUse".into(),
                matcher: "Bash".into(),
                command: "rtk hook claude".into(),
            }),
            // Dry-run of the hook engine: exits 0 and prints the rewrite for a noisy command.
            verify: s(&["hook", "check", "git status"]),
            uninstall: Vec::new(),
            touches: s(&[".claude"]),
            edition_probe: s(&["--help"]),
            full_markers: s(&["pipeline", "methods", "tasks"]),
            sanity_marker: "init".into(),
            agents: s(&["claude-code"]),
            allow_busl: false,
        }
    }

    /// Apply a config override: every non-empty field replaces the default.
    pub fn overlay(&mut self, o: &HookSpec) {
        fn s(dst: &mut String, src: &str) {
            if !src.is_empty() {
                *dst = src.to_string();
            }
        }
        fn v(dst: &mut Vec<String>, src: &[String]) {
            if !src.is_empty() {
                *dst = src.to_vec();
            }
        }
        s(&mut self.binary, &o.binary);
        s(&mut self.min_version, &o.min_version);
        s(&mut self.edition, &o.edition);
        s(&mut self.licence, &o.licence);
        s(&mut self.sanity_marker, &o.sanity_marker);
        v(&mut self.install, &o.install);
        v(&mut self.verify, &o.verify);
        v(&mut self.uninstall, &o.uninstall);
        v(&mut self.touches, &o.touches);
        v(&mut self.edition_probe, &o.edition_probe);
        v(&mut self.full_markers, &o.full_markers);
        v(&mut self.agents, &o.agents);
        self.allow_busl = o.allow_busl;
        // Field by field, like every other field: a table that names only `command` must keep the
        // default `file`, `event` and `matcher` rather than blank them.
        if let Some(oh) = &o.settings_hook {
            match &mut self.settings_hook {
                Some(h) => {
                    s(&mut h.file, &oh.file);
                    s(&mut h.event, &oh.event);
                    s(&mut h.matcher, &oh.matcher);
                    s(&mut h.command, &oh.command);
                }
                None => self.settings_hook = Some(oh.clone()),
            }
        }
    }
}

/// Edition of a found binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edition {
    Oss,
    Full,
    Unknown,
}

/// What a PATH probe found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookProbe {
    pub binary_path: String,
    pub version: String,
    pub edition: Edition,
    /// The full edition was accepted only because `allow_busl` is set.
    pub busl_allowed: bool,
}

/// Why a hook optimizer cannot be used. Every variant's text names what is missing and the fix.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum HookRefusal {
    #[error("no spec for hook optimizer '{0}'. Add [optimizers.input.{0}] with binary, min_version, edition and install to .ta/daemon.toml")]
    NoSpec(String),
    #[error("'{binary}' was not found on PATH. TA never installs it. Install the Apache-2.0 edition yourself: brew install rtk-ai/tap/rtk-oss")]
    BinaryMissing { binary: String },
    #[error("could not read a version from `{binary} --version` (printed {output:?}). Install a current rtk-oss: brew install rtk-ai/tap/rtk-oss")]
    VersionUnreadable { binary: String, output: String },
    #[error("'{binary}' is version {found} but TA needs at least {min}. Upgrade: brew upgrade rtk-ai/tap/rtk-oss")]
    TooOld {
        binary: String,
        found: String,
        min: String,
    },
    #[error("'{binary}' is the full edition (Apache-2.0 plus BUSL-1.1). The owner chose rtk-oss (Apache-2.0 only). Install it: brew uninstall rtk; brew install rtk-ai/tap/rtk-oss. For a local experiment only, set allow_busl = true under [optimizers.input.rtk]")]
    FullEdition { binary: String },
    #[error("could not determine the edition of '{binary}' ({detail}), so TA treats it as unknown and refuses. Install the Apache-2.0 edition: brew install rtk-ai/tap/rtk-oss. For a local experiment only, set allow_busl = true under [optimizers.input.rtk]")]
    EditionUnknown { binary: String, detail: String },
    #[error("hook optimizer spec for '{name}' has no `{field}` command line. Set `{field}` under [optimizers.input.{name}] in .ta/daemon.toml")]
    SpecIncomplete { name: String, field: &'static str },
    #[error("installing the '{name}' hook failed in {dir}: {detail}. The goal runs with no input optimizer; check `{cmd}` by hand in a scratch repo")]
    InstallFailed {
        name: String,
        dir: String,
        detail: String,
        cmd: String,
    },
    #[error("the '{name}' hook was installed but could not be verified: {detail}. The goal runs with no input optimizer; check `{cmd}` by hand in a scratch repo")]
    NotVerified {
        name: String,
        detail: String,
        cmd: String,
    },
    #[error("the '{name}' installer wrote outside the goal's staging copy (found {paths} in a scratch home), so it is not project scope and was not used. Set a project-scope `install` under [optimizers.input.{name}]")]
    EscapedProjectScope { name: String, paths: String },
}

/// Parse `1.2.3`, `v1.2.3` or `rtk 1.2.3-beta` into numeric components.
pub fn parse_version(text: &str) -> Option<Vec<u64>> {
    for tok in text.split_whitespace() {
        let tok = tok.trim_start_matches('v');
        let core = tok.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<_> = core.split('.').map(str::parse::<u64>).collect();
        if parts.len() >= 2 && parts.iter().all(|p| p.is_ok()) {
            return Some(parts.into_iter().map(|p| p.unwrap()).collect());
        }
    }
    None
}

fn version_at_least(found: &[u64], min: &[u64]) -> bool {
    for i in 0..found.len().max(min.len()) {
        let (f, m) = (
            found.get(i).copied().unwrap_or(0),
            min.get(i).copied().unwrap_or(0),
        );
        if f != m {
            return f > m;
        }
    }
    true
}

/// A command that never opens a console window on Windows.
fn quiet_command(program: &Path) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

fn run_capture(
    program: &Path,
    args: &[String],
    dir: Option<&Path>,
    home: Option<&Path>,
) -> std::io::Result<(bool, String)> {
    let mut cmd = quiet_command(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    if let Some(h) = home {
        cmd.env("HOME", h)
            .env("USERPROFILE", h)
            .env("XDG_CONFIG_HOME", h.join(".config"))
            .env("APPDATA", h.join("AppData"));
    }
    // ETXTBSY ("Text file busy", os error 26) is transient on Linux when the binary was just
    // written or replaced (an upgrade, or a parallel fork still holding the old write handle).
    // Same bounded backoff as `ta-plugin`'s spawn; any other error returns at once.
    let mut out = cmd.output();
    for delay_ms in [20u64, 80, 200, 500] {
        match &out {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                out = cmd.output();
            }
            _ => break,
        }
    }
    let out = out?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

fn describe_failure(out: &str) -> String {
    let t: String = out.trim().chars().take(300).collect();
    if t.is_empty() {
        "the command exited non-zero and printed nothing".into()
    } else {
        t
    }
}

/// Look the binary up and judge version and edition. Runs `--version` and the edition probe;
/// never installs anything.
pub fn probe(spec: &HookSpec, name: &str) -> Result<HookProbe, HookRefusal> {
    if spec.binary.is_empty() {
        return Err(HookRefusal::NoSpec(name.to_string()));
    }
    let path = which::which(&spec.binary).map_err(|_| HookRefusal::BinaryMissing {
        binary: spec.binary.clone(),
    })?;
    let (_, vout) = run_capture(&path, &["--version".into()], None, None).map_err(|e| {
        HookRefusal::VersionUnreadable {
            binary: spec.binary.clone(),
            output: e.to_string(),
        }
    })?;
    let found = parse_version(&vout).ok_or_else(|| HookRefusal::VersionUnreadable {
        binary: spec.binary.clone(),
        output: vout.trim().chars().take(120).collect(),
    })?;
    let min = parse_version(&spec.min_version).unwrap_or_default();
    if !version_at_least(&found, &min) {
        return Err(HookRefusal::TooOld {
            binary: spec.binary.clone(),
            found: found
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join("."),
            min: spec.min_version.clone(),
        });
    }
    let version = found
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".");

    let probe_args = if spec.edition_probe.is_empty() {
        vec!["--help".to_string()]
    } else {
        spec.edition_probe.clone()
    };
    let (edition, detail) = match run_capture(&path, &probe_args, None, None) {
        Err(e) => (Edition::Unknown, format!("`--help` failed: {e}")),
        Ok((_, text)) => {
            let lower = text.to_ascii_lowercase();
            let has = |w: &str| {
                lower
                    .split(|c: char| !c.is_alphanumeric() && c != '-')
                    .any(|t| t == w.to_ascii_lowercase())
            };
            if spec.full_markers.iter().any(|m| has(m)) {
                (Edition::Full, String::new())
            } else if !spec.sanity_marker.is_empty() && has(&spec.sanity_marker) {
                (Edition::Oss, String::new())
            } else {
                (
                    Edition::Unknown,
                    format!(
                        "the probe output named neither a full-edition command nor `{}`",
                        spec.sanity_marker
                    ),
                )
            }
        }
    };
    let wanted_full = spec.edition.eq_ignore_ascii_case("full");
    let busl_allowed = match edition {
        Edition::Oss => false,
        _ if wanted_full => false,
        Edition::Full if spec.allow_busl => true,
        Edition::Unknown if spec.allow_busl => true,
        Edition::Full => {
            return Err(HookRefusal::FullEdition {
                binary: spec.binary.clone(),
            })
        }
        Edition::Unknown => {
            return Err(HookRefusal::EditionUnknown {
                binary: spec.binary.clone(),
                detail,
            })
        }
    };
    Ok(HookProbe {
        binary_path: path.display().to_string(),
        version,
        edition,
        busl_allowed,
    })
}

/// What a goal's metrics record says about the hook. `active` is true only when the hook was
/// installed and verified for this goal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookRecord {
    pub optimizer: String,
    pub active: bool,
    pub version: Option<String>,
    pub edition: Option<Edition>,
    pub licence: String,
    pub allow_busl: bool,
    /// `optimizer: rtk` or `optimizer: none (rtk hook not verified: <reason>)`.
    pub summary: String,
}

impl HookRecord {
    /// A record for a goal that runs with no optimizer, with `reason` as the explanation.
    pub fn none(name: &str, spec: &HookSpec, reason: &str) -> Self {
        Self {
            optimizer: "none".into(),
            active: false,
            version: None,
            edition: None,
            licence: spec.licence.clone(),
            allow_busl: false,
            summary: format!("optimizer: none ({name} hook not verified: {reason})"),
        }
    }

    fn inactive(name: &str, spec: &HookSpec, reason: &HookRefusal, p: Option<&HookProbe>) -> Self {
        Self {
            optimizer: "none".into(),
            active: false,
            version: p.map(|p| p.version.clone()),
            edition: p.map(|p| p.edition),
            licence: spec.licence.clone(),
            allow_busl: p.is_some_and(|p| p.busl_allowed),
            summary: format!("optimizer: none ({name} hook not verified: {reason})"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    goal_id: String,
    pid: u32,
    name: String,
    staging: String,
    binary: String,
    uninstall: Vec<String>,
    /// rel file -> content hashes before and after the install.
    entries: BTreeMap<String, FileState>,
    scratch_home: String,
}

/// Content hash of one touched file before the install and after it (`None` = absent).
/// `post_known` is false until the installer has finished, so a crash mid-install is still
/// recoverable.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileState {
    pre: Option<String>,
    post: Option<String>,
    post_known: bool,
}

/// Installs a spec's hook into one goal's staging copy.
pub struct HookInstaller<'a> {
    pub name: &'a str,
    pub spec: &'a HookSpec,
}

/// Guard for one goal's install. Dropping it restores the staging copy.
pub struct HookInstall {
    pub record: HookRecord,
    manifest_dir: Option<PathBuf>,
    staging: PathBuf,
    snap_dir: Option<PathBuf>,
    entries: BTreeMap<String, FileState>,
    scratch_home: Option<PathBuf>,
    uninstall: Option<(PathBuf, Vec<String>)>,
    /// Files the agent changed after the install; cleanup left them alone.
    pub left_modified: Vec<String>,
}

impl HookInstaller<'_> {
    /// Install for one goal. Never returns an error: a hook that cannot be installed or verified
    /// yields an inactive [`HookInstall`] whose `record.summary` says why and which already left
    /// the staging copy exactly as it found it.
    pub fn install(&self, project_root: &Path, staging: &Path, goal_id: &str) -> HookInstall {
        let inactive = |reason: HookRefusal, p: Option<&HookProbe>| HookInstall {
            record: HookRecord::inactive(self.name, self.spec, &reason, p),
            manifest_dir: None,
            staging: staging.to_path_buf(),
            snap_dir: None,
            entries: BTreeMap::new(),
            scratch_home: None,
            uninstall: None,
            left_modified: Vec::new(),
        };
        let probed = match probe(self.spec, self.name) {
            Ok(p) => p,
            Err(e) => return inactive(e, None),
        };
        for (field, empty) in [
            (
                "install",
                self.spec.install.is_empty() && self.spec.settings_hook.is_none(),
            ),
            ("verify", self.spec.verify.is_empty()),
        ] {
            if empty {
                return inactive(
                    HookRefusal::SpecIncomplete {
                        name: self.name.into(),
                        field,
                    },
                    Some(&probed),
                );
            }
        }
        let bin = PathBuf::from(&probed.binary_path);
        let mdir = project_root.join(INSTALLS_DIR).join(goal_id);
        let snap = mdir.join("snap");
        let scratch = mdir.join("home");
        let pre = scan(staging, &self.spec.touches);
        let write_manifest = |entries: &BTreeMap<String, FileState>| -> std::io::Result<()> {
            let manifest = Manifest {
                goal_id: goal_id.into(),
                pid: std::process::id(),
                name: self.name.into(),
                staging: staging.display().to_string(),
                binary: bin.display().to_string(),
                uninstall: self.spec.uninstall.clone(),
                entries: entries.clone(),
                scratch_home: scratch.display().to_string(),
            };
            std::fs::write(
                mdir.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).map_err(std::io::Error::other)?,
            )
        };
        let setup = (|| -> std::io::Result<BTreeMap<String, FileState>> {
            std::fs::create_dir_all(&snap)?;
            std::fs::create_dir_all(&scratch)?;
            let mut entries = BTreeMap::new();
            for (rel, hash) in &pre {
                copy_recursive(&staging.join(rel), &snap.join(rel))?;
                entries.insert(
                    rel.clone(),
                    FileState {
                        pre: Some(hash.clone()),
                        post: None,
                        post_known: false,
                    },
                );
            }
            write_manifest(&entries)?;
            Ok(entries)
        })();
        let entries = match setup {
            Ok(e) => e,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&mdir);
                return inactive(
                    HookRefusal::InstallFailed {
                        name: self.name.into(),
                        dir: mdir.display().to_string(),
                        detail: format!("cannot write the install manifest: {e}"),
                        cmd: "check permissions on .ta/compression".into(),
                    },
                    Some(&probed),
                );
            }
        };
        let mut guard = HookInstall {
            record: HookRecord {
                optimizer: self.name.into(),
                active: true,
                version: Some(probed.version.clone()),
                edition: Some(probed.edition),
                licence: self.spec.licence.clone(),
                allow_busl: probed.busl_allowed,
                summary: format!("optimizer: {} {}", self.name, probed.version),
            },
            manifest_dir: Some(mdir.clone()),
            staging: staging.to_path_buf(),
            snap_dir: Some(snap),
            entries,
            scratch_home: Some(scratch.clone()),
            uninstall: (!self.spec.uninstall.is_empty())
                .then(|| (bin.clone(), self.spec.uninstall.clone())),
            left_modified: Vec::new(),
        };
        let cmd_text = |a: &[String]| format!("{} {}", self.spec.binary, a.join(" "));
        let installed = match &self.spec.settings_hook {
            Some(h) => merge_settings_hook(staging, h)
                .map(|()| (true, String::new()))
                .or_else(|e| Ok::<_, std::io::Error>((false, e))),
            None => run_capture(&bin, &self.spec.install, Some(staging), Some(&scratch)),
        };
        let failure = match installed {
            Err(e) => Some(HookRefusal::InstallFailed {
                name: self.name.into(),
                dir: staging.display().to_string(),
                detail: e.to_string(),
                cmd: cmd_text(&self.spec.install),
            }),
            Ok((false, out)) => Some(HookRefusal::InstallFailed {
                name: self.name.into(),
                dir: staging.display().to_string(),
                detail: describe_failure(&out),
                cmd: cmd_text(&self.spec.install),
            }),
            Ok((true, _)) => {
                let leaked = list_files(&scratch);
                if !leaked.is_empty() {
                    Some(HookRefusal::EscapedProjectScope {
                        name: self.name.into(),
                        paths: leaked.join(", "),
                    })
                } else {
                    match run_capture(&bin, &self.spec.verify, Some(staging), Some(&scratch)) {
                        Ok((true, _)) => None,
                        Ok((false, out)) => Some(HookRefusal::NotVerified {
                            name: self.name.into(),
                            detail: describe_failure(&out),
                            cmd: cmd_text(&self.spec.verify),
                        }),
                        Err(e) => Some(HookRefusal::NotVerified {
                            name: self.name.into(),
                            detail: e.to_string(),
                            cmd: cmd_text(&self.spec.verify),
                        }),
                    }
                }
            }
        };
        // Record the post-install state so cleanup reverts only what the installer wrote.
        let post = scan(staging, &self.spec.touches);
        for rel in post.keys().chain(guard.entries.clone().keys()) {
            let st = guard.entries.entry(rel.clone()).or_insert(FileState {
                pre: None,
                post: None,
                post_known: false,
            });
            st.post = post.get(rel).cloned();
            st.post_known = true;
        }
        let _ = write_manifest(&guard.entries);
        if let Some(reason) = failure {
            guard.cleanup();
            guard.record = HookRecord::inactive(self.name, self.spec, &reason, Some(&probed));
        }
        guard
    }
}

impl HookInstall {
    pub fn active(&self) -> bool {
        self.record.active
    }

    /// Run the spec's uninstall (best effort), restore every touched path, drop the manifest.
    /// Safe to call twice.
    pub fn cleanup(&mut self) {
        if let Some((bin, args)) = self.uninstall.take() {
            // Best effort: the byte restore below is the guarantee, this only lets the tool tidy
            // state of its own that lives inside the touched paths.
            let _ = run_capture(
                &bin,
                &args,
                Some(&self.staging),
                self.scratch_home.as_deref(),
            );
        }
        if let Some(snap) = self.snap_dir.take() {
            self.left_modified = restore(&self.staging, &snap, &self.entries);
        }
        if let Some(d) = self.manifest_dir.take() {
            let _ = std::fs::remove_dir_all(d);
        }
        self.scratch_home = None;
    }
}

impl Drop for HookInstall {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Merge one hook entry into a settings file inside the staging copy, keeping everything already
/// there (other hooks, permissions, unrelated keys). Idempotent: an identical entry is not added
/// twice. The file is written through a temp file and a rename so a crash never leaves a torn
/// settings file. Returns a human-readable reason on failure.
fn merge_settings_hook(staging: &Path, h: &SettingsHook) -> Result<(), String> {
    for (name, value) in [
        ("file", &h.file),
        ("event", &h.event),
        ("matcher", &h.matcher),
        ("command", &h.command),
    ] {
        if value.trim().is_empty() {
            return Err(format!(
                "settings_hook.{name} is empty; set all of file, event, matcher and command under [optimizers.input.<name>.settings_hook]"
            ));
        }
    }
    let rel = Path::new(&h.file);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "settings_hook.file '{}' must be a path inside the staging copy (no absolute path, no '..')",
            h.file
        ));
    }
    let path = staging.join(&h.file);
    let mut root: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => serde_json::json!({}),
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            format!(
                "{} is not valid JSON ({e}); fix or remove it, TA will not overwrite it",
                path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("`hooks` in {} is not an object", path.display()))?;
    let event = hooks
        .entry(h.event.clone())
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| format!("`hooks.{}` in {} is not an array", h.event, path.display()))?;
    let entry = serde_json::json!({
        "matcher": h.matcher,
        "hooks": [{ "type": "command", "command": h.command }],
    });
    if !event.contains(&entry) {
        event.push(entry);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.ta-tmp");
    let body = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })
}

/// Revert each file the installer changed. A file whose current content is neither the
/// installer's output nor the original was edited by the agent afterwards and is left alone;
/// those paths are returned.
fn restore(staging: &Path, snap: &Path, entries: &BTreeMap<String, FileState>) -> Vec<String> {
    let mut kept = Vec::new();
    for (rel, st) in entries {
        let path = staging.join(rel);
        let cur = hash_file(&path);
        let ours = if st.post_known {
            cur == st.post
        } else {
            cur != st.pre
        };
        if cur == st.pre {
            if st.pre.is_none() {
                prune_empty_parents(staging, &path);
            }
            continue;
        }
        if !ours {
            kept.push(rel.clone());
            continue;
        }
        match st.pre {
            Some(_) => {
                let _ = copy_recursive(&snap.join(rel), &path);
            }
            None => {
                let _ = std::fs::remove_file(&path);
                prune_empty_parents(staging, &path);
            }
        }
    }
    kept
}

/// Remove directories the installer created and left empty, up to (not including) `staging`.
fn prune_empty_parents(staging: &Path, file: &Path) {
    let mut p = file.parent();
    while let Some(d) = p {
        if d == staging || std::fs::remove_dir(d).is_err() {
            break;
        }
        p = d.parent();
    }
}

fn hash_file(p: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(p).ok()?;
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

/// Hash every file under the declared paths (files or directories), keyed by staging-relative
/// path with `/` separators.
fn scan(staging: &Path, touches: &[String]) -> BTreeMap<String, String> {
    fn walk(staging: &Path, p: &Path, out: &mut BTreeMap<String, String>) {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    walk(staging, &e.path(), out);
                }
            }
        } else if let Some(h) = hash_file(p) {
            let rel = p.strip_prefix(staging).unwrap_or(p);
            out.insert(rel.to_string_lossy().replace('\\', "/"), h);
        }
    }
    let mut out = BTreeMap::new();
    for t in touches {
        walk(staging, &staging.join(t), &mut out);
    }
    out
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_recursive(&e.path(), &dst.join(e.file_name()))?;
        }
    } else {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

fn list_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(base: &Path, d: &Path, out: &mut Vec<String>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(base, &p, out);
                } else {
                    out.push(p.strip_prefix(base).unwrap_or(&p).display().to_string());
                }
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

/// Is `pid` a live process? Used so the sweep never touches an install a running TA still owns.
fn pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        let mut c = quiet_command(Path::new("tasklist"));
        c.args(["/FI", &format!("PID eq {pid}"), "/NH"]);
        c.output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

/// One orphan the sweep dealt with.
#[derive(Debug, Clone, PartialEq)]
pub struct SweptInstall {
    pub goal_id: String,
    pub staging: String,
    pub restored: usize,
}

/// Remove installs left behind by a crashed or killed TA. Installs whose owner process is still
/// alive are left alone. Returns what was cleaned so the caller can report it.
pub fn sweep_orphans(project_root: &Path) -> Vec<SweptInstall> {
    let mut swept = Vec::new();
    let Ok(rd) = std::fs::read_dir(project_root.join(INSTALLS_DIR)) else {
        return swept;
    };
    for e in rd.flatten() {
        let mdir = e.path();
        let Ok(raw) = std::fs::read(mdir.join("manifest.json")) else {
            continue;
        };
        let Ok(m) = serde_json::from_slice::<Manifest>(&raw) else {
            continue;
        };
        if pid_alive(m.pid) {
            continue;
        }
        let staging = PathBuf::from(&m.staging);
        if staging.is_dir() {
            if !m.uninstall.is_empty() {
                let _ = run_capture(
                    Path::new(&m.binary),
                    &m.uninstall,
                    Some(&staging),
                    Some(Path::new(&m.scratch_home)),
                );
            }
            restore(&staging, &mdir.join("snap"), &m.entries);
        }
        let _ = std::fs::remove_dir_all(&mdir);
        swept.push(SweptInstall {
            goal_id: m.goal_id,
            staging: m.staging,
            restored: m.entries.len(),
        });
    }
    swept
}

/// Metrics for a before/after pair TA actually saw (for example a tool result captured with and
/// without the hook). `lossless` is computed from the two texts, never assumed. The hook rewrites
/// what the model reads, so its output is untrusted until compared.
pub fn measure_pair(
    before: &str,
    after: &str,
    ms: u64,
    store: &OriginalStore,
) -> Result<OptMetrics, crate::OptError> {
    let original_ref = store.save(before)?;
    Ok(OptMetrics {
        bytes_in: before.len(),
        bytes_out: after.len(),
        est_tokens_in: estimate_tokens(before),
        est_tokens_out: estimate_tokens(after),
        ms,
        lossless: before == after,
        original_ref,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fake `rtk`: `--version`, `--help`, and an `init` verb with install, `--show` and
    /// `--uninstall`. `leaky` makes install write to `$HOME` instead of the project.
    fn fake_rtk(
        dir: &Path,
        version: &str,
        full: bool,
        leaky: bool,
        edition_blind: bool,
    ) -> PathBuf {
        let help = if edition_blind {
            "garbage output".to_string()
        } else if full {
            "Usage: rtk <cmd>\n  init\n  gain\n  pipeline\n  methods".to_string()
        } else {
            "Usage: rtk <cmd>\n  init\n  gain".to_string()
        };
        let install = if leaky {
            "mkdir -p \"$HOME/.claude\"; echo hook > \"$HOME/.claude/settings.json\""
        } else {
            "mkdir -p .claude/hooks; echo hook > .claude/hooks/rtk.sh; echo '{\"hooks\":1}' > .claude/settings.json; echo \"$HOME\" > seen_home"
        };
        let body = format!(
            r#"#!/bin/sh
case "$1" in
  --version) echo "rtk {version}";;
  --help) printf '%s\n' "{help}";;
  hook) shift
    case "$1" in
      check) case "$2" in "git status") echo "rtk git status";; *) echo "No rewrite for: $2"; exit 1;; esac;;
    esac;;
  init) shift
    case "$1" in
      --show) [ -f .claude/hooks/rtk.sh ];;
      --uninstall) rm -f .claude/hooks/rtk.sh;;
      *) {install};;
    esac;;
esac
"#
        );
        let p = dir.join("rtk");
        write_exec(&p, &body);
        p
    }

    /// Write an executable script so it can be run at once. Writing in place and then
    /// exec'ing can fail with ETXTBSY ("Text file busy") on Linux while the write handle is
    /// still open; writing a sibling file, closing it, and renaming it over the target means
    /// the final inode never has a writer.
    fn write_exec(path: &Path, body: &str) {
        use std::io::Write;
        let tmp = path.with_extension("tmp-write");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(body.as_bytes()).unwrap();
            f.sync_all().unwrap();
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    /// Command-path spec: the fake's `init` verb installs the hook.
    fn spec_for(bin: &Path) -> HookSpec {
        let mut s = HookSpec::rtk_oss();
        s.binary = bin.display().to_string();
        s.settings_hook = None;
        s.install = vec!["init".into(), "--auto-patch".into()];
        s.verify = vec!["init".into(), "--show".into()];
        s.uninstall = vec!["init".into(), "--uninstall".into()];
        s.touches = vec![".claude".into(), "CLAUDE.md".into()];
        s
    }

    /// Settings-entry spec: the real `rtk_oss()` default, pointed at the fake binary.
    fn spec_settings_for(bin: &Path) -> HookSpec {
        let mut s = HookSpec::rtk_oss();
        s.binary = bin.display().to_string();
        s
    }

    struct Fx {
        _t: tempfile::TempDir,
        root: PathBuf,
        staging: PathBuf,
        home: PathBuf,
        bin_dir: PathBuf,
    }

    fn fx() -> Fx {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("project");
        let staging = root.join(".ta/staging/g1");
        let home = t.path().join("home");
        let bin_dir = t.path().join("bin");
        for d in [&staging, &home.join(".claude"), &bin_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(home.join(".claude/settings.json"), "{\"user\":true}").unwrap();
        std::fs::write(staging.join("CLAUDE.md"), "# goal context\n").unwrap();
        std::fs::create_dir_all(staging.join(".claude")).unwrap();
        std::fs::write(staging.join(".claude/settings.local.json"), "{\"ta\":1}").unwrap();
        Fx {
            _t: t,
            root,
            staging,
            home,
            bin_dir,
        }
    }

    fn tree(dir: &Path) -> BTreeMap<String, String> {
        scan(dir, &[".".to_string()])
    }

    #[test]
    fn install_touches_only_staging_and_cleanup_restores_it_byte_for_byte() {
        let f = fx();
        let bin = fake_rtk(&f.bin_dir, "0.23.1", false, false, false);
        let spec = spec_for(&bin);
        let (home_before, staging_before) = (tree(&f.home), tree(&f.staging));
        let g = HookInstaller {
            name: "rtk",
            spec: &spec,
        }
        .install(&f.root, &f.staging, "g1");
        assert!(g.active(), "{}", g.record.summary);
        assert!(f.staging.join(".claude/hooks/rtk.sh").exists());
        assert_eq!(g.record.version.as_deref(), Some("0.23.1"));
        assert_eq!(g.record.edition, Some(Edition::Oss));
        assert!(!g.record.allow_busl);
        // The installer ran with a scratch home, never the fixture home.
        let seen = std::fs::read_to_string(f.staging.join("seen_home")).unwrap();
        assert!(
            seen.trim().contains(INSTALLS_DIR) && !seen.contains("home\n/"),
            "{seen}"
        );
        std::fs::remove_file(f.staging.join("seen_home")).unwrap();
        drop(g);
        assert_eq!(tree(&f.staging), staging_before, "staging restored");
        assert_eq!(tree(&f.home), home_before, "user config byte-identical");
        assert!(
            !f.root.join(INSTALLS_DIR).join("g1").exists(),
            "manifest removed"
        );
    }

    #[test]
    fn failed_install_and_failed_verify_leave_everything_unchanged_and_say_why() {
        let f = fx();
        let bin = fake_rtk(&f.bin_dir, "0.23.1", false, false, false);
        let before = tree(&f.staging);
        // verify can never pass
        let mut spec = spec_for(&bin);
        // A binary whose install runs but whose `init --show` reports the hook inactive.
        let verify_bin = f.bin_dir.join("verifyfail");
        write_exec(&verify_bin, "#!/bin/sh\ncase \"$1\" in --version) echo 'rtk 0.23.1';; --help) echo 'init';; init) shift; case \"$1\" in --show) exit 1;; *) mkdir -p .claude/hooks; echo x > .claude/hooks/rtk.sh;; esac;; esac\n");
        spec.binary = verify_bin.display().to_string();
        spec.verify = vec!["init".into(), "--show".into()];
        let g = HookInstaller {
            name: "rtk",
            spec: &spec,
        }
        .install(&f.root, &f.staging, "g1");
        assert!(!g.active());
        assert!(
            g.record
                .summary
                .starts_with("optimizer: none (rtk hook not verified:"),
            "{}",
            g.record.summary
        );
        assert_eq!(g.record.optimizer, "none");
        assert_eq!(tree(&f.staging), before);
    }

    #[test]
    fn installer_that_escapes_project_scope_is_refused_and_user_home_is_untouched() {
        let f = fx();
        let bin = fake_rtk(&f.bin_dir, "0.23.1", false, true, false);
        let spec = spec_for(&bin);
        let home_before = tree(&f.home);
        let before = tree(&f.staging);
        let g = HookInstaller {
            name: "rtk",
            spec: &spec,
        }
        .install(&f.root, &f.staging, "g1");
        assert!(!g.active());
        assert!(
            g.record.summary.contains("not project scope"),
            "{}",
            g.record.summary
        );
        assert_eq!(tree(&f.home), home_before);
        assert_eq!(tree(&f.staging), before);
    }

    #[test]
    fn leaked_guard_is_cleaned_by_the_orphan_sweep_after_a_simulated_crash() {
        let f = fx();
        let bin = fake_rtk(&f.bin_dir, "0.23.1", false, false, false);
        let spec = spec_for(&bin);
        let before = tree(&f.staging);
        let g = HookInstaller {
            name: "rtk",
            spec: &spec,
        }
        .install(&f.root, &f.staging, "g1");
        assert!(g.active(), "{}", g.record.summary);
        std::fs::remove_file(f.staging.join("seen_home")).unwrap();
        std::mem::forget(g); // SIGKILL / crash: Drop never runs
        assert!(f.staging.join(".claude/hooks/rtk.sh").exists());
        // The owner is this live test process, so a sweep must leave it alone.
        assert!(sweep_orphans(&f.root).is_empty());
        assert!(f.staging.join(".claude/hooks/rtk.sh").exists());
        // Make the owner a dead pid.
        let mut child = Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let mp = f.root.join(INSTALLS_DIR).join("g1/manifest.json");
        let mut m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mp).unwrap()).unwrap();
        m["pid"] = dead.into();
        std::fs::write(&mp, serde_json::to_vec(&m).unwrap()).unwrap();
        let swept = sweep_orphans(&f.root);
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].goal_id, "g1");
        assert_eq!(tree(&f.staging), before);
        assert!(!f.root.join(INSTALLS_DIR).join("g1").exists());
    }

    /// Linux returns ETXTBSY when exec'ing a file that still has a writer. A real `rtk` being
    /// upgraded mid-goal does exactly this, so the probe must wait it out instead of reporting
    /// the hook as unverifiable.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_binary_that_is_briefly_busy_is_retried_not_refused() {
        use std::io::Write;
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("rtk");
        let mut w = std::fs::File::create(&p).unwrap();
        w.write_all(b"#!/bin/sh\necho rtk 0.23.1\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        // `w` is still open for writing here, so exec fails with ETXTBSY until it is dropped.
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            drop(w);
        });
        let (ok, text) = run_capture(&p, &["--version".into()], None, None).unwrap();
        release.join().unwrap();
        assert!(ok, "{text}");
        assert!(text.contains("rtk 0.23.1"), "{text}");
    }

    #[test]
    fn cleanup_keeps_agent_edits_made_after_the_install() {
        let f = fx();
        let bin = fake_rtk(&f.bin_dir, "0.23.1", false, false, false);
        let spec = spec_for(&bin);
        let mut g = HookInstaller {
            name: "rtk",
            spec: &spec,
        }
        .install(&f.root, &f.staging, "g1");
        assert!(g.active(), "{}", g.record.summary);
        std::fs::write(f.staging.join("CLAUDE.md"), "# agent rewrote this\n").unwrap();
        g.cleanup();
        assert_eq!(
            std::fs::read_to_string(f.staging.join("CLAUDE.md")).unwrap(),
            "# agent rewrote this\n"
        );
        assert!(!f.staging.join(".claude/hooks/rtk.sh").exists());
    }

    #[test]
    fn full_edition_is_refused_unless_allow_busl_and_the_opt_in_is_recorded() {
        let f = fx();
        let bin = fake_rtk(&f.bin_dir, "0.23.1", true, false, false);
        let mut spec = spec_for(&bin);
        let err = probe(&spec, "rtk").unwrap_err().to_string();
        assert!(
            err.contains("full edition")
                && err.contains("rtk-oss")
                && err.contains("brew install rtk-ai/tap/rtk-oss"),
            "{err}"
        );
        spec.allow_busl = true;
        let p = probe(&spec, "rtk").unwrap();
        assert_eq!(p.edition, Edition::Full);
        assert!(p.busl_allowed);
        let g = HookInstaller {
            name: "rtk",
            spec: &spec,
        }
        .install(&f.root, &f.staging, "g1");
        assert!(g.active(), "{}", g.record.summary);
        assert!(g.record.allow_busl);
        assert_eq!(g.record.edition, Some(Edition::Full));
    }

    #[test]
    fn missing_old_and_unknown_edition_each_refuse_with_an_actionable_message() {
        let f = fx();
        let mut spec = HookSpec::rtk_oss();
        spec.binary = f.bin_dir.join("no-such-rtk").display().to_string();
        let e = probe(&spec, "rtk").unwrap_err().to_string();
        assert!(
            e.contains("not found on PATH") && e.contains("brew install rtk-ai/tap/rtk-oss"),
            "{e}"
        );

        let old = fake_rtk(&f.bin_dir, "0.22.9", false, false, false);
        let e = probe(&spec_for(&old), "rtk").unwrap_err().to_string();
        assert!(
            e.contains("0.22.9") && e.contains("0.23.0") && e.contains("upgrade"),
            "{e}"
        );

        let blind = fake_rtk(&f.bin_dir, "0.23.1", false, false, true);
        let e = probe(&spec_for(&blind), "rtk").unwrap_err().to_string();
        assert!(
            e.contains("unknown") && e.contains("refuses") && e.contains("rtk-oss"),
            "{e}"
        );
    }

    #[test]
    fn version_parsing_and_comparison() {
        assert_eq!(parse_version("rtk 0.23.1"), Some(vec![0, 23, 1]));
        assert_eq!(parse_version("v1.2.3-beta+x"), Some(vec![1, 2, 3]));
        assert_eq!(parse_version("no version"), None);
        assert!(version_at_least(&[0, 23], &[0, 23, 0]));
        assert!(!version_at_least(&[0, 22, 99], &[0, 23, 0]));
    }

    #[test]
    fn lossless_comes_from_real_content() {
        let d = tempfile::tempdir().unwrap();
        let store = OriginalStore::new(d.path());
        let same = measure_pair("abc", "abc", 1, &store).unwrap();
        assert!(same.lossless);
        let diff = measure_pair("abcdef", "abc", 1, &store).unwrap();
        assert!(!diff.lossless && diff.bytes_out < diff.bytes_in);
        assert_eq!(store.recover(&diff.original_ref).unwrap(), "abcdef");
    }

    /// The real rtk-oss default: the hook is one settings entry TA writes itself. Proven by hand
    /// against rtk-oss 1.0.1: `rtk init` at project scope does not install it, and `rtk init -g`
    /// would write to the user's ~/.claude, so TA must never call either.
    #[test]
    fn settings_entry_install_keeps_existing_hooks_and_restores_byte_for_byte() {
        let f = fx();
        let existing = r#"{"permissions":{"allow":["Bash(ls)"]},"hooks":{"PreToolUse":[{"matcher":"Edit","hooks":[{"type":"command","command":"fmt-check"}]}]}}"#;
        std::fs::write(f.staging.join(".claude/settings.json"), existing).unwrap();
        let bin = fake_rtk(&f.bin_dir, "1.0.1", false, false, false);
        let spec = spec_settings_for(&bin);
        let (home_before, staging_before) = (tree(&f.home), tree(&f.staging));
        let g = HookInstaller {
            name: "rtk",
            spec: &spec,
        }
        .install(&f.root, &f.staging, "g1");
        assert!(g.active(), "{}", g.record.summary);
        let after: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(f.staging.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let pre = after["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2, "the user's own hook survives: {after}");
        assert_eq!(pre[0]["hooks"][0]["command"], "fmt-check");
        assert_eq!(pre[1]["matcher"], "Bash");
        assert_eq!(pre[1]["hooks"][0]["command"], "rtk hook claude");
        assert_eq!(
            after["permissions"]["allow"][0], "Bash(ls)",
            "unrelated keys kept"
        );
        assert_eq!(tree(&f.home), home_before, "never touches the user's home");
        drop(g);
        assert_eq!(
            tree(&f.staging),
            staging_before,
            "staging restored byte-for-byte"
        );
        assert_eq!(tree(&f.home), home_before);
    }

    #[test]
    fn settings_entry_install_is_idempotent_and_creates_a_missing_file() {
        let f = fx();
        let h = HookSpec::rtk_oss().settings_hook.unwrap();
        let dir = tempfile::tempdir().unwrap();
        merge_settings_hook(dir.path(), &h).unwrap();
        merge_settings_hook(dir.path(), &h).unwrap();
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert!(!dir.path().join(".claude/settings.json.ta-tmp").exists());
        drop(f);
    }

    #[test]
    fn a_partial_settings_hook_table_overrides_only_what_it_names() {
        // The shape shown in docs/USAGE.md (a flat table plus a `settings_hook` sub-table), as the
        // JSON value daemon.toml becomes. This crate has no toml dependency and `hook_override`
        // deserialises through serde_json anyway.
        let cfg: crate::OptimizersConfig = serde_json::from_value(serde_json::json!({
            "input": {
                "use": "rtk",
                "rtk": {
                    "min_version": "9.9.9",
                    "settings_hook": { "command": "rtk hook claude --quiet" }
                }
            }
        }))
        .unwrap();
        let over = cfg
            .input
            .hook_override("rtk")
            .unwrap()
            .expect("table present");
        let mut spec = HookSpec::rtk_oss();
        spec.overlay(&over);
        assert_eq!(spec.min_version, "9.9.9");
        let h = spec.settings_hook.expect("settings hook kept");
        assert_eq!(h.command, "rtk hook claude --quiet");
        // Fields the table does not name keep the real defaults, not blanks.
        assert_eq!(h.file, ".claude/settings.json");
        assert_eq!(h.event, "PreToolUse");
        assert_eq!(h.matcher, "Bash");
        assert_eq!(spec.verify, ["hook", "check", "git status"]);
        assert_eq!(spec.binary, "rtk");
    }

    #[test]
    fn a_blank_or_escaping_settings_hook_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let good = HookSpec::rtk_oss().settings_hook.unwrap();
        for (label, bad) in [
            (
                "blank file",
                SettingsHook {
                    file: String::new(),
                    ..good.clone()
                },
            ),
            (
                "blank command",
                SettingsHook {
                    command: "  ".into(),
                    ..good.clone()
                },
            ),
            (
                "parent dir",
                SettingsHook {
                    file: "../outside.json".into(),
                    ..good.clone()
                },
            ),
            (
                "absolute",
                SettingsHook {
                    file: "/tmp/outside.json".into(),
                    ..good.clone()
                },
            ),
        ] {
            let err = merge_settings_hook(dir.path(), &bad).expect_err(label);
            assert!(err.contains("settings_hook"), "{label}: {err}");
        }
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "a refused hook must leave staging untouched"
        );
    }

    #[test]
    fn settings_entry_install_refuses_unparseable_settings_without_overwriting_them() {
        let f = fx();
        let broken = "{ this is not json";
        std::fs::write(f.staging.join(".claude/settings.json"), broken).unwrap();
        let bin = fake_rtk(&f.bin_dir, "1.0.1", false, false, false);
        let spec = spec_settings_for(&bin);
        let g = HookInstaller {
            name: "rtk",
            spec: &spec,
        }
        .install(&f.root, &f.staging, "g1");
        assert!(!g.active(), "must run unoptimised, not clobber the file");
        assert!(
            g.record.summary.contains("not valid JSON"),
            "{}",
            g.record.summary
        );
        assert_eq!(
            std::fs::read_to_string(f.staging.join(".claude/settings.json")).unwrap(),
            broken
        );
    }
}
