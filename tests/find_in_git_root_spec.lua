---@diagnostic disable: undefined-global

describe('find_in_git_root', function()
  local main
  local old_notify
  local old_system
  local old_getcwd
  local old_find_files_in_dir
  local notifications
  local opened_dir

  before_each(function()
    package.loaded['fff.main'] = nil
    package.loaded['fff.core'] = nil
    notifications = {}
    opened_dir = nil
    old_notify = vim.notify
    old_system = vim.system
    old_getcwd = vim.fn.getcwd
    vim.notify = function(msg, level)
      table.insert(notifications, { msg = msg, level = level })
    end
    vim.fn.getcwd = function()
      return '/repo/subdir'
    end
  end)

  after_each(function()
    vim.notify = old_notify
    vim.system = old_system
    vim.fn.getcwd = old_getcwd
    package.loaded['fff.main'] = nil
    package.loaded['fff.core'] = nil
  end)

  it('falls back to git rev-parse when git root is not ready yet', function()
    package.loaded['fff.core'] = {
      ensure_initialized = function()
        return {
          wait_for_initial_scan = function() return false end,
          get_git_root = function() return nil end,
        }
      end,
    }

    vim.system = function(cmd, opts)
      assert.are.same({ 'git', 'rev-parse', '--show-toplevel' }, cmd)
      assert.are.equal('/repo/subdir', opts.cwd)
      return {
        wait = function()
          return { code = 0, stdout = '/repo\n' }
        end,
      }
    end

    main = require('fff.main')
    old_find_files_in_dir = main.find_files_in_dir
    main.find_files_in_dir = function(dir)
      opened_dir = dir
    end

    main.find_in_git_root()

    assert.are.equal('/repo', opened_dir)
    assert.are.equal(0, #notifications)
    main.find_files_in_dir = old_find_files_in_dir
  end)

  it('warns when neither cached git root nor fallback command succeeds', function()
    package.loaded['fff.core'] = {
      ensure_initialized = function()
        return {
          wait_for_initial_scan = function() return false end,
          get_git_root = function() return nil end,
        }
      end,
    }

    vim.system = function()
      return {
        wait = function()
          return { code = 128, stdout = '' }
        end,
      }
    end

    main = require('fff.main')
    old_find_files_in_dir = main.find_files_in_dir
    main.find_files_in_dir = function(dir)
      opened_dir = dir
    end

    main.find_in_git_root()

    assert.is_nil(opened_dir)
    assert.are.equal(1, #notifications)
    assert.are.equal('Not in a git repository', notifications[1].msg)
    main.find_files_in_dir = old_find_files_in_dir
  end)
end)
