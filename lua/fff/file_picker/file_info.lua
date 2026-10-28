local M = {}

---@class FFFFileInfoExtmark
---@field row integer
---@field col integer
---@field end_col integer|nil
---@field hl_group string|nil

---@class FFFFileInfoResult
---@field lines string[]
---@field extmarks FFFFileInfoExtmark[]
---@field height integer

---@class FFFFileInfoSections
---@field file_info boolean
---@field score_breakdown boolean
---@field timings boolean
---@field full_path boolean

---@class FFFFileInfoFile
---@field relative_path string
---@field absolute_path string|nil
---@field size_formatted string
---@field filetype string
---@field git_status string
---@field access_frecency_score integer
---@field modification_frecency_score integer
---@field times_opened integer
---@field modified_formatted string
---@field accessed_formatted string

---@class FFFFileInfoScore
---@field total integer
---@field match_type string
---@field base_score integer
---@field filename_bonus integer
---@field special_filename_bonus integer
---@field frecency_boost integer
---@field git_status_boost integer
---@field git_recency_boost integer
---@field combo_match_boost integer
---@field distance_penalty integer
---@field current_file_penalty integer

local Builder = {}
Builder.__index = Builder

function Builder.new() return setmetatable({ lines = {}, extmarks = {} }, Builder) end

function Builder:add_line(text) table.insert(self.lines, text or '') end

function Builder:add_hl(row, col, end_col, hl_group)
  if not hl_group or hl_group == '' then return end
  table.insert(self.extmarks, { row = row, col = col, end_col = end_col, hl_group = hl_group })
end

-- Section header: `─ <label> ` then dashes filling the remaining width.
function Builder:add_section_header(label, hls, width)
  local row = #self.lines
  local prefix = '─ '
  local left_text = prefix .. label .. ' '
  local fill_chars = math.max(1, (width or 0) - vim.fn.strdisplaywidth(left_text))
  local fill = string.rep('─', fill_chars)
  self:add_line(left_text .. fill)
  self:add_hl(row, 0, #prefix, hls.file_info_separator)
  self:add_hl(row, #prefix, #prefix + #label, hls.file_info_section)
  self:add_hl(row, #left_text, #left_text + #fill, hls.file_info_separator)
end

local GIT_HL_KEYS = {
  staged_new = 'git_staged',
  staged_modified = 'git_staged',
  staged_deleted = 'git_staged',
  modified = 'git_modified',
  deleted = 'git_deleted',
  renamed = 'git_renamed',
  untracked = 'git_untracked',
  ignored = 'git_ignored',
}

local function git_hl(hls, status)
  local key = GIT_HL_KEYS[status]
  return (key and hls[key]) or hls.file_info_value
end

local GIT_LABELS = {
  staged_new = 'staged (new)',
  staged_modified = 'staged',
  staged_deleted = 'staged (del)',
  untracked = 'untracked',
  modified = 'modified',
  deleted = 'deleted',
  renamed = 'renamed',
  ignored = 'ignored',
  clean = 'clean',
  clear = '',
  unknown = '?',
}

local function frecency_value(file)
  return string.format('acc %d / mod %d', file.access_frecency_score or 0, file.modification_frecency_score or 0)
end

local function signed(n)
  n = n or 0
  if n > 0 then return '+' .. tostring(n) end
  return tostring(n)
end

-- Pad to `width` cols, truncate with '…' if longer.
local function pad(text, width)
  text = text or ''
  local w = vim.fn.strdisplaywidth(text)
  if w == width then return text end
  if w < width then return text .. string.rep(' ', width - w) end
  if width <= 1 then return text:sub(1, math.max(0, width)) end
  return text:sub(1, width - 1) .. '…'
end

-- 4-cell grid row: [indent][label1][value1][label2][value2]. `value2_max`
-- clamps the trailing cell so the row doesn't overflow.
local function add_grid_row(b, indent_w, label1_w, value1_w, label2_w, cells, hls_by_cell, value2_max)
  local row = #b.lines
  local l1 = pad(cells[1] or '', label1_w)
  local v1 = pad(cells[2] or '', value1_w)
  local l2 = pad(cells[3] or '', label2_w)
  local v2_raw = cells[4] or ''
  local v2 = (value2_max and value2_max > 0) and pad(v2_raw, value2_max) or v2_raw
  b:add_line(string.rep(' ', indent_w) .. l1 .. v1 .. l2 .. v2)
  local pos = indent_w
  if cells[1] and cells[1] ~= '' then b:add_hl(row, pos, pos + #cells[1], hls_by_cell[1]) end
  pos = pos + #l1
  if cells[2] and cells[2] ~= '' then b:add_hl(row, pos, pos + #cells[2], hls_by_cell[2]) end
  pos = pos + #v1
  if cells[3] and cells[3] ~= '' then b:add_hl(row, pos, pos + #cells[3], hls_by_cell[3]) end
  pos = pos + #l2
  if v2 ~= '' then
    local content_len = math.min(#v2_raw, value2_max or math.huge)
    b:add_hl(row, pos, pos + content_len, hls_by_cell[4])
  end
end

-- hardcoded because specifically optimized to make every section look good
local W = {
  indent = 1,
  label1 = 6,
  value1 = 15,
  label2 = 9, -- score/match section trailing label
  file_label2 = 8, -- file overview trailing label ("Opened")
  timings_label = 10,
  timings_inline_width = 62,
}
W.grid_prefix = W.indent + W.label1 + W.value1 + W.label2

local function format_opened(count)
  if count == 0 then return 'never' end
  if count == 1 then return '1 time last 30 days' end
  if count >= 128 then return '128+ times last 30 days' end
  return count .. ' times last 30 days'
end

local function render_file_overview_section(b, file, hls, width)
  local git_status = file.git_status or 'clean'
  local git_value = GIT_LABELS[git_status] or git_status
  if git_value == '' then git_value = 'clean' end
  local opened_count = file.times_opened or 0

  local v2_max = math.max(4, width - W.indent - W.label1 - W.value1 - W.file_label2)
  local function row(label1, value1, hl1, label2, value2, hl2)
    add_grid_row(
      b,
      W.indent,
      W.label1,
      W.value1,
      W.file_label2,
      { label1, value1, label2, value2 },
      { hls.file_info_label, hl1, hls.file_info_label, hl2 },
      v2_max
    )
  end

  row('Size', file.size_formatted or 'N/A', hls.file_info_size, 'Type', file.filetype or 'text', hls.file_info_type)
  row(
    'Git',
    git_value,
    git_hl(hls, git_status),
    'Opened',
    format_opened(opened_count),
    opened_count > 0 and hls.file_info_value or hls.file_info_value_dim
  )
end

local function render_score_section(b, score, file, hls, width)
  local v2_max = math.max(4, width - W.grid_prefix)
  if not score then
    add_grid_row(
      b,
      W.indent,
      W.label1,
      W.value1,
      W.label2,
      { 'Total', 'N/A', '', '' },
      { hls.file_info_label, hls.file_info_value_dim, hls.file_info_label, hls.file_info_value },
      v2_max
    )
    return
  end

  local total_str = tostring(score.total or 0)
  local mt_str = score.match_type or 'unknown'
  local frec_str = file and frecency_value(file) or ''
  local indent = string.rep(' ', W.indent)
  local total_label = 'Total  '
  local frec_label = '  Frecency  '

  local left = indent .. total_label .. total_str .. ' ' .. mt_str
  local right = (frec_str ~= '') and (frec_label .. frec_str) or ''
  local total_w = vim.fn.strdisplaywidth(left) + vim.fn.strdisplaywidth(right)
  if total_w > width then
    local overflow = total_w - width + 1
    local mt_keep = math.max(1, vim.fn.strdisplaywidth(mt_str) - overflow)
    mt_str = mt_str:sub(1, mt_keep) .. '…'
    left = indent .. total_label .. total_str .. ' ' .. mt_str
  end

  local row = #b.lines
  b:add_line(left .. right)
  b:add_hl(row, W.indent, W.indent + 5, hls.file_info_label)
  local total_byte = W.indent + #total_label
  b:add_hl(row, total_byte, total_byte + #total_str, hls.file_info_total_score)
  local mt_byte = total_byte + #total_str + 1
  b:add_hl(row, mt_byte, mt_byte + #mt_str, hls.file_info_match_type)
  if right ~= '' then
    local right_byte = #left
    b:add_hl(row, right_byte + 2, right_byte + 2 + 8, hls.file_info_label)
    b:add_hl(row, right_byte + #frec_label, right_byte + #right, hls.file_info_value)
  end

  local total_pen = (score.distance_penalty or 0) + (score.current_file_penalty or 0)
  local pos_hl = hls.file_info_score_pos
  local neg_hl = hls.file_info_score_neg
  local val_hl = hls.file_info_value
  local function bonus_hl(n) return (n or 0) > 0 and pos_hl or val_hl end
  local function mod_hl(n) return (n or 0) >= 0 and pos_hl or neg_hl end

  local segments = {
    { 'base ' .. (score.base_score or 0), val_hl },
    { '  +name ' .. (score.filename_bonus or 0), bonus_hl(score.filename_bonus) },
    { '  +special ' .. (score.special_filename_bonus or 0), bonus_hl(score.special_filename_bonus) },
    { '  +frec ' .. signed(score.frecency_boost or 0), mod_hl(score.frecency_boost) },
    { '  +recent ' .. signed(score.git_recency_boost or 0), bonus_hl(score.git_recency_boost) },
    { '  +combo ' .. signed(score.combo_match_boost or 0), bonus_hl(score.combo_match_boost) },
    { '  penalty ' .. total_pen, total_pen > 0 and neg_hl or val_hl },
  }

  local row2 = #b.lines
  local line_text = indent
  local hl_ranges = {}
  for _, seg in ipairs(segments) do
    local from = #line_text
    line_text = line_text .. seg[1]
    table.insert(hl_ranges, { from, #line_text, seg[2] })
  end
  if vim.fn.strdisplaywidth(line_text) > width then line_text = line_text:sub(1, width - 1) .. '…' end
  b:add_line(line_text)
  for _, r in ipairs(hl_ranges) do
    if r[1] < #line_text then b:add_hl(row2, r[1], math.min(r[2], #line_text), r[3]) end
  end
end

local function render_score(b, score, file, hls, width)
  b:add_section_header('Score', hls, width)
  render_score_section(b, score, file, hls, width)
end

-- `timings_opt` may be a boolean or `{ modified = bool, accessed = bool }`.
local function render_timings_section(b, file, hls, width, timings_opt)
  local show_modified, show_accessed
  if type(timings_opt) == 'table' then
    show_modified = timings_opt.modified ~= false
    show_accessed = timings_opt.accessed ~= false
  else
    show_modified = true
    show_accessed = true
  end
  if not show_modified and not show_accessed then return end

  b:add_section_header('Timings', hls, width)

  local label_hl = hls.file_info_label
  local value_hl = hls.file_info_value
  local both_inline = show_modified and show_accessed and width >= W.timings_inline_width

  if both_inline then
    add_grid_row(
      b,
      W.indent,
      W.timings_label,
      22,
      W.timings_label,
      { 'Modified', file.modified_formatted or 'N/A', 'Accessed', file.accessed_formatted or 'N/A' },
      { label_hl, value_hl, label_hl, value_hl }
    )
    return
  end

  if show_modified then
    add_grid_row(
      b,
      W.indent,
      W.timings_label,
      19,
      0,
      { 'Modified', file.modified_formatted or 'N/A', '', '' },
      { label_hl, value_hl, label_hl, value_hl }
    )
  end
  if show_accessed then
    add_grid_row(
      b,
      W.indent,
      W.timings_label,
      19,
      0,
      { 'Accessed', file.accessed_formatted or 'N/A', '', '' },
      { label_hl, value_hl, label_hl, value_hl }
    )
  end
end

-- Manual path split — buffer is nowrap so wrap would misalign extmarks.
local function render_path_section(b, file, hls, width)
  b:add_section_header('Path', hls, width)
  local path = file.relative_path or file.absolute_path or ''
  local indent = string.rep(' ', W.indent)
  local content_width = math.max(10, width - W.indent - 1)
  local path_hl = hls.file_info_path
  if #path <= content_width then
    local row = #b.lines
    b:add_line(indent .. path)
    b:add_hl(row, W.indent, W.indent + #path, path_hl)
  else
    local first = path:sub(1, content_width)
    local rest = path:sub(content_width + 1)
    if #rest > content_width then rest = '…' .. rest:sub(-content_width + 1) end
    local r1 = #b.lines
    b:add_line(indent .. first)
    b:add_hl(r1, W.indent, W.indent + #first, path_hl)
    local r2 = #b.lines
    b:add_line(indent .. rest)
    b:add_hl(r2, W.indent, W.indent + #rest, path_hl)
  end
end

---@class FFFFileInfoInput
---@field file FFFFileInfoFile
---@field score FFFFileInfoScore|nil
---@field width integer
---@field sections FFFFileInfoSections
---@field hls table The `hl` block from FffConfig.

---@param input FFFFileInfoInput
---@return FFFFileInfoResult
function M.build(input)
  local width = input.width or 80
  local sections = input.sections or {}
  local hls = input.hls
  local file = input.file
  local b = Builder.new()

  if sections.file_info ~= false then render_file_overview_section(b, file, hls, width) end
  if sections.score_breakdown ~= false then render_score(b, input.score, file, hls, width) end
  if sections.timings ~= false then render_timings_section(b, file, hls, width, sections.timings) end
  if sections.full_path ~= false then render_path_section(b, file, hls, width) end

  return { lines = b.lines, extmarks = b.extmarks, height = #b.lines }
end

--- @param sections table|boolean
--- @param panel_width integer
--- @return integer
function M.calculate_required_height(sections, panel_width)
  if sections == false then return 0 end
  local s = sections
  if type(s) == 'boolean' then
    s = { file_info = s, score_breakdown = s, timings = s, full_path = s }
  elseif type(s) ~= 'table' then
    s = {}
  end

  local total = 0
  if s.file_info ~= false then total = total + 2 end
  if s.score_breakdown ~= false then total = total + 3 end

  local t = s.timings
  if t ~= false and t ~= nil then
    local show_modified, show_accessed
    if type(t) == 'table' then
      show_modified = t.modified ~= false
      show_accessed = t.accessed ~= false
    else
      show_modified = true
      show_accessed = true
    end

    local visible = (show_modified and 1 or 0) + (show_accessed and 1 or 0)
    if visible > 0 then
      local inline = visible == 2 and (panel_width or 0) >= W.timings_inline_width
      total = total + 1 + (inline and 1 or visible)
    end
  end

  if s.full_path ~= false and s.full_path ~= nil then total = total + 2 end

  return total
end

return M
