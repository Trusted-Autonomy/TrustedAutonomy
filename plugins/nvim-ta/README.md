# nvim-ta

Neovim plugin for [Trusted Autonomy](https://github.com/trustedautonomy/ta) — start AI goals, review
staged diffs, and approve or deny changes without leaving your editor.

## Requirements

- Neovim 0.9+
- `curl` available on `PATH`
- `ta` daemon running (`>= 0.16.6`) — start with `ta shell` or `ta run "<goal>"`

## Installation

### lazy.nvim

```lua
{
  "trustedautonomy/ta",
  subdir = "plugins/nvim-ta",
  config = function()
    require("ta").setup({
      -- daemon_url = "http://127.0.0.1:7700",  -- default
      -- api_token  = "",                        -- leave empty for localhost
      -- poll_interval = 15,                     -- seconds between status polls
      -- notify_draft_ready = true,
      -- notify_goal_failed = true,
    })
  end,
}
```

### packer.nvim

```lua
use {
  "trustedautonomy/ta",
  rtp = "plugins/nvim-ta",
  config = function() require("ta").setup() end,
}
```

### LuaRocks

```bash
luarocks install nvim-ta
```

### Manual

```bash
git clone https://github.com/trustedautonomy/ta ~/.local/share/nvim/site/pack/ta/start/nvim-ta \
  --no-checkout --depth 1
cd ~/.local/share/nvim/site/pack/ta/start/nvim-ta
git sparse-checkout set plugins/nvim-ta
git checkout
ln -s plugins/nvim-ta/lua lua
ln -s plugins/nvim-ta/plugin plugin
```

## Commands

| Command | Description |
|---|---|
| `:TA status` | Floating window: daemon health + active goals |
| `:TA start [title]` | Start a new goal (prompts for title and optional phase) |
| `:TA approve [id]` | Approve a pending draft (picker if no id given) |
| `:TA deny [id]` | Deny a draft with a reason |
| `:TA diff [id]` | Open original vs staged diff in a split tab |
| `:TA shell` | Open the TA web shell in the system browser |
| `:TA help` | Show available commands |

All commands that take an `[id]` show an interactive picker when no id is supplied.

### Tab completion

`:TA <Tab>` completes subcommand names.

## Statusline integration

Add the daemon status to any statusline:

```lua
-- lualine
require("lualine").setup {
  sections = {
    lualine_x = { require("ta").statusline },
  },
}

-- Plain statusline string
vim.o.statusline = "%f  %=" .. "%{luaeval(\"require('ta').statusline()\")}"
```

Output: `TA: 2 running` · `TA: 1 pending` · `TA: ready` · `TA: offline`

## Diff view

`:TA diff` opens the staged and original versions of each changed file side-by-side in
Neovim's built-in diff mode (a new tab, two vertical panes). Navigate with the standard
`]c` / `[c` diff jump keys.

- **Original** — read from the project root (the file as it exists on disk)
- **Staged** — read from `.ta/staging/<goal_id>/` (the agent's proposed changes)

If the staging workspace has been cleaned up (after `ta draft apply`), the staged pane
shows a placeholder message.

## Real-time notifications

When `notify_draft_ready = true` (the default), a notification appears as soon as the
agent finishes and a draft is ready to review — no manual polling needed.

Events handled:
- `draft_ready` → `TA: <title>  —  :TA approve`
- `goal_state_changed` (applied/completed/denied) → `TA: Applied — <title>`
- `goal_failed` → `TA: Goal failed — <title>: <message>` (warn level)

The SSE stream reconnects automatically with exponential backoff (5 s → 60 s).

## Configuration

```lua
require("ta").setup({
  daemon_url         = "http://127.0.0.1:7700",
  api_token          = "",      -- Bearer token; empty = no auth (localhost default)
  notify_draft_ready = true,    -- toast when a draft is ready for review
  notify_goal_failed = true,    -- toast when a goal fails
})
```

## Keymaps (optional)

nvim-ta does not set any global keymaps by default. Add your own:

```lua
vim.keymap.set("n", "<leader>ts", ":TA status<CR>",  { desc = "TA status" })
vim.keymap.set("n", "<leader>ta", ":TA approve<CR>", { desc = "TA approve draft" })
vim.keymap.set("n", "<leader>td", ":TA diff<CR>",    { desc = "TA diff" })
vim.keymap.set("n", "<leader>tn", ":TA start<CR>",   { desc = "TA new goal" })
```

## License

Apache-2.0 — same as Trusted Autonomy.
