---@diagnostic disable: undefined-field, missing-fields
local grep = require('fff.picker_ui.grep_renderer')
local fuzzy = require('fff.fuzzy')
local rust = require('fff.rust')

describe('grep classification bridge', function()
  local original, calls, result

  before_each(function()
    original = fuzzy.live_grep
    calls = {}
    result = { items = {}, total_matched = 0, total_files_searched = 7, next_file_offset = 19 }
    fuzzy.live_grep = function(...)
      calls[#calls + 1] = { ... }
      assert.are.equal(1, #calls)
      return result
    end
  end)

  after_each(function() fuzzy.live_grep = original end)

  for _, mode in ipairs({ true, 'classify', false, 'off', 'definition_only', 'usage_only' }) do
    it('uses one call and preserves an empty page for ' .. tostring(mode), function()
      local page = grep.search('needle', 7, 5, { classify_definitions = mode }, 'plain')
      assert.are.equal(result, page)
      assert.are.equal(1, #calls)
      assert.are.equal(7, calls[1][2])
      assert.are.equal(5, calls[1][3])
      assert.are.equal('plain', calls[1][7])
      assert.are.equal(mode == true and 'classify' or (mode == false and 'off' or mode), calls[1][12])
      assert.are.equal(19, grep.get_search_metadata().next_file_offset)
    end)
  end

  it('requests filtering only when the definitions mode is selected', function()
    grep.search('needle', 7, 5, { classify_definitions = true }, 'definition_only')
    assert.are.equal(1, #calls)
    assert.are.equal('plain', calls[1][7])
    assert.are.equal('definition_only', calls[1][12])
  end)
end)

describe('native grep page classification', function()
  local root
  local config = { classify_definitions = true, max_matches_per_file = 0, time_budget_ms = 0 }

  before_each(function()
    root = vim.fn.tempname()
    vim.fn.mkdir(root, 'p')
    for i = 1, 24 do
      vim.fn.writefile(
        { 'let _ = Needle();', 'fn Needle() {}', 'let _ = NeedleExtra();', 'struct NeedleExtra;' },
        root .. '/' .. i .. '.rs'
      )
    end
    -- Parallel spec processes share the default databases, so keep this spec off them.
    vim.g.fff = { frecency = { enabled = false }, history = { enabled = false } }
    require('fff.file_picker').setup()
    rust.cleanup_file_picker()
    rust.init_file_picker(root)
    rust.wait_for_initial_scan(30000)
  end)

  after_each(function()
    rust.stop_background_monitor()
    rust.cleanup_file_picker()
    vim.fn.delete(root, 'rf')
    vim.g.fff = nil
  end)

  for _, mode in ipairs({ 'plain', 'regex', 'fuzzy' }) do
    it('stably regroups each ' .. mode .. ' page without changing search work or cursors', function()
      local offset, seen, count = 0, {}, 0
      repeat
        local ordinary =
          grep.search('Needle', offset, 5, vim.tbl_extend('force', config, { classify_definitions = false }), mode)
        local classified = grep.search('Needle', offset, 5, config, mode)
        assert.are.equal(ordinary.next_file_offset, classified.next_file_offset)
        assert.are.equal(ordinary.total_files_searched, classified.total_files_searched)
        assert.are.equal(ordinary.total_matched, classified.total_matched)
        assert.are.equal(#ordinary.items, #classified.items)
        local expected = {}
        for _, definition in ipairs({ true, false }) do
          for _, item in ipairs(ordinary.items) do
            if (item.line_number == 2 or item.line_number == 4) == definition then
              expected[#expected + 1] = item.relative_path .. ':' .. item.line_number
            end
          end
        end
        for i, item in ipairs(classified.items) do
          local key = item.relative_path .. ':' .. item.line_number
          assert.are.equal(expected[i], key)
          assert.are.equal(item.line_number == 2 or item.line_number == 4, item.is_definition, vim.inspect(item))
          assert.is_nil(seen[key])
          seen[key], count = true, count + 1
        end
        offset = classified.next_file_offset
      until offset == 0
      assert.are.equal(96, count)
    end)
  end

  it('returns a broad keyword page without scanning ahead for definitions', function()
    local ordinary = grep.search('fn', 0, 3, vim.tbl_extend('force', config, { classify_definitions = false }))
    local classified = grep.search('fn', 0, 3, config)
    assert.are.same(ordinary, classified)
    assert.are.equal(3, #classified.items)
    assert.is_true(classified.next_file_offset > 0)
    assert.is_true(classified.total_files_searched < 24)
    for _, item in ipairs(classified.items) do
      assert.is_false(item.is_definition)
    end
  end)

  it('does not look beyond the ordinary per-file limit for definitions', function()
    local page = grep.search('Needle', 0, 3, vim.tbl_extend('force', config, { max_matches_per_file = 1 }))
    assert.are.equal(3, #page.items)
    assert.is_true(page.next_file_offset > 0)
    for _, item in ipairs(page.items) do
      assert.are.equal(1, item.line_number)
      assert.is_false(item.is_definition)
    end
  end)

  it('filters before the per-file limit in explicit definition_only mode', function()
    local offset, seen, count = 0, {}, 0
    repeat
      local page = grep.search(
        'Needle',
        offset,
        3,
        vim.tbl_extend('force', config, { max_matches_per_file = 1 }),
        'definition_only'
      )
      for _, item in ipairs(page.items) do
        assert.is_true(item.is_definition)
        assert.are.equal(2, item.line_number)
        assert.is_nil(seen[item.relative_path])
        seen[item.relative_path], count = true, count + 1
      end
      offset = page.next_file_offset
    until offset == 0
    assert.are.equal(24, count)
    local empty = grep.search('fn', 0, 3, config, 'definition_only')
    assert.are.equal(0, #empty.items)
    assert.are.equal(0, empty.next_file_offset)
  end)
end)
