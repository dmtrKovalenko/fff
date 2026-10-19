---@diagnostic disable: undefined-field
local fff = require('fff')
local rust = require('fff.rust')
local fuzzy = require('fff.fuzzy')
local core = require('fff.core')
local grep

describe('go_to_definition', function()
  local root, original_cwd, fallback
  local live_grep, search, get_scan_progress, health_check, realpath

  before_each(function()
    original_cwd = vim.fn.getcwd()
    root = vim.fn.tempname()
    vim.fn.mkdir(root, 'p')
    vim.fn.writefile({
      'mod nested {',
      '    fn UniqueTarget() {}',
      '}',
      'fn RepeatedTarget() {}',
      'fn RepeatedTargetExtra() {}',
      'fn test_partial_name() {}',
    }, root .. '/definitions.rs')
    vim.fn.writefile({ 'UniqueTarget();', 'RepeatedTarget();', 'NoDefinition();' }, root .. '/usage.rs')
    vim.cmd('noautocmd cd ' .. vim.fn.fnameescape(root))
    local config = require('fff.conf').get()
    config.base_path = root
    config.frecency.enabled = false
    config.history.enabled = false
    config.frecency.db_path = root .. '/frecency'
    config.history.db_path = root .. '/history'
    config.logging.enabled = false
    config.grep.classify_definitions = false
    config.grep.trim_whitespace = true
    config.grep.max_matches_per_file = 1
    core.mark_file_picker_uninitialized()
    core.ensure_initialized()
    rust.wait_for_initial_scan(30000)
    assert.is_true(fuzzy.get_scan_progress().is_index_ready)
    grep = require('fff.picker_ui.grep_renderer')
    vim.cmd('edit usage.rs')
    vim.api.nvim_win_set_cursor(0, { 1, 0 })
    fallback = nil
    live_grep, search, get_scan_progress = fff.live_grep, grep.search, fuzzy.get_scan_progress
    health_check, realpath = fuzzy.health_check, vim.uv.fs_realpath
    fff.live_grep = function(opts) fallback = opts end
  end)

  after_each(function()
    fff.live_grep, grep.search, fuzzy.get_scan_progress = live_grep, search, get_scan_progress
    fuzzy.health_check, vim.uv.fs_realpath = health_check, realpath
    vim.wo.winfixbuf = false
    vim.cmd('only!')
    vim.cmd('noautocmd %bwipeout!')
    rust.stop_background_monitor()
    rust.cleanup_file_picker()
    rust.destroy_frecency_db()
    rust.destroy_query_db()
    core.mark_file_picker_uninitialized()
    vim.cmd('noautocmd cd ' .. vim.fn.fnameescape(original_cwd))
    vim.fn.delete(root, 'rf')
  end)

  it('jumps from a use to the only declaration with an untrimmed byte column', function()
    assert.is_true(fff.go_to_definition())
    assert.is_nil(fallback)
    assert.are.equal('definitions.rs', vim.fn.expand('%:t'))
    assert.are.same({ 2, 7 }, vim.api.nvim_win_get_cursor(0))
    vim.cmd.normal({ vim.api.nvim_replace_termcodes('<C-o>', true, false, true), bang = true })
    assert.are.equal('usage.rs', vim.fn.expand('%:t'))
    assert.are.same({ 1, 0 }, vim.api.nvim_win_get_cursor(0))
  end)

  it('keeps unsaved changes when the definition is in the current buffer', function()
    vim.cmd('edit definitions.rs')
    vim.api.nvim_buf_set_lines(0, -1, -1, false, { '// unsaved' })
    assert.is_true(fff.go_to_definition({ query = 'UniqueTarget' }))
    assert.is_true(vim.bo.modified)
    assert.are.equal('// unsaved', vim.api.nvim_buf_get_lines(0, -2, -1, false)[1])
    assert.are.same({ 2, 7 }, vim.api.nvim_win_get_cursor(0))
  end)

  it('reuses the active index without resolving roots or running health checks', function()
    local other = root .. '/other'
    local expected = realpath(root) .. '/definitions.rs'
    vim.fn.mkdir(other, 'p')
    vim.cmd('noautocmd lcd ' .. vim.fn.fnameescape(other))
    fuzzy.health_check = function() error('navigation must not run health checks') end
    vim.uv.fs_realpath = function(_path, _callback) error('default navigation must not resolve roots') end
    assert.is_true(fff.go_to_definition())
    assert.is_true(fff.go_to_definition({ query = 'UniqueTarget' }))
    assert.are.equal(expected, vim.api.nvim_buf_get_name(0))
    assert.are.same({ 2, 7 }, vim.api.nvim_win_get_cursor(0))
  end)

  it('opens another window when the source window has winfixbuf', function()
    local source = vim.api.nvim_get_current_win()
    vim.wo.winfixbuf = true
    assert.is_true(fff.go_to_definition())
    assert.are_not.equal(source, vim.api.nvim_get_current_win())
    assert.are.equal('usage.rs', vim.fn.fnamemodify(vim.api.nvim_buf_get_name(vim.api.nvim_win_get_buf(source)), ':t'))
    assert.are.equal('definitions.rs', vim.fn.expand('%:t'))
  end)

  it('detects two declarations in one file despite a configured limit of one', function()
    local opts = { query = 'RepeatedTarget', grep = { modes = { 'fuzzy' } } }
    local saved = vim.deepcopy(opts)
    assert.is_false(fff.go_to_definition(opts))
    assert.are.equal('RepeatedTarget', fallback.query)
    assert.are.same({ 'definition_only' }, fallback.grep.modes)
    assert.are.equal('definition_only', fallback.grep.classify_definitions)
    assert.are.same(saved, opts)
    assert.are.equal('usage.rs', vim.fn.expand('%:t'))
  end)

  it('opens the definition picker when only usages match', function()
    assert.is_false(fff.go_to_definition({ query = 'NoDefinition' }))
    assert.are.equal('NoDefinition', fallback.query)
    assert.are.same({ 'definition_only' }, fallback.grep.modes)
  end)

  it('allows a query to match part of the declared name', function()
    assert.is_true(fff.go_to_definition({ query = 'partial_name' }))
    assert.are.same({ 6, 8 }, vim.api.nvim_win_get_cursor(0))
  end)

  it('does not treat one result on an incomplete page as unique', function()
    grep.search = function(...)
      local result = search(...)
      assert.are.equal(1, #result.items)
      result.next_file_offset = 1
      return result
    end
    assert.is_false(fff.go_to_definition())
    assert.are.equal('UniqueTarget', fallback.query)
    assert.are.equal('usage.rs', vim.fn.expand('%:t'))
  end)

  it('opens the picker without probing a partial index', function()
    fuzzy.get_scan_progress = function() return { is_index_ready = false } end
    grep.search = function() error('partial index must not be searched') end
    assert.is_false(fff.go_to_definition())
    assert.are.equal('UniqueTarget', fallback.query)
  end)

  it('does not jump if the searched index was incomplete despite the earlier readiness check', function()
    grep.search = function(...)
      local result = search(...)
      result.is_index_ready = false
      return result
    end
    assert.is_false(fff.go_to_definition())
    assert.are.equal('usage.rs', vim.fn.expand('%:t'))
  end)

  it('jumps for an explicit cwd that matches the active index', function()
    assert.is_true(fff.go_to_definition({ cwd = root }))
    assert.are.equal('definitions.rs', vim.fn.expand('%:t'))
  end)

  it('opens the picker for a different root without searching the old index', function()
    local other = root .. '/other'
    vim.fn.mkdir(other, 'p')
    grep.search = function() error('the old root must not be searched') end
    assert.is_false(fff.go_to_definition({ cwd = other }))
    assert.are.equal(vim.uv.fs_realpath(other), fallback.cwd)
    assert.are.equal('usage.rs', vim.fn.expand('%:t'))
  end)

  it('opens an empty definition picker when there is no word under the cursor', function()
    assert.is_false(fff.go_to_definition({ query = '' }))
    assert.are.equal('', fallback.query)
  end)

  it('does not jump to the old index while a cwd change is pending', function()
    local other = root .. '/other'
    vim.fn.mkdir(other, 'p')
    require('fff.conf').get().base_path = other
    assert.is_false(fff.go_to_definition({ cwd = other }))
    assert.are.equal(vim.uv.fs_realpath(other), fallback.cwd)
    assert.are.equal('usage.rs', vim.fn.expand('%:t'))
  end)

  it('validates the public arguments', function()
    assert.has_error(function() fff.go_to_definition('UniqueTarget') end)
    assert.has_error(function() fff.go_to_definition({ query = 1 }) end)
    assert.has_error(function() fff.go_to_definition({ cwd = false }) end)
    assert.has_error(function() fff.go_to_definition({ grep = 'plain' }) end)
  end)
end)
