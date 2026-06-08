-- :TA status — floating window showing daemon health and active goals.
local M = {}

local win_id = nil
local buf_id = nil

local function fmt_secs(s)
  s = s or 0
  if s < 60 then return string.format("%ds", s) end
  if s < 3600 then return string.format("%dm%ds", math.floor(s / 60), s % 60) end
  return string.format("%dh%dm", math.floor(s / 3600), math.floor((s % 3600) / 60))
end

local function pad_right(s, n)
  return s .. string.rep(" ", math.max(0, n - #s))
end

local WIDTH = 40

local function render(status)
  local lines = {}
  local function line(s) table.insert(lines, s or "") end
  local function rule() line(string.rep("─", WIDTH)) end

  line("")
  line(pad_right("  Daemon:  " .. (status.daemon_version or status.version or "?"), WIDTH))
  line(pad_right("  Project: " .. (status.project or "?"), WIDTH))
  if status.current_phase then
    line(pad_right("  Phase:   " .. status.current_phase.id .. " — " .. status.current_phase.title, WIDTH))
  end
  line("")
  rule()

  local agents = status.active_agents or {}
  if #agents == 0 then
    line("  No active goals")
  else
    line(string.format("  Active goals (%d)", #agents))
    for _, a in ipairs(agents) do
      line("")
      line("  ● " .. (a.title or a.goal_id or "?"))
      line("    state:  " .. (a.state or "?"))
      line("    time:   " .. fmt_secs(a.running_secs))
      if a.vcs_state and a.vcs_state ~= "" then
        line("    branch: " .. a.vcs_state)
      end
    end
  end

  line("")
  rule()

  local pd = status.pending_drafts or 0
  if pd == 0 then
    line("  No pending drafts")
  else
    line(string.format("  Pending drafts: %d  →  :TA approve", pd))
  end

  line("")
  line("  q/<Esc> close    r refresh")
  line("")

  return lines
end

local function close()
  if win_id and vim.api.nvim_win_is_valid(win_id) then
    vim.api.nvim_win_close(win_id, true)
  end
  win_id = nil
  buf_id = nil
end

local function set_lines(lines)
  if not buf_id or not vim.api.nvim_buf_is_valid(buf_id) then return end
  vim.bo[buf_id].modifiable = true
  vim.api.nvim_buf_set_lines(buf_id, 0, -1, false, lines)
  vim.bo[buf_id].modifiable = false
end

local function ensure_win(height)
  if win_id and vim.api.nvim_win_is_valid(win_id) then
    vim.api.nvim_win_set_config(win_id, {
      relative = "editor",
      width = WIDTH,
      height = height,
      row = math.floor((vim.o.lines - height) / 2),
      col = math.floor((vim.o.columns - WIDTH) / 2),
    })
    return
  end

  win_id = vim.api.nvim_open_win(buf_id, true, {
    relative = "editor",
    width = WIDTH,
    height = height,
    row = math.floor((vim.o.lines - height) / 2),
    col = math.floor((vim.o.columns - WIDTH) / 2),
    style = "minimal",
    border = "rounded",
    title = " TA Status ",
    title_pos = "center",
  })
  vim.wo[win_id].wrap = false
  vim.wo[win_id].cursorline = true
end

local function ensure_buf()
  if buf_id and vim.api.nvim_buf_is_valid(buf_id) then return end
  buf_id = vim.api.nvim_create_buf(false, true)
  vim.bo[buf_id].modifiable = false
  vim.bo[buf_id].bufhidden = "wipe"
  vim.bo[buf_id].filetype = "ta-status"

  local opts = { buffer = buf_id, nowait = true, silent = true }
  vim.keymap.set("n", "q", close, opts)
  vim.keymap.set("n", "<Esc>", close, opts)
  vim.keymap.set("n", "r", M.show, opts)
end

function M.show()
  ensure_buf()
  set_lines({ "", "  Fetching status…", "" })
  ensure_win(3)

  local client = require("ta.client")
  client.get_status(function(ok, data)
    vim.schedule(function()
      if not ok then
        client.health(function(hok, hdata)
          vim.schedule(function()
            local msg = hok
              and ("  Daemon v" .. (hdata.version or "?") .. " — no status data")
              or  "  TA daemon is offline — run: ta shell"
            set_lines({ "", msg, "" })
            ensure_win(3)
          end)
        end)
        return
      end
      require("ta.events").update_from_status(data)
      local lines = render(data)
      set_lines(lines)
      ensure_win(#lines)
    end)
  end)
end

return M
