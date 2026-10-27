local M = {}

local conf = require('fff.conf')
local layout = require('fff.layout')
local preview = require('fff.file_picker.preview')
local list_separator = require('fff.list_separator')
local picker_ui_state = require('fff.picker_ui.picker_ui_state')

-- Parent module reference (set by picker_ui.lua during initialization).
-- Allows ui_creator functions to call back into the main picker module.
---@type table
local P = nil

function M.init(parent_module) P = parent_module end

-- Convenience alias
local S = picker_ui_state.state

local function resolve_winhl(kind)
  local hl = S.config.hl
  local winhl = hl.winhl
  local default_winhl = string.format('Normal:%s,FloatBorder:%s,FloatTitle:%s', hl.normal, hl.border, hl.title)

  if winhl == nil then return default_winhl end
  if type(winhl) == 'string' then return winhl end
  if type(winhl) == 'table' then return winhl[kind] or default_winhl end
  return default_winhl
end

local function open_preview(win_cfg)
  if not win_cfg then return end
  if S.preview_win and vim.api.nvim_win_is_valid(S.preview_win) then return end

  if not (S.preview_buf and vim.api.nvim_buf_is_valid(S.preview_buf)) then
    S.preview_buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_set_option_value('bufhidden', 'wipe', { buf = S.preview_buf })
    vim.api.nvim_buf_set_name(S.preview_buf, 'fffile preview')
    vim.api.nvim_set_option_value('buftype', 'nofile', { buf = S.preview_buf })
    vim.api.nvim_set_option_value('filetype', 'fff_preview', { buf = S.preview_buf })
    vim.api.nvim_set_option_value('modifiable', false, { buf = S.preview_buf })
  end

  S.preview_win = vim.api.nvim_open_win(S.preview_buf, false, win_cfg)

  local win_hl = resolve_winhl('preview')

  vim.api.nvim_set_option_value('wrap', false, { win = S.preview_win })
  vim.api.nvim_set_option_value('cursorline', false, { win = S.preview_win })
  vim.api.nvim_set_option_value('cursorlineopt', vim.o.cursorlineopt, { win = S.preview_win })
  vim.api.nvim_set_option_value(
    'number',
    S.mode == 'grep' or (conf.get().preview and conf.get().preview.line_numbers or false),
    { win = S.preview_win }
  )
  vim.api.nvim_set_option_value('relativenumber', false, { win = S.preview_win })
  vim.api.nvim_set_option_value('signcolumn', 'no', { win = S.preview_win })
  vim.api.nvim_set_option_value('foldcolumn', '0', { win = S.preview_win })
  vim.api.nvim_set_option_value('winhighlight', win_hl, { win = S.preview_win })

  preview.set_preview_window(S.preview_win)
end

local function close_preview()
  if S.preview_win and vim.api.nvim_win_is_valid(S.preview_win) then vim.api.nvim_win_close(S.preview_win, true) end
  S.preview_win = nil

  if S.preview_buf and vim.api.nvim_buf_is_valid(S.preview_buf) then
    preview.clear_buffer(S.preview_buf)
    vim.api.nvim_buf_delete(S.preview_buf, { force = true })
  end
  S.preview_buf = nil
  S.last_preview_file = nil
  S.last_preview_location = nil
end

local function set_keymap(mode, keys, handler, opts)
  local normalized_keys

  if type(keys) == 'string' then
    normalized_keys = { keys }
  elseif type(keys) == 'table' then
    normalized_keys = keys
  else
    normalized_keys = {}
  end

  for _, key in ipairs(normalized_keys) do
    vim.keymap.set(mode, key, handler, opts)
  end
end

local function handle_mouse_click_or_fallback(action, fallback)
  local pos = vim.fn.getmousepos()

  if P.state.active and pos.winid == S.list_win then
    local item_idx = S.line_to_item[pos.line]
    if not item_idx then return '' end

    vim.schedule(function()
      if not P.state.active then return end
      if not S.filtered_items[item_idx] then return end

      if S.cursor ~= item_idx then
        S.cursor = item_idx
        P.render_list()
        if S.mode == 'grep' or S.suggestion_source == 'grep' then
          P.update_preview_smart()
        else
          P.update_preview()
        end
        P.update_status()
      end

      if action then P.select(action) end
    end)
    return ''
  end

  return fallback
end

local function move_list_cursor(direction)
  if not P.state.active then return end

  local items = S.filtered_items
  if #items == 0 then return end

  local wrap_around = S.config and S.config.wrap_around or false
  local new_cursor = S.cursor + direction

  if wrap_around then
    if new_cursor < 1 then
      new_cursor = #items
    elseif new_cursor > #items then
      new_cursor = 1
    end
  else
    new_cursor = math.max(1, math.min(new_cursor, #items))
  end

  if new_cursor ~= S.cursor then
    S.cursor = new_cursor
    P.render_list()
    if S.mode == 'grep' or S.suggestion_source == 'grep' then
      P.update_preview_smart()
    else
      P.update_preview()
    end
    P.update_status()
  end
end

local insert_guard_ns = vim.api.nvim_create_namespace('fff_picker_insert_guard')
-- vim.on_key can discard keys only since 0.11
local can_hold_keys = vim.fn.has('nvim-0.11') == 1
local insert_guard = nil

-- Foreign autocmds running `windo` leave the prompt window, which makes neovim
-- stop insert mode. Re-enter it unless a key was typed since those autocmds ran.
local function setup_insert_mode_guard(group)
  local guard = { key_tick = 0, held_keys = {}, generation = 0 }
  insert_guard = guard

  local function finish_restore(replay)
    local keys = table.concat(guard.held_keys)
    guard.restoring = false
    guard.held_keys = {}
    if replay and keys ~= '' then vim.api.nvim_feedkeys(keys, 'mt', false) end
  end

  local function restore_insert()
    guard.scheduled = false
    if not guard.restoring then return end
    if not P.state.active or vim.api.nvim_get_current_win() ~= S.input_win then return finish_restore(false) end
    if vim.api.nvim_get_mode().mode ~= 'n' then return end

    -- '^ is the exact position where insert mode was stopped
    guard.col = guard.col or vim.api.nvim_buf_get_mark(S.input_buf, '^')[2]
    local line = vim.api.nvim_buf_get_lines(S.input_buf, 0, 1, false)[1] or ''
    if guard.col >= #line then
      vim.cmd('startinsert!')
    else
      vim.api.nvim_win_set_cursor(S.input_win, { 1, guard.col })
      vim.cmd('startinsert')
    end
  end

  -- startinsert is ignored while the mode change is still in progress
  local function schedule_restore()
    if guard.scheduled then return end
    guard.scheduled = true
    vim.schedule(restore_insert)
  end

  vim.on_key(function(_, typed)
    if not P.state.active or insert_guard ~= guard then
      if insert_guard == guard then vim.on_key(nil, insert_guard_ns) end
      return
    end
    guard.key_tick = guard.key_tick + 1
    if not guard.restoring then return end

    if not can_hold_keys then return finish_restore(false) end
    -- typed before insert mode is back, replayed once it is
    if typed and typed ~= '' then
      table.insert(guard.held_keys, typed)
      return ''
    end
  end, insert_guard_ns)

  -- keys that only edited the query are not the ones leaving insert mode
  vim.api.nvim_buf_attach(S.input_buf, false, {
    on_lines = function()
      if guard.armed_tick then guard.armed_tick = guard.key_tick end
    end,
  })

  local events = { 'FileType', 'Syntax', 'OptionSet', 'BufReadPost', 'BufWinEnter', 'BufWinLeave', 'BufEnter', 'BufLeave' }
  vim.api.nvim_create_autocmd(events, {
    group = group,
    callback = function()
      if not P.state.active then return end
      local mode = vim.api.nvim_get_mode().mode
      if mode:sub(1, 1) == 'i' then
        guard.armed_tick = guard.key_tick
      elseif mode == 'n' and guard.restoring then
        -- the pending startinsert may have been swallowed as well
        schedule_restore()
      end
    end,
    desc = 'Track autocmds that may kick the prompt out of insert mode',
  })

  vim.api.nvim_create_autocmd('ModeChanged', {
    group = group,
    pattern = { 'i:n', 'n:i' },
    callback = function()
      if vim.v.event.new_mode == 'i' then
        if guard.restoring then finish_restore(true) end
        return
      end

      local tick = guard.key_tick
      local dropped = guard.armed_tick == tick and guard.keep_normal_tick ~= tick
      guard.armed_tick = nil
      if not dropped or not P.state.active then return end

      guard.restoring = true
      guard.col = nil
      guard.generation = guard.generation + 1
      schedule_restore()

      -- never keep holding keys if insert mode does not come back
      local generation = guard.generation
      vim.defer_fn(function()
        if guard.restoring and guard.generation == generation then finish_restore(P.state.active) end
      end, 500)
    end,
    desc = 'Restore insert mode dropped by foreign autocmds',
  })
end

function M.create_ui()
  local config = S.config
  if not config then return false end

  -- Prompt editing should behave consistently even if the user has :set paste.
  S.restore_paste = (function()
    if not vim.o.paste then return false end
    vim.o.paste = false
    return true
  end)()

  if not S.ns_id then
    S.ns_id = vim.api.nvim_create_namespace('fff_picker_status')
    list_separator.init(S.ns_id)
  end

  local computed_layout = layout.compute(config, conf.preview_enabled(config))
  local win_configs = computed_layout.win_configs
  local debug_enabled_in_preview = computed_layout.debug_enabled

  S.layout = computed_layout.layout
  S.preview_visible = computed_layout.preview_visible

  S.input_buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_set_option_value('bufhidden', 'wipe', { buf = S.input_buf })

  S.list_buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_set_option_value('bufhidden', 'wipe', { buf = S.list_buf })

  if debug_enabled_in_preview then
    S.file_info_buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_set_option_value('bufhidden', 'wipe', { buf = S.file_info_buf })
  else
    S.file_info_buf = nil
  end

  S.list_win = vim.api.nvim_open_win(S.list_buf, false, win_configs.list)
  if debug_enabled_in_preview and win_configs.file_info then
    S.file_info_win = vim.api.nvim_open_win(S.file_info_buf, false, win_configs.file_info)
  else
    S.file_info_win = nil
  end

  if S.preview_visible then open_preview(win_configs.preview) end

  S.input_win = vim.api.nvim_open_win(S.input_buf, false, win_configs.input)

  M.setup_buffers()
  M.setup_windows()
  M.setup_keymaps()

  vim.api.nvim_set_current_win(S.input_win)

  P.update_results_sync()
  P.clear_preview()
  P.update_status()

  return true
end

function M.setup_buffers()
  vim.api.nvim_buf_set_name(S.input_buf, 'fffile search')
  vim.api.nvim_buf_set_name(S.list_buf, 'fffiles list')

  vim.api.nvim_set_option_value('buftype', 'prompt', { buf = S.input_buf })
  vim.api.nvim_set_option_value('filetype', 'fff_input', { buf = S.input_buf })

  vim.fn.prompt_setprompt(S.input_buf, S.config.prompt)

  -- Changing the contents of the input buffer will trigger Neovim to guess the language
  -- in order to provide syntax highlighting. This makes sure that it's always off.
  vim.api.nvim_create_autocmd('Syntax', {
    buffer = S.input_buf,
    callback = function() vim.api.nvim_set_option_value('syntax', '', { buf = S.input_buf }) end,
  })

  vim.api.nvim_set_option_value('buftype', 'nofile', { buf = S.list_buf })
  vim.api.nvim_set_option_value('filetype', 'fff_list', { buf = S.list_buf })
  vim.api.nvim_set_option_value('modifiable', false, { buf = S.list_buf })

  if S.file_info_buf then
    vim.api.nvim_set_option_value('buftype', 'nofile', { buf = S.file_info_buf })
    vim.api.nvim_set_option_value('filetype', 'fff_file_info', { buf = S.file_info_buf })
    vim.api.nvim_set_option_value('modifiable', false, { buf = S.file_info_buf })
  end
end

function M.setup_windows()
  local prompt_win_hl = resolve_winhl('prompt')
  local list_win_hl = resolve_winhl('list')
  local file_info_win_hl = resolve_winhl('file_info')

  vim.api.nvim_set_option_value('wrap', false, { win = S.input_win })
  vim.api.nvim_set_option_value('cursorline', false, { win = S.input_win })
  vim.api.nvim_set_option_value('number', false, { win = S.input_win })
  vim.api.nvim_set_option_value('relativenumber', false, { win = S.input_win })
  vim.api.nvim_set_option_value('signcolumn', 'no', { win = S.input_win })
  vim.api.nvim_set_option_value('foldcolumn', '0', { win = S.input_win })
  vim.api.nvim_set_option_value('winhighlight', prompt_win_hl, { win = S.input_win })

  vim.api.nvim_set_option_value('wrap', false, { win = S.list_win })
  vim.api.nvim_set_option_value('cursorline', false, { win = S.list_win })
  vim.api.nvim_set_option_value('number', false, { win = S.list_win })
  vim.api.nvim_set_option_value('relativenumber', false, { win = S.list_win })
  vim.api.nvim_set_option_value('signcolumn', 'yes:1', { win = S.list_win })
  vim.api.nvim_set_option_value('foldcolumn', '0', { win = S.list_win })
  vim.api.nvim_set_option_value('winhighlight', list_win_hl, { win = S.list_win })

  if S.file_info_win and vim.api.nvim_win_is_valid(S.file_info_win) then
    vim.api.nvim_set_option_value('wrap', false, { win = S.file_info_win })
    vim.api.nvim_set_option_value('cursorline', false, { win = S.file_info_win })
    vim.api.nvim_set_option_value('number', false, { win = S.file_info_win })
    vim.api.nvim_set_option_value('relativenumber', false, { win = S.file_info_win })
    vim.api.nvim_set_option_value('signcolumn', 'no', { win = S.file_info_win })
    vim.api.nvim_set_option_value('foldcolumn', '0', { win = S.file_info_win })
    vim.api.nvim_set_option_value('winhighlight', file_info_win_hl, { win = S.file_info_win })
  end

  local picker_group = vim.api.nvim_create_augroup('fff_picker_focus', { clear = true })

  local function is_picker_window(win)
    if not win or not vim.api.nvim_win_is_valid(win) then return false end

    local picker_windows = { S.input_win, S.list_win }
    if S.preview_win then table.insert(picker_windows, S.preview_win) end
    if S.file_info_win then table.insert(picker_windows, S.file_info_win) end

    for _, picker_win in ipairs(picker_windows) do
      if picker_win and vim.api.nvim_win_is_valid(picker_win) and win == picker_win then return true end
    end

    return false
  end

  vim.api.nvim_create_autocmd('WinLeave', {
    group = picker_group,
    callback = function()
      if not P.state.active then return end

      local leaving_win = vim.api.nvim_get_current_win()

      if not is_picker_window(leaving_win) then return end

      vim.schedule(function()
        if not P.state.active then return end

        local new_win = vim.api.nvim_get_current_win()

        if not is_picker_window(new_win) then P.close() end
      end)
    end,
    desc = 'Close picker when focus leaves picker windows',
  })

  setup_insert_mode_guard(picker_group)

  vim.api.nvim_create_autocmd('VimResized', {
    group = picker_group,
    callback = function()
      if not P.state.active then return end
      vim.schedule(function()
        if not P.state.active then return end
        P.relayout()
      end)
    end,
    desc = 'Re-layout picker on terminal resize',
  })
end

function M.setup_keymaps()
  local keymaps = S.config.keymaps
  local input_opts = { buffer = S.input_buf, noremap = true, silent = true }
  local list_opts = { buffer = S.list_buf, noremap = true, silent = true }

  vim.keymap.set('i', '<C-w>', function()
    local col = vim.fn.col('.') - 1
    local line = vim.fn.getline('.')
    local prompt_len = #S.config.prompt
    if col <= prompt_len then return '' end
    local text_part = line:sub(prompt_len + 1, col)
    local after_cursor = line:sub(col + 1)
    local new_text = text_part:gsub('%S*%s*$', '')
    local new_line = S.config.prompt .. new_text .. after_cursor
    local new_col = prompt_len + #new_text
    vim.fn.setline('.', new_line)
    vim.fn.cursor(vim.fn.line('.'), new_col + 1)
    return ''
  end, input_opts)

  set_keymap({ 'n', 'i' }, keymaps.move_up, P.move_up, input_opts)
  set_keymap({ 'n', 'i' }, keymaps.move_down, P.move_down, input_opts)
  set_keymap('i', keymaps.cycle_previous_query, P.recall_query_from_history, input_opts)
  set_keymap('i', keymaps.cycle_forward_query, P.cycle_forward_query, input_opts)
  set_keymap('n', 'j', P.move_down, input_opts)
  set_keymap('n', 'k', P.move_up, input_opts)
  set_keymap('n', 'q', P.close, input_opts)
  set_keymap('n', keymaps.focus_list, M.focus_list_win, input_opts)
  set_keymap('n', keymaps.focus_preview, M.focus_preview_win, input_opts)

  if keymaps.grep_jump_to_next_file then
    set_keymap({ 'i', 'n' }, keymaps.grep_jump_to_next_file, function() P.grep_jump_to_next_file() end, input_opts)
  end
  if keymaps.grep_jump_to_prev_file then
    set_keymap({ 'i', 'n' }, keymaps.grep_jump_to_prev_file, function() P.grep_jump_to_prev_file() end, input_opts)
  end

  if S.config.prompt_vim_mode then
    set_keymap('n', keymaps.close, P.close, input_opts)
    set_keymap('i', '<C-c>', P.close, input_opts)

    -- cc/S clear the whole line, wiping the prompt buffer's prompt and leaving
    -- a stray icon behind. Reset to just the prompt and re-enter insert instead.
    local function clear_query_line()
      local prompt = S.config.prompt
      vim.api.nvim_set_option_value('modifiable', true, { buf = S.input_buf })
      vim.api.nvim_buf_set_lines(S.input_buf, 0, -1, false, { prompt })
      vim.schedule(function()
        if S.input_win and vim.api.nvim_win_is_valid(S.input_win) then
          vim.api.nvim_win_set_cursor(S.input_win, { 1, #prompt })
          vim.cmd('startinsert!')
        end
      end)
    end
    -- remap=true so existing user mappings of cc/S still resolve
    local clear_opts = vim.tbl_extend('force', input_opts, { noremap = false, remap = true })
    set_keymap('n', 'cc', clear_query_line, clear_opts)
    set_keymap('n', 'S', clear_query_line, clear_opts)
  else
    set_keymap({ 'i', 'n' }, keymaps.close, P.close, input_opts)
  end

  set_keymap({ 'i', 'n' }, keymaps.select, P.select, input_opts)
  set_keymap({ 'i', 'n' }, keymaps.select_split, function() P.select('split') end, input_opts)
  set_keymap({ 'i', 'n' }, keymaps.select_vsplit, function() P.select('vsplit') end, input_opts)
  set_keymap({ 'i', 'n' }, keymaps.select_tab, function() P.select('tab') end, input_opts)
  set_keymap({ 'i', 'n' }, keymaps.preview_scroll_up, P.scroll_preview_up, input_opts)
  set_keymap({ 'i', 'n' }, keymaps.preview_scroll_down, P.scroll_preview_down, input_opts)
  set_keymap({ 'i', 'n' }, keymaps.toggle_debug, P.toggle_debug, input_opts)
  set_keymap({ 'i', 'n' }, keymaps.toggle_select, P.toggle_select, input_opts)
  set_keymap({ 'i', 'n' }, keymaps.send_to_quickfix, P.send_to_quickfix, input_opts)
  set_keymap({ 'i', 'n' }, keymaps.cycle_grep_modes, P.cycle_grep_modes, input_opts)
  -- last, so an explicitly configured key wins over the built-in bound to it
  set_keymap('i', keymaps.clear_query, P.clear_query, input_opts)

  if keymaps.insert_newline_escape then
    -- Inserts the literal 2-char `\n` sequence which the grep engine
    -- interprets as a multiline search boundary
    local newline_escape_opts = vim.tbl_extend('force', input_opts, { expr = true, replace_keycodes = false })
    set_keymap('i', keymaps.insert_newline_escape, function()
      if S.mode ~= 'grep' then return '' end
      return '\\n'
    end, newline_escape_opts)
  end

  local input_mouse_opts = vim.tbl_extend('force', input_opts, { expr = true, replace_keycodes = true })
  set_keymap(
    { 'i', 'n' },
    '<LeftMouse>',
    function() return handle_mouse_click_or_fallback(nil, '<LeftMouse>') end,
    input_mouse_opts
  )
  set_keymap(
    { 'i', 'n' },
    '<2-LeftMouse>',
    function() return handle_mouse_click_or_fallback('edit', '<2-LeftMouse>') end,
    input_mouse_opts
  )

  -- List buffer
  set_keymap('n', keymaps.close, P.close, list_opts)
  set_keymap('n', 'q', P.close, list_opts)
  set_keymap('n', 'j', function() move_list_cursor(1) end, list_opts)
  set_keymap('n', 'k', function() move_list_cursor(-1) end, list_opts)
  set_keymap('n', 'i', M.focus_input_win, list_opts)
  set_keymap('n', keymaps.focus_preview, M.focus_preview_win, list_opts)
  set_keymap('n', keymaps.select, P.select, list_opts)
  set_keymap('n', keymaps.select_split, function() P.select('split') end, list_opts)
  set_keymap('n', keymaps.select_vsplit, function() P.select('vsplit') end, list_opts)
  set_keymap('n', keymaps.select_tab, function() P.select('tab') end, list_opts)
  set_keymap('n', keymaps.preview_scroll_up, P.scroll_preview_up, list_opts)
  set_keymap('n', keymaps.preview_scroll_down, P.scroll_preview_down, list_opts)
  set_keymap('n', keymaps.toggle_debug, P.toggle_debug, list_opts)
  set_keymap('n', keymaps.toggle_select, P.toggle_select, list_opts)
  set_keymap('n', keymaps.send_to_quickfix, P.send_to_quickfix, list_opts)

  local list_mouse_opts = vim.tbl_extend('force', list_opts, { expr = true, replace_keycodes = true })
  set_keymap(
    'n',
    '<LeftMouse>',
    function() return handle_mouse_click_or_fallback(nil, '<LeftMouse>') end,
    list_mouse_opts
  )
  set_keymap(
    'n',
    '<2-LeftMouse>',
    function() return handle_mouse_click_or_fallback('edit', '<2-LeftMouse>') end,
    list_mouse_opts
  )

  -- Preview buffer
  if S.preview_buf then
    local preview_opts = { buffer = S.preview_buf, noremap = true, silent = true }

    set_keymap('n', keymaps.close, P.close, preview_opts)
    set_keymap('n', 'q', P.close, preview_opts)
    set_keymap('n', 'i', M.focus_input_win, preview_opts)
    set_keymap('n', keymaps.focus_list, M.focus_list_win, preview_opts)
    set_keymap('n', keymaps.select, P.select, preview_opts)
    set_keymap('n', keymaps.select_split, function() P.select('split') end, preview_opts)
    set_keymap('n', keymaps.select_vsplit, function() P.select('vsplit') end, preview_opts)
    set_keymap('n', keymaps.select_tab, function() P.select('tab') end, preview_opts)
    set_keymap('n', keymaps.toggle_debug, P.toggle_debug, preview_opts)
    set_keymap('n', keymaps.toggle_select, P.toggle_select, preview_opts)
    set_keymap('n', keymaps.send_to_quickfix, P.send_to_quickfix, preview_opts)
  end

  -- Applied last so a user mapping wins over the built-in on the same lhs.
  for mode, maps in pairs(S.config.mappings or {}) do
    for lhs, rhs in pairs(maps) do
      set_keymap(mode, lhs, rhs, input_opts)
    end
  end

  vim.api.nvim_buf_attach(S.input_buf, false, {
    on_lines = function()
      vim.schedule(function() P.on_input_change() end)
    end,
  })

  if S.config.prompt_vim_mode then
    vim.api.nvim_create_autocmd({ 'CursorMoved', 'CursorMovedI' }, {
      buffer = S.input_buf,
      callback = function()
        local prompt_len = #S.config.prompt
        if vim.fn.col('.') <= prompt_len then vim.fn.cursor(vim.fn.line('.'), prompt_len + 1) end
      end,
    })
  end
end

function M.focus_list_win()
  if not P.state.active then return end
  if not S.list_win or not vim.api.nvim_win_is_valid(S.list_win) then return end

  M.stop_insert()
  vim.api.nvim_set_current_win(S.list_win)
end

function M.focus_preview_win()
  if not P.state.active then return end
  if not S.preview_win or not vim.api.nvim_win_is_valid(S.preview_win) then return end

  M.stop_insert()
  vim.api.nvim_set_current_win(S.preview_win)
end

function M.focus_input_win()
  if not P.state.active then return end
  if not S.input_win or not vim.api.nvim_win_is_valid(S.input_win) then return end

  vim.api.nvim_set_current_win(S.input_win)
  vim.api.nvim_win_call(S.input_win, function() vim.cmd('startinsert!') end)
end

--- Leave insert mode on purpose, the insert mode guard will not undo it.
function M.stop_insert()
  if insert_guard then insert_guard.keep_normal_tick = insert_guard.key_tick end
  vim.cmd('stopinsert')
end

-- Expose open/close preview for relayout in the main module
M.open_preview = open_preview
M.close_preview = close_preview

return M
