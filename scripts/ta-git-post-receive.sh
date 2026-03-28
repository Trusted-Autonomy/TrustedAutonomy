#!/usr/bin/env bash
# ta-git-post-receive.sh — Git server-side post-receive hook for TA VCS Event Hooks (v0.14.8.3).
#
# Notifies the local TA daemon when commits are pushed to a git server.
# Works with self-hosted Gitea, GitLab (self-managed), Bitbucket Server,
# Gitolite, and any bare git repository.
#
# Installation:
#   ta setup git-hooks          — installs this script into the current bare repo's hooks/
#   # Or manually:
#   cp ta-git-post-receive.sh /path/to/repo.git/hooks/post-receive
#   chmod +x /path/to/repo.git/hooks/post-receive
#
# Configuration (set in the hook file or shell environment):
#   TA_DAEMON_URL  — TA daemon base URL (default: http://localhost:7700)
#   TA_VCS_SECRET  — HMAC secret from [webhooks.vcs] secret in daemon.toml (optional for localhost)
#   TA_REPO_NAME   — Repository name sent with events (default: basename of repo dir)

set -euo pipefail

TA_DAEMON_URL="${TA_DAEMON_URL:-http://localhost:7700}"
TA_VCS_SECRET="${TA_VCS_SECRET:-}"
TA_REPO_NAME="${TA_REPO_NAME:-$(basename "$(pwd)" .git)}"

# Git post-receive is called with lines on stdin: "<old-sha> <new-sha> <ref>"
while read -r OLD_SHA NEW_SHA REF; do
    # Skip deletions (new SHA is all zeros).
    if [[ "$NEW_SHA" == "0000000000000000000000000000000000000000" ]]; then
        continue
    fi

    # Skip tag refs.
    if [[ "$REF" == refs/tags/* ]]; then
        continue
    fi

    BRANCH="${REF#refs/heads/}"
    PUSHED_BY="${GL_USER:-${GITOLITE_USER:-$(whoami)}}"

    PAYLOAD=$(cat <<JSON
{
  "event": "branch_pushed",
  "payload": {
    "repo": "$TA_REPO_NAME",
    "branch": "$BRANCH",
    "pushed_by": "$PUSHED_BY",
    "commit_sha": "$NEW_SHA",
    "provider": "git"
  }
}
JSON
)

    # Compute HMAC-SHA256 signature if secret is set.
    SIGNATURE=""
    if [[ -n "$TA_VCS_SECRET" ]]; then
        SIGNATURE=$(printf '%s' "$PAYLOAD" | openssl dgst -sha256 -hmac "$TA_VCS_SECRET" | awk '{ print $2 }')
    fi

    # Send the webhook (non-blocking best-effort — don't fail the push).
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        --max-time 5 \
        -X POST \
        -H "Content-Type: application/json" \
        -H "X-TA-Signature: sha256=${SIGNATURE}" \
        -d "$PAYLOAD" \
        "${TA_DAEMON_URL}/api/webhooks/vcs" 2>/dev/null || echo "000")

    if [[ "$HTTP_CODE" == "200" ]]; then
        echo "[ta] Push to $BRANCH registered with TA daemon"
    else
        echo "[ta] Warning: TA daemon unreachable (HTTP $HTTP_CODE) — push continues normally"
    fi
done

exit 0
