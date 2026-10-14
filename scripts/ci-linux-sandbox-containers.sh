#!/usr/bin/env bash
# Linux sandbox conformance in degraded environments (v0.17.12.8).
#
# Runs `cargo test -p ta-runtime --test linux_conformance` inside containers
# that take away one layer at a time, and checks TA reports each loss instead of
# running the agent with less isolation than it claims.
#
#   default   Docker's default seccomp profile and no extra privileges: bubblewrap
#             cannot create a user namespace, Landlock still works. Expects the
#             Landlock-only path.
#   no-landlock  A seccomp profile that answers ENOSYS to the landlock_* calls
#             (an old kernel): TA must refuse to start the agent and say why.
#
# Usage: scripts/ci-linux-sandbox-containers.sh [default|no-landlock|all]
# Needs Docker. On failure, each step prints the exact `docker run` to repeat.
set -euo pipefail

IMAGE="${TA_CI_RUST_IMAGE:-rust:1-bookworm}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

run() {
  local name="$1"; shift
  echo "::group::linux sandbox conformance: $name"
  echo "[ci] image: $IMAGE; repeat with: docker run --rm $* -v $ROOT:/src:ro -w /src $IMAGE cargo test -p ta-runtime --test linux_conformance"
  if ! docker run --rm "$@" -v "$ROOT":/src:ro -v "$WORK/target:/tgt" -e CARGO_TARGET_DIR=/tgt \
      -e TA_REQUIRE_SANDBOX_CONFORMANCE=1 -w /src "$IMAGE" \
      cargo test -p ta-runtime --test linux_conformance -- --test-threads=1; then
    echo "::endgroup::"
    echo "[ci] FAILED: $name. Re-run: scripts/ci-linux-sandbox-containers.sh $name"
    exit 1
  fi
  echo "::endgroup::"
}

default_case() {
  # No bwrap in the image and no userns: expect the Landlock-only path.
  run default -e TA_EXPECT_NO_BWRAP=1
}

no_landlock_case() {
  # Docker's default profile plus ENOSYS for the Landlock syscalls (444-446).
  python3 - "$WORK/seccomp-no-landlock.json" <<'PY'
import json, sys
profile = {
    "defaultAction": "SCMP_ACT_ALLOW",
    "syscalls": [{
        "names": ["landlock_create_ruleset", "landlock_add_rule", "landlock_restrict_self"],
        "action": "SCMP_ACT_ERRNO",
        "errnoRet": 38,
    }],
}
json.dump(profile, open(sys.argv[1], "w"))
PY
  run no-landlock --security-opt "seccomp=$WORK/seccomp-no-landlock.json" \
    -e TA_EXPECT_NO_BWRAP=1 -e TA_EXPECT_NO_LANDLOCK=1
}

case "${1:-all}" in
  default) default_case ;;
  no-landlock) no_landlock_case ;;
  all) default_case; no_landlock_case ;;
  *) echo "usage: $0 [default|no-landlock|all]" >&2; exit 2 ;;
esac
echo "[ci] linux sandbox container conformance passed"
