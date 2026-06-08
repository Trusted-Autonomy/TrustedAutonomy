-- Diff view: opens original vs staged in a vertically split diff buffer.
-- Reads original from the project root and staged from .ta/staging/<goal_id>/.
-- Falls back to vim.diff() unified display when staging is unavailable.
local M = {}

local function path_from_uri(uri)
  local prefix = "fs://workspace/"
  if vim.startswith(uri, prefix) then
    return uri:sub(#prefix + 1)
  end
  return nil
end

local function read_file(fpath)
  local ok, lines = pcall(vim.fn.readfile, fpath)
  if ok and type(lines) == "table" then
    return table.concat(lines, "\n")
  end
  return nil
end

-- Walk up from cwd looking for .ta/ to find the project root.
local function project_root()
  local dir = vim.fn.getcwd()
  for _ = 1, 12 do
    if vim.fn.isdirectory(dir .. "/.ta") == 1 then
      return dir
    end
    local parent = vim.fn.fnamemodify(dir, ":h")
    if parent == dir then break end
    dir = parent
  end
  return vim.fn.getcwd()
end

local function make_scratch(name, content)
  local buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, vim.split(content or "", "\n"))
  vim.bo[buf].modifiable = false
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  -- name may collide; pcall to avoid errors on repeated calls
  pcall(vim.api.nvim_buf_set_name, buf, "TA:" .. name)
  return buf
end

local function open_split_diff(orig_content, staged_content, label)
  -- Open a fresh tab so we don't clutter the user's layout.
  vim.cmd("tabnew")

  local orig_buf = make_scratch("original/" .. label, orig_content)
  local staged_buf = make_scratch("staged/" .. label, staged_content)

  -- Left pane: original
  vim.api.nvim_win_set_buf(0, orig_buf)
  vim.cmd("diffthis")

  -- Right pane: staged
  vim.cmd("vsplit")
  vim.api.nvim_win_set_buf(0, staged_buf)
  vim.cmd("diffthis")

  -- Land cursor in the staged (right) pane
  vim.cmd("wincmd l")
end

local function show_artifact(root, goal_id, rel_path, change_type, goal_title)
  local change = (change_type or ""):lower()
  local short = vim.fn.fnamemodify(rel_path, ":t")
  local label = short .. " — " .. goal_title

  local orig_content = ""
  local staged_content = ""

  if change ~= "add" and change ~= "create" and change ~= "added" then
    orig_content = read_file(root .. "/" .. rel_path) or ""
  end

  if change ~= "delete" and change ~= "remove" and change ~= "deleted" then
    local sp = root .. "/.ta/staging/" .. goal_id .. "/" .. rel_path
    staged_content = read_file(sp)
    if staged_content == nil then
      staged_content = "-- Staged content not available --\n"
        .. "-- Path: " .. sp .. "\n"
        .. "-- The staging workspace may have been cleaned up after apply."
    end
  end

  open_split_diff(orig_content, staged_content, label)
end

local function pick_and_show(draft)
  local root = project_root()
  local artifacts = {}
  for _, a in ipairs((draft.changes or {}).artifacts or {}) do
    if path_from_uri(a.resource_uri) then
      table.insert(artifacts, a)
    end
  end

  if #artifacts == 0 then
    vim.notify("TA: Draft has no reviewable file changes.", vim.log.levels.WARN)
    return
  end

  if #artifacts == 1 then
    local a = artifacts[1]
    show_artifact(root, draft.goal.goal_id, path_from_uri(a.resource_uri), a.change_type, draft.goal.title)
    return
  end

  vim.ui.select(artifacts, {
    prompt = "Select file to diff (" .. draft.goal.title .. ")",
    format_item = function(a)
      return path_from_uri(a.resource_uri) .. "  [" .. (a.change_type or "modified") .. "]"
    end,
  }, function(a)
    if not a then return end
    show_artifact(root, draft.goal.goal_id, path_from_uri(a.resource_uri), a.change_type, draft.goal.title)
  end)
end

-- Public entry point. draft_id may be nil → show picker.
function M.show(draft_id)
  local client = require("ta.client")

  local function fetch(id)
    client.get_draft(id, function(ok, draft)
      vim.schedule(function()
        if not ok then
          vim.notify("TA: Cannot load draft — " .. tostring(draft), vim.log.levels.ERROR)
          return
        end
        pick_and_show(draft)
      end)
    end)
  end

  if draft_id and draft_id ~= "" then
    fetch(draft_id)
    return
  end

  client.list_drafts(function(ok, drafts)
    vim.schedule(function()
      if not ok then
        vim.notify("TA: Cannot load drafts — " .. tostring(drafts), vim.log.levels.ERROR)
        return
      end
      local terminal = { applied = true, superseded = true, closed = true }
      local active = vim.tbl_filter(function(d)
        return not terminal[(d.status or ""):lower()]
      end, drafts)

      if #active == 0 then
        vim.notify("TA: No pending drafts.", vim.log.levels.INFO)
        return
      end

      vim.ui.select(active, {
        prompt = "Select draft to diff",
        format_item = function(d)
          return (d.title or d.package_id:sub(1, 12))
            .. "  [" .. d.status .. "]  "
            .. d.artifact_count .. " file" .. (d.artifact_count == 1 and "" or "s")
        end,
      }, function(d)
        if d then fetch(d.package_id) end
      end)
    end)
  end)
end

return M
