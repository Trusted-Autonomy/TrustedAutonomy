# OS-Level Sandboxing for Agent Processes: Cross-Platform Plan

**Date:** 2026-10-07
**Status:** proposal (docs only, no code in this change)
**Closes (when delivered):** red-team findings CR-02, CR-03, CR-12, CR-15 and CR-42 from `docs/security/2026-10-07-cos-pipeline-redteam.md` (branch `docs/cos-redteam-2026-10-07`), the isolation half of TA-04 and the agent-facing part of TA-06 (both deferred to "SA" in PLAN.md v0.17.11.4 item 8).
**Builds on:** `crates/ta-runtime/src/sandbox.rs`, `crates/ta-runtime/src/sandbox_windows.rs`, `crates/ta-sandbox` (command allowlist), `crates/ta-credentials` (`keyring` custody), `crates/ta-credential-broker`, `crates/ta-daemon/src/transport.rs` (Unix socket MCP transport), the compiled-constitution design on branch `docs/compiled-constitution-design`, and `docs/superpowers/specs/2026-08-03-agent-credential-security-design.md`.

---

## 1. The problem in one paragraph

Every agent TA launches runs as the same OS user as TA's own control plane (`ta`, `ta-daemon`, `ta serve`, NATS). Staging lives at `<project>/.ta/staging/<goal>/`, two directories below the files that make TA's decisions trustworthy: `broker_root.key`, `audit.key`, `daemon-tokens.json`, `tokens/*`, team-session `state.json` bearer tokens, persona and agent YAML, `pr_packages/*.json`, and the NATS JetStream data. The daemon listens on `127.0.0.1:7700` and, with `local_bypass`, grants Admin to any loopback caller. So an agent with a shell can mint capability tokens (CR-03), approve and apply its own draft (CR-02), widen the next launch by editing persona or agent config (CR-12), edit a reviewed package before apply (CR-15), and rewrite the audit chain (CR-42). Auth fixes close some doors one at a time. An OS sandbox closes the whole class: the agent process tree cannot open those files or reach those ports, whatever it is told to do.

## 2. What exists today (and what it does not do)

Read before designing; most of the plumbing is here, but none of it protects against the threat above in practice.

| Piece | Where | What it actually does | Gap against this threat |
|---|---|---|---|
| `SandboxPolicy` / `SandboxProvider` | `ta-runtime/src/sandbox.rs` | Wraps the agent `SpawnRequest` in `sandbox-exec -p <profile>` (macOS) or `bwrap` (Linux); AppContainer + Job Object on Windows. | Opt-in, and gated twice (see next row). |
| Enable switch | `apps/ta-cli/src/commands/run.rs` around line 6148 | Requires `[sandbox] enabled = true` in `.ta/workflow.toml` **and** `[experimental] sandbox = true` in `.ta/daemon.toml`. | Both files are read from `staging_path/.ta/`, but the overlay never copies `.ta/` into staging (`overlay.rs` test `assert!(!overlay.staging_dir().join(".ta").exists())`). **Likely latent bug: the project's `[sandbox]` setting is never seen, so agents run unsandboxed with no message.** Needs a regression test to confirm. Either way, launch policy must not come from a path an agent can write (CR-12). |
| Security levels | `ta-goal/src/security.rs` | `mid` and `high` set `sandbox_enabled = true` in the computed profile. | `profile.sandbox_enabled` is only used for warnings (`run.rs` around 3448, `status.rs` around 67); it does not turn the sandbox on. |
| macOS Seatbelt profile | `generate_macos_profile` | `(deny default)`, allows `/usr`, `/System`, `/nix`, `/private/tmp`, staging rw. | `(allow mach-lookup)` for all services and `(allow process-exec*)` for everything, which allows escapes through launchd, AppleEvents and `securityd` (Keychain). Denies all of `$HOME`, so a real Claude Code agent (needs `~/.claude`, its Node runtime, `$TMPDIR` under `/private/var/folders`) very likely fails to start. Any non-empty `allow_network` allows all outbound **including loopback to the daemon and NATS**. `/private/tmp` is shared across goals. |
| Linux bwrap | `apply_linux_bwrap` | ro binds of `/usr`, `/lib*`, `/etc/ssl`, `/nix`; staging rw; `--unshare-net` when no network. | No `--unshare-user/pid/ipc`, no `--die-with-parent`, no `--new-session` (TIOCSTI terminal injection), no seccomp, no Landlock (docs say "landlock", code has none). With any network allowed, the host netns is shared, so `127.0.0.1:7700` and `:4222` are reachable. Real agents likely break (no `$HOME`, no `/etc/passwd`). |
| Windows AppContainer | `sandbox_windows.rs` | Real: container SID, DACL grant on staging, `internetClient` capability when network is wanted, Job Object teardown. Has real tests (`appcontainer_denies_write_outside_staging_path`). | Strongest of the three today. `internetClient` means all internet; there is no host allowlist. Agent toolchains in the user profile are unreadable without extra ACL grants. Falls back to Job-Object-only (no file isolation) on a warning. |
| `ta-sandbox` crate | `crates/ta-sandbox` | Command-name allowlist, cwd convention, `NetworkPolicy` for the DB proxy. | Application-level checks, not an OS boundary (USAGE.md already says so). Keep it as a layer, not as the sandbox. |
| MCP gateway placement | `write_stable_agent_mcp_config` (`run.rs` around 7142) | `ta serve` is launched **by the agent** as a stdio child. | Any OS sandbox applied to the agent is inherited by `ta serve`, which needs to read and write `<project>/.ta/`. **This is the main architectural blocker**: the mediated path cannot live inside the sandbox it mediates. |
| Credential shims | `credential_helper.rs`, `ta-credential-broker/src/shim.rs` | git `credential.helper` and a `gh` wrapper resolve secrets through the broker. | `resolve_for_host` opens `CredentialBroker` at `<project>/.ta` and reads the vault **inside the agent's process tree**. That proves the agent tree can read the root key today, and it will break under a sandbox that denies `.ta/`. |
| Unix-socket MCP transport | `ta-daemon/src/transport.rs` `serve_unix` | Serves the gateway on a socket with a connection auth token. | Already the right shape for an out-of-sandbox gateway. |
| Keychain custody | `ta-credentials/src/encryption.rs` | Vault key in the OS keychain via the `keyring` crate, file fallback with a loud warning. | Only the vault key. Broker root key, audit key and daemon tokens are still plain files in `.ta/`. |
| ProjFS staging | `ta-workspace` feature `projfs` | Virtualized staging on Windows. | The ProjFS provider runs in TA's process with TA's rights; it must apply the same infra-dir deny (case-insensitively, see CR-06). |
| OCI runtime slot | `ta-runtime/src/config.rs` | `RuntimeRegistry` knows the name `oci`; resolves to a plugin. | No built-in container runtime. |

## 3. Threat model and non-goals

**In scope (the attacker):** a prompt-injected or misbehaving agent process, its children, and any tool it runs (build scripts, test runners, linters, package install hooks), all running as the user. Also plugin subprocesses (CR-25) and verification commands TA itself runs in staging (`cargo test` runs untrusted `build.rs`).

**Out of scope:** a malicious user, root or admin compromise, kernel exploits, other unsandboxed processes the user runs, hardware side channels, and a compromised `ta` binary (that is TA-05's release signing). The sandbox is a containment boundary for agent code, not a defense of the whole machine.

**Design principle:** two independent layers must each stop the top attacks. The sandbox denies reach; the control plane refuses unauthenticated or wrongly-scoped requests (local bypass off, role-bound tokens, hash-pinned config). Neither layer is allowed to rely on the other being present.

## 4. Policy model

A sandbox profile is a platform-neutral value (`SandboxProfile`) compiled per launch. Providers translate it; they never invent policy.

### 4.1 Filesystem

| Rule | Default |
|---|---|
| Read + write | the goal's own staging dir; a per-goal `TMPDIR`; a per-goal agent home (see 4.4) |
| Read only | OS libraries and toolchains (`/usr`, `/System`, `/Library/Apple`, `/nix/store`, `C:\Windows`, `C:\Program Files`), the agent runtime install path, declared toolchain caches (read-only, with a per-goal writable overlay or cache dir where the tool insists on writing) |
| Deny read and write | `<project>/.ta/**` except `<project>/.ta/staging/<this-goal>/**`; every other goal's staging; the TA state dir (section 8); the source project tree outside staging (the agent works on the copy, never the original); `~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.config/gh`, `~/.docker`, `~/.kube`, `~/.netrc`, keychain and keyring stores, browser profiles; `.git` of the source project |
| Deny write | everything not listed above, including the `ta` and `ta-daemon` binary directories (closes the CR-08 laundering path from inside the sandbox) |

Paths are canonicalized at compile time, compared case-insensitively on case-insensitive volumes (macOS default APFS, NTFS), and symlinks inside staging are resolved by the OS mechanism, not by string checks (the OS resolves the real target before checking, which is the point).

### 4.2 Network

| Rule | Default |
|---|---|
| Loopback | deny all, except the single per-goal mediation endpoint (section 5) |
| LAN / link-local / cloud metadata (`169.254.169.254`) | deny |
| Egress | only through the TA egress proxy, which allows only declared hostnames (model API plus role-declared hosts) |
| Inbound listen | deny, except loopback inside a private network namespace where the platform gives one |
| DNS | resolved by the proxy (agent does not need direct DNS when everything goes through `HTTPS_PROXY`) |

Hostname filtering is done by the proxy, never by the kernel mechanism, because none of the kernel mechanisms can filter by hostname (section 6). The proxy does CONNECT-level filtering on the SNI/CONNECT host with no TLS interception. Credential injection through a MITM CA stays the separate, explicit trust decision it already is (v0.17.6.7 item 3).

### 4.3 Process

- No ptrace, no `task_for_pid`, no `process_vm_readv`, no `OpenProcess` on processes outside the sandbox.
- No way to start an unsandboxed process: the sandbox is inherited by every descendant, and every "ask someone else to run it" channel (launchd submit, AppleEvents, `open`, `systemd-run --user`, D-Bus activation, `at`, `crontab`, Windows Task Scheduler and WMI, COM out-of-proc servers) is denied.
- Exec allowlist per role (CoS: only the agent runtime; workers: also shells and declared toolchains).
- New session / no controlling terminal (blocks TIOCSTI injection into the user's shell).
- Resource limits: process count, memory, and wall-clock through the existing Job Object (Windows), cgroup v2 user scope (Linux, when available) or `setrlimit` (all).

### 4.4 Environment

`SpawnRequest.clear_env = true` always. Allowlist: `PATH` (rebuilt), `HOME` (per-goal agent home), `TMPDIR`, `LANG`, `LC_*`, `TERM`, `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` (pointing at the TA proxy), the model credential (broker-injected, role-scoped), `TA_GOAL_ID`, the MCP bridge endpoint, and role-declared extras. Never pass: `SSH_AUTH_SOCK` (an open door to every SSH key), `GPG_AGENT_INFO`, `DBUS_SESSION_BUS_ADDRESS`, `XDG_RUNTIME_DIR`, `AWS_*`, `GITHUB_TOKEN`, `GH_TOKEN`, `TA_*` other than the above, `DOCKER_HOST`, `KUBECONFIG`.

Per-goal agent home: for Claude Code, `CLAUDE_CONFIG_DIR` points to a per-goal copy of a TA-owned template (settings, MCP config, permissions). The agent can write to it but nothing it writes outlives the goal, and the user's global `~/.claude/settings.json` (whose broad allow rules caused CR-01) is not read at all. This closes a persistence route the sandbox would otherwise leave open: an agent editing the global settings to widen the next, possibly unsandboxed, launch.

### 4.5 What the profile does not cover

- What a model does with data it is allowed to read and send to an allowed host (exfiltration to the model API itself is always possible; this is a data-classification question, not a sandbox one).
- Correctness of the mediated MCP tools (that is the gateway's and the constitution's job).
- Side effects through allowed hosts (a worker allowed to reach `registry.npmjs.org` can publish if it holds a token; it should not hold one, see v0.17.6.7).

## 5. Architecture: move the mediated path out of the sandbox

```
            unsandboxed (TA trusted computing base)                 |   sandboxed (one per goal)
                                                                    |
  ta-daemon ---- owns secrets (keychain / state dir)                |
      |                                                             |
      +-- per-goal MCP gateway (ta serve, outside) --- socket/pipe --+--> ta mcp-bridge (stdio shim)
      |        authenticated, role-scoped                           |        ^ stdio
      |                                                             |    agent runtime (claude, codex, ...)
      +-- per-goal egress proxy (CONNECT allowlist) -- socket/port --+--> HTTPS_PROXY in agent env
      |                                                             |        |
      +-- broker shim endpoint (git/gh credential resolution) ------+--> ta credential-helper (client only)
                                                                    |   children inherit the sandbox
```

1. **Gateway outside, bridge inside.** TA starts `ta serve` for the goal before launching the agent, outside the sandbox, on a per-goal endpoint: a Unix socket at `<staging>/.ta-mcp.sock` (macOS, Linux) or a named pipe whose DACL grants only the goal's AppContainer SID (Windows). The agent's MCP config points at `ta mcp-bridge --endpoint <path>`, a tiny stdio-to-socket relay that holds no secrets and needs no file access beyond the socket. `serve_unix` and `authenticate_connection` already exist; the bridge sends the per-goal connection token it got through an inherited fd or its environment. The gateway then has full `.ta/` access because it is trusted code outside the box, and the agent can reach it only through MCP.
2. **Credential shims become clients.** `ta credential-helper` and the `gh` wrapper stop opening the broker themselves; they ask the gateway (or a dedicated broker endpoint) over the same socket. The raw secret still lives only in the helper's memory for one request, as v0.17.6.7 intended, but the root key never enters the sandbox.
3. **Egress proxy outside.** A per-goal (or per-daemon, keyed by connection token) CONNECT proxy enforces the hostname allowlist and logs every connection to the audit stream. How the sandbox reaches it is platform-specific (section 6).
4. **Daemon API stays unreachable.** Nothing in the sandbox can reach `127.0.0.1:7700` or NATS `:4222`. The daemon also stops trusting loopback (CR-02 fix) and, where it serves mutating routes on a Unix socket, rejects peers that are sandboxed (`sandbox_check(pid)` on macOS, cgroup or Landlock-domain membership via a TA-owned cgroup on Linux, `GetNamedPipeClientProcessId` plus "token is AppContainer" on Windows). Defense in depth: either layer alone stops self-approval.
5. **Policy comes from outside the project.** The launcher compiles the profile from the hash-pinned persona and constitution (section 7) held in the TA state dir, never from `<project>/.ta/workflow.toml` or `<staging>/.ta/`. Project files may only tighten.

## 6. Platform mechanisms

### 6.1 macOS

**Mechanism: Seatbelt via `sandbox-exec` (or `sandbox_init` from a TA-owned launcher).**

- *Status.* `sandbox-exec` and `sandbox_init` with custom profiles have been marked deprecated in the man pages for many releases and the profile language (SBPL) is undocumented. They still work on current macOS and are what Chrome, Bazel, OpenAI Codex CLI and Anthropic's own Claude Code sandboxing use. Risk: Apple could remove or change it in any release, with no public replacement for sandboxing arbitrary third-party CLI processes. Mitigation: keep the container/VM fallback (6.4) working on macOS so there is a plan B, test against every new macOS beta in CI (macOS runners), and keep the SBPL generator small and well-tested.
- *App Sandbox is not an option for agents.* App Sandbox is enabled through entitlements embedded in a code signature. It applies to binaries we sign, not to `claude`, `node`, `git` or `cargo`. Re-signing third-party binaries to add entitlements breaks their notarization and is exactly the CR-08 pattern. App Sandbox also redirects `$HOME` into a container. It is a possible future hardening for TA's own helpers (bridge, proxy), not for the agent.
- *Profile shape.* Reads are allow-mostly with targeted denies (an allow-list for reads breaks too many tools and gives little: the secrets to protect are enumerable). Writes are deny-default. SBPL takes the last matching rule, so ordering is: `(allow file-read*)`, then `(deny file-read* (subpath "<project>/.ta"))` plus the sensitive-path list, then `(allow file-read* file-write* (subpath "<staging>"))`. Literal and regex filters must be canonical real paths (`/private/var`, not `/var`).
- *mach-lookup must be an allowlist.* Today's blanket `(allow mach-lookup)` lets an agent reach `com.apple.securityd` / `com.apple.SecurityServer` (Keychain), launchd job submission, LaunchServices (`open`), and the AppleEvents path (`osascript -e 'tell app "Terminal" to do script ...'` runs a command **outside** the sandbox). The profile must enumerate the services the agent runtime needs (to be discovered empirically with violation logging) and add `(deny appleevent-send)`, `(deny lsopen)`, `(deny process-info* (target others))`, `(deny signal (target others))`, and no `mach-priv-task-port`.
- *Keychain interaction.* Keychain ACLs trust a code identity, not a process tree. An agent that can exec the signed `ta` (or a copy of it: the code signature travels with the file) can ask it to read TA's items. So: (1) only `ta-daemon` ever reads TA secrets, never the `ta` CLI in an agent context; (2) the sandbox denies `securityd` lookup for agents whose runtime does not need it; (3) if the agent runtime does need the Keychain for its own login (Claude Code OAuth on macOS stores credentials there), prefer giving it an API key or broker-issued token through the environment and denying `securityd`; where that is impossible, the profile must at least deny exec of `ta-daemon` and of `/usr/bin/security`, and this is recorded as a known residual risk.
- *Code signing.* `sandbox-exec` does not touch signatures. The CR-08 self-heal re-sign must not run for anything spawned into a sandbox, and the sandbox's write-deny on the binary directories prevents an agent from planting a binary for TA to re-sign.
- *Rosetta.* Translated x86_64 processes are sandboxed like native ones. The profile must allow reading the Rosetta runtime and AOT cache (`/Library/Apple/usr/libexec/oah`, `/usr/libexec/rosetta`, and the cache under `/private/var/db/oah`, exact paths to be confirmed on an Intel-binary test). Universal or arm64 agent binaries avoid this entirely.
- *Network.* SBPL filters by address and port with only `*` or `localhost` as the host. That is enough for what the kernel layer must do here: deny `(remote ip "localhost:*")`, then allow `(remote ip "localhost:<proxy-port>")` (or `(remote unix-socket (path-literal "<proxy.sock>"))`), and deny all other outbound. Hostname policy lives in the proxy.
- *Observability.* Denials go to the unified log (`log stream --predicate 'sender == "Sandbox"'`). TA tails it filtered by the agent's pid tree and turns denials into `SandboxViolation` events with path and operation, so a developer sees "agent tried to read ~/.ssh/id_ed25519, denied" instead of a mysterious EPERM.
- *Cannot do:* hostname filtering; per-thread policy; loosening for a child; reliable stability guarantees from Apple; protection against a sandboxed process abusing an allowed mach service it legitimately needs.

### 6.2 Linux

Three layers, used together where available, each probed at runtime, never assumed.

**Landlock (unprivileged LSM, preferred for filesystem).**
- ABI versions: v1 (kernel 5.13) filesystem access rights; v2 (5.19) `REFER` for cross-directory rename/link; v3 (6.2) `TRUNCATE`; v4 (6.7) TCP `bind`/`connect` by **port**; v5 (6.10) device `IOCTL`; v6 (6.12) scoping of abstract Unix sockets and signals to the domain. Audit logging of denials arrives in later kernels (6.15 era; verify before relying on it).
- Rules are allow-lists of hierarchies, inherited by children, cannot be removed, need no privileges and no `no_new_privs` beyond what the crate sets. Landlock also blocks ptrace from inside the domain to processes outside it.
- Use the `landlock` crate with best-effort mode **off** for required rights: TA must know exactly which rights were enforced and report them, not silently downgrade.
- *Cannot do:* filter by IP address or hostname (v4 is ports only; allowing TCP connect only to the proxy port blocks 7700 and 4222 everywhere, which helps); UDP; connecting to existing **pathname** Unix sockets is not a Landlock-controlled operation, so the daemon's own sockets must be hidden by a mount namespace or protected by peer checks; anything on kernels before 5.13.

**seccomp-bpf (syscall filter).**
- Deny: `ptrace`, `process_vm_readv/writev`, `kcmp`, `perf_event_open`, `bpf`, `userfaultfd`, `keyctl`/`add_key`/`request_key`, `mount`/`umount2`/`pivot_root`, `unshare` and `clone` with `CLONE_NEWUSER` after setup, `io_uring_*` (it bypasses some per-syscall filtering), `personality` changes, `ioctl(TIOCSTI)`.
- Set `PR_SET_NO_NEW_PRIVS`. Use `SECCOMP_RET_ERRNO(EPERM)` with a parallel `SECCOMP_RET_LOG` build for diagnosis.
- *Cannot do:* inspect path arguments (pointers); stop anything allowed syscalls can do.

**Namespaces via bubblewrap (filesystem view, pid, network).**
- `bwrap --unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-net --die-with-parent --new-session --cap-drop ALL`, a constructed root (ro binds of system dirs, `/nix/store`, toolchains), staging rw, tmpfs `/tmp`, private `/proc`, and a bind of only the two sockets the agent may use (MCP bridge, proxy). `<project>/.ta` simply does not exist inside, which is stronger than a deny rule.
- Network: a fresh netns has its own loopback, so the host's `127.0.0.1:7700` is unreachable by construction. Egress goes through a small in-sandbox relay that listens on the namespace's `127.0.0.1:3128` and forwards to the bind-mounted proxy socket (the same pattern Anthropic's open-source sandbox runtime uses). If `pasta` or `slirp4netns` is used instead, host-loopback mapping must be disabled (`slirp4netns --disable-host-loopback`; pasta's equivalent), otherwise `10.0.2.2` reaches the daemon.
- *Availability problems:*
  - Ubuntu 23.10+ and 24.04 restrict unprivileged user namespaces through AppArmor (`kernel.apparmor_restrict_unprivileged_userns=1`). The distro `bwrap` ships an AppArmor profile that allows it; a vendored or Nix-built `bwrap` binary will fail unless a profile is installed. Detect and explain.
  - Debian before 11 (`kernel.unprivileged_userns_clone=0`), RHEL/CentOS 7 (`user.max_user_namespaces=0`), and hardened kernels disable it. A setuid `bwrap` is possible but must be an admin decision.
  - Inside Docker, the default seccomp profile blocks `unshare(CLONE_NEWUSER)`, so `bwrap` fails in containers unless run with a custom profile. Landlock generally still works in containers on new enough kernels (whether Docker's default seccomp profile allows the `landlock_*` syscalls depends on the Docker version; probe).
- *Old kernels.* Below 5.13: no Landlock, so filesystem isolation depends entirely on bwrap. Without userns either: no OS sandbox; degraded-mode rules (section 9) decide.
- *WSL2.* Microsoft's WSL2 kernels (5.15, 6.6 lines) support user namespaces; whether Landlock is compiled in varies by kernel build, so probe. WSL2-specific escapes must be closed in any TA-managed WSL distro: Windows interop (`/proc/sys/fs/binfmt_misc/WSLInterop` lets a Linux process launch `cmd.exe` as the Windows user, outside every Linux sandbox) and `/mnt/c` automount. In the sandbox, the netns blocks interop's network side, and the mount namespace must not bind `/mnt/c` or the interop socket; in a dedicated distro, set `[interop] enabled=false` and `[automount] enabled=false` in `wsl.conf`. Mirrored networking mode shares `localhost` with Windows, which matters for the daemon if it runs on the Windows side.
- *Nix.* `/nix/store` is read-only already; bind it ro. Nix devShell environment variables carry paths into the store, which is fine. On NixOS, `bwrap` is not setuid and userns is allowed by default. The existing `ta` dev wrapper (`./dev`) is a developer tool and is not used for agents.
- *cgroups.* When a systemd user session exists, launch each goal in a transient scope (`systemd-run --user --scope`, done by TA outside the sandbox) for memory and pid limits and for reliable peer identification; otherwise fall back to `setrlimit`.

**Linux composition:** bwrap (view + pid + net) as the outer layer, then Landlock (rights, also protects against a bwrap misconfiguration) and seccomp applied by a TA-owned pre-exec helper (`ta sandbox-exec-helper`) inside the namespace before it execs the agent. If bwrap is unavailable but Landlock is, run Landlock + seccomp + port-restricted TCP (v4+) and report "no network namespace".

### 6.3 Windows

**Mechanism: AppContainer (exists) plus Job Object (exists), extended.**

- *File access.* An AppContainer token can open only objects whose DACL grants its SID, a capability SID, or `ALL APPLICATION PACKAGES`. User-profile files (including `<project>\.ta\` and the TA state dir) are **denied by default**, which is the right default. Staging gets an inheritable ACE for the container SID set on the staging root **before** files are copied in, so new files inherit it (cheap) instead of a recursive re-ACL of a large tree (slow, seconds on big repos).
- *Toolchains.* `node`, `git`, Claude Code under `%APPDATA%\npm`, `%USERPROFILE%\.cargo` are not readable by the container. Options: grant read ACEs to the specific install dirs for the goal SID (must be reverted on teardown; a crash leaves stale ACEs for a deleted SID, which is harmless but untidy, so `ta doctor --fix` sweeps them), or install agent runtimes under a TA-owned directory readable by `ALL APPLICATION PACKAGES`. Some tools behave badly in AppContainer (anything expecting to write under `%LOCALAPPDATA%` or to use certain COM servers); expect a compatibility list.
- *Network.* AppContainers **cannot reach loopback** unless a loopback exemption is set, so the daemon's `127.0.0.1:7700` and NATS are blocked by default: a real advantage. `internetClient` allows all internet. The egress proxy cannot be on a loopback TCP port (that would need a loopback exemption, which opens all loopback ports, including the daemon's). Use a **named pipe** with a DACL for the container SID plus an in-container relay, or skip `internetClient` entirely and route all egress through the pipe. Never grant `privateNetworkClientServer` (LAN) by default.
- *Process.* AppContainer processes cannot open handles to processes outside the container (no debugging the daemon). Job Object: `KILL_ON_JOB_CLOSE` (exists), `ACTIVE_PROCESS` limit, no breakaway, UI restrictions (no global hooks, no clipboard read, no desktop switching). Task Scheduler, WMI and most COM activation are unavailable to an AppContainer, which closes the "run something unsandboxed for me" routes.
- *Integrity levels and restricted tokens (why not alone).* A Low-IL process is stopped from **writing** up but can still **read** medium-integrity files (no-read-up is not the default label policy), so Low IL does not protect secrets. Restricted tokens with deny-only SIDs can work but need careful SID engineering and are easier to get wrong. LPAC (less-privileged AppContainer) is stricter still and a candidate for the CoS profile later. AppContainer is the right base.
- *ProjFS staging.* The ProjFS provider callbacks run in TA's process with the user's full rights and hydrate files from source: the provider must refuse to project anything under `.ta`, `.git` or other infra dirs (case-insensitive, NTFS is case-insensitive, CR-06), and must grant the container SID on the virtualization root. Treat the provider as a confused-deputy risk in review.
- *Windows path tricks.* Profile compilation and the ProjFS provider must reject ADS (`::$DATA`), trailing dots and spaces, 8.3 short names and device names (CR-44).
- *WSL2 as an alternative on Windows.* Run the agent inside a TA-managed WSL2 distro under the Linux sandbox. Requires interop and automount disabled (6.2), staging placed in the distro's filesystem (`/mnt/c` is slow and breaks the isolation story), and the TA daemon either inside the distro or reached through a mediated socket. Useful for Linux-native toolchains; heavier to install. Treat as an optional provider, not the default.
- *Cannot do:* hostname filtering; protect anything the user grants `ALL APPLICATION PACKAGES` to (some system and Program Files paths are world-readable to containers, which is acceptable); full compatibility with every developer tool.

### 6.4 Container / VM fallback (universal)

A container or VM gives the strongest and most uniform boundary at the highest cost. It is the plan B on macOS if Seatbelt goes away, the answer on Linux hosts with no userns, and the "strict, regulated" option everywhere.

- *Providers.* Docker and rootless Podman on Linux; Docker Desktop, Podman machine, Colima/Lima on macOS (all run a Linux VM); Docker Desktop or WSL2 on Windows. Apple's newer native container framework on recent macOS is a candidate to evaluate, not a dependency.
- *Shape.* Implemented as the `oci` runtime plugin the registry already names. Mount only the staging dir rw, read-only image for toolchains, `--network none` plus a mounted proxy socket and relay (or an internal network whose only route is a proxy sidecar), `--cap-drop ALL`, `--security-opt no-new-privileges`, default seccomp, non-root user, `--pids-limit`, `--memory`.
- *Pitfalls.* Never mount the Docker socket (it is root on the host). Docker Desktop's `host.docker.internal` (and Lima's host gateway) reaches the host's loopback services including the daemon: block it in the network design. Rootful Docker's bind mounts write files as root. Image supply chain becomes part of the TCB: pin by digest, sign.
- *Cost.* Start-up of 1 to 3 seconds per goal; on macOS, bind-mounted file I/O through the VM (virtiofs) is markedly slower for builds (often several times slower for metadata-heavy work such as `cargo` or `node_modules`); disk for images. Acceptable for CoS and reviewers, painful for heavy compile loops.

### 6.5 Capability matrix

Each provider reports which of these it enforces at launch; the profile states which it requires.

| Capability | macOS Seatbelt | Linux bwrap+Landlock+seccomp | Linux Landlock-only | Windows AppContainer+Job | Container/VM |
|---|---|---|---|---|---|
| `fs.deny_ta_tree` | yes (deny rules) | yes (not mounted) | yes (not in allow-list) | yes (no ACE) | yes (not mounted) |
| `fs.write_confined` | yes | yes | yes | yes | yes |
| `net.loopback_blocked` | yes (port rules) | yes (netns) | partial (ports only, Landlock v4+, TCP only) | yes (default) | yes, if host-gateway blocked |
| `net.egress_allowlist` (via proxy) | yes | yes | partial (cannot stop direct connects to port 443 elsewhere) | yes (pipe relay) | yes |
| `proc.no_ptrace_out` | yes | yes | yes | yes | yes |
| `proc.no_unsandboxed_spawn` | yes, if mach/AppleEvents denied | yes | partial (D-Bus and launcher sockets stay reachable without a mount namespace) | yes | yes |
| `env.scrubbed` | yes (TA side) | yes | yes | yes | yes |
| `ipc.no_daemon_socket` | yes | yes | no (pathname sockets not covered) | yes | yes |

## 7. Per-role profiles and how they are derived

### 7.1 Profiles

| Role | Filesystem | Network | Process | Notes |
|---|---|---|---|---|
| **CoS (chat mode)**, strictest | chat scratch dir rw; agent runtime ro; nothing else from the project (it reads project content only through MCP tools) | model API host only | exec only the agent runtime binary; no shells; `securityd` denied | Pairs with PR #640's server-side MCP surface; the sandbox makes "read-only" true even if the tool allow-list is wrong (CR-01, CR-13). |
| **Reviewer / read-only roles** | staging ro; scratch rw | model API only | runtime + read-only tools (`rg`, `git` without push) | |
| **Worker (coding)** | staging rw; toolchains ro; per-goal caches rw | model API + role-declared hosts (package registries, docs sites) | shells and declared toolchains; no ptrace | Credentials only through broker shims over the mediated socket. |
| **Verification runs by TA** (`cargo test`, linters in staging during draft build) | same as worker, network default none | none unless declared | same as worker | These execute untrusted code from the draft; today they run unsandboxed. |
| **Plugins** (adapters, channel plugins, CR-25) | own install dir ro, own data dir rw | hosts declared in the plugin manifest | no shells unless declared | Credentials via broker; profile shown at install and pinned by hash (trust on first use for project-local plugins). |
| **Librarian / wiki ingestion** | cache dir rw (moved out of the agent-writable tree, CR-37) | Wayfinder host only | runtime only | |
| **Gateway, proxy, daemon, bridge host** | n/a | n/a | n/a | Trusted, unsandboxed by design; keep them small. |

### 7.2 Derivation

`profile = base(role_class) ∩ persona.sandbox ∩ constitution.sandbox(posture, origin)`

- `role_class` comes from the team role or `TeamRole` data (`chat`, `reviewer`, `worker`, `plugin`), not from a free-form string the agent can influence.
- The persona TOML may declare `[sandbox]` additions (`allow_hosts`, `toolchains`, `allow_read`). Persona files are hash-pinned in the TA state dir (CR-12); drift refuses launch.
- The compiled constitution (`docs/compiled-constitution-design`) gains a `sandbox` table: per posture and per origin (`chat`, `cos`, `human`), which capabilities are required and which additions are permitted. A persona can only tighten what the constitution permits, never loosen it; origin `cos` or `chat` forces the CoS class regardless of persona.
- Project-level `.ta/workflow.toml [sandbox]` may only add denies. Any `allow_*` there is ignored for team roles with a visible warning (it is agent-writable from earlier goals).
- The compiled profile is printed by `ta sandbox explain <goal|role>`, hashed, and the hash is written into the goal record and the audit log, so "what box was this agent in?" is answerable after the fact.

## 8. Moving secrets out of the project tree

The sandbox deny on `.ta/**` and the secret move reinforce each other: the move makes secrets unreachable even when the sandbox is degraded or off; the sandbox protects whatever has to stay in `.ta/` (packages, personas, events). Neither waits for the other; the secret move is S/M and should land first.

| Secret / state | Today | Target |
|---|---|---|
| Broker root key (`broker_root.key`) | `<project>/.ta/` file | OS keychain item owned by the daemon (macOS Keychain, Windows Credential Manager/DPAPI, Linux Secret Service via `keyring`), file fallback in the TA state dir with mode 0600 |
| Audit HMAC key (`audit.key`) | `<project>/.ta/` file | same as above; plus periodic signed checkpoints of the chain head stored in the state dir (CR-42) |
| Daemon API tokens (`daemon-tokens.json`, `tokens/*`) | `<project>/.ta/` plaintext | state dir, 0600; CLI and Studio read their own token from there |
| Team-session bearer tokens (`team-sessions/<id>/state.json`) | plaintext in project | never persisted; handed to launches via inherited fd or env, role-scoped (CR-04) |
| Vault key | already keychain (with file fallback) | unchanged; fallback file moves to the state dir |
| Wayfinder / wiki credentials | vault | unchanged, resolved only by daemon-side code |
| Persona and agent config hashes | none | state dir (CR-12) |
| Package hashes at approve time | none | state dir (CR-15) |
| NATS JetStream data | `<project>/.ta/nats-jetstream` | state dir, and NATS bound to loopback with auth (CR-05) |

**TA state dir** (daemon-only, outside every project):
- macOS: `~/Library/Application Support/TrustedAutonomy/projects/<project-id>/`
- Linux: `$XDG_STATE_HOME/trusted-autonomy/projects/<project-id>/` (default `~/.local/state/...`)
- Windows: `%LOCALAPPDATA%\TrustedAutonomy\projects\<project-id>\`

`<project-id>` is a stable hash of the canonical project path plus a random salt recorded in `.ta/project-id` (non-secret).

**Honest limits of OS stores against a same-user, unsandboxed process:** macOS Keychain ACLs bind to code identity (strong only if the requesting binary cannot be exec'd by the attacker, see 6.1); DPAPI user scope and Linux Secret Service unlock for any process of the logged-in user; a 0600 file is readable by the same uid. So **the stores protect against offline theft, other users and backups; against an agent, the protection comes from the sandbox denying the store** (no `securityd` lookup, no D-Bus session bus socket, no access to the state dir, AppContainer has no ACE on the files). This is the main reason the two efforts must ship together for real protection, even though the move can ship first.

Headless Linux with no Secret Service: file fallback in the state dir, with the same loud warning `ta-credentials` already emits, and `TA_NO_KEYCHAIN` honored (v0.17.11.14).

Migration: `ta doctor --fix` moves existing keys, rewrites references, deletes the old files, and records the move in the audit log. A daemon that finds a key in the old location refuses to start in `strict`, migrates automatically in `balanced` and `open`.

## 9. Degraded mode

Never silent. Every launch records which capabilities were required, which were enforced, and the decision, as a `SandboxDecision` event in the audit log, a line in `ta run` output, a field in `ta status`, and a badge in Studio.

| Posture | Required capability missing for **CoS / chat / untrusted-origin** roles | Missing for **workers** | Missing for **plugins** |
|---|---|---|---|
| **strict** | refuse to launch; message names the missing capability, the platform reason, and the fix (install bubblewrap, enable userns, use the `oci` provider) | refuse | refuse |
| **balanced** (proposed default) | refuse | launch with the best available layers; loud warning each launch; draft is labelled "built without sandbox capability X" and the label is shown in review | launch with warning |
| **open** | launch unsandboxed with a warning banner and audit event | same | same |

Rules:
- "Capability missing" is computed from the capability matrix (6.5), not from "provider is not None". Seatbelt with mach-lookup open is not counted as `proc.no_unsandboxed_spawn`; Landlock-only Linux is not counted as `ipc.no_daemon_socket`.
- A provider that fails **at spawn** (bwrap exits 1, AppContainer creation fails) is handled the same way; today's AppContainer to Job-Object-only fallback becomes a posture decision instead of an automatic downgrade.
- Mapping to existing `[security] level`: `low` to `open`, `mid` to `balanced`, `high` to `strict` until the posture model from the compiled-constitution design replaces `security.level`. The `[experimental] sandbox` gate is removed.
- No per-run flag can turn the sandbox off for CoS or chat roles. For workers, `ta run --no-sandbox` exists only in `open` and is audited.

## 10. Developer experience and performance

**DX.**
- `ta sandbox explain <goal|role>`: the compiled profile in plain words, its hash, and the provider and capabilities in force.
- `ta sandbox test [--role worker]`: runs the conformance probe (section 11) on this machine and prints a pass/fail table; part of `ta doctor`.
- Violation surfacing: macOS unified-log tail, Linux seccomp `RET_LOG` / Landlock audit (where the kernel supports it) / probe-mode re-run, Windows: access-denied errors reported by the bridge plus optional ETW. Each becomes a `SandboxViolation` event with role, path or host, and a one-line suggestion ("add `allow_hosts = [\"pypi.org\"]` to persona X, which needs review").
- Escape hatches are explicit and reviewable: persona additions go through review and re-pin; there is no "disable for this one command".
- Toolchain caches: per-goal `CARGO_HOME`/`npm_config_cache` overlays, or shared read-only caches with per-goal write dirs, so a sandboxed build does not redownload the world.
- Clear failure on first run when a platform prerequisite is missing (bubblewrap, userns, AppArmor profile, Docker), with the exact command to fix.

**Performance (expected; to be measured in phase P1's benchmark).**
- Seatbelt and Landlock: per-syscall overhead is small (low single-digit percent on file-heavy builds is the commonly reported range); profile compile is milliseconds.
- bwrap: tens of milliseconds start-up; netns and the relay add one local hop per connection.
- Proxy: one extra local hop and a CONNECT round trip per connection; no TLS interception, so CPU cost is negligible.
- AppContainer: profile creation and ACE setup in milliseconds when the ACE is set before staging is populated; seconds if a large tree is re-ACL'd after the fact.
- Containers: 1 to 3 s start; slow bind-mount I/O on macOS. Default only where posture or platform requires it.

## 11. Testing in CI

A single **conformance probe** binary (`ta-sandbox-probe`) is launched under each provider with a given profile and attempts a fixed list of forbidden and allowed actions, printing a machine-readable result. The same assertions run on every OS:

1. Read a fake `<project>/.ta/broker_root.key` and `audit.key` (must fail).
2. Read another goal's staging (fail) and its own staging (succeed).
3. Write outside staging, including next to the `ta` binary (fail).
4. Connect to a test listener on `127.0.0.1:<port>` standing in for the daemon and NATS (fail); connect to the mediation endpoint (succeed).
5. Connect via the proxy to an allowed fake upstream (succeed) and a disallowed one (fail); direct outbound connect bypassing the proxy (fail).
6. ptrace / `task_for_pid` / `OpenProcess` on a sibling test process (fail).
7. Spawn a child and verify the child is also confined (re-run 1 and 4 from the child).
8. Escape attempts: macOS `osascript`/`open`/`launchctl submit`; Linux `systemd-run --user`, D-Bus, TIOCSTI; Windows `schtasks`, WMI process create (all fail).
9. Environment: no `SSH_AUTH_SOCK`, no `TA_*` secrets, no `AWS_*` (asserted).
10. Case and path tricks: `.TA/` on case-insensitive volumes, symlink from staging to `.ta`, hard link where possible, Windows ADS and short names (all fail).

**What CI can prove:**
- **Linux (ubuntu-latest):** recent kernel, so Landlock ABI v4+ is available to test; bwrap from the distro package; seccomp. Additional jobs: a container job with Docker's default seccomp (proves bwrap-unavailable detection and Landlock-only degraded mode), a job that disables userns via sysctl where the runner allows it, and a fake-ENOSYS mode (a test shim that makes `landlock_create_ruleset` fail) to prove old-kernel handling deterministically. The `oci` provider runs here with Docker.
- **macOS (macos-latest, arm64):** `sandbox-exec` works on hosted runners; the full probe suite runs. Keychain tests use a temporary keychain (`security create-keychain`), never the login keychain. Rosetta only if the runner has it installed (not guaranteed; mark the test skipped with a reason, never silently).
- **Windows (windows-latest):** AppContainer creation works on hosted runners (the repo already has AppContainer tests). The probe suite plus the named-pipe relay and loopback-blocked assertions.
- Unit tests on every OS for the platform-neutral compiler: profile algebra (intersection only tightens), persona hash drift refuses, project config cannot loosen, degraded-mode table for every posture and role.

**Needs manual verification (checklist per release):**
- A real Claude Code (and one other runtime) session doing real work inside each provider, with login via API key and, separately, via OAuth on macOS (Keychain interaction).
- Ubuntu desktop 24.04 with the AppArmor userns restriction; Fedora; NixOS; an old-kernel host (RHEL 8, 4.18) for the fallback path.
- WSL2 with NAT and with mirrored networking; Docker Desktop on macOS with the host-gateway block.
- MDM-managed macOS and Windows with Defender/AppLocker policies.
- ProjFS staging with AppContainer on Windows.
- Performance on a large repository (this repo is a good benchmark).

## 12. Delivery plan

Effort: S (days), M (1 to 2 weeks), L (3+ weeks).

| # | Phase | Effort | Depends on | Release |
|---|---|---|---|---|
| P0 | Secrets out of the project tree + daemon-only state dir | M | none (pairs with CR-02 local-bypass fix) | v0.17 final security release |
| P1 | Sandbox launch plumbing: policy from outside the project, enable bug fix, out-of-sandbox gateway + `ta mcp-bridge`, shims as clients, env scrub, per-goal agent home, capability reporting, degraded-mode table | L | P0 | v0.17 final |
| P2 | macOS Seatbelt profile v2 (mach allowlist, AppleEvents/lsopen deny, read-deny model, loopback-deny, proxy) + egress proxy + conformance probe on macOS | M | P1 | v0.17 final |
| P3 | Linux: bwrap hardening + Landlock + seccomp + netns relay + probe on Linux CI | M | P1 (shares proxy from P2) | v0.17 final |
| P4 | Windows: named-pipe bridge and proxy relay, toolchain ACE handling, ProjFS provider deny, probe on Windows CI | M | P1 | v0.17 final if time allows, else first v0.18.x |
| P5 | Default-on for CoS and team roles under `balanced`; per-role profiles from persona + constitution; `ta sandbox explain/test`; violation events | M | P2, P3 (P4 for Windows default-on); compiled constitution phase 1 | v0.17 final for macOS and Linux |
| P6 | Sandbox TA's own verification runs and plugin subprocesses | M | P5 | v0.18.x |
| P7 | `oci` container provider (Docker/Podman/Lima), strict posture option everywhere, plan B for macOS | L | P1 | v0.18.x |
| P8 | Optional WSL2 provider on Windows; LPAC for CoS on Windows | M | P4, P7 | v0.18.x |
| P9 | Multi-tenant scoping (TA-06 proper) and daemon-side peer identity enforcement for shared daemons | L | P5, v0.18.0 | v0.18.x (SA) |

**Why P0 to P5 belong in a final v0.17.x security release:** CR-02 and CR-03 are Critical and confirmed, and the current CoS and team-session features (already shipped in v0.17) are what expose them. Deferring the isolation of a shipped feature to an "enterprise" milestone repeats the reasoning v0.17.11.4 explicitly rejected for TA-01 to TA-03. P6 to P9 extend coverage and add heavier options; they are genuinely v0.18.x.

**Ordering with the red-team Phase A items:** local-bypass off, NATS on loopback, chat mode on, and case-folded infra checks (all S) should land before or alongside P0; they are the second layer this plan assumes.

## 13. Open questions for the owner

1. Is `balanced` the default posture for new installs, or `strict`? (This plan assumes `balanced`, with CoS always strict.)
2. Claude Code's OAuth login on macOS needs the Keychain. Is "API key or broker token only for sandboxed roles" acceptable, so `securityd` can be denied?
3. Reuse Anthropic's open-source sandbox runtime (Seatbelt + bubblewrap + proxy, the same architecture) as a dependency or reference, or implement natively in `ta-runtime`? This plan assumes native Rust in `ta-runtime`, using it as a reference design, because TA needs Windows parity and a single capability report. Licensing and current API stability were not checked.
4. Should the source project tree be readable at all by workers (some tools resolve paths outside cwd)? This plan says no.
5. Does the `security.level` setting survive, or is it replaced by the compiled-constitution posture? This plan maps one to the other in the interim.

## 14. What this plan could not determine

- Whether the `[sandbox]` setting is really never read (section 2): inferred from code (config read from `staging/.ta/`, overlay excludes `.ta/`), not run. P1 starts with a regression test.
- The exact mach services Claude Code and other runtimes need under Seatbelt; must be measured with violation logging.
- Whether current GitHub-hosted Ubuntu runners restrict unprivileged userns via AppArmor, and whether Docker's default seccomp profile in CI allows the Landlock syscalls.
- Landlock availability in shipping WSL2 kernels.
- Whether an AppContainer process can read the user's Credential Manager entries (believed not, not verified).
- Exact Rosetta runtime paths required under Seatbelt.
- Real performance numbers on this repo for each provider.

---

## PLAN.md-ready phases

Numbering continues the repo's style. `v0.17.12.x` is unused in PLAN.md and on every remote branch as of 2026-10-07 and forms the final v0.17 security release; `v0.18.0.5` onward sits under the existing SA milestone (`v0.18.0`, `v0.18.0.4`). The coordinator may renumber. Headings use `: ` as the id/title separator; PLAN.md's existing heading style can be applied when they are copied in.

### v0.17.12.1: Control-Plane Secrets Out of the Project Tree
- Add a daemon-only TA state dir per OS (macOS Application Support, `$XDG_STATE_HOME`, `%LOCALAPPDATA%`) keyed by a stable project id.
- Move `broker_root.key` and `audit.key` to the OS keychain via `keyring` (file fallback in the state dir, loud warning, `TA_NO_KEYCHAIN` honored); move `daemon-tokens.json`, `tokens/*` and NATS JetStream data to the state dir.
- Stop persisting team-session bearer tokens in `state.json`; pass them to launches via inherited fd or environment.
- Signed audit-chain checkpoints in the state dir (CR-42); persona/agent-config and package hashes stored there (groundwork for CR-12, CR-15).
- `ta doctor --fix` migration with audit entries; daemon refuses old-location keys in `strict`.
- **Depends on:** none (land with the CR-02 local-bypass fix). **Effort:** M.

### v0.17.12.2: Sandbox Launch Plumbing: Out-of-Sandbox Gateway, Trusted Policy Source, Capability Reporting
- Regression test for, and fix of, the `[sandbox]` config being read from `staging/.ta/`; remove the `[experimental] sandbox` gate; wire `security.level` / posture to actual enforcement.
- Start `ta serve` outside the sandbox on a per-goal Unix socket or named pipe; add `ta mcp-bridge` stdio relay; point agent MCP config at the bridge.
- Convert `ta credential-helper` and the `gh` shim into clients of the gateway (no broker or vault access from the agent tree).
- Platform-neutral `SandboxProfile` compiled from hash-pinned persona + constitution in the state dir; project config may only tighten.
- Environment scrub allowlist and per-goal agent home (`CLAUDE_CONFIG_DIR`, `TMPDIR`); `SandboxDecision` audit event with required vs enforced capabilities and the posture-based degraded-mode table.
- **Depends on:** v0.17.12.1. **Effort:** L.

### v0.17.12.3: macOS Seatbelt Profile v2 + Egress Proxy
- Rewrite the SBPL generator: read-allow with targeted denies (`.ta/**` except own staging, state dir, `~/.ssh` and friends), deny-default writes, last-match ordering, canonical case-folded paths.
- mach-lookup allowlist; deny `appleevent-send`, `lsopen`, `process-info*` and `signal` on others; deny exec of `ta-daemon` and `/usr/bin/security`.
- Per-goal CONNECT egress proxy with hostname allowlist and audit logging (shared by all platforms); Seatbelt allows only the proxy and bridge endpoints, denies all other loopback.
- Unified-log violation tail into `SandboxViolation` events; Rosetta read paths.
- `ta-sandbox-probe` conformance suite running on macOS CI.
- **Depends on:** v0.17.12.2. **Effort:** M.

### v0.17.12.4: Linux: Hardened bubblewrap + Landlock + seccomp
- bwrap with user, pid, ipc, uts and net namespaces, `--die-with-parent`, `--new-session`, constructed root without `.ta`, private `/proc`, tmpfs `/tmp`.
- Pre-exec helper applying Landlock (strict, ABI-aware, no silent best-effort) and a seccomp deny list (ptrace, process_vm_*, bpf, keyctl, io_uring, TIOCSTI, nested userns).
- In-namespace relay to the bind-mounted proxy socket; Landlock-only degraded path with TCP port restriction on ABI v4+.
- Runtime probes and actionable errors for AppArmor userns restriction, disabled userns, containers, WSL2 interop and automount, Nix.
- Conformance suite on Linux CI including container and fake-ENOSYS jobs.
- **Depends on:** v0.17.12.2 (proxy from v0.17.12.3). **Effort:** M.

### v0.17.12.5: Default-On Isolation for CoS and Team Roles
- Per-role profiles (CoS chat strictest, reviewer, worker, librarian) derived from role class ∩ persona ∩ constitution; origin `cos`/`chat` forces the CoS class.
- Default-on under `balanced` for macOS and Linux; CoS/chat refuse to launch without required capabilities in every posture except `open`.
- `ta sandbox explain` and `ta sandbox test`; `ta doctor` capability matrix; Studio and `ta status` badges.
- Drafts built without a required capability are labelled in review.
- USAGE.md rewrite of the sandbox section as how-to, honest per-platform limits.
- **Depends on:** v0.17.12.3, v0.17.12.4, compiled-constitution phase 1. **Effort:** M.

### v0.18.0.5: Windows AppContainer Parity
- Named-pipe gateway bridge and egress relay with container-SID DACLs (no loopback exemption).
- Inheritable staging ACE set before population; toolchain read grants with teardown and `ta doctor --fix` sweep, or a TA-owned runtime dir.
- ProjFS provider refuses infra dirs case-insensitively and grants the container SID; ADS, short-name and device-name rejection.
- Job Object process limits and UI restrictions; AppContainer-to-Job-Object fallback becomes a posture decision.
- Conformance suite on Windows CI; default-on for team roles on Windows.
- **Depends on:** v0.17.12.2, v0.17.12.3. **Effort:** M. (Move into v0.17.12.x if the release can wait for it.)

### v0.18.0.6: Sandbox Verification Runs and Plugin Subprocesses
- Run TA's own staging verification commands (build, test, lint) under the worker profile with network off by default.
- Plugin manifests declare sandbox needs (hosts, dirs); profile shown at install, hash-pinned, trust-on-first-use for project-local plugins (CR-25).
- Per-plugin scoped broker tokens, environment allowlist.
- Conformance tests for a sample adapter and channel plugin.
- **Depends on:** v0.17.12.5. **Effort:** M.

### v0.18.0.7: `oci` Container Provider (Universal Fallback)
- Implement the `oci` runtime plugin for Docker and rootless Podman (Linux), Docker Desktop / Colima / Lima (macOS), Docker Desktop (Windows).
- Staging-only rw mount, `--network none` + proxy relay, host-gateway blocked, cap-drop, no-new-privileges, non-root, limits; never mount the Docker socket.
- Digest-pinned, signed agent images; per-goal cache volumes.
- Selectable as the required provider under `strict`; documented plan B if macOS removes Seatbelt.
- Performance benchmark against native providers on this repo.
- **Depends on:** v0.17.12.2. **Effort:** L.

### v0.18.0.8: Windows WSL2 Provider and LPAC for CoS
- Optional TA-managed WSL2 distro with interop and automount disabled, staging in the distro filesystem, Linux sandbox inside.
- Less-privileged AppContainer for the CoS profile on Windows.
- Networking-mode handling (NAT vs mirrored) for daemon reachability.
- Manual verification checklist and CI where runners allow.
- **Depends on:** v0.18.0.5, v0.18.0.7. **Effort:** M.

### v0.18.0.9: Multi-Tenant Token Scoping and Peer Identity (TA-06)
- Per-project and per-org `TokenScope`; tokens can no longer act across projects on a shared daemon.
- Daemon mutating routes on a Unix socket / named pipe with peer identity checks that reject sandboxed or agent-descendant peers.
- Per-goal cgroup / Job Object identity as the peer-identification source.
- Tests for cross-project and sandboxed-peer rejection.
- **Depends on:** v0.17.12.5, v0.18.0. **Effort:** L.
