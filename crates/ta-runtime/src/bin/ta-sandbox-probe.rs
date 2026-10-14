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
        _ => {
            eprintln!("usage: ta-sandbox-probe <read|write|connect|unix|exec|mach|signal|pidinfo|signal-child|rosetta> [arg...]");
            ExitCode::from(2)
        }
    }
}
