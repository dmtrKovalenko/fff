local fuzzy = require('fff.fuzzy')
if not fuzzy then error('Failed to load fff.fuzzy module. Ensure the Rust backend is compiled and available.') end

local M = {}

local fs_scanning_refusal

---@class fff.core.State
local state = {
  ---@type boolean
  initialized = false,
  ---@type boolean
  file_picker_initialized = false,
}

---@param config table
local function setup_global_autocmds(config)
  local group = vim.api.nvim_create_augroup('fff_file_tracking', { clear = true })

  if config.frecency.enabled then
    vim.api.nvim_create_autocmd({ 'BufEnter' }, {
      group = group,
      desc = 'Track file access for FFF frecency',
      callback = function(args)
        local file_path = args.file
        if not (file_path and file_path ~= '' and not vim.startswith(file_path, 'term://')) then return end

        vim.uv.fs_stat(file_path, function(err, stat)
          if err or not stat then return end

          vim.uv.fs_realpath(file_path, function(rp_err, real_path)
            if rp_err or not real_path then return end
            local ok, track_err = pcall(fuzzy.track_access, real_path)

            if not ok then
              vim.schedule(
                function() vim.notify('FFF: Failed to track file access: ' .. tostring(track_err), vim.log.levels.ERROR) end
              )
            end
          end)
        end)
      end,
    })
  end

  -- make sure that this won't work correctly if autochdir plugins are enabled
  -- using a pure :cd command but will work using lua api or :e command
  vim.api.nvim_create_autocmd('DirChanged', {
    group = group,
    callback = function()
      -- Window-local `:lcd` / `:tcd` are per-window — they don't change the
      -- effective project root for the picker, so bail before touching
      -- anything else.
      if vim.v.event.scope == 'window' then return end
      if not state.initialized then return end

      local new_cwd = vim.v.event.cwd
      if not new_cwd or new_cwd == '' then return end

      -- Canonicalize both sides before comparing. `vim.v.event.cwd` is
      -- whatever the caller passed to `:cd` (often unexpanded, sometimes
      -- containing `~` or symlinks), while `config.base_path` is the form
      -- the picker was last re-indexed against (post-`expand`). Without
      -- resolving symlinks + ensuring an absolute path, trivially
      -- equivalent paths compare as different (`/private/var/x` vs
      -- `/var/x` on macOS, resolved-vs-unresolved symlinks from LSP root
      -- detection, etc.) and every such mismatch schedules a 450k-file
      -- reindex through the Rust side.
      local function canonicalize(p)
        if not p or p == '' then return p end
        local abs = vim.fn.fnamemodify(vim.fn.expand(p), ':p')
        -- `:p` leaves a trailing slash on directories — strip for
        -- comparison stability.
        abs = abs:gsub('/+$', '')
        local ok, resolved = pcall(vim.fn.resolve, abs)
        return (ok and resolved ~= '') and resolved or abs
      end

      local new_canonical = canonicalize(new_cwd)
      local base_canonical = canonicalize(config.base_path)
      if new_canonical == base_canonical then return end

      vim.schedule(function()
        local change_ok, err = pcall(M.change_indexing_directory, new_canonical)
        if not change_ok then
          vim.notify('FFF: Failed to change indexing directory: ' .. tostring(err), vim.log.levels.ERROR)
        end
      end)
    end,
    desc = 'Automatically sync FFF directory changes',
  })
end

--- @return boolean
M.is_file_picker_initialized = function() return state.file_picker_initialized end

--- Change the base directory for the file picker. Triggers a reindex on the
--- Rust side and updates `config.base_path` so subsequent `:cd` events compare
--- against the new root.
--- @param new_path string New directory path to use as base
--- @return boolean ok `true` if the reindex was scheduled, `false` otherwise
M.change_indexing_directory = function(new_path)
  if not new_path or new_path == '' then
    vim.notify('Directory path is required', vim.log.levels.ERROR)
    return false
  end

  local expanded_path = vim.fn.expand(new_path)
  if vim.fn.isdirectory(expanded_path) ~= 1 then
    vim.notify('Directory does not exist: ' .. expanded_path, vim.log.levels.ERROR)
    return false
  end

  local fff_rust = M.ensure_initialized()
  local config = require('fff.conf').get()

  local refusal = fs_scanning_refusal(vim.tbl_extend('force', config, { base_path = expanded_path }))
  if refusal then
    vim.notify('FFF: ' .. refusal, vim.log.levels.WARN)
    return false
  end

  local ok, err = pcall(fff_rust.restart_index_in_path, expanded_path, {
    follow_symlinks = config.follow_symlinks,
    enable_fs_root_scanning = config.enable_fs_root_scanning,
    enable_home_dir_scanning = config.enable_home_dir_scanning,
    enable_filename_constraint = config.grep and config.grep.enable_filename_constraint,
    git_recency = config.git and config.git.recency,
  })
  if not ok then
    vim.notify('Failed to change directory: ' .. err, vim.log.levels.ERROR)
    return false
  end

  require('fff.conf').get().base_path = expanded_path
  return true
end

--- Reset the file-picker flag so the next `ensure_initialized` recreates the
--- Rust picker. Call after `cleanup_file_picker` drops it (`FFFClearCache`);
--- otherwise the flag stays set and every later call operates on a dropped
--- picker (see #772).
M.mark_file_picker_uninitialized = function() state.file_picker_initialized = false end

M.ensure_initialized = function()
  local config = require('fff.conf').get()

  -- Refusal gates both one-time setup and (re)creating the picker so we never
  -- index fs-root / home, even after a cache clear.
  -- Some folks are complaining that neovim instance is closing if ffi returns error on startup (via lazy=false)
  -- I can't repro so just precheck on lua side to prevent crashing neovim instance
  local refusal = fs_scanning_refusal(config)
  if refusal then
    state.initialized = true
    vim.notify('FFF: ' .. refusal, vim.log.levels.WARN)
    return fuzzy
  end

  if not state.initialized then
    state.initialized = true
    if config.logging.enabled then
      local log_success, log_error =
        pcall(fuzzy.init_tracing, config.logging.log_file, config.logging.log_level, config.logging.retain_runs)
      if log_success then
        M.log_file_path = log_error
      else
        vim.notify('Failed to initialize logging: ' .. (tostring(log_error) or 'unknown error'), vim.log.levels.WARN)
      end
    end

    local ok, result = pcall(fuzzy.init_db, config.frecency.db_path, config.history.db_path, true)
    if not ok then vim.notify('Failed to databases: ' .. tostring(result), vim.log.levels.WARN) end

    setup_global_autocmds(config)

    local highlights = require('fff.highlights')
    highlights.setup()

    vim.api.nvim_create_autocmd('ColorScheme', {
      group = vim.api.nvim_create_augroup('fff_highlights', { clear = true }),
      callback = function() highlights.setup() end,
      desc = 'Re-apply FFF highlights on colorscheme change',
    })
  end

  -- Recreated whenever the picker was torn down (e.g. `FFFClearCache files`).
  -- Guarded separately from one-time setup so a cache clear rebuilds the
  -- picker instead of leaving a dropped one behind (#772).
  if not state.file_picker_initialized then
    local ok, result = pcall(fuzzy.init_file_picker, config.base_path, {
      follow_symlinks = config.follow_symlinks,
      enable_fs_root_scanning = config.enable_fs_root_scanning,
      enable_home_dir_scanning = config.enable_home_dir_scanning,
      enable_filename_constraint = config.grep and config.grep.enable_filename_constraint,
      git_recency = config.git and config.git.recency,
    })
    if not ok then
      vim.notify('Failed to initialize file picker: ' .. tostring(result), vim.log.levels.ERROR)
      return fuzzy
    end
    state.file_picker_initialized = true
  end

  return fuzzy
end

function fs_scanning_refusal(config)
  local path = vim.fn.fnamemodify(vim.fn.expand(config.base_path), ':p'):gsub('/+$', '')

  if not config.enable_fs_root_scanning and (path == '' or path:match('^%a:$')) then
    return 'Refusing to index filesystem root. Set enable_fs_root_scanning = true to override.'
  end

  if not config.enable_home_dir_scanning then
    local home = (vim.fn.expand('$HOME') or ''):gsub('/+$', '')
    if home ~= '' and path == home then
      return 'Refusing to index home directory. Set enable_home_dir_scanning = true to override.'
    end
  end

  return nil
end

return M
