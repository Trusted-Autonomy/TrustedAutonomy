# Lazy copy-on-write staging

Status: implemented as the opt-in `[staging] strategy = "lazy"` (PLAN.md v0.17.12.32).

## Problem

`ta goal start` / `ta run` build the agent a private, normal-looking copy of the
project under `.ta/staging/<goal-id>/`. The full-copy overlay pays for that copy
up front (bytes and time proportional to the project), then pays again at the
end: the diff reads every file in both trees, and conflict detection hashes
every source file at goal start and again at apply time. The build-output
excludes (`DEFAULT_EXCLUDES`, `.taignore`) exist because the copy cannot afford
`target/` and `node_modules/`.

The goal: staging that costs disk and time only for files the agent writes, with
the contract unchanged.

## Contract (must not change)

1. The agent sees an ordinary directory tree and works with its normal tools
   (including shell commands, `sed -i`, `cargo`, `git`). TA is invisible to it.
2. Reads of files the agent has not written see the source content as it was at
   goal start.
3. A draft built from staging contains exactly the artifacts the agent created,
   modified or deleted, nothing else.
4. Apply never overwrites a source file that changed after the goal started
   without conflict detection firing, and never writes outside the target root.
5. No code path can lose or corrupt a user file in the source tree.

## Options considered

| Option | What it is | Verdict |
| --- | --- | --- |
| Hardlink on read, copy on first write | Staging holds hardlinks to source inodes; a write breaks the link | **Rejected.** Nothing intercepts the write. `echo > f`, `sed -i` with some flags, editors that truncate in place, `cargo` writing `Cargo.lock` all write through the shared inode and modify the *source* file. Making linked files read-only would instead break the agent (and `chmod` on the shared inode changes the source's mode). Unsafe by construction. |
| Symlinks to source (`smart` mode today) | Staging holds symlinks | **Rejected for files** (a write through the link modifies source). Kept only for the excluded directories `smart` mode already links. |
| FUSE / macFUSE / File Provider mount | A user-space filesystem serves reads from source and redirects writes to a scratch layer | Correct, but needs a kernel extension or a signed system extension on macOS, `/dev/fuse` and `fusermount` on Linux, and a long-lived supervised process whose death hangs every agent. Too heavy and too fragile for a default; not implemented. |
| overlayfs (Linux) | `lowerdir=source`, `upperdir=scratch` | Mounting needs `CAP_SYS_ADMIN` or an unprivileged user namespace (kernel 5.11+) in which the **agent** must then run. That couples staging to the sandbox launcher. Not implemented; the design leaves room for it (see Future). |
| ProjFS (Windows) | Kernel projection of source, writes land in scratch | Already shipped as `strategy = "projfs"` (feature-gated, needs the Client-ProjFS optional feature). Unchanged by this work. |
| Per-file clone (`FICLONE` / `clonefile` per file) | Each file becomes a CoW clone | What the full-copy overlay already does on APFS and Btrfs/XFS. Zero data copied, but one syscall and one inode per file, and the diff and snapshot still read every file. |
| **Clone of directory trees (APFS `clonefile(2)` on directories)** | One syscall clones a whole directory subtree; blocks are shared until written | **Chosen on macOS.** The kernel is the copy-on-write layer, so every writer (shell, editor, compiler) is handled with no interposition. |

The chosen design is therefore two independent parts:

* **Population** uses the cheapest copy-on-write clone the OS offers, so
  creating staging costs no data blocks, and falls back to the full copy where
  none exists.
* **Change tracking** records a metadata baseline of the staging tree right after
  population, so diff, apply and conflict detection touch only files the agent
  wrote, created or deleted. This part works the same for a clone and for the
  full-copy fallback.

## Mechanism per OS

| Platform | Population | Notes |
| --- | --- | --- |
| macOS, APFS, source and staging on one volume | `clonefile(2)` per directory subtree (`CLONE_NOFOLLOW`), per-file `clonefile` inside directories that need filtering | Probed against a real source file first. |
| Linux, Btrfs / XFS with reflink / bcachefs | `FICLONE` per file (Linux has no directory clone) | Probed first. ext4 and tmpfs have no reflink and use the fallback. |
| Everything else (ext4, NTFS, HFS+, cross-volume, Windows) | The existing full copy | Reported by `ta goal start` with the reason. Windows can opt into `projfs` explicitly. |

Selection is automatic per goal: `strategy = "lazy"` asks for it, the probe
decides whether it is available, and the goal output says which one was used.

### Directory clones and the excludes

A directory is cloned in one syscall only when its whole subtree is "pure": no
entry that the excludes drop (`target/`, `node_modules/`, `.git/`, `.ta/`,
`.taignore` entries), no symlink, no special file. Impure directories are
created and filled entry by entry; their pure subdirectories are still cloned
whole. The pre-scan reads directory entries only (no per-file `stat`).

`DEFAULT_EXCLUDES` stays. A clone makes unwritten files free, but it does not
make them irrelevant: a cloned `target/` still costs inodes and clone time and
shows the agent a stale build tree, and the agent's own `cargo build` writes
into `target/` inside staging, which is a *write* that would otherwise appear as
thousands of artifacts. The measured cost of cloning the excluded trees is in
[Measurements](#measurements). The comments that called the list "V1
temporary" were removed; the list is the permanent definition of what is never
reviewable work product.

## Change tracking

### Baseline

Right after population (and after the ephemeral staging files are removed),
TA walks the staging tree once with `lstat` and stores, per file:

* size, mtime (ns), ctime (ns), inode, mode, and the link target for symlinks.

The baseline lives in `.ta/staging-baselines/<goal-id>.json`, **outside** the
staging directory, so an agent that can write to its staging tree cannot edit
the record of what it started with and hide a change from review. Everything
that removes a staging directory is safe: an orphaned baseline is swept the
next time a lazy staging is created, and `OverlayWorkspace::cleanup` removes it.

A file is "untouched" when its fingerprint equals the baseline. `ctime` cannot
be set from user space and changes on any write, truncate, `chmod`, rename or
replace (a replace also changes the inode), so `touch -r`, `utime`, and
same-size rewrites are all detected. Files whose fingerprint differs are then
compared with the source by content exactly as the full-copy diff does, so a
file the agent rewrote with identical content (formatters, `touch`) is still not
an artifact, and TA's own CLAUDE.md inject/restore cycle still diffs clean.

Timestamp granularity race ("racy git"): a write in the same timestamp tick as
the baseline `lstat` would keep an identical fingerprint. After population TA
waits one tick (25 ms on nanosecond-timestamp filesystems, 2 s when sampled
timestamps all have zero nanoseconds, i.e. HFS+/FAT) before taking the
baseline, and any entry whose timestamp is still within a tick of the capture
time is marked racy and always compared by content.

Platforms without `ctime` in `std` (Windows) do not trust fingerprints: every
staging file is compared by content (the cost of the full-copy diff), while
deletions are still taken from the baseline.

### Diff

`diff_all` with a baseline:

* modified or created: staging files whose fingerprint differs from the baseline
  (or that are not in it), then content-compared with source;
* deleted: baseline paths absent from staging that still exist in source. A file
  somebody else added to source after the goal began is not in the baseline and
  can never be reported as a deletion (same guarantee as the v0.17.10.3
  base-snapshot check);
* unchanged files are never opened, and source files are not walked at all.

A changed or new symlink in staging is only followed when its resolved target
stays inside the staging root; otherwise it is skipped with a warning, never
read, and never applied (an agent must not be able to turn a symlink to
`/etc/passwd` into a source file at apply).

### Conflict detection

The full-copy overlay hashes every source file at goal start (SHA-256 of the
whole project) and again at apply time. The lazy snapshot records only
`size` and `mtime` for ordinary files, plus a content hash for the "shared"
files that are 3-way merged (`PLAN.md`, `CLAUDE.md`, `Cargo.toml`,
`docs/USAGE.md`, `memory/*.md`). At apply time a source file counts as changed
when it is missing or its size or mtime differs from the snapshot. A touch
without content change is therefore reported as a change, which is the
conservative direction: it can only ever create a conflict prompt for a file the
agent also wrote (conflicts on files outside the draft are listed as safe and
skipped), never hide a real one. Apply continues to refuse overlapping conflicts
unless the user chooses merge or force exactly as before.

## Symlinks and the O_NOFOLLOW checks

Population never follows a symlink: entries are read with `symlink_metadata`,
directory clones use `CLONE_NOFOLLOW`, and a relative symlink whose target
normalises to a location inside the project root is recreated verbatim
(the agent sees the same link and it resolves inside staging). Anything else
would let a write through staging land in the real tree or outside the project:

* absolute targets, and relative targets that escape the project root: a link to
  a regular file is replaced by a copy of the file's content (what the full-copy
  overlay does for every symlink); a link to a directory is skipped with a
  warning naming the link and its target;
* dangling in-root relative links are preserved (they are dangling in source too).

Special files (sockets, FIFOs, devices) are skipped; copying a FIFO would block
goal start forever.

The baseline walk and the diff use `symlink_metadata`, never recursing into
symlinked directories, so a link cycle cannot hang them.

## How agents that write via shell see it

Nothing special: the staging tree is real files on a real filesystem. A shell
redirect, `sed -i`, `mv`, `rm`, `git checkout`, a compiler, all operate on
ordinary files; on APFS and Btrfs the kernel breaks sharing on the first write to
a block. The mechanisms that need interception to be safe (hardlinks, symlinks)
were rejected for exactly this reason.

## Crash safety

* Population writes `.ta/staging-baselines/<goal-id>.creating` first and removes
  it after the baseline is saved. If TA dies in between, the staging tree is
  partial: `diff_all` refuses to run on it with an error that names the marker
  and says to delete the staging directory and start the goal again, instead of
  reporting every missing file as a deletion.
* The baseline is written to a temp file and renamed into place.
* The agent being killed mid-write leaves a truncated file in *staging* only; it
  shows up as a modified artifact for review. Source is never written until
  `ta draft apply`, which keeps its existing journal and rollback guard.
* Clones are independent files: deleting staging, or the agent deleting files in
  it, can never affect source.
* Partial directory clones (a failed `clonefile` on one subtree) are removed and
  that subtree is redone entry by entry; a failure that survives the fallback
  fails goal start loudly.

## Configuration and reporting

```toml
[staging]
strategy = "lazy"   # "full" (default) | "smart" | "refs-cow" | "projfs" | "lazy"
```

`ta goal start` and `ta run` print the strategy actually used and, when lazy was
requested but unavailable, why it fell back. `ta doctor` reports the configured
strategy. Default stays `full` until the lazy path has soaked on real goals; the
full copy remains the fallback and the default for every unsupported platform.

## Future

* overlayfs inside the sandbox's user namespace on Linux (zero population cost,
  no per-file inodes); the baseline/diff half is already mechanism-independent
  except that an overlay's upper layer lists the written files directly.
* Windows ReFS block cloning for `refs-cow` (currently a stub that falls back to
  `smart`).

## Measurements

Measured with the debug build of `ta` on macOS arm64 / APFS, on a scratch
`cp -cR` copy of this repository (without `target/`; staged tree after the
default excludes: 1608 files, 49.1 MB). The machine was busy with other builds,
so wall times are noisy; disk numbers come from an isolated 400 MB APFS volume
where `df` is exact.

| Measure | Full copy (per-file APFS clone) | Lazy |
| --- | --- | --- |
| Staging creation (reported by `ta goal start`), this repo, 3 runs | 0.7 s, 0.8 s, 0.9 s | 0.33 s, 0.92 s, 0.94 s (one 0.54 s and one 0.86 s in earlier runs) |
| Whole `ta goal start` command, this repo, 3 runs | 11.1 s, 12.8 s, 13.6 s | 3.3 s, 5.0 s, 7.5 s |
| Syscalls to populate | 1608 file clones | 110 directory clones + 63 file clones |
| Physically copied bytes | 0 | 0.1 MB (1 file) |
| Disk used per goal on an isolated APFS volume (49 MB project) | 4.3 MB, 4.4 MB | 4.3 MB, 4.3 MB |
| Baseline file | none | 211 KB JSON (1608 files), outside staging |
| `ta draft build`, goal that wrote nothing | 5.4 s | 4.9 s (reports "no changes", as full does) |
| `ta draft build`, 5 changes (1 modify, 2 delete, 1 rename = delete + create, 1 create) | 2.9 s | 4.7 s; identical artifact list |
| `ta draft apply` of that draft, lazy (modify, delete, rename written through) | not run | 3.6 min (the apply path re-hashes the whole source when the source moved; unchanged by this work) |
| Cloning the excluded `target/` into staging (29,089 files, ~12 GB logical) | n/a | 10.3 s, which is why the excludes stay |

Reading the numbers honestly:

* On APFS the full copy already clones per file, so lazy does **not** save disk
  there: both cost about 4.3 MB per goal on this repo, almost all of it goal
  records and directory metadata, not file data. A goal that writes nothing costs
  about that in both modes.
* Staging creation time is about the same on this repo, because the default
  excludes already leave only 1608 files to stage.
* The saving is the rest of goal start: lazy does not read or hash the source
  tree (the full mode SHA-256s every source file for conflict detection), which
  took the whole command from 11-14 s to 3-8 s here, and the diff reads only the
  files the agent wrote.
* `ta draft build` is not faster at this size (stat-walk of 1608 files vs reading
  1608 files from page cache are both small next to the CLI fixed cost of about
  3-5 s in a debug build). The difference grows with project size and cold caches;
  that was not measured here.
* Not measured: Linux Btrfs/XFS reflink, ext4/NTFS fallback, Windows. Those paths
  are type-checked and clippy-clean for `x86_64-unknown-linux-gnu` and
  `x86_64-pc-windows-msvc`, and the conformance suite runs on them in CI, but they
  were not executed here.
