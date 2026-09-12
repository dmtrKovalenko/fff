---@diagnostic disable: undefined-field, missing-fields
local fff_rust = require('fff.rust')
local fuzzy = require('fff.fuzzy')

local function git(dir, args)
  local cmd = { 'git', '-C', dir, '-c', 'user.name=t', '-c', 'user.email=t@t' }
  vim.list_extend(cmd, args)
  local out = vim.fn.system(cmd)
  assert(vim.v.shell_error == 0, table.concat(cmd, ' ') .. ' failed: ' .. out)
end

describe('git recency scoring', function()
  local tmp

  local function write(rel, content)
    local path = tmp .. '/' .. rel
    vim.fn.mkdir(vim.fn.fnamemodify(path, ':h'), 'p')
    local f = assert(io.open(path, 'w'))
    f:write(content)
    f:close()
  end

  local function commit(rel, content, msg)
    write(rel, content)
    git(tmp, { 'add', '-A' })
    git(tmp, { 'commit', '-m', msg, '--no-gpg-sign' })
  end

  --- Empty-query (frecency mode) search; returns the item and score for `name`.
  local function find(name)
    local result = fuzzy.fuzzy_search_files('', 4, nil, 100, 3, 0, 50)
    for i, item in ipairs(result.items) do
      if item.name == name then return item, result.scores[i] end
    end
    return nil, nil
  end

  local function wait_for_recency(name, expected)
    return vim.wait(15000, function()
      local item = find(name)
      return item ~= nil and item.git_recency_score == expected
    end, 50)
  end

  before_each(function()
    pcall(fff_rust.stop_background_monitor)
    pcall(fff_rust.cleanup_file_picker)

    tmp = vim.fn.tempname()
    vim.fn.mkdir(tmp, 'p')
    tmp = vim.fn.resolve(vim.fn.fnamemodify(tmp, ':p')):gsub('/+$', '')
    git(tmp, { 'init', '-b', 'main' })
    commit('hot.lua', 'return 1', 'c1')
    commit('cold.lua', 'return 1', 'c2')
    commit('hot.lua', 'return 2', 'c3')
  end)

  after_each(function()
    pcall(fff_rust.stop_background_monitor)
    pcall(fff_rust.cleanup_file_picker)
    if tmp then vim.fn.delete(tmp, 'rf') end
  end)

  it('boosts files by +1 per participating commit', function()
    assert.is_true(fff_rust.init_file_picker(tmp))
    fff_rust.wait_for_initial_scan(30000)

    -- Recency scores are applied asynchronously by the git-status worker.
    assert.is_true(wait_for_recency('hot.lua', 2), 'hot.lua never reached recency score 2')

    local hot_item, hot_score = find('hot.lua')
    local cold_item, cold_score = find('cold.lua')
    assert(hot_item and hot_score and cold_item and cold_score)

    assert.are.equal(2, hot_item.git_recency_score)
    assert.are.equal(2, hot_score.git_recency_boost)
    assert.are.equal(1, cold_item.git_recency_score)
    assert.are.equal(1, cold_score.git_recency_boost)
    assert.is_true(hot_score.total > cold_score.total, 'recent file must rank higher')
  end)

  it('respects the git_recency config passed through init opts', function()
    assert.is_true(fff_rust.init_file_picker(tmp, { git_recency = { max_commits = 1 } }))
    fff_rust.wait_for_initial_scan(30000)

    -- Only the latest commit (touching hot.lua) is analyzed.
    assert.is_true(wait_for_recency('hot.lua', 1), 'hot.lua never reached recency score 1')
    local cold_item = find('cold.lua')
    assert(cold_item)
    assert.are.equal(0, cold_item.git_recency_score)
  end)

  it('can be disabled entirely', function()
    assert.is_true(fff_rust.init_file_picker(tmp, { git_recency = false }))
    fff_rust.wait_for_initial_scan(30000)

    -- Dirty a committed file and refresh synchronously: the status flipping
    -- to 'modified' proves a full status+recency pass ran with the feature off.
    write('hot.lua', 'return 3')
    fff_rust.refresh_git_status()

    local hot_item, hot_score = find('hot.lua')
    assert(hot_item and hot_score)
    assert.are.equal('modified', hot_item.git_status)
    assert.are.equal(0, hot_item.git_recency_score)
    assert.are.equal(0, hot_score.git_recency_boost)
  end)
end)
