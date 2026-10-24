#!/usr/bin/env bash
# Compile-only Windows check (v0.17.12.25 item 6): the same
# `cargo check --all-targets` with warnings denied that the "Windows Check" CI
# job runs, so `dead_code` on cfg(unix)-only helpers and other Windows-only
# compile failures show up before a PR is opened instead of 20-25 minutes into CI.
#
# Usage: scripts/check-windows.sh [extra cargo args, e.g. -p ta-goal]
#
# Needs the target installed once:  rustup target add x86_64-pc-windows-msvc
# Limit: crates that build C code for MSVC (ring, aws-lc-sys via TLS) need
# NASM / clang-cl and cannot be cross-checked from macOS or Linux. With
# --keep-going cargo still checks every crate that does not depend on them,
# and the script says so. CI checks the whole workspace on a Windows runner.
set -uo pipefail

TARGET=x86_64-pc-windows-msvc
if ! command -v rustup >/dev/null 2>&1; then
    echo "check-windows: rustup not found; cannot verify the $TARGET target is installed." >&2
    echo "  Install rustup (https://rustup.rs), then: rustup target add $TARGET" >&2
    exit 2
fi
if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
    echo "check-windows: target $TARGET is not installed." >&2
    echo "  Run: rustup target add $TARGET   (adds only the target's std library)" >&2
    exit 2
fi

echo "check-windows: RUSTFLAGS='-D warnings' cargo check --workspace --all-targets --keep-going --target $TARGET $*"
log=$(mktemp)
trap 'rm -f "$log"' EXIT
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D warnings" \
    cargo check --workspace --all-targets --keep-going --target "$TARGET" "$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}

# Failures from C-building dependencies are an environment limit, not a finding.
env_failures=$(grep -cE "failed to run custom build command for .(ring|aws-lc-sys)" "$log" || true)
real_errors=$(grep -E "^(error|warning)" "$log" \
    | grep -vE "failed to run custom build command for .(ring|aws-lc-sys)|build failed, waiting|could not compile .(ring|aws-lc-sys)|aborting due to|warning: .*(ring|aws-lc-sys)@" \
    | grep -vE "^error: (could not compile .* due to|failed to run custom build)" || true)

if [[ $status -eq 0 ]]; then
    echo "check-windows: OK (whole workspace compiles for $TARGET)."
    exit 0
fi
if [[ -n "$real_errors" ]]; then
    echo "check-windows: FAILED. Windows-only errors or warnings:" >&2
    echo "$real_errors" >&2
    exit 1
fi
if [[ $env_failures -gt 0 ]]; then
    echo "check-windows: no Windows errors in the crates that could be checked, but ring/aws-lc-sys" >&2
    echo "  cannot build C code for $TARGET on this host, so crates depending on TLS were NOT checked." >&2
    echo "  CI's 'Windows Check' job covers the whole workspace." >&2
    exit 0
fi
echo "check-windows: cargo failed (exit $status) for a reason this script does not recognise; see the output above." >&2
exit $status
