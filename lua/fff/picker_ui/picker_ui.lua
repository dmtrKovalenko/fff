local M = {}

local conf = require('fff.conf')
local file_picker = require('fff.file_picker')
local preview = require('fff.file_picker.preview')
local utils = require('fff.utils')
local location_utils = require('fff.location_utils')
local layout = require('fff.layout')
local picker_ui_state = require('fff.picker_ui.picker_ui_state')
local picker_ui_utils = require('fff.picker_ui.utils')
local ui_creator = require('fff.picker_ui.ui_creator')
local search_manager = require('fff.picker_ui.search_manager')
local renderer = require('fff.picker_ui.renderer')
local preview_manager = require('fff.picker_ui.preview_manager')
local navigation = require('fff.picker_ui.navigation')
local layout_manager = require('fff.picker_ui.layout_manager')

local canonicalize_fff_path = utils.canonicalize_fff_path

local preview_config = conf.get().preview
if preview_config then preview.setup(preview_config) end

local function get_prompt_position() return layout.resolve_prompt_position(M.state.config) end

-- Wire state from picker_ui_state module
M.state = picker_ui_state.state

-- Alias pure state functions from picker_ui_state
M.clear_selections = picker_ui_state.clear_selections
M.reset_history_state = picker_ui_state.reset_history_state

-- Wire ui_creator module (UI creation, window/buffer/keymap setup)
ui_creator.init(M)
M.create_ui = ui_creator.create_ui
M.setup_buffers = ui_creator.setup_buffers
M.setup_windows = ui_creator.setup_windows
M.setup_keymaps = ui_creator.setup_keymaps
M.focus_list_win = ui_creator.focus_list_win
M.focus_preview_win = ui_creator.focus_preview_win
M.focus_input_win = ui_creator.focus_input_win
M.open_preview = ui_creator.open_preview
M.close_preview = ui_creator.close_preview

-- Wire search_manager module (search, pagination, history)
search_manager.init(M)
M.update_results_sync = search_manager.update_results_sync
M.update_results = search_manager.update_results
M.load_page_at_index = search_manager.load_page_at_index
M.load_next_page = search_manager.load_next_page
M.load_previous_page = search_manager.load_previous_page
M.on_input_change = search_manager.on_input_change
M.cycle_grep_modes = search_manager.cycle_grep_modes
M.recall_query_from_history = search_manager.recall_query_from_history
M.cycle_forward_query = search_manager.cycle_forward_query
M.clear_query = search_manager.clear_query
M.get_suggestion_renderer = search_manager.get_suggestion_renderer

-- Wire renderer module (list rendering, scroll, empty state)
renderer.init(M)
M.render_list = renderer.render_list
M.render_after_cursor_move = renderer.render_after_cursor_move

-- Wire preview_manager module (preview rendering, debounce, clear)
preview_manager.init(M)
M.close_preview_timer = preview_manager.close_preview_timer
M.update_preview_debounced = preview_manager.update_preview_debounced
M.update_preview_smart = preview_manager.update_preview_smart
M.update_preview_title = preview_manager.update_preview_title
M.update_preview = preview_manager.update_preview
M.clear_preview = preview_manager.clear_preview

-- Wire navigation module (cursor movement, preview scroll, pagination wraparound)
navigation.init(M)
M.wrap_to_first = navigation.wrap_to_first
M.wrap_to_last = navigation.wrap_to_last
M.move_up = navigation.move_up
M.move_down = navigation.move_down
M.scroll_preview_up = navigation.scroll_preview_up
M.scroll_preview_down = navigation.scroll_preview_down
M.grep_jump_to_next_file = navigation.grep_jump_to_next_file
M.grep_jump_to_prev_file = navigation.grep_jump_to_prev_file

-- Expose helpers used by navigation
M.scroll_to_bottom = renderer.scroll_to_bottom

-- Wire layout_manager module (relayout, close)
layout_manager.init(M)
M.relayout = layout_manager.relayout

--- @class fff.ResumeState
--- @field files table|nil Snapshot from last find_files session
--- @field grep table|nil Snapshot from last live_grep session
--- @field last_mode 'files'|'grep'|nil Mode of the most recently closed picker
local resume_state = { files = nil, grep = nil, last_mode = nil }

--- Save the current picker state for later resume, then close.
function M.close()
  if M.state.query == '' then
    layout_manager.close()
    return
  end
  if not M.state.active then return end

  local snapshot = vim.deepcopy(M.state)
  snapshot.base_path = M.state.config and M.state.config.base_path or nil

  if M.state.mode == 'grep' then
    resume_state.grep = snapshot
    resume_state.last_mode = 'grep'
  else
    resume_state.files = snapshot
    resume_state.last_mode = 'files'
  end

  layout_manager.close()
end

--- Internal: restore picker from a saved state snapshot.
---@param state table The saved state table
---@param source_label string Label for error messages
---@return boolean
local function restore_from_state(state, source_label)
  -- Ensure the file picker is initialized
  if not file_picker.is_initialized() then
    if not file_picker.setup() then
      vim.notify('Failed to initialize file picker', vim.log.levels.ERROR)
      return false
    end
  end

  -- Restore the picker with the saved config and mode
  M.state.renderer = state.renderer
  M.state.mode = state.mode
  M.state.grep_config = state.grep_config
  M.state.grep_mode = state.grep_mode
  M.state.selected_files = vim.deepcopy(state.selected_files or {})
  M.state.selected_file_order = vim.deepcopy(state.selected_file_order or {})
  M.state.selected_items = vim.deepcopy(state.selected_items or {})

  -- Restore the saved base_path for the indexer if it differs from the current CWD
  if state.base_path then require('fff.core').change_indexing_directory(state.base_path) end

  -- Use the saved config directly to restore the exact picker state
  M.state.config = state.config

  if not M.create_ui() then
    vim.notify('FFF: failed to create picker UI for ' .. source_label, vim.log.levels.ERROR)
    return false
  end

  M.state.active = true
  M.state.current_file_cache = state.current_file_cache

  -- Restore the full picker state
  M.state.query = state.query
  M.state.items = state.items or {}
  M.state.filtered_items = state.filtered_items or {}
  M.state.cursor = math.min(state.cursor or 1, #(state.filtered_items or {}))
  M.state.cursor = math.max(M.state.cursor, 1)
  M.state.location = state.location
  M.state.pagination = vim.deepcopy(state.pagination or {
    page_index = 0,
    page_size = 20,
    total_matched = 0,
    prefetch_margin = 5,
    grep_file_offsets = {},
    grep_next_file_offset = 0,
  })
  M.state.combo_visible = state.combo_visible ~= false
  M.state.combo_initial_cursor = state.combo_initial_cursor
  M.state.suggestion_items = state.suggestion_items
  M.state.suggestion_source = state.suggestion_source

  -- Writing the query below triggers on_lines -> on_input_change, which re-runs
  -- the search (results may have changed since close). Stash the saved cursor so
  -- that re-search restores the position instead of resetting it to the top.
  if state.query and state.query ~= '' then
    M.state.pending_restore_cursor = M.state.cursor
    vim.api.nvim_buf_set_lines(M.state.input_buf, 0, -1, false, { M.state.config.prompt .. state.query })
  end

  -- Render the restored state
  M.render_list()
  M.update_preview()
  M.update_status()

  vim.api.nvim_set_current_win(M.state.input_win)

  -- Position cursor at end of query
  vim.schedule(function()
    if M.state.active and M.state.input_win and vim.api.nvim_win_is_valid(M.state.input_win) then
      local prompt_len = #M.state.config.prompt
      vim.api.nvim_win_set_cursor(M.state.input_win, { 1, prompt_len + #state.query })
      ui_creator.stop_insert()
    end
  end)

  return true
end

--- Close any active picker before resuming so the user can re-trigger
--- resume to recreate the previous results without manually closing first.
local function close_active_for_resume()
  if M.state.active then layout_manager.close() end
end

---@return boolean|nil true if a picker was resumed, false otherwise
function M.resume()
  close_active_for_resume()

  if resume_state.last_mode == 'grep' then
    return M.resume_live_grep()
  elseif resume_state.last_mode == 'files' then
    return M.resume_find_files()
  end

  if resume_state.grep then return restore_from_state(resume_state.grep, 'grep resume') end
  if resume_state.files then return restore_from_state(resume_state.files, 'files resume') end

  return M.open()
end

--- Resume the last file picker (find_files mode).
--- Falls back to opening a new find_files picker if nothing to resume.
---@param opts? table Optional config overrides for fallback open
---@return boolean|nil
function M.resume_find_files(opts)
  close_active_for_resume()

  if not resume_state.files then return M.open(opts) end

  return restore_from_state(resume_state.files, 'find_files resume')
end

--- Resume the last live_grep picker.
--- Falls back to opening a new live_grep picker if nothing to resume.
---@param opts? table Optional config overrides for fallback open
---@return boolean
function M.resume_live_grep(opts)
  close_active_for_resume()

  if not resume_state.grep then
    local config = conf.get()
    local grep_renderer = require('fff.picker_ui.grep_renderer')
    local grep_config = vim.tbl_deep_extend('force', config.grep or {}, (opts and opts.grep) or {})
    M.open(vim.tbl_deep_extend('force', {
      mode = 'grep',
      renderer = grep_renderer,
      grep_config = grep_config,
      title = 'Live Grep',
    }, opts or {}))
    return true
  end

  return restore_from_state(resume_state.grep, 'live_grep resume')
end

function M.toggle_debug()
  local config_changed = conf.toggle_debug()
  if config_changed then
    local current_query = M.state.query
    local current_items = M.state.items
    local current_cursor = M.state.cursor
    -- Preserve mode-specific state across close/open cycle
    local current_mode = M.state.mode
    local current_renderer = M.state.renderer
    local current_grep_mode = M.state.grep_mode
    local current_grep_config = M.state.grep_config
    local current_filtered_items = M.state.filtered_items
    local current_selected_files = M.state.selected_files
    local current_selected_file_order = M.state.selected_file_order
    local current_selected_items = M.state.selected_items

    M.close()
    M.open({
      mode = current_mode,
      renderer = current_renderer,
      grep_config = current_grep_config,
    })

    M.state.query = current_query
    M.state.items = current_items
    M.state.cursor = current_cursor
    M.state.grep_mode = current_grep_mode
    M.state.filtered_items = current_filtered_items
    M.state.selected_files = current_selected_files
    M.state.selected_file_order = current_selected_file_order
    M.state.selected_items = current_selected_items
    M.render_list()
    M.update_preview()
    M.update_status()

    vim.schedule(function()
      if M.state.active and M.state.input_win then
        vim.api.nvim_set_current_win(M.state.input_win)
        vim.cmd('startinsert!')
      end
    end)
  else
    M.update_results()
  end
end

function M.render_debounced()
  vim.schedule(function()
    if M.state.active then
      M.render_list()
      M.update_preview()
      M.update_status()
    end
  end)
end

--- Update status information on the right side of input using virtual text
function M.update_status(progress)
  if not M.state.active or not M.state.ns_id then return end
  local config = M.state.config
  if config == nil then return end

  if M.state.mode == 'grep' then
    -- Determine available modes to decide if we should show the mode indicator
    -- Use grep_config.modes if provided, otherwise fall back to global config
    ---@diagnostic disable-next-line: undefined-field
    local modes = (M.state.grep_config and M.state.grep_config.modes)
      or config.grep.modes
      or { 'plain', 'regex', 'fuzzy' }

    -- When regex compilation failed and we fell back to literal search, show a warning
    local fallback_label = nil
    if M.state.grep_regex_fallback_error then fallback_label = 'invalid regex, using literal' end

    -- If only one mode configured and no fallback error, hide the mode indicator completely
    if #modes <= 1 and not fallback_label then
      -- Clear any existing status and don't show anything
      vim.api.nvim_buf_clear_namespace(M.state.input_buf, M.state.ns_id, 0, -1)
      M.state.last_status_info = nil
      return
    end

    local keybind = config.keymaps.cycle_grep_modes
    -- Normalize: if it's a table of keys, use the first one for display
    if type(keybind) == 'table' then keybind = keybind[1] or '<S-Tab>' end

    local mode_labels = {
      plain = 'plain',
      regex = 'regex',
      fuzzy = 'fuzzy',
    }
    local mode_label = mode_labels[M.state.grep_mode] or 'plain'
    local hl
    if M.state.grep_mode == 'plain' then
      hl = config.hl.grep_plain_active or 'Comment'
    elseif M.state.grep_mode == 'regex' then
      hl = config.hl.grep_regex_active or 'DiagnosticInfo'
    else -- fuzzy
      hl = config.hl.grep_fuzzy_active or 'DiagnosticHint'
    end

    local cache_key = keybind .. M.state.grep_mode .. (fallback_label or '')
    if cache_key == M.state.last_status_info then return end
    M.state.last_status_info = cache_key

    vim.api.nvim_buf_clear_namespace(M.state.input_buf, M.state.ns_id, 0, -1)

    local win_width = vim.api.nvim_win_get_width(M.state.input_win)
    local available_width = win_width - 2

    if fallback_label then
      local total_len = #fallback_label
      local col_position = available_width - total_len
      vim.api.nvim_buf_set_extmark(M.state.input_buf, M.state.ns_id, 0, 0, {
        virt_text = { { fallback_label, 'DiagnosticWarn' } },
        virt_text_win_col = col_position,
      })
    else
      local total_len = #keybind + 1 + #mode_label
      local col_position = available_width - total_len
      vim.api.nvim_buf_set_extmark(M.state.input_buf, M.state.ns_id, 0, 0, {
        virt_text = {
          { keybind .. ' ', hl },
          { mode_label, hl },
        },
        virt_text_win_col = col_position,
      })
    end
    return
  end

  -- File picker mode: show match counts
  local status_info
  if progress and progress.is_scanning then
    status_info = string.format('Indexing files %d', progress.scanned_files_count)
  else
    local search_metadata = file_picker.get_search_metadata()
    if #M.state.query < 2 then
      status_info = string.format('%d', search_metadata.total_files)
    else
      status_info = string.format('%d/%d', search_metadata.total_matched, search_metadata.total_files)
    end
  end

  if status_info == M.state.last_status_info then return end
  M.state.last_status_info = status_info

  vim.api.nvim_buf_clear_namespace(M.state.input_buf, M.state.ns_id, 0, -1)

  local win_width = vim.api.nvim_win_get_width(M.state.input_win)
  local available_width = win_width - 2
  local col_position = available_width - #status_info

  vim.api.nvim_buf_set_extmark(M.state.input_buf, M.state.ns_id, 0, 0, {
    virt_text = { { status_info, 'LineNr' } },
    virt_text_win_col = col_position,
  })
end

--- Check whether the given window has 'winfixbuf' enabled.
--- pcall-guarded so this stays safe on Neovim versions that predate the option.
local window_has_winfixbuf = utils.window_has_winfixbuf

--- Toggle selection for the current item.
--- In grep mode, selection is per-occurrence; in file mode, per-file.
function M.toggle_select()
  if not M.state.active then return end

  local was_selected = picker_ui_state.toggle_selection()

  M.render_list()

  -- only when selecting the element not deselecting
  if not was_selected then
    if get_prompt_position() == 'bottom' then
      M.move_up()
    else
      M.move_down()
    end
  end
end

M.send_to_quickfix = picker_ui_utils.send_to_quickfix

function M.select(action)
  if not M.state.active then return end

  local items = M.state.filtered_items
  if #items == 0 or M.state.cursor > #items then return end

  ---@diagnostic disable-next-line: need-check-nil
  local item = items[M.state.cursor]
  if not item then return end

  action = action or 'edit'

  -- Anchor against the indexer's base_path (may differ from cwd), then rephrase
  -- as cwd-relative for a nicer buffer name when possible. When outside cwd,
  -- fnamemodify(':.') leaves the absolute path intact.
  local abs_path = canonicalize_fff_path(item.relative_path)
  if not abs_path then return end
  local relative_path = vim.fn.fnamemodify(abs_path, ':.')
  local location = M.state.location -- Capture location before closing
  local query = M.state.query -- Capture query before closing for tracking
  local mode = M.state.mode -- Capture mode before closing for tracking
  local suggestion_source = M.state.suggestion_source -- Capture suggestion context
  local config = M.state.config -- Capture config before M.close() resets state

  -- In grep mode (or when selecting a grep suggestion), derive location from the match item
  local is_grep_item = mode == 'grep' or suggestion_source == 'grep'

  -- When opening with selections active, open every selected file. The first
  -- selected file is focused; the rest are added as listed buffers.
  local selected_file_entries = {}
  if action == 'edit' and not is_grep_item then selected_file_entries = picker_ui_state.get_selected_file_entries() end

  if is_grep_item and item.line_number and item.line_number > 0 then
    location = { line = item.line_number }
    if item.col and item.col > 0 then
      location.col = item.col + 1 -- Convert 0-based byte col to 1-based
    end
  end

  -- Fallback: if location is nil but query has a :line suffix, parse it directly
  if not location and query and query ~= '' then
    local line_str = query:match(':(%d+)$')
    if line_str then
      local line_num = tonumber(line_str)
      if line_num and line_num > 0 then
        local col_and_line = query:match(':(%d+):(%d+)$')
        if col_and_line then
          local l, c = query:match(':(%d+):(%d+)$')
          location = { line = tonumber(l), col = tonumber(c) }
        else
          location = { line = line_num }
        end
      end
    end
  end

  -- The focused file is the first selection, which may differ from the cursor
  -- item; a cursor-derived location no longer applies, so drop it.
  if #selected_file_entries > 0 and selected_file_entries[1].relative_path ~= item.relative_path then location = nil end

  vim.cmd('stopinsert')
  M.close()

  local on_submit = config and config.on_submit

  -- Defer file open past picker float teardown. Without this, foldexpr is not
  -- recomputed on the new window (folds appear missing) on some platforms.
  vim.schedule(function()
    if type(on_submit) == 'function' then
      local ok, err = pcall(on_submit, item, {
        action = action,
        path = abs_path,
        relative_path = relative_path,
        location = location,
        query = query,
        mode = mode,
      })
      if not ok then vim.notify('FFF: on_submit error: ' .. tostring(err), vim.log.levels.ERROR) end
    else
      if config and config.select and type(config.select.select_window) == 'function' then
        local ok, win = pcall(config.select.select_window, vim.api.nvim_get_current_buf(), action)
        if not ok then
          vim.notify('FFF: select.select_window error: ' .. tostring(win), vim.log.levels.WARN)
        elseif type(win) == 'number' and vim.api.nvim_win_is_valid(win) then
          vim.api.nvim_set_current_win(win)
        end
      end

      if action == 'edit' then
        -- Add every additional selection as a listed buffer before focusing one.
        for _, entry in ipairs(selected_file_entries) do
          local buf = vim.fn.bufadd(entry.edit_path)
          vim.bo[buf].buflisted = true
        end

        local edit_path = #selected_file_entries > 0 and selected_file_entries[1].edit_path or relative_path

        -- Hard guard against E1513 ("Cannot switch buffer. 'winfixbuf' is enabled"):
        -- if the (post-hook) current window is pinned, fall back to :split.
        local opened_via_split = false
        if window_has_winfixbuf(vim.api.nvim_get_current_win()) then
          vim.cmd('split ' .. vim.fn.fnameescape(edit_path))
          opened_via_split = true
        end

        if not opened_via_split then vim.cmd('edit ' .. vim.fn.fnameescape(edit_path)) end
      elseif action == 'split' then
        vim.cmd('split ' .. vim.fn.fnameescape(relative_path))
      elseif action == 'vsplit' then
        vim.cmd('vsplit ' .. vim.fn.fnameescape(relative_path))
      elseif action == 'tab' then
        vim.cmd('tabedit ' .. vim.fn.fnameescape(relative_path))
      end

      if location then location_utils.jump_to_location(location) end
    end

    if query and query ~= '' then
      local cfg = config or conf.get()
      if cfg.history and cfg.history.enabled then
        local fff = require('fff.core').ensure_initialized()
        -- Track in background thread (non-blocking, handled by Rust)
        if mode == 'grep' then
          pcall(fff.track_grep_query, query)
        elseif #selected_file_entries > 0 then
          for _, entry in ipairs(selected_file_entries) do
            pcall(fff.track_query_completion, query, entry.relative_path)
          end
        else
          pcall(fff.track_query_completion, query, item.relative_path)
        end
      end
    end
  end)
end

--- @return string|nil Current file cache path
local function get_current_file_cache(base_path)
  if not base_path then return nil end
  local current_buf = vim.api.nvim_get_current_buf()
  if not current_buf or not vim.api.nvim_buf_is_valid(current_buf) then return nil end

  local current_file = vim.api.nvim_buf_get_name(current_buf)
  if current_file == '' then return nil end

  -- Use vim.uv.fs_stat to check if file exists and is readable
  local stat = vim.uv.fs_stat(current_file)
  if not stat or stat.type ~= 'file' then return nil end

  local absolute_path = vim.fn.fnamemodify(current_file, ':p')
  local resolved_abs = vim.fn.resolve(absolute_path)
  local resolved_base = vim.fn.resolve(base_path)

  -- icloud direcrtoes on macos contain a lot of special characters that break
  -- the fnamemodify which have to escaped with %
  local escaped_base = resolved_base:gsub('([%%^$()%.%[%]*+%-?])', '%%%1')
  local relative_path = resolved_abs:gsub('^' .. escaped_base .. '/', '')
  if relative_path == '' or relative_path == resolved_abs then return nil end
  return relative_path
end

--- Helper function for common picker initialization
--- @param opts table|nil Options passed to the picker
--- @return table|nil, string|nil Merged configuration and base path, nil config if initialization failed
local function initialize_picker(opts)
  local base_path = opts and opts.cwd or vim.uv.cwd()

  -- Initialize file picker if needed
  if not file_picker.is_initialized() then
    if not file_picker.setup() then
      vim.notify('Failed to initialize file picker', vim.log.levels.ERROR)
      return nil
    end
  end

  local config = conf.get()
  local merged_config = vim.tbl_deep_extend('force', config or {}, opts or {})

  return merged_config, base_path
end

--- Helper function to open UI with optional prefetched results
--- @param query string|nil Pre-filled query (nil for empty)
--- @param results table|nil Pre-fetched results (nil to search normally)
--- @param location table|nil Pre-fetched location data
--- @param merged_config table Merged configuration
--- @param current_file_cache string|nil Current file cache
local function open_ui_with_state(query, results, location, merged_config, current_file_cache)
  M.state.config = merged_config

  if not M.create_ui() then
    vim.notify('Failed to create picker UI', vim.log.levels.ERROR)
    return false
  end

  M.state.active = true
  M.state.current_file_cache = current_file_cache

  -- Set up initial state
  if query then
    M.state.query = query
    vim.api.nvim_buf_set_lines(M.state.input_buf, 0, -1, false, { M.state.config.prompt .. query })
  else
    M.state.query = ''
  end

  if results then
    -- Use prefetched results
    M.state.items = results
    M.state.filtered_items = results
    M.state.cursor = #results > 0 and 1 or 1
    M.state.location = location

    M.render_list()
    M.update_preview()
    M.update_status()
  else
    M.update_results()
    M.clear_preview()
    M.update_status()
  end

  vim.api.nvim_set_current_win(M.state.input_win)

  -- Position cursor at end of query if there is one
  if query then
    vim.schedule(function()
      if M.state.active and M.state.input_win and vim.api.nvim_win_is_valid(M.state.input_win) then
        local line = vim.api.nvim_buf_get_lines(M.state.input_buf, 0, 1, false)[1] or ''
        vim.api.nvim_win_set_cursor(M.state.input_win, { 1, #line })
        vim.cmd('startinsert!')
      end
    end)
  else
    vim.cmd('startinsert!')
  end

  M.monitor_scan_progress(0)

  vim.api.nvim_exec_autocmds('User', { pattern = 'FFFOpen', modeline = false })
  return true
end

--- Execute a search query with callback handling before potentially opening the UI
--- @param query string The search query to execute
--- @param callback function Function called with results: function(results, metadata, location, get_file_score) -> boolean
--- @param opts? table Optional configuration to override defaults (same as M.open)
--- @return boolean true if callback handled results, false if UI was opened
function M.open_with_callback(query, callback, opts)
  if M.state.active then return false end

  -- open_with_callback runs the file-picker flow, never grep. Reset the
  -- renderer/mode/grep_config defensively so we can't inherit stale state
  -- from a previous live_grep session (close() must always do this too,
  -- but belt-and-braces).
  M.state.renderer = nil
  M.state.mode = nil
  M.state.grep_config = nil

  local merged_config, base_path = initialize_picker(opts)
  if not merged_config then return false end

  local current_file_cache = get_current_file_cache(base_path)
  local results = file_picker.search_files(query, current_file_cache, nil, nil, nil)

  local metadata = file_picker.get_search_metadata()
  local location = file_picker.get_search_location()

  local callback_handled = false
  if type(callback) == 'function' then
    local ok, result = pcall(callback, results, metadata, location, file_picker.get_file_score)
    if ok then
      callback_handled = result == true
    else
      vim.notify('Error in search callback: ' .. tostring(result), vim.log.levels.ERROR)
    end
  end

  if callback_handled then return true end
  open_ui_with_state(query, results, location, merged_config, current_file_cache)

  return false
end

--- Open the file picker UI
--- @param opts? {cwd?: string, title?: string, prompt?: string, max_results?: number, max_threads?: number, layout?: {width?: number|function, height?: number|function, prompt_position?: string|function, preview_position?: string|function, preview_size?: number|function}, renderer?: table, mode?: string, grep_config?: table, query?: string} Optional configuration to override defaults
function M.open(opts)
  if M.state.active then return end

  M.state.selected_files = {}
  M.state.selected_file_order = {}
  M.state.selected_items = {}
  M.state.renderer = opts and opts.renderer or nil
  M.state.mode = opts and opts.mode or nil
  M.state.grep_config = opts and opts.grep_config or nil

  local merged_config, base_path = initialize_picker(opts)
  if not merged_config then return end

  if base_path then require('fff.core').change_indexing_directory(base_path) end

  -- Initialize grep_mode to first configured mode when opening in grep mode
  if M.state.mode == 'grep' then
    -- Use grep_config.modes if provided, otherwise fall back to global config
    ---@diagnostic disable-next-line: undefined-field
    local modes = (M.state.grep_config and M.state.grep_config.modes)
      or merged_config.grep.modes
      or { 'plain', 'regex', 'fuzzy' }
    M.state.grep_mode = modes[1] or 'plain'
  end

  local current_file_cache = get_current_file_cache(base_path)
  local query = opts and opts.query or nil ---@type string|nil
  return open_ui_with_state(query, nil, nil, merged_config, current_file_cache)
end

function M.monitor_scan_progress(iteration)
  if not M.state.active then return end

  local progress = file_picker.get_scan_progress()

  if progress.is_scanning then
    M.update_status(progress)

    -- progressive decay for larger directories
    local timeout
    if iteration < 10 then
      timeout = 100
    elseif iteration < 20 then
      timeout = 300
    else
      timeout = 500
    end

    vim.defer_fn(function() M.monitor_scan_progress(iteration + 1) end, timeout)
  else
    M.update_results()
  end
end

return M
