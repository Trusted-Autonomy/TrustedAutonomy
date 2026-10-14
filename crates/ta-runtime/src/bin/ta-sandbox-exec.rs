// ta-sandbox-exec: the in-namespace half of the Linux sandbox (v0.17.12.8).
//
// `bwrap` starts this program inside the new namespaces. It applies Landlock and
// the seccomp deny list to itself, then execs the agent, so the agent inherits
// both and cannot lift either. If a required layer cannot be applied it exits
// 125 without starting the agent.
//
// `ta sandbox-exec-helper` is the same code reachable through the main binary,
// for installs that ship only `ta`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(ta_runtime::linux_confine::run_helper(&args));
}
