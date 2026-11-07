#!/usr/bin/env bash
# ci-local.sh: ONE command that runs every CI check on this machine, across the
# platforms it can reach, and prints a PASS/FAIL table plus the evidence line to
# paste into a pull request body. Validate here first; CI (.github/workflows/ci.yml)
# is then a cheap confirmation of an already-green change, not the development loop.
#
# Usage:
#   scripts/ci-local.sh                 everything below
#   scripts/ci-local.sh --quick         checks + host fmt/clippy only (about a minute warm)
#   scripts/ci-local.sh --only host     one or more groups: checks,host,windows,linux
#   scripts/ci-local.sh --skip-windows  --skip-docker (skip the Linux group)
#   scripts/ci-local.sh -v              stream every command's output (default: logs + tail on failure)
#   scripts/ci-local.sh -x              stop at the first failure
#   scripts/ci-local.sh --allow-dirty   print the evidence line even with uncommitted changes
#   scripts/ci-local.sh --clean         delete the caches and Docker volumes this script made
#
# Groups (each mirrors a CI job; see ci.yml):
#   checks   version consistency (Cargo.toml, CLAUDE.md, manifests), no-ip-docs, secret scan,
#            script self-tests, actionlint on the workflows (if installed)
#   host     fmt, clippy -D warnings, cargo test --workspace, standalone plugin tests, ta --version,
#            leftover-process guard, and on macOS the Seatbelt conformance suite
#            (= "Lint and Test (macos-latest)" and the fmt/clippy/test part of the ubuntu job)
#   windows  cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
#            on this machine's rustup toolchain (= the compile gate of "Windows Build")
#   linux    inside a Docker container (native architecture of the Docker VM; aarch64 on Apple
#            silicon): clippy, cargo test --workspace, and the sandbox conformance container cases
#            (= "Lint and Test (ubuntu-latest)" and "Linux Sandbox Conformance")
#
# What is NOT covered (printed again at the end of every run): Windows test EXECUTION and AppContainer
# conformance (clippy type-checks the Windows code; nothing runs), the x86_64 Linux architecture, the
# macOS runner image, real Keychain behaviour (never touched: TA_NO_KEYCHAIN=1, throwaway HOME), release
# builds and packaging (nightly/release workflows), and the GitHub-only glue in the workflows.
#
# Caching (re-runs are incremental): host and Windows target dirs live under $CI_LOCAL_CACHE
# (default ~/.cache/ci-local/ta); the Linux group uses Docker named volumes ta-ci-local-src,
# ta-ci-local-target, ta-ci-local-cargo. Disk: roughly 8-12 GB host + 4-6 GB Windows +
# 8-12 GB Linux volume once warm. Free it all with: scripts/ci-local.sh --clean
#
# Environment: CI_LOCAL_CACHE (cache root), CI_LOCAL_JOBS (test threads, default 4),
# CI_LOCAL_DOCKER_JOBS (cargo build jobs for Linux tests, default 2: linking many test binaries at once
# is killed by the out-of-memory killer in a small Docker VM). Exit status: 0 all selected steps passed, 1 a step
# failed, 2 usage or missing prerequisite. Works on macOS bash 3.2 and Linux bash.
#
# Safety: runs under `env -i` with a throwaway HOME (the real Keychain and real ~/.ta are never
# touched), TA_NO_KEYCHAIN=1 and TA_TEST_NO_EXTERNAL_AGENTS=1 (no test may reach a paid agent);
# the cargo runner that codesigns test binaries is replaced by /usr/bin/env.
set -o pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 2

ONLY=""
QUICK=0
SKIP_DOCKER=0
SKIP_WINDOWS=0
VERBOSE=0
FAILFAST=0
CLEAN=0
ALLOW_DIRTY=0

usage() { sed -n '2,/^set -o pipefail/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --quick) QUICK=1 ;;
        --only) shift; ONLY="${1:-}"; [ -n "$ONLY" ] || { echo "ci-local: --only needs a group list (checks,host,windows,linux)" >&2; exit 2; } ;;
        --only=*) ONLY="${1#--only=}" ;;
        --skip-docker|--skip-linux) SKIP_DOCKER=1 ;;
        --skip-windows) SKIP_WINDOWS=1 ;;
        -v|--verbose) VERBOSE=1 ;;
        -x|--fail-fast) FAILFAST=1 ;;
        --clean) CLEAN=1 ;;
        --allow-dirty) ALLOW_DIRTY=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "ci-local: unknown argument '$1'. Run: scripts/ci-local.sh --help" >&2; exit 2 ;;
    esac
    shift
done

REAL_HOME="$HOME"
CACHE="${CI_LOCAL_CACHE:-${XDG_CACHE_HOME:-$REAL_HOME/.cache}/ci-local/ta}"
JOBS="${CI_LOCAL_JOBS:-4}"
DJOBS="${CI_LOCAL_DOCKER_JOBS:-2}"
VOL_SRC=ta-ci-local-src
VOL_TARGET=ta-ci-local-target
VOL_CARGO=ta-ci-local-cargo
VOL_SANDBOX=ta-ci-local-sandbox-target

if [ "$CLEAN" -eq 1 ]; then
    echo "ci-local: removing $CACHE"
    rm -rf "$CACHE"
    if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
        for v in "$VOL_SRC" "$VOL_TARGET" "$VOL_CARGO" "$VOL_SANDBOX"; do
            docker volume rm -f "$v" >/dev/null 2>&1 && echo "ci-local: removed docker volume $v"
        done
        for i in $(docker images --format '{{.Repository}}:{{.Tag}}' 2>/dev/null | grep '^ta-ci-local'); do
            docker rmi "$i" >/dev/null 2>&1 && echo "ci-local: removed docker image $i"
        done
    else
        echo "ci-local: docker not reachable; Docker volumes/images (ta-ci-local-*) were not removed."
    fi
    echo "ci-local: clean done."
    exit 0
fi

want() { [ -z "$ONLY" ] && return 0; case ",$ONLY," in *",$1,"*) return 0 ;; esac; return 1; }

# ---- prerequisites ------------------------------------------------------------------
free_kb=$(df -Pk "$ROOT" 2>/dev/null | awk 'NR==2 {print $4}')
if [ -n "$free_kb" ] && [ "$free_kb" -lt 10485760 ]; then
    echo "ci-local: less than 10 GB free on the disk holding $ROOT ($((free_kb / 1048576)) GB). A warm run needs more." >&2
    echo "  Free space (scripts/ci-local.sh --clean, 'cargo clean', docker system prune) and re-run." >&2
    exit 2
fi
if ! command -v rustup >/dev/null 2>&1 && [ ! -x "$REAL_HOME/.cargo/bin/rustup" ]; then
    echo "ci-local: rustup not found. Install it (https://rustup.rs); the toolchain pinned by rust-toolchain.toml is installed from there." >&2
    exit 2
fi
RUSTUP_BIN="$REAL_HOME/.cargo/bin/rustup"; [ -x "$RUSTUP_BIN" ] || RUSTUP_BIN="$(command -v rustup)"
RUST_VER=$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' rust-toolchain.toml | head -1)
if [ -z "$RUST_VER" ]; then
    echo "ci-local: no 'channel = \"...\"' in rust-toolchain.toml; cannot pick the toolchain CI uses." >&2
    exit 2
fi

mkdir -p "$CACHE" || { echo "ci-local: cannot create cache dir $CACHE (set CI_LOCAL_CACHE)." >&2; exit 2; }
LOGDIR="$CACHE/logs/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$LOGDIR"
# Short, fixed prefix on purpose: unix-socket paths are limited to 104 bytes on macOS and some tests
# bind sockets under TMPDIR (a long $TMPDIR such as a nix-shell one makes them fail with SUN_LEN).
TMPHOME="$(mktemp -d /tmp/cil.XXXXXX)"
mkdir -p "$TMPHOME/tmp"
trap 'rm -rf "$TMPHOME"' EXIT
# Keep the five newest log directories.
ls -1dt "$CACHE"/logs/*/ 2>/dev/null | sed -n '6,$p' | while IFS= read -r d; do rm -rf "$d"; done

HOST_TRIPLE=$(env -i HOME="$TMPHOME" PATH="$REAL_HOME/.cargo/bin:/usr/bin:/bin" RUSTUP_TOOLCHAIN="$RUST_VER" rustc -vV 2>/dev/null | sed -n 's/^host: //p')
HOST_TRIPLE_ENV=$(printf '%s' "$HOST_TRIPLE" | tr 'a-z-' 'A-Z_')
BASE_PATH="$REAL_HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin"

# Run a command in a scrubbed environment. Cargo's codesigning runner is replaced by
# /usr/bin/env so no Keychain lookup ever happens.
host_env() {
    env -i HOME="$TMPHOME" TMPDIR="$TMPHOME/tmp" PATH="$BASE_PATH" LANG=en_US.UTF-8 \
        CARGO_HOME="${CARGO_HOME:-$REAL_HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$REAL_HOME/.rustup}" \
        RUSTUP_TOOLCHAIN="$RUST_VER" CARGO_TARGET_DIR="$CACHE/target" CARGO_TERM_COLOR=never \
        CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
        TA_NO_KEYCHAIN=1 TA_TEST_NO_EXTERNAL_AGENTS=1 \
        "CARGO_TARGET_${HOST_TRIPLE_ENV}_RUNNER=/usr/bin/env" \
        "$@"
}

# ---- step bookkeeping ---------------------------------------------------------------
NAMES=(); STATUS=(); SECS=(); NOTES=(); NOTCOV=()
add_result() { local i=${#NAMES[@]}; NAMES[$i]="$1"; STATUS[$i]="$2"; SECS[$i]="$3"; NOTES[$i]="$4"; }
any_fail() { local s; for s in "${STATUS[@]}"; do [ "$s" = FAIL ] && return 0; done; return 1; }

run_step() { # name command...
    local name="$1"; shift
    local n=${#NAMES[@]}
    local slug; slug=$(printf '%s' "$name" | tr -c 'A-Za-z0-9\n' '-')
    local log="$LOGDIR/$(printf '%02d' "$n")-$slug.log"
    if [ "$FAILFAST" -eq 1 ] && any_fail; then add_result "$name" SKIP 0 "earlier failure (-x)"; return 0; fi
    printf '[ci-local] RUN  %s\n' "$name"
    local t0 t1 rc
    t0=$(date +%s)
    if [ "$VERBOSE" -eq 1 ]; then "$@" 2>&1 | tee "$log"; rc=${PIPESTATUS[0]}; else "$@" >"$log" 2>&1; rc=$?; fi
    t1=$(date +%s)
    if [ "$rc" -eq 0 ]; then
        add_result "$name" PASS $((t1 - t0)) ""
        printf '[ci-local] PASS %s (%ss)\n' "$name" $((t1 - t0))
    else
        add_result "$name" FAIL $((t1 - t0)) "exit $rc; log $log"
        printf '[ci-local] FAIL %s (%ss, exit %s). Last lines of %s:\n' "$name" $((t1 - t0)) "$rc" "$log"
        [ "$VERBOSE" -eq 1 ] || tail -n 40 "$log" | sed 's/^/    | /'
        if grep -q -E 'signal: 9|signal 9|Killed|out of memory' "$log" 2>/dev/null; then
            echo "    HINT: a process was killed (signal 9), usually the Docker VM running out of memory while linking test binaries."
            echo "          Lower the parallelism (CI_LOCAL_DOCKER_JOBS=1) or give the VM more memory (colima stop && colima start --memory 12)."
        fi
    fi
}

skip_step() { # name reason
    add_result "$1" SKIP 0 "$2"
    printf '[ci-local] SKIP %s (%s)\n' "$1" "$2"
}

# ---- group: checks ------------------------------------------------------------------
if want checks; then
    run_step "version consistency" ./scripts/check-version-consistency.sh
    run_step "no IP docs in public repo" ./scripts/check-no-ip-docs.sh --all
    run_step "secret scan" bash -c './scripts/secret-scan.sh --self-test && ./scripts/secret-scan.sh'
    run_step "script self-tests" bash -c './scripts/ci-changes.sh --self-test && ./scripts/ci-leftover-processes.sh --self-test && ./scripts/test-publish-order.sh'
    if command -v actionlint >/dev/null 2>&1; then
        # shellcheck findings of style/info level in pre-existing workflow scripts are not CI failures.
        # plugin-release.yml names the retired runner label macos-13 (pre-existing; not a CI-cost topic).
        run_step "actionlint workflows" bash -c 'actionlint -shellcheck= -ignore "label \"macos-13\" is unknown" .github/workflows/*.yml'
    else
        skip_step "actionlint workflows" "actionlint not installed (brew install actionlint)"
        NOTCOV[${#NOTCOV[@]}]="workflow syntax lint (actionlint missing)"
    fi
fi

# ---- group: host --------------------------------------------------------------------
if want host; then
    HOST_OS=$(uname -s)
    if ! host_env rustup toolchain list 2>/dev/null | grep -q "^$RUST_VER"; then
        run_step "install rust $RUST_VER" host_env rustup toolchain install "$RUST_VER" --profile minimal --component clippy --component rustfmt --no-self-update
    fi
    run_step "host fmt" host_env cargo fmt --all -- --check
    run_step "host clippy ($HOST_TRIPLE)" host_env cargo clippy --workspace --all-targets -- -D warnings
    if [ "$QUICK" -eq 1 ]; then
        skip_step "host tests" "--quick"; NOTCOV[${#NOTCOV[@]}]="host tests, plugins, sandbox conformance (--quick)"
    else
        ps_count() { ps -axo command= 2>/dev/null | awk -v want="$1" '{n=split($1,p,"/"); if (p[n]==want) c++} END{print c+0}'; }
        before_gh=$(ps_count gh); before_ta=$(ps_count ta); before_daemon=$(ps_count ta-daemon)
        run_step "host tests ($HOST_TRIPLE)" host_env cargo test --workspace --no-fail-fast -- --test-threads="$JOBS"
        if [ "$HOST_OS" = Darwin ]; then
            # A skip is a failure here, as in CI: the Seatbelt isolation proof must actually run.
            run_step "host Seatbelt conformance" host_env TA_REQUIRE_SANDBOX_CONFORMANCE=1 \
                cargo test -p ta-runtime --test seatbelt_conformance -- --test-threads=1
        elif [ "$HOST_OS" = Linux ]; then
            skip_step "host Linux sandbox conformance" "run by the linux group (container) or CI"
        fi
        plugins_step() {
            local p
            for p in ta-channel-discord ta-channel-slack ta-channel-email; do
                host_env CARGO_TARGET_DIR="$CACHE/target-plugins-$p" cargo test --manifest-path "plugins/$p/Cargo.toml" || return 1
            done
        }
        run_step "standalone plugins (discord, slack, email)" plugins_step
        run_step "host ta --version (debug)" "$CACHE/target/debug/ta" --version
        after_gh=$(ps_count gh); after_ta=$(ps_count ta); after_daemon=$(ps_count ta-daemon)
        # The CI guard counts absolute processes on a fresh runner; a developer machine may already
        # run a daemon, so compare against the count before the tests instead.
        if [ $((after_daemon - before_daemon)) -gt 0 ] || [ $((after_gh - before_gh)) -gt 5 ] || [ $((after_ta - before_ta)) -gt 5 ]; then
            add_result "no leftover gh/ta-daemon/ta processes" FAIL 0 "new after tests: gh +$((after_gh - before_gh)), ta +$((after_ta - before_ta)), ta-daemon +$((after_daemon - before_daemon)); inspect: ps -axo pid,command | grep -E '(gh|ta|ta-daemon)( |$)'"
            printf '[ci-local] FAIL no leftover gh/ta-daemon/ta processes (gh +%s, ta +%s, ta-daemon +%s)\n' \
                $((after_gh - before_gh)) $((after_ta - before_ta)) $((after_daemon - before_daemon))
        else
            add_result "no leftover gh/ta-daemon/ta processes" PASS 0 ""
            echo "[ci-local] PASS no leftover gh/ta-daemon/ta processes (delta vs before tests)"
        fi
    fi
fi

# ---- group: windows -----------------------------------------------------------------
# Windows is type-checked and linted, not run. `cargo clippy --target x86_64-pc-windows-msvc` compiles
# every cfg(windows) line and denies warnings (dead_code on cfg(unix)-only helpers is the classic find).
# C dependencies (ring, aws-lc-sys) compile C in their build scripts and need the MSVC headers, which a
# Mac does not have, so for THIS TARGET ONLY the C compiler, archiver and nasm are stand-ins that create
# empty outputs. That is sound only because check/clippy never link; nothing is executed.
if want windows; then
    WIN_TARGET=x86_64-pc-windows-msvc
    if [ "$QUICK" -eq 1 ] || [ "$SKIP_WINDOWS" -eq 1 ]; then
        skip_step "windows clippy ($WIN_TARGET)" "$([ "$QUICK" -eq 1 ] && echo --quick || echo --skip-windows)"
        NOTCOV[${#NOTCOV[@]}]="windows compile/clippy (skipped by flag)"
    else
        if ! host_env rustup target list --toolchain "$RUST_VER" --installed 2>/dev/null | grep -qx "$WIN_TARGET"; then
            run_step "windows add rustup target" host_env rustup target add --toolchain "$RUST_VER" "$WIN_TARGET"
        fi
        STUBS="$CACHE/win-stubs"
        mkdir -p "$STUBS/bin"
        cat >"$STUBS/stub-cc" <<'STUB'
#!/bin/sh
# Stand-in C compiler for cargo check/clippy: "compiling" creates the -o file.
out=""; prev=""
for a in "$@"; do
  [ "$prev" = "-o" ] && out="$a"
  case "$a" in -o?*) out="${a#-o}";; /Fo*) out="${a#/Fo}";; esac
  prev="$a"
done
case " $* " in *" --version "*|*" -v "*) echo "stub-cc 0.0";; esac
[ -n "$out" ] && [ "$out" != /dev/null ] && : > "$out"
exit 0
STUB
        cat >"$STUBS/stub-ar" <<'STUB'
#!/bin/sh
# Stand-in archiver and nasm: create the output archive/object named on the command line.
for a in "$@"; do
  case "$a" in -out:*|-OUT:*|/OUT:*|/out:*) p="${a#*:}"; : > "$p"; exit 0;; esac
done
prev=""
for a in "$@"; do
  [ "$prev" = "-o" ] && { : > "$a"; exit 0; }
  prev="$a"
done
for a in "$@"; do
  case "$a" in -*|/*|cq|crs|cr|rcs|s|q|r|c) continue;; esac
  : > "$a"; exit 0
done
exit 0
STUB
        cp "$STUBS/stub-ar" "$STUBS/bin/nasm"
        chmod +x "$STUBS/stub-cc" "$STUBS/stub-ar" "$STUBS/bin/nasm"
        # nasm sits alone in $STUBS/bin: a stub named cc/ar on PATH would break host linking of build scripts.
        win_env() {
            host_env PATH="$STUBS/bin:$BASE_PATH" CARGO_TARGET_DIR="$CACHE/target-windows" \
                CC_x86_64_pc_windows_msvc="$STUBS/stub-cc" AR_x86_64_pc_windows_msvc="$STUBS/stub-ar" "$@"
        }
        run_step "windows clippy ($WIN_TARGET)" win_env cargo clippy --workspace --all-targets --target "$WIN_TARGET" -- -D warnings
        NOTCOV[${#NOTCOV[@]}]="windows test execution and AppContainer conformance (type-check + clippy only; C deps stubbed, nothing linked or run)"
    fi
fi

# ---- group: linux (Docker) ----------------------------------------------------------
if want linux; then
    if [ "$QUICK" -eq 1 ] || [ "$SKIP_DOCKER" -eq 1 ]; then
        skip_step "linux (docker)" "$([ "$QUICK" -eq 1 ] && echo --quick || echo --skip-docker)"
        NOTCOV[${#NOTCOV[@]}]="linux clippy/tests/sandbox conformance (skipped by flag)"
    elif ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
        add_result "linux (docker)" FAIL 0 "docker is not running or not installed; start Docker/colima (colima start) or pass --skip-docker"
        echo "[ci-local] FAIL linux (docker): docker is not reachable. Start it (colima start / Docker Desktop) or pass --skip-docker." >&2
    else
        IMG="ta-ci-local:$RUST_VER"
        build_image() {
            docker image inspect "$IMG" >/dev/null 2>&1 && return 0
            docker build -t "$IMG" - <<EOF
FROM rust:$RUST_VER-bookworm
RUN rustup component add clippy rustfmt \\
 && useradd -m -u 1000 ci \\
 && mkdir -p /work /tgt /cargo-home \\
 && chown ci:ci /work /tgt /cargo-home
USER ci
ENV CARGO_HOME=/cargo-home CARGO_TARGET_DIR=/tgt
WORKDIR /work
EOF
        }
        # Copy the working tree (tracked + untracked, not ignored, deleted files skipped) into a volume, so
        # the container sees exactly what is about to be committed and the host tree is never written.
        # tar keeps mtimes, so unchanged files keep their cargo fingerprints between runs.
        sync_src() {
            docker volume create "$VOL_SRC" >/dev/null && docker volume create "$VOL_TARGET" >/dev/null \
                && docker volume create "$VOL_CARGO" >/dev/null || return 1
            ( cd "$ROOT" && git ls-files -z --cached --others --exclude-standard | while IFS= read -r -d '' f; do
                    [ -e "$f" ] && printf '%s\0' "$f"
                done | COPYFILE_DISABLE=1 tar -c --null -T - -f - ) \
                | docker run --rm -i -v "$VOL_SRC:/work" -v "$VOL_TARGET:/tgt" -v "$VOL_CARGO:/cargo-home" "$IMG" \
                    sh -c 'find /work -mindepth 1 -delete && tar -x -C /work && git init -q /work 2>/dev/null; true'
        }
        lx() { # command... (runs as the non-root ci user, like a CI runner)
            # /dev/shm is noexec by default in Docker; some tests run mock plugin scripts from it (it is exec on runners).
            docker run --rm --tmpfs /dev/shm:rw,exec,nosuid,size=512m -v "$VOL_SRC:/work" -v "$VOL_TARGET:/tgt" -v "$VOL_CARGO:/cargo-home" \
                -e CARGO_INCREMENTAL=0 -e CARGO_PROFILE_DEV_DEBUG=0 -e CARGO_PROFILE_TEST_DEBUG=0 -e CARGO_TERM_COLOR=never \
                -e TA_NO_KEYCHAIN=1 -e TA_TEST_NO_EXTERNAL_AGENTS=1 "$IMG" "$@"
        }
        run_step "linux docker image ($IMG)" build_image
        run_step "linux sync source into volume" sync_src
        run_step "linux fmt" lx cargo fmt --all -- --check
        run_step "linux clippy ($(docker info --format '{{.Architecture}}' 2>/dev/null))" lx cargo clippy --workspace --all-targets -- -D warnings
        # GNU ld on the Docker VM (often ~8 GB shared with other containers) is OOM-killed linking the big test
        # binaries; these link flags only trade link speed for memory and do not change the compiled code.
        run_step "linux tests" lx env CARGO_BUILD_JOBS="$DJOBS" \
            RUSTFLAGS="-C link-arg=-Wl,--no-keep-memory -C link-arg=-Wl,--reduce-memory-overheads" \
            cargo test --workspace --no-fail-fast -- --test-threads="$JOBS"
        run_step "linux ta --version (debug)" lx /tgt/debug/ta --version
        # Degraded-container cases of "Linux Sandbox Conformance": no user namespaces / no Landlock.
        run_step "linux sandbox conformance (containers)" env TA_CI_RUST_IMAGE="rust:$RUST_VER-bookworm" \
            TA_CI_SANDBOX_VOLUME="$VOL_SANDBOX" ./scripts/ci-linux-sandbox-containers.sh all
        NOTCOV[${#NOTCOV[@]}]="linux sandbox HOST suite (bubblewrap + Landlock + seccomp on a bare runner kernel): only the container cases run here"
        NOTCOV[${#NOTCOV[@]}]="linux x86_64 (Docker VM architecture is $(docker info --format '{{.Architecture}}' 2>/dev/null))"
    fi
fi

# ---- summary ------------------------------------------------------------------------
echo
echo "================ ci-local summary ($(git rev-parse --short HEAD 2>/dev/null)) ================"
printf '%-6s %-52s %6s  %s\n' RESULT STEP SECS NOTE
i=0; npass=0; nfail=0; nskip=0; skipped=""
while [ $i -lt ${#NAMES[@]} ]; do
    printf '%-6s %-52s %6s  %s\n' "${STATUS[$i]}" "${NAMES[$i]}" "${SECS[$i]}" "${NOTES[$i]}"
    case "${STATUS[$i]}" in
        PASS) npass=$((npass + 1)) ;;
        FAIL) nfail=$((nfail + 1)) ;;
        SKIP) nskip=$((nskip + 1)); skipped="${skipped:+$skipped; }${NAMES[$i]} (${NOTES[$i]})" ;;
    esac
    i=$((i + 1))
done
echo "-----------------------------------------------------------------------------------"
echo "pass=$npass fail=$nfail skip=$nskip   logs: $LOGDIR"
if [ ${#NOTCOV[@]} -gt 0 ]; then
    echo "NOT covered by this run:"
    for c in "${NOTCOV[@]}"; do echo "  - $c"; done
fi
SHA=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)
DATE=$(date +%Y-%m-%d)
DIRTY=""
if [ -n "$(git status --porcelain --untracked-files=normal 2>/dev/null)" ]; then DIRTY=1; fi

if [ "$nfail" -gt 0 ]; then
    echo "ci-local: FAILED at $SHA $DATE ($nfail failing step(s)); fix them and re-run: scripts/ci-local.sh"
    exit 1
fi
if [ "$npass" -eq 0 ]; then
    echo "ci-local: nothing ran (check --only/--quick/--skip flags)." >&2
    exit 2
fi
if [ -n "$DIRTY" ] && [ "$ALLOW_DIRTY" -eq 0 ]; then
    echo "ci-local: all selected steps passed, but the working tree has uncommitted changes, so no evidence line is printed"
    echo "  (the sha would not describe what was validated). Commit, then re-run (warm caches make it quick), or pass --allow-dirty."
    exit 0
fi
if [ "$nskip" -gt 0 ]; then
    echo "ci-local: green at $SHA $DATE (NOT covered: $skipped)"
else
    echo "ci-local: all green at $SHA $DATE"
fi
exit 0
