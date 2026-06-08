-- Implementations for each :TA subcommand.
local M = {}

local function err(msg) vim.notify("TA: " .. msg, vim.log.levels.ERROR) end
local function info(msg) vim.notify("TA: " .. msg, vim.log.levels.INFO) end

-- Helpers -------------------------------------------------------------------

local TERMINAL = { applied = true, superseded = true, closed = true, denied = true }

local function active_drafts(drafts)
  return vim.tbl_filter(function(d)
    return not TERMINAL[(d.status or ""):lower()]
  end, drafts)
end

local function draft_label(d)
  return (d.title or d.package_id:sub(1, 12))
    .. "  [" .. d.status .. "]  "
    .. d.artifact_count .. " file" .. (d.artifact_count == 1 and "" or "s")
end

-- :TA start -----------------------------------------------------------------

function M.start(args)
  local default = #args > 0 and table.concat(args, " ") or ""
  vim.ui.input({ prompt = "Goal description: ", default = default }, function(title)
    if not title or vim.trim(title) == "" then return end
    title = vim.trim(title)

    vim.ui.input({ prompt = "Plan phase (optional, e.g. v0.16.6): " }, function(phase)
      local phase_arg = ""
      if phase and vim.trim(phase) ~= "" then
        local p = vim.trim(phase)
        -- Phase IDs are version strings (e.g. v0.16.6.1) — only alphanumeric,
        -- dots, and dashes are valid.  Reject anything else so characters like
        -- `"` cannot break out of the quoted argument and inject extra flags.
        if p:match("[^%w%.%-]") then
          err('Invalid phase "' .. p .. '" — only letters, digits, dots, and dashes allowed')
          return
        end
        phase_arg = ' --phase "' .. p .. '"'
      end
      local cmd = 'ta run "' .. title:gsub('"', '\\"') .. '"' .. phase_arg
      info("Starting goal: " .. title .. "…")
      require("ta.client").run_command(cmd, function(ok, result)
        vim.schedule(function()
          if not ok then
            err("Cannot reach TA daemon — " .. tostring(result) .. ". Is it running?")
          elseif result.exit_code == 0 then
            info("Goal started: " .. title)
          else
            local detail = (result.stderr ~= "" and result.stderr) or result.stdout or "no details"
            err("Failed to start goal: " .. detail:sub(1, 120))
          end
        end)
      end)
    end)
  end)
end

-- :TA approve ---------------------------------------------------------------

function M.approve(args)
  local client = require("ta.client")

  local function do_approve(id, label)
    vim.ui.select({ "Approve", "Cancel" }, {
      prompt = 'Apply draft "' .. label .. '"?',
    }, function(choice)
      if choice ~= "Approve" then return end
      client.approve_draft(id, function(ok, result)
        vim.schedule(function()
          if not ok then err("Approve failed — " .. tostring(result))
          else info(result.message or result.status or "approved") end
        end)
      end)
    end)
  end

  if args[1] and args[1] ~= "" then
    do_approve(args[1], args[1]:sub(1, 12))
    return
  end

  client.list_drafts(function(ok, drafts)
    vim.schedule(function()
      if not ok then err("Cannot load drafts — " .. tostring(drafts)); return end
      local active = active_drafts(drafts)
      if #active == 0 then info("No pending drafts."); return end
      vim.ui.select(active, {
        prompt = "Select draft to approve",
        format_item = draft_label,
      }, function(d)
        if d then do_approve(d.package_id, d.title or d.package_id:sub(1, 12)) end
      end)
    end)
  end)
end

-- :TA deny ------------------------------------------------------------------

function M.deny(args)
  local client = require("ta.client")

  local function do_deny(id, label)
    vim.ui.input({ prompt = 'Reason for denying "' .. label .. '": ' }, function(reason)
      if reason == nil then return end
      if vim.trim(reason) == "" then reason = "Denied via Neovim plugin" end
      client.deny_draft(id, reason, function(ok, result)
        vim.schedule(function()
          if not ok then err("Deny failed — " .. tostring(result))
          else info(result.message or result.status or "denied") end
        end)
      end)
    end)
  end

  if args[1] and args[1] ~= "" then
    do_deny(args[1], args[1]:sub(1, 12))
    return
  end

  client.list_drafts(function(ok, drafts)
    vim.schedule(function()
      if not ok then err("Cannot load drafts — " .. tostring(drafts)); return end
      local active = active_drafts(drafts)
      if #active == 0 then info("No pending drafts."); return end
      vim.ui.select(active, {
        prompt = "Select draft to deny",
        format_item = draft_label,
      }, function(d)
        if d then do_deny(d.package_id, d.title or d.package_id:sub(1, 12)) end
      end)
    end)
  end)
end

-- :TA shell -----------------------------------------------------------------

function M.shell(_args)
  local url = require("ta").config.daemon_url .. "/shell"
  local opener = vim.fn.has("mac") == 1 and "open"
    or vim.fn.has("unix") == 1 and "xdg-open"
    or "start"
  vim.fn.jobstart({ opener, url }, { detach = true })
  info("Opening shell at " .. url)
end

-- :TA diff ------------------------------------------------------------------

function M.diff(args)
  require("ta.diff").show(args[1])
end

-- :TA status ----------------------------------------------------------------

function M.status(_args)
  require("ta.status").show()
end

-- :TA help ------------------------------------------------------------------

function M.help(_args)
  local msg = table.concat({
    "TA — Trusted Autonomy",
    "",
    "  :TA status           daemon health + active goals (floating window)",
    "  :TA start [title]    start a new goal",
    "  :TA approve [id]     approve a pending draft",
    "  :TA deny    [id]     deny a draft with a reason",
    "  :TA diff    [id]     view staged vs original diff in a split tab",
    "  :TA shell            open TA web shell in the system browser",
    "  :TA help             show this help",
    "",
    "Statusline: require('ta').statusline()",
    "  → 'TA: 2 running' | 'TA: 1 pending' | 'TA: ready' | 'TA: offline'",
  }, "\n")
  vim.notify(msg, vim.log.levels.INFO)
end

-- Dispatch ------------------------------------------------------------------

local SUBCOMMANDS = {
  status  = M.status,
  start   = M.start,
  approve = M.approve,
  deny    = M.deny,
  diff    = M.diff,
  shell   = M.shell,
  help    = M.help,
}

function M.dispatch(opts)
  local fargs = opts.fargs or {}
  local sub = fargs[1] or "status"
  local rest = vim.list_slice(fargs, 2)
  local fn = SUBCOMMANDS[sub]
  if fn then
    fn(rest)
  else
    err("Unknown subcommand '" .. sub .. "' — try :TA help")
  end
end

function M.complete(arg_lead, cmd_line, _cursor)
  local parts = vim.split(cmd_line, "%s+", { trimempty = true })
  -- If we are completing the first arg after "TA", offer subcommand names.
  if #parts <= 2 then
    local names = vim.tbl_keys(SUBCOMMANDS)
    table.sort(names)
    return vim.tbl_filter(function(s) return vim.startswith(s, arg_lead) end, names)
  end
  return {}
end

return M
