#!/usr/bin/env bash
# ta-p4-trigger.sh — Perforce trigger script for TA VCS Event Hooks (v0.14.8.3).
#
# Notifies the local TA daemon when a Perforce changelist is submitted.
#
# DEPOT-COMMITTED SETUP (recommended — no ongoing server access needed):
#
#   1. Commit this script to your depot, e.g.:
#        p4 add //depot/.ta/triggers/ta-p4-trigger.sh
#        p4 submit -d "Add TA webhook trigger"
#
#   2. Register once with Perforce (requires p4 admin, one-time only):
#        p4 triggers -o > /tmp/p4triggers.txt
#        # Add this line under "Triggers:" — it fetches the latest depot version each time:
#        # ta-cl-submitted change-commit //depot/... "bash -c 'TA_DAEMON_URL=http://your-ta-host:7700 TA_VCS_SECRET=your-secret p4 print -q //depot/.ta/triggers/ta-p4-trigger.sh | bash -s -- %change%'"
#        p4 triggers -i < /tmp/p4triggers.txt
#
#   3. Future updates: just submit the updated script to the depot. No server access needed.
#
# Configuration:
#   TA_DAEMON_URL  — TA daemon base URL (default: http://localhost:7700)
#   TA_VCS_SECRET  — HMAC secret from [webhooks.vcs] secret in daemon.toml (optional)
#
# Usage:
#   ta-p4-trigger.sh <change-number>
#   ta-p4-trigger.sh install   — print complete p4 triggers setup instructions

set -euo pipefail

TA_DAEMON_URL="${TA_DAEMON_URL:-http://localhost:7700}"
TA_VCS_SECRET="${TA_VCS_SECRET:-}"
P4_PORT="${P4PORT:-perforce:1666}"

if [[ "${1:-}" == "install" ]]; then
    cat <<'EOF'
# ─── Perforce Trigger Installation (depot-committed) ─────────────────────────
#
# This approach stores the trigger script in your depot so updates require
# no server access — just commit the new version and it takes effect immediately.
#
# STEP 1: Commit this script to your depot
#
#   p4 edit //depot/.ta/triggers/ta-p4-trigger.sh   # if it exists
#   # or: p4 add //depot/.ta/triggers/ta-p4-trigger.sh  # first time
#   p4 submit -d "Update TA webhook trigger"
#
#   Adjust the depot path to match your project layout, e.g.:
#     //your-project/main/.ta/triggers/ta-p4-trigger.sh
#
# STEP 2: Register with Perforce (one-time, requires p4 admin access)
#
#   p4 triggers -o > /tmp/p4triggers.txt
#
#   Add this line under "Triggers:" — replace the depot path and env vars:
#
#   ta-cl-submitted change-commit //depot/... "bash -c 'TA_DAEMON_URL=http://your-ta-host:7700 TA_VCS_SECRET=your-secret p4 print -q //depot/.ta/triggers/ta-p4-trigger.sh | bash -s -- %change%'"
#
#   Then apply:
#   p4 triggers -i < /tmp/p4triggers.txt
#
#   The trigger form entry uses "p4 print -q" to fetch the latest committed version
#   from the depot each time it fires. No script file lives on the server filesystem.
#
# STEP 3: Set the configuration values in the trigger entry
#
#   Replace these placeholders in the Triggers line above:
#     http://your-ta-host:7700  — URL of the machine running the TA daemon
#     your-secret               — value of [webhooks.vcs] secret in .ta/daemon.toml
#     //depot/.ta/triggers/...  — the depot path where you committed this script
#     //depot/...               — the depot path pattern that should fire the trigger
#                                 (narrow this to avoid firing on every change across all depots)
#
# STEP 4: Verify
#
#   # Submit a test changelist; the daemon should log a vcs.changelist_submitted event.
#   # Or simulate without a real submit:
#   ta webhook test vcs changelist_submitted --change 12345 --depot //depot/main/...
#
# ─────────────────────────────────────────────────────────────────────────────
EOF
    exit 0
fi

CHANGE="${1:?Usage: ta-p4-trigger.sh <change-number>}"

# Fetch changelist details from Perforce.
SUBMITTER=$(p4 -p "$P4_PORT" -z tag change -o "$CHANGE" 2>/dev/null | awk '/\^User:/ { print $2; exit }' || echo "unknown")
DESCRIPTION=$(p4 -p "$P4_PORT" -z tag change -o "$CHANGE" 2>/dev/null | awk '/\^Description:/ { getline; gsub(/^\t/, ""); print; exit }' || echo "")
DEPOT_PATH=$(p4 -p "$P4_PORT" files "@=$CHANGE" 2>/dev/null | head -1 | sed 's|#.*||' | sed 's|/[^/]*$|/...|' || echo "//depot/...")

# Build the payload.
PAYLOAD=$(cat <<JSON
{
  "event": "changelist_submitted",
  "payload": {
    "depot_path": "$DEPOT_PATH",
    "change_number": $CHANGE,
    "submitter": "$SUBMITTER",
    "description": "$DESCRIPTION",
    "provider": "perforce"
  }
}
JSON
)

# Compute HMAC-SHA256 signature if secret is set.
SIGNATURE=""
if [[ -n "$TA_VCS_SECRET" ]]; then
    SIGNATURE=$(printf '%s' "$PAYLOAD" | openssl dgst -sha256 -hmac "$TA_VCS_SECRET" | awk '{ print $2 }')
fi

# Send the webhook.
RESPONSE=$(curl -s -o /dev/stderr -w "%{http_code}" \
    -X POST \
    -H "Content-Type: application/json" \
    -H "X-TA-Signature: sha256=${SIGNATURE}" \
    -d "$PAYLOAD" \
    "${TA_DAEMON_URL}/api/webhooks/vcs")

HTTP_CODE="$RESPONSE"

if [[ "$HTTP_CODE" == "200" ]]; then
    echo "[ta-p4-trigger] Changelist $CHANGE submitted to TA (depot: $DEPOT_PATH)" >&2
else
    echo "[ta-p4-trigger] Warning: TA webhook returned HTTP $HTTP_CODE for changelist $CHANGE" >&2
    echo "[ta-p4-trigger] Check TA daemon is running: ta daemon status" >&2
    # Exit 0 — don't block Perforce submit on TA availability.
fi

exit 0
