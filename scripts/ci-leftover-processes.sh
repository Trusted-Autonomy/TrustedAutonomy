#!/usr/bin/env bash
# ci-leftover-processes.sh: regression guard for processes a CI test run leaves
# behind (macOS and Linux).
#
# Usage:
#   scripts/ci-leftover-processes.sh                 list and check leftovers
#   scripts/ci-leftover-processes.sh --self-test     prove the counting and the
#                                                    limits with a fake process list
#
# Lists every running gh, ta-daemon and ta process, prints them, and exits 1
# when there is any ta-daemon, or more than MAX_GH gh or MAX_TA ta processes.
# Exit status: 0 within limits, 1 over a limit, 2 usage error.
#
# Env overrides: MAX_GH (default 5), MAX_TA (default 5).
# Matches on the basename of the command's first word only, so a path or
# argument that merely contains "gh" never counts. (ps `comm` is truncated on
# macOS when other columns follow, so the full command line is used.)
# Needs only ps and awk.

set -u

MAX_GH="${MAX_GH:-5}"
MAX_TA="${MAX_TA:-5}"

# Reads "pid command-line" lines on stdin, prints "pid name args" for watched names.
filter_watched() {
    awk '{
        pid = $1; exe = $2; n = split(exe, parts, "/"); base = parts[n];
        if (base == "gh" || base == "ta-daemon" || base == "ta") {
            args = ""; for (i = 3; i <= NF; i++) args = args " " $i;
            print pid, base, args
        }
    }'
}

# Reads filtered lines on stdin; prints the verdict and returns 0 or 1.
evaluate() {
    local lines gh ta daemon
    lines="$(cat)"
    gh=$(printf '%s\n' "$lines" | awk '$2 == "gh"' | grep -c .)
    ta=$(printf '%s\n' "$lines" | awk '$2 == "ta"' | grep -c .)
    daemon=$(printf '%s\n' "$lines" | awk '$2 == "ta-daemon"' | grep -c .)
    echo "Leftover processes: gh=$gh (limit $MAX_GH), ta=$ta (limit $MAX_TA), ta-daemon=$daemon (limit 0)"
    if [ -n "$lines" ]; then
        echo "Process list (pid name args):"
        printf '%s\n' "$lines" | head -n 50
        total=$(printf '%s\n' "$lines" | grep -c .)
        [ "$total" -gt 50 ] && echo "... $((total - 50)) more not shown"
    fi
    local bad=0
    if [ "$daemon" -gt 0 ]; then
        echo "ERROR: $daemon ta-daemon process(es) left running. A test started a daemon and did not shut it down."
        bad=1
    fi
    if [ "$gh" -gt "$MAX_GH" ]; then
        echo "ERROR: $gh gh processes left running (limit $MAX_GH). A test is reaching the real gh or a gh wrapper is re-executing itself."
        bad=1
    fi
    if [ "$ta" -gt "$MAX_TA" ]; then
        echo "ERROR: $ta ta processes left running (limit $MAX_TA). A test spawned ta and did not wait on or kill it."
        bad=1
    fi
    if [ "$bad" -eq 1 ]; then
        echo "Fix: find the test that spawns the process (see CONTRIBUTING.md, 'Hermetic tests and the leftover-process check'), or raise MAX_GH / MAX_TA only with a reason."
    fi
    return "$bad"
}

self_test() {
    local fail=0 out
    out=$(printf '%s\n' "1 /usr/bin/gh pr list" "2 /bin/zsh -c gh" "3 /x/ta run" | filter_watched)
    [ "$(printf '%s\n' "$out" | grep -c .)" = 2 ] || { echo "self-test FAIL: filter should keep 2 of 3 lines, got: $out"; fail=1; }

    printf '%s\n' "1 gh" "2 gh" | filter_watched | evaluate >/dev/null || { echo "self-test FAIL: 2 gh must pass"; fail=1; }
    printf '%s\n' "1 ta-daemon --api" | filter_watched | evaluate >/dev/null && { echo "self-test FAIL: ta-daemon must fail"; fail=1; }
    local many=""
    for i in 1 2 3 4 5 6; do many="$many$i gh\n"; done
    printf "$many" | filter_watched | evaluate >/dev/null && { echo "self-test FAIL: 6 gh must fail"; fail=1; }
    [ "$fail" -eq 0 ] && echo "self-test OK"
    return "$fail"
}

case "${1:-}" in
    --self-test) self_test; exit $? ;;
    "") ;;
    *) echo "usage: $0 [--self-test]" >&2; exit 2 ;;
esac

echo "Checking for leftover gh, ta-daemon and ta processes (this host: $(uname -s))"
ps -axww -o pid= -o args= | filter_watched | evaluate
