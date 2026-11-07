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
#   --list-order  Print the publish order (one crate per line) and exit. Needs
#                 no token and no network; used by scripts/test-publish-order.sh.
#
# Publish order: COMPUTED, not hand-written. `cargo metadata` gives every
# workspace crate and its internal dependencies; crates are published in
# topological order (a crate only after every workspace crate it depends on
# through [dependencies] or [build-dependencies]; dev-dependencies are ignored
# because `cargo publish` drops path-only dev-dependencies). Ties break by
# name, so the order is stable. A hand-kept list went stale (it listed a crate
# that no longer exists, omitted a dozen that do, and published ta-audit
# before ta-credentials, which it depends on), so adding or moving a crate
# needs no edit here. A dependency cycle or an unversioned path dependency is
# an error that names the crates involved.
#
# Requires python3 (present on ubuntu runners and macOS).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DRY_RUN=false
SKIP_CHECK=false
LIST_ORDER=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)    DRY_RUN=true;    shift ;;
    --skip-check) SKIP_CHECK=true; shift ;;
    --list-order) LIST_ORDER=true; shift ;;
    *) echo "Unknown flag: $1"; exit 1 ;;
  esac
done

compute_order() {
  # Prints "<crate>\t<manifest dir relative to the repo root>" per line, in publish order.
  cargo metadata --no-deps --format-version 1 | python3 -c '
import json, os, sys
m = json.load(sys.stdin)
root = m["workspace_root"]
pkgs = {p["name"]: p for p in m["packages"] if p.get("publish") != []}
deps, errors = {}, []
for name, p in pkgs.items():
    deps[name] = set()
    for d in p["dependencies"]:
        if d["name"] not in pkgs or d["kind"] == "dev":
            continue
        deps[name].add(d["name"])
        if d["req"] == "*":
            errors.append("%s depends on %s by path without a version; crates.io rejects that. Add version = \"<workspace version>\" to the dependency." % (name, d["name"]))
order, done = [], set()
while len(done) < len(pkgs):
    ready = sorted(n for n in pkgs if n not in done and deps[n] <= done)
    if not ready:
        left = sorted(n for n in pkgs if n not in done)
        errors.append("dependency cycle among: " + ", ".join(left) + ". Break it (a path-only dev-dependency is fine) before publishing.")
        break
    for n in ready:
        order.append(n)
        done.add(n)
if errors:
    for e in errors:
        sys.stderr.write("publish order ERROR: " + e + "\n")
    sys.exit(1)
for n in order:
    print("%s\t%s" % (n, os.path.relpath(os.path.dirname(pkgs[n]["manifest_path"]), root)))
'
}

if [[ "$LIST_ORDER" == "true" ]]; then
  compute_order | cut -f1
  exit 0
fi

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
  result=$(curl -sf -A "ta-publish-crates (https://github.com/Trusted-Autonomy/TrustedAutonomy)" "https://crates.io/api/v1/crates/${crate_name}/${version}" 2>/dev/null || true)
  if echo "$result" | grep -q '"num"'; then
    return 0  # already published
  fi
  return 1  # not published
}

# Preflight: refuse to start if a workspace crate depends on a crate that is not a
# workspace member and lives only in git. crates.io cannot resolve a git source, so
# without this the run would publish half the workspace and then fail. The
# dependency must be published to crates.io first, and the dependency
# declaration must carry `version = "..."` next to `git = "..."` (cargo uses the
# git source locally and the version when publishing).
preflight_external_deps() {
  local missing=""
  local line name req users
  while IFS=$'\t' read -r name req users; do
    [[ -z "$name" ]] && continue
    if [[ "$req" == "*" ]]; then
      echo "ERROR: $users depend on '$name' from git without a version. Add version = \"<its crates.io version>\" next to git = \"...\"." >&2
      missing="yes"
      continue
    fi
    local code
    code=$(curl -s -o /dev/null -w '%{http_code}' -A "ta-publish-crates (https://github.com/Trusted-Autonomy/TrustedAutonomy)" "https://crates.io/api/v1/crates/${name}/" || true)
    if [[ "$code" != "200" ]]; then
      echo "ERROR: '$name' (used by: $users) is a git dependency and is not on crates.io (HTTP $code). Publish '$name' to crates.io first, then re-run; nothing was published." >&2
      missing="yes"
    fi
  done < <(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
m = json.load(sys.stdin)
ws = {p["name"] for p in m["packages"]}
ext = {}
for p in m["packages"]:
    for d in p["dependencies"]:
        if d["name"] in ws or not (d.get("source") or "").startswith("git+"):
            continue
        e = ext.setdefault(d["name"], {"req": d["req"], "users": set()})
        e["users"].add(p["name"])
for n, e in sorted(ext.items()):
    print("%s\t%s\t%s" % (n, e["req"], ",".join(sorted(e["users"]))))
')
  if [[ -n "$missing" ]]; then
    return 1
  fi
}

preflight_external_deps || exit 1

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
    # Retry loop: handles 429 rate-limit by sleeping until the Retry-After time.
    # crates.io allows ~1 new crate per 10 minutes when publishing many new crates.
    while true; do
      # Use || to prevent set -e from aborting on non-zero exit before we can
      # inspect the output (e.g. "already exists" / 429 should not be hard failures).
      PUBLISH_EXIT=0
      PUBLISH_OUTPUT=$(cargo publish -p "$crate_name" --token "$CARGO_REGISTRY_TOKEN" 2>&1) || PUBLISH_EXIT=$?
      echo "$PUBLISH_OUTPUT"
      if [ $PUBLISH_EXIT -eq 0 ]; then
        echo "  ✓ Published $crate_name@$WORKSPACE_VERSION"
        # crates.io needs a moment between publishes to index the crate.
        # Without this delay, dependent crates fail to resolve the just-published version.
        echo "  Waiting 20s for crates.io index propagation..."
        sleep 20
        break
      elif echo "$PUBLISH_OUTPUT" | grep -q "already exists"; then
        # crates.io API check can lag behind the index — if cargo itself reports
        # "already exists", treat it as a successful skip rather than a failure.
        echo "  ✓ Already published at $WORKSPACE_VERSION (confirmed by cargo) — skipping"
        break
      elif echo "$PUBLISH_OUTPUT" | grep -q "429 Too Many Requests"; then
        # Rate limited. Parse the Retry-After timestamp and sleep exactly that long.
        RETRY_AFTER=$(echo "$PUBLISH_OUTPUT" | grep -oP 'after \K[^"]+(?= GMT)' | head -1 || true)
        if [[ -n "$RETRY_AFTER" ]]; then
          RETRY_EPOCH=$(date -d "$RETRY_AFTER GMT" +%s 2>/dev/null \
            || date -j -f "%a, %d %b %Y %T" "$RETRY_AFTER" +%s 2>/dev/null \
            || echo "")
          NOW_EPOCH=$(date +%s)
          if [[ -n "$RETRY_EPOCH" && "$RETRY_EPOCH" -gt "$NOW_EPOCH" ]]; then
            WAIT=$(( RETRY_EPOCH - NOW_EPOCH + 10 ))
            echo "  Rate limited. Retry after: $RETRY_AFTER GMT — sleeping ${WAIT}s ..."
            sleep "$WAIT"
          else
            echo "  Rate limited (could not parse retry time) — sleeping 615s ..."
            sleep 615
          fi
        else
          echo "  Rate limited (no retry-after found) — sleeping 615s ..."
          sleep 615
        fi
        echo "  Retrying $crate_name ..."
        # loop continues
      else
        echo "  ✗ FAILED to publish $crate_name"
        return 1
      fi
    done
  fi
}

ORDER_FILE="$(mktemp)"
trap 'rm -f "$ORDER_FILE"' EXIT
if ! compute_order > "$ORDER_FILE"; then
  echo "ERROR: could not compute the publish order from 'cargo metadata' (see above). Nothing was published."
  exit 1
fi
CRATE_COUNT=$(wc -l < "$ORDER_FILE" | tr -d ' ')

echo "Publishing ${WORKSPACE_VERSION} to crates.io in dependency order (${CRATE_COUNT} crates)..."
echo ""

# The order file is read on fd 3 so a child process (cargo) cannot consume it as stdin.
while IFS=$'\t' read -r -u 3 crate_name crate_path; do
  publish_crate "$crate_name" "$crate_path"
done 3< "$ORDER_FILE"

echo ""
echo "──────────────────────────────────────────"
if [[ "$DRY_RUN" == "true" ]]; then
  echo "Dry run complete. All crates passed publishability checks."
  echo "Run without --dry-run to publish for real."
else
  echo "All crates published successfully."
  echo "  cargo install ta-cli  # should now work in a clean environment"
fi
