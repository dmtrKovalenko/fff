local M = {}

local preview = require('fff.file_picker.preview')
local utils = require('fff.utils')
local layout = require('fff.layout')
local picker_ui_state = require('fff.picker_ui.picker_ui_state')

local canonicalize_fff_path = utils.canonicalize_fff_path

-- Parent module reference (set by picker_ui.lua during initialization).
---@type table
local P = nil

function M.init(parent_module) P = parent_module end

local S = picker_ui_state.state

local function filename_pos() return layout.resolve_title_pos(S.config and S.config.layout.filename_position) end

function M.close_preview_timer(timer)
  timer = timer or S.preview_timer
  if not timer then return end

  if S.preview_timer == timer then S.preview_timer = nil end
  if timer:is_closing() then return end

  timer:stop()
  timer:close()
end

function M.update_preview_debounced()
  M.close_preview_timer()

  local timer = vim.uv.new_timer()
  S.preview_timer = timer
  timer:start(
    S.preview_debounce_ms,
    0,
    vim.schedule_wrap(function()
      local is_current = S.preview_timer == timer
      M.close_preview_timer(timer)
      if is_current and P.state.active then M.update_preview() end
    end)
  )
end

--- Smart preview update for cursor movement.
function M.update_preview_smart()
  if not S.preview_visible then return end
  if not P.state.active then return end

  local items = S.filtered_items
  if #items == 0 or S.cursor > #items then
    M.update_preview()
    return
  end

  local item = items[S.cursor]
  if not item then
    M.update_preview()
    return
  end

  if S.last_preview_file == item.relative_path then
    M.update_preview()
    return
  end

  M.update_preview_debounced()
end

function M.update_preview_title(item, location)
  if not S.preview_win or not vim.api.nvim_win_is_valid(S.preview_win) then return end

  local relative_path = item.relative_path
  local max_title_width = vim.api.nvim_win_get_width(S.preview_win)

  local suffix = ''
  local is_grep_item = S.mode == 'grep' or S.suggestion_source == 'grep'
  if is_grep_item and location and location.line then suffix = ':' .. tostring(location.line) end

  local display_path = relative_path .. suffix
  local title

  if #display_path + 2 <= max_title_width then
    title = string.format(' %s ', display_path)
  else
    local available_chars = max_title_width - 2

    local filename = vim.fn.fnamemodify(relative_path, ':t') .. suffix
    if available_chars <= 3 then
      title = filename
    else
      if #filename + 5 <= available_chars then
        local normalized_path = vim.fs.normalize(relative_path)
        local path_parts = vim.split(normalized_path, '[/\\]', { plain = false })

        local segments = {}
        for _, part in ipairs(path_parts) do
          if part ~= '' then table.insert(segments, part) end
        end

        segments[#segments] = vim.fn.fnamemodify(relative_path, ':t') .. suffix

        local segments_to_show = { segments[#segments] }
        local current_length = #segments_to_show[1] + 4

        for i = #segments - 1, 1, -1 do
          local segment = segments[i]
          local new_length = current_length + #segment + 1

          if new_length <= available_chars then
            table.insert(segments_to_show, 1, segment)
            current_length = new_length
          else
            break
          end
        end

        if #segments_to_show == #segments then
          title = string.format(' %s ', table.concat(segments_to_show, '/'))
        else
          title = string.format(' ../%s ', table.concat(segments_to_show, '/'))
        end
      else
        local truncated = filename:sub(1, available_chars - 3) .. '...'
        title = string.format(' %s ', truncated)
      end
    end
  end

  vim.api.nvim_win_set_config(S.preview_win, {
    title = title,
    title_pos = filename_pos(),
  })
end

function M.update_preview()
  if not S.preview_visible then return end
  if not P.state.active then return end

  local items = S.filtered_items
  if #items == 0 or S.cursor > #items then
    M.clear_preview()
    S.last_preview_file = nil
    S.last_preview_location = nil
    return
  end

  local item = items[S.cursor]
  if not item then
    M.clear_preview()
    S.last_preview_file = nil
    S.last_preview_location = nil
    return
  end

  local effective_location = S.location

  if not effective_location and S.query and S.query ~= '' then
    local line_str = S.query:match(':(%d+)$')
    if line_str then
      local line_num = tonumber(line_str)
      if line_num and line_num > 0 then
        local l, c = S.query:match(':(%d+):(%d+)$')
        if l then
          effective_location = { line = tonumber(l), col = tonumber(c) }
        else
          effective_location = { line = line_num }
        end
      end
    end
  end

  local is_grep_item = S.mode == 'grep' or S.suggestion_source == 'grep'
  if is_grep_item and item.line_number and item.line_number > 0 then
    effective_location = { line = item.line_number }
    if item.col and item.col > 0 then effective_location.col = item.col + 1 end
    effective_location.grep_query = S.query
    if S.grep_mode == 'fuzzy' and item.match_ranges then effective_location.fuzzy_match_ranges = item.match_ranges end
  end

  local location_changed = not vim.deep_equal(S.last_preview_location, effective_location)

  if S.last_preview_file == item.relative_path and not location_changed then return end

  if S.last_preview_file == item.relative_path and location_changed then
    S.last_preview_location = effective_location and vim.deepcopy(effective_location) or nil
    preview.state.location = effective_location
    if is_grep_item and effective_location and effective_location.line then
      M.update_preview_title(item, effective_location)
    end
    if S.preview_buf and vim.api.nvim_buf_is_valid(S.preview_buf) then
      preview.apply_location_highlighting(S.preview_buf)
    end
    return
  end

  preview.clear()

  S.last_preview_file = item.relative_path
  S.last_preview_location = effective_location and vim.deepcopy(effective_location) or nil

  M.update_preview_title(item, effective_location)

  if S.file_info_buf then
    preview.update_file_info_buffer(item, S.file_info_buf, S.cursor, S.preview_win)
    if S.file_info_win and vim.api.nvim_win_is_valid(S.file_info_win) then
      local rel = item.relative_path or item.path or ''
      pcall(vim.api.nvim_win_set_config, S.file_info_win, { title = ' ' .. rel .. ' ', title_pos = filename_pos() })
    end
  end

  preview.set_preview_window(S.preview_win)
  preview.preview(canonicalize_fff_path(item.relative_path), S.preview_buf, effective_location, item.is_binary)
end

function M.clear_preview()
  if not P.state.active then return end
  if not S.preview_visible then return end

  vim.api.nvim_win_set_config(S.preview_win, {
    title = ' Preview ',
    title_pos = filename_pos(),
  })

  if S.file_info_buf then
    vim.api.nvim_set_option_value('modifiable', true, { buf = S.file_info_buf })
    vim.api.nvim_buf_set_lines(S.file_info_buf, 0, -1, false, {})
    vim.api.nvim_set_option_value('modifiable', false, { buf = S.file_info_buf })
    pcall(vim.api.nvim_buf_clear_namespace, S.file_info_buf, preview.file_info_ns, 0, -1)
  end

  vim.api.nvim_set_option_value('modifiable', true, { buf = S.preview_buf })
  vim.api.nvim_buf_set_lines(S.preview_buf, 0, -1, false, { 'No preview available' })
  vim.api.nvim_set_option_value('modifiable', false, { buf = S.preview_buf })
end

return M
