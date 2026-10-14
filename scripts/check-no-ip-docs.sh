#!/usr/bin/env bash
# check-no-ip-docs.sh: constitution rule `no-ip-docs-in-public-repos`.
#
# TrustedAutonomy is a PUBLIC repository. Invention disclosures, patent drafts,
# attorney work product and other IP-sensitive documents must live in the
# private Wayfinder repository, never here. Once pushed to a public repo a file
# is in every clone, tag archive and release tarball, and rewriting history does
# not recall those copies.
#
# This is a TRIPWIRE, not a guarantee: no script can recognise an IP document in
# general. It blocks the two signals that identified the one that leaked:
#   1. a path under an IP directory, or an IDD-* / invention-disclosure file name
#   2. a content marker such as "IP-SENSITIVE" or "DO NOT DISTRIBUTE EXTERNALLY"
#
# Usage:
#   check-no-ip-docs.sh              check files added or modified relative to the merge base
#   check-no-ip-docs.sh --all        check every tracked file
#   check-no-ip-docs.sh --rev <rev>  check the files added or changed by one commit
#
# Exit 0: clean. Exit 1: a violation (prints each file, why, and what to do).
# Exit 2: usage or git error (never silently passes).

set -u

PATH_PATTERN='(^|/)(IP|ip-docs|invention-disclosures?)/|(^|/)IDD-[^/]*$|invention[-_ ]disclosure'
CONTENT_PATTERN='IP-SENSITIVE|DO NOT DISTRIBUTE EXTERNALLY|ATTORNEY WORK PRODUCT|DISCLOSE PUBLICLY BEFORE A FILING'
# Files that legitimately NAME the markers (this script, the rule text, its test).
ALLOWLIST='^(scripts/check-no-ip-docs\.sh|\.ta/constitution\.toml)$'

mode="diff"
rev=""
case "${1:-}" in
  "") ;;
  --all) mode="all" ;;
  --rev)
    mode="rev"
    rev="${2:-}"
    if [ -z "$rev" ]; then echo "check-no-ip-docs: --rev needs a revision" >&2; exit 2; fi
    ;;
  *) echo "usage: check-no-ip-docs.sh [--all | --rev <rev>]" >&2; exit 2 ;;
esac

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-no-ip-docs: not inside a git repository (cwd: $(pwd))" >&2
  exit 2
fi

list_files() {
  case "$mode" in
    all) git ls-files ;;
    rev) git diff-tree --root --no-commit-id --name-only --diff-filter=AMR -r "$rev" ;;
    diff)
      base=$(git merge-base HEAD origin/main 2>/dev/null || git rev-parse HEAD~1 2>/dev/null || true)
      {
        if [ -n "$base" ]; then git diff --name-only --diff-filter=AMR "$base" HEAD; fi
        git diff --name-only --diff-filter=AMR HEAD
        git ls-files --others --exclude-standard
      } | sort -u
      ;;
  esac
}

read_content() {
  # Print the file's content as git or the working tree has it. Binary files are skipped.
  local f="$1"
  if [ "$mode" = "rev" ]; then
    git show "$rev:$f" 2>/dev/null
  elif [ -f "$f" ]; then
    cat "$f"
  fi
}

violations=0
files=$(list_files) || { echo "check-no-ip-docs: could not list files (mode=$mode)" >&2; exit 2; }

while IFS= read -r f; do
  [ -z "$f" ] && continue
  if printf '%s\n' "$f" | grep -Eq "$ALLOWLIST"; then continue; fi
  reason=""
  if printf '%s\n' "$f" | grep -Eiq "$PATH_PATTERN"; then
    reason="path looks like an IP document"
  else
    case "$f" in
      *.md|*.txt|*.rst|*.adoc|*.html|*.tex|*.docx|*.pdf) ;;  # only scan text-like docs for markers
      *) continue ;;
    esac
    if read_content "$f" | LC_ALL=C grep -aEq "$CONTENT_PATTERN"; then
      reason="content carries an IP confidentiality marker"
    fi
  fi
  if [ -n "$reason" ]; then
    echo "ERROR: $f -- $reason" >&2
    violations=$((violations + 1))
  fi
done <<EOF
$files
EOF

if [ "$violations" -gt 0 ]; then
  cat >&2 <<MSG

$violations file(s) look like IP-sensitive documents. This repository is PUBLIC.
Constitution rule: no-ip-docs-in-public-repos.
What to do:
  1. Do not commit or push them. Move them to the private Wayfinder repository.
  2. If a file is already staged: git rm --cached <file>
  3. If it was already pushed, tell the owner immediately: removing it from history
     does not recall copies, and counsel may need to assess the disclosure.
  4. If this is a false positive, change the wording of the document, or ask the owner
     to adjust the patterns in scripts/check-no-ip-docs.sh.
MSG
  exit 1
fi

echo "check-no-ip-docs: ok (mode=$mode, $(printf '%s\n' "$files" | grep -c .) file(s) checked)"
exit 0
