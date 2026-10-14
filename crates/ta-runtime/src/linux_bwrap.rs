// linux_bwrap.rs — bubblewrap command construction and the shared filesystem view (v0.17.12.8)
//
// Everything here is pure: it turns a `SandboxPolicy` into a list of paths and
// a `bwrap` argument vector without touching the kernel, so it is unit tested on
// every platform. The same `FsView` feeds both the bubblewrap mounts and the
// Landlock rules the helper applies, so the two layers cannot disagree about
// what the agent may see.
//
// The root is constructed, not inherited: bwrap starts from an empty tmpfs and
// mounts only what is listed. `.ta/`, the TA state directory and credential
// stores are never mounted. Where a denied path lies inside a mounted tree (the
// staging directory carries its own `.ta/`), it is covered with an empty tmpfs
// or `/dev/null`, and the per-goal paths are mounted again on top.

use std::path::{Path, PathBuf};

use crate::adapter::SpawnRequest;
use crate::linux_confine::{HelperArgs, LandlockMode, SeccompMode};
use crate::linux_probe::HelperCommand;
use crate::sandbox::SandboxPolicy;

/// System locations an agent toolchain needs, mounted read-only when present.
pub const SYSTEM_RO: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/nix",
    "/opt",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/pki",
    "/etc/alternatives",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/passwd",
    "/etc/group",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/localtime",
    "/etc/gitconfig",
    "/etc/os-release",
    "/etc/terminfo",
];

/// The filesystem the agent sees, as three disjoint-purpose lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FsView {
    /// The agent's workspace (the goal's staging dir). It usually lies inside a
    /// denied tree (`<project>/.ta/`), so it is granted again after the denies.
    pub workspace: PathBuf,
    pub ro: Vec<PathBuf>,
    pub rw: Vec<PathBuf>,
    /// Hidden inside mounted trees (bubblewrap) or carved out of the Landlock
    /// allow list (Landlock-only, which has no deny rule).
    pub deny: Vec<PathBuf>,
    /// Re-granted read-write after `deny` (the goal's own staging and home).
    pub goal: Vec<PathBuf>,
}

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    if !v.contains(&p) {
        v.push(p);
    }
}

/// True when `child` is `parent` or lies beneath it (lexical, no symlinks).
pub fn within(child: &Path, parent: &Path) -> bool {
    child.starts_with(parent)
}

/// Build the view. `working_dir` is the agent's workspace (always read-write).
pub fn fs_view(policy: &SandboxPolicy, working_dir: &Path) -> FsView {
    let mut v = FsView {
        workspace: working_dir.to_path_buf(),
        ..FsView::default()
    };
    for s in SYSTEM_RO {
        let p = PathBuf::from(s);
        if p.exists() {
            push_unique(&mut v.ro, p);
        }
    }
    for p in &policy.allow_read {
        if p.exists() {
            push_unique(&mut v.ro, p.clone());
        }
    }
    push_unique(&mut v.rw, working_dir.to_path_buf());
    for p in &policy.allow_write {
        if p.exists() {
            push_unique(&mut v.rw, p.clone());
        }
    }
    for p in &policy.deny_paths {
        push_unique(&mut v.deny, p.clone());
    }
    for p in &policy.goal_paths {
        if p.exists() {
            push_unique(&mut v.goal, p.clone());
        }
    }
    // A path that is both writable and readable is just writable.
    let rw = v.rw.clone();
    v.ro.retain(|r| !rw.iter().any(|w| within(r, w)));
    v
}

/// Landlock has allow rules only. To hide a path inside an allowed tree, list
/// the tree's other entries instead of the tree itself, recursively, until the
/// denied path is the only thing left out.
pub fn carve_denies(allowed: &[PathBuf], deny: &[PathBuf]) -> Vec<PathBuf> {
    fn carve(dir: &Path, deny: &[PathBuf], out: &mut Vec<PathBuf>, budget: &mut usize) {
        if deny.iter().any(|d| within(dir, d)) {
            return; // entirely denied
        }
        if !deny.iter().any(|d| within(d, dir)) {
            out.push(dir.to_path_buf());
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return; // unreadable: grant nothing rather than too much
        };
        for e in rd.flatten() {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let p = e.path();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                carve(&p, deny, out, budget);
            } else if !deny.iter().any(|d| within(&p, d)) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    let mut budget = 20_000usize;
    for a in allowed {
        carve(a, deny, &mut out, &mut budget);
    }
    out
}

/// Like `carve_denies` for a path that is granted on purpose even though a
/// denied tree contains it: only denies strictly inside the path apply.
pub fn carve_regrant(path: &Path, deny: &[PathBuf]) -> Vec<PathBuf> {
    let inside: Vec<PathBuf> = deny
        .iter()
        .filter(|d| within(d, path) && *d != path)
        .cloned()
        .collect();
    carve_denies(&[path.to_path_buf()], &inside)
}

/// Denied paths that Landlock-only mode cannot enforce because they sit inside
/// the workspace. Landlock rules inherit downward and a carved directory cannot
/// receive new files, so the workspace is granted whole. The bubblewrap provider
/// hides these paths with a mount and has no such gap.
pub fn workspace_inner_denies(view: &FsView) -> Vec<PathBuf> {
    view.deny
        .iter()
        .filter(|d| within(d, &view.workspace) && **d != view.workspace)
        .cloned()
        .collect()
}

/// Arguments for `bwrap` itself (everything before the `--` and the helper).
///
/// `egress_dir` is the host directory holding the relay socket, bind-mounted so
/// the in-namespace relay can reach it.
pub fn bwrap_args(
    view: &FsView,
    policy: &SandboxPolicy,
    working_dir: &Path,
    helper_program: &Path,
    egress_dir: Option<&Path>,
) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut flag = |s: &str| a.push(s.to_string());
    flag("--unshare-user");
    flag("--unshare-pid");
    flag("--unshare-ipc");
    flag("--unshare-uts");
    flag("--unshare-cgroup-try");
    if !network_open(policy) {
        flag("--unshare-net");
    }
    flag("--die-with-parent");
    flag("--cap-drop");
    flag("ALL");
    flag("--hostname");
    flag("ta-sandbox");
    // Constructed root: an empty tmpfs, then only what is listed.
    flag("--tmpfs");
    flag("/");
    flag("--dev");
    flag("/dev");
    flag("--proc");
    flag("/proc");
    flag("--tmpfs");
    flag("/tmp");

    let mut mount = |kind: &str, p: &Path| {
        a.push(kind.to_string());
        a.push(p.to_string_lossy().into_owned());
        a.push(p.to_string_lossy().into_owned());
    };
    for p in &view.ro {
        mount("--ro-bind-try", p);
    }
    // The helper must be runnable inside the namespace.
    if !view.ro.iter().any(|r| within(helper_program, r)) {
        mount("--ro-bind-try", helper_program);
    }
    for p in &view.rw {
        mount("--bind-try", p);
    }
    // Hide denied paths that fall inside something mounted above.
    let mounted: Vec<&PathBuf> = view.ro.iter().chain(view.rw.iter()).collect();
    for d in &view.deny {
        if !mounted.iter().any(|m| within(d, m)) || !d.exists() {
            continue; // never mounted, nothing to hide
        }
        if d.is_dir() {
            a.push("--tmpfs".into());
            a.push(d.to_string_lossy().into_owned());
        } else {
            a.push("--ro-bind".into());
            a.push("/dev/null".into());
            a.push(d.to_string_lossy().into_owned());
        }
    }
    let mut mount = |kind: &str, p: &Path| {
        a.push(kind.to_string());
        a.push(p.to_string_lossy().into_owned());
        a.push(p.to_string_lossy().into_owned());
    };
    for p in &view.goal {
        mount("--bind-try", p);
    }
    for sock in &policy.unix_sockets {
        if let Some(dir) = sock.parent() {
            mount("--bind-try", dir);
        }
    }
    if let Some(dir) = egress_dir {
        mount("--bind-try", dir);
    }
    a.push("--chdir".into());
    a.push(working_dir.to_string_lossy().into_owned());
    a
}

/// True when the policy allows unrestricted outbound network.
pub fn network_open(policy: &SandboxPolicy) -> bool {
    policy.allow_network.iter().any(|h| h == "*")
}

/// What the helper must enforce for this view. `under_bwrap` selects the
/// default Landlock mode: best-effort under bwrap (the mount namespace already
/// confines the filesystem), required when Landlock is the only layer.
pub fn helper_args(
    view: &FsView,
    policy: &SandboxPolicy,
    under_bwrap: bool,
    command: Vec<String>,
) -> HelperArgs {
    let (ro, rw) = if under_bwrap {
        // The mount namespace already hides denied paths; Landlock mirrors it.
        let mut rw = view.rw.clone();
        for g in &view.goal {
            push_unique(&mut rw, g.clone());
        }
        (view.ro.clone(), rw)
    } else {
        let mut rw = carve_denies(&view.rw, &view.deny);
        for g in view.goal.iter().chain(std::iter::once(&view.workspace)) {
            if *g == view.workspace {
                // New files in a carved directory would have no rule, and a
                // rule on the directory re-exposes its denied children, so the
                // workspace is granted whole. See `workspace_inner_denies`.
                push_unique(&mut rw, g.clone());
                continue;
            }
            for p in carve_regrant(g, &view.deny) {
                push_unique(&mut rw, p);
            }
        }
        (carve_denies(&view.ro, &view.deny), rw)
    };
    let mut ro = ro;
    let mut rw = rw;
    // The process and device trees every program needs. `/proc` is the private
    // procfs under bwrap; `/tmp` is bwrap's private tmpfs, so it is safe to
    // write (Landlock-only has only the host's `/tmp` and does not grant it).
    push_unique(&mut ro, PathBuf::from("/proc"));
    if under_bwrap {
        push_unique(&mut rw, PathBuf::from("/tmp"));
    }
    // Sockets the agent may connect to need their directory reachable.
    for s in &policy.unix_sockets {
        if let Some(d) = s.parent() {
            push_unique(&mut ro, d.to_path_buf());
        }
    }
    let egress_socket = policy.egress_unix_socket.clone();
    let egress_port = policy.egress_proxy_port;
    if let Some(s) = &egress_socket {
        if let Some(d) = s.parent() {
            push_unique(&mut ro, d.to_path_buf());
        }
    }
    let restrict_net = !under_bwrap && !network_open(policy);
    HelperArgs {
        ro,
        rw,
        dev: vec![PathBuf::from("/dev")],
        landlock: if under_bwrap {
            LandlockMode::Auto
        } else {
            LandlockMode::Require
        },
        restrict_net,
        connect_ports: if restrict_net {
            policy.egress_proxy_port.into_iter().collect()
        } else {
            Vec::new()
        },
        seccomp: if std::env::var_os("TA_SANDBOX_SECCOMP_LOG").is_some() {
            SeccompMode::Log
        } else {
            SeccompMode::Enforce
        },
        // Only the namespace sandbox needs the in-namespace relay.
        egress_socket: if under_bwrap { egress_socket } else { None },
        egress_port: if under_bwrap { egress_port } else { None },
        command,
    }
}

/// Wrap `request` as `bwrap ... -- helper ... -- command args`.
pub fn wrap_bwrap(
    mut request: SpawnRequest,
    policy: &SandboxPolicy,
    bwrap: &Path,
    helper: &HelperCommand,
) -> SpawnRequest {
    let view = fs_view(policy, &request.working_dir);
    let egress_dir = policy
        .egress_unix_socket
        .as_ref()
        .and_then(|s| s.parent().map(Path::to_path_buf));
    let mut args = bwrap_args(
        &view,
        policy,
        &request.working_dir,
        &helper.program,
        egress_dir.as_deref(),
    );
    let mut cmd = vec![request.command.clone()];
    cmd.extend(request.args.iter().cloned());
    args.push("--".into());
    args.push(helper.program.to_string_lossy().into_owned());
    args.extend(helper.prefix_args.iter().cloned());
    args.extend(helper_args(&view, policy, true, cmd).to_args());
    request.command = bwrap.to_string_lossy().into_owned();
    request.args = args;
    request
}

/// Wrap `request` as `helper ... -- command args` (no namespaces).
pub fn wrap_landlock_only(
    mut request: SpawnRequest,
    policy: &SandboxPolicy,
    helper: &HelperCommand,
) -> SpawnRequest {
    let view = fs_view(policy, &request.working_dir);
    let gaps = workspace_inner_denies(&view);
    if !gaps.is_empty() {
        tracing::warn!(
            paths = ?gaps,
            "Landlock-only sandbox cannot hide denied paths inside the workspace; they stay \
             reachable. Install bubblewrap (and allow user namespaces) for full isolation"
        );
    }
    let mut cmd = vec![request.command.clone()];
    cmd.extend(request.args.iter().cloned());
    let mut args: Vec<String> = helper.prefix_args.clone();
    args.extend(helper_args(&view, policy, false, cmd).to_args());
    request.command = helper.program.to_string_lossy().into_owned();
    request.args = args;
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn policy(dir: &Path) -> SandboxPolicy {
        let mut p = SandboxPolicy::disabled();
        p.enabled = true;
        p.allow_read = vec![dir.join("read")];
        p.allow_write = vec![dir.join("write")];
        p.deny_paths = vec![
            dir.join("ws").join("private"),
            dir.join("home").join(".ssh"),
        ];
        p.goal_paths = vec![dir.join("goalhome")];
        std::fs::create_dir_all(dir.join("read")).unwrap();
        std::fs::create_dir_all(dir.join("write")).unwrap();
        std::fs::create_dir_all(dir.join("goalhome")).unwrap();
        std::fs::create_dir_all(dir.join("ws").join("private")).unwrap();
        std::fs::create_dir_all(dir.join("home").join(".ssh")).unwrap();
        p
    }

    fn helper() -> HelperCommand {
        HelperCommand {
            program: PathBuf::from("/opt/ta/ta-sandbox-exec"),
            prefix_args: Vec::new(),
        }
    }

    fn request(ws: &Path) -> SpawnRequest {
        SpawnRequest {
            command: "claude".into(),
            args: vec!["--print".into()],
            env: HashMap::new(),
            working_dir: ws.to_path_buf(),
            stdin_mode: crate::adapter::StdinMode::Null,
            stdout_mode: crate::adapter::StdoutMode::Inherited,
            clear_env: false,
        }
    }

    fn pairs(args: &[String], flag: &str) -> Vec<String> {
        args.windows(3)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].clone())
            .collect()
    }

    #[test]
    fn isolates_every_namespace_the_spec_names() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        let ws = d.path().join("ws");
        let v = fs_view(&p, &ws);
        let a = bwrap_args(&v, &p, &ws, &helper().program, None);
        for f in [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-net",
            "--die-with-parent",
        ] {
            assert!(a.contains(&f.to_string()), "missing {f}: {a:?}");
        }
        let i = a.iter().position(|x| x == "--cap-drop").unwrap();
        assert_eq!(a[i + 1], "ALL");
    }

    #[test]
    fn root_is_constructed_with_private_proc_and_tmpfs_tmp() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        let v = fs_view(&p, &d.path().join("ws"));
        let a = bwrap_args(&v, &p, &d.path().join("ws"), &helper().program, None);
        let has = |x: &str, y: &str| a.windows(2).any(|w| w[0] == x && w[1] == y);
        assert!(has("--tmpfs", "/"), "root must start empty");
        assert!(has("--proc", "/proc"));
        assert!(has("--tmpfs", "/tmp"));
        assert!(has("--dev", "/dev"));
        // No whole-root bind of the host.
        assert!(!a
            .windows(3)
            .any(|w| w[0].starts_with("--") && w[0].contains("bind") && w[1] == "/"));
    }

    #[test]
    fn denied_dirs_inside_mounts_are_covered_and_goal_paths_remounted_after() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        let ws = d.path().join("ws");
        let v = fs_view(&p, &ws);
        let a = bwrap_args(&v, &p, &ws, &helper().program, None);
        let ta = ws.join("private").to_string_lossy().into_owned();
        let goal = d.path().join("goalhome").to_string_lossy().into_owned();
        let cover = a
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == ta)
            .expect("a denied dir inside the workspace must be covered");
        let regrant = a
            .windows(2)
            .position(|w| w[0] == "--bind-try" && w[1] == goal)
            .expect("goal path must be mounted");
        assert!(regrant > cover, "goal path must be mounted after the cover");
        // ~/.ssh was never inside a mount here, so there is nothing to cover.
        let ssh = d
            .path()
            .join("home")
            .join(".ssh")
            .to_string_lossy()
            .into_owned();
        assert!(!a.contains(&ssh));
    }

    #[test]
    fn read_and_write_lists_use_the_right_bind_kind() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        let ws = d.path().join("ws");
        let v = fs_view(&p, &ws);
        let a = bwrap_args(&v, &p, &ws, &helper().program, None);
        let ro = pairs(&a, "--ro-bind-try");
        let rw = pairs(&a, "--bind-try");
        assert!(ro.contains(&d.path().join("read").to_string_lossy().into_owned()));
        assert!(rw.contains(&ws.to_string_lossy().into_owned()));
        assert!(rw.contains(&d.path().join("write").to_string_lossy().into_owned()));
        assert!(!ro.contains(&ws.to_string_lossy().into_owned()));
    }

    #[test]
    fn open_network_policy_keeps_the_network_namespace_shared() {
        let d = tempfile::tempdir().unwrap();
        let mut p = policy(d.path());
        p.allow_network = vec!["*".into()];
        let ws = d.path().join("ws");
        let v = fs_view(&p, &ws);
        let a = bwrap_args(&v, &p, &ws, &helper().program, None);
        assert!(!a.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn egress_dir_and_mediation_socket_dir_are_mounted() {
        let d = tempfile::tempdir().unwrap();
        let mut p = policy(d.path());
        let med = d.path().join("med");
        std::fs::create_dir_all(&med).unwrap();
        p.unix_sockets = vec![med.join("m.sock")];
        let egress = d.path().join("eg");
        std::fs::create_dir_all(&egress).unwrap();
        let ws = d.path().join("ws");
        let v = fs_view(&p, &ws);
        let a = bwrap_args(&v, &p, &ws, &helper().program, Some(&egress));
        let rw = pairs(&a, "--bind-try");
        assert!(rw.contains(&med.to_string_lossy().into_owned()));
        assert!(rw.contains(&egress.to_string_lossy().into_owned()));
    }

    #[test]
    fn wrapped_command_runs_helper_then_agent_after_separator() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        let r = wrap_bwrap(
            request(&d.path().join("ws")),
            &p,
            Path::new("/usr/bin/bwrap"),
            &helper(),
        );
        assert_eq!(r.command, "/usr/bin/bwrap");
        let sep = r.args.iter().position(|x| x == "--").unwrap();
        assert_eq!(r.args[sep + 1], "/opt/ta/ta-sandbox-exec");
        // The agent command is last, after the helper's own separator.
        assert_eq!(&r.args[r.args.len() - 2..], ["claude", "--print"]);
        let parsed = HelperArgs::parse(&r.args[sep + 2..]).unwrap();
        assert_eq!(parsed.command, vec!["claude", "--print"]);
        assert_eq!(parsed.landlock, LandlockMode::Auto);
    }

    #[test]
    fn landlock_only_requires_landlock_and_restricts_tcp_to_the_proxy_port() {
        let d = tempfile::tempdir().unwrap();
        let mut p = policy(d.path());
        p.egress_proxy_port = Some(41234);
        let r = wrap_landlock_only(request(&d.path().join("ws")), &p, &helper());
        assert_eq!(r.command, "/opt/ta/ta-sandbox-exec");
        let parsed = HelperArgs::parse(&r.args).unwrap();
        assert_eq!(parsed.landlock, LandlockMode::Require, "never best-effort");
        assert!(parsed.restrict_net);
        assert_eq!(parsed.connect_ports, vec![41234]);
        assert!(parsed.egress_socket.is_none(), "no relay without a netns");
    }

    #[test]
    fn landlock_only_carves_denied_paths_out_of_allowed_trees() {
        let d = tempfile::tempdir().unwrap();
        let mut p = policy(d.path());
        let ws = d.path().join("ws");
        let w = d.path().join("write");
        let private = w.join("private");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(w.join("keep.txt"), "x").unwrap();
        std::fs::create_dir_all(w.join("src")).unwrap();
        std::fs::write(private.join("secret"), "x").unwrap();
        p.deny_paths.push(private.clone());
        let r = wrap_landlock_only(request(&ws), &p, &helper());
        let parsed = HelperArgs::parse(&r.args).unwrap();
        assert!(!parsed.rw.contains(&w), "the tree is no longer one rule");
        assert!(parsed.rw.contains(&w.join("keep.txt")));
        assert!(parsed.rw.contains(&w.join("src")));
        for path in parsed.rw.iter().chain(parsed.ro.iter()) {
            assert!(
                !within(path, &private),
                "{} leaks the denied tree",
                path.display()
            );
        }
    }

    #[test]
    fn workspace_under_a_denied_ancestor_is_granted_whole_and_inner_denies_are_reported() {
        // The real layout: staging lives inside the denied `<project>/.ta/`.
        let d = tempfile::tempdir().unwrap();
        let ta = d.path().join("proj").join(".ta");
        let ws = ta.join("staging").join("g1");
        std::fs::create_dir_all(ws.join("private")).unwrap();
        std::fs::write(ws.join("a.txt"), "x").unwrap();
        std::fs::write(ta.join("workflow.toml"), "x").unwrap();
        let mut p = SandboxPolicy::disabled();
        p.enabled = true;
        p.deny_paths = vec![ta.clone(), ws.join("private")];
        let r = wrap_landlock_only(request(&ws), &p, &helper());
        let parsed = HelperArgs::parse(&r.args).unwrap();
        // Granted whole so new files can be created in the workspace root.
        assert!(parsed.rw.contains(&ws), "{:?}", parsed.rw);
        for path in &parsed.rw {
            assert!(
                !within(path, &ta) || within(path, &ws),
                "{} reaches the denied project tree",
                path.display()
            );
        }
        let view = fs_view(&p, &ws);
        assert_eq!(workspace_inner_denies(&view), vec![ws.join("private")]);
        assert!(!parsed.rw.contains(&ta.join("workflow.toml")));
    }

    #[test]
    fn carve_leaves_trees_without_denies_whole() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        assert_eq!(
            carve_denies(std::slice::from_ref(&a), &[d.path().join("elsewhere")]),
            vec![a]
        );
    }

    #[test]
    fn carve_drops_an_allowed_path_that_is_itself_denied() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        assert!(carve_denies(std::slice::from_ref(&a), &[d.path().to_path_buf()]).is_empty());
    }

    #[test]
    fn seccomp_log_mode_is_opt_in_only() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        let v = fs_view(&p, &d.path().join("ws"));
        if std::env::var_os("TA_SANDBOX_SECCOMP_LOG").is_none() {
            assert_eq!(
                helper_args(&v, &p, true, vec!["x".into()]).seccomp,
                SeccompMode::Enforce
            );
        }
    }

    #[test]
    fn egress_relay_args_only_with_bwrap() {
        let d = tempfile::tempdir().unwrap();
        let mut p = policy(d.path());
        p.egress_proxy_port = Some(5555);
        p.egress_unix_socket = Some(d.path().join("eg").join("egress.sock"));
        let v = fs_view(&p, &d.path().join("ws"));
        let with = helper_args(&v, &p, true, vec!["x".into()]);
        assert_eq!(with.egress_port, Some(5555));
        assert!(with.egress_socket.is_some());
        assert!(!with.restrict_net);
        let without = helper_args(&v, &p, false, vec!["x".into()]);
        assert!(without.egress_socket.is_none());
        assert!(without.restrict_net);
    }
}
