#!/usr/bin/env bash
# scripts/plan-version.sh — print or check the version PLAN.md says Cargo.toml should carry.
#
# A thin wrapper over `ta plan expected-version`, so CI (version-sync.yml,
# nightly.yml) and humans all use the same Rust code as `ta plan status`.
#
# Usage:
#   scripts/plan-version.sh                # print the expected version
#   scripts/plan-version.sh --check        # compare against Cargo.toml
#   scripts/plan-version.sh --check --json
#
# Exit codes (passed through from `ta plan expected-version`):
#   0  version computed; with --check, Cargo.toml matches
#   1  --check: Cargo.toml lags PLAN.md (a bump is needed)
#   2  no expected version could be computed, or a file was unreadable
#   3  --check: Cargo.toml carries a pinned non-alpha release version (not managed)
#   4  --check: Cargo.toml is ahead of PLAN.md (never lowered automatically)
#
# Binary: $TA_BIN if set (a prebuilt `ta`), otherwise `cargo run -p ta-cli`.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Never prompt for the Keychain from a script or CI run.
export TA_NO_KEYCHAIN=1

if [[ -n "${TA_BIN:-}" ]]; then
  CMD=("$TA_BIN" plan expected-version "$@")
else
  CMD=(cargo run --quiet -p ta-cli --bin ta -- plan expected-version "$@")
fi

echo "[plan-version] running: ${CMD[*]}" >&2
"${CMD[@]}"
rc=$?
if [[ $rc -ne 0 ]]; then
  echo "[plan-version] exit code $rc. Re-run manually: scripts/plan-version.sh $*" >&2
fi
exit $rc
