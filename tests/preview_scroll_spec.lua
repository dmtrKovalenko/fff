---@diagnostic disable: undefined-field, need-check-nil
local location_utils = require('fff.location_utils')
local preview = require('fff.file_picker.preview')

describe('location_utils.get_target_col', function()
  it('extracts 1-indexed column from line:col, range, and fuzzy locations', function()
    assert.are.equal(52, location_utils.get_target_col({ line = 10, col = 52 }))
    assert.are.equal(
      12,
      location_utils.get_target_col({ start = { line = 1, col = 12 }, ['end'] = { line = 1, col = 18 } })
    )
    assert.are.equal(9, location_utils.get_target_col({ line = 2, fuzzy_match_ranges = { { 8, 12 } } }))
    assert.is_nil(location_utils.get_target_col({ line = 10 }))
    assert.is_nil(location_utils.get_target_col(nil))
  end)
end)

describe('preview horizontal scroll', function()
  local buf, win

  local function get_leftcol()
    return vim.api.nvim_win_call(win, function() return vim.fn.winsaveview().leftcol end)
  end

  before_each(function()
    preview.setup({})
    buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, {
      'short line with SSE near start',
      string.rep(' ', 12) .. 'vars(dashboard.curses).pop("BUTTON5_PRESSED", None)',
      string.rep('中文', 25) .. 'SSE_TAIL',
    })
    win = vim.api.nvim_open_win(buf, false, {
      relative = 'editor',
      row = 1,
      col = 1,
      width = 40,
      height = 10,
      style = 'minimal',
    })
    vim.api.nvim_set_option_value('wrap', false, { win = win })
    preview.state.bufnr = buf
    preview.state.winid = win
    preview.state.has_more_content = false
  end)

  after_each(function()
    if win and vim.api.nvim_win_is_valid(win) then vim.api.nvim_win_close(win, true) end
    if buf and vim.api.nvim_buf_is_valid(buf) then vim.api.nvim_buf_delete(buf, { force = true }) end
    preview.state.bufnr = nil
    preview.state.winid = nil
    preview.state.location = nil
  end)

  it('centers an off-screen match column horizontally when wrap is disabled', function()
    preview.state.location = { line = 2, col = 52 }
    preview.apply_location_highlighting(buf)

    local leftcol = get_leftcol()
    assert.are.equal(31, leftcol)
    assert.are.same({ 2, 51 }, vim.api.nvim_win_get_cursor(win))
  end)

  it('keeps leftcol at 0 for matches near the start of the line', function()
    preview.state.location = { line = 1, col = 17 }
    preview.apply_location_highlighting(buf)

    assert.are.equal(0, get_leftcol())
  end)

  it('resets leftcol to 0 when moving from a far match to a near or line-only match', function()
    preview.state.location = { line = 2, col = 52 }
    preview.apply_location_highlighting(buf)
    assert.is_true(get_leftcol() > 0)

    preview.state.location = { line = 1 }
    preview.apply_location_highlighting(buf)
    assert.are.equal(0, get_leftcol())
  end)

  it('uses display width for multibyte characters', function()
    local line3 = vim.api.nvim_buf_get_lines(buf, 2, 3, false)[1]
    local col = line3:find('SSE_TAIL', 1, true)
    preview.state.location = { line = 3, col = col }
    preview.apply_location_highlighting(buf)

    -- 25 CJK pairs = 100 display columns (150 bytes); centered in 40-col window -> leftcol = 80
    assert.are.equal(80, get_leftcol())
  end)

  it('leaves leftcol at 0 when wrap is enabled', function()
    vim.api.nvim_set_option_value('wrap', true, { win = win })
    preview.state.location = { line = 2, col = 52 }
    preview.apply_location_highlighting(buf)

    assert.are.equal(0, get_leftcol())
  end)

  it('clamps out-of-bounds columns without error', function()
    preview.state.location = { line = 1, col = 999 }
    preview.apply_location_highlighting(buf)

    local cursor = vim.api.nvim_win_get_cursor(win)
    assert.are.equal(1, cursor[1])
    assert.are.equal(29, cursor[2])
  end)
end)
