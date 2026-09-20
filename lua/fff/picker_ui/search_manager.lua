local M = {}

local file_picker = require('fff.file_picker')
local grep = require('fff.picker_ui.grep_renderer')
local picker_ui_state = require('fff.picker_ui.picker_ui_state')

-- Parent module reference (set by picker_ui.lua during initialization).
-- Allows search_manager functions to call back into the main picker module.
---@type table
local P = nil

function M.init(parent_module) P = parent_module end

-- Convenience alias
local S = picker_ui_state.state

function M.update_results() M.update_results_sync() end

function M.update_results_sync()
  if not P.state.active then return end

  if not S.current_file_cache then
    local current_buf = vim.api.nvim_get_current_buf()
    if current_buf and vim.api.nvim_buf_is_valid(current_buf) then
      local current_file = vim.api.nvim_buf_get_name(current_buf)
      S.current_file_cache = (current_file ~= '' and vim.fn.filereadable(current_file) == 1) and current_file or nil
    end
  end

  local page_size
  if S.list_win and vim.api.nvim_win_is_valid(S.list_win) then
    page_size = vim.api.nvim_win_get_height(S.list_win)
  else
    page_size = S.config.max_results or 100
  end

  S.pagination.page_size = page_size
  S.pagination.page_index = 0
  S.combo_visible = true
  S.combo_initial_cursor = 1

  local min_combo_override = nil
  if S.next_search_force_combo_boost then min_combo_override = 0 end

  local results
  if S.mode == 'grep' then
    S.grep_regex_fallback_error = nil
    if S.query == '' then
      results = {}
      S.pagination.total_matched = 0
      S.pagination.grep_file_offsets = {}
      S.pagination.grep_next_file_offset = 0
    else
      local grep_result = grep.search(S.query, 0, page_size, S.grep_config, S.grep_mode)
      results = grep_result.items or {}
      S.pagination.total_matched = grep_result.total_matched or 0
      S.pagination.grep_file_offsets = { 0 }
      S.pagination.grep_next_file_offset = grep_result.next_file_offset or 0
      S.grep_regex_fallback_error = grep_result.regex_fallback_error or nil
      if grep_result.next_file_offset and grep_result.next_file_offset > 0 then
        S.pagination.grep_file_offsets[2] = grep_result.next_file_offset
      end
    end
    S.location = nil
  else
    results = file_picker.search_files_paginated(
      S.query,
      S.current_file_cache,
      S.config.max_threads,
      min_combo_override,
      0,
      page_size
    )
    S.location = file_picker.get_search_location()
    local metadata = file_picker.get_search_metadata()
    S.pagination.total_matched = metadata.total_matched
  end

  S.items = results
  S.filtered_items = results

  S.suggestion_items = nil
  S.suggestion_source = nil
  local suggestions_enabled = (S.config.suggestions or {}).enabled ~= false
  if #results == 0 and S.query ~= '' and suggestions_enabled then
    if S.mode == 'grep' then
      local suggestion_results =
        file_picker.search_files_paginated(S.query, S.current_file_cache, S.config.max_threads, nil, 0, page_size)
      if suggestion_results and #suggestion_results > 0 then
        S.suggestion_items = suggestion_results
        S.suggestion_source = 'files'
      end
    else
      local grep_items = M.suggestion_grep(page_size)
      if #grep_items > 0 then
        S.suggestion_items = grep_items
        S.suggestion_source = 'grep'
      end
    end
  end

  if S.suggestion_items and #S.suggestion_items > 0 then S.filtered_items = S.suggestion_items end

  -- On resume, restore the saved cursor onto the fresh results (clamped, since
  -- the result set may have changed since close). Otherwise reset to the top.
  if S.pending_restore_cursor then
    S.cursor = math.max(1, math.min(S.pending_restore_cursor, #S.filtered_items))
    S.pending_restore_cursor = nil
  else
    S.cursor = 1
  end

  P.render_debounced()
end

-- Hint only: hard time budget (a zero-hit query would otherwise scan every file)
-- and skipped unless the bigram index is built, since without it grep is a cold disk scan.
---@param page_size number
---@return table[]
function M.suggestion_grep(page_size)
  if not file_picker.get_scan_progress().is_index_ready then return {} end
  local suggest_config = S.config.suggestions or {}

  local grep_config = vim.tbl_extend('force', S.grep_config or S.config.grep or {}, {
    enforce_time_budget = true,
    time_budget_ms = suggest_config.grep_time_budget_ms or 50,
  })
  local ok, grep_result = pcall(grep.search, S.query, 0, page_size, grep_config, 'plain')
  if not ok or not grep_result then return {} end
  return grep_result.items or {}
end

function M.load_page_at_index(new_page_index, adjust_cursor_fn)
  local ok, err, results
  local page_size = S.pagination.page_size

  if page_size == 0 then return false end
  if S.mode ~= 'grep' then
    local total = S.pagination.total_matched
    if total == 0 then return false end

    local max_page_index = math.max(0, math.ceil(total / page_size) - 1)
    new_page_index = math.max(0, math.min(new_page_index, max_page_index))
  end

  if S.mode == 'grep' then
    local file_offset = S.pagination.grep_file_offsets[new_page_index + 1]
    if file_offset == nil then return false end

    ok, results = pcall(grep.search, S.query, file_offset, page_size, S.grep_config, S.grep_mode)
    if ok and results then
      local grep_result = results
      results = grep_result.items or {}
      S.pagination.total_matched = grep_result.total_matched or 0
      S.pagination.grep_next_file_offset = grep_result.next_file_offset or 0
      S.grep_regex_fallback_error = grep_result.regex_fallback_error or nil

      if grep_result.next_file_offset and grep_result.next_file_offset > 0 then
        S.pagination.grep_file_offsets[new_page_index + 2] = grep_result.next_file_offset
      end
    end
  else
    ok, results = pcall(
      file_picker.search_files_paginated,
      S.query,
      S.current_file_cache,
      S.config.max_threads,
      nil,
      new_page_index,
      page_size
    )
  end

  if not ok then
    vim.notify('Error in paginated search: ' .. tostring(results), vim.log.levels.ERROR)
    return false
  end

  if #results == 0 then return false end

  if S.mode ~= 'grep' then
    local metadata = file_picker.get_search_metadata()
    S.pagination.total_matched = metadata.total_matched
  end

  S.items = results
  S.filtered_items = results
  S.pagination.page_index = new_page_index

  if adjust_cursor_fn then
    local cursor_ok, cursor_err = pcall(adjust_cursor_fn, #results)
    if not cursor_ok then
      vim.notify('Error in cursor adjustment: ' .. tostring(cursor_err), vim.log.levels.ERROR)
      return false
    end
  end

  ok, err = pcall(P.render_list)
  if not ok then
    vim.notify('Error in render_list: ' .. tostring(err), vim.log.levels.ERROR)
    return false
  end

  ok, err = pcall(P.update_preview)
  if not ok then
    vim.notify('Error in update_preview: ' .. tostring(err), vim.log.levels.ERROR)
    return false
  end

  ok, err = pcall(P.update_status)
  if not ok then
    vim.notify('Error in update_status: ' .. tostring(err), vim.log.levels.ERROR)
    return false
  end
  return true
end

function M.load_next_page()
  local page_size = S.pagination.page_size
  local current_page = S.pagination.page_index

  if page_size == 0 then return false end

  if S.mode == 'grep' then
    if S.pagination.grep_next_file_offset == 0 then return false end
    return M.load_page_at_index(current_page + 1, function() S.cursor = 1 end)
  end

  local total = S.pagination.total_matched
  if total == 0 then return false end

  local max_page_index = math.max(0, math.ceil(total / page_size) - 1)
  if current_page >= max_page_index then return false end

  return M.load_page_at_index(current_page + 1, function() S.cursor = 1 end)
end

function M.load_previous_page()
  if S.pagination.page_index == 0 then return false end
  return M.load_page_at_index(S.pagination.page_index - 1, function(result_count) S.cursor = result_count end)
end

function M.on_input_change()
  if not P.state.active then return end

  local lines = vim.api.nvim_buf_get_lines(S.input_buf, 0, -1, false)
  local prompt_len = #S.config.prompt
  local query = ''

  if #lines > 1 then
    local all_text = table.concat(lines, '')
    if all_text:sub(1, prompt_len) == S.config.prompt then
      query = all_text:sub(prompt_len + 1)
    else
      query = all_text
    end

    query = query:gsub('\r', ''):match('^%s*(.-)%s*$') or ''

    vim.api.nvim_set_option_value('modifiable', true, { buf = S.input_buf })
    vim.api.nvim_buf_set_lines(S.input_buf, 0, -1, false, { S.config.prompt .. query })

    vim.schedule(function()
      if P.state.active and S.input_win and vim.api.nvim_win_is_valid(S.input_win) then
        vim.api.nvim_win_set_cursor(S.input_win, { 1, prompt_len + #query })
      end
    end)
  else
    local full_line = lines[1] or ''
    if full_line:sub(1, prompt_len) == S.config.prompt then query = full_line:sub(prompt_len + 1) end
  end

  S.query = query

  M.update_results_sync()
end

function M.cycle_grep_modes()
  if not P.state.active or S.mode ~= 'grep' then return end

  ---@diagnostic disable-next-line: undefined-field
  local modes = (S.grep_config and S.grep_config.modes) or S.config.grep.modes or { 'plain', 'regex', 'fuzzy' }

  if #modes <= 1 then return end

  local current_idx = 1
  for i, m in ipairs(modes) do
    if m == S.grep_mode then
      current_idx = i
      break
    end
  end
  S.grep_mode = modes[(current_idx % #modes) + 1]

  if S.grep_mode ~= 'regex' then S.grep_regex_fallback_error = nil end

  S.last_status_info = nil
  P.update_status()

  if S.query ~= '' then M.update_results_sync() end
end

function M.recall_query_from_history()
  if not P.state.active then return end

  if S.history_offset == nil then
    S.history_offset = 0
  else
    S.history_offset = S.history_offset + 1
  end

  local fuzzy = require('fff.core').ensure_initialized()
  local history_fn = S.mode == 'grep' and fuzzy.get_historical_grep_query or fuzzy.get_historical_query
  local ok, query = pcall(history_fn, S.history_offset)

  if not ok or not query then
    S.history_offset = 0
    ok, query = pcall(history_fn, 0)

    if not ok or not query then
      vim.notify('No query history available', vim.log.levels.INFO)
      S.history_offset = nil
      return
    end
  end

  if S.mode ~= 'grep' then S.next_search_force_combo_boost = true end

  vim.api.nvim_buf_set_lines(S.input_buf, 0, -1, false, { S.config.prompt .. query })

  vim.schedule(function()
    if P.state.active and S.input_win and vim.api.nvim_win_is_valid(S.input_win) then
      vim.api.nvim_win_set_cursor(S.input_win, { 1, #S.config.prompt + #query })
    end
  end)
end

function M.clear_query()
  if not P.state.active then return end

  S.history_offset = nil
  vim.api.nvim_buf_set_lines(S.input_buf, 0, -1, false, { S.config.prompt })

  vim.schedule(function()
    if P.state.active and S.input_win and vim.api.nvim_win_is_valid(S.input_win) then
      vim.api.nvim_win_set_cursor(S.input_win, { 1, #S.config.prompt })
    end
  end)
end

function M.cycle_forward_query()
  if not P.state.active then return end

  if S.history_offset == nil then
    -- At top of stack (fresh open or resume with pre-filled input).
    -- Clear input to return to a clean slate.
    S.history_offset = nil
    vim.api.nvim_buf_set_lines(S.input_buf, 0, -1, false, { S.config.prompt })
    return
  elseif S.history_offset == 0 then
    -- At the most recent history entry, go back to present
    S.history_offset = nil
    vim.api.nvim_buf_set_lines(S.input_buf, 0, -1, false, { S.config.prompt })
    return
  else
    S.history_offset = S.history_offset - 1
  end

  local fuzzy = require('fff.core').ensure_initialized()
  local history_fn = S.mode == 'grep' and fuzzy.get_historical_grep_query or fuzzy.get_historical_query
  local ok, query = pcall(history_fn, S.history_offset)

  if not ok or not query then
    S.history_offset = nil
    return
  end

  if S.mode ~= 'grep' then S.next_search_force_combo_boost = true end

  vim.api.nvim_buf_set_lines(S.input_buf, 0, -1, false, { S.config.prompt .. query })

  vim.schedule(function()
    if P.state.active and S.input_win and vim.api.nvim_win_is_valid(S.input_win) then
      vim.api.nvim_win_set_cursor(S.input_win, { 1, #S.config.prompt + #query })
    end
  end)
end

function M.get_suggestion_renderer()
  if S.suggestion_source == 'grep' then
    return require('fff.picker_ui.grep_renderer')
  else
    return require('fff.picker_ui.file_renderer')
  end
end

return M
