---@diagnostic disable: undefined-field, missing-fields
-- Integration tests for grep mode file-group jump shortcuts.
local picker_ui
local state_mod

local plugin_dir = vim.fn.fnamemodify(vim.fn.resolve(debug.getinfo(1, 'S').source:sub(2)), ':h:h')

local function make_item(path, line, col)
  return { relative_path = path, line_number = line, col = col, line_content = '' }
end

local function reset_state(items, cursor)
  local S = state_mod.state
  S.active = true
  S.mode = 'grep'
  S.filtered_items = items
  S.items = items
  S.cursor = cursor or 1
  S.pagination = S.pagination or {}
  S.pagination.page_size = #items
  S.pagination.page_index = 0
  S.pagination.total_matched = #items
  S.pagination.grep_file_offsets = { 0 }
  S.pagination.grep_next_file_offset = 0
end

local stubs_installed = false
local function install_stubs()
  if stubs_installed then return end
  -- Mock UI updates called by submodules
  picker_ui.render_after_cursor_move = function() return true end
  picker_ui.update_status = function() end
  picker_ui.update_preview_debounced = function() end
  stubs_installed = true
end

describe('grep_jump_to_next_file / grep_jump_to_prev_file', function()
  before_each(function()
    vim.g.fff = {}

    local fff_rust = require('fff.rust')
    local file_picker = require('fff.file_picker')

    -- Initialize core components and background worker threads
    file_picker.setup()
    fff_rust.init_file_picker(plugin_dir)

    -- Resolve to the correct coordinator path inside the submodules directory
    picker_ui = require('fff.picker_ui.picker_ui')
    state_mod = require('fff.picker_ui.picker_ui_state')

    install_stubs()
  end)

  after_each(function()
    local fff_rust = require('fff.rust')
    pcall(fff_rust.stop_background_monitor)
    pcall(fff_rust.cleanup_file_picker)
    vim.g.fff = nil
  end)

  it('jumps to first match of the next file group', function()
    local items = {
      make_item('a.lua', 1, 1),
      make_item('a.lua', 5, 3),
      make_item('a.lua', 9, 1),
      make_item('b.lua', 2, 1),
      make_item('b.lua', 7, 1),
      make_item('c.lua', 4, 2),
    }
    reset_state(items, 1)

    picker_ui.grep_jump_to_next_file()
    assert.are.equal(4, state_mod.state.cursor)

    picker_ui.grep_jump_to_next_file()
    assert.are.equal(6, state_mod.state.cursor)
  end)

  it('jumps to first match of the previous file group', function()
    local items = {
      make_item('a.lua', 1, 1),
      make_item('a.lua', 5, 3),
      make_item('b.lua', 2, 1),
      make_item('b.lua', 7, 1),
      make_item('c.lua', 4, 2),
    }
    reset_state(items, 5)

    picker_ui.grep_jump_to_prev_file()
    assert.are.equal(3, state_mod.state.cursor)

    picker_ui.grep_jump_to_prev_file()
    assert.are.equal(1, state_mod.state.cursor)
  end)

  it('is a no-op when not in grep mode', function()
    local items = { make_item('a.lua', 1, 1), make_item('b.lua', 1, 1) }
    reset_state(items, 1)
    state_mod.state.mode = nil

    picker_ui.grep_jump_to_next_file()
    assert.are.equal(1, state_mod.state.cursor)
  end)

  it('loads next page when no later file group exists on current page', function()
    local page1 = {
      make_item('a.lua', 1, 1),
      make_item('a.lua', 2, 1),
    }
    local page2 = {
      make_item('b.lua', 1, 1),
      make_item('b.lua', 4, 1),
    }
    reset_state(page1, 2)
    state_mod.state.pagination.grep_next_file_offset = 1

    local called = false
    local original_load_next = picker_ui.load_next_page

    picker_ui.load_next_page = function()
      called = true
      state_mod.state.filtered_items = page2
      state_mod.state.items = page2
      state_mod.state.cursor = 1
      state_mod.state.pagination.page_index = 1
      return true
    end

    picker_ui.grep_jump_to_next_file()
    assert.is_true(called, 'expected load_next_page to be invoked')
    assert.are.equal(1, state_mod.state.cursor)
    assert.are.equal('b.lua', state_mod.state.filtered_items[state_mod.state.cursor].relative_path)

    picker_ui.load_next_page = original_load_next
  end)

  it('navigates empty partial grep pages for both prompt positions', function()
    local next_page, previous_page = 0, 0
    local original_next, original_previous = picker_ui.load_next_page, picker_ui.load_previous_page
    local original_config = state_mod.state.config
    picker_ui.load_next_page = function() next_page = next_page + 1 end
    picker_ui.load_previous_page = function() previous_page = previous_page + 1 end
    local ok, err = pcall(function()
      for _, position in ipairs({ 'top', 'bottom' }) do
        reset_state({}, 1)
        state_mod.state.config = { layout = { prompt_position = position } }
        state_mod.state.pagination.page_size = 50
        state_mod.state.pagination.grep_next_file_offset = 7
        local forward = position == 'top' and picker_ui.move_down or picker_ui.move_up
        local backward = position == 'top' and picker_ui.move_up or picker_ui.move_down
        forward()
        state_mod.state.pagination.page_index = 1
        backward()
      end
      assert.are.equal(2, next_page)
      assert.are.equal(2, previous_page)
    end)
    picker_ui.load_next_page, picker_ui.load_previous_page = original_next, original_previous
    state_mod.state.config = original_config
    assert.is_true(ok, err)
  end)

  it('records consecutive empty partial pages before a matching page', function()
    local manager = require('fff.picker_ui.search_manager')
    local grep = require('fff.picker_ui.grep_renderer')
    local original_search = grep.search
    manager.init({ render_list = function() end, update_preview = function() end, update_status = function() end })
    grep.search = function(_, offset)
      return {
        items = offset == 9 and { make_item('a.lua', 1, 0) } or {},
        next_file_offset = offset == 0 and 4 or (offset == 4 and 9 or 0),
      }
    end
    local ok, err = pcall(function()
      reset_state({}, 1)
      state_mod.state.pagination.page_size = 50
      assert.is_true(manager.load_page_at_index(0))
      assert.is_true(manager.load_next_page())
      assert.are.equal(1, state_mod.state.pagination.page_index)
      assert.are.equal(9, state_mod.state.pagination.grep_next_file_offset)
      assert.is_true(manager.load_next_page())
      assert.are.equal(2, state_mod.state.pagination.page_index)
      assert.are.equal(1, #state_mod.state.filtered_items)
      assert.is_true(manager.load_previous_page())
      assert.are.equal(1, state_mod.state.pagination.page_index)
    end)
    grep.search = original_search
    manager.init(picker_ui)
    assert.is_true(ok, err)
  end)

  it('renders completion when all partial pages are empty', function()
    local manager = require('fff.picker_ui.search_manager')
    local grep = require('fff.picker_ui.grep_renderer')
    local original_search = grep.search
    local rendered = 0
    manager.init({
      render_list = function() rendered = rendered + 1 end,
      update_preview = function() end,
      update_status = function() end,
    })
    grep.search = function(_, offset) return { items = {}, next_file_offset = offset == 0 and 4 or 0 } end
    local ok, err = pcall(function()
      reset_state({}, 1)
      state_mod.state.pagination.page_size = 50
      assert.is_true(manager.load_page_at_index(0))
      assert.is_true(manager.load_next_page())
      assert.are.equal(0, state_mod.state.pagination.grep_next_file_offset)
      assert.are.equal(2, rendered)
      assert.is_false(manager.load_next_page())
    end)
    grep.search = original_search
    manager.init(picker_ui)
    assert.is_true(ok, err)
  end)
end)
