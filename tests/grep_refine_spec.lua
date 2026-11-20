---@diagnostic disable: undefined-field, missing-fields
vim.g.fff = {
  frecency = { enabled = false, db_path = vim.fn.tempname() },
  history = { enabled = false, db_path = vim.fn.tempname() },
  logging = { enabled = false },
}

local fff_rust = require('fff.rust')
local picker_ui = require('fff.picker_ui.picker_ui')
local S = require('fff.picker_ui.picker_ui_state').state

local C_LINES = 150
local TOTAL_MATCHES = C_LINES + 2

local function norm(p) return vim.fs.normalize(vim.uv.fs_realpath(p) or p):gsub('/$', '') end

local function wait_for(predicate, message) assert.is_true(vim.wait(15000, predicate, 20), message) end

local function prompt_text() return vim.api.nvim_buf_get_lines(S.input_buf, 0, 1, false)[1]:sub(#S.config.prompt + 1) end

local function type_prompt(query)
  vim.api.nvim_buf_set_lines(S.input_buf, 0, -1, false, { S.config.prompt .. query })
  wait_for(function() return S.query == query end, 'prompt change never reached the search')
end

local function start_refine()
  picker_ui.toggle_grep_refine()
  wait_for(function() return S.refine ~= nil and S.query == '' end, 'refine never started')
end

local function create_fixture()
  local root = vim.fn.tempname()
  vim.fn.mkdir(root .. '/b', 'p')
  root = norm(root)

  local function write(rel, content)
    local f = assert(io.open(root .. '/' .. rel, 'w'))
    f:write(content)
    f:close()
  end

  write('a.lua', 'local alpha = needle\nlocal beta = needle\n')
  local c_lines = {}
  for i = 1, C_LINES do
    c_lines[i] = 'needle ' .. i
  end
  write('b/c.lua', table.concat(c_lines, '\n') .. '\n')
  write('d.txt', 'nothing here\n')
  return root
end

describe('grep refine', function()
  -- Shared so resume, which reindexes the base_path the picker opened with, stays on it
  local tmp = create_fixture()

  before_each(function()
    require('fff').live_grep({ cwd = tmp, query = 'needle' })
    wait_for(function()
      local ok, health = pcall(fff_rust.health_check, tmp)
      return ok and health.file_picker.base_path and norm(health.file_picker.base_path) == tmp
    end, 'picker never switched to the fixture')
    fff_rust.wait_for_initial_scan(30000)
    picker_ui.update_results()
    wait_for(function()
      for _, item in ipairs(S.items) do
        if item.relative_path == 'a.lua' then return true end
      end
      return false
    end, 'live grep never found the fixture matches')
  end)

  after_each(function()
    pcall(vim.cmd, 'cclose')
    pcall(picker_ui.close)
    pcall(fff_rust.stop_background_monitor)
    pcall(fff_rust.cleanup_file_picker)
  end)

  it('filters every match of the grep query and returns to live grep', function()
    start_refine()
    assert.are.equal('needle', S.refine.query)
    assert.are.equal(TOTAL_MATCHES, #S.refine.items)
    assert.are.equal('', prompt_text())

    type_prompt('beta')
    assert.are.equal(1, #S.filtered_items)
    assert.are.equal('a.lua', S.filtered_items[1].relative_path)
    assert.are.equal(2, S.filtered_items[1].line_number)

    type_prompt('149')
    assert.are.equal(1, #S.filtered_items)
    assert.are.equal('b/c.lua', S.filtered_items[1].relative_path)
    assert.are.equal(149, S.filtered_items[1].line_number)

    picker_ui.toggle_grep_refine()
    wait_for(function() return S.refine == nil and S.query == 'needle' end, 'live grep never came back')
    assert.are.equal('needle', prompt_text())
  end)

  it('pages through the filtered matches in grep order', function()
    start_refine()
    local page_size = S.pagination.page_size
    assert.is_true(page_size < TOTAL_MATCHES)
    assert.are.equal(page_size, #S.filtered_items)
    assert.are.equal(TOTAL_MATCHES, S.pagination.total_matched)

    assert.is_true(picker_ui.load_next_page())
    assert.are.equal(S.refine.filtered[page_size + 1], S.filtered_items[1])

    local last_line = 0
    for _, item in ipairs(S.refine.items) do
      if item.relative_path == 'b/c.lua' then
        assert.is_true(item.line_number > last_line)
        last_line = item.line_number
      end
    end
    assert.are.equal(C_LINES, last_line)
  end)

  it('needs a query and keeps the grep mode while refining', function()
    type_prompt('')
    picker_ui.toggle_grep_refine()
    assert.is_nil(S.refine)

    type_prompt('needle')
    start_refine()
    local grep_mode = S.grep_mode
    picker_ui.cycle_grep_modes()
    assert.are.equal(grep_mode, S.grep_mode)
  end)

  it('sends the filtered matches to quickfix', function()
    start_refine()
    type_prompt('beta')
    picker_ui.send_to_quickfix()

    local qf = vim.fn.getqflist()
    assert.are.equal(1, #qf)
    assert.are.equal(2, qf[1].lnum)
  end)

  it('resumes into the refined results', function()
    start_refine()
    type_prompt('beta')
    picker_ui.close()
    assert.is_false(S.active)

    picker_ui.resume_live_grep()
    wait_for(function() return S.active and S.refine ~= nil and S.query == 'beta' end, 'resume lost the refine')
    assert.are.equal(1, #S.filtered_items)
    assert.are.equal('a.lua', S.filtered_items[1].relative_path)
  end)
end)
