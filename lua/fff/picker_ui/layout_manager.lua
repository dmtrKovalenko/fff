local M = {}

local layout = require('fff.layout')
local list_separator = require('fff.list_separator')
local scrollbar = require('fff.scrollbar')
local preview = require('fff.file_picker.preview')
local picker_ui_state = require('fff.picker_ui.picker_ui_state')

-- Parent module reference (set by picker_ui.lua during initialization).
---@type table
local P = nil

function M.init(parent_module) P = parent_module end

local S = picker_ui_state.state

local function restore_paste(should_restore)
  if should_restore then vim.o.paste = true end
end

function M.relayout()
  if not P.state.active then return end

  local config = S.config
  if not config then return end

  if S.maximized then
    config = vim.deepcopy(config)
    config.layout.width = 1.0
    config.layout.height = 1.0
  end

  local computed_layout = layout.compute(config, P.is_preview_enabled())
  local win_configs = computed_layout.win_configs
  S.layout = computed_layout.layout
  S.preview_visible = computed_layout.preview_visible

  if S.list_win and vim.api.nvim_win_is_valid(S.list_win) then
    vim.api.nvim_win_set_config(S.list_win, win_configs.list)
  end

  if S.input_win and vim.api.nvim_win_is_valid(S.input_win) then
    vim.api.nvim_win_set_config(S.input_win, win_configs.input)
  end

  local preview_win_alive = S.preview_win and vim.api.nvim_win_is_valid(S.preview_win)
  if S.preview_visible and win_configs.preview then
    if preview_win_alive then
      vim.api.nvim_win_set_config(S.preview_win, win_configs.preview)
    else
      P.open_preview(win_configs.preview)
    end
  elseif preview_win_alive then
    P.close_preview()
  end

  local file_info_win_alive = S.file_info_win and vim.api.nvim_win_is_valid(S.file_info_win)
  if win_configs.file_info then
    if file_info_win_alive then
      vim.api.nvim_win_set_config(S.file_info_win, win_configs.file_info)
    else
      S.file_info_buf = vim.api.nvim_create_buf(false, true)
      vim.api.nvim_set_option_value('bufhidden', 'wipe', { buf = S.file_info_buf })
      vim.api.nvim_set_option_value('buftype', 'nofile', { buf = S.file_info_buf })
      vim.api.nvim_set_option_value('filetype', 'fff_file_info', { buf = S.file_info_buf })
      vim.api.nvim_set_option_value('modifiable', false, { buf = S.file_info_buf })
      S.file_info_win = vim.api.nvim_open_win(S.file_info_buf, false, win_configs.file_info)
    end
  elseif file_info_win_alive then
    vim.api.nvim_win_close(S.file_info_win, true)
    S.file_info_win = nil
    if S.file_info_buf and vim.api.nvim_buf_is_valid(S.file_info_buf) then
      vim.api.nvim_buf_delete(S.file_info_buf, { force = true })
    end
    S.file_info_buf = nil
  end

  P.render_list()
  P.update_preview()
  P.update_status()
end

function M.close()
  if not P.state.active then return end

  vim.cmd('stopinsert')
  P.state.active = false

  restore_paste(S.restore_paste)

  list_separator.cleanup()
  scrollbar.cleanup()

  local ts_ok, ts_hl = pcall(require, 'fff.treesitter_hl')
  if ts_ok then ts_hl.cleanup() end

  local windows = { S.input_win, S.list_win, S.preview_win }
  if S.file_info_win then table.insert(windows, S.file_info_win) end

  for _, win in ipairs(windows) do
    if win and vim.api.nvim_win_is_valid(win) then vim.api.nvim_win_close(win, true) end
  end

  local buffers = { S.input_buf, S.list_buf, S.file_info_buf }
  if S.preview_buf then buffers[#buffers + 1] = S.preview_buf end

  for _, buf in ipairs(buffers) do
    if buf and vim.api.nvim_buf_is_valid(buf) then
      vim.api.nvim_buf_clear_namespace(buf, -1, 0, -1)
      if buf == S.preview_buf then preview.clear_buffer(buf) end
      vim.api.nvim_buf_delete(buf, { force = true })
    end
  end

  P.close_preview_timer()

  S.input_win = nil
  S.list_win = nil
  S.file_info_win = nil
  S.preview_win = nil
  S.input_buf = nil
  S.list_buf = nil
  S.file_info_buf = nil
  S.preview_buf = nil
  S.preview_visible = false
  S.preview_enabled = nil
  S.maximized = false
  S.items = {}
  S.filtered_items = {}
  S.line_to_item = {}
  S.item_to_lines = {}
  S.last_render_ctx = nil
  S.cursor = 1
  S.query = ''
  S.ns_id = nil
  S.last_preview_file = nil
  S.last_preview_location = nil
  S.current_file_cache = nil
  S.location = nil
  S.selected_files = {}
  S.selected_file_order = {}
  S.selected_items = {}
  S.mode = nil
  S.grep_config = nil
  S.grep_mode = 'plain'
  S.grep_regex_fallback_error = nil
  S.suggestion_items = nil
  S.suggestion_source = nil
  S.renderer = nil
  S.restore_paste = false
  S.combo_visible = true
  S.combo_initial_cursor = nil
  P.reset_history_state()
  pcall(vim.api.nvim_del_augroup_by_name, 'fff_picker_focus')
  require('fff.file_picker').pause_git_status()

  vim.api.nvim_exec_autocmds('User', { pattern = 'FFFClose', modeline = false })
end

return M
