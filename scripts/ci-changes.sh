#!/usr/bin/env bash
# ci-changes.sh: classify a list of changed files so CI (and scripts/ci-local.sh)
# run only the jobs a change can affect.
#
# Usage:
#   git diff --name-only origin/main...HEAD | scripts/ci-changes.sh
#   scripts/ci-changes.sh --self-test
#
# Reads one path per line on stdin and prints three lines, suitable for
# `>> "$GITHUB_OUTPUT"`:
#   code=true|false      false only when EVERY file is a repo-root *.md or under docs/.
#                        Markdown under crates/, apps/ or templates/ is code because
#                        include_str! compiles it into binaries.
#   sandbox=true|false   true when a file can change the Linux sandbox conformance
#                        result (ta-runtime, the container script, toolchain, lockfile, CI).
#   plugins=true|false   true when a standalone channel plugin (discord, slack, email),
#                        the toolchain or CI itself changed.
# An empty list (API failure, new branch) answers true for all three: anything
# uncertain runs the full pipeline.
#
# Exit status: 0 ok, 2 usage error or failed self-test.
set -u

classify() {
    local files code=false sandbox=false plugins=false any=false f
    files=$(cat)
    while IFS= read -r f; do
        [ -z "$f" ] && continue
        any=true
        if ! [[ "$f" =~ ^([^/]+\.md|docs/.*)$ ]]; then
            code=true
        fi
        if [[ "$f" =~ ^(crates/ta-runtime/|crates/ta-sandbox/|scripts/ci-linux-sandbox-containers\.sh$|Cargo\.lock$|rust-toolchain\.toml$|\.github/(workflows/ci\.yml|actions/)) ]]; then
            sandbox=true
        fi
        if [[ "$f" =~ ^(plugins/ta-channel-(discord|slack|email)/|rust-toolchain\.toml$|\.github/(workflows/ci\.yml|actions/)) ]]; then
            plugins=true
        fi
    done <<<"$files"
    if [ "$any" = false ]; then
        code=true; sandbox=true; plugins=true
    fi
    printf 'code=%s\nsandbox=%s\nplugins=%s\n' "$code" "$sandbox" "$plugins"
}

self_test() {
    local fail=0
    check() { # name expected-output files...
        local name="$1" want="$2"; shift 2
        local got
        got=$(printf '%s\n' "$@" | classify | tr '\n' ' ')
        if [ "$got" != "$want " ]; then
            echo "ci-changes self-test FAILED: $name: wanted '$want' got '$got'" >&2
            fail=1
        fi
    }
    check "docs only" "code=false sandbox=false plugins=false" README.md docs/USAGE.md docs/a/b.md PLAN.md
    check "crate markdown is code" "code=true sandbox=false plugins=false" crates/ta-goal/src/prompt.md
    check "runtime is sandbox" "code=true sandbox=true plugins=false" crates/ta-runtime/src/lib.rs
    check "plugin" "code=true sandbox=false plugins=true" plugins/ta-channel-slack/src/main.rs
    check "workflow" "code=true sandbox=true plugins=true" .github/workflows/ci.yml
    check "other workflow is code but not sandbox" "code=true sandbox=false plugins=false" .github/workflows/nightly.yml
    check "docs plus code" "code=true sandbox=false plugins=false" docs/USAGE.md apps/ta-cli/src/main.rs
    check "nested md is code" "code=true sandbox=false plugins=false" docs/x.md plugins/vscode-ta/README.md
    local empty
    empty=$(printf '' | classify | tr '\n' ' ')
    [ "$empty" = "code=true sandbox=true plugins=true " ] || { echo "ci-changes self-test FAILED: empty list: '$empty'" >&2; fail=1; }
    if [ "$fail" -ne 0 ]; then exit 2; fi
    echo "ci-changes self-test: OK"
}

case "${1:-}" in
    --self-test) self_test ;;
    "") classify ;;
    *) echo "usage: $0 [--self-test]  (reads changed paths on stdin)" >&2; exit 2 ;;
esac
