-- Async HTTP client via curl (vim.fn.jobstart).
-- All callbacks receive (ok: boolean, data_or_err: table|string).
local M = {}

local function cfg()
  return require("ta").config
end

-- The human credential created by `ta login --file` (or TA_HUMAN_CREDENTIAL).
-- The daemon requires it for approve, deny, apply, commands and answers.
local function human_credential()
  local env = os.getenv("TA_HUMAN_CREDENTIAL")
  if env and env ~= "" then
    return env
  end
  local dir = os.getenv("TA_USER_STATE_DIR")
  if not dir or dir == "" then
    dir = (os.getenv("HOME") or "") .. "/.config/ta/human"
  end
  local f = io.open(dir .. "/credential", "r")
  if not f then
    return nil
  end
  local text = f:read("*a") or ""
  f:close()
  text = text:gsub("%s+$", "")
  return text ~= "" and text or nil
end

-- Advisory X-TA-Actor-Context value when nvim was launched from an agent (same
-- environment markers the `ta` CLI checks), else nil. The daemon records it as
-- actor_context = agent_delegated in the audit trail. Not a security control.
local function actor_context()
  local reasons = {}
  for _, name in ipairs({ "TA_AGENT_CONTEXT", "TA_IS_STAGING", "TA_AGENT_ID", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT" }) do
    local v = os.getenv(name)
    if v and v ~= "" then
      table.insert(reasons, name .. " is set")
    end
  end
  local mode = os.getenv("TA_CALLER_MODE")
  if mode and mode ~= "" and mode ~= "normal" then
    table.insert(reasons, "TA_CALLER_MODE=" .. mode)
  end
  local chat = os.getenv("TA_CHAT_MODE")
  if chat and chat ~= "" then
    table.insert(reasons, "TA_CHAT_MODE is set")
  end
  if #reasons == 0 then
    return nil
  end
  local encoded = {}
  for i = 1, math.min(#reasons, 5) do
    local r = reasons[i]:sub(1, 60):gsub("[^%w%._%-=]", function(c)
      return string.format("%%%02X", string.byte(c))
    end)
    table.insert(encoded, r)
  end
  return "agent_delegated; reasons=" .. table.concat(encoded, ",")
end

local function build_args(method, path, body_json)
  local c = cfg()
  local args = {
    "curl", "-s",
    "-X", method,
    "-H", "Content-Type: application/json",
    "-H", "Accept: application/json",
    "--max-time", "15",
  }
  if c.api_token and c.api_token ~= "" then
    table.insert(args, "-H")
    table.insert(args, "Authorization: Bearer " .. c.api_token)
  end
  local credential = human_credential()
  if credential then
    table.insert(args, "-H")
    table.insert(args, "X-TA-Human-Credential: " .. credential)
  end
  local actor = actor_context()
  if actor then
    table.insert(args, "-H")
    table.insert(args, "X-TA-Actor-Context: " .. actor)
  end
  if body_json then
    table.insert(args, "-d")
    table.insert(args, body_json)
  end
  table.insert(args, c.daemon_url .. path)
  return args
end

function M.request(method, path, body, callback)
  local body_json = body and vim.fn.json_encode(body) or nil
  local args = build_args(method, path, body_json)
  local chunks = {}

  vim.fn.jobstart(args, {
    stdout_buffered = true,
    on_stdout = function(_, data)
      for _, chunk in ipairs(data) do
        if chunk ~= "" then
          table.insert(chunks, chunk)
        end
      end
    end,
    on_exit = function(_, code)
      if code ~= 0 then
        callback(false, "curl exited with code " .. code .. " — is ta running?")
        return
      end
      local text = table.concat(chunks, "")
      if text == "" then
        callback(false, "empty response from daemon")
        return
      end
      local ok, decoded = pcall(vim.fn.json_decode, text)
      if ok then
        callback(true, decoded)
      else
        callback(false, "invalid JSON from daemon: " .. text:sub(1, 100))
      end
    end,
  })
end

function M.get(path, cb)   M.request("GET", path, nil, cb) end
function M.post(path, body, cb) M.request("POST", path, body, cb) end

function M.health(cb)         M.get("/health", cb) end
function M.get_status(cb)     M.get("/api/status", cb) end
function M.list_drafts(cb)    M.get("/api/drafts", cb) end
function M.get_draft(id, cb)  M.get("/api/drafts/" .. id, cb) end

function M.approve_draft(id, cb)
  M.post("/api/drafts/" .. id .. "/approve", {}, cb)
end

function M.deny_draft(id, reason, cb)
  M.post("/api/drafts/" .. id .. "/deny", { reason = reason }, cb)
end

function M.run_command(command, cb)
  M.post("/api/cmd", { command = command }, cb)
end

return M
