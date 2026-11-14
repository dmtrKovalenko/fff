---@diagnostic disable: undefined-field
local fff_rust = require('fff.rust')

--- Wait for the scan to fully complete, handling the startup race where
--- the background thread hasn't set is_scanning=true yet.
--- @param timeout_ms number Maximum time to wait in milliseconds
local function wait_for_scan(timeout_ms)
  -- Small sleep to let the background thread start and set is_scanning=true.
  -- This handles the race between init_file_picker returning and the thread starting.
  vim.wait(100, function() return false end)
  fff_rust.wait_for_initial_scan(timeout_ms)
end

describe('fff.nvim core', function()
  local test_dir

  before_each(function()
    -- Use the plugin's own repo directory as a known git repo for testing
    test_dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':h:h')
    -- Make sure it resolves to an actual directory
    if vim.fn.isdirectory(test_dir) ~= 1 then test_dir = vim.fn.getcwd() end
  end)

  after_each(function()
    -- Cleanup: stop background monitor and release the file picker
    pcall(fff_rust.stop_background_monitor)
    pcall(fff_rust.cleanup_file_picker)
  end)

  describe('init and scan', function()
    it('should initialize the file picker and scan files', function()
      local ok = fff_rust.init_file_picker(test_dir)
      assert.is_true(ok)

      wait_for_scan(10000)

      local progress = fff_rust.get_scan_progress()
      assert.is_not_nil(progress)
      assert.is_number(progress.scanned_files_count)
      assert.is_true(
        progress.scanned_files_count > 0,
        'expected scanned files > 0, got ' .. progress.scanned_files_count
      )
      assert.is_false(progress.is_scanning)
    end)
  end)

  describe('fuzzy search', function()
    it('should return results for a known query', function()
      local ok = fff_rust.init_file_picker(test_dir)
      assert.is_true(ok)
      wait_for_scan(10000)

      -- Search for "main" which should match main.lua and possibly other files
      -- Args: query, max_threads, current_file, combo_boost_score_multiplier, min_combo_count, offset, page_size
      local result = fff_rust.fuzzy_search_files('main', 2, nil, 100, 3, 0, 10)
      assert.is_not_nil(result)
      assert.is_not_nil(result.items)
      assert.is_true(#result.items > 0, 'expected search results for "main"')

      -- Each item should have required fields
      local first = result.items[1]
      assert.is_not_nil(first.relative_path)
      assert.is_string(first.relative_path)
    end)

    it('should return empty results for nonsense query', function()
      local ok = fff_rust.init_file_picker(test_dir)
      assert.is_true(ok)
      wait_for_scan(10000)

      local result = fff_rust.fuzzy_search_files('zzzxxxqqq_no_match_ever', 2, nil, 100, 3, 0, 10)
      assert.is_not_nil(result)
      assert.is_not_nil(result.items)
      assert.are.equal(0, #result.items)
    end)
  end)

  describe('git root detection', function()
    it('should return the git root for a git repository', function()
      local ok = fff_rust.init_file_picker(test_dir)
      assert.is_true(ok)
      wait_for_scan(10000)

      local git_root = fff_rust.get_git_root()
      assert.is_not_nil(git_root, 'expected git root to be found in the plugin repo')
      assert.is_string(git_root)
      -- The git root should be a real directory
      assert.are.equal(1, vim.fn.isdirectory(git_root), 'git root should be a valid directory: ' .. git_root)
    end)

    it('should return nil for a non-git directory', function()
      -- Use a temp directory that is definitely not a git repo
      local tmp_dir = vim.fn.tempname()
      vim.fn.mkdir(tmp_dir, 'p')

      local ok = fff_rust.init_file_picker(tmp_dir)
      assert.is_true(ok)
      wait_for_scan(10000)

      local git_root = fff_rust.get_git_root()
      assert.is_nil(git_root)

      vim.fn.delete(tmp_dir, 'rf')
    end)
  end)

  describe('health check', function()
    it('should return version and component status', function()
      local ok = fff_rust.init_file_picker(test_dir)
      assert.is_true(ok)
      wait_for_scan(10000)

      local health = fff_rust.health_check(test_dir)
      assert.is_not_nil(health)
      assert.is_string(health.version)

      -- Git info should be present
      assert.is_not_nil(health.git)
      assert.is_true(health.git.available)
      assert.is_true(health.git.backend == 'gitoxide' or health.git.backend == 'libgit2')
      if health.git.backend == 'libgit2' then assert.is_string(health.git.libgit2_version) end

      -- File picker should be initialized
      assert.is_not_nil(health.file_picker)
      assert.is_true(health.file_picker.initialized)
      assert.is_string(health.file_picker.base_path)
    end)
  end)
end)
