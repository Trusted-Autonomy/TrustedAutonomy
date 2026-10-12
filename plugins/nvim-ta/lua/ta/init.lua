local M = {}

local FALLBACK_URL = "http://127.0.0.1:7700"

local function read_file(path)
  local f = io.open(path, "r")
  if not f then
    return nil
  end
  local text = f:read("*a")
  f:close()
  return text
end

-- Each project's daemon has its own port. Walk up from `start_dir` (default:
-- the current directory) to the nearest project that records one: the live
-- daemon's `.ta/daemon.pid` first, then `[server] port` in `.ta/daemon.toml`.
-- Falls back to 7700 only when no project records a port.
function M.resolve_daemon_url(start_dir)
  local dir = start_dir or vim.fn.getcwd()
  while dir and dir ~= "" do
    local pid = read_file(dir .. "/.ta/daemon.pid")
    if pid then
      local host, port = pid:match("bind=([^:\n]+):(%d+)")
      if not port then
        port = pid:match("port=(%d+)")
        host = "127.0.0.1"
      end
      if port then
        if host == "0.0.0.0" or host == "::" then
          host = "127.0.0.1"
        end
        return string.format("http://%s:%s", host, port)
      end
    end
    local toml = read_file(dir .. "/.ta/daemon.toml")
    if toml then
      local server = ("\n" .. toml):match("\n%[server%][^\n]*\n(.-)\n%[") or ("\n" .. toml):match("\n%[server%][^\n]*\n(.*)")
      local port = server and ("\n" .. server):match("\n%s*port%s*=%s*(%d+)")
      if port then
        return string.format("http://127.0.0.1:%s", port)
      end
    end
    local parent = vim.fn.fnamemodify(dir, ":h")
    if parent == dir then
      break
    end
    dir = parent
  end
  return FALLBACK_URL
end

M.config = {
  daemon_url = FALLBACK_URL,
  api_token = "",
  poll_interval = 15,
  notify_draft_ready = true,
  notify_goal_failed = true,
}

function M.setup(opts)
  opts = opts or {}
  M.config = vim.tbl_deep_extend("force", M.config, opts)
  if not opts.daemon_url then
    -- Not set by the user: use this project's own daemon, not a shared default.
    M.config.daemon_url = M.resolve_daemon_url()
  end

  if M.config.notify_draft_ready or M.config.notify_goal_failed then
    require("ta.events").start()
  end
end

-- Embeddable statusline component.
-- Returns "TA: 2 running" | "TA: 1 pending" | "TA: ready" | "TA: offline"
function M.statusline()
  local state = require("ta.events").get_state()
  if not state.connected then
    return "TA: offline"
  end
  if state.active_goals > 0 then
    return string.format("TA: %d running", state.active_goals)
  end
  if state.pending_drafts > 0 then
    return string.format("TA: %d pending", state.pending_drafts)
  end
  return "TA: ready"
end

return M
