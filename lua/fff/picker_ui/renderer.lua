local M = {}

local rust = require('fff.rust')
local list_separator = require('fff.list_separator')
local scrollbar = require('fff.scrollbar')
local list_renderer = require('fff.picker_ui.list_renderer')
local combo_renderer = require('fff.picker_ui.combo_renderer')
local file_picker = require('fff.file_picker')
local layout = require('fff.layout')
local picker_ui_state = require('fff.picker_ui.picker_ui_state')

-- Parent module reference (set by picker_ui.lua during initialization).
-- Allows renderer functions to call back into the main picker module.
---@type table
local P = nil

function M.init(parent_module) P = parent_module end

-- Convenience alias
local S = picker_ui_state.state

local function get_prompt_position() return layout.resolve_prompt_position(S.config) end

local function shrink_path(path, max_width)
  local config = S.config or {}
  local strategy = config.layout and config.layout.path_shorten_strategy or 'middle_number'
  return rust.shorten_path(path, max_width, strategy)
end

local function format_file_display(item, max_width)
  local filename = item.name
  if type(filename) ~= 'string' then filename = filename and tostring(filename) or '' end
  local dir_path = item.directory or ''
  if type(dir_path) ~= 'string' then dir_path = dir_path and tostring(dir_path) or '' end

  if dir_path == '' and item.relative_path then
    local parent_dir = vim.fn.fnamemodify(item.relative_path, ':h')
    if parent_dir ~= '.' and parent_dir ~= '' then dir_path = parent_dir end
  end

  local filename_width = vim.fn.strdisplaywidth(filename)
  local base_width = filename_width + 1
  local path_max_width = math.max(max_width - base_width, 0)

  if dir_path == '' or path_max_width == 0 then return filename, '' end
  local display_path = shrink_path(dir_path, path_max_width)

  return filename, display_path
end

--- Adjust scroll for bottom prompt to eliminate gaps.
function M.scroll_to_bottom()
  if not S.list_win or not vim.api.nvim_win_is_valid(S.list_win) then return end

  local win_height = vim.api.nvim_win_get_height(S.list_win)
  local buf_lines = vim.api.nvim_buf_line_count(S.list_buf)

  vim.api.nvim_win_call(S.list_win, function()
    local view = vim.fn.winsaveview()
    local bottom_topline = math.max(1, buf_lines - win_height + 1)
    local cursor_line = vim.api.nvim_win_get_cursor(S.list_win)[1]

    if cursor_line >= bottom_topline then
      view.topline = bottom_topline
    elseif cursor_line < view.topline then
      view.topline = math.max(1, cursor_line - 1)
    elseif cursor_line >= view.topline + win_height then
      view.topline = bottom_topline
    end
    vim.fn.winrestview(view)
  end)
end

--- Render the grep empty state.
local function render_grep_empty_state(ctx)
  list_separator.hide()

  local config = ctx.config
  local win_width = ctx.win_width
  local win_height = ctx.win_height
  local prompt_position = ctx.prompt_position

  local content = {}
  local hl_cmds = {}

  table.insert(content, '')
  table.insert(content, '  Start typing to search file contents...')
  table.insert(content, '')
  table.insert(content, '  Tips:')
  table.insert(content, '    "pattern *.rs"    search only in Rust files')
  table.insert(content, '    "pattern /src/"   limit search to src/ directory')
  table.insert(content, '    "!test pattern"   exclude test files')
  table.insert(content, '')

  if prompt_position == 'bottom' then
    local empty_needed = math.max(0, win_height - #content)
    for _ = 1, empty_needed do
      table.insert(content, 1, string.rep(' ', win_width + 5))
      for _, h in ipairs(hl_cmds) do
        h.row = h.row + 1
      end
    end
  end

  vim.api.nvim_set_option_value('modifiable', true, { buf = S.list_buf })
  vim.api.nvim_buf_set_lines(S.list_buf, 0, -1, false, content)
  vim.api.nvim_set_option_value('modifiable', false, { buf = S.list_buf })

  vim.api.nvim_buf_clear_namespace(S.list_buf, S.ns_id, 0, -1)

  if prompt_position == 'bottom' then M.scroll_to_bottom() end
  for _, h in ipairs(hl_cmds) do
    pcall(
      vim.api.nvim_buf_set_extmark,
      S.list_buf,
      S.ns_id,
      h.row,
      h.col_start,
      { end_col = h.col_end, hl_group = h.hl }
    )
  end

  for i = 0, #content - 1 do
    local line = content[i + 1]
    if
      line and (line:match('^%s+Start typing') or line:match('^%s+Tips') or line:match('^%s+"') or line:match('^%s+!'))
    then
      pcall(
        vim.api.nvim_buf_set_extmark,
        S.list_buf,
        S.ns_id,
        i,
        0,
        { end_row = i + 1, end_col = 0, hl_group = 'Comment' }
      )
    end
    if line and (line:match('^%s+[╭╰│]') or line:match('[╮╯│]%s*$')) then
      pcall(
        vim.api.nvim_buf_set_extmark,
        S.list_buf,
        S.ns_id,
        i,
        0,
        { end_row = i + 1, end_col = 0, hl_group = config.hl.border or 'FloatBorder' }
      )
    end
  end
end

--- Grep results come with definitions first (see `grep.definitions_first`):
--- box them off from the usages below with the same separator as combo.
--- @return ListSeparator|nil
local function definitions_separator(items, config)
  local count = 0
  while items[count + 1] and items[count + 1].is_definition do
    count = count + 1
  end
  if count == 0 or count == #items then return nil end

  return {
    idx = count,
    text = count == 1 and 'Definition' or string.format('Definitions (%d)', count),
    text_hl = config.hl.definitions_header or 'Function',
    border_hl = config.hl.border,
  }
end

--- Build rendering context with all necessary data.
local function build_render_context()
  local config = S.config or {}
  local items = S.filtered_items
  local win_height = vim.api.nvim_win_get_height(S.list_win)
  local win_width = vim.api.nvim_win_get_width(S.list_win)
  local prompt_position = get_prompt_position()

  local win_info = vim.fn.getwininfo(S.list_win)[1]
  local text_offset = win_info and win_info.textoff or 2
  local text_width = win_width - text_offset

  local combo_boost_score_multiplier = config.history and config.history.combo_boost_score_multiplier or 100
  local separator = nil
  local combo_info = nil
  if S.mode ~= 'grep' and not S.suggestion_source then
    combo_info = combo_renderer.detect(
      items,
      file_picker,
      combo_boost_score_multiplier,
      S.next_search_force_combo_boost or config.history.min_combo_count == 0
    )
  end
  S.next_search_force_combo_boost = false

  if combo_info and S.combo_visible then
    separator = {
      idx = combo_info.idx,
      text = combo_info.text,
      text_hl = config.hl.combo_header,
      border_hl = config.hl.border,
    }
    S.combo_initial_cursor = combo_info.idx
  elseif S.mode == 'grep' and not S.suggestion_source then
    separator = definitions_separator(items, config)
  end

  local display_start = 1
  local display_end = #items

  if combo_info and separator and (display_end - display_start + 1) >= win_height then
    if separator.idx == display_start then
      display_end = display_end - 1
    else
      display_start = display_start + 1
    end
  end

  local iter_start, iter_end, iter_step
  if prompt_position == 'bottom' then
    iter_start, iter_end, iter_step = display_end, display_start, -1
  else
    iter_start, iter_end, iter_step = display_start, display_end, 1
  end

  return {
    config = config,
    items = items,
    cursor = S.cursor,
    win_height = win_height,
    win_width = win_width,
    max_path_width = text_width,
    debug_enabled = config and config.debug and config.debug.show_scores,
    prompt_position = prompt_position,
    separator = separator,
    combo_info = combo_info,
    display_start = display_start,
    display_end = display_end,
    iter_start = iter_start,
    iter_end = iter_end,
    iter_step = iter_step,
    renderer = S.suggestion_source and P.get_suggestion_renderer() or S.renderer,
    query = S.query,
    selected_files = S.selected_files,
    selected_items = S.selected_items,
    mode = S.mode,
    format_file_display = format_file_display,
    suggestion_source = S.suggestion_source,
  }
end

--- Place the separator float over its buffer line, following the list scroll;
--- hidden while that line is scrolled out of view.
function M.sync_separator()
  local ctx = S.last_render_ctx
  local line = S.separator_line
  if not (ctx and ctx.separator and line and S.list_win and vim.api.nvim_win_is_valid(S.list_win)) then
    list_separator.hide()
    return
  end

  local topline = vim.fn.line('w0', S.list_win)
  local height = vim.api.nvim_win_get_height(S.list_win)
  if line < topline or line >= topline + height then
    list_separator.hide()
    return
  end

  local arrow = ctx.prompt_position == 'bottom' and '↓' or '↑'
  local list_cfg = vim.api.nvim_win_get_config(S.list_win)
  list_separator.update({
    list_win = S.list_win,
    row = list_cfg.row + line - topline + 1,
    text = arrow .. ' ' .. ctx.separator.text,
    text_hl = ctx.separator.text_hl,
    border_hl = ctx.separator.border_hl,
  })
end

--- Grep pages are taller than the window, so the separator has to follow
--- scrolling that does not re-render (mouse wheel, <C-e>, cursor moves).
local function watch_list_scroll()
  if S.separator_scroll_win == S.list_win then return end
  S.separator_scroll_win = S.list_win
  vim.api.nvim_create_autocmd('WinScrolled', {
    group = vim.api.nvim_create_augroup('fff_list_separator', { clear = true }),
    pattern = tostring(S.list_win),
    callback = function()
      if P.state.active then M.sync_separator() end
    end,
  })
end

local function finalize_render(separator_line, ctx)
  S.separator_line = ctx.separator and separator_line or nil
  M.sync_separator()
  if S.separator_line then watch_list_scroll() end

  if ctx.mode ~= 'grep' and not ctx.suggestion_source then
    scrollbar.render(S.layout, ctx.config, S.list_win, S.pagination, ctx.prompt_position)
  end
end

function M.render_list()
  if not P.state.active then return end

  local ctx = build_render_context()
  if S.mode == 'grep' and #ctx.items == 0 then
    S.line_to_item = {}
    S.item_to_lines = {}
    S.last_render_ctx = nil
    render_grep_empty_state(ctx)
    return
  end

  local item_to_lines, separator_line = list_renderer.render(ctx, S.list_buf, S.list_win, S.ns_id)
  S.item_to_lines = item_to_lines
  S.last_render_ctx = ctx

  local line_to_item = {}
  for item_idx, mapping in pairs(item_to_lines) do
    for line = mapping.first, mapping.last do
      line_to_item[line] = item_idx
    end
  end
  S.line_to_item = line_to_item

  if ctx.prompt_position == 'bottom' then M.scroll_to_bottom() end

  finalize_render(separator_line, ctx)
end

local function rerender_cursor_rows(old_cursor, new_cursor)
  if S.suggestion_source then return false end

  local ctx = S.last_render_ctx
  if not ctx or ctx.items ~= S.filtered_items then return false end
  if ctx.renderer and not ctx.renderer.supports_cursor_rerender then return false end

  local old_lines = S.item_to_lines[old_cursor]
  local new_lines = S.item_to_lines[new_cursor]
  if not old_lines or not new_lines then return false end

  local renderer = ctx.renderer or require('fff.picker_ui.file_renderer')
  local entries = {
    { item_idx = old_cursor, lines = old_lines },
    { item_idx = new_cursor, lines = new_lines },
  }

  for _, entry in ipairs(entries) do
    entry.item = ctx.items[entry.item_idx]
    if not entry.item then return false end

    local line_idx = entry.lines.last
    entry.line = vim.api.nvim_buf_get_lines(S.list_buf, line_idx - 1, line_idx, false)[1]
    if not entry.line then return false end
  end

  ctx.cursor = new_cursor
  for _, entry in ipairs(entries) do
    vim.api.nvim_buf_clear_namespace(S.list_buf, S.ns_id, entry.lines.first - 1, entry.lines.last)
    renderer.apply_highlights(entry.item, ctx, entry.item_idx, S.list_buf, S.ns_id, entry.lines.last, entry.line)
  end

  vim.api.nvim_win_set_cursor(S.list_win, { new_lines.last, 0 })
  return true
end

function M.render_after_cursor_move(old_cursor)
  if old_cursor == S.cursor then return false end
  if old_cursor and rerender_cursor_rows(old_cursor, S.cursor) then
    M.sync_separator()
    return true
  end
  M.render_list()
  return true
end

return M
