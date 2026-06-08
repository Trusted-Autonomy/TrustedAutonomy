-- Auto-sourced by Neovim on plugin load.
-- Registers the :TA user command with tab-completion.

if vim.g.ta_loaded then return end
vim.g.ta_loaded = true

vim.api.nvim_create_user_command("TA", function(opts)
  require("ta.commands").dispatch(opts)
end, {
  nargs = "*",
  desc = "Trusted Autonomy: start goals, review drafts, view diffs",
  complete = function(arg_lead, cmd_line, cursor_pos)
    return require("ta.commands").complete(arg_lead, cmd_line, cursor_pos)
  end,
})
