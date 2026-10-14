// linux_probe.rs — What can this Linux host actually sandbox? (v0.17.12.8)
//
// The Linux provider is two independent layers: bubblewrap (namespaces and a
// constructed root) and a TA-owned pre-exec helper that applies Landlock and a
// seccomp deny list. Either can be unavailable for reasons that have nothing to
// do with TA: AppArmor restricting unprivileged user namespaces (Ubuntu 23.10+),
// namespaces compiled out or sysctl'd off, a container whose seccomp profile
// blocks `unshare`, WSL2 kernels without Landlock, a Nix-built `bwrap`.
//
// Everything that interprets a kernel or tool answer is a pure function over
// text so it is tested on every platform. Only `probe()` touches the host.
// Classification never guesses: an unclassified failure is reported verbatim.

use std::path::PathBuf;

/// Why a `bwrap` that is installed cannot create the namespaces TA needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnusableReason {
    /// `kernel.apparmor_restrict_unprivileged_userns=1` and no profile lets this `bwrap` through.
    AppArmorRestricted,
    /// `user.max_user_namespaces=0` or `kernel.unprivileged_userns_clone=0`.
    UserNamespacesDisabled,
    /// A container runtime's seccomp or capability set blocks `unshare`/`mount`.
    ContainerRestricted,
    /// None of the above matched; the verbatim `bwrap` message.
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BwrapStatus {
    /// `bwrap` ran a namespaced no-op successfully with the flags TA uses.
    Usable {
        path: PathBuf,
        version: String,
    },
    NotInstalled,
    Unusable {
        path: PathBuf,
        reason: UnusableReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    Docker,
    Podman,
    Kubernetes,
    Lxc,
    Other,
}

impl ContainerKind {
    pub fn name(self) -> &'static str {
        match self {
            ContainerKind::Docker => "Docker",
            ContainerKind::Podman => "Podman",
            ContainerKind::Kubernetes => "Kubernetes",
            ContainerKind::Lxc => "LXC",
            ContainerKind::Other => "a container",
        }
    }
}

/// Facts about the surrounding environment, used only to explain a failure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostEnvironment {
    pub container: Option<ContainerKind>,
    pub wsl2: bool,
    /// `bwrap` lives in the Nix store, or `/nix` exists.
    pub nix: bool,
}

/// Raw sysctl readings (`None` = file absent, which means "not restricted by this knob").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsernsSysctls {
    pub max_user_namespaces: Option<i64>,
    pub unprivileged_userns_clone: Option<i64>,
    pub apparmor_restrict_unprivileged_userns: Option<i64>,
}

/// Landlock availability. `abi` is the highest ABI the running kernel reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandlockStatus {
    Available {
        abi: u32,
    },
    /// The `landlock_create_ruleset` syscall failed; the errno text explains why.
    Unavailable {
        reason: String,
    },
}

impl LandlockStatus {
    pub fn abi(&self) -> Option<u32> {
        match self {
            LandlockStatus::Available { abi } => Some(*abi),
            LandlockStatus::Unavailable { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxProbe {
    pub bwrap: BwrapStatus,
    pub landlock: LandlockStatus,
    /// seccomp-bpf filter support for this CPU architecture and kernel.
    pub seccomp: bool,
    pub env: HostEnvironment,
    pub sysctls: UsernsSysctls,
    /// The pre-exec helper TA launches inside the namespace, when it can be found.
    pub helper: Option<PathBuf>,
}

/// Which provider the probe supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// bubblewrap plus the helper (Landlock when the kernel has it, always seccomp).
    Bwrap,
    /// No namespaces: Landlock and seccomp through the helper only.
    LandlockOnly,
    None,
}

// ── Pure parsers ───────────────────────────────────────────────────────────

/// Parse a sysctl file's contents (`"0\n"`, `"1"`), `None` for anything else.
pub fn parse_sysctl(text: &str) -> Option<i64> {
    text.trim().parse::<i64>().ok()
}

/// `true` when unprivileged user namespaces are blocked at the kernel level
/// (not by AppArmor).
pub fn userns_disabled(s: &UsernsSysctls) -> bool {
    s.max_user_namespaces == Some(0) || s.unprivileged_userns_clone == Some(0)
}

pub fn apparmor_restricts_userns(s: &UsernsSysctls) -> bool {
    s.apparmor_restrict_unprivileged_userns == Some(1)
}

/// Container runtime from the usual markers.
pub fn detect_container(
    dockerenv_exists: bool,
    containerenv_exists: bool,
    cgroup_text: &str,
    container_env: Option<&str>,
) -> Option<ContainerKind> {
    if containerenv_exists {
        return Some(ContainerKind::Podman);
    }
    if dockerenv_exists {
        return Some(ContainerKind::Docker);
    }
    if let Some(v) = container_env {
        let v = v.to_ascii_lowercase();
        return Some(match v.as_str() {
            "podman" => ContainerKind::Podman,
            "docker" => ContainerKind::Docker,
            "lxc" | "lxc-libvirt" => ContainerKind::Lxc,
            _ => ContainerKind::Other,
        });
    }
    let c = cgroup_text;
    if c.contains("kubepods") {
        Some(ContainerKind::Kubernetes)
    } else if c.contains("/docker/") || c.contains("docker-") {
        Some(ContainerKind::Docker)
    } else if c.contains("libpod") {
        Some(ContainerKind::Podman)
    } else if c.contains("/lxc/") {
        Some(ContainerKind::Lxc)
    } else {
        None
    }
}

/// WSL2 kernels identify themselves in `/proc/version` or `osrelease`.
pub fn is_wsl2(proc_version: &str) -> bool {
    let v = proc_version.to_ascii_lowercase();
    v.contains("microsoft-standard") || v.contains("wsl2") || v.contains("microsoft")
}

/// Turn a failed `bwrap` run into a reason. `stderr` is bwrap's own message.
pub fn classify_bwrap_failure(
    stderr: &str,
    sysctls: &UsernsSysctls,
    env: &HostEnvironment,
) -> UnusableReason {
    let s = stderr.to_ascii_lowercase();
    // The three messages bubblewrap prints when it cannot make a namespace.
    let ns_refused = s.contains("no permissions to create new namespace")
        || s.contains("creating new namespace failed")
        || s.contains("setting up uid map")
        || s.contains("unshare failed")
        || s.contains("operation not permitted")
        || s.contains("permission denied");
    if !ns_refused {
        return UnusableReason::Other(stderr.trim().to_string());
    }
    // Kernel knobs first: they are definitive and have a specific fix.
    if userns_disabled(sysctls) {
        UnusableReason::UserNamespacesDisabled
    } else if apparmor_restricts_userns(sysctls) {
        UnusableReason::AppArmorRestricted
    } else if env.container.is_some() {
        UnusableReason::ContainerRestricted
    } else {
        UnusableReason::Other(stderr.trim().to_string())
    }
}

impl UnusableReason {
    /// What happened and what to do, for the launch banner.
    pub fn explain(&self, env: &HostEnvironment, bwrap_path: &str) -> String {
        match self {
            UnusableReason::AppArmorRestricted => format!(
                "AppArmor restricts unprivileged user namespaces on this host \
                 (kernel.apparmor_restrict_unprivileged_userns=1), so {bwrap_path} cannot start. \
                 Fix: install the distribution's bubblewrap package (it ships an AppArmor \
                 profile that allows it) instead of a vendored or Nix-built binary, or add a \
                 profile for {bwrap_path}, or as an administrator run \
                 `sysctl kernel.apparmor_restrict_unprivileged_userns=0`."
            ),
            UnusableReason::UserNamespacesDisabled => format!(
                "unprivileged user namespaces are disabled on this kernel (user.max_user_namespaces \
                 is 0 or kernel.unprivileged_userns_clone is 0), so {bwrap_path} cannot start. \
                 Fix: as an administrator enable them (for example \
                 `sysctl user.max_user_namespaces=15000` and \
                 `sysctl kernel.unprivileged_userns_clone=1`), or accept the Landlock-only \
                 degraded sandbox."
            ),
            UnusableReason::ContainerRestricted => format!(
                "this process runs inside {} whose seccomp profile or capability set blocks the \
                 namespaces {bwrap_path} needs. Fix: start the container with a profile that \
                 allows unshare and mount (for example Docker's \
                 `--security-opt seccomp=unconfined` for a trusted CI container), or run TA on \
                 the host; Landlock-only isolation is used meanwhile.",
                env.container.map(|c| c.name()).unwrap_or("a container")
            ),
            UnusableReason::Other(msg) => format!(
                "{bwrap_path} is installed but failed a namespace self-test: {msg}. Run \
                 `{bwrap_path} --unshare-user --unshare-pid --ro-bind / / true` by hand to \
                 reproduce."
            ),
        }
    }
}

impl LinuxProbe {
    /// The provider this host supports. `bwrap` needs the helper too: namespaces
    /// without the seccomp and Landlock layer is not the hardened profile.
    pub fn selection(&self) -> Selection {
        if self.helper.is_none() || !self.seccomp {
            return Selection::None;
        }
        match (&self.bwrap, &self.landlock) {
            (BwrapStatus::Usable { .. }, _) => Selection::Bwrap,
            (_, LandlockStatus::Available { .. }) => Selection::LandlockOnly,
            _ => Selection::None,
        }
    }

    /// Human-readable findings that explain a missing or degraded layer. Empty
    /// when everything is available. Each line says what to do.
    pub fn findings(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.bwrap {
            BwrapStatus::Usable { .. } => {}
            BwrapStatus::NotInstalled => out.push(
                "bubblewrap (`bwrap`) is not installed: no namespace isolation. Install it \
                 (apt install bubblewrap, dnf install bubblewrap, pacman -S bubblewrap, or \
                 nix-env -iA nixpkgs.bubblewrap)."
                    .to_string(),
            ),
            BwrapStatus::Unusable { path, reason } => {
                out.push(reason.explain(&self.env, &path.display().to_string()))
            }
        }
        match &self.landlock {
            LandlockStatus::Available { .. } => {}
            LandlockStatus::Unavailable { reason } => out.push(format!(
                "Landlock is unavailable ({reason}). It needs Linux 5.13 or newer with the \
                 landlock LSM enabled (check `cat /sys/kernel/security/lsm`){}",
                if self.env.wsl2 {
                    "; many WSL2 kernels build without it, update with `wsl --update`"
                } else if self.env.container.is_some() {
                    "; the container runtime's seccomp profile may block the landlock_* syscalls"
                } else {
                    ""
                }
            )),
        }
        if !self.seccomp {
            out.push(
                "seccomp filtering is not supported on this CPU architecture or kernel; the \
                 hardened Linux profile is unavailable."
                    .to_string(),
            );
        }
        if self.helper.is_none() {
            out.push(
                "the sandbox helper (`ta-sandbox-exec`, or the `ta` binary's \
                 `sandbox-exec-helper` command) was not found next to this executable. \
                 Reinstall TA, or set TA_SANDBOX_EXEC to its path."
                    .to_string(),
            );
        }
        if self.env.wsl2 {
            out.push(
                "WSL2 detected: Windows interop and /mnt/c are not mounted into the sandbox, \
                 but they stay reachable if you run without it. For a dedicated distro set \
                 `[interop] enabled=false` and `[automount] enabled=false` in /etc/wsl.conf."
                    .to_string(),
            );
        }
        out
    }

    /// One line per layer for `ta sandbox` style status output.
    pub fn summary(&self) -> String {
        let bwrap = match &self.bwrap {
            BwrapStatus::Usable { version, .. } => format!("usable ({version})"),
            BwrapStatus::NotInstalled => "not installed".to_string(),
            BwrapStatus::Unusable { reason, .. } => format!(
                "unusable ({})",
                match reason {
                    UnusableReason::AppArmorRestricted => "AppArmor restriction",
                    UnusableReason::UserNamespacesDisabled => "user namespaces disabled",
                    UnusableReason::ContainerRestricted => "container restriction",
                    UnusableReason::Other(_) => "self-test failed",
                }
            ),
        };
        let landlock = match &self.landlock {
            LandlockStatus::Available { abi } => format!("ABI {abi}"),
            LandlockStatus::Unavailable { .. } => "unavailable".to_string(),
        };
        format!(
            "bubblewrap: {bwrap}; landlock: {landlock}; seccomp: {}; helper: {}",
            if self.seccomp { "yes" } else { "no" },
            if self.helper.is_some() {
                "found"
            } else {
                "missing"
            }
        )
    }
}

// ── The helper program ─────────────────────────────────────────────────────

/// How to start the in-namespace helper: a program and the arguments that
/// select helper mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperCommand {
    pub program: PathBuf,
    pub prefix_args: Vec<String>,
}

pub const HELPER_BIN_NAME: &str = "ta-sandbox-exec";
pub const HELPER_SUBCOMMAND: &str = "sandbox-exec-helper";
pub const HELPER_ENV: &str = "TA_SANDBOX_EXEC";

/// Locate the helper. Order: `TA_SANDBOX_EXEC` (a program that speaks the helper
/// protocol directly), a `ta-sandbox-exec` next to this executable (or one level
/// up, for cargo's `deps/`), then this executable itself when it is `ta`.
pub fn find_helper() -> Option<HelperCommand> {
    if let Some(p) = std::env::var_os(HELPER_ENV).map(PathBuf::from) {
        return p.is_file().then_some(HelperCommand {
            program: p,
            prefix_args: Vec::new(),
        });
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for d in [Some(dir), dir.parent()].into_iter().flatten() {
        let cand = d.join(HELPER_BIN_NAME);
        if cand.is_file() {
            return Some(HelperCommand {
                program: cand,
                prefix_args: Vec::new(),
            });
        }
    }
    let stem = exe.file_stem()?.to_string_lossy().to_string();
    (stem == "ta").then(|| HelperCommand {
        program: exe,
        prefix_args: vec![HELPER_SUBCOMMAND.to_string()],
    })
}

// ── The host probe ─────────────────────────────────────────────────────────

/// The process-wide probe, computed once (it runs `bwrap`).
pub fn cached() -> &'static LinuxProbe {
    static PROBE: std::sync::OnceLock<LinuxProbe> = std::sync::OnceLock::new();
    PROBE.get_or_init(probe)
}

/// The findings to show next to a launch, only when the layer set is degraded.
pub fn provider_note() -> Option<String> {
    let p = cached();
    let f = p.findings();
    if f.is_empty() {
        None
    } else {
        Some(format!(
            "Linux sandbox layers: {}. {}",
            p.summary(),
            f.join(" ")
        ))
    }
}

#[cfg(target_os = "linux")]
pub fn probe() -> LinuxProbe {
    use std::path::Path;
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let sysctls = UsernsSysctls {
        max_user_namespaces: parse_sysctl(&read("/proc/sys/user/max_user_namespaces")),
        unprivileged_userns_clone: parse_sysctl(&read(
            "/proc/sys/kernel/unprivileged_userns_clone",
        )),
        apparmor_restrict_unprivileged_userns: parse_sysctl(&read(
            "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
        )),
    };
    let bwrap_path = which::which("bwrap").ok();
    let mut env = HostEnvironment {
        container: detect_container(
            Path::new("/.dockerenv").exists(),
            Path::new("/run/.containerenv").exists(),
            &read("/proc/1/cgroup"),
            std::env::var("container").ok().as_deref(),
        ),
        wsl2: is_wsl2(&read("/proc/version")),
        nix: Path::new("/nix").exists(),
    };
    let bwrap = match bwrap_path {
        None => BwrapStatus::NotInstalled,
        Some(path) => {
            env.nix = env.nix || path.starts_with("/nix");
            let version = std::process::Command::new(&path)
                .arg("--version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            // The self-test uses the namespace flags the launch uses, so a host
            // that can run it can run the sandbox.
            let out = std::process::Command::new(&path)
                .args([
                    "--unshare-user",
                    "--unshare-pid",
                    "--unshare-ipc",
                    "--unshare-uts",
                    "--unshare-net",
                    "--die-with-parent",
                    "--cap-drop",
                    "ALL",
                    "--ro-bind",
                    "/",
                    "/",
                    "--dev",
                    "/dev",
                    "--proc",
                    "/proc",
                    "--tmpfs",
                    "/tmp",
                    "--",
                    "true",
                ])
                .output();
            match out {
                Ok(o) if o.status.success() => BwrapStatus::Usable { path, version },
                Ok(o) => BwrapStatus::Unusable {
                    reason: classify_bwrap_failure(
                        &String::from_utf8_lossy(&o.stderr),
                        &sysctls,
                        &env,
                    ),
                    path,
                },
                Err(e) => BwrapStatus::Unusable {
                    reason: UnusableReason::Other(e.to_string()),
                    path,
                },
            }
        }
    };
    LinuxProbe {
        bwrap,
        landlock: crate::linux_confine::landlock_status(),
        seccomp: crate::linux_confine::seccomp_supported(),
        env,
        sysctls,
        helper: find_helper().map(|h| h.program),
    }
}

/// Not Linux: nothing is available. `selection()` is `None`.
#[cfg(not(target_os = "linux"))]
pub fn probe() -> LinuxProbe {
    LinuxProbe {
        bwrap: BwrapStatus::NotInstalled,
        landlock: LandlockStatus::Unavailable {
            reason: "not a Linux host".to_string(),
        },
        seccomp: false,
        env: HostEnvironment::default(),
        sysctls: UsernsSysctls::default(),
        helper: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc(max: Option<i64>, clone: Option<i64>, aa: Option<i64>) -> UsernsSysctls {
        UsernsSysctls {
            max_user_namespaces: max,
            unprivileged_userns_clone: clone,
            apparmor_restrict_unprivileged_userns: aa,
        }
    }

    #[test]
    fn sysctl_parsing() {
        assert_eq!(parse_sysctl("0\n"), Some(0));
        assert_eq!(parse_sysctl(" 15000 "), Some(15000));
        assert_eq!(parse_sysctl(""), None);
        assert_eq!(parse_sysctl("n/a"), None);
    }

    #[test]
    fn userns_knobs() {
        assert!(userns_disabled(&sc(Some(0), None, None)));
        assert!(userns_disabled(&sc(Some(100), Some(0), None)));
        assert!(!userns_disabled(&sc(Some(100), Some(1), None)));
        assert!(!userns_disabled(&sc(None, None, None)));
        assert!(apparmor_restricts_userns(&sc(None, None, Some(1))));
        assert!(!apparmor_restricts_userns(&sc(None, None, Some(0))));
    }

    #[test]
    fn container_detection() {
        assert_eq!(
            detect_container(true, false, "", None),
            Some(ContainerKind::Docker)
        );
        assert_eq!(
            detect_container(false, true, "", None),
            Some(ContainerKind::Podman)
        );
        assert_eq!(
            detect_container(false, false, "0::/kubepods/burstable/pod1", None),
            Some(ContainerKind::Kubernetes)
        );
        assert_eq!(
            detect_container(false, false, "12:cpu:/docker/abc", None),
            Some(ContainerKind::Docker)
        );
        assert_eq!(
            detect_container(false, false, "0::/", Some("lxc")),
            Some(ContainerKind::Lxc)
        );
        assert_eq!(detect_container(false, false, "0::/user.slice", None), None);
    }

    #[test]
    fn wsl2_detection() {
        assert!(is_wsl2(
            "Linux version 5.15.153.1-microsoft-standard-WSL2 (root@..)"
        ));
        assert!(!is_wsl2("Linux version 6.8.0-45-generic (buildd@lcy02)"));
    }

    #[test]
    fn bwrap_failure_classification_prefers_the_definitive_knob() {
        let msg = "bwrap: No permissions to create new namespace, likely because the kernel does \
                   not allow non-privileged user namespaces.";
        let env = HostEnvironment::default();
        assert_eq!(
            classify_bwrap_failure(msg, &sc(Some(0), None, None), &env),
            UnusableReason::UserNamespacesDisabled
        );
        assert_eq!(
            classify_bwrap_failure(msg, &sc(Some(100), None, Some(1)), &env),
            UnusableReason::AppArmorRestricted
        );
        // The Ubuntu 24.04 message when AppArmor blocks the uid map write.
        assert_eq!(
            classify_bwrap_failure(
                "bwrap: setting up uid map: Permission denied",
                &sc(Some(100), None, Some(1)),
                &env
            ),
            UnusableReason::AppArmorRestricted
        );
        let docker = HostEnvironment {
            container: Some(ContainerKind::Docker),
            ..Default::default()
        };
        assert_eq!(
            classify_bwrap_failure(msg, &sc(Some(100), None, Some(0)), &docker),
            UnusableReason::ContainerRestricted
        );
    }

    #[test]
    fn unknown_bwrap_failure_is_reported_verbatim() {
        let r = classify_bwrap_failure(
            "bwrap: execvp true: No such file or directory\n",
            &UsernsSysctls::default(),
            &HostEnvironment::default(),
        );
        assert_eq!(
            r,
            UnusableReason::Other("bwrap: execvp true: No such file or directory".to_string())
        );
        // A namespace refusal nothing explains is also verbatim, not a guess.
        let r2 = classify_bwrap_failure(
            "bwrap: Creating new namespace failed: Operation not permitted",
            &sc(Some(100), Some(1), Some(0)),
            &HostEnvironment::default(),
        );
        assert!(matches!(r2, UnusableReason::Other(_)));
    }

    #[test]
    fn every_unusable_reason_explains_a_fix() {
        let env = HostEnvironment::default();
        for r in [
            UnusableReason::AppArmorRestricted,
            UnusableReason::UserNamespacesDisabled,
            UnusableReason::ContainerRestricted,
            UnusableReason::Other("boom".into()),
        ] {
            let text = r.explain(&env, "/usr/bin/bwrap");
            assert!(text.contains("/usr/bin/bwrap"), "{text}");
            assert!(
                text.contains("Fix") || text.contains("reproduce"),
                "no actionable step: {text}"
            );
        }
    }

    fn probe_with(bwrap: BwrapStatus, landlock: LandlockStatus) -> LinuxProbe {
        LinuxProbe {
            bwrap,
            landlock,
            seccomp: true,
            env: HostEnvironment::default(),
            sysctls: UsernsSysctls::default(),
            helper: Some(PathBuf::from("/opt/ta/ta-sandbox-exec")),
        }
    }

    fn usable() -> BwrapStatus {
        BwrapStatus::Usable {
            path: "/usr/bin/bwrap".into(),
            version: "bubblewrap 0.9.0".into(),
        }
    }

    #[test]
    fn selection_prefers_bwrap_then_landlock_only_then_none() {
        let ll = LandlockStatus::Available { abi: 4 };
        let none = LandlockStatus::Unavailable {
            reason: "ENOSYS".into(),
        };
        assert_eq!(
            probe_with(usable(), ll.clone()).selection(),
            Selection::Bwrap
        );
        // bwrap without Landlock is still the hardened profile (namespaces plus seccomp).
        assert_eq!(
            probe_with(usable(), none.clone()).selection(),
            Selection::Bwrap
        );
        assert_eq!(
            probe_with(BwrapStatus::NotInstalled, ll.clone()).selection(),
            Selection::LandlockOnly
        );
        assert_eq!(
            probe_with(BwrapStatus::NotInstalled, none).selection(),
            Selection::None
        );
        let mut no_helper = probe_with(usable(), ll.clone());
        no_helper.helper = None;
        assert_eq!(no_helper.selection(), Selection::None);
        let mut no_seccomp = probe_with(usable(), ll);
        no_seccomp.seccomp = false;
        assert_eq!(no_seccomp.selection(), Selection::None);
    }

    #[test]
    fn findings_are_empty_when_everything_works_and_actionable_otherwise() {
        let ok = probe_with(usable(), LandlockStatus::Available { abi: 4 });
        assert!(ok.findings().is_empty());
        let mut bad = probe_with(
            BwrapStatus::Unusable {
                path: "/usr/bin/bwrap".into(),
                reason: UnusableReason::AppArmorRestricted,
            },
            LandlockStatus::Unavailable {
                reason: "ENOSYS".into(),
            },
        );
        bad.env.wsl2 = true;
        let f = bad.findings().join("\n");
        assert!(f.contains("AppArmor"), "{f}");
        assert!(f.contains("Landlock is unavailable"), "{f}");
        assert!(f.contains("wsl --update"), "{f}");
        assert!(f.contains("interop"), "{f}");
        assert!(bad.summary().contains("AppArmor restriction"));
    }

    #[test]
    fn missing_bwrap_and_helper_each_say_what_to_install() {
        let mut p = probe_with(
            BwrapStatus::NotInstalled,
            LandlockStatus::Available { abi: 1 },
        );
        p.helper = None;
        let f = p.findings().join("\n");
        assert!(f.contains("apt install bubblewrap"), "{f}");
        assert!(f.contains("TA_SANDBOX_EXEC"), "{f}");
    }

    #[test]
    fn helper_env_override_requires_an_existing_file() {
        // Process-global env: use a name only this test touches the value of.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("helper");
        std::fs::write(&f, "x").unwrap();
        std::env::set_var(HELPER_ENV, &f);
        let h = find_helper().unwrap();
        assert_eq!(h.program, f);
        assert!(h.prefix_args.is_empty());
        std::env::set_var(HELPER_ENV, dir.path().join("missing"));
        assert!(find_helper().is_none(), "a bad override must not fall back");
        std::env::remove_var(HELPER_ENV);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_hosts_select_nothing() {
        assert_eq!(probe().selection(), Selection::None);
    }
}
