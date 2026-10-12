#!/usr/bin/env bash
# Embedded in TA Studio.app/Contents/MacOS/ta-studio-launcher
# Each project has its own daemon port; ask `ta` for this project's rather than
# assuming 7700. TA_PORT overrides; 7700 is only the last resort.
PORT="${TA_PORT:-$(ta daemon config 2>/dev/null | sed -n 's/^ *Daemon URL: *http:\/\/[^:]*:\([0-9][0-9]*\).*/\1/p' | head -n 1)}"
PORT="${PORT:-7700}"
MAX_WAIT=5

check_daemon() {
  curl -sf "http://localhost:${PORT}/health" >/dev/null 2>&1
}

if ! check_daemon; then
  ta daemon start --background 2>/dev/null || true
  for i in $(seq 1 $MAX_WAIT); do
    sleep 1
    if check_daemon; then break; fi
    if [ "$i" -eq "$MAX_WAIT" ]; then
      osascript -e "display dialog \"TA daemon did not start within ${MAX_WAIT} seconds. Open Terminal and run 'ta daemon start' to diagnose.\" with title \"TA Studio\" buttons {\"OK\"} default button \"OK\" with icon stop" 2>/dev/null || true
      exit 1
    fi
  done
fi

# `ta login open` signs the UI in with the human credential; fall back to a plain open.
ta login open 2>/dev/null || open "http://localhost:${PORT}"
