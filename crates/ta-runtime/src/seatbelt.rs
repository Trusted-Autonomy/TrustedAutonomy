// seatbelt.rs: macOS Seatbelt (SBPL) profile generator, v2 (v0.17.12.7).
//
// Pure string generation: nothing here touches the sandbox, so the rules are
// unit-testable on every platform. The macOS-only conformance suite
// (`tests/seatbelt_conformance.rs` + the `ta-sandbox-probe` binary) proves the
// generated profile actually holds under `sandbox-exec`.
//
// ## Shape
//
// SBPL takes the LAST matching rule, so the order below is load-bearing:
//
//   1. `(deny default)`, untagged (a message here hides the tags on the explicit denies).
//   2. Broad reads, then process, signal and process-info rules.
//   3. A mach-lookup ALLOWLIST (never a blanket allow) and explicit denies
//      for Apple events, LaunchServices and exec of `ta-daemon` / `security`.
//   4. Targeted read+write DENIES for protected paths (other `.ta` content,
//      the TA state dir, credential stores, browser profiles).
//   5. Writes are deny-default: only the goal's staging, its agent home and
//      declared `allow_write` paths are granted, AFTER the denies so staging
//      (which lives inside the denied `.ta/` tree) survives them.
//   6. Network: every loopback address is denied, then only the per-goal
//      egress proxy port and the mediation socket are re-allowed.
//
// ## Paths
//
// Seatbelt matches the kernel's canonical path (`/private/tmp`, never `/tmp`;
// case as stored on disk, compared case-insensitively on APFS). A rule written
// with a symlinked prefix silently never matches, so every path is resolved
// through `canonical` before it is emitted.
//
// ## Reads are NOT confined
//
// Reads are allow-all with targeted denies, because a deny-default read set
// cannot start `/usr/bin/true` and agent toolchains live in too many places
// to enumerate. Anything not named in the deny set (for example another
// project's source tree) remains readable. This profile does not enforce
// `fs.read_confined`, and nothing may describe macOS reads as confined.

use std::path::{Path, PathBuf};

use crate::sandbox::SandboxPolicy;

/// Mach services the agent runtime was observed to need (measured with
/// violation logging against git, node, python, curl and the `claude` binary).
/// Everything else, notably `com.apple.SecurityServer` (Keychain), launchd
/// job submission, LaunchServices and the Apple-events services, is denied.
pub const ALLOWED_MACH_SERVICES: &[&str] = &[
    "com.apple.system.notification_center",
    "com.apple.logd",
    "com.apple.system.opendirectoryd.libinfo",
    "com.apple.system.opendirectoryd.membership",
    "com.apple.bsd.dirhelper",
    "com.apple.SystemConfiguration.configd",
    "com.apple.cfprefsd.daemon",
    "com.apple.trustd",
    "com.apple.trustd.agent",
    "com.apple.FSEvents",
];

/// Mach services that must stay unreachable. Not emitted as rules (default
/// deny already covers them, and an explicit deny placed after a future allow
/// would be redundant); the conformance suite and the unit tests assert both
/// that none of these is allowed and that they are denied at runtime.
pub const DENIED_MACH_SERVICES: &[&str] = &[
    "com.apple.SecurityServer",
    "com.apple.securityd",
    "com.apple.coreservices.launchservicesd",
    "com.apple.coreservices.appleevents",
    "com.apple.pasteboard.1",
];

/// Executables an agent may never launch directly. Defense in depth only: a
/// copy of the binary under another name is not caught by a path rule, which
/// is why `com.apple.SecurityServer` is also unreachable (a copied
/// `security` cannot talk to the Keychain). Recorded as a known residual risk.
pub const DENIED_EXEC_LITERALS: &[&str] = &["/usr/bin/security"];

/// Rosetta runtime locations: x86_64 agent binaries need these to translate.
pub const ROSETTA_PATHS: &[&str] = &[
    "/Library/Apple/usr/libexec/oah",
    "/usr/libexec/rosetta",
    "/private/var/db/oah",
];

/// Character devices an agent legitimately writes.
const WRITABLE_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/tty",
    "/dev/dtracehelper",
    "/dev/urandom",
    "/dev/random",
];

/// Escape a string for inclusion in an SBPL string literal.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

/// Resolve `path` the way the kernel will see it: symlinks followed in the
/// deepest existing ancestor, the not-yet-existing remainder appended as is.
pub fn canonical(path: &Path) -> PathBuf {
    let folded = lexical(path);
    let path: &Path = &folded;
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

/// Fold `.` and `..` textually so a not-yet-existing tail cannot hide them.
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn sb_path(path: &Path) -> String {
    escape(&canonical(path).to_string_lossy())
}

/// The tag appended to every denial of this profile in the unified log.
pub fn violation_tag(goal_id: &str) -> String {
    let short: String = goal_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(36)
        .collect();
    format!("ta:{short}")
}

/// Generate the Seatbelt profile for `policy`, confining `working_dir`.
pub fn generate(policy: &SandboxPolicy, working_dir: &Path) -> String {
    let mut l: Vec<String> = Vec::new();
    let workspace = sb_path(working_dir);

    // 1. Default deny. It stays untagged on purpose: probed on macOS, a message
    // on the default rule shadows the message on the explicit deny rules, and the
    // explicit denies (credentials, TA state) are the ones owners need attributed.
    l.push("(version 1)".into());
    l.push("(deny default)".into());

    // 2. Reads, processes, signals. Metadata is needed for even `/usr/bin/true`.
    l.push("(allow file-read*)".into());
    l.push("(allow file-read-metadata)".into());
    l.push("(allow sysctl-read)".into());
    l.push("(allow process-exec*)".into());
    l.push("(allow process-fork)".into());
    l.push("(allow ipc-posix-shm)".into());
    // Others first, then the allow for the box itself (last match wins).
    l.push("(deny signal (target others))".into());
    // `(target others)` does not stop proc_pidpath on a foreign pid (probed), so
    // the deny is unconditional and the allows below restore self and the box.
    l.push("(deny process-info*)".into());
    l.push("(allow signal (target same-sandbox))".into());
    l.push("(allow process-info* (target self))".into());
    l.push("(allow process-info-pidinfo (target same-sandbox))".into());

    // 3. Mach allowlist and the escape hatches that must stay shut.
    l.push("(deny appleevent-send)".into());
    l.push("(deny lsopen)".into());
    l.push("(allow mach-lookup".into());
    for svc in ALLOWED_MACH_SERVICES {
        l.push(format!(r#"    (global-name "{}")"#, escape(svc)));
    }
    l.push(")".into());
    for exe in DENIED_EXEC_LITERALS {
        l.push(format!(
            r#"(deny process-exec (literal "{}"))"#,
            escape(exe)
        ));
    }
    l.push(r#"(deny process-exec (regex #"/ta-daemon$"))"#.into());

    // Rosetta: translated x86_64 agents. Read is already broad; execute and map.
    for p in ROSETTA_PATHS {
        l.push(format!(
            r#"(allow file-map-executable (subpath "{}"))"#,
            escape(p)
        ));
    }

    // Devices. Everything else under /dev stays read-only.
    for dev in WRITABLE_DEVICES {
        l.push(format!(
            r#"(allow file-write* file-ioctl (literal "{}"))"#,
            escape(dev)
        ));
    }
    l.push(r#"(allow file-write* file-ioctl (regex #"^/dev/ttys[0-9]+$"))"#.into());

    // 4. Targeted denies (read AND write). Drop duplicates after canonicalizing.
    let mut denied: Vec<String> = Vec::new();
    for p in &policy.deny_paths {
        let s = sb_path(p);
        if !denied.contains(&s) {
            denied.push(s);
        }
    }
    for s in &denied {
        l.push(format!(r#"(deny file-read* file-write* (subpath "{s}"))"#));
    }

    // 5. Writes: deny-default; grants come after the denies. Declared
    // `allow_read` paths are already readable (reads are allow-all).
    l.push(format!(
        r#"(allow file-read* file-write* (subpath "{workspace}"))"#
    ));
    for p in policy.allow_write.iter().chain(policy.goal_paths.iter()) {
        l.push(format!(
            r#"(allow file-read* file-write* (subpath "{}"))"#,
            sb_path(p)
        ));
    }

    // 6. Network. A grant of "*" is the owner's explicit open-network choice
    // and still never reaches loopback (the daemon and NATS live there).
    if policy.allow_network.iter().any(|h| h == "*") {
        l.push("(allow network*)".into());
    }
    l.push(r#"(deny network* (remote ip "localhost:*"))"#.into());
    if let Some(port) = policy.egress_proxy_port {
        l.push(format!(
            r#"(allow network-outbound (remote ip "localhost:{port}"))"#
        ));
    }
    for sock in &policy.unix_sockets {
        l.push(format!(
            r#"(allow network-outbound (remote unix-socket (path-literal "{}")))"#,
            sb_path(sock)
        ));
    }

    // Explicit denies carry the goal tag too: only the default rule's message
    // would otherwise reach the log, and most real denials are explicit. The
    // modifier goes right after the operations; placed after the filters the
    // kernel was observed not to log the denial at all.
    if let Some(tag) = &policy.violation_tag {
        let modifier = format!(r#" (with message "{}")"#, escape(tag));
        for line in l.iter_mut() {
            if !line.starts_with("(deny ") || line.starts_with("(deny default") {
                continue;
            }
            match line.find(" (") {
                Some(at) => line.insert_str(at, &modifier),
                None => {
                    line.pop();
                    line.push_str(&modifier);
                    line.push(')');
                }
            }
        }
    }

    l.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxProvider;

    fn policy() -> SandboxPolicy {
        SandboxPolicy {
            enabled: true,
            provider: SandboxProvider::MacosSandboxExec,
            allow_read: Vec::new(),
            allow_write: Vec::new(),
            allow_network: Vec::new(),
            deny_paths: Vec::new(),
            goal_paths: Vec::new(),
            unix_sockets: Vec::new(),
            egress_proxy_port: None,
            egress_unix_socket: None,
            windows: Default::default(),
            violation_tag: None,
        }
    }

    fn pos(profile: &str, needle: &str) -> usize {
        profile
            .find(needle)
            .unwrap_or_else(|| panic!("profile lacks {needle:?}:\n{profile}"))
    }

    #[test]
    fn escape_handles_quotes_backslashes_and_newlines() {
        assert_eq!(escape(r#"/a "b" \c"#), r#"/a \"b\" \\c"#);
        assert_eq!(escape("/a\nb"), "/a\\nb");
    }

    #[test]
    fn canonical_resolves_symlinked_prefix_and_missing_tail() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert_eq!(
                canonical(&link.join("not/yet/there")),
                real.join("not/yet/there")
            );
        }
        assert_eq!(canonical(&dir.path().join("x")), real.join("x"));
    }

    #[test]
    fn violation_tag_is_sanitized() {
        assert_eq!(violation_tag("e0d1-6a\"b"), "ta:e0d1-6ab");
    }

    #[test]
    fn explicit_denies_carry_the_tag_and_the_default_rule_does_not() {
        let mut p = policy();
        p.violation_tag = Some("ta:goal-1".into());
        let out = generate(&p, Path::new("/tmp/ws"));
        // Probed on macOS: a message on `(deny default ...)` suppresses the tag on
        // the explicit deny rules (credential and state denials never reach the
        // log), so the default rule stays plain.
        assert!(out.lines().any(|l| l == "(deny default)"), "{out}");
        assert!(!out.contains(r#"(deny default (with message"#), "{out}");
        assert!(out.contains(r#"(deny signal (with message "ta:goal-1") (target others))"#));
        // Every other (deny ...) stays tagged and balanced.
        for line in out
            .lines()
            .filter(|l| l.starts_with("(deny ") && *l != "(deny default)")
        {
            assert!(line.contains(r#"(with message "ta:goal-1")"#), "{line}");
        }
    }

    #[test]
    fn mach_lookup_is_an_allowlist_never_blanket() {
        let out = generate(&policy(), Path::new("/tmp/ws"));
        assert!(!out.contains("(allow mach-lookup)"), "{out}");
        for svc in ALLOWED_MACH_SERVICES {
            assert!(out.contains(&format!(r#"(global-name "{svc}")"#)), "{svc}");
        }
        for svc in DENIED_MACH_SERVICES {
            assert!(!out.contains(svc), "{svc} must not be allowed");
        }
    }

    #[test]
    fn escape_hatches_are_denied() {
        let out = generate(&policy(), Path::new("/tmp/ws"));
        for rule in [
            "(deny appleevent-send)",
            "(deny lsopen)",
            "(deny signal (target others))",
            "(deny process-info*)",
            r#"(deny process-exec (literal "/usr/bin/security"))"#,
            r#"(deny process-exec (regex #"/ta-daemon$"))"#,
        ] {
            assert!(out.contains(rule), "missing {rule}");
        }
        // "others" is denied before same-sandbox is allowed (last match wins).
        assert!(
            pos(&out, "(deny signal (target others))")
                < pos(&out, "(allow signal (target same-sandbox))")
        );
    }

    #[test]
    fn writes_are_deny_default_and_scratch_is_per_goal() {
        let mut p = policy();
        p.goal_paths = vec![PathBuf::from("/tmp/ta-home/goal1")];
        let out = generate(&p, Path::new("/tmp/ws"));
        assert!(
            !out.contains("(subpath \"/private/tmp\")"),
            "shared /tmp must not be writable"
        );
        assert!(!out.contains(r#"(allow file-write* (subpath "/dev"))"#));
        assert!(!out.contains("(allow file-write*)"));
        assert!(out.contains(r#"(allow file-write* file-ioctl (literal "/dev/null"))"#));
    }

    #[test]
    fn denies_come_before_goal_grants_so_staging_survives() {
        let home = tempfile::tempdir().unwrap();
        let ssh_path = canonical(&home.path().join(".ssh"));
        // The generator canonicalizes every path. On Windows that rewrites a
        // Unix-looking literal (drive prefix, backslashes), so the expected
        // text is built from the same function instead of hardcoded.
        let ta_dir = canonical(Path::new("/proj/.ta"));
        let staging = canonical(Path::new("/proj/.ta/staging/g1"));
        let mut p = policy();
        p.deny_paths = vec![PathBuf::from("/proj/.ta"), home.path().join(".ssh")];
        let out = generate(&p, Path::new("/proj/.ta/staging/g1"));
        let deny = pos(
            &out,
            &format!(
                r#"(deny file-read* file-write* (subpath "{}"))"#,
                escape(&ta_dir.to_string_lossy())
            ),
        );
        let ssh = pos(
            &out,
            &format!(r#"(subpath "{}")"#, escape(&ssh_path.to_string_lossy())),
        );
        let grant = pos(
            &out,
            &format!(
                r#"(allow file-read* file-write* (subpath "{}"))"#,
                escape(&staging.to_string_lossy())
            ),
        );
        assert!(deny < grant && ssh < grant);
    }

    #[test]
    fn duplicate_denies_collapse_after_canonicalization() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = policy();
        p.deny_paths = vec![dir.path().join("a"), dir.path().join("a/../a")];
        let out = generate(&p, Path::new("/tmp/ws"));
        assert_eq!(
            out.matches("(deny file-read* file-write*").count(),
            1,
            "{out}"
        );
    }

    #[test]
    fn loopback_denied_then_only_proxy_and_socket_allowed() {
        let mut p = policy();
        p.egress_proxy_port = Some(40123);
        p.unix_sockets = vec![PathBuf::from("/tmp/ws/.ta-mcp.sock")];
        let out = generate(&p, Path::new("/tmp/ws"));
        let deny = pos(&out, r#"(deny network* (remote ip "localhost:*"))"#);
        let proxy = pos(
            &out,
            r#"(allow network-outbound (remote ip "localhost:40123"))"#,
        );
        let sock = pos(&out, "(remote unix-socket (path-literal ");
        assert!(deny < proxy && deny < sock);
        assert!(!out.contains("(allow network-outbound)"));
        assert!(!out.contains("(allow network*)"));
    }

    #[test]
    fn no_network_grant_means_no_allow_rule() {
        let out = generate(&policy(), Path::new("/tmp/ws"));
        assert!(!out.contains("(allow network"));
        assert!(out.contains(r#"(deny network* (remote ip "localhost:*"))"#));
    }

    #[test]
    fn wildcard_grant_still_denies_loopback_afterwards() {
        let mut p = policy();
        p.allow_network = vec!["*".into()];
        let out = generate(&p, Path::new("/tmp/ws"));
        assert!(
            pos(&out, "(allow network*)")
                < pos(&out, r#"(deny network* (remote ip "localhost:*"))"#)
        );
    }

    #[test]
    fn rosetta_runtime_is_mappable() {
        let out = generate(&policy(), Path::new("/tmp/ws"));
        for p in ROSETTA_PATHS {
            assert!(out.contains(&format!(r#"(allow file-map-executable (subpath "{p}"))"#)));
        }
    }

    #[test]
    fn path_with_quote_cannot_break_out_of_the_literal() {
        let mut p = policy();
        p.deny_paths = vec![PathBuf::from("/x\") (allow default) (\"")];
        let out = generate(&p, Path::new("/tmp/ws"));
        assert!(!out.contains("(allow default)\n"), "{out}");
        assert!(out.contains(r#"\") (allow default) (\""#));
    }
}
