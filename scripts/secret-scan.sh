#!/usr/bin/env bash
# secret-scan.sh: fast pattern-based scan for real-shaped secrets in tracked files.
#
# Usage:
#   scripts/secret-scan.sh                 scan git-tracked files in the repo
#   scripts/secret-scan.sh --root DIR      scan DIR (git-tracked files if DIR is a
#                                          git repo, otherwise every file under it)
#   scripts/secret-scan.sh --allowlist F   use allowlist file F
#   scripts/secret-scan.sh --self-test     prove the scanner catches a planted
#                                          secret and passes on clean input
#
# Exit status: 0 clean, 1 real-shaped secret found outside the allowlist,
# 2 usage or internal error.
#
# Allowlist (scripts/secret-scan.allowlist): one path glob per line, '#'
# comments, CRLF tolerated. '*' matches across '/', so 'dir/*' covers a tree.
# Every entry should carry a comment saying why it is allowed.
#
# Works on macOS bash 3.2 and Linux. No dependencies beyond git, grep, find.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ALLOWLIST="$SCRIPT_DIR/secret-scan.allowlist"
SELF_TEST=0

# name|extended regex. Shapes are deliberately specific (length and charset of
# real credentials) so placeholders such as "sk-ant-..." or "ghp_xxx" do not trip.
PATTERNS='Stripe-style live/test key|sk[-_](live|test)[-_][A-Za-z0-9_-]{8,}
Anthropic API key|sk-ant-[A-Za-z0-9_-]{20,}
OpenAI project key|sk-proj-[A-Za-z0-9_-]{20,}
GitHub personal access token|gh[pousr]_[A-Za-z0-9]{30,}
GitHub fine-grained token|github_pat_[A-Za-z0-9_]{22,}
Slack token|xox[abprs]-[0-9]{9,}[0-9A-Za-z-]*
AWS access key id|(AKIA|ASIA)[0-9A-Z]{16}
Google API key|AIza[0-9A-Za-z_-]{35}
Private key block|-----BEGIN ([A-Z]+ )*PRIVATE KEY-----'

usage() {
  sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --self-test) SELF_TEST=1 ;;
    --root) shift; ROOT="${1:-}" ;;
    --allowlist) shift; ALLOWLIST="${1:-}" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "secret-scan: unknown argument '$1'" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

# is_allowed PATH ALLOWLIST_PATTERNS_FILE: 0 if PATH matches a glob in the file.
is_allowed() {
  local p="$1" glob
  while IFS= read -r glob || [ -n "$glob" ]; do
    glob="${glob%$'\r'}"
    case "$glob" in ''|'#'*) continue ;; esac
    glob="${glob%%[[:space:]]#*}"   # trailing inline comment
    glob="${glob%"${glob##*[![:space:]]}"}"
    [ -z "$glob" ] && continue
    glob="${glob//\*\*/*}"
    # shellcheck disable=SC2254
    case "$p" in $glob) return 0 ;; esac
  done < "$2"
  return 1
}

# scan_tree ROOT ALLOWLIST: prints violations, returns 1 if any.
scan_tree() {
  local root="$1" allow="$2" tmp list top rroot found=0 npat=0 nfiles=0 name re
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/secret-scan.XXXXXX")" || return 2
  list="$tmp/files"
  : > "$tmp/allow"
  [ -f "$allow" ] && tr -d '\r' < "$allow" > "$tmp/allow"

  echo "[secret-scan] step 1/3: listing files under $root"
  top="$(git -C "$root" rev-parse --show-toplevel 2>/dev/null || true)"
  rroot="$(cd "$root" && pwd -P)"
  if [ -n "$top" ] && [ "$(cd "$top" && pwd -P)" = "$rroot" ]; then
    # Tracked plus untracked-but-not-ignored, so new files are scanned pre-commit.
    git -C "$root" ls-files --cached --others --exclude-standard | tr -d '\r' > "$tmp/all"
    echo "[secret-scan]   source: git ls-files (tracked and untracked, ignores honored)"
  else
    (cd "$root" && find . -type f -not -path './.git/*' | sed 's|^\./||') > "$tmp/all"
    echo "[secret-scan]   source: find (not a git work tree)"
  fi
  : > "$list"
  while IFS= read -r f || [ -n "$f" ]; do
    [ -f "$root/$f" ] || continue
    if is_allowed "$f" "$tmp/allow"; then continue; fi
    printf '%s\0' "$f" >> "$list"
    nfiles=$((nfiles + 1))
  done < "$tmp/all"
  echo "[secret-scan] step 2/3: scanning $nfiles files (allowlist: $allow)"

  : > "$tmp/hits"
  while IFS='|' read -r name re; do
    [ -z "$name" ] && continue
    npat=$((npat + 1))
    # grep -I skips binary files; tr strips CR so $-anchored patterns would
    # still behave, though none of ours are anchored.
    (cd "$root" && xargs -0 grep -I -n -H -E -e "$re" < "$list" 2>/dev/null) \
      | while IFS= read -r hit; do
          loc="${hit%%:*}"; rest="${hit#*:}"; line="${rest%%:*}"
          printf '%s:%s: %s\n' "$loc" "$line" "$name" >> "$tmp/hits"
        done
  done <<EOP
$PATTERNS
EOP
  # The while-pipeline above runs in a subshell, so count patterns again here.
  npat=$(printf '%s\n' "$PATTERNS" | grep -c .)
  echo "[secret-scan] step 3/3: $npat patterns scanned"

  if [ -s "$tmp/hits" ]; then
    found=1
    echo ""
    echo "ERROR: real-shaped secret(s) found outside the allowlist:"
    sort -u "$tmp/hits" | sed 's/^/  /'
    echo ""
    echo "Fix: replace the literal with a plain sentinel (for example"
    echo "  FAKE_API_KEY_FOR_TESTS), or build the shaped string at runtime in tests"
    echo "  (format!(\"{}{}\", \"prefix\", \"body\")). If it is a documented placeholder"
    echo "  that must stay, add its path glob plus a comment to:"
    echo "  $allow"
    echo "Re-run: $0"
  else
    echo "OK: no real-shaped secrets found ($nfiles files, $npat patterns)"
  fi
  rm -rf "$tmp"
  return $found
}

self_test() {
  local d rc
  d="$(mktemp -d "${TMPDIR:-/tmp}/secret-scan-selftest.XXXXXX")" || exit 2
  echo "[secret-scan] self-test in $d"
  mkdir -p "$d/src" "$d/fixtures"
  : > "$d/empty.allow"
  # Clean input, including placeholder-style text that must not trip.
  printf 'export ANTHROPIC_API_KEY=sk-ant-...\r\nTOKEN=ghp_xxx\r\n' > "$d/src/clean.txt"
  # Planted secrets assembled at runtime so this file holds no literal.
  printf 'key = "%s%s"\r\n' "sk-ant-" "api03-AAAAAAAAAAAAAAAAAAAAAAAA" > "$d/src/planted.txt"
  printf 'k=%s%s\n' "AKIA" "ABCDEFGHIJKLMNOP" > "$d/fixtures/allowed.txt"

  scan_tree "$d" "$d/empty.allow" > "$d/out1.log" 2>&1; rc=$?
  if [ $rc -ne 1 ] || ! grep -q 'src/planted.txt:1:' "$d/out1.log"; then
    echo "SELF-TEST FAILED: planted secret was not reported (rc=$rc)"; cat "$d/out1.log"; rm -rf "$d"; exit 1
  fi
  echo "  ok: planted secret detected with file:line"

  printf '# fixtures hold shaped secrets on purpose\r\nfixtures/*\r\nsrc/planted.txt\r\n' > "$d/crlf.allow"
  scan_tree "$d" "$d/crlf.allow" > "$d/out2.log" 2>&1; rc=$?
  if [ $rc -ne 0 ]; then
    echo "SELF-TEST FAILED: allowlisted paths (CRLF file) still reported (rc=$rc)"; cat "$d/out2.log"; rm -rf "$d"; exit 1
  fi
  echo "  ok: allowlist globs honored, CRLF allowlist tolerated"

  rm -f "$d/src/planted.txt"
  scan_tree "$d" "$d/empty.allow" > "$d/out3.log" 2>&1; rc=$?
  if [ $rc -ne 1 ]; then
    echo "SELF-TEST FAILED: non-allowlisted fixture not reported (rc=$rc)"; cat "$d/out3.log"; rm -rf "$d"; exit 1
  fi
  rm -f "$d/fixtures/allowed.txt"
  scan_tree "$d" "$d/empty.allow" > "$d/out4.log" 2>&1; rc=$?
  if [ $rc -ne 0 ]; then
    echo "SELF-TEST FAILED: clean input reported (rc=$rc)"; cat "$d/out4.log"; rm -rf "$d"; exit 1
  fi
  echo "  ok: clean input passes (placeholders and CRLF lines ignored)"
  rm -rf "$d"
  echo "SELF-TEST PASSED"
}

if [ "$SELF_TEST" -eq 1 ]; then
  self_test
  exit 0
fi

scan_tree "$ROOT" "$ALLOWLIST"
exit $?
