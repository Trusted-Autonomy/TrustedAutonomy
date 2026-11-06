#!/usr/bin/env bash
# Fail when the version sources disagree. CI and scripts/ci-local.sh run this
# same script.
#   1. CLAUDE.md "Current version" must echo Cargo.toml [workspace.package]
#      (Cargo.toml is the single source of truth; the agent-context copy in
#      CLAUDE.md must not drift).
#   2. Any crate manifest disagreeing with the workspace version, i.e. the
#      condition under which cargo reports "failed to select a version for the
#      requirement `ta-x = ^...`". Mirrors verify_version_consistency in
#      apps/ta-cli/src/commands/draft_version.rs.
set -euo pipefail

root="${1:-.}"
ws=$(grep -m1 '^version = "' "$root/Cargo.toml" | sed 's/version = "\(.*\)"/\1/')
echo "Workspace version: $ws"

bad=0

if [ -f "$root/CLAUDE.md" ]; then
  claude=$(grep -m1 'Current version' "$root/CLAUDE.md" | sed "s/.*\`\(.*\)\`.*/\1/" || true)
  echo "CLAUDE.md version: $claude"
  if [ "$claude" != "$ws" ]; then
    echo "MISMATCH CLAUDE.md: says $claude, Cargo.toml says $ws"
    echo "  Fix: run ./scripts/bump-version.sh $ws or update CLAUDE.md 'Current version' to match Cargo.toml."
    bad=1
  fi
fi

while IFS= read -r manifest; do
  rel="${manifest#"$root"/}"
  # Literal [package] version (version.workspace = true does not match).
  pkg=$(grep -m1 -E '^version[[:space:]]*=[[:space:]]*"' "$manifest" | sed 's/.*"\(.*\)".*/\1/' || true)
  if [ -n "$pkg" ] && [ "$pkg" != "$ws" ]; then
    echo "MISMATCH $rel: declares version $pkg"
    bad=1
  fi
  # ta-* dependency version requirements.
  while IFS= read -r line; do
    name=$(echo "$line" | sed -E 's/^[[:space:]]*(ta-[a-z0-9-]+).*/\1/')
    req=$(echo "$line" | sed -E 's/.*version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')
    plain="${req#[\^=~]}"
    if [ "$plain" != "$ws" ]; then
      echo "MISMATCH $rel: requires $name $req"
      bad=1
    fi
  done < <(grep -E '^[[:space:]]*ta-[a-z0-9-]+[[:space:]]*=.*version[[:space:]]*=[[:space:]]*"' "$manifest" || true)
done < <(find "$root/apps" "$root/crates" -name Cargo.toml -not -path '*/target/*' | sort)

if [ "$bad" -ne 0 ]; then
  echo ""
  echo "ERROR: version sources disagree with workspace version $ws, so cargo or agent context would be wrong."
  echo "Fix: run ./scripts/bump-version.sh $ws to align every manifest and CLAUDE.md."
  exit 1
fi
echo "OK: CLAUDE.md and every manifest agree with workspace version $ws"
