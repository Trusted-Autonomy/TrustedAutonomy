local M = {}

M.config = {
  daemon_url = "http://127.0.0.1:7700",
  api_token = "",
  poll_interval = 15,
  notify_draft_ready = true,
  notify_goal_failed = true,
}

function M.setup(opts)
  M.config = vim.tbl_deep_extend("force", M.config, opts or {})

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
