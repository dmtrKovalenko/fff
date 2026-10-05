--- End-to-end snapshot tests for fff.nvim's picker UI.
local MiniTest = require('mini.test')
local fixture_lib = require('tests.snapshot.fixture')

local PLUGIN_DIR = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h')
local FORCE = vim.env.UPDATE_SNAPSHOTS == '1'

local child, fixture

local function setup(geometry, opts)
  opts = opts or {}
  fixture = fixture_lib.create(opts.extra_files)
  child = MiniTest.new_child_neovim()
  child.start({
    '--clean',
    '-n',
    '-i',
    'NONE',
    '--cmd',
    string.format('let &lines = %d', geometry.rows),
    '--cmd',
    string.format('let &columns = %d', geometry.cols),
  }, {
    connection_timeout = 15000,
  })

  child.o.lines = geometry.rows
  child.o.columns = geometry.cols

  local debug_enabled = opts.debug == true
  local show_path_first = opts.show_path_first == true
  -- Default show_file_info hides timings: Modified/Accessed timestamps drift
  -- between runs and would otherwise force `ignore_text` on those rows. Tests
  -- can override (or restore) by passing `opts.show_file_info`.
  local show_file_info = opts.show_file_info
    or { file_info = true, score_breakdown = true, timings = false, full_path = true }

  child.lua(
    string.format(
      [[
        local plugin = %q
        vim.opt.runtimepath:prepend(plugin)
        package.path = plugin .. '/lua/?.lua;' .. plugin .. '/lua/?/init.lua;' .. package.path
        vim.cmd('cd ' .. vim.fn.fnameescape(%q))
        vim.cmd('syntax off')
        local winborder = %q
        if winborder ~= '' then vim.o.winborder = winborder end
        vim.g.fff = {
          prompt = '> ',
          frecency = { enabled = true, db_path = %q },
          history  = { enabled = true, db_path = %q },
          layout = {
            show_path_first = %s,
          },
          logging  = { enabled = false },
          debug    = {
            enabled = %s,
            show_scores = %s,
            show_file_info = %s,
          },
        }
        require('fff.core').ensure_initialized()
        require('fff.rust').wait_for_initial_scan(8000)
      ]],
      PLUGIN_DIR,
      fixture.root,
      geometry.winborder or '',
      fixture.frecency_db,
      fixture.history_db,
      tostring(show_path_first),
      tostring(debug_enabled),
      tostring(debug_enabled),
      vim.inspect(show_file_info)
    )
  )
end

local function teardown()
  if child and child.is_running() then pcall(child.stop) end
  if fixture then fixture_lib.cleanup(fixture) end
  child, fixture = nil, nil
end

--- @param opts table|nil { ignore_text?: number[] }
local function assert_snapshot_match(opts)
  opts = opts or {}
  MiniTest.expect.reference_screenshot(child.get_screenshot(), nil, {
    force = FORCE,
    ignore_text = opts.ignore_text or false,
  })
end

local function open_grep(prompt_position, query)
  child.lua(string.format('require("fff").live_grep({ layout = { prompt_position = %q } })', prompt_position))
  vim.loop.sleep(400)
  child.type_keys(query)
  vim.loop.sleep(400)
end

local function open_picker(prompt_position, query)
  child.lua(
    string.format('require("fff.picker_ui.picker_ui").open({ layout = { prompt_position = %q } })', prompt_position)
  )
  vim.loop.sleep(400)

  if query and query ~= '' then
    child.type_keys(query)
    vim.loop.sleep(400)
  end
end

local LAYOUTS = {
  { name = 'wide', cols = 180, rows = 40, winborder = 'double' },
  { name = 'default', cols = 140, rows = 32 }, -- standard on most screens
  { name = 'narrow', cols = 70, rows = 24, winborder = 'rounded' },
  -- Extra-wide: exists primarily so the file_info panel hits the H2 layout.
  { name = 'xwide', cols = 240, rows = 48 },
}

local T = MiniTest.new_set()

local PROMPT_POSITIONS = { 'bottom', 'top' }

for _, geometry in ipairs(LAYOUTS) do
  local set = MiniTest.new_set({
    hooks = {
      pre_case = function() setup(geometry) end,
      post_case = teardown,
    },
  })

  -- Run every per-geometry case for both prompt positions: layout math and
  -- list rendering diverge between top/bottom (see AGENTS.md), so a snapshot
  -- on a single side would silently miss regressions in the other.
  for _, prompt in ipairs(PROMPT_POSITIONS) do
    set['empty_' .. prompt] = function()
      open_picker(prompt)
      assert_snapshot_match()
    end

    set['query_main_' .. prompt] = function()
      open_picker(prompt, 'main')
      assert_snapshot_match()
    end

    set['no_results_' .. prompt] = function()
      open_picker(prompt, 'zzzzzzzzz')
      assert_snapshot_match()
    end

    set['cursor_second_item_' .. prompt] = function()
      open_picker(prompt)
      -- Bottom prompt visually goes up with <Down>; top prompt goes down with <Down>.
      -- Either way one keypress moves to the second item — what we want to capture.
      child.type_keys('<Down>')
      vim.loop.sleep(200)
      assert_snapshot_match()
    end
  end

  T[geometry.name] = set
end

-- File info panel snapshots. Timings are disabled at the config level (see
-- `setup`) so the snapshots stay deterministic — Modified/Accessed timestamps
-- drift between runs. We snapshot a narrow and a wide variant for both prompt
-- positions to keep the adaptive panel layout (label widths, section headers,
-- top vs bottom prompt geometry) covered.
local debug_narrow_set = MiniTest.new_set({
  hooks = {
    pre_case = function() setup(LAYOUTS[2], { debug = true }) end, -- default 140x32, panel ~57 cols
    post_case = teardown,
  },
})

for _, prompt in ipairs(PROMPT_POSITIONS) do
  debug_narrow_set['file_info_panel_' .. prompt] = function()
    open_picker(prompt, 'main')
    assert_snapshot_match()
  end
end
T['debug_narrow'] = debug_narrow_set

local debug_wide_set = MiniTest.new_set({
  hooks = {
    pre_case = function() setup(LAYOUTS[4], { debug = true }) end, -- xwide 240x48, panel ~96 cols
    post_case = teardown,
  },
})

for _, prompt in ipairs(PROMPT_POSITIONS) do
  debug_wide_set['file_info_panel_' .. prompt] = function()
    open_picker(prompt, 'main')
    assert_snapshot_match()
  end
end
T['debug_wide'] = debug_wide_set

T['show_path_first'] = MiniTest.new_set({
  hooks = {
    pre_case = function() setup(LAYOUTS[3], { show_path_first = true }) end,
    post_case = teardown,
  },
})

for _, prompt in ipairs(PROMPT_POSITIONS) do
  T['show_path_first']['query_main_' .. prompt] = function()
    open_picker(prompt, 'main')
    assert_snapshot_match()
  end
end

T['combo'] = MiniTest.new_set({
  hooks = {
    pre_case = function() setup({ cols = 140, rows = 32 }) end,
    post_case = teardown,
  },
})

local function train_combo()
  -- Train: "main" → src/main.rs four times so open_count crosses the default
  -- min_combo_count (3). track_query_completion is async; pause between calls
  -- + final settle for the lmdb writer.
  for _ = 1, 4 do
    child.lua(
      string.format('require("fff.rust").track_query_completion(%q, %q)', 'main', fixture.root .. '/src/main.rs')
    )
    vim.loop.sleep(120)
  end
  vim.loop.sleep(400)
end

for _, prompt in ipairs(PROMPT_POSITIONS) do
  T['combo']['boost_' .. prompt] = function()
    train_combo()
    open_picker(prompt, 'main')
    -- Combo overlay float renders asynchronously after render_list.
    vim.loop.sleep(400)
    assert_snapshot_match()
  end
end

-- Grep matches on definitions of the query come first, boxed off from the
-- usages; handler.rs has both, so its header repeats below the separator.
local DEFINITION_FILES = {
  ['src/handler.rs'] = table.concat({
    'pub struct Handler {',
    '    inner: Inner,',
    '}',
    '',
    'impl Handler {',
    '    pub fn new() -> Handler {',
    '        Handler { inner: Inner::default() }',
    '    }',
    '}',
    '',
  }, '\n'),
  ['src/app.ts'] = table.concat({
    'export function useHandler(h: Handler) {',
    '  const local = new Handler()',
    '  return local',
    '}',
    '',
  }, '\n'),
}

T['definitions'] = MiniTest.new_set({
  hooks = {
    pre_case = function() setup({ cols = 140, rows = 32 }, { extra_files = DEFINITION_FILES }) end,
    post_case = teardown,
  },
})

for _, prompt in ipairs(PROMPT_POSITIONS) do
  T['definitions']['grep_' .. prompt] = function()
    open_grep(prompt, 'Handler')
    assert_snapshot_match()
  end
end

T['scrollbar'] = MiniTest.new_set({
  hooks = {
    pre_case = function() setup({ cols = 140, rows = 32 }) end,
    post_case = teardown,
  },
})

-- Cursor advance key differs by prompt position: bottom prompt iterates the
-- list in reverse so `<Up>` walks toward higher indices; top prompt is
-- conventional. Either way we walk past the last in-page item to trigger
-- load_next_page and surface the scrollbar thumb at the new offset.
local SCROLL_KEY = { bottom = '<Up>', top = '<Down>' }

for _, prompt in ipairs(PROMPT_POSITIONS) do
  T['scrollbar']['next_page_' .. prompt] = function()
    open_picker(prompt)
    for _ = 1, 30 do
      child.type_keys(SCROLL_KEY[prompt])
      vim.loop.sleep(20)
    end
    vim.loop.sleep(400)
    assert_snapshot_match()
  end
end

return T
