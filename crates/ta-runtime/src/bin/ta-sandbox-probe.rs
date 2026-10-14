// ta-sandbox-probe: one sandbox-relevant action per invocation (v0.17.12.7).
//
// The conformance suite (`tests/seatbelt_conformance.rs`) runs this binary
// inside the generated Seatbelt profile and checks, per action, that the
// profile allowed or denied it as intended. Exit status:
//   0  the action SUCCEEDED
//   1  the action was REFUSED or failed (reason on stderr)
//   2  bad usage
//
// Actions never have side effects beyond what they name: mach lookups do not
// start services, signal checks use signal 0, and nothing here shells out to git.
//
//   read <path>            open and read one byte
//   write <path>           create or append one byte
//   connect <host:port>    TCP connect (3s timeout)
//   unix <path>            connect to a unix socket
//   exec <path> [args...]  run a program, succeed if it exits 0
//   mach <service>         bootstrap lookup (macOS)
//   signal <pid>           kill(pid, 0)
//   pidinfo <pid>          proc_pidpath(pid) (macOS)
//   signal-child           spawn a sleeper, signal and reap it
//   rosetta                run /usr/bin/true as x86_64 via arch (macOS)
//
// Linux actions (v0.17.12.8). A raw syscall "succeeds" unless the kernel or a
// seccomp filter answered EPERM, because a denied call fails with EPERM:
//   syscall <bpf|keyctl|io_uring_setup|process_vm_readv>
//   ptrace                 PTRACE_ATTACH to a child of ours
//   unshare-user           unshare(CLONE_NEWUSER)
//   tiocsti                ioctl(TIOCSTI) on /dev/null (never a real tty)
//   clone3-enosys          succeeds only if clone3 answers ENOSYS
//   countpids              print the number of pids visible in /proc
//   netdevs                print the network interface names
//   listen <port>          bind 127.0.0.1:<port> and accept one connection
//   echo-proxy <host:port> send a line to a TCP service and expect it back

use std::io::{Read, Write};
use std::process::{Command, ExitCode};
use std::time::Duration;

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

#[cfg(target_os = "macos")]
mod mach {
    use std::ffi::CString;
    extern "C" {
        static bootstrap_port: u32;
        fn bootstrap_look_up(bp: u32, name: *const std::os::raw::c_char, sp: *mut u32) -> i32;
    }
    /// Kernel return code of the lookup; 0 means the service was found.
    pub fn look_up(service: &str) -> i32 {
        let Ok(name) = CString::new(service) else {
            return -1;
        };
        let mut port: u32 = 0;
        // SAFETY: `name` is a valid NUL-terminated string and `port` a valid out-pointer;
        // `bootstrap_port` is initialized by libSystem before main.
        unsafe { bootstrap_look_up(bootstrap_port, name.as_ptr(), &mut port) }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::fail;
    use std::process::ExitCode;

    pub fn handles(act: &str) -> bool {
        matches!(
            act,
            "syscall"
                | "ptrace"
                | "unshare-user"
                | "tiocsti"
                | "clone3-enosys"
                | "countpids"
                | "netdevs"
                | "listen"
                | "echo-proxy"
        )
    }

    /// Succeeds unless the call failed with EPERM.
    fn not_eperm(name: &str, rc: libc::c_long) -> ExitCode {
        if rc >= 0 {
            return ExitCode::SUCCESS;
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EPERM) {
            fail(format!("{name}: EPERM (denied)"))
        } else {
            eprintln!("{name}: {e} (not a permission denial)");
            ExitCode::SUCCESS
        }
    }

    pub fn run(act: &str, arg: Option<&str>) -> ExitCode {
        // SAFETY (all raw syscalls below): arguments are null or zero, which every
        // call here rejects with EFAULT or EINVAL at worst; nothing is written.
        match act {
            "syscall" => {
                let rc = match arg {
                    Some("bpf") => unsafe { libc::syscall(libc::SYS_bpf, 0, 0usize, 0u32) },
                    Some("keyctl") => unsafe { libc::syscall(libc::SYS_keyctl, 0, 0, 0, 0, 0) },
                    Some("io_uring_setup") => unsafe {
                        libc::syscall(libc::SYS_io_uring_setup, 0u32, 0usize)
                    },
                    Some("process_vm_readv") => unsafe {
                        libc::syscall(
                            libc::SYS_process_vm_readv,
                            libc::getpid(),
                            0usize,
                            0usize,
                            0usize,
                            0usize,
                            0usize,
                        )
                    },
                    _ => return ExitCode::from(2),
                };
                not_eperm(arg.unwrap_or("syscall"), rc)
            }
            "ptrace" => {
                let mut child = match std::process::Command::new("sleep").arg("30").spawn() {
                    Ok(c) => c,
                    Err(e) => return fail(format!("spawn sleep: {e}")),
                };
                // SAFETY: attaching to our own live child.
                let rc = unsafe {
                    libc::ptrace(
                        libc::PTRACE_ATTACH,
                        child.id() as libc::pid_t,
                        0usize,
                        0usize,
                    )
                };
                let out = not_eperm("ptrace", rc);
                let _ = child.kill();
                let _ = child.wait();
                out
            }
            "unshare-user" => {
                // SAFETY: unshare only changes this process's own namespaces.
                let rc = unsafe { libc::unshare(libc::CLONE_NEWUSER) };
                if rc == 0 {
                    ExitCode::SUCCESS
                } else {
                    fail(format!(
                        "unshare(CLONE_NEWUSER): {}",
                        std::io::Error::last_os_error()
                    ))
                }
            }
            "tiocsti" => {
                use std::os::fd::AsRawFd;
                let f = match std::fs::File::open("/dev/null") {
                    Ok(f) => f,
                    Err(e) => return fail(format!("open /dev/null: {e}")),
                };
                let c = b'x';
                // SAFETY: /dev/null is not a terminal, so this never injects input.
                let rc = unsafe { libc::ioctl(f.as_raw_fd(), 0x5412, &c as *const u8) };
                not_eperm("ioctl(TIOCSTI)", rc as libc::c_long)
            }
            "clone3-enosys" => {
                // SAFETY: a null clone_args pointer with size 0 is rejected by the kernel.
                let rc = unsafe { libc::syscall(libc::SYS_clone3, 0usize, 0usize) };
                if rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS) {
                    ExitCode::SUCCESS
                } else {
                    fail("clone3 did not answer ENOSYS")
                }
            }
            "countpids" => {
                let n = std::fs::read_dir("/proc")
                    .map(|d| {
                        d.flatten()
                            .filter(|e| {
                                e.file_name()
                                    .to_string_lossy()
                                    .chars()
                                    .all(|c| c.is_ascii_digit())
                            })
                            .count()
                    })
                    .unwrap_or(0);
                println!("{n}");
                ExitCode::SUCCESS
            }
            "netdevs" => match std::fs::read_to_string("/proc/net/dev") {
                Ok(t) => {
                    let names: Vec<&str> = t
                        .lines()
                        .skip(2)
                        .filter_map(|l| l.split(':').next())
                        .map(str::trim)
                        .collect();
                    println!("{}", names.join(" "));
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("read /proc/net/dev: {e}")),
            },
            "listen" => {
                let Some(port) = arg.and_then(|p| p.parse::<u16>().ok()) else {
                    return ExitCode::from(2);
                };
                match std::net::TcpListener::bind(("127.0.0.1", port)) {
                    Ok(_) => ExitCode::SUCCESS,
                    Err(e) => fail(format!("bind 127.0.0.1:{port}: {e}")),
                }
            }
            "echo-proxy" => {
                use std::io::{Read, Write};
                let Some(addr) = arg else {
                    return ExitCode::from(2);
                };
                let mut s = match std::net::TcpStream::connect(addr) {
                    Ok(s) => s,
                    Err(e) => return fail(format!("connect {addr}: {e}")),
                };
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                if s.write_all(b"ping").is_err() {
                    return fail("write failed");
                }
                let mut buf = [0u8; 4];
                match s.read_exact(&mut buf) {
                    Ok(()) if &buf == b"ping" => ExitCode::SUCCESS,
                    Ok(()) => fail("unexpected reply"),
                    Err(e) => fail(format!("read reply: {e}")),
                }
            }
            _ => ExitCode::from(2),
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = |i: usize| args.get(i).map(String::as_str);
    match (a(0), a(1)) {
        (Some("read"), Some(p)) => match std::fs::File::open(p).and_then(|mut f| {
            let mut b = [0u8; 1];
            f.read(&mut b)
        }) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => fail(format!("read {p}: {e}")),
        },
        (Some("write"), Some(p)) => match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .and_then(|mut f| f.write_all(b"x"))
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(format!("write {p}: {e}")),
        },
        (Some("connect"), Some(addr)) => {
            use std::net::{TcpStream, ToSocketAddrs};
            let resolved = addr.to_socket_addrs().ok().and_then(|mut it| it.next());
            match resolved {
                Some(sa) => match TcpStream::connect_timeout(&sa, Duration::from_secs(3)) {
                    Ok(_) => ExitCode::SUCCESS,
                    Err(e) => fail(format!("connect {addr}: {e}")),
                },
                None => fail(format!("connect {addr}: not a socket address")),
            }
        }
        #[cfg(unix)]
        (Some("unix"), Some(p)) => match std::os::unix::net::UnixStream::connect(p) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => fail(format!("unix {p}: {e}")),
        },
        (Some("exec"), Some(p)) => match Command::new(p).args(&args[2..]).status() {
            Ok(s) if s.success() => ExitCode::SUCCESS,
            Ok(s) => fail(format!("exec {p}: exited with {s}")),
            Err(e) => fail(format!("exec {p}: {e}")),
        },
        #[cfg(target_os = "macos")]
        (Some("mach"), Some(svc)) => match mach::look_up(svc) {
            0 => ExitCode::SUCCESS,
            rc => fail(format!("mach lookup {svc}: kern_return {rc}")),
        },
        #[cfg(unix)]
        (Some("signal"), Some(pid)) => match pid.parse::<i32>() {
            // SAFETY: signal 0 only checks permission; it delivers nothing.
            Ok(n) if unsafe { libc::kill(n, 0) } == 0 => ExitCode::SUCCESS,
            Ok(n) => fail(format!("kill({n}, 0): {}", std::io::Error::last_os_error())),
            Err(_) => ExitCode::from(2),
        },
        #[cfg(target_os = "macos")]
        (Some("pidinfo"), Some(pid)) => match pid.parse::<i32>() {
            Ok(n) => {
                let mut buf = vec![0u8; 4096];
                // SAFETY: `buf` is valid for `buf.len()` bytes.
                let len =
                    unsafe { libc::proc_pidpath(n, buf.as_mut_ptr().cast(), buf.len() as u32) };
                if len > 0 {
                    ExitCode::SUCCESS
                } else {
                    fail(format!(
                        "proc_pidpath({n}): {}",
                        std::io::Error::last_os_error()
                    ))
                }
            }
            Err(_) => ExitCode::from(2),
        },
        #[cfg(unix)]
        (Some("signal-child"), _) => match Command::new("/bin/sleep").arg("30").spawn() {
            Ok(mut child) => {
                // SAFETY: the pid is our own live child.
                let rc = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                let _ = child.wait();
                if rc == 0 {
                    ExitCode::SUCCESS
                } else {
                    fail(format!(
                        "signalling own child: {}",
                        std::io::Error::last_os_error()
                    ))
                }
            }
            Err(e) => fail(format!("spawn /bin/sleep: {e}")),
        },
        #[cfg(target_os = "macos")]
        (Some("rosetta"), _) => match Command::new("/usr/bin/arch")
            .args(["-x86_64", "/usr/bin/true"])
            .status()
        {
            Ok(s) if s.success() => ExitCode::SUCCESS,
            Ok(s) => fail(format!("arch -x86_64 /usr/bin/true: {s}")),
            Err(e) => fail(format!("arch: {e}")),
        },
        #[cfg(target_os = "linux")]
        (Some(act), arg) if linux::handles(act) => linux::run(act, arg),
        _ => {
            eprintln!("usage: ta-sandbox-probe <read|write|connect|unix|exec|mach|signal|pidinfo|signal-child|rosetta|syscall|ptrace|unshare-user|tiocsti|clone3-enosys|countpids|netdevs|listen|echo-proxy> [arg...]");
            ExitCode::from(2)
        }
    }
}
