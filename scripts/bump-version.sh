#!/usr/bin/env bash
# scripts/bump-version.sh — atomically update version across all canonical locations.
#
# Updates:
#   Cargo.toml          [workspace.package] version
#   .release.toml       last_release_tag (set to previous tag before bump)
#   CLAUDE.md           "Current version" line
#
# Usage:
#   ./scripts/bump-version.sh 0.14.22-rc.5
#   ./scripts/bump-version.sh 0.14.23-alpha --last-tag public-alpha-v0.14.22.4
#   ./scripts/bump-version.sh 0.15.0-alpha  --title-suffix "Content Pipeline & Platform Integrations"
#   ./scripts/bump-version.sh 0.15.0        --stable        # marks prerelease=false
#
# After running, commit:
#   git add Cargo.toml .release.toml CLAUDE.md
#   git commit -m "chore: bump version to <new>"

set -euo pipefail

NEW_VERSION=""
LAST_TAG=""
TITLE_SUFFIX=""
STABLE=false

# Parse args
while [[ $# -gt 0 ]]; do
  case "$1" in
    --last-tag)    LAST_TAG="$2";      shift 2 ;;
    --title-suffix) TITLE_SUFFIX="$2"; shift 2 ;;
    --stable)      STABLE=true;        shift   ;;
    -*)            echo "Unknown flag: $1"; exit 1 ;;
    *)             NEW_VERSION="$1";   shift   ;;
  esac
done

if [[ -z "$NEW_VERSION" ]]; then
  echo "Usage: $0 <new-version> [--last-tag <tag>] [--title-suffix \"...\"] [--stable]"
  echo ""
  echo "Examples:"
  echo "  $0 0.14.22-rc.5"
  echo "  $0 0.14.23-alpha --last-tag public-alpha-v0.14.22.4"
  echo "  $0 0.15.0 --stable --title-suffix \"Content Pipeline\""
  exit 1
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CARGO_TOML="Cargo.toml"
RELEASE_TOML=".release.toml"
CLAUDE_MD="CLAUDE.md"

# --- Read current values ---
OLD_VERSION=$(grep '^version' "$CARGO_TOML" | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "Bumping: $OLD_VERSION → $NEW_VERSION"

# --- Cargo.toml ---
python3 - <<PYEOF
import re
with open("$CARGO_TOML") as f:
    content = f.read()
# Replace only the first top-level version = "..." (workspace version)
content = re.sub(r'^version = ".*"', 'version = "$NEW_VERSION"', content, count=1, flags=re.MULTILINE)
with open("$CARGO_TOML", "w") as f:
    f.write(content)
print("  Cargo.toml: version = \\"$NEW_VERSION\\"")
PYEOF

# --- .release.toml ---
# If --last-tag not supplied, derive from OLD_VERSION pattern (don't guess, just skip)
if [[ -z "$LAST_TAG" ]]; then
  EXISTING_LAST=$(grep '^last_release_tag' "$RELEASE_TOML" 2>/dev/null | head -1 | sed 's/.*= *"\(.*\)"/\1/' || true)
  echo "  .release.toml: last_release_tag unchanged ($EXISTING_LAST) — pass --last-tag to update"
else
  python3 - <<PYEOF
import re
with open("$RELEASE_TOML") as f:
    content = f.read()
content = re.sub(r'^last_release_tag = ".*"', 'last_release_tag = "$LAST_TAG"', content, flags=re.MULTILINE)
with open("$RELEASE_TOML", "w") as f:
    f.write(content)
print("  .release.toml: last_release_tag = \\"$LAST_TAG\\"")
PYEOF
fi

if [[ -n "$TITLE_SUFFIX" ]]; then
  python3 - <<PYEOF
import re
with open("$RELEASE_TOML") as f:
    content = f.read()
content = re.sub(r'^title_suffix = ".*"', 'title_suffix = "$TITLE_SUFFIX"', content, flags=re.MULTILINE)
with open("$RELEASE_TOML", "w") as f:
    f.write(content)
print("  .release.toml: title_suffix = \\"$TITLE_SUFFIX\\"")
PYEOF
fi

if $STABLE; then
  python3 - <<PYEOF
import re
with open("$RELEASE_TOML") as f:
    content = f.read()
content = re.sub(r'^prerelease = .*', 'prerelease = false', content, flags=re.MULTILINE)
with open("$RELEASE_TOML", "w") as f:
    f.write(content)
print("  .release.toml: prerelease = false")
PYEOF
else
  python3 - <<PYEOF
import re
with open("$RELEASE_TOML") as f:
    content = f.read()
content = re.sub(r'^prerelease = .*', 'prerelease = true', content, flags=re.MULTILINE)
with open("$RELEASE_TOML", "w") as f:
    f.write(content)
print("  .release.toml: prerelease = true")
PYEOF
fi

# --- CLAUDE.md ---
# Use a simple Python script written to a temp file to avoid heredoc quoting issues.
python3 -c "
import re, sys
new_ver = sys.argv[1]
with open('${CLAUDE_MD}') as f:
    content = f.read()
def replace_ver(m):
    return m.group(1) + new_ver + m.group(3)
content = re.sub(r'(\*\*Current version\*\*: \`)([^\`]+)(\`)', replace_ver, content, count=1)
with open('${CLAUDE_MD}', 'w') as f:
    f.write(content)
print('  CLAUDE.md: Current version =', new_ver)
" "$NEW_VERSION"

# --- Cargo.lock ---
# Regenerate Cargo.lock so the lockfile stays in sync with the bumped version.
# This prevents Cargo.lock from being left dirty after every version bump.
if command -v cargo &>/dev/null; then
  echo "  Regenerating Cargo.lock (cargo update --workspace)..."
  cargo update --workspace --quiet 2>/dev/null \
    && echo "  Cargo.lock: updated" \
    || echo "  Cargo.lock: could not regenerate (cargo not in PATH — run manually)"
else
  echo "  Cargo.lock: cargo not found — run 'cargo update --workspace' manually before committing"
fi

echo ""
echo "Done. Verify with:  grep -E 'version|Current version' Cargo.toml CLAUDE.md .release.toml"
echo ""
echo "Next:"
echo "  git add Cargo.toml .release.toml CLAUDE.md Cargo.lock"
echo "  git commit -m \"chore: bump version to $NEW_VERSION\""
echo "  git tag public-alpha-v<X.Y.Z.N>"
echo "  git push && git push origin <tag>"
echo "  gh workflow run release.yml --field tag=<tag> --field prerelease=true"
