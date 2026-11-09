#!/usr/bin/env bash
# Fail when PLAN.md writes a phase status marker anywhere except on a line of
# its own. A phase's status is the comment line `<!-- status: X -->`. Prose
# that quotes the comment (in a goal, an item, an example) looks like a phase
# status to every text search (awk, grep, a quick script) and makes finished
# phases appear pending. Real tooling (`ta plan list`) is not fooled, but people
# and scripts were, repeatedly. So the rule is: that comment appears only as a
# bare marker line. Describe a marker in words ("the `done` status marker").
#
#   scripts/check-plan-markers.sh [PLAN.md]   check a file (default ./PLAN.md)
#   scripts/check-plan-markers.sh --self-test
# CI and scripts/ci-local.sh run this same script.
set -euo pipefail

check_file() {
  local file="$1" bad
  if [ ! -f "$file" ]; then
    echo "check-plan-markers: $file not found. Run from the repo root or pass the PLAN.md path."
    return 2
  fi
  # Lines holding the marker opener that are not exactly one bare marker.
  bad=$(grep -n '<!-- status:' "$file" | grep -Ev '^[0-9]+:<!-- status: [a-z_]+ -->$' || true)
  if [ -n "$bad" ]; then
    echo "check-plan-markers: $file quotes a status marker inside other text:"
    echo "$bad" | cut -c1-160 | sed 's/^/  line /'
    echo "  Why: a search for pending phases would count these lines as phases."
    echo "  Fix: reword each line to describe the marker in words, for example"
    echo "  'the \`done\` status marker'. Only a line holding just the marker may contain it."
    return 1
  fi
  echo "check-plan-markers: $file ok (every status marker is on its own line)."
}

self_test() {
  local d rc
  d=$(mktemp -d)
  trap 'rm -rf "$d"' RETURN
  printf '### v1.0 - A\n<!-- status: done -->\n\ntext\n' >"$d/ok.md"
  check_file "$d/ok.md" >/dev/null || { echo "self-test FAILED: a bare marker was refused"; return 1; }
  printf '### v1.0 - A\n<!-- status: done -->\nAfter <!-- status: pending --> phases.\n' >"$d/prose.md"
  rc=0; check_file "$d/prose.md" >/dev/null || rc=$?
  [ "$rc" = 1 ] || { echo "self-test FAILED: prose quoting a marker was accepted (rc=$rc)"; return 1; }
  printf '### v1.0 - A <!-- status: pending -->\n' >"$d/heading.md"
  rc=0; check_file "$d/heading.md" >/dev/null || rc=$?
  [ "$rc" = 1 ] || { echo "self-test FAILED: a marker on a heading line was accepted (rc=$rc)"; return 1; }
  rc=0; check_file "$d/missing.md" >/dev/null || rc=$?
  [ "$rc" = 2 ] || { echo "self-test FAILED: a missing file was not reported (rc=$rc)"; return 1; }
  echo "check-plan-markers self-test: ok"
}

case "${1:-}" in
  --self-test) self_test ;;
  *) check_file "${1:-PLAN.md}" ;;
esac
