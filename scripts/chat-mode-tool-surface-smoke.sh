#!/usr/bin/env bash
# Live smoke test for the chat-mode tool surface (security hypothesis H13).
#
# Launches the real `claude` CLI with the exact flags a TA chat-mode launch
# uses (`--tools ""` + `--strict-mcp-config --mcp-config <file>`, plus the
# restricted-launch flags) against a trivial local MCP server named `ta`, and
# asserts from Claude Code's own session-init event that the agent's tool list
# contains ONLY `mcp__ta__*` tools: no built-in (ListAgents, SendMessage,
# CronCreate, RemoteTrigger, Bash, Read, ...) and no other MCP server.
#
# Usage:   scripts/chat-mode-tool-surface-smoke.sh
# Needs:   `claude` on PATH and logged in (one tiny model call), python3.
# Skips:   exits 0 with a SKIP line when `claude` or python3 is absent.
# Keep the FLAGS line in sync with ta_goal::chat_mode::CHAT_MODE_BUILTIN_TOOLS_FLAG
# and RESTRICTED_LAUNCH_FLAGS in apps/ta-cli/src/commands/run.rs.
set -euo pipefail

if ! command -v claude >/dev/null 2>&1; then
  echo "SKIP: 'claude' not found on PATH; cannot run the live chat-mode tool-surface check."
  exit 0
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "SKIP: python3 not found on PATH; needed for the stub MCP server and the check."
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cat >"$WORK/srv.py" <<'PY'
import sys, json
for line in sys.stdin:
    m = json.loads(line)
    i, meth = m.get("id"), m.get("method")
    if meth == "initialize":
        r = {"protocolVersion": m["params"]["protocolVersion"], "capabilities": {"tools": {}},
             "serverInfo": {"name": "ta", "version": "0"}}
    elif meth == "tools/list":
        r = {"tools": [{"name": "ta_ping", "description": "ping",
                        "inputSchema": {"type": "object", "properties": {}}}]}
    elif meth == "tools/call":
        r = {"content": [{"type": "text", "text": "PONG"}]}
    else:
        if i is None:
            continue
        r = {}
    print(json.dumps({"jsonrpc": "2.0", "id": i, "result": r}), flush=True)
PY

cat >"$WORK/mcp.json" <<EOF
{"mcpServers":{"ta":{"command":"python3","args":["$WORK/srv.py"]}}}
EOF
echo '{"permissions":{"allow":["mcp__ta__ta_ping"]}}' >"$WORK/settings.json"

cat >"$WORK/check.py" <<'PY'
import sys, json
tools = None
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("type") == "system" and m.get("subtype") == "init":
        tools = m.get("tools")
        break
if tools is None:
    print("FAIL: no session init event with a tool list was emitted")
    sys.exit(1)
print("tools visible to the agent:", tools)
bad = [t for t in tools if not t.startswith("mcp__ta__")]
if bad:
    print("FAIL: tools other than mcp__ta__* are reachable:", bad)
    sys.exit(1)
if "mcp__ta__ta_ping" not in tools:
    print("FAIL: the MCP tool is not reachable with the chat-mode flags")
    sys.exit(1)
print("PASS: only mcp__ta__* tools are available (no built-in tool, no other MCP server).")
PY

FLAGS=(--tools "" --strict-mcp-config --mcp-config "$WORK/mcp.json"
       --setting-sources local --permission-mode dontAsk --settings "$WORK/settings.json")
echo "Running: claude -p ... ${FLAGS[*]}"
claude -p "Reply with the single word ok." "${FLAGS[@]}" \
  --output-format stream-json --verbose 2>/dev/null | python3 "$WORK/check.py"
