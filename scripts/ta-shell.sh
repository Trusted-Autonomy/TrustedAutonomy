#!/usr/bin/env bash
# ta-shell.sh -- Build, start the TA daemon, and launch the interactive shell.
#
# Usage:
#   ./scripts/ta-shell.sh [--port 7700] [--project-root .] [--no-build] [shell args...]
#
# The script builds the workspace (via Nix devShell), checks whether the daemon
# is already listening, starts one if not, and opens the shell.
# On exit, the daemon keeps running (use `kill` or stop it manually).

set -euo pipefail

PORT="${TA_DAEMON_PORT:-7700}"
BIND="${TA_DAEMON_BIND:-127.0.0.1}"
PROJECT_ROOT="."
SHELL_ARGS=()
SKIP_BUILD=false

# Parse arguments.
while [[ $# -gt 0 ]]; do
  case "$1" in
    --port)         PORT="$2"; shift 2 ;;
    --project-root) PROJECT_ROOT="$2"; shift 2 ;;
    --no-build)     SKIP_BUILD=true; shift ;;
    *)              SHELL_ARGS+=("$1"); shift ;;
  esac
done

DAEMON_URL="http://${BIND}:${PORT}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="${SCRIPT_DIR}/.."
DEV_SCRIPT="${REPO_ROOT}/dev"

# ── Build ─────────────────────────────────────────────────────
if [[ "$SKIP_BUILD" == false ]]; then
  echo "Building ta-daemon and ta (via Nix devShell)..."
  if [[ -x "$DEV_SCRIPT" ]]; then
    "$DEV_SCRIPT" "cargo build --bin ta-daemon --bin ta" || {
      echo "Error: build failed" >&2
      exit 1
    }
  else
    echo "Warning: ./dev script not found, assuming binaries are up to date" >&2
  fi
fi

# ── Locate binaries ──────────────────────────────────────────
find_bin() {
  local name="$1"
  if [[ -x "${REPO_ROOT}/target/release/${name}" ]]; then
    echo "${REPO_ROOT}/target/release/${name}"
  elif [[ -x "${REPO_ROOT}/target/debug/${name}" ]]; then
    echo "${REPO_ROOT}/target/debug/${name}"
  elif command -v "$name" &>/dev/null; then
    command -v "$name"
  else
    echo "Error: cannot find binary '${name}'" >&2
    exit 1
  fi
}

TA_BIN="$(find_bin ta)"
DAEMON_BIN="$(find_bin ta-daemon)"

echo "Using daemon: ${DAEMON_BIN}"
echo "Using CLI:    ${TA_BIN}"

# ── Start daemon if needed ───────────────────────────────────
daemon_healthy() {
  curl -sf "${DAEMON_URL}/api/status" >/dev/null 2>&1
}

daemon_version() {
  curl -sf "${DAEMON_URL}/api/status" 2>/dev/null | grep -o '"version":"[^"]*"' | cut -d'"' -f4
}

built_version() {
  "$DAEMON_BIN" --version 2>&1 | grep -o '[0-9][0-9.]*-[a-z]*' || echo "unknown"
}

if daemon_healthy; then
  RUNNING_VER="$(daemon_version)"
  echo "Daemon already running at ${DAEMON_URL} (v${RUNNING_VER})"

  # Kill and restart if the running daemon is stale compared to the built binary.
  BUILT_VER="$(built_version)"
  if [[ -n "$RUNNING_VER" && -n "$BUILT_VER" && "$RUNNING_VER" != "$BUILT_VER" && "$BUILT_VER" != "unknown" ]]; then
    echo "Stale daemon (v${RUNNING_VER}) — restarting with v${BUILT_VER}..."
    pkill -f 'ta-daemon --api' 2>/dev/null || true
    sleep 1

    echo "Starting daemon at ${DAEMON_URL} ..."
    "$DAEMON_BIN" --api --project-root "$PROJECT_ROOT" &
    DAEMON_PID=$!

    for i in $(seq 1 20); do
      if daemon_healthy; then
        echo "Daemon ready (pid ${DAEMON_PID})"
        break
      fi
      if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        echo "Error: daemon exited unexpectedly" >&2
        exit 1
      fi
      sleep 0.5
    done

    if ! daemon_healthy; then
      echo "Error: restarted daemon did not become healthy within 10 seconds" >&2
      kill "$DAEMON_PID" 2>/dev/null || true
      exit 1
    fi
  fi
else
  echo "Starting daemon at ${DAEMON_URL} ..."
  "$DAEMON_BIN" --api --project-root "$PROJECT_ROOT" &
  DAEMON_PID=$!

  # Wait up to 10 seconds for the daemon to become healthy.
  for i in $(seq 1 20); do
    if daemon_healthy; then
      echo "Daemon ready (pid ${DAEMON_PID})"
      break
    fi
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      echo "Error: daemon exited unexpectedly" >&2
      echo "Try running: ${DAEMON_BIN} --api --project-root ${PROJECT_ROOT}" >&2
      exit 1
    fi
    sleep 0.5
  done

  if ! daemon_healthy; then
    echo "Error: daemon did not become healthy within 10 seconds" >&2
    kill "$DAEMON_PID" 2>/dev/null || true
    exit 1
  fi
fi

# ── Launch the shell ─────────────────────────────────────────
exec "$TA_BIN" shell --url "$DAEMON_URL" "${SHELL_ARGS[@]}"
