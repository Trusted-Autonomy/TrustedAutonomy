#!/usr/bin/env bash
# ta-studio — Launch TA Studio in the system browser.
# Starts the TA daemon if not already running, then opens http://localhost:7700.
set -euo pipefail

PORT="${TA_PORT:-7700}"
MAX_WAIT=5

check_daemon() {
  curl -sf "http://localhost:${PORT}/health" >/dev/null 2>&1
}

if ! check_daemon; then
  echo "Starting TA daemon..."
  ta daemon start --background 2>/dev/null || true
  # Wait up to 5 seconds
  for i in $(seq 1 $MAX_WAIT); do
    sleep 1
    if check_daemon; then break; fi
    if [ "$i" -eq "$MAX_WAIT" ]; then
      MSG="TA daemon did not start within ${MAX_WAIT} seconds. Run 'ta daemon start' manually to diagnose."
      if command -v zenity >/dev/null 2>&1; then
        zenity --error --text="$MSG" --title="TA Studio" 2>/dev/null || true
      elif command -v notify-send >/dev/null 2>&1; then
        notify-send "TA Studio" "$MSG" 2>/dev/null || true
      else
        echo "ERROR: $MSG" >&2
      fi
      exit 1
    fi
  done
fi

URL="http://localhost:${PORT}"
if command -v xdg-open >/dev/null 2>&1; then
  xdg-open "$URL"
elif command -v open >/dev/null 2>&1; then
  open "$URL"
else
  echo "TA Studio is ready at $URL"
fi
