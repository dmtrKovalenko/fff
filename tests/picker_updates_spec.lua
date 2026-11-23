local assert = require('luassert')
require('fff.main').setup({ git = { status_updates = 'manual' } })

local ui = require('fff.picker_ui.picker_ui')
local files = require('fff.file_picker')
local search = files.search_files_paginated
local progress = files.get_scan_progress

local function flush()
  local done = false
  vim.schedule(function()
    vim.schedule(function() done = true end)
  end)
  assert.is_true(vim.wait(2000, function() return done end, 1))
end

local function edit(query)
  vim.api.nvim_buf_set_lines(ui.state.input_buf, 0, -1, false, { ui.state.config.prompt .. query })
end

describe('picker search updates', function()
  local queries

  before_each(function()
    files.setup()
    assert.is_true(files.wait_for_initial_scan(10000))
    queries = {}
    files.search_files_paginated = function(query, ...)
      table.insert(queries, query)
      return search(query, ...)
    end
  end)

  after_each(function()
    ui.close()
    files.search_files_paginated = search
    files.get_scan_progress = progress
  end)

  for _, position in ipairs({ 'top', 'bottom' }) do
    it('searches once per open and batch of edits with the prompt at the ' .. position, function()
      ui.open({ query = 'picker', layout = { prompt_position = position } })
      flush()
      assert.same({ 'picker' }, queries)

      edit('co')
      edit('conf')
      edit('config')
      flush()
      assert.same({ 'picker', 'config' }, queries)
      assert.are.equal('config', ui.state.query)
      assert.is_true(#ui.state.items > 1)

      ui.state.cursor = 2
      ui.close()
      ui.resume_find_files()
      flush()
      assert.same({ 'picker', 'config', 'config' }, queries)
      assert.are.equal(2, ui.state.cursor)
    end)
  end

  it('ignores prompt edits queued by a closed picker', function()
    ui.open()
    edit('old')
    ui.close()
    ui.open({ query = 'picker' })
    flush()
    assert.same({ '', 'picker' }, queries)
  end)

  it('searches immediately during indexing and refreshes the latest query when ready', function()
    files.get_scan_progress = function() return { is_scanning = true, scanned_files_count = 0 } end
    ui.open({ query = 'picker' })
    edit('config')
    flush()
    assert.same({ 'picker', 'config' }, queries)

    files.get_scan_progress = progress
    assert.is_true(vim.wait(2000, function() return #queries >= 3 end))
    assert.same({ 'picker', 'config', 'config' }, queries)
  end)
end)
