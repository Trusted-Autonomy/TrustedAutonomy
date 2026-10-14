// linux_confine.rs — Landlock and seccomp, applied by a pre-exec helper (v0.17.12.8)
//
// `ta-sandbox-exec` (or `ta sandbox-exec-helper`) runs INSIDE the bubblewrap
// namespaces, applies the two kernel layers below to itself, then execs the
// agent. It is trusted code started by the launcher, never by the agent.
//
//   Landlock  An allow-list of filesystem hierarchies (and, with ABI 4, TCP
//             ports), inherited by every descendant and impossible to widen.
//             The access set is derived from the kernel's reported ABI. A
//             required layer that cannot be enforced is an error, never a
//             silent downgrade; the one exception (`auto`, used under bwrap
//             where the mount namespace already confines the filesystem) says
//             so on stderr.
//   seccomp   A deny list: ptrace, process_vm_*, bpf, keyctl, io_uring,
//             nested user namespaces, TIOCSTI, mount and module syscalls.
//             Denied calls return EPERM (or are logged in diagnostic mode).
//
// Everything that decides WHAT to apply (access masks, the BPF program, the
// argument syntax) is platform-neutral and unit tested everywhere, including a
// small BPF interpreter that checks the generated filter allows and denies the
// right syscalls. Only the thin syscall wrappers are Linux-only.

use std::path::PathBuf;

// ── Landlock constants ─────────────────────────────────────────────────────

pub mod landlock {
    pub const FS_EXECUTE: u64 = 1 << 0;
    pub const FS_WRITE_FILE: u64 = 1 << 1;
    pub const FS_READ_FILE: u64 = 1 << 2;
    pub const FS_READ_DIR: u64 = 1 << 3;
    pub const FS_REMOVE_DIR: u64 = 1 << 4;
    pub const FS_REMOVE_FILE: u64 = 1 << 5;
    pub const FS_MAKE_CHAR: u64 = 1 << 6;
    pub const FS_MAKE_DIR: u64 = 1 << 7;
    pub const FS_MAKE_REG: u64 = 1 << 8;
    pub const FS_MAKE_SOCK: u64 = 1 << 9;
    pub const FS_MAKE_FIFO: u64 = 1 << 10;
    pub const FS_MAKE_BLOCK: u64 = 1 << 11;
    pub const FS_MAKE_SYM: u64 = 1 << 12;
    /// ABI 2 (Linux 5.19).
    pub const FS_REFER: u64 = 1 << 13;
    /// ABI 3 (Linux 6.2).
    pub const FS_TRUNCATE: u64 = 1 << 14;

    /// ABI 4 (Linux 6.7).
    pub const NET_BIND_TCP: u64 = 1 << 0;
    pub const NET_CONNECT_TCP: u64 = 1 << 1;

    /// ABI 6 (Linux 6.12).
    pub const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
    pub const SCOPE_SIGNAL: u64 = 1 << 1;

    /// Rights that make sense on a file rather than a directory.
    pub const FILE_RIGHTS: u64 = FS_EXECUTE | FS_WRITE_FILE | FS_READ_FILE | FS_TRUNCATE;

    /// Filesystem rights this ABI can handle. `LANDLOCK_ACCESS_FS_IOCTL_DEV`
    /// (ABI 5) is deliberately not handled: it would break ioctls on the tty
    /// and other devices the agent needs, and TIOCSTI is closed by seccomp.
    pub fn fs_handled(abi: u32) -> u64 {
        let mut m = (1u64 << 13) - 1; // bits 0..=12, ABI 1
        if abi >= 2 {
            m |= FS_REFER;
        }
        if abi >= 3 {
            m |= FS_TRUNCATE;
        }
        m
    }

    pub fn net_handled(abi: u32, restrict_net: bool) -> u64 {
        if restrict_net && abi >= 4 {
            NET_BIND_TCP | NET_CONNECT_TCP
        } else {
            0
        }
    }

    pub fn scoped(abi: u32) -> u64 {
        if abi >= 6 {
            SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL
        } else {
            0
        }
    }

    /// `landlock_ruleset_attr` grows with the ABI; older kernels accept only
    /// the size they know.
    pub fn attr_size(abi: u32) -> usize {
        match abi {
            0..=3 => 8,
            4..=5 => 16,
            _ => 24,
        }
    }

    pub fn ro_access(handled: u64) -> u64 {
        handled & (FS_EXECUTE | FS_READ_FILE | FS_READ_DIR)
    }

    pub fn rw_access(handled: u64) -> u64 {
        handled
    }

    /// Device directories (`/dev`): open, read, write and list, but never
    /// create, remove or rename. `/dev/null`, `/dev/urandom`, `/dev/tty` and
    /// `/dev/pts/*` need no more.
    pub fn dev_access(handled: u64) -> u64 {
        handled & (FS_READ_FILE | FS_WRITE_FILE | FS_READ_DIR | FS_TRUNCATE)
    }

    /// A rule on a non-directory may carry only file rights, or the kernel
    /// rejects it with EINVAL.
    pub fn for_target(access: u64, is_dir: bool) -> u64 {
        if is_dir {
            access
        } else {
            access & FILE_RIGHTS
        }
    }
}

// ── seccomp ────────────────────────────────────────────────────────────────

pub const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
pub const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
pub const SECCOMP_RET_LOG: u32 = 0x7ffc_0000;
pub const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const EPERM: u32 = 1;
const ENOSYS: u32 = 38;
pub const CLONE_NEWUSER: u32 = 0x1000_0000;
pub const TIOCSTI: u32 = 0x5412;

/// One classic BPF instruction (`struct sock_filter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JGE_K: u16 = 0x35;
const BPF_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;

// `struct seccomp_data` offsets.
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const fn off_arg(i: u32) -> u32 {
    16 + 8 * i
}

/// Per-architecture syscall numbers. Numbers are from the kernel's syscall
/// tables; the unit tests pin them so a typo cannot ship.
pub struct ArchTable {
    pub name: &'static str,
    pub audit_arch: u32,
    /// x86_64 only: syscall numbers with bit 30 set are the x32 ABI.
    pub reject_x32: bool,
    pub unshare: u32,
    pub clone: u32,
    pub clone3: u32,
    pub ioctl: u32,
    /// Denied outright, by name.
    pub denied: &'static [(&'static str, u32)],
}

pub const X86_64: ArchTable = ArchTable {
    name: "x86_64",
    audit_arch: 0xC000_003E,
    reject_x32: true,
    unshare: 272,
    clone: 56,
    clone3: 435,
    ioctl: 16,
    denied: &[
        ("ptrace", 101),
        ("process_vm_readv", 310),
        ("process_vm_writev", 311),
        ("bpf", 321),
        ("keyctl", 250),
        ("add_key", 248),
        ("request_key", 249),
        ("io_uring_setup", 425),
        ("io_uring_enter", 426),
        ("io_uring_register", 427),
        ("setns", 308),
        ("perf_event_open", 298),
        ("kexec_load", 246),
        ("kexec_file_load", 320),
        ("userfaultfd", 323),
        ("mount", 165),
        ("umount2", 166),
        ("pivot_root", 155),
        ("open_by_handle_at", 304),
        ("init_module", 175),
        ("finit_module", 313),
        ("delete_module", 176),
        ("reboot", 169),
        ("swapon", 167),
        ("swapoff", 168),
        ("acct", 163),
        ("open_tree", 428),
        ("move_mount", 429),
        ("fsopen", 430),
        ("fsconfig", 431),
        ("fsmount", 432),
        ("fspick", 433),
        ("mount_setattr", 442),
    ],
};

pub const AARCH64: ArchTable = ArchTable {
    name: "aarch64",
    audit_arch: 0xC000_00B7,
    reject_x32: false,
    unshare: 97,
    clone: 220,
    clone3: 435,
    ioctl: 29,
    denied: &[
        ("ptrace", 117),
        ("process_vm_readv", 270),
        ("process_vm_writev", 271),
        ("bpf", 280),
        ("keyctl", 219),
        ("add_key", 217),
        ("request_key", 218),
        ("io_uring_setup", 425),
        ("io_uring_enter", 426),
        ("io_uring_register", 427),
        ("setns", 268),
        ("perf_event_open", 241),
        ("kexec_load", 104),
        ("kexec_file_load", 294),
        ("userfaultfd", 282),
        ("mount", 40),
        ("umount2", 39),
        ("pivot_root", 41),
        ("open_by_handle_at", 265),
        ("init_module", 105),
        ("finit_module", 273),
        ("delete_module", 106),
        ("reboot", 142),
        ("swapon", 224),
        ("swapoff", 225),
        ("acct", 89),
        ("open_tree", 428),
        ("move_mount", 429),
        ("fsopen", 430),
        ("fsconfig", 431),
        ("fsmount", 432),
        ("fspick", 433),
        ("mount_setattr", 442),
    ],
};

/// The table for the architecture this binary was built for, if supported.
pub fn native_arch() -> Option<&'static ArchTable> {
    #[cfg(target_arch = "x86_64")]
    {
        Some(&X86_64)
    }
    #[cfg(target_arch = "aarch64")]
    {
        Some(&AARCH64)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeccompMode {
    /// Denied calls fail with EPERM.
    Enforce,
    /// Denied calls are allowed but logged (`SECCOMP_RET_LOG`), for diagnosis.
    Log,
}

#[derive(Clone, Copy)]
enum Tgt {
    Next,
    L(&'static str),
}

enum Ins {
    Ld(u32),
    Jeq(u32, Tgt, Tgt),
    Jge(u32, Tgt, Tgt),
    Jset(u32, Tgt, Tgt),
    Ret(u32),
}

/// Assemble the deny-list filter for `arch`.
///
/// Layout: verify the architecture (kill on mismatch, which also covers 32-bit
/// compat calls), compare the syscall number against the unconditional list,
/// then the argument-sensitive calls, and allow everything else.
pub fn build_filter(arch: &ArchTable, mode: SeccompMode) -> Result<Vec<SockFilter>, String> {
    let deny_ret = match mode {
        SeccompMode::Enforce => SECCOMP_RET_ERRNO | EPERM,
        SeccompMode::Log => SECCOMP_RET_LOG,
    };
    let mut ops: Vec<Ins> = Vec::new();
    let mut labels: Vec<(&'static str, usize)> = Vec::new();

    ops.push(Ins::Ld(OFF_ARCH));
    ops.push(Ins::Jeq(arch.audit_arch, Tgt::Next, Tgt::L("kill")));
    ops.push(Ins::Ld(OFF_NR));
    if arch.reject_x32 {
        ops.push(Ins::Jge(0x4000_0000, Tgt::L("kill"), Tgt::Next));
    }
    for (_, nr) in arch.denied {
        ops.push(Ins::Jeq(*nr, Tgt::L("deny"), Tgt::Next));
    }
    // unshare(2) and clone(2) with CLONE_NEWUSER: no nested user namespaces,
    // which are how an unprivileged process regains capabilities.
    ops.push(Ins::Jeq(arch.unshare, Tgt::Next, Tgt::L("c_clone")));
    ops.push(Ins::Ld(off_arg(0)));
    ops.push(Ins::Jset(CLONE_NEWUSER, Tgt::L("deny"), Tgt::L("allow")));
    labels.push(("c_clone", ops.len()));
    ops.push(Ins::Jeq(arch.clone, Tgt::Next, Tgt::L("c_ioctl")));
    ops.push(Ins::Ld(off_arg(0)));
    ops.push(Ins::Jset(CLONE_NEWUSER, Tgt::L("deny"), Tgt::L("allow")));
    // ioctl(TIOCSTI) injects keystrokes into the controlling terminal.
    labels.push(("c_ioctl", ops.len()));
    ops.push(Ins::Jeq(arch.ioctl, Tgt::Next, Tgt::L("c_clone3")));
    ops.push(Ins::Ld(off_arg(1)));
    ops.push(Ins::Jeq(TIOCSTI, Tgt::L("deny"), Tgt::L("allow")));
    // clone3 takes its flags through a struct the filter cannot read. ENOSYS
    // makes libc fall back to clone(2), which the rule above can inspect.
    labels.push(("c_clone3", ops.len()));
    ops.push(Ins::Jeq(arch.clone3, Tgt::L("enosys"), Tgt::L("allow")));
    labels.push(("allow", ops.len()));
    ops.push(Ins::Ret(SECCOMP_RET_ALLOW));
    labels.push(("deny", ops.len()));
    ops.push(Ins::Ret(deny_ret));
    labels.push(("enosys", ops.len()));
    ops.push(Ins::Ret(match mode {
        SeccompMode::Enforce => SECCOMP_RET_ERRNO | ENOSYS,
        SeccompMode::Log => SECCOMP_RET_LOG,
    }));
    labels.push(("kill", ops.len()));
    ops.push(Ins::Ret(SECCOMP_RET_KILL_PROCESS));

    let find = |name: &str| -> Result<usize, String> {
        labels
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, i)| *i)
            .ok_or_else(|| format!("internal error: seccomp label {name} is not defined"))
    };
    let mut out = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        let rel = |t: &Tgt| -> Result<u8, String> {
            let target = match t {
                Tgt::Next => i + 1,
                Tgt::L(n) => find(n)?,
            };
            let off = target
                .checked_sub(i + 1)
                .ok_or_else(|| "internal error: seccomp jump goes backwards".to_string())?;
            u8::try_from(off).map_err(|_| {
                format!(
                    "internal error: seccomp jump of {off} instructions exceeds the BPF limit \
                     of 255; shorten the deny list or split the program"
                )
            })
        };
        out.push(match op {
            Ins::Ld(k) => SockFilter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: *k,
            },
            Ins::Jeq(k, t, f) => SockFilter {
                code: BPF_JEQ_K,
                jt: rel(t)?,
                jf: rel(f)?,
                k: *k,
            },
            Ins::Jge(k, t, f) => SockFilter {
                code: BPF_JGE_K,
                jt: rel(t)?,
                jf: rel(f)?,
                k: *k,
            },
            Ins::Jset(k, t, f) => SockFilter {
                code: BPF_JSET_K,
                jt: rel(t)?,
                jf: rel(f)?,
                k: *k,
            },
            Ins::Ret(k) => SockFilter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: *k,
            },
        });
    }
    if out.len() > 4096 {
        return Err(format!(
            "seccomp program has {} instructions; the kernel limit is 4096",
            out.len()
        ));
    }
    Ok(out)
}

// ── Helper arguments ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandlockMode {
    /// Landlock must be enforced or the launch fails (Landlock-only provider).
    Require,
    /// Enforce when the kernel can, and say so loudly when it cannot (bwrap
    /// provider: the mount namespace already confines the filesystem).
    Auto,
}

/// What the launcher asks the helper to do. Round-trips through `to_args` /
/// `parse` so the builder and the helper cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperArgs {
    pub ro: Vec<PathBuf>,
    pub rw: Vec<PathBuf>,
    /// Device directories: read, write and list, no create or remove.
    pub dev: Vec<PathBuf>,
    pub landlock: LandlockMode,
    /// Handle TCP bind/connect rights (ABI 4+). Used by the Landlock-only
    /// provider, where there is no network namespace.
    pub restrict_net: bool,
    /// TCP ports the agent may connect to when `restrict_net` is set.
    pub connect_ports: Vec<u16>,
    pub seccomp: SeccompMode,
    /// In-namespace relay: listen on `127.0.0.1:egress_port`, forward each
    /// connection to this Unix socket (the host's egress proxy relay).
    pub egress_socket: Option<PathBuf>,
    pub egress_port: Option<u16>,
    /// The agent command and its arguments.
    pub command: Vec<String>,
}

impl HelperArgs {
    pub fn to_args(&self) -> Vec<String> {
        let mut a = Vec::new();
        for p in &self.ro {
            a.push("--ro".into());
            a.push(p.to_string_lossy().into_owned());
        }
        for p in &self.rw {
            a.push("--rw".into());
            a.push(p.to_string_lossy().into_owned());
        }
        for p in &self.dev {
            a.push("--dev".into());
            a.push(p.to_string_lossy().into_owned());
        }
        a.push("--landlock".into());
        a.push(
            match self.landlock {
                LandlockMode::Require => "require",
                LandlockMode::Auto => "auto",
            }
            .into(),
        );
        if self.restrict_net {
            a.push("--restrict-net".into());
        }
        for p in &self.connect_ports {
            a.push("--connect-port".into());
            a.push(p.to_string());
        }
        a.push("--seccomp".into());
        a.push(
            match self.seccomp {
                SeccompMode::Enforce => "enforce",
                SeccompMode::Log => "log",
            }
            .into(),
        );
        if let (Some(s), Some(port)) = (&self.egress_socket, self.egress_port) {
            a.push("--egress-socket".into());
            a.push(s.to_string_lossy().into_owned());
            a.push("--egress-port".into());
            a.push(port.to_string());
        }
        a.push("--".into());
        a.extend(self.command.iter().cloned());
        a
    }

    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut out = HelperArgs {
            ro: Vec::new(),
            rw: Vec::new(),
            dev: Vec::new(),
            landlock: LandlockMode::Require,
            restrict_net: false,
            connect_ports: Vec::new(),
            seccomp: SeccompMode::Enforce,
            egress_socket: None,
            egress_port: None,
            command: Vec::new(),
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let mut value = |name: &str| -> Result<String, String> {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match flag.as_str() {
                "--ro" => out.ro.push(PathBuf::from(value("--ro")?)),
                "--rw" => out.rw.push(PathBuf::from(value("--rw")?)),
                "--dev" => out.dev.push(PathBuf::from(value("--dev")?)),
                "--landlock" => {
                    out.landlock = match value("--landlock")?.as_str() {
                        "require" => LandlockMode::Require,
                        "auto" => LandlockMode::Auto,
                        other => {
                            return Err(format!(
                                "--landlock must be \"require\" or \"auto\", got \"{other}\""
                            ))
                        }
                    }
                }
                "--restrict-net" => out.restrict_net = true,
                "--connect-port" => {
                    let v = value("--connect-port")?;
                    out.connect_ports.push(
                        v.parse().map_err(|_| {
                            format!("--connect-port needs a port number, got \"{v}\"")
                        })?,
                    );
                }
                "--seccomp" => {
                    out.seccomp = match value("--seccomp")?.as_str() {
                        "enforce" => SeccompMode::Enforce,
                        "log" => SeccompMode::Log,
                        other => {
                            return Err(format!(
                                "--seccomp must be \"enforce\" or \"log\", got \"{other}\""
                            ))
                        }
                    }
                }
                "--egress-socket" => {
                    out.egress_socket = Some(PathBuf::from(value("--egress-socket")?))
                }
                "--egress-port" => {
                    let v = value("--egress-port")?;
                    out.egress_port =
                        Some(v.parse().map_err(|_| {
                            format!("--egress-port needs a port number, got \"{v}\"")
                        })?);
                }
                "--" => {
                    out.command = it.cloned().collect();
                    break;
                }
                other => return Err(format!("unknown option \"{other}\"")),
            }
        }
        if out.command.is_empty() {
            return Err("no command after `--`".to_string());
        }
        if out.egress_socket.is_some() != out.egress_port.is_some() {
            return Err("--egress-socket and --egress-port go together".to_string());
        }
        Ok(out)
    }
}

pub const HELPER_USAGE: &str =
    "usage: ta-sandbox-exec [--ro PATH]... [--rw PATH]... [--dev PATH]... \
[--landlock require|auto] [--restrict-net] [--connect-port N]... [--seccomp enforce|log] \
[--egress-socket PATH --egress-port N] -- COMMAND [ARGS...]\n\
This program is started by `ta run` inside the sandbox; it is not meant to be run by hand.";

/// Exit status when the helper could not apply a required layer. The agent is
/// never started in that case.
pub const EXIT_CONFINE_FAILED: i32 = 125;

// ── Linux syscalls ─────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod sys {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
    const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
    const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
    const LANDLOCK_RULE_NET_PORT: u32 = 2;
    const PR_SET_SECCOMP: libc::c_int = 22;
    const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
    const PR_GET_SECCOMP: libc::c_int = 21;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
        scoped: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    #[repr(C)]
    struct NetPortAttr {
        allowed_access: u64,
        port: u64,
    }

    #[repr(C)]
    struct SockFprog {
        len: u16,
        filter: *const SockFilter,
    }

    /// The kernel's Landlock ABI, or why there is none.
    pub fn landlock_status() -> crate::linux_probe::LandlockStatus {
        use crate::linux_probe::LandlockStatus;
        // SAFETY: a version query passes a null attr and size 0; no memory is read.
        let r = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            )
        };
        if r >= 1 {
            LandlockStatus::Available { abi: r as u32 }
        } else {
            let e = std::io::Error::last_os_error();
            let why = match e.raw_os_error() {
                Some(libc::ENOSYS) => {
                    "kernel older than 5.13 or built without Landlock".to_string()
                }
                Some(libc::EOPNOTSUPP) => {
                    "Landlock is compiled in but disabled at boot (add `landlock` to the lsm= \
                     kernel parameter)"
                        .to_string()
                }
                Some(libc::EPERM) => {
                    "blocked by a seccomp profile (common in containers)".to_string()
                }
                _ => e.to_string(),
            };
            LandlockStatus::Unavailable { reason: why }
        }
    }

    pub fn seccomp_supported() -> bool {
        if native_arch().is_none() {
            return false;
        }
        // SAFETY: PR_GET_SECCOMP takes no pointer arguments.
        let r = unsafe { libc::prctl(PR_GET_SECCOMP) };
        r >= 0
    }

    pub fn set_no_new_privs() -> Result<(), String> {
        // SAFETY: PR_SET_NO_NEW_PRIVS with constant arguments.
        let r = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if r != 0 {
            return Err(format!(
                "could not set no_new_privs: {}. The kernel refused; the sandbox cannot be \
                 completed.",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub fn install_seccomp(mode: SeccompMode) -> Result<usize, String> {
        let arch = native_arch().ok_or_else(|| {
            "seccomp filtering is implemented for x86_64 and aarch64 only; this build targets \
             another architecture, so the hardened Linux profile cannot run here"
                .to_string()
        })?;
        let prog = build_filter(arch, mode)?;
        let fprog = SockFprog {
            len: prog.len() as u16,
            filter: prog.as_ptr(),
        };
        // SAFETY: `fprog` points at `prog`, which outlives the call; the kernel copies it.
        let r = unsafe {
            libc::prctl(
                PR_SET_SECCOMP,
                SECCOMP_MODE_FILTER,
                &fprog as *const SockFprog as libc::c_ulong,
                0,
                0,
            )
        };
        if r != 0 {
            return Err(format!(
                "could not install the seccomp filter: {}. A container runtime may forbid \
                 nested seccomp filters; run outside it or relax its profile.",
                std::io::Error::last_os_error()
            ));
        }
        Ok(prog.len())
    }

    /// What Landlock enforced, for the status line.
    pub struct LandlockOutcome {
        pub abi: u32,
        pub rules: usize,
        pub absent: usize,
        pub net: bool,
        pub scopes: bool,
    }

    pub fn apply_landlock(args: &HelperArgs, abi: u32) -> Result<LandlockOutcome, String> {
        let fs = landlock::fs_handled(abi);
        let net = landlock::net_handled(abi, args.restrict_net);
        let scope = landlock::scoped(abi);
        let attr = RulesetAttr {
            handled_access_fs: fs,
            handled_access_net: net,
            scoped: scope,
        };
        // SAFETY: `attr` is a valid RulesetAttr and the size passed is no larger than it.
        let fd = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                &attr as *const RulesetAttr,
                landlock::attr_size(abi),
                0u32,
            )
        };
        if fd < 0 {
            return Err(format!(
                "landlock_create_ruleset failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let ruleset = fd as libc::c_int;
        let mut rules = 0usize;
        let mut absent = 0usize;
        let result = (|| -> Result<(), String> {
            for (paths, access) in [
                (&args.ro, landlock::ro_access(fs)),
                (&args.rw, landlock::rw_access(fs)),
                (&args.dev, landlock::dev_access(fs)),
            ] {
                for p in paths {
                    let c = CString::new(p.as_os_str().as_bytes())
                        .map_err(|_| format!("path {} contains a NUL byte", p.display()))?;
                    // SAFETY: `c` is a valid NUL-terminated path.
                    let pfd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
                    if pfd < 0 {
                        // A path that does not exist grants nothing and needs no rule.
                        absent += 1;
                        continue;
                    }
                    let is_dir = std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(true);
                    let rule = PathBeneathAttr {
                        allowed_access: landlock::for_target(access, is_dir),
                        parent_fd: pfd,
                    };
                    // SAFETY: `rule` is a valid packed PathBeneathAttr; `ruleset` is open.
                    let r = unsafe {
                        libc::syscall(
                            SYS_LANDLOCK_ADD_RULE,
                            ruleset,
                            LANDLOCK_RULE_PATH_BENEATH,
                            &rule as *const PathBeneathAttr,
                            0u32,
                        )
                    };
                    let err = std::io::Error::last_os_error();
                    // SAFETY: closing the O_PATH descriptor opened above.
                    unsafe { libc::close(pfd) };
                    if r != 0 {
                        return Err(format!(
                            "landlock_add_rule failed for {}: {err}",
                            p.display()
                        ));
                    }
                    rules += 1;
                }
            }
            if net != 0 {
                for port in &args.connect_ports {
                    let rule = NetPortAttr {
                        allowed_access: landlock::NET_CONNECT_TCP,
                        port: *port as u64,
                    };
                    // SAFETY: `rule` is a valid NetPortAttr; `ruleset` is open.
                    let r = unsafe {
                        libc::syscall(
                            SYS_LANDLOCK_ADD_RULE,
                            ruleset,
                            LANDLOCK_RULE_NET_PORT,
                            &rule as *const NetPortAttr,
                            0u32,
                        )
                    };
                    if r != 0 {
                        return Err(format!(
                            "landlock_add_rule failed for TCP port {port}: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                }
            }
            set_no_new_privs()?;
            // SAFETY: restricting the calling thread; `ruleset` is a valid ruleset fd.
            let r = unsafe { libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset, 0u32) };
            if r != 0 {
                return Err(format!(
                    "landlock_restrict_self failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        })();
        // SAFETY: closing the ruleset descriptor opened above.
        unsafe { libc::close(ruleset) };
        result?;
        Ok(LandlockOutcome {
            abi,
            rules,
            absent,
            net: net != 0,
            scopes: scope != 0,
        })
    }

    /// Apply Landlock then seccomp. Returns the status line to print.
    pub fn confine(args: &HelperArgs) -> Result<String, String> {
        use crate::linux_probe::LandlockStatus;
        let mut parts: Vec<String> = Vec::new();
        match (landlock_status(), args.landlock) {
            (LandlockStatus::Available { abi }, _) => {
                let o = apply_landlock(args, abi)?;
                parts.push(format!(
                    "landlock ABI {} enforced ({} path rule(s){}{}{})",
                    o.abi,
                    o.rules,
                    if o.absent > 0 {
                        format!(", {} absent path(s) skipped", o.absent)
                    } else {
                        String::new()
                    },
                    if o.net {
                        ", TCP connect limited to the egress port"
                    } else if args.restrict_net {
                        ", TCP NOT restricted: needs ABI 4 (Linux 6.7)"
                    } else {
                        ""
                    },
                    if o.scopes {
                        ", signals and abstract sockets scoped"
                    } else {
                        ""
                    }
                ));
            }
            (LandlockStatus::Unavailable { reason }, LandlockMode::Require) => {
                return Err(format!(
                    "Landlock is required for this sandbox but unavailable: {reason}. Install \
                     bubblewrap and enable unprivileged user namespaces for the namespace \
                     sandbox, or run on Linux 5.13+ with the landlock LSM enabled."
                ));
            }
            (LandlockStatus::Unavailable { reason }, LandlockMode::Auto) => {
                set_no_new_privs()?;
                parts.push(format!(
                    "landlock NOT enforced ({reason}); the bubblewrap mount namespace still \
                     confines the filesystem"
                ));
            }
        }
        let n = install_seccomp(args.seccomp)?;
        parts.push(format!(
            "seccomp {} ({n} instructions)",
            match args.seccomp {
                SeccompMode::Enforce => "enforced",
                SeccompMode::Log => "in LOG-ONLY diagnostic mode (not blocking)",
            }
        ));
        Ok(parts.join("; "))
    }

    /// Run the helper. Returns an exit code only on failure; success replaces
    /// the process with the agent.
    pub fn run_helper(argv: &[String]) -> i32 {
        use std::os::unix::process::CommandExt;
        let args = match HelperArgs::parse(argv) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("ta-sandbox-exec: {e}\n{HELPER_USAGE}");
                return 2;
            }
        };

        // The relay child is created before confinement (it needs to bind) and
        // confined itself before it serves.
        let mut relay_listener = None;
        if let (Some(_), Some(port)) = (&args.egress_socket, args.egress_port) {
            match std::net::TcpListener::bind(("127.0.0.1", port)) {
                Ok(l) => relay_listener = Some(l),
                Err(e) => {
                    eprintln!(
                        "ta-sandbox-exec: cannot start the egress relay on 127.0.0.1:{port} \
                         inside the sandbox: {e}. The sandbox network namespace has no loopback \
                         address; use bubblewrap 0.4 or newer."
                    );
                    return EXIT_CONFINE_FAILED;
                }
            }
        }
        if let Some(listener) = relay_listener {
            let sock = args.egress_socket.clone().unwrap_or_default();
            // SAFETY: the helper is single-threaded here, so fork is safe.
            let pid = unsafe { libc::fork() };
            if pid < 0 {
                eprintln!(
                    "ta-sandbox-exec: cannot fork the egress relay: {}",
                    std::io::Error::last_os_error()
                );
                return EXIT_CONFINE_FAILED;
            }
            if pid == 0 {
                // SAFETY: arranging for the relay to die with its parent.
                unsafe {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                }
                // SAFETY: getppid has no preconditions.
                if unsafe { libc::getppid() } == 1 {
                    std::process::exit(0);
                }
                if let Err(e) = confine(&args) {
                    eprintln!("ta-sandbox-exec: egress relay could not be confined: {e}");
                    std::process::exit(EXIT_CONFINE_FAILED);
                }
                crate::linux_relay::serve_tcp_to_unix(listener, &sock);
                std::process::exit(0);
            }
            drop(listener);
        }

        match confine(&args) {
            Ok(line) => eprintln!("[sandbox] linux: {line}"),
            Err(e) => {
                eprintln!(
                    "ta-sandbox-exec: the sandbox could not be completed, so the agent was not \
                     started: {e}"
                );
                return EXIT_CONFINE_FAILED;
            }
        }
        let err = std::process::Command::new(&args.command[0])
            .args(&args.command[1..])
            .exec();
        eprintln!(
            "ta-sandbox-exec: cannot run {}: {err}. Check that the command and its \
             interpreter are inside the sandbox view (add their directories to allow_read).",
            args.command[0]
        );
        127
    }
}

#[cfg(target_os = "linux")]
pub use sys::{landlock_status, run_helper, seccomp_supported};

#[cfg(not(target_os = "linux"))]
pub fn landlock_status() -> crate::linux_probe::LandlockStatus {
    crate::linux_probe::LandlockStatus::Unavailable {
        reason: "not a Linux host".to_string(),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn seccomp_supported() -> bool {
    false
}

/// Windows: the helper is the in-container egress relay plus launcher.
#[cfg(target_os = "windows")]
pub fn run_helper(argv: &[String]) -> i32 {
    crate::windows_relay::run_helper(argv)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn run_helper(_argv: &[String]) -> i32 {
    eprintln!(
        "ta-sandbox-exec: the Landlock and seccomp helper runs on Linux only. On this platform \
         the sandbox uses the native provider (Seatbelt on macOS, AppContainer on Windows)."
    );
    EXIT_CONFINE_FAILED
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── A minimal BPF interpreter for the subset the generator emits ──────

    fn run_bpf(prog: &[SockFilter], arch: u32, nr: u32, args: [u64; 6]) -> u32 {
        let load = |off: u32| -> u32 {
            match off {
                0 => nr,
                4 => arch,
                o if (16..64).contains(&o) && (o - 16) % 8 == 0 => {
                    args[((o - 16) / 8) as usize] as u32
                }
                o => panic!("filter loads unexpected offset {o}"),
            }
        };
        let mut a = 0u32;
        let mut pc = 0usize;
        loop {
            let i = prog[pc];
            pc += 1;
            match i.code {
                BPF_LD_W_ABS => a = load(i.k),
                BPF_JEQ_K => pc += if a == i.k { i.jt } else { i.jf } as usize,
                BPF_JGE_K => pc += if a >= i.k { i.jt } else { i.jf } as usize,
                BPF_JSET_K => pc += if a & i.k != 0 { i.jt } else { i.jf } as usize,
                BPF_RET_K => return i.k,
                c => panic!("unexpected opcode {c:#x}"),
            }
        }
    }

    const DENY: u32 = SECCOMP_RET_ERRNO | EPERM;

    fn nr(arch: &ArchTable, name: &str) -> u32 {
        arch.denied
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("{name} missing from {}", arch.name))
            .1
    }

    fn check_arch(arch: &ArchTable) {
        let prog = build_filter(arch, SeccompMode::Enforce).unwrap();
        let run = |n: u32, args: [u64; 6]| run_bpf(&prog, arch.audit_arch, n, args);
        let z = [0u64; 6];

        // Every named syscall is denied with EPERM.
        for (name, n) in arch.denied {
            assert_eq!(run(*n, z), DENY, "{name} on {}", arch.name);
        }
        // The calls the spec names, explicitly.
        for name in [
            "ptrace",
            "process_vm_readv",
            "process_vm_writev",
            "bpf",
            "keyctl",
            "io_uring_setup",
            "io_uring_enter",
            "io_uring_register",
        ] {
            assert_eq!(run(nr(arch, name), z), DENY, "{name}");
        }
        // Nested user namespaces: unshare and clone with CLONE_NEWUSER.
        let newuser = [CLONE_NEWUSER as u64, 0, 0, 0, 0, 0];
        assert_eq!(run(arch.unshare, newuser), DENY, "unshare(NEWUSER)");
        assert_eq!(run(arch.clone, newuser), DENY, "clone(NEWUSER)");
        // Ordinary threads and processes still work.
        let thread_flags = 0x0001_0f00u64 | 0x0010_0000; // CLONE_VM|FS|FILES|SIGHAND|THREAD...
        assert_eq!(
            run(arch.clone, [thread_flags, 0, 0, 0, 0, 0]),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            run(arch.unshare, [0x0002_0000, 0, 0, 0, 0, 0]),
            SECCOMP_RET_ALLOW
        );
        // The high half of a 64-bit flags register does not hide CLONE_NEWUSER.
        let wide = [(1u64 << 40) | CLONE_NEWUSER as u64, 0, 0, 0, 0, 0];
        assert_eq!(run(arch.clone, wide), DENY);
        // clone3 falls back to clone through ENOSYS.
        assert_eq!(run(arch.clone3, z), SECCOMP_RET_ERRNO | ENOSYS);
        // TIOCSTI is blocked, other ioctls are not.
        assert_eq!(run(arch.ioctl, [0, TIOCSTI as u64, 0, 0, 0, 0]), DENY);
        assert_eq!(
            run(arch.ioctl, [0, 0x5401 /* TCGETS */, 0, 0, 0, 0]),
            SECCOMP_RET_ALLOW
        );
        // Common syscalls are allowed: read=0/write=1 on x86_64; use an unlisted number.
        for n in [0u32, 1, 2, 3, 59, 60, 231, 257] {
            if arch.denied.iter().all(|(_, d)| *d != n)
                && n != arch.unshare
                && n != arch.clone
                && n != arch.ioctl
                && n != arch.clone3
            {
                assert_eq!(run(n, z), SECCOMP_RET_ALLOW, "syscall {n} on {}", arch.name);
            }
        }
        // A foreign architecture (the 32-bit compat ABI) is killed, not allowed.
        assert_eq!(
            run_bpf(&prog, 0x4000_0003 /* AUDIT_ARCH_I386 */, 1, z),
            SECCOMP_RET_KILL_PROCESS
        );
        if arch.reject_x32 {
            assert_eq!(
                run(0x4000_0000 | 101, z),
                SECCOMP_RET_KILL_PROCESS,
                "x32 ptrace"
            );
        }
    }

    #[test]
    fn x86_64_filter_denies_and_allows_correctly() {
        check_arch(&X86_64);
    }

    #[test]
    fn aarch64_filter_denies_and_allows_correctly() {
        check_arch(&AARCH64);
    }

    #[test]
    fn syscall_numbers_match_the_kernel_tables() {
        // Spot-pinned against arch/x86/entry/syscalls/syscall_64.tbl and
        // include/uapi/asm-generic/unistd.h so a typo cannot ship.
        assert_eq!(nr(&X86_64, "ptrace"), 101);
        assert_eq!(nr(&X86_64, "process_vm_readv"), 310);
        assert_eq!(nr(&X86_64, "bpf"), 321);
        assert_eq!(nr(&X86_64, "keyctl"), 250);
        assert_eq!(nr(&AARCH64, "ptrace"), 117);
        assert_eq!(nr(&AARCH64, "process_vm_readv"), 270);
        assert_eq!(nr(&AARCH64, "bpf"), 280);
        assert_eq!(nr(&AARCH64, "keyctl"), 219);
        assert_eq!(
            nr(&X86_64, "io_uring_setup"),
            nr(&AARCH64, "io_uring_setup")
        );
        assert_eq!(X86_64.unshare, 272);
        assert_eq!(AARCH64.unshare, 97);
        assert_eq!(X86_64.clone, 56);
        assert_eq!(AARCH64.clone, 220);
        assert_eq!(X86_64.ioctl, 16);
        assert_eq!(AARCH64.ioctl, 29);
    }

    #[test]
    fn deny_list_numbers_are_unique_per_arch() {
        for arch in [&X86_64, &AARCH64] {
            let mut seen = std::collections::HashSet::new();
            for (name, n) in arch.denied {
                assert!(
                    seen.insert(*n),
                    "duplicate syscall number {n} ({name}) on {}",
                    arch.name
                );
                assert_ne!(*n, arch.clone);
                assert_ne!(*n, arch.unshare);
                assert_ne!(*n, arch.ioctl);
            }
        }
    }

    #[test]
    fn log_mode_does_not_block() {
        let prog = build_filter(&X86_64, SeccompMode::Log).unwrap();
        let r = run_bpf(&prog, X86_64.audit_arch, 101, [0; 6]);
        assert_eq!(r, SECCOMP_RET_LOG);
    }

    #[test]
    fn filter_jumps_fit_in_a_byte() {
        for arch in [&X86_64, &AARCH64] {
            let p = build_filter(arch, SeccompMode::Enforce).unwrap();
            assert!(p.len() < 120, "unexpectedly long program: {}", p.len());
            assert_eq!(p.last().unwrap().k, SECCOMP_RET_KILL_PROCESS);
        }
    }

    // ── Landlock masks ────────────────────────────────────────────────────

    #[test]
    fn landlock_rights_grow_with_the_abi() {
        use landlock::*;
        assert_eq!(fs_handled(1) & FS_REFER, 0);
        assert_eq!(fs_handled(1) & FS_TRUNCATE, 0);
        assert_ne!(fs_handled(2) & FS_REFER, 0);
        assert_eq!(fs_handled(2) & FS_TRUNCATE, 0);
        assert_ne!(fs_handled(3) & FS_TRUNCATE, 0);
        assert_eq!(fs_handled(1).count_ones(), 13);
        // Device ioctls are not handled at any ABI.
        assert_eq!(fs_handled(7) & (1 << 15), 0);
        assert_eq!(net_handled(3, true), 0);
        assert_eq!(net_handled(4, true), NET_BIND_TCP | NET_CONNECT_TCP);
        assert_eq!(net_handled(4, false), 0);
        assert_eq!(scoped(5), 0);
        assert_eq!(scoped(6), SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL);
        assert_eq!(attr_size(1), 8);
        assert_eq!(attr_size(4), 16);
        assert_eq!(attr_size(6), 24);
    }

    #[test]
    fn landlock_read_only_access_never_writes() {
        use landlock::*;
        for abi in 1..=7 {
            let ro = ro_access(fs_handled(abi));
            assert_eq!(ro, FS_EXECUTE | FS_READ_FILE | FS_READ_DIR);
            let rw = rw_access(fs_handled(abi));
            assert_ne!(rw & FS_WRITE_FILE, 0);
            assert_ne!(rw & FS_MAKE_REG, 0);
            assert_ne!(rw & FS_REMOVE_FILE, 0);
        }
    }

    #[test]
    fn landlock_dev_access_cannot_create_or_remove() {
        use landlock::*;
        let d = dev_access(fs_handled(3));
        assert_ne!(d & FS_WRITE_FILE, 0);
        assert_ne!(d & FS_READ_FILE, 0);
        assert_eq!(
            d & (FS_MAKE_REG | FS_MAKE_CHAR | FS_REMOVE_FILE | FS_REFER | FS_EXECUTE),
            0
        );
    }

    #[test]
    fn landlock_file_rules_drop_directory_only_rights() {
        use landlock::*;
        let rw = rw_access(fs_handled(3));
        let for_file = for_target(rw, false);
        assert_eq!(for_file & (FS_READ_DIR | FS_MAKE_REG | FS_REMOVE_DIR), 0);
        assert_ne!(for_file & FS_WRITE_FILE, 0);
        assert_eq!(for_target(rw, true), rw);
    }

    // ── Helper arguments ──────────────────────────────────────────────────

    fn sample() -> HelperArgs {
        HelperArgs {
            ro: vec!["/usr".into(), "/etc/ssl".into()],
            rw: vec!["/work/staging".into()],
            dev: vec!["/dev".into()],
            landlock: LandlockMode::Auto,
            restrict_net: true,
            connect_ports: vec![41000],
            seccomp: SeccompMode::Log,
            egress_socket: Some("/run/egress/e.sock".into()),
            egress_port: Some(41000),
            command: vec!["claude".into(), "--print".into(), "--flag=--ro".into()],
        }
    }

    #[test]
    fn helper_args_round_trip() {
        let a = sample();
        let parsed = HelperArgs::parse(&a.to_args()).unwrap();
        assert_eq!(parsed, a);
    }

    #[test]
    fn helper_args_keep_agent_flags_after_the_separator() {
        // The agent's own arguments look like helper flags and must pass through untouched.
        let mut a = sample();
        a.command = vec!["tool".into(), "--rw".into(), "/etc".into(), "--".into()];
        let parsed = HelperArgs::parse(&a.to_args()).unwrap();
        assert_eq!(parsed.command, a.command);
        assert_eq!(parsed.rw, vec![PathBuf::from("/work/staging")]);
    }

    #[test]
    fn helper_args_reject_bad_input_with_a_reason() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(HelperArgs::parse(&s(&["--ro", "/usr"]))
            .unwrap_err()
            .contains("no command"));
        assert!(HelperArgs::parse(&s(&["--bogus", "--", "x"]))
            .unwrap_err()
            .contains("unknown option"));
        assert!(HelperArgs::parse(&s(&["--landlock", "maybe", "--", "x"]))
            .unwrap_err()
            .contains("require"));
        assert!(HelperArgs::parse(&s(&["--ro"]))
            .unwrap_err()
            .contains("needs a value"));
        assert!(HelperArgs::parse(&s(&["--egress-port", "x", "--", "c"]))
            .unwrap_err()
            .contains("port number"));
        assert!(HelperArgs::parse(&s(&["--egress-port", "80", "--", "c"]))
            .unwrap_err()
            .contains("go together"));
    }

    #[test]
    fn landlock_defaults_to_required() {
        let a = HelperArgs::parse(&["--".to_string(), "x".to_string()]).unwrap();
        assert_eq!(
            a.landlock,
            LandlockMode::Require,
            "the safe default must not be best-effort"
        );
        assert_eq!(a.seccomp, SeccompMode::Enforce);
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    #[test]
    fn helper_refuses_to_run_off_linux() {
        assert_eq!(
            run_helper(&["--".into(), "true".into()]),
            EXIT_CONFINE_FAILED
        );
    }
}
