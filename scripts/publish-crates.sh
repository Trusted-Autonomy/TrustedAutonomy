#!/usr/bin/env bash
# scripts/publish-crates.sh — publish all workspace crates to crates.io in dependency order.
#
# Publishes leaf crates first, then up the dependency tree to ta-cli.
# Each crate is published only if its current version is not already on crates.io (idempotent).
#
# Requirements:
#   - CARGO_REGISTRY_TOKEN must be set (crates.io API token with publish rights)
#   - All crates must be at the same version (single workspace version)
#   - Run from the workspace root
#
# Usage:
#   CARGO_REGISTRY_TOKEN=<token> ./scripts/publish-crates.sh
#   CARGO_REGISTRY_TOKEN=<token> ./scripts/publish-crates.sh --dry-run
#
# Options:
#   --dry-run     Run `cargo publish --dry-run` for each crate (no actual publish)
#   --skip-check  Skip the crates.io version check (always attempt publish)
#
# Publish order (dependency tiers, leaf crates first):
#   Tier 1: no internal deps
#   Tier 2: depends on tier 1 only
#   Tier 3: depends on tiers 1-2
#   Tier 4: depends on tiers 1-3
#   Tier 5: depends on tiers 1-4 (ta-daemon)
#   Tier 6: ta-cli (depends on everything)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DRY_RUN=false
SKIP_CHECK=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)    DRY_RUN=true;    shift ;;
    --skip-check) SKIP_CHECK=true; shift ;;
    *) echo "Unknown flag: $1"; exit 1 ;;
  esac
done

if [[ -z "${CARGO_REGISTRY_TOKEN:-}" ]]; then
  echo "ERROR: CARGO_REGISTRY_TOKEN is not set."
  echo "  Set it to a crates.io API token with publish rights for the ta-* namespace."
  echo "  Generate one at https://crates.io/settings/tokens"
  exit 1
fi

# Read workspace version from Cargo.toml
WORKSPACE_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "Workspace version: $WORKSPACE_VERSION"
echo ""

# Check if a crate at a specific version is already published on crates.io.
# Returns 0 (true/already published) or 1 (false/not published).
is_published() {
  local crate_name="$1"
  local version="$2"
  local result
  result=$(curl -sf "https://crates.io/api/v1/crates/${crate_name}/${version}" 2>/dev/null || true)
  if echo "$result" | grep -q '"num"'; then
    return 0  # already published
  fi
  return 1  # not published
}

# Publish a single crate. Skips if already on crates.io (idempotent).
publish_crate() {
  local crate_name="$1"
  local crate_path="$2"

  echo "──────────────────────────────────────────"
  echo "Crate: $crate_name  (path: $crate_path)"

  if [[ "$SKIP_CHECK" == "false" ]] && is_published "$crate_name" "$WORKSPACE_VERSION"; then
    echo "  ✓ Already published at $WORKSPACE_VERSION — skipping"
    return 0
  fi

  if [[ "$DRY_RUN" == "true" ]]; then
    echo "  [dry-run] cargo publish -p $crate_name --dry-run"
    if cargo publish -p "$crate_name" --dry-run --token "$CARGO_REGISTRY_TOKEN" 2>&1; then
      echo "  ✓ Dry run passed"
    else
      echo "  ✗ Dry run FAILED for $crate_name"
      return 1
    fi
  else
    echo "  Publishing $crate_name@$WORKSPACE_VERSION to crates.io..."
    PUBLISH_OUTPUT=$(cargo publish -p "$crate_name" --token "$CARGO_REGISTRY_TOKEN" 2>&1)
    PUBLISH_EXIT=$?
    echo "$PUBLISH_OUTPUT"
    if [ $PUBLISH_EXIT -eq 0 ]; then
      echo "  ✓ Published $crate_name@$WORKSPACE_VERSION"
      # crates.io needs a moment between publishes to index the crate.
      # Without this delay, dependent crates fail to resolve the just-published version.
      echo "  Waiting 20s for crates.io index propagation..."
      sleep 20
    elif echo "$PUBLISH_OUTPUT" | grep -q "already exists"; then
      # crates.io API check can lag behind the index — if cargo itself reports
      # "already exists", treat it as a successful skip rather than a failure.
      echo "  ✓ Already published at $WORKSPACE_VERSION (confirmed by cargo) — skipping"
    else
      echo "  ✗ FAILED to publish $crate_name"
      return 1
    fi
  fi
}

echo "Publishing ${WORKSPACE_VERSION} to crates.io in dependency order..."
echo ""

# ── Tier 1: leaf crates (no internal workspace deps) ──────────────────────────
publish_crate "ta-audit"             "crates/ta-audit"
publish_crate "ta-output-schema"     "crates/ta-output-schema"
publish_crate "ta-events"            "crates/ta-events"
publish_crate "ta-credentials"       "crates/ta-credentials"
publish_crate "ta-changeset"         "crates/ta-changeset"
publish_crate "ta-goal"              "crates/ta-goal"
publish_crate "ta-actions"           "crates/ta-actions"
publish_crate "ta-build"             "crates/ta-build"
publish_crate "ta-db-overlay"        "crates/ta-db-overlay"
publish_crate "ta-extension"         "crates/ta-extension"
publish_crate "ta-agent-ollama"      "crates/ta-agent-ollama"
publish_crate "ta-memory"            "crates/ta-memory"
publish_crate "ta-policy"            "crates/ta-policy"
publish_crate "ta-connector-web"     "crates/ta-connectors/web"
publish_crate "ta-connector-mock-drive"  "crates/ta-connectors/mock-drive"
publish_crate "ta-connector-mock-gmail" "crates/ta-connectors/mock-gmail"
publish_crate "ta-connector-unity"   "crates/ta-connectors/unity"

# ── Tier 2: depends on tier 1 ─────────────────────────────────────────────────
publish_crate "ta-workspace"         "crates/ta-workspace"
publish_crate "ta-sandbox"           "crates/ta-sandbox"
publish_crate "ta-runtime"           "crates/ta-runtime"
publish_crate "ta-db-proxy"          "crates/ta-db-proxy"
publish_crate "ta-session"           "crates/ta-session"
publish_crate "ta-workflow"          "crates/ta-workflow"
publish_crate "ta-connector-slack"   "crates/ta-connectors/slack"
publish_crate "ta-connector-discord" "crates/ta-connectors/discord"
publish_crate "ta-connector-email"   "crates/ta-connectors/email"
publish_crate "ta-connector-unreal"  "crates/ta-connectors/unreal"
publish_crate "ta-connector-comfyui" "crates/ta-connectors/comfyui"

# ── Tier 3: depends on tiers 1-2 ──────────────────────────────────────────────
publish_crate "ta-db-proxy-sqlite"   "crates/ta-db-proxy-sqlite"
publish_crate "ta-connector-fs"      "crates/ta-connectors/fs"
publish_crate "ta-submit"            "crates/ta-submit"
publish_crate "ta-mediation"         "crates/ta-mediation"

# ── Tier 4: depends on tiers 1-3 ──────────────────────────────────────────────
publish_crate "ta-mcp-gateway"       "crates/ta-mcp-gateway"

# ── Tier 5: depends on tiers 1-4 ──────────────────────────────────────────────
publish_crate "ta-daemon"            "crates/ta-daemon"

# ── Tier 6: ta-cli (depends on everything) ────────────────────────────────────
publish_crate "ta-cli"               "apps/ta-cli"

echo ""
echo "──────────────────────────────────────────"
if [[ "$DRY_RUN" == "true" ]]; then
  echo "Dry run complete. All crates passed publishability checks."
  echo "Run without --dry-run to publish for real."
else
  echo "All crates published successfully."
  echo "  cargo install ta-cli  # should now work in a clean environment"
fi
