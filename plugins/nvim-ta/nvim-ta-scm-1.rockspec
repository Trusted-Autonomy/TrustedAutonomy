package = "nvim-ta"
version = "scm-1"

source = {
  url = "git+https://github.com/trustedautonomy/ta",
  subdir = "plugins/nvim-ta",
}

description = {
  summary = "Neovim plugin for Trusted Autonomy — start goals, review drafts, view diffs",
  detailed = [[
nvim-ta integrates Trusted Autonomy into Neovim via a single :TA command.

Commands:
  :TA status           daemon health and active goals (floating window)
  :TA start [title]    start a new goal
  :TA approve [id]     approve a pending draft
  :TA deny [id]        deny a draft with a reason
  :TA diff [id]        view staged vs original diff in a split tab
  :TA shell            open the TA web shell in the system browser

Statusline:
  require("ta").statusline()
  Returns "TA: 2 running" | "TA: 1 pending" | "TA: ready" | "TA: offline"

Requirements:
  Neovim 0.9+
  curl (available on PATH)
  ta daemon >= 0.16.6  (https://github.com/trustedautonomy/ta)
  ]],
  homepage = "https://github.com/trustedautonomy/ta",
  license = "Apache-2.0",
  labels = { "neovim", "plugin", "ai", "agent" },
}

supported_platforms = { "linux", "macosx" }

dependencies = {
  "lua >= 5.1",
}

build = {
  type = "builtin",
  modules = {
    ["ta"]          = "lua/ta/init.lua",
    ["ta.client"]   = "lua/ta/client.lua",
    ["ta.commands"] = "lua/ta/commands.lua",
    ["ta.diff"]     = "lua/ta/diff.lua",
    ["ta.events"]   = "lua/ta/events.lua",
    ["ta.status"]   = "lua/ta/status.lua",
  },
  copy_directories = { "plugin" },
}
