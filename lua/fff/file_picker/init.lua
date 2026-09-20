--- FFF.nvim File Picker - High-performance file picker for Neovim
--- Uses advanced fuzzy search algorithm with frecency scoring

local M = {}
local fuzzy = require('fff.core').ensure_initialized()

M.state = {
  initialized = false,
  base_path = nil,
  last_scan_time = 0,
}

function M.setup()
  local config = require('fff.conf').get()
  M.state.initialized = true
  M.state.base_path = config.base_path

  return true
end

--- Trigger scan of files in the current directory (asynchronous)
function M.scan_files()
  if not M.state.initialized then return end

  local ok, result = pcall(fuzzy.scan_files)
  if not ok then
    vim.notify('Failed to trigger file scan: ' .. tostring(result), vim.log.levels.ERROR)
    return
  end

  M.state.last_scan_time = os.time()
end

--- Search files with fuzzy matching using blink.cmp's advanced algorithm
--- Results are always returned in descending order (best scores first)
--- @param query string Search query
--- @param max_results number|nil Maximum number of results (optional)
--- @param max_threads number|nil Maximum number of threads (optional)
--- @param current_file string|nil Path to current file to deprioritize (optional)
--- @param min_combo_count_override number|nil Optional override for min_combo_count (nil uses config)
--- @return table List of matching files
function M.search_files(query, current_file, max_results, max_threads, min_combo_count_override)
  -- Delegate to paginated version with offset=0 and limit=max_results
  return M.search_files_paginated(query, current_file, max_threads, min_combo_count_override, 0, max_results)
end

--- Search files with pagination support
--- Results are always returned in descending order (best scores first)
--- @param query string Search query
--- @param current_file string|nil Path to current file to deprioritize (optional)
--- @param max_threads number|nil Maximum number of threads to use
--- @param min_combo_count_override number|nil Optional override for min_combo_count (nil uses config)
--- @param page_index number Page index (0-based: 0, 1, 2, ...)
--- @param page_size number|nil Items per page (nil uses config default)
--- @return table List of matching files
function M.search_files_paginated(query, current_file, max_threads, min_combo_count_override, page_index, page_size)
  local config = require('fff.conf').get()
  if not M.state.initialized then return {} end

  max_threads = max_threads or config.max_threads or 4
  page_index = page_index or 0
  page_size = page_size or 0

  local min_combo_count = min_combo_count_override
  if min_combo_count == nil then min_combo_count = config.history and config.history.min_combo_count or 3 end

  local combo_boost_score_multiplier = config.history and config.history.combo_boost_score_multiplier or 100

  -- Convert page_index to offset (Rust expects offset in items, not page number)
  local offset = page_index * page_size

  local ok, search_result = pcall(
    fuzzy.fuzzy_search_files,
    query,
    max_threads,
    current_file,
    combo_boost_score_multiplier,
    min_combo_count,
    offset,
    page_size
  )

  if not ok then
    vim.notify('Failed to search files: ' .. tostring(search_result), vim.log.levels.ERROR)
    return {}
  end

  M.state.last_search_result = search_result
  return search_result.items
end

--- Get the last search result metadata
--- @return table Search metadata with total_matched and total_files
function M.get_search_metadata()
  if not M.state.last_search_result then return { total_matched = 0, total_files = 0 } end
  return {
    total_matched = M.state.last_search_result.total_matched,
    total_files = M.state.last_search_result.total_files,
  }
end

--- Get location data from the last search result
--- @return table|nil Location data if available
function M.get_search_location()
  if not M.state.last_search_result then return nil end
  return M.state.last_search_result.location
end

--- Get score information for a file by index (1-based)
--- @param index number The index of the file in the last search results
--- @return table|nil Score information or nil if not available
function M.get_file_score(index)
  if not M.state.last_search_result or not M.state.last_search_result.scores then return nil end

  return M.state.last_search_result.scores[index]
end

--- Record file access for frecency tracking
--- @param file_path string Path to the file that was accessed
function M.track_access(file_path)
  if not M.state.initialized then return end

  local ok, result = pcall(fuzzy.track_access, file_path)
  if not ok then vim.notify('Failed to record file access: ' .. tostring(result), vim.log.levels.WARN) end
end

--- Get file content for preview
--- @param file_path string Path to the file
--- @return string|nil File content or nil if failed
function M.get_file_preview(file_path)
  local preview = require('fff.file_picker.preview')

  -- Create a temporary buffer to get the preview
  local temp_buf = vim.api.nvim_create_buf(false, true)
  local success = preview.preview(file_path, temp_buf)

  if not success then
    vim.api.nvim_buf_delete(temp_buf, { force = true })
    return nil
  end

  local lines = vim.api.nvim_buf_get_lines(temp_buf, 0, -1, false)
  vim.api.nvim_buf_delete(temp_buf, { force = true })

  return table.concat(lines, '\n')
end

--- Check if file picker is initialized
--- @return boolean
function M.is_initialized() return M.state.initialized end

--- Get scan progress information
--- @return table Progress information with scanned_files_count, is_scanning, is_warmup_complete, is_index_ready
function M.get_scan_progress()
  if not M.state.initialized then
    return {
      total_files = 0,
      scanned_files_count = 0,
      is_scanning = false,
      is_warmup_complete = false,
      is_index_ready = false,
    }
  end

  local ok, result = pcall(fuzzy.get_scan_progress)
  if not ok then
    vim.notify('Failed to get scan progress: ' .. tostring(result), vim.log.levels.WARN)
    return { scanned_files_count = 0, is_scanning = false, is_warmup_complete = false, is_index_ready = false }
  end

  return result
end

--- Refresh git status on cached files (call after git status loading completes)
--- @return table List of files with updated git status
function M.refresh_git_status()
  if not M.state.initialized then return {} end

  local ok, result = pcall(fuzzy.refresh_git_status)
  if not ok then
    vim.notify('Failed to refresh git status: ' .. tostring(result), vim.log.levels.WARN)
    return {}
  end

  -- Update our cache
  return result
end

--- Stop background git status monitoring
--- @return boolean Success status
function M.stop_background_monitor()
  if not M.state.initialized then return false end

  local ok, result = pcall(fuzzy.stop_background_monitor)
  if not ok then
    vim.notify('Failed to stop background monitor: ' .. tostring(result), vim.log.levels.WARN)
    return false
  end
  return result
end

--- Wait for initial scan to complete
--- @param timeout_ms number Optional timeout in milliseconds (default 5000)
--- @return boolean True if scan completed, false if timed out
function M.wait_for_initial_scan(timeout_ms)
  if not M.state.initialized then return false end

  local ok, result = pcall(fuzzy.wait_for_initial_scan, timeout_ms)
  if not ok then
    vim.notify('Failed to wait for initial scan: ' .. tostring(result), vim.log.levels.WARN)
    return false
  end
  return result
end

--- Get current state
--- @return table
function M.get_state() return M.state end

return M
