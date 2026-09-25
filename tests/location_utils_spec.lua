---@diagnostic disable: undefined-field, need-check-nil
local location_utils = require('fff.location_utils')

--- Collect extmarks grouped by hl_group, asserting none use line_hl_group
--- @param buf number Buffer number
--- @param ns number Namespace to read extmarks from
--- @return table<string, table[]> marks Marks keyed by hl_group with row, col and details
local function marks_by_group(buf, ns)
  local by_group = {}
  for _, mark in ipairs(vim.api.nvim_buf_get_extmarks(buf, ns, 0, -1, { details = true })) do
    local details = mark[4]
    assert.is_nil(details.line_hl_group)
    if details.hl_group then
      by_group[details.hl_group] = by_group[details.hl_group] or {}
      table.insert(by_group[details.hl_group], { row = mark[2], col = mark[3], details = details })
    end
  end
  return by_group
end

--- Assert a single full-width CursorLine mark on row, with lower priority than every match
--- @param marks table<string, table[]> Result of marks_by_group
--- @param match_group string Highlight group of the match marks
--- @param row number 0-based row expected to carry the CursorLine mark
local function assert_cursor_line_below(marks, match_group, row)
  local cursor_line = marks.CursorLine[1]
  assert.are.equal(1, #marks.CursorLine)
  assert.are.equal(row, cursor_line.row)
  assert.are.equal(0, cursor_line.col)
  assert.are.equal(row + 1, cursor_line.details.end_row)
  assert.is_true(cursor_line.details.hl_eol)
  assert.are.equal('CursorLineNr', cursor_line.details.number_hl_group)

  for _, match in ipairs(marks[match_group]) do
    assert.is_true(match.details.priority > cursor_line.details.priority)
  end
end

describe('location_utils highlight', function()
  local buf, ns

  before_each(function()
    buf = vim.api.nvim_create_buf(false, true)
    ns = vim.api.nvim_create_namespace('fff_location_utils_spec')
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, { 'hello world', 'abc hello x' })
  end)

  after_each(function() vim.api.nvim_buf_delete(buf, { force = true }) end)

  it('keeps fuzzy grep matches above the pinned cursor line', function()
    location_utils.highlight_location(buf, { grep_query = 'hlo', line = 2, fuzzy_match_ranges = { { 4, 9 } } }, ns)
    local marks = marks_by_group(buf, ns)
    local grep_hl = require('fff.conf').get().hl.grep_match or 'IncSearch'

    assert.are.equal(1, #marks[grep_hl])
    assert.are.equal(4, marks[grep_hl][1].col)
    assert_cursor_line_below(marks, grep_hl, 1)
  end)

  it('keeps line:col highlight above the cursor line', function()
    location_utils.highlight_location(buf, { line = 2, col = 5 }, ns)
    local marks = marks_by_group(buf, ns)

    assert.are.equal(4, marks.IncSearch[1].col)
    assert_cursor_line_below(marks, 'IncSearch', 1)
  end)

  it('keeps single line range highlight above the cursor line', function()
    location_utils.highlight_location(buf, { start = { line = 1, col = 1 }, ['end'] = { line = 1, col = 6 } }, ns)
    local marks = marks_by_group(buf, ns)

    assert.are.equal(0, marks.IncSearch[1].col)
    assert.are.equal(5, marks.IncSearch[1].details.end_col)
    assert_cursor_line_below(marks, 'IncSearch', 0)
  end)
end)
