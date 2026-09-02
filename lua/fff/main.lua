local M = {}

M.state = { initialized = false }

--- Setup the file picker with the given configuration
--- @param config table Configuration options
function M.setup(config) vim.g.fff = config end

--- Find files in current directory.
--- When opts.resume is true, resumes the last find_files picker (or opens a new one if none saved).
--- When opts.on_submit is set, it replaces the default `:edit` action on user selection.
--- Signature: `fun(item: table, ctx: { action: string, path: string, relative_path: string, location: table|nil, query: string, mode: string|nil })`.
--- @param opts? table Optional configuration {renderer = custom_renderer, resume = boolean, on_submit = function}
function M.find_files(opts)
  local picker_ok, picker_ui = pcall(require, 'fff.picker_ui.picker_ui')
  if not picker_ok then
    vim.notify('Failed to load picker UI: ' .. picker_ui, vim.log.levels.ERROR)
    return
  end

  if opts and opts.resume then
    picker_ui.resume_find_files(opts)
    return
  end

  picker_ui.open(opts)
end

--- Live grep: search file contents in the current directory.
--- When opts.resume is true, resumes the last live_grep picker (or opens a new one if none saved).
--- @param opts? {cwd?: string, title?: string, prompt?: string, layout?: table, grep?: {max_file_size?: number, smart_case?: boolean, max_matches_per_file?: number, modes?: string[]}, query?: string, resume?: boolean} Optional configuration overrides
function M.live_grep(opts)
  local picker_ok, picker_ui = pcall(require, 'fff.picker_ui.picker_ui')
  if not picker_ok then
    vim.notify('Failed to load picker UI: ' .. picker_ui, vim.log.levels.ERROR)
    return
  end

  if opts and opts.resume then
    picker_ui.resume_live_grep(opts)
    return
  end

  local config = require('fff.conf').get()
  local grep_renderer = require('fff.picker_ui.grep_renderer')

  local grep_config = vim.tbl_deep_extend('force', config.grep or {}, (opts and opts.grep) or {})

  local picker_opts = vim.tbl_deep_extend('force', {
    title = 'Live Grep',
    mode = 'grep',
    renderer = grep_renderer,
    grep_config = grep_config,
  }, opts or {})

  picker_ui.open(picker_opts)
end

--- Live grep prefilled with the current word (normal mode) or the visual selection (visual mode).
--- @param opts? table Forwarded to `live_grep`; `query` is overwritten by the resolved text.
function M.live_grep_under_cursor(opts)
  local mode = vim.fn.mode()
  local query
  if mode == 'v' or mode == 'V' or mode == '\22' then
    -- Exit visual so '< / '> marks settle, then read the range directly —
    -- no yank, no register clobber.
    vim.cmd('normal! ' .. vim.api.nvim_replace_termcodes('<Esc>', true, false, true))
    local s = vim.fn.getpos("'<")
    local e = vim.fn.getpos("'>")
    local lines = vim.fn.getregion(s, e, { type = mode })
    query = table.concat(lines, ' ')
  else
    query = vim.fn.expand('<cword>')
  end

  opts = vim.tbl_deep_extend('force', opts or {}, { query = query })
  M.live_grep(opts)
end

--- Changes the directory indexed by the file picker to the git root and opens the file picker
--- @deprecated Use `find_files` instead
function M.find_in_git_root()
  local fuzzy = require('fff.core').ensure_initialized()
  local ok, git_root = pcall(fuzzy.get_git_root)

  if not ok or not git_root then
    vim.notify('Not in a git repository', vim.log.levels.WARN)
    return
  end

  M.find_files_in_dir(git_root)
end

--- Clear FFF caches (both in-memory state and on-disk database files)
--- @param scope? string Cache scope: all|frecency|files
function M.clear_cache(scope)
  local fuzzy = require('fff.fuzzy')
  if not scope or scope == '' then scope = 'all' end

  local errors = {}

  if scope == 'all' or scope == 'files' then
    local ok, err = pcall(fuzzy.cleanup_file_picker)
    if not ok then
      table.insert(errors, 'cleanup file picker: ' .. tostring(err))
    else
      -- Rust picker is gone; clear the core flag so the next ensure_initialized
      -- rebuilds it instead of operating on a dropped picker (#772).
      require('fff.core').mark_file_picker_uninitialized()
    end
  end

  if scope == 'all' or scope == 'frecency' then
    local ok, err = pcall(fuzzy.destroy_frecency_db)
    if not ok then table.insert(errors, 'destroy frecency db: ' .. tostring(err)) end

    ok, err = pcall(fuzzy.destroy_query_db)
    if not ok then table.insert(errors, 'destroy query db: ' .. tostring(err)) end
  end

  if #errors > 0 then
    vim.notify('FFF: errors clearing cache: ' .. table.concat(errors, '; '), vim.log.levels.ERROR)
    return false
  end

  vim.notify('Cleared FFF cache: ' .. scope, vim.log.levels.INFO)
  return true
end

--- Trigger rescan of files in the current directory
function M.scan_files()
  local fuzzy = require('fff.core').ensure_initialized()
  local ok = pcall(fuzzy.scan_files)
  if not ok then vim.notify('Failed to scan files', vim.log.levels.ERROR) end
end

--- Refresh git status for the active file lock
function M.refresh_git_status()
  local fuzzy = require('fff.core').ensure_initialized()
  local ok, updated_files_count = pcall(fuzzy.refresh_git_status)
  if ok then
    vim.notify('Refreshed git status for ' .. tostring(updated_files_count) .. ' files', vim.log.levels.INFO)
  else
    vim.notify('Failed to refresh git status', vim.log.levels.ERROR)
  end
end

--- Search files programmatically
--- @param query string Search query
--- @param max_results number Maximum number of results
--- @return table List of matching files
function M.search(query, max_results)
  local fuzzy = require('fff.core').ensure_initialized()
  local config = require('fff.conf').get()
  max_results = max_results or config.max_results
  local max_threads = config.max_threads or 4
  local combo_boost_score_multiplier = config.history and config.history.combo_boost_score_multiplier or 100
  local min_combo_count = config.history and config.history.min_combo_count or 3
  -- Args: query, max_threads, current_file, combo_boost_score_multiplier, min_combo_count, offset, page_size
  local ok, search_result = pcall(
    fuzzy.fuzzy_search_files,
    query,
    max_threads,
    nil,
    combo_boost_score_multiplier,
    min_combo_count,
    0,
    max_results
  )
  if ok and search_result.items then return search_result.items end
  return {}
end

--- @class fff.FileSearchOpts
--- @field mode? "files"|"directories"|"mixed" Item type to search (default: 'files').
--- @field max_results? number Max items per page (default: config.max_results).
--- @field page? number 0-based page index (default: 0).
--- @field current_file? string Path to deprioritize (default: nil).
--- @field max_threads? number Worker threads (default: config.max_threads).
--- @field combo_boost_score_multiplier? number Override history combo boost.
--- @field min_combo_count? number Override history min_combo_count.
--- @field cwd? string If set and different from the current indexed root, switch the index to this directory before searching. Implies waiting for the new scan unless `wait_for_index_ms = 0`.
--- @field wait_for_index_ms? number Block up to this many ms for the index to be ready (default: 10000 when `cwd` triggers a re-index, 0 otherwise). Set to 0 to never block.

--- Switch the indexed root if `cwd` is set and different from the current
--- `base_path`, then optionally block until the new scan completes.
--- Returns `true` when the index is ready (or no wait requested), or `false`
--- with an error message on timeout / invalid cwd.
--- @param cwd string|nil
--- @param wait_for_index_ms number|nil
--- @return boolean ok, string? err
local function ensure_indexed(cwd, wait_for_index_ms)
  -- ensure_initialized is idempotent; first call kicks off the initial scan
  -- at config.base_path.
  require('fff.core').ensure_initialized()
  local config = require('fff.conf').get()

  local is_windows = vim.fn.has('win32') == 1
  local function canon(p)
    if not p or p == '' then return '' end
    local abs = vim.fn.fnamemodify(vim.fn.expand(p), ':p')
    abs = (abs:gsub('[/\\]+$', ''))
    -- fs_realpath resolves Windows 8.3 short names (RUNNER~1 -> runneradmin)
    -- so picker base_path (canonicalized in rust) compares equal to the cwd
    -- argument. fnamemodify(':p') alone keeps the short form on Windows.
    local realpath_ok, realpath = pcall(vim.uv.fs_realpath, abs)
    if realpath_ok and realpath then abs = realpath end
    local normalized = vim.fs.normalize(abs)
    if is_windows then normalized = normalized:lower() end
    return normalized
  end

  local cwd_triggered_reindex = false

  if cwd and cwd ~= '' then
    local expanded = vim.fn.expand(cwd)
    if vim.fn.isdirectory(expanded) ~= 1 then return false, 'cwd does not exist: ' .. expanded end

    if canon(config.base_path) ~= canon(expanded) then
      if not require('fff.core').change_indexing_directory(expanded) then
        return false, 'failed to change indexing directory to ' .. expanded
      end
      cwd_triggered_reindex = true
    end
  end

  -- Default: only wait when cwd actually swapped the picker. Callers can
  -- pass wait_for_index_ms explicitly to force a wait on first-time init too.
  local wait_ms = wait_for_index_ms
  if wait_ms == nil then wait_ms = cwd_triggered_reindex and 10000 or 0 end
  if wait_ms <= 0 then return true end

  local fff_rust = require('fff.rust')

  -- The picker swap runs on a background thread; wait_for_scan reads the
  -- picker pointer once at entry, so polling health_check first guarantees
  -- we wait on the new picker rather than racing the old one.
  if cwd_triggered_reindex then
    local target = canon(cwd)
    local deadline = vim.uv.hrtime() + wait_ms * 1e6
    local matched = false
    while vim.uv.hrtime() < deadline do
      local ok, health = pcall(fff_rust.health_check, target)
      if ok and health and health.file_picker and health.file_picker.base_path then
        if canon(health.file_picker.base_path) == target then
          matched = true
          break
        end
      end
      vim.wait(20, function() return false end)
    end
    if not matched then return false, 'timeout waiting for re-index swap' end
    -- Subtract the time we spent polling so the scan wait stays bounded.
    local remaining = math.max(0, math.floor((deadline - vim.uv.hrtime()) / 1e6))
    if remaining == 0 then return false, 'timeout waiting for index scan' end
    wait_ms = remaining
  end

  local scan_ok = require('fff.file_picker').wait_for_initial_scan(wait_ms)
  if not scan_ok then return false, 'timeout waiting for index scan' end
  return true
end

--- Programmatic file search.
--- Returns the full structured result so callers can read scores, totals,
--- and (for `files`/`mixed` modes) the parsed `location`.
---
--- For `mixed` mode each item has a `type` field of `"file"` or `"directory"`.
--- @param query string Search query (constraint syntax supported)
--- @param opts? fff.FileSearchOpts
--- @return { items: table[], scores: table[], total_matched: number, total_files?: number, total_dirs?: number, location?: table }
function M.file_search(query, opts)
  vim.validate({
    query = { query, 'string' },
    opts = { opts, 'table', true },
  })
  opts = opts or {}

  local indexed_ok, err = ensure_indexed(opts.cwd, opts.wait_for_index_ms)
  if not indexed_ok then
    vim.notify('FFF file_search: ' .. err, vim.log.levels.ERROR)
    return { items = {}, scores = {}, total_matched = 0 }
  end

  local fuzzy = require('fff.fuzzy')
  local config = require('fff.conf').get()
  local mode = opts.mode or 'files'
  local max_threads = opts.max_threads or config.max_threads or 4
  local page_size = opts.max_results or config.max_results or 100
  local page_index = opts.page or 0
  local current_file = opts.current_file
  local combo_boost = opts.combo_boost_score_multiplier
    or (config.history and config.history.combo_boost_score_multiplier)
    or 100
  local min_combo = opts.min_combo_count or (config.history and config.history.min_combo_count) or 3

  local empty = { items = {}, scores = {}, total_matched = 0 }
  if mode == 'files' then
    local offset = page_index * page_size
    local ok, result =
      pcall(fuzzy.fuzzy_search_files, query, max_threads, current_file, combo_boost, min_combo, offset, page_size)
    if not ok then
      vim.notify('FFF file_search failed: ' .. tostring(result), vim.log.levels.ERROR)
      return empty
    end
    return result
  elseif mode == 'directories' then
    local ok, result = pcall(fuzzy.fuzzy_search_directories, query, max_threads, current_file, page_index, page_size)
    if not ok then
      vim.notify('FFF file_search(directories) failed: ' .. tostring(result), vim.log.levels.ERROR)
      return empty
    end
    return result
  elseif mode == 'mixed' then
    local ok, result =
      pcall(fuzzy.fuzzy_search_mixed, query, max_threads, current_file, combo_boost, min_combo, page_index, page_size)
    if not ok then
      vim.notify('FFF file_search(mixed) failed: ' .. tostring(result), vim.log.levels.ERROR)
      return empty
    end
    return result
  else
    error("fff.file_search: opts.mode must be 'files', 'directories', or 'mixed', got " .. tostring(mode))
  end
end

--- @class fff.ContentSearchOpts
--- @field mode? "plain"|"regex"|"fuzzy" Grep mode (default: 'plain').
--- @field max_file_size? number Skip files larger than N bytes (default: config.grep.max_file_size).
--- @field max_matches_per_file? number Cap matches per file, 0 = unlimited (default: config.grep.max_matches_per_file).
--- @field smart_case? boolean Case-insensitive when query is all lowercase (default: config.grep.smart_case).
--- @field page_size? number Max matches returned (default: 50).
--- @field file_offset? number File-based pagination offset (default: 0).
--- @field time_budget_ms? number Max wall-clock time, 0 = unlimited (default: config.grep.time_budget_ms).
--- @field enforce_time_budget? boolean Apply the budget even with zero matches (default: config.grep.enforce_time_budget).
--- @field trim_whitespace? boolean Strip leading whitespace from matched lines (default: config.grep.trim_whitespace).
--- @field cwd? string Switch indexed root before grepping (same semantics as `file_search`).
--- @field wait_for_index_ms? number Block up to this many ms for the index to be ready.

--- Programmatic content (grep) search.
--- Returns the full structured `GrepResult` (items, totals, regex fallback).
--- @param query string Grep query (`*.rs pattern`, glob constraints, etc. supported)
--- @param opts? fff.ContentSearchOpts
--- @return { items: table[], total_matched: number, total_files_searched: number, total_files: number, filtered_file_count: number, next_file_offset: number, regex_fallback_error?: string }
function M.content_search(query, opts)
  vim.validate({
    query = { query, 'string' },
    opts = { opts, 'table', true },
  })
  opts = opts or {}

  local mode = opts.mode or 'plain'
  if mode ~= 'plain' and mode ~= 'regex' and mode ~= 'fuzzy' then
    error("fff.content_search: opts.mode must be 'plain', 'regex', or 'fuzzy', got " .. tostring(mode))
  end

  local empty = {
    items = {},
    total_matched = 0,
    total_files_searched = 0,
    total_files = 0,
    filtered_file_count = 0,
    next_file_offset = 0,
  }

  local indexed_ok, err = ensure_indexed(opts.cwd, opts.wait_for_index_ms)
  if not indexed_ok then
    vim.notify('FFF content_search: ' .. err, vim.log.levels.ERROR)
    return empty
  end

  local config = require('fff.conf').get()
  local grep_cfg = config.grep or {}
  local grep = require('fff.picker_ui.grep_renderer')
  local merged_grep_cfg = {
    max_file_size = opts.max_file_size or grep_cfg.max_file_size,
    max_matches_per_file = opts.max_matches_per_file or grep_cfg.max_matches_per_file,
    smart_case = opts.smart_case == nil and grep_cfg.smart_case or opts.smart_case,
    time_budget_ms = opts.time_budget_ms or grep_cfg.time_budget_ms,
    enforce_time_budget = opts.enforce_time_budget == nil and grep_cfg.enforce_time_budget or opts.enforce_time_budget,
    trim_whitespace = opts.trim_whitespace == nil and grep_cfg.trim_whitespace or opts.trim_whitespace,
  }

  local ok, result = pcall(grep.search, query, opts.file_offset or 0, opts.page_size or 50, merged_grep_cfg, mode)
  if not ok then
    vim.notify('FFF content_search failed: ' .. tostring(result), vim.log.levels.ERROR)
    return empty
  end
  return result
end

--- Search and show results in a nice format
--- @param query string Search query
function M.search_and_show(query)
  if not query or query == '' then
    M.find_files()
    return
  end

  local results = M.search(query, 20)

  if #results == 0 then
    print('🔍 No files found matching "' .. query .. '"')
    return
  end

  -- Filter out directories (should already be done by Rust, but just in case)
  local files = {}
  for _, item in ipairs(results) do
    if not item.is_dir then table.insert(files, item) end
  end

  if #files == 0 then
    print('🔍 No files found matching "' .. query .. '"')
    return
  end

  print('🔍 Found ' .. #files .. ' files matching "' .. query .. '":')

  for i, file in ipairs(files) do
    if i <= 15 then
      local file_extension = vim.fn.fnamemodify(file.name, ':e')
      local icon = file_extension ~= '' and '.' .. file_extension or '📄'
      local frecency = file.total_frecency_score > 0 and ' ⭐' .. file.total_frecency_score or ''
      print('  ' .. i .. '. ' .. icon .. ' ' .. file.relative_path .. frecency)
    end
  end

  if #files > 15 then print('  ... and ' .. (#files - 15) .. ' more files') end

  print('Use :FFFFind to browse all files')
end

--- Get file preview
--- @param file_path string Path to the file
--- @return string|nil File content or nil if failed
function M.get_preview(file_path)
  local preview = require('fff.file_picker.preview')
  local temp_buf = vim.api.nvim_create_buf(false, true)
  local success = preview.preview(file_path, temp_buf)
  if not success then
    vim.api.nvim_buf_delete(temp_buf, { force = true })
    return nil
  end
  local lines = vim.api.nvim_buf_get_lines(temp_buf, 0, -1, false)
  vim.api.nvim_buf_delete(temp_buf, { force = true })
  return table.concat(lines, '\n')
end

--- Find files in a specific directory
--- @param directory string Directory path to search in
function M.find_files_in_dir(directory)
  if not directory then
    vim.notify('Directory path required for find_files_in_dir', vim.log.levels.ERROR)
    return
  end

  local picker_ok, picker_ui = pcall(require, 'fff.picker_ui.picker_ui')
  if picker_ok then
    picker_ui.open({
      title = 'Files in ' .. vim.fn.fnamemodify(directory, ':t'),
      cwd = directory,
    })
  else
    vim.notify('Failed to load picker UI', vim.log.levels.ERROR)
  end
end

--- Change the base directory for the file picker
--- @param new_path string New directory path to use as base
--- @return boolean `true` if successful, `false` otherwise
function M.change_indexing_directory(new_path) return require('fff.core').change_indexing_directory(new_path) end

--- Resume the most recently closed picker (find_files or live_grep).
--- Similar to Telescope's `require('telescope.builtin').resume()`.
---@return boolean true if a picker was resumed, false if there is nothing to resume
function M.resume()
  local picker_ok, picker_ui = pcall(require, 'fff.picker_ui.picker_ui')
  if not picker_ok then
    vim.notify('Failed to load picker UI: ' .. picker_ui, vim.log.levels.ERROR)
    return false
  end
  return picker_ui.resume()
end

-- Strip wrapper punctuation that frequently surrounds paths in prose: leading
-- markdown-link `[`, parens `(`, brackets `<`, quotes; trailing sentence
-- punctuation. We additionally truncate at the first closing wrapper so a
-- cWORD like `[file.lua](./somewhere)` collapses to just `file.lua`. We
-- deliberately keep `:` and digits inside the word so `path:line:col`
-- suffixes survive.
local function strip_path_wrappers(s)
  if not s or s == '' then return s end
  s = s:gsub('^[%(%[%{<"\'`]+', '')
  s = s:gsub('([%)%]%}>"\'`]).*$', '')
  s = s:gsub('[,;!%?]+$', '')
  s = s:gsub('([^%.])%.$', '%1')
  -- Drop a leading `./` or `.\` — purely presentational, but the rust scorer
  -- otherwise can't recognise the path as an exact filename / path match.
  s = s:gsub('^%./', '')
  s = s:gsub('^%.\\', '')
  return s
end

-- Split a `path:line:col` or `path:line` suffix off a path candidate.
-- Returns `(path, location|nil)`.
local function split_location_suffix(s)
  if not s or s == '' then return s, nil end
  local p, l, c = s:match('^(.-):(%d+):(%d+)$')
  if p and p ~= '' then return p, { line = tonumber(l), col = tonumber(c) } end
  local p2, l2 = s:match('^(.-):(%d+)$')
  if p2 and p2 ~= '' then return p2, { line = tonumber(l2) } end
  return s, nil
end

-- Heuristic: only a string with an explicit path separator (or `~`) is treated
-- as "definitely a path" worth resolving directly. Bare names like `foo.lua`
-- still go through the fuzzy picker so frecency / disambiguation can help.
local function looks_like_path(s)
  if not s or s == '' then return false end
  if vim.startswith(s, '~') then return true end
  return s:find('[/\\]') ~= nil
end

-- Resolve `path` to an existing file on disk. Tries (in order): expanded
-- absolute, base_path-relative, cwd-relative. Returns the absolute path on
-- success, otherwise `nil`.
local function resolve_existing_file(path)
  if not path or path == '' then return nil end
  local expanded = vim.fn.expand(path)

  -- Absolute (after ~ expansion): check directly
  if vim.fn.fnamemodify(expanded, ':p') == expanded then
    if vim.fn.filereadable(expanded) == 1 then return expanded end
    return nil
  end

  local seen = {}
  local function try(candidate)
    if not candidate or seen[candidate] then return nil end
    seen[candidate] = true
    if vim.fn.filereadable(candidate) == 1 then return candidate end
    return nil
  end

  local base = require('fff.conf').get().base_path
  if base and base ~= '' then
    local hit = try(vim.fs.normalize(base .. '/' .. expanded))
    if hit then return hit end
  end
  return try(vim.fs.normalize(vim.fn.getcwd() .. '/' .. expanded))
end

-- Open `abs_path` honouring the same window-targeting dance as `M.select`:
-- if the current window is `winfixbuf` / has a special buftype, retarget to
-- a suitable window, else fall back to `:split`. Optionally jumps to a
-- `location = { line, col }` after the buffer loads.
local function open_resolved_file(abs_path, relative_path, location, open_cb)
  local utils = require('fff.utils')
  local cwd_relative = vim.fn.fnamemodify(abs_path, ':.')

  if open_cb and type(open_cb) == 'function' then
    local cb_ok, cb_err = pcall(open_cb, abs_path, relative_path or cwd_relative)
    if not cb_ok then vim.notify('open_file_under_cursor open_cb error: ' .. tostring(cb_err), vim.log.levels.ERROR) end
  end

  local current_win = vim.api.nvim_get_current_win()
  local current_buf = vim.api.nvim_get_current_buf()
  local current_buftype = vim.api.nvim_get_option_value('buftype', { buf = current_buf })
  local current_modifiable = vim.api.nvim_get_option_value('modifiable', { buf = current_buf })
  local current_winfixbuf = utils.window_has_winfixbuf(current_win)

  local opened_via_split = false
  if current_buftype ~= '' or not current_modifiable or current_winfixbuf then
    local suitable_win = utils.find_suitable_window()
    if suitable_win then
      vim.api.nvim_set_current_win(suitable_win)
    elseif current_winfixbuf then
      vim.cmd('split ' .. vim.fn.fnameescape(cwd_relative))
      opened_via_split = true
    end
  end

  if not opened_via_split then vim.cmd('edit ' .. vim.fn.fnameescape(cwd_relative)) end

  if location then vim.schedule(function() require('fff.location_utils').jump_to_location(location) end) end
end

--- Try to open the file/path under the cursor.
---
--- Picks up the `<cWORD>` (whitespace-delimited token) from the current line,
--- strips wrapping punctuation (`[]`, `()`, quotes, trailing `,`/`.`/etc.),
--- and tries to open it. Resolution order:
---
---   1. **Direct path**: if the cWORD looks like a path (has `/`, `\`, or
---      `~`) and resolves to a real file (absolute, or relative to the
---      picker's `base_path`, then to neovim's cwd), open it directly. A
---      `:line:col` suffix is parsed and the cursor jumps to that location.
---      This skips the fuzzy picker entirely — when the user has clearly
---      typed a path, we don't second-guess them.
---   2. **Fuzzy match**: otherwise run a fuzzy search. If exactly one file
---      matches, or the top hit is an exact-path match, open it.
---   3. **Picker UI fallback**: if the cWORD looks like a path but several
---      files match ambiguously, open the picker UI with the cWORD as a
---      starter query.
---   4. **No-op**: if the cWORD is empty or matches nothing (and isn't a
---      resolvable path), do nothing — no surprise UI popup.
---
--- `:edit` is window-aware: if the current window has `winfixbuf` or a
--- special buftype, the file is opened in another suitable window or via
--- `:split`.
---
--- The optional `open_cb` is invoked **before** `:edit` runs with
--- `(absolute_path, relative_path)` — useful for plugins that want to mirror
--- the open into a side panel, log the access, etc.
--- @param open_cb fun(abs_path: string, relative_path: string)|nil
function M.open_file_under_cursor(open_cb)
  local raw_word = vim.fn.expand('<cWORD>')
  local query = strip_path_wrappers(raw_word)
  if not query or query == '' then return end

  -- Fast path: cWORD looks like a path AND resolves on disk → just open it.
  -- This catches the common `gf`-on-`./file_picker.rs` case where the fuzzy
  -- search would otherwise return many substring matches and pop the UI.
  local path_part, location = split_location_suffix(query)
  if looks_like_path(path_part) then
    local resolved = resolve_existing_file(path_part)
    if resolved then
      open_resolved_file(resolved, path_part, location, open_cb)
      return
    end
  end

  local picker_ok, picker_ui = pcall(require, 'fff.picker_ui.picker_ui')
  if not picker_ok then
    vim.notify('Failed to load picker UI', vim.log.levels.ERROR)
    return
  end

  picker_ui.open_with_callback(query, function(files, _, fuzzy_location, get_file_score)
    -- Empty results: don't pop up the picker UI on words that aren't paths.
    if not files or #files == 0 then return true end

    local first_score = get_file_score and get_file_score(1) or nil
    local exact = first_score and first_score.exact_match or false
    if #files ~= 1 and not exact then
      -- Ambiguous: let the picker UI surface the candidates.
      return false
    end

    local utils = require('fff.utils')
    local item = files[1]
    local abs_path = utils.canonicalize_fff_path(item.relative_path)
    if not abs_path then return true end

    open_resolved_file(abs_path, item.relative_path, fuzzy_location, open_cb)
    return true
  end)
end

return M
