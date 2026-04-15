# File System Strategy

TA governs agent access to the filesystem through three tiers of increasing isolation. Each tier is additive: Tier 1 is always active; Tier 2 extends it to paths outside the project; Tier 3 wraps the agent process in an OS-level jail. Choose the tier based on how far outside the project the agent needs to write, and how much trust you place in the agent's behavior.

---

## Tier 1 — Project Overlay

**Implemented** (`crates/ta-workspace/src/overlay.rs`)

The agent works in `.ta/staging/` — a copy of the project directory. TA diffs staging against the source on exit and builds a draft. The agent uses its normal tools (git, cargo, cat, rg) with no virtualization layer visible to it.

**Copy strategies** (selected automatically by `OverlayWorkspace::create`):

| Strategy | When | Performance |
|---|---|---|
| APFS clonefile | macOS on APFS volumes | Near-instant; pages shared until modified |
| Btrfs reflink | Linux on Btrfs/XFS/OCFS2 | Near-instant; CoW at block level |
| Full copy | Windows, HFS+, ext4, NFS | All files copied at staging time |

Large build artifacts can be excluded via `.taignore` (same syntax as `.gitignore`) or by the default exclude list (`target/`, `node_modules/`, `.venv/`, etc.).

**What it covers:** All files inside the project directory.

**What it does not cover:** Anything the agent writes outside the project — model checkpoints, generated images, system configs, other repos on disk. Those writes succeed and are invisible to TA governance.

**Security gaps:**
- Agent runs as the current user and can read any file the user can reach (`~/.ssh`, `~/.aws`, `~/.config/`, other repos). No read restriction exists at this tier.
- Writes outside the project directory land immediately and cannot be reviewed or rolled back by TA.
- A compromised or prompt-injected agent can exfiltrate secrets to an external service via `curl` unless the command sandbox blocks it.

**Configuration:** Active by default. Nothing in workflow.toml required.

---

## Tier 2 — Managed Paths

**Planned** — `allowed_paths`/`blocked_paths` for auto-approve conditions are implemented (`crates/ta-policy/src/document.rs`); the `governed_paths` extension and SHA filesystem are not yet built.

Tier 2 extends the overlay to directories outside the project. The agent writes to those directories normally, but TA intercepts each write, stores the content in a content-addressed store, and records a URI→SHA mapping in a journal. The result appears in `ta draft view` alongside project file diffs, and `ta draft apply` can replay or roll back those writes.

### SHA Filesystem + URI Journal

This is **not** a blob-of-changes model. It is a content-addressed file store with a path-alias journal — the same model git uses for its object store, applied to the working tree.

**Write algorithm:**

```
agent writes file at /data/comfyui/outputs/img.png
  → compute SHA256(content)
  → if .ta/sha-fs/<sha256> does not exist: write full content there
  → append to .ta/sha-journal.jsonl:
      {"uri":"fs://governed/comfyui/outputs/img.png",
       "sha":"<sha256>",
       "written_at":"...",
       "goal_id":"...",
       "size_bytes":...}
  → write succeeds to real path normally
```

Blobs in `.ta/sha-fs/` are immutable and deduplicated. Two files with identical content share one blob. The journal is the only mutable state.

**Read algorithm:**

```
agent reads file at /data/comfyui/outputs/img.png
  → look up most recent entry in .ta/sha-journal.jsonl with matching URI
  → if found: serve from .ta/sha-fs/<sha256>   (read-your-writes consistency)
  → if not found: serve from real on-disk path  (transparent fallback)
```

Reads of files the agent did not touch are zero-cost — no interception, no copy.

**Rollback:**

Rollback restores on-disk files to the SHA recorded in the pre-goal journal snapshot. The SHA blobs are never deleted during rollback (content-addressed, safe to retain). A separate GC process prunes blobs not referenced by any live journal entry.

**Draft integration:**

- `ta draft build` reads the journal and produces `Artifact` records with `resource_uri = "fs://governed/<relative-path>"` for each governed-path write.
- `ta draft view` shows these alongside project file diffs.
- `ta draft apply` writes each SHA blob's content to the real path.
- `ta draft deny` leaves real paths as-is.

**Security gap (Tier 2):** Writes to governed paths land immediately (the agent's write succeeds before TA's journal entry). `ta draft deny` does not undo them. Tier 2 provides auditability and replay, not prevention. Tier 3 (sandbox) is required for true write prevention on external paths.

**Configuration (planned):**

```toml
# .ta/workflow.toml

[[governed_paths]]
path = "/data/comfyui/outputs"
mode = "read-write"
purpose = "ComfyUI image outputs — all writes captured and diffed"
max_sha_store_mb = 4000

[[governed_paths]]
path = "/data/simpletrain/checkpoints"
mode = "read-write"
purpose = "SimpleTuner LoRA checkpoints"

[[governed_paths]]
path = "/etc/myapp/config"
mode = "read-only"          # agent may read but TA blocks writes
purpose = "Application config reference — changes must go through separate process"
```

---

## Tier 3 — Full OS Sandbox

**Implemented** (`crates/ta-runtime/src/sandbox.rs`) — disabled by default.

The agent process runs inside an OS-enforced isolation boundary. TA generates the sandbox profile at goal start and passes it to the OS before spawning the agent subprocess. The agent cannot reach outside the allowed paths, network endpoints, or processes regardless of what commands it runs.

**macOS — Seatbelt (`sandbox-exec`):**

TA generates a `.sb` profile and applies it via `sandbox-exec`. The profile uses `(deny default)` as its base rule. Explicit allow rules cover:

- Staging workspace: read-write.
- Rust/system libraries (`/usr`, `/Library/Frameworks`): read.
- Nix store (`/nix`): read (if present).
- DNS/hostname lookups (`/private/etc/hosts`, `/private/etc/resolv.conf`): read.
- `allow_network` hosts: TCP connect, filtered by hostname.

Anything not in the allow-list is denied at the kernel syscall boundary — not by TA logic.

**Linux — bubblewrap (`bwrap`):**

TA builds a `bwrap` command that:
- Bind-mounts system libraries read-only (`/usr`, `/lib`, `/lib64`).
- Bind-mounts the staging workspace read-write.
- Replaces `/tmp` with tmpfs.
- Unshares the network namespace by default (unless `allow_network` is non-empty, in which case host networking is used with TA command-level filtering as the secondary control).

Falls back to no sandboxing if `bwrap` is not found in PATH. A missing `bwrap` with sandbox enabled prints a warning; it does not silently degrade without notice.

**Windows:**

No OS-level sandbox is implemented. The command allowlist (`ta-sandbox/src/lib.rs`) is the only active containment on Windows. This is a known gap — a Windows Job Object or AppContainer implementation is tracked for a future phase.

**Command allowlist — always active on all platforms (`crates/ta-sandbox/src/lib.rs`):**

This is a separate, always-on layer that does not depend on sandbox enablement. It operates on every shell command the agent attempts:

- **Allow-list:** `rg`, `cargo`, `git`, `cat`, `jq`, `ls`, `find`, `grep` (and configured additions).
- **Network deny-by-default** with explicit allows: `github.com`, `crates.io`, `registry.npmjs.org`, `api.anthropic.com`.
- **Invocation limits** per command per session (prevents runaway loops).
- **Audit transcript hashing:** Every command's stdout/stderr is SHA-256 hashed and appended to the session audit log.

**What the sandbox does not cover:**

- Files the agent can read via paths explicitly allowed in the profile (e.g., system libraries could theoretically contain creds).
- Agent calls to TA's own MCP server — these are permitted by design. TA then applies its own action governance policy before executing external actions.
- Inter-process communication with already-running daemons (e.g., a local PostgreSQL server) if the DB socket path is reachable.

**Combining with Tier 1 and Tier 2:**

Tier 3 wraps both. Inside the sandbox, the agent sees only the staging workspace (plus allowed read paths). Writes the sandbox permits flow through the SHA journal. `ta draft deny` is now truly preventive: the agent cannot write to paths the sandbox profile does not allow, and the SHA journal captures everything it does write.

**Configuration:**

```toml
# .ta/workflow.toml
[sandbox]
enabled = true
provider = "auto"     # "auto" | "macos-seatbelt" | "linux-bwrap" | "none"

[sandbox.allow_read]
paths = ["/nix/store", "/usr", "/Library/Frameworks"]

[sandbox.allow_network]
hosts = ["api.anthropic.com", "github.com", "crates.io"]
# Empty list = block all network
```

---

## Implementation Status

| Capability | Status | Location |
|---|---|---|
| APFS clonefile staging | **Implemented** | `crates/ta-workspace/src/overlay.rs` |
| Btrfs reflink staging | **Implemented** | `crates/ta-workspace/src/overlay.rs` |
| Full-copy staging fallback | **Implemented** | `crates/ta-workspace/src/overlay.rs` |
| `.taignore` exclusion patterns | **Implemented** | `crates/ta-workspace/src/overlay.rs` |
| `allowed_paths`/`blocked_paths` (auto-approve conditions) | **Implemented** | `crates/ta-policy/src/document.rs` |
| `protected_paths` enforcement (general governance) | **Stub** | `examples/policy.yaml` — not enforced in code |
| `governed_paths` config | **Planned** | — |
| SHA filesystem store (`.ta/sha-fs/`) | **Planned** | — |
| URI journal (`.ta/sha-journal.jsonl`) | **Planned** | — |
| FUSE intercept for external path writes | **Planned** | — |
| Governed-path artifacts in draft view | **Planned** | — |
| macOS Seatbelt sandbox | **Implemented** | `crates/ta-runtime/src/sandbox.rs` |
| Linux bwrap sandbox | **Implemented** | `crates/ta-runtime/src/sandbox.rs` |
| Windows sandbox (Job Object / AppContainer) | **Not implemented** | — |
| Command allowlist (all platforms) | **Implemented** | `crates/ta-sandbox/src/lib.rs` |
| SHA blob GC | **Planned** | — |

---

## Security Gap Summary

| Threat | Tier 1 | Tier 2 | Tier 3 |
|---|---|---|---|
| Agent reads `~/.ssh` or `~/.aws` | Unprotected | Unprotected | Blocked (not in allow-list) |
| Agent writes outside project | Invisible to TA | Captured in SHA journal | Blocked at kernel level |
| Agent exfiltrates via curl | Command allowlist only | Command allowlist only | Sandbox + command allowlist |
| `ta draft deny` undoes write | Yes (staging not applied) | No (real path already written) | Yes (write was blocked) |
| Agent writes to DB via socket | Unprotected | Unprotected | Blocked if socket not in allow-list |
| Agent modifies system config | Unprotected | Captured if path governed | Blocked |

For projects where the agent handles credentials, production data, or where a malicious prompt injection is a concern, `[sandbox] enabled = true` is the correct baseline. Tier 2 managed paths is appropriate for external process outputs (ComfyUI, SimpleTuner) where performance matters and the trust model is "agent is trusted but we want full auditability and rollback."
