-- Background SSE listener via a persistent curl job.
-- Reconnects with exponential backoff on disconnect.
local M = {}

local state = { connected = false, active_goals = 0, pending_drafts = 0 }
local job_id = nil
local reconnect_timer = nil
local last_timestamp = nil
local reconnect_delay = 5000  -- ms; doubles on each retry, capped at 60 s

local function schedule_reconnect()
  if reconnect_timer then
    reconnect_timer:stop()
    reconnect_timer:close()
    reconnect_timer = nil
  end
  reconnect_timer = vim.loop.new_timer()
  reconnect_timer:start(reconnect_delay, 0, vim.schedule_wrap(function()
    reconnect_delay = math.min(reconnect_delay * 2, 60000)
    M.start()
  end))
end

local function handle_event(event_type, data_str)
  local ok, data = pcall(vim.fn.json_decode, data_str)
  if not ok or type(data) ~= "table" then return end

  if data.timestamp then last_timestamp = data.timestamp end

  local c = require("ta").config
  local payload = data.payload or {}

  if event_type == "draft_ready" and c.notify_draft_ready then
    local title = payload.draft_title or "Draft ready"
    vim.notify("TA: " .. title .. "  —  :TA approve", vim.log.levels.INFO)
    state.pending_drafts = state.pending_drafts + 1

  elseif event_type == "goal_state_changed" then
    local s = payload.state or ""
    if s == "applied" or s == "completed" or s == "denied" then
      state.active_goals = math.max(0, state.active_goals - 1)
      vim.notify("TA: " .. s:sub(1,1):upper() .. s:sub(2) .. " — " .. (payload.title or "goal"), vim.log.levels.INFO)
    end

  elseif event_type == "goal_failed" and c.notify_goal_failed then
    local title = payload.title or "goal"
    local msg = payload.message or "unknown error"
    vim.notify("TA: Goal failed — " .. title .. ": " .. msg, vim.log.levels.WARN)

  elseif event_type == "draft_approved" or event_type == "draft_denied" then
    state.pending_drafts = math.max(0, state.pending_drafts - 1)
  end
end

local current_event = "message"
local current_data = ""

local function on_line(line)
  if line == "" then
    if current_data ~= "" then
      local ev, d = current_event, current_data
      vim.schedule(function() handle_event(ev, d) end)
    end
    current_event = "message"
    current_data = ""
  elseif vim.startswith(line, "event:") then
    current_event = vim.trim(line:sub(7))
  elseif vim.startswith(line, "data:") then
    current_data = vim.trim(line:sub(6))
  end
end

function M.start()
  if job_id then
    vim.fn.jobstop(job_id)
    job_id = nil
  end
  current_event = "message"
  current_data = ""

  local c = require("ta").config
  local url = c.daemon_url
    .. "/api/events?types=draft_ready,goal_state_changed,goal_failed,draft_approved,draft_denied"
  if last_timestamp then
    url = url .. "&since=" .. vim.uri_encode(last_timestamp)
  end

  local args = {
    "curl", "-s", "--no-buffer",
    "-H", "Accept: text/event-stream",
    "-H", "Cache-Control: no-cache",
    "--max-time", "120",
  }
  if c.api_token and c.api_token ~= "" then
    vim.list_extend(args, { "-H", "Authorization: Bearer " .. c.api_token })
  end
  table.insert(args, url)

  local partial = ""
  job_id = vim.fn.jobstart(args, {
    on_stdout = function(_, data)
      state.connected = true
      reconnect_delay = 5000
      for i, chunk in ipairs(data) do
        if i == 1 then
          chunk = partial .. chunk
          partial = ""
        end
        if i == #data then
          -- last element may be incomplete; carry it forward
          partial = chunk
        else
          on_line(chunk)
        end
      end
    end,
    on_exit = function()
      job_id = nil
      state.connected = false
      schedule_reconnect()
    end,
  })

  if not job_id or job_id <= 0 then
    state.connected = false
    schedule_reconnect()
  end
end

function M.stop()
  if reconnect_timer then
    reconnect_timer:stop()
    reconnect_timer:close()
    reconnect_timer = nil
  end
  if job_id then
    vim.fn.jobstop(job_id)
    job_id = nil
  end
  state.connected = false
end

function M.get_state()
  return vim.deepcopy(state)
end

function M.update_from_status(status)
  state.connected = true
  state.active_goals = status.active_goals or 0
  state.pending_drafts = status.pending_drafts or 0
end

return M
