local M = {}

local picker_ui_state = require('fff.picker_ui.picker_ui_state')
local layout_manager = require('fff.picker_ui.layout_manager')
local utils = require('fff.utils')

local canonicalize_fff_path = utils.canonicalize_fff_path

--- Build quickfix list from selections and open it.
--- Handles close, setqflist, copen, and notification.
function M.send_to_quickfix()
  local S = picker_ui_state.state
  if not S.active then return end

  local qf_list = {}

  if S.mode == 'grep' then
    local has_selections = next(S.selected_items) ~= nil

    if has_selections then
      for _, item in pairs(S.selected_items) do
        local abs = canonicalize_fff_path(item.relative_path)
        if abs then
          table.insert(qf_list, {
            filename = abs,
            lnum = item.line_number or 1,
            col = (item.col or 0) + 1,
            text = item.line_content or vim.fn.fnamemodify(abs, ':.'),
          })
        end
      end
    else
      local all_items
      if S.refine then
        all_items = S.refine.filtered
      else
        local grep = require('fff.picker_ui.grep_renderer')
        local exhaustive_config = vim.tbl_extend('force', S.grep_config or {}, { max_matches_per_file = 0 })
        local exhaustive = grep.search(S.query, 0, 10000, exhaustive_config, S.grep_mode)
        all_items = exhaustive and exhaustive.items or {}
      end

      if #all_items == 0 then
        vim.notify('No matches to send to quickfix', vim.log.levels.WARN)
        return
      end

      for _, item in ipairs(all_items) do
        local abs = canonicalize_fff_path(item.relative_path)
        if abs then
          table.insert(qf_list, {
            filename = abs,
            lnum = item.line_number or 1,
            col = (item.col or 0) + 1,
            text = item.line_content or vim.fn.fnamemodify(abs, ':.'),
          })
        end
      end
    end
  else
    local paths = {}

    if next(S.selected_files) then
      for relative_path, _ in pairs(S.selected_files) do
        table.insert(paths, canonicalize_fff_path(relative_path))
      end
    else
      for _, item in ipairs(S.filtered_items) do
        local abs = canonicalize_fff_path(item.relative_path)
        if abs then table.insert(paths, abs) end
      end
    end

    if #paths == 0 then
      vim.notify('No files to send to quickfix', vim.log.levels.WARN)
      return
    end

    for _, path in ipairs(paths) do
      table.insert(qf_list, {
        filename = path,
        lnum = 1,
        col = 1,
        text = vim.fn.fnamemodify(path, ':.'),
      })
    end
  end

  local is_grep = S.mode == 'grep'

  -- Close picker, populate quickfix, open it
  layout_manager.close()

  vim.fn.setqflist(qf_list)
  vim.cmd('copen')

  local count = #qf_list
  local unit = is_grep and (count == 1 and 'match' or 'matches') or (count == 1 and 'file' or 'files')
  vim.notify(string.format('Added %d %s to quickfix list', count, unit), vim.log.levels.INFO)
end

return M
