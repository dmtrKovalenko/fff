local M = {}

local grep = require('fff.picker_ui.grep_renderer')
local picker_ui_state = require('fff.picker_ui.picker_ui_state')

local S = picker_ui_state.state

-- Same cap as the exhaustive quickfix search
local MAX_MATCHES = 10000

---@class fff.grep.RefineState
---@field query string Grep query whose matches were collected
---@field items fff.grep.Match[] Every collected match
---@field haystack {text: string, idx: number}[] Fuzzy match targets, one per item
---@field filtered fff.grep.Match[] Items matching the current prompt
---@field truncated boolean True when collection stopped at the match cap

local collect_matches

--- Collect every match of the current grep query so the prompt can filter them.
---@return boolean started False when there is nothing to refine
function M.start()
  if S.mode ~= 'grep' or S.refine or S.query == '' or S.suggestion_source then return false end

  local items, truncated = collect_matches(S.query)
  if #items == 0 then return false end

  local haystack = {}
  for i, item in ipairs(items) do
    local content = item.is_binary_content and '' or (item.line_content or '')
    haystack[i] = { text = content .. ' ' .. (item.relative_path or ''), idx = i }
  end

  S.refine = { query = S.query, items = items, haystack = haystack, filtered = items, truncated = truncated }
  return true
end

--- Drop the collected matches.
---@return string query The grep query to search again
function M.stop()
  local query = S.refine and S.refine.query or ''
  S.refine = nil
  return query
end

--- Fuzzy filter the collected matches, keeping grep order.
---@param query string
function M.filter(query)
  local refine = S.refine
  if not refine then return end
  if query == '' then
    refine.filtered = refine.items
    return
  end

  local matched = vim.fn.matchfuzzy(refine.haystack, query, { key = 'text' })
  local indices = {}
  for i, entry in ipairs(matched) do
    indices[i] = entry.idx
  end
  table.sort(indices)

  local filtered = {}
  for i, idx in ipairs(indices) do
    filtered[i] = refine.items[idx]
  end
  refine.filtered = filtered
end

--- A page of filtered matches shaped like a grep search result.
---@param offset number Index of the first match on the page (0-based)
---@param page_size number
---@return {items: fff.grep.Match[], total_matched: number, next_file_offset: number}
function M.page(offset, page_size)
  local filtered = S.refine and S.refine.filtered or {}
  local items = {}
  for i = offset + 1, math.min(offset + page_size, #filtered) do
    items[#items + 1] = filtered[i]
  end

  local next_offset = offset + page_size
  if next_offset >= #filtered then next_offset = 0 end

  return { items = items, total_matched = #filtered, next_file_offset = next_offset }
end

collect_matches = function(query)
  local config = vim.tbl_extend('force', S.grep_config or require('fff.conf').get().grep, { max_matches_per_file = 0 })
  local definitions, others = {}, {}
  local offset, count = 0, 0

  repeat
    local result = grep.search(query, offset, MAX_MATCHES - count, config, S.grep_mode)
    for _, item in ipairs(result.items or {}) do
      table.insert(item.is_definition and definitions or others, item)
      count = count + 1
    end
    offset = result.next_file_offset or 0
  until offset == 0 or count >= MAX_MATCHES

  -- Each grep page lists its definitions first; keep that order across pages
  vim.list_extend(definitions, others)
  return definitions, offset ~= 0
end

return M
