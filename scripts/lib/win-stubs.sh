# shellcheck shell=bash
# shellcheck disable=SC2034  # WIN_STUBS_PATH is read by the scripts that source this file.
# Stand-in C toolchain for compile-only Windows checks (sourced, not executed).
#
# `cargo check`/`cargo clippy --target x86_64-pc-windows-msvc` compile every
# cfg(windows) line but never link. The C dependencies (ring, aws-lc-sys) still run
# build scripts that call a C compiler, archiver and nasm for MSVC, which a Mac or
# Linux host does not have. For THIS TARGET ONLY these stand-ins create the empty
# output files the build scripts expect, so the whole workspace type-checks. That is
# sound only because nothing is linked or run.
#
# Used by scripts/ci-local.sh (windows group) and scripts/check-windows.sh.
#
#   win_stubs_setup <dir>   creates <dir>/stub-cc, <dir>/stub-ar and <dir>/bin/nasm
#                           and exports CC_/AR_ for the Windows target plus
#                           WIN_STUBS_PATH (a directory holding only nasm, to put in
#                           front of PATH: a stub named cc/ar on PATH would break host
#                           linking of build scripts).

win_stubs_setup() {
    local dir="$1"
    mkdir -p "$dir/bin"
    cat >"$dir/stub-cc" <<'STUB'
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
    cat >"$dir/stub-ar" <<'STUB'
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
    cp "$dir/stub-ar" "$dir/bin/nasm"
    chmod +x "$dir/stub-cc" "$dir/stub-ar" "$dir/bin/nasm"
    export CC_x86_64_pc_windows_msvc="$dir/stub-cc"
    export AR_x86_64_pc_windows_msvc="$dir/stub-ar"
    WIN_STUBS_PATH="$dir/bin"
}
