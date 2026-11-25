if vim.g.fff_loaded then return end
vim.g.fff_loaded = true

-- Defer indexing until after UIEnter, so as to not block the UI.
-- This is equivalent to lazy.nvim's VeryLazy, but works with all plugin managers.

local init = vim.schedule_wrap(function()
  if vim.v.exiting ~= vim.NIL then return end
  -- PERF: We query the vim.g.fff config to avoid eagerly requiring Lua modules
  local lazy_sync = vim.tbl_get(vim.g, 'fff', 'lazy_sync')
  if lazy_sync == nil or not lazy_sync then require('fff.core').ensure_initialized() end
end)

if vim.v.vim_did_enter == 1 then
  init()
else
  vim.api.nvim_create_autocmd('UIEnter', {
    group = vim.api.nvim_create_augroup('fff.main', {}),
    once = true,
    nested = true,
    callback = init,
  })
end

vim.api.nvim_create_user_command('FFFResume', function() require('fff').resume() end, {
  desc = 'Resume the last FFF picker (restores query, results, cursor, and mode)',
})

vim.api.nvim_create_user_command('FFFFind', function(opts)
  local fff = require('fff')
  if opts.args and opts.args ~= '' then
    -- If argument looks like a directory, use it as base path
    if vim.fn.isdirectory(opts.args) == 1 then
      fff.find_files_in_dir(opts.args)
    else
      -- Otherwise treat as search query
      fff.search_and_show(opts.args)
    end
  else
    fff.find_files()
  end
end, {
  nargs = '?',
  complete = function(arg_lead)
    -- Complete with directories and common search terms
    local dirs = vim.fn.glob(arg_lead .. '*', false, true)
    local results = {}
    for _, dir in ipairs(dirs) do
      if vim.fn.isdirectory(dir) == 1 then table.insert(results, dir) end
    end
    return results
  end,
  desc = 'Find files with FFF (use directory path or search query)',
})

vim.api.nvim_create_user_command('FFFScan', function() require('fff').scan_files() end, {
  desc = 'Scan files for FFF',
})

vim.api.nvim_create_user_command('FFFRefreshGit', function() require('fff').refresh_git_status() end, {
  desc = 'Manually refresh git status for all files',
})

vim.api.nvim_create_user_command('FFFTogglePreview', function() require('fff').toggle_preview() end, {
  desc = 'Toggle the FFF preview panel',
})

vim.api.nvim_create_user_command('FFFToggleMaximize', function() require('fff').toggle_maximize() end, {
  desc = 'Toggle FFF maximize layout',
})

vim.api.nvim_create_user_command('FFFClearCache', function(opts) require('fff').clear_cache(opts.args) end, {
  nargs = '?',
  complete = function() return { 'all', 'frecency', 'files' } end,
  desc = 'Clear FFF caches (all|frecency|files)',
})

vim.api.nvim_create_user_command('FFFHealth', function() vim.cmd('checkhealth fff') end, {
  desc = 'Check FFF health',
})

vim.api.nvim_create_user_command('FFFDebug', function(opts)
  local config = require('fff.conf').get()
  if opts.args == 'toggle' or opts.args == '' then
    config.debug.show_scores = not config.debug.show_scores
    config.debug.enabled = config.debug.show_scores
    local status = config.debug.show_scores and 'enabled' or 'disabled'
    vim.notify('FFF debug scores ' .. status, vim.log.levels.INFO)
  elseif opts.args == 'on' then
    config.debug.show_scores = true
    config.debug.enabled = true
    vim.notify('FFF debug scores enabled', vim.log.levels.INFO)
  elseif opts.args == 'off' then
    config.debug.show_scores = false
    config.debug.enabled = false
    vim.notify('FFF debug scores disabled', vim.log.levels.INFO)
  else
    vim.notify('Usage: :FFFDebug [on|off|toggle]', vim.log.levels.ERROR)
  end
end, {
  nargs = '?',
  complete = function() return { 'on', 'off', 'toggle' } end,
  desc = 'Toggle FFF debug scores display',
})

vim.api.nvim_create_user_command('FFFOpenLog', function()
  local core = require('fff.core')
  core.ensure_initialized()

  if core.log_file_path then
    vim.cmd('tabnew ' .. vim.fn.fnameescape(core.log_file_path))
  else
    vim.notify('Log file path not available', vim.log.levels.ERROR)
  end
end, {
  desc = 'Open FFF log file in new tab',
})
