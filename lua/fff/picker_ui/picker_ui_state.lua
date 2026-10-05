local M = {}

local utils = require('fff.utils')

local canonicalize_fff_path = utils.canonicalize_fff_path

-- State structure definitions
M.state = {
  -- UI state
  active = false,
  layout = nil,
  input_win = nil,
  input_buf = nil,
  list_win = nil,
  list_buf = nil,
  file_info_win = nil,
  file_info_buf = nil,
  preview_win = nil,
  preview_buf = nil,
  preview_visible = false,

  -- Data state
  items = {},
  filtered_items = {},
  cursor = 1,
  top = 1,
  query = '',
  line_to_item = {},
  item_to_lines = {},
  last_render_ctx = nil,
  location = nil,

  -- Cursor index to restore after the next search completes (set on resume).
  -- Lets the re-search run for fresh results while keeping the saved position.
  pending_restore_cursor = nil,

  -- History cycling state
  history_offset = nil,
  next_search_force_combo_boost = false,

  -- Combo state
  combo_visible = true,
  combo_initial_cursor = nil,

  -- List separator (combo or grep definitions): 1-based buffer line of its gap
  -- row, and the list window whose scrolling repositions it
  separator_line = nil,
  separator_scroll_win = nil,

  -- History cycling state (tracked alongside combo state)
  updating_from_history = false,

  -- Pagination state
  pagination = {
    page_index = 0,
    page_size = 20,
    total_matched = 0,
    prefetch_margin = 5,
    grep_file_offsets = {},
    grep_next_file_offset = 0,
  },

  -- Configuration and mode
  config = nil,
  renderer = nil,
  mode = nil,
  grep_mode = 'plain',
  grep_config = nil,
  grep_regex_fallback_error = nil,

  -- Selection state
  selected_files = {},
  selected_file_order = {},
  selected_items = {},

  -- Cross-mode suggestion state
  suggestion_items = nil,
  suggestion_source = nil,

  -- Preview state
  last_preview_file = nil,
  last_preview_location = nil,
  preview_timer = nil,
  preview_debounce_ms = 10,

  -- Misc
  ns_id = nil,
  last_status_info = nil,
  restore_paste = false,
  current_file_cache = nil,
}

-- Helper function to generate grep item keys
local function grep_item_key(item)
  return string.format('%s:%d:%d', item.relative_path, item.line_number or 1, item.col or 0)
end

-- Reset history-related state
function M.reset_history_state()
  M.state.history_offset = nil
  M.state.next_search_force_combo_boost = false
  M.state.combo_visible = true
  M.state.combo_initial_cursor = nil
  M.state.updating_from_history = false
end

-- Complete state reset (called on close)
function M.reset_state()
  M.state.active = false
  M.state.layout = nil
  M.state.input_win = nil
  M.state.input_buf = nil
  M.state.list_win = nil
  M.state.list_buf = nil
  M.state.file_info_win = nil
  M.state.file_info_buf = nil
  M.state.preview_win = nil
  M.state.preview_buf = nil
  M.state.preview_visible = false

  M.state.items = {}
  M.state.filtered_items = {}
  M.state.cursor = 1
  M.state.top = 1
  M.state.query = ''
  M.state.line_to_item = {}
  M.state.item_to_lines = {}
  M.state.last_render_ctx = nil
  M.state.location = nil
  M.state.pending_restore_cursor = nil

  M.reset_history_state()

  M.state.pagination = {
    page_index = 0,
    page_size = 20,
    total_matched = 0,
    prefetch_margin = 5,
    grep_file_offsets = {},
    grep_next_file_offset = 0,
  }

  M.state.config = nil
  M.state.renderer = nil
  M.state.mode = nil
  M.state.grep_mode = 'plain'
  M.state.grep_config = nil
  M.state.grep_regex_fallback_error = nil

  M.state.selected_files = {}
  M.state.selected_file_order = {}
  M.state.selected_items = {}
  M.state.suggestion_items = nil
  M.state.suggestion_source = nil

  M.state.last_preview_file = nil
  M.state.last_preview_location = nil
  M.state.preview_timer = nil
  M.state.current_file_cache = nil
  M.state.ns_id = nil
  M.state.last_status_info = nil
  M.state.restore_paste = false
  M.state.combo_visible = true
  M.state.combo_initial_cursor = nil
  M.state.updating_from_history = false
end

-- Clear all selections
function M.clear_selections()
  M.state.selected_files = {}
  M.state.selected_file_order = {}
  M.state.selected_items = {}
end

-- Get selected items for quickfix list
function M.get_selected_items()
  local selected = {}

  if M.state.mode == 'grep' then
    -- Grep mode: return selected items
    for _, item in pairs(M.state.selected_items) do
      table.insert(selected, item)
    end
  else
    -- Normal file mode: return selected files
    for relative_path, _ in pairs(M.state.selected_files) do
      local abs_path = canonicalize_fff_path(relative_path)
      if abs_path then table.insert(selected, { relative_path = relative_path }) end
    end
  end

  return selected
end

-- Toggle item selection (called from keymaps via M.toggle_select)
function M.toggle_selection()
  if not M.state.active then return end

  local items = M.state.filtered_items
  if #items == 0 or M.state.cursor > #items then return end

  local item = items[M.state.cursor]
  if not item or not item.relative_path then return end

  local was_selected

  if M.state.mode == 'grep' then
    -- Per-occurrence selection for grep mode
    local key = grep_item_key(item)
    was_selected = M.state.selected_items[key] ~= nil
    if was_selected then
      M.state.selected_items[key] = nil
    else
      M.state.selected_items[key] = item
    end
  else
    -- Per-file selection for normal file mode
    was_selected = M.state.selected_files[item.relative_path]
    if was_selected then
      M.state.selected_files[item.relative_path] = nil
      M.state.selected_file_order = vim.tbl_filter(
        function(path) return path ~= item.relative_path end,
        M.state.selected_file_order
      )
    else
      M.state.selected_files[item.relative_path] = true
      table.insert(M.state.selected_file_order, item.relative_path)
    end
  end

  return was_selected
end

-- Ordered, deduped selected file entries for opening (file mode only).
-- Each entry has the raw fff relative_path and a cwd-relative edit_path.
function M.get_selected_file_entries()
  if not next(M.state.selected_files) then return {} end

  local entries = {}
  local seen = {}

  local function add(relative_path)
    if not relative_path or seen[relative_path] or not M.state.selected_files[relative_path] then return end

    local abs_path = canonicalize_fff_path(relative_path)
    if not abs_path then return end

    seen[relative_path] = true
    table.insert(entries, {
      relative_path = relative_path,
      edit_path = vim.fn.fnamemodify(abs_path, ':.'),
    })
  end

  for _, relative_path in ipairs(M.state.selected_file_order) do
    add(relative_path)
  end
  for relative_path, _ in pairs(M.state.selected_files) do
    add(relative_path)
  end

  return entries
end

return M
