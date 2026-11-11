---@diagnostic disable: undefined-field, missing-fields
local fff = require('fff')
local fff_rust = require('fff.rust')
local file_picker = require('fff.file_picker')

local plugin_dir = vim.fn.fnamemodify(vim.fn.resolve(debug.getinfo(1, 'S').source:sub(2)), ':h:h')
local log_file = vim.fs.normalize(plugin_dir .. '/fff-test.log')

local ok, session_log = pcall(require('fff.rust').init_tracing, log_file, 'trace')
if not ok then session_log = nil end

-- comment to get a local log of this file
vim.api.nvim_create_autocmd('VimLeavePre', {
  once = true,
  callback = function()
    if os.getenv('CI') == nil and session_log then pcall(vim.fn.delete, session_log) end
  end,
})

local function init_picker_at_plugin_dir(timeout_ms)
  fff_rust.init_file_picker(plugin_dir)
  vim.wait(100, function() return false end)
  fff_rust.wait_for_initial_scan(timeout_ms or 30000)
end

local function find_result_by_name(items, name)
  for _, item in ipairs(items) do
    if item.name == name then return item end
  end
  return nil
end

describe('programmatic search APIs', function()
  describe('against the actual fff repo', function()
    before_each(function()
      pcall(vim.api.nvim_del_augroup_by_name, 'fff_file_tracking')
      vim.g.fff = {}
      file_picker.setup()
      init_picker_at_plugin_dir()
    end)

    after_each(function()
      pcall(fff_rust.stop_background_monitor)
      pcall(fff_rust.cleanup_file_picker)
      vim.g.fff = nil
    end)

    describe('file_search', function()
      it('defaults to mode=files and finds this very test file', function()
        local result = fff.file_search('programmatic_search_spec')

        assert.is_table(result)
        assert.is_table(result.items)
        assert.is_true(#result.items > 0, 'expected at least one match')
        assert.is_number(result.total_matched)
        assert.is_number(result.total_files)

        local hit = find_result_by_name(result.items, 'programmatic_search_spec.lua')
        assert.is_not_nil(hit, 'this spec file should appear in its own search results')
        ---@cast hit -nil
        assert.are.equal('file', hit.type)
        assert.is_string(hit.relative_path)
        local normalized = vim.fs.normalize(hit.relative_path)
        assert.is_true(
          normalized:find('tests/', 1, true) ~= nil,
          'expected relative_path under tests/, got ' .. tostring(hit.relative_path)
        )
        assert.is_number(hit.size)
      end)

      it('mode=directories finds the lua/fff/file_picker directory', function()
        local result = fff.file_search('file_picker', { mode = 'directories' })
        assert.is_true(#result.items > 0, 'expected at least one directory match')

        local lua_dir
        for _, item in ipairs(result.items) do
          if item.name == 'file_picker' and vim.fs.normalize(item.relative_path):find('lua/fff/', 1, true) then
            lua_dir = item
            break
          end
        end
        assert.is_not_nil(lua_dir, 'lua/fff/file_picker/ missing from directory results')
        assert.are.equal('directory', lua_dir.type)
        assert.is_nil(lua_dir.size, 'DirItem must not carry file-only fields')
        assert.is_nil(lua_dir.is_binary)
      end)

      it('mode=mixed returns both files and directories with type tags', function()
        local result = fff.file_search('file_picker', { mode = 'mixed' })
        assert.is_true(#result.items > 0, 'mixed search returned nothing')
        assert.is_number(result.total_files)
        assert.is_number(result.total_dirs)

        local seen_file, seen_dir = false, false
        for _, item in ipairs(result.items) do
          if item.type == 'file' then seen_file = true end
          if item.type == 'directory' then seen_dir = true end
        end
        assert.is_true(seen_file, 'mixed search did not return any files for "file_picker"')
        assert.is_true(seen_dir, 'mixed search did not return any directories for "file_picker"')
      end)

      it('rejects invalid mode', function()
        assert.has_error(function() fff.file_search('main', { mode = 'bogus' }) end)
      end)
    end)

    describe('content_search', function()
      -- Grep for an identifier that we own and is unlikely to disappear:
      -- `canonicalize_fff_path` is exported on `fff.utils` and consumed
      -- by both `picker_ui.lua` and `main.lua`, so it should appear in at
      -- least 2 different files.
      local marker = 'canonicalize_fff_path'

      it('defaults to plain mode and finds the marker', function()
        local result = fff.content_search(marker)
        assert.is_table(result)
        assert.is_true(#result.items > 0, 'plain content_search returned no matches for ' .. marker)

        local seen_files = {}
        for _, item in ipairs(result.items) do
          assert.is_string(item.relative_path)
          assert.is_number(item.line_number)
          assert.is_string(item.line_content)
          assert.is_true(item.line_content:find(marker, 1, true) ~= nil, 'matched line missing the marker')
          seen_files[item.relative_path] = true
        end
        local count = 0
        for _ in pairs(seen_files) do
          count = count + 1
        end
        assert.is_true(count >= 2, 'expected the marker in at least 2 files, got ' .. count)
      end)

      it('keeps file order unless the user config enables classification', function()
        local fuzzy = require('fff.fuzzy')
        local original, saved = fuzzy.live_grep, vim.g.fff
        local modes = {}
        fuzzy.live_grep = function(...)
          modes[#modes + 1] = select(12, ...)
          return original(...)
        end
        local search_ok, err = pcall(function()
          fff.content_search(marker)
          vim.g.fff = vim.tbl_deep_extend('force', saved or {}, { grep = { classify_definitions = true } })
          fff.content_search(marker)
        end)
        fuzzy.live_grep, vim.g.fff = original, saved
        assert(search_ok, err)
        assert.are.same({ 'off', 'classify' }, modes)
      end)

      it('regex mode matches a pattern', function()
        local result = fff.content_search('canonicalize_\\w+', { mode = 'regex' })
        assert.is_true(#result.items > 0, 'regex content_search returned no matches')
        assert.is_nil(result.regex_fallback_error, 'regex compilation should not have fallen back')
      end)

      it('fuzzy mode tolerates query typos', function()
        -- Drop a letter from the marker; fuzzy mode should still match.
        local query = 'lua/ ' .. marker:gsub('fff', 'ff', 1)
        local result = fff.content_search(query, { mode = 'fuzzy', time_budget_ms = 0 })
        assert.is_true(#result.items > 0, 'fuzzy content_search returned no matches')
        local found_marker = false
        for _, item in ipairs(result.items) do
          if item.line_content:find(marker, 1, true) then found_marker = true end
          assert.is_number(item.fuzzy_score)
        end
        assert.is_true(found_marker, 'fuzzy content_search did not match the source marker')
      end)

      it('rejects invalid mode', function()
        assert.has_error(function() fff.content_search('foo', { mode = 'bogus' }) end)
      end)
    end)
  end)

  describe('cwd switching', function()
    -- These specifically prove the picker can swap to a directory it has
    -- never indexed, so they need an isolated throwaway sandbox by design.
    local sandbox_root

    before_each(function()
      pcall(vim.api.nvim_del_augroup_by_name, 'fff_file_tracking')
      vim.g.fff = {}
      file_picker.setup()
      init_picker_at_plugin_dir()
    end)

    after_each(function()
      pcall(fff_rust.stop_background_monitor)
      pcall(fff_rust.cleanup_file_picker)
      if sandbox_root then vim.fn.delete(sandbox_root, 'rf') end
      sandbox_root = nil
      vim.g.fff = nil
    end)

    it('file_search switches the indexed root and waits for the new scan', function()
      sandbox_root = vim.fn.tempname() .. '_other'
      local other_filename = 'totally_unique_other.lua'
      vim.fn.mkdir(sandbox_root, 'p')
      local fd = assert(io.open(sandbox_root .. '/' .. other_filename, 'w'))
      fd:write('-- only lives in the other sandbox\n')
      fd:close()

      -- Sanity: the file does not exist in the primary (fff) index.
      local before = fff.file_search(other_filename)
      assert.are.equal(0, #before.items, 'sandbox file leaked into primary fff index')

      local result = fff.file_search(other_filename, { cwd = sandbox_root })
      assert.is_true(#result.items > 0, 'cwd switch did not surface file from the new root')
      local hit = find_result_by_name(result.items, other_filename)
      assert.is_not_nil(hit, 'expected file from the new cwd missing from results')
    end)

    it('file_search returns an empty result for a non-existent cwd', function()
      local missing = vim.fn.tempname() .. '_does_not_exist'
      local result = fff.file_search('main', { cwd = missing })
      assert.are.equal(0, #result.items)
      assert.are.equal(0, result.total_matched)
    end)

    it('content_search switches indexed root before grepping', function()
      sandbox_root = vim.fn.tempname() .. '_other_grep'
      vim.fn.mkdir(sandbox_root, 'p')
      -- Build the marker by concatenation so the literal string doesn't
      -- appear anywhere in the fff tree (otherwise the "before" grep
      -- would find this very test file via its own marker constant).
      local marker = 'isolated_grep' .. '_marker_xyzzy'
      local fd = assert(io.open(sandbox_root .. '/grep_target.lua', 'w'))
      fd:write('-- ' .. marker .. '\n')
      fd:close()

      -- on windows metadata cache is updated lazily if we programmatically start searching
      -- the new directory right after we update it we can sometimes see a race window especially on CI
      -- this is fine in practice cause this is not a real use case for fff to search RIGHT AFTER mkdir
      if vim.fn.has('win32') == 1 then vim.wait(250, function() return false end) end

      -- Marker must not exist anywhere in the primary fff tree.
      local before = fff.content_search(marker)
      assert.are.equal(0, #before.items, 'marker leaked into primary fff tree')

      -- Poll instead of asserting on the first grep: the index of the new root
      -- can lag a mkdir by a few ms on CI, which flaked on linux too.
      local result
      vim.wait(10000, function()
        result = fff.content_search(marker, { cwd = sandbox_root })
        return #result.items > 0
      end, 50)
      if #result.items == 0 then
        -- Print the index state so a CI failure shows why the new root did not match.
        local health_ok, health = pcall(fff_rust.health_check, sandbox_root)
        local files = fff.file_search('grep_target', { cwd = sandbox_root })
        print(vim.inspect({
          health = health_ok and health or tostring(health),
          file_hits = #files.items,
          dir = vim.fn.readdir(sandbox_root),
          base_path = require('fff.conf').get().base_path,
          grep = result,
        }))
      end
      assert.is_true(#result.items > 0, 'cwd switch did not surface match from the new root')
    end)
  end)

  describe('cwd switching without the picker UI', function()
    -- API-only callers never open the picker, so file_picker.setup() never runs (#938)
    local sandbox_root, saved_initialized

    local function capture_errors(fn)
      local errors, original = {}, vim.notify
      ---@diagnostic disable-next-line: duplicate-set-field
      vim.notify = function(msg, level)
        if level == vim.log.levels.ERROR then errors[#errors + 1] = msg end
      end
      local call_ok, result = pcall(fn)
      vim.notify = original
      assert(call_ok, result)
      return result, errors
    end

    before_each(function()
      pcall(vim.api.nvim_del_augroup_by_name, 'fff_file_tracking')
      vim.g.fff = {}
      saved_initialized = file_picker.state.initialized
      file_picker.state.initialized = false
      init_picker_at_plugin_dir()
      sandbox_root = vim.fn.tempname() .. '_no_picker_ui'
      vim.fn.mkdir(sandbox_root, 'p')
    end)

    after_each(function()
      pcall(fff_rust.stop_background_monitor)
      pcall(fff_rust.cleanup_file_picker)
      if sandbox_root then vim.fn.delete(sandbox_root, 'rf') end
      sandbox_root = nil
      file_picker.state.initialized = saved_initialized
      vim.g.fff = nil
    end)

    it('file_search switches the indexed root', function()
      local fd = assert(io.open(sandbox_root .. '/no_picker_ui_target.lua', 'w'))
      fd:write('-- only lives in the sandbox\n')
      fd:close()

      local result, errors = capture_errors(
        function() return fff.file_search('no_picker_ui_target', { cwd = sandbox_root }) end
      )
      assert.are.same({}, errors)
      assert.is_not_nil(find_result_by_name(result.items, 'no_picker_ui_target.lua'))
    end)

    it('content_search switches the indexed root', function()
      local marker = 'no_picker_ui' .. '_marker_plugh'
      local fd = assert(io.open(sandbox_root .. '/grep_target.lua', 'w'))
      fd:write('-- ' .. marker .. '\n')
      fd:close()

      -- same lazy windows metadata cache race as the cwd switching grep test
      if vim.fn.has('win32') == 1 then vim.wait(250, function() return false end) end

      local result, errors = capture_errors(function() return fff.content_search(marker, { cwd = sandbox_root }) end)
      assert.are.same({}, errors)
      -- same index lag after mkdir as the cwd switching grep test
      vim.wait(10000, function()
        if #result.items == 0 then result = fff.content_search(marker, { cwd = sandbox_root }) end
        return #result.items > 0
      end, 50)
      assert.is_true(#result.items > 0, 'cwd switch did not surface match from the new root')
    end)
  end)
end)
