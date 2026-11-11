---@diagnostic disable: undefined-field, missing-fields
local fff_rust = require('fff.rust')
local fuzzy = require('fff.fuzzy')

local function git(dir, args)
  local cmd = { 'git', '-C', dir, '-c', 'user.name=t', '-c', 'user.email=t@t' }
  vim.list_extend(cmd, args)
  local out = vim.fn.system(cmd)
  assert(vim.v.shell_error == 0, table.concat(cmd, ' ') .. ' failed: ' .. out)
end

local function norm(p) return vim.fs.normalize(vim.uv.fs_realpath(p) or p):gsub('/$', '') end

describe('git status updates', function()
  local tmp

  local function write(rel, content)
    local f = assert(io.open(tmp .. '/' .. rel, 'w'))
    f:write(content)
    f:close()
  end

  local function status_of(name)
    local result = fuzzy.fuzzy_search_files('', 4, nil, 100, 3, 0, 50)
    for _, item in ipairs(result.items) do
      if item.name == name then return item.git_status end
    end
  end

  local function wait_git_status_applied()
    return vim.wait(15000, function() return not fff_rust.is_git_status_refreshing() end, 20)
  end

  before_each(function()
    pcall(fff_rust.stop_background_monitor)
    pcall(fff_rust.cleanup_file_picker)

    tmp = vim.fn.tempname()
    vim.fn.mkdir(tmp, 'p')
    tmp = norm(tmp)
    git(tmp, { 'init', '-b', 'main' })
    write('tracked.lua', 'return 1')
    git(tmp, { 'add', '-A' })
    git(tmp, { 'commit', '-m', 'init', '--no-gpg-sign' })
    write('untracked.lua', 'return 1')
  end)

  after_each(function()
    pcall(require('fff.picker_ui.picker_ui').close)
    pcall(fff_rust.resume_git_status)
    pcall(fff_rust.stop_background_monitor)
    pcall(fff_rust.cleanup_file_picker)
    if tmp then vim.fn.delete(tmp, 'rf') end
  end)

  it('pausing before init skips the status read until resumed', function()
    fff_rust.pause_git_status()
    assert.is_true(fff_rust.init_file_picker(tmp))
    fff_rust.wait_for_initial_scan(30000)

    assert.is_false(fff_rust.is_git_status_refreshing())
    assert.are.equal('clean', status_of('untracked.lua'))

    -- manual refresh works while paused
    fff_rust.refresh_git_status()
    assert.are.equal('untracked', status_of('untracked.lua'))

    write('tracked.lua', 'return 2')
    fff_rust.resume_git_status()
    assert.is_true(wait_git_status_applied(), 'resume refresh never finished')
    assert.are.equal('modified', status_of('tracked.lua'))
  end)

  it('picker refreshes on open, reruns the query and pauses on close', function()
    local picker_ui = require('fff.picker_ui.picker_ui')
    local core = require('fff.core')
    assert.are.equal('while_open', core.git_status_updates())

    -- startup path: the picker is created paused since the UI is closed
    core.mark_file_picker_uninitialized()
    core.ensure_initialized()
    assert.is_true(core.change_indexing_directory(tmp))
    assert.is_true(vim.wait(15000, function()
      local ok, health = pcall(fff_rust.health_check, tmp)
      return ok and health.file_picker.base_path and norm(health.file_picker.base_path) == tmp
    end, 20))
    fff_rust.wait_for_initial_scan(30000)
    assert.are.equal('clean', status_of('untracked.lua'))

    local function shown_status(name)
      for _, item in ipairs(picker_ui.state.items or {}) do
        if item.name == name then return item.git_status end
      end
    end

    picker_ui.open({ cwd = tmp })
    assert.is_true(
      vim.wait(15000, function() return shown_status('untracked.lua') == 'untracked' end, 20),
      'picker never re-rendered with the refreshed git status'
    )

    picker_ui.close()
    write('tracked.lua', 'return 2')
    vim.wait(500, function() return false end)
    assert.is_true(wait_git_status_applied())
    assert.are.equal('clean', status_of('tracked.lua'))

    picker_ui.open({ cwd = tmp })
    assert.is_true(
      vim.wait(15000, function() return shown_status('tracked.lua') == 'modified' end, 20),
      'reopened picker never showed the change made while closed'
    )
  end)
end)
