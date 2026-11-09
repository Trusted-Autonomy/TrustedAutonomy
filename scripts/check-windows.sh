#!/usr/bin/env bash
# Compile-only Windows check (v0.17.12.25 item 6): `cargo check --all-targets` for
# x86_64-pc-windows-msvc with warnings denied, the same compile gate CI runs on a
# Windows runner. `dead_code` on cfg(unix)-only helpers and other Windows-only compile
# failures show up here instead of 20-25 minutes into CI.
#
# Usage: scripts/check-windows.sh [--skip-if-unavailable] [extra cargo args, e.g. -p ta-goal]
#
#   --skip-if-unavailable  for the `[verify]` gate of `ta run` / `ta draft apply`: when
#                          the check cannot run on this machine (no rustup, target not
#                          installed, or already a Windows host) say so loudly and exit 0
#                          instead of blocking. Without the flag those cases exit 2.
#
# Needs the target installed once:  rustup target add x86_64-pc-windows-msvc
# The C dependencies (ring, aws-lc-sys) are given a stand-in C toolchain (see
# scripts/lib/win-stubs.sh), so the WHOLE workspace is checked from macOS or Linux.
# Only compiling is checked: nothing is linked, so Windows test EXECUTION (CRLF
# handling, paths) stays a CI job.
set -uo pipefail

TARGET=x86_64-pc-windows-msvc
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SKIP_IF_UNAVAILABLE=0
if [[ "${1:-}" == "--skip-if-unavailable" ]]; then
    SKIP_IF_UNAVAILABLE=1
    shift
fi

unavailable() { # message...
    echo "check-windows: $*" >&2
    if [[ $SKIP_IF_UNAVAILABLE -eq 1 ]]; then
        echo "check-windows: SKIPPED. Windows-only compile errors will NOT be caught before CI on this machine." >&2
        exit 0
    fi
    exit 2
}

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*)
        unavailable "this is already a Windows host: the ordinary build and tests cover it; nothing to cross-check."
        ;;
esac
if ! command -v rustup >/dev/null 2>&1; then
    unavailable "rustup not found; cannot verify the $TARGET target is installed. Install rustup (https://rustup.rs), then: rustup target add $TARGET"
fi
if ! (cd "$ROOT" && rustup target list --installed 2>/dev/null) | grep -qx "$TARGET"; then
    unavailable "target $TARGET is not installed. Run: rustup target add $TARGET   (adds only the target's std library)"
fi

STUBS="${TMPDIR:-/tmp}/ta-check-windows-stubs.$$"
trap 'rm -rf "$STUBS"' EXIT
# shellcheck source=lib/win-stubs.sh
. "$ROOT/scripts/lib/win-stubs.sh"
win_stubs_setup "$STUBS"

cd "$ROOT" || exit 2
echo "check-windows: RUSTFLAGS='-D warnings' cargo check --workspace --all-targets --target $TARGET $* (C dependencies stubbed; compile only)"
PATH="$WIN_STUBS_PATH:$PATH" RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D warnings" \
    cargo check --workspace --all-targets --target "$TARGET" "$@"
status=$?
if [[ $status -eq 0 ]]; then
    echo "check-windows: OK (whole workspace compiles for $TARGET)."
    exit 0
fi
echo "check-windows: FAILED (cargo exit $status). The errors above appear only when compiling for Windows:" >&2
echo "  a helper whose only caller is #[cfg(unix)] fails dead_code; put it under the same cfg or use it on both platforms." >&2
echo "  Re-run just this check with: scripts/check-windows.sh" >&2
exit $status
