local M = {}
local system = require('fff.utils.system')
local fs_utils = require('fff.utils.fs')
local fff_version = require('fff.utils.version')

local GITHUB_REPO = 'dmtrKovalenko/fff'

local function get_binary_dir(plugin_dir) return plugin_dir .. '/../target/release' end

local function get_binary_path(plugin_dir)
  local binary_dir = get_binary_dir(plugin_dir)
  local extension = system.get_lib_extension()
  return binary_dir .. '/libfff_nvim.' .. extension
end

local function binary_exists(plugin_dir)
  local binary_path = get_binary_path(plugin_dir)
  local stat = vim.uv.fs_stat(binary_path)
  if stat and stat.type == 'file' then return true end

  -- On Windows the rename over a loaded DLL fails, so a verified binary may be
  -- left at binary_path .. '.tmp'. Promote it now that the old session is gone.
  local tmp_path = binary_path .. '.tmp'
  local tmp_stat = vim.uv.fs_stat(tmp_path)
  if tmp_stat and tmp_stat.type == 'file' then
    -- Verify the .tmp is a valid library before promoting it, in case the
    -- process was killed between the loadlib check and the rename attempt
    -- during a previous download, leaving a corrupt or partial .tmp on disk.
    local loader = package.loadlib(tmp_path, 'luaopen_fff_nvim')
    if not loader then
      vim.uv.fs_unlink(tmp_path)
      return false
    end
    local ok = vim.uv.fs_rename(tmp_path, binary_path)
    return ok ~= nil
  end

  return false
end

local function download_file(url, output_path, opts, callback)
  opts = opts or {}

  local dir = vim.fn.fnamemodify(output_path, ':h')
  fs_utils.mkdir_recursive(dir, function(mkdir_ok, mkdir_err)
    if not mkdir_ok then
      callback(false, mkdir_err)
      return
    end

    local curl_args = {
      'curl',
      '--fail',
      '--location',
      '--silent',
      '--show-error',
      '--output',
      output_path,
    }

    if opts.proxy then
      table.insert(curl_args, '--proxy')
      table.insert(curl_args, opts.proxy)
    end

    if opts.extra_curl_args then
      for _, arg in ipairs(opts.extra_curl_args) do
        table.insert(curl_args, arg)
      end
    end

    table.insert(curl_args, url)
    vim.system(curl_args, {}, function(result)
      if result.code ~= 0 then
        callback(false, 'Failed to download: ' .. (result.stderr or 'unknown error'))
        return
      end
      callback(true, nil)
    end)
  end)
end

local function download_from_github(version, binary_path, opts, callback)
  opts = opts or {}

  local triple = system.get_triple()
  local extension = system.get_lib_extension()
  local binary_name = triple .. '.' .. extension
  local url = string.format('https://github.com/%s/releases/download/%s/%s', GITHUB_REPO, version, binary_name)

  vim.schedule(function()
    vim.notify(string.format('Downloading fff.nvim binary for ' .. version), vim.log.levels.INFO)
    vim.notify(string.format('Do not open fff until you see a success notification.'), vim.log.levels.WARN)
  end)

  -- Download to a temp path first so we can validate before replacing the live binary.
  -- If we wrote directly to binary_path and the current process already has the old
  -- library loaded, package.loadlib() on the same path returns the *cached* handle —
  -- meaning a truncated download would pass validation silently.
  -- Using a distinct temp path forces dlopen to load the new file for real.
  local tmp_path = binary_path .. '.tmp'

  download_file(url, tmp_path, {
    proxy = opts.proxy,
    extra_curl_args = opts.extra_curl_args,
  }, function(success, err)
    if not success then
      vim.uv.fs_unlink(tmp_path)
      callback(false, err)
      return
    end

    vim.schedule(function()
      -- Validate the downloaded binary by actually loading it (temp path is not yet
      -- loaded by this process, so dlopen loads the new file for real and catches
      -- truncated or corrupt downloads).
      -- Note: package.loadlib returns (nil, error_string) on failure rather than throwing.
      local loader, load_err = package.loadlib(tmp_path, 'luaopen_fff_nvim')

      if not loader then
        vim.uv.fs_unlink(tmp_path)
        callback(false, 'Downloaded binary is not valid: ' .. (load_err or 'unknown error'))
        return
      end

      -- Atomically replace the live binary only after successful validation.
      -- On Windows the old .dll may be locked by the current process, so rename can
      -- fail if fff is already loaded. In that case, leave the verified .tmp on disk
      -- so the next Neovim start can pick it up automatically.
      local rename_ok, rename_err = vim.uv.fs_rename(tmp_path, binary_path)
      if not rename_ok then
        if vim.uv.os_uname().sysname:lower():match('windows') then
          vim.notify(
            'fff.nvim binary downloaded to '
              .. tmp_path
              .. '.\nThe live binary is locked by the current session — please restart Neovim to apply the update.',
            vim.log.levels.WARN
          )
          callback(true, nil)
        else
          vim.uv.fs_unlink(tmp_path)
          callback(false, 'Failed to install binary: ' .. (rename_err or 'unknown error'))
        end
        return
      end

      vim.notify('fff.nvim binary downloaded successfully!', vim.log.levels.INFO)
      callback(true, nil)
    end)
  end)
end

function M.ensure_downloaded(opts, callback)
  opts = opts or {}
  local plugin_dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':h:h')

  if binary_exists(plugin_dir) and not opts.force then
    callback(true, nil)
    return
  end

  local function on_release_tag(release_tag)
    if not release_tag then
      callback(false, 'Could not determine target version')
      return
    end

    local binary_path = get_binary_path(plugin_dir)
    download_from_github(release_tag, binary_path, opts, callback)
  end

  if opts.version then
    on_release_tag(opts.version)
  else
    -- plugin_dir is <repo>/lua; parent is the repo root
    local repo_root = vim.fn.fnamemodify(plugin_dir, ':h')

    -- 1. Try reading the CI-created tag on HEAD (no version computation)
    local tag = fff_version.current_release_tag(repo_root)
    if tag then
      on_release_tag(tag)
      return
    end

    -- 2. No local tag — construct the nightly version (bumps patch so
    --    the prerelease is higher than Cargo.toml base in semver)
    local info, err = fff_version.resolve(repo_root)
    if info then
      on_release_tag(info.release_tag)
      return
    end

    callback(false, err or 'Could not determine target version')
  end
end

function M.download_binary(callback)
  M.ensure_downloaded({ force = true }, function(success, err)
    if not success then
      if callback then
        callback(false, err)
      else
        vim.schedule(
          function()
            vim.notify('Failed to download fff.nvim binary: ' .. (err or 'unknown error'), vim.log.levels.ERROR)
          end
        )
      end
      return
    end
    if callback then callback(true, nil) end
  end)
end

function M.build_binary(callback)
  local plugin_dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':h:h')
  local has_rustup = vim.fn.executable('rustup') == 1
  if not has_rustup then
    callback(
      false,
      'rustup is not found. It is required to build the fff.nvim binary. Install it from https://rustup.rs/'
    )
    return
  end

  vim.system({ 'cargo', 'build', '--release', '-p', 'fff-nvim' }, { cwd = plugin_dir }, function(result)
    if result.code ~= 0 then
      callback(false, 'Failed to build rust binary: ' .. (result.stderr or 'unknown error'))
      return
    end
    callback(true, nil)
  end)
end

function M.download_or_build_binary()
  local done = false
  local fatal_error = nil

  M.ensure_downloaded({ force = true }, function(download_success, download_error)
    if download_success then
      done = true
      return
    end

    vim.schedule(
      function()
        vim.notify(
          'Error downloading binary: ' .. (download_error or 'unknown error') .. '\nTrying cargo build --release\n',
          vim.log.levels.WARN
        )
      end
    )

    M.build_binary(function(build_success, build_error)
      if not build_success then
        fatal_error = 'Failed to build fff.nvim binary. Build error: ' .. (build_error or 'unknown error')
      else
        vim.schedule(function() vim.notify('fff.nvim binary built successfully!', vim.log.levels.INFO) end)
      end
      done = true
    end)
  end)

  -- Block the caller (and keep the Neovim event loop alive) until the entire
  -- download-or-build chain finishes.  This is critical for lazy.nvim build
  -- hooks: lazy returns from the hook immediately after this function returns,
  -- and if Neovim exits before the final rename(tmp → libfff_nvim.{dylib,so,dll})
  -- executes, the binary is never written to disk.  vim.wait pumps the event
  -- loop so all vim.system / vim.schedule callbacks can fire.
  local timeout_ms = 1000 * 60 * 2 -- 2 minutes
  local ok, wait_err = vim.wait(timeout_ms, function() return done end, 100)
  if not ok and wait_err == -2 then error('fff.nvim: download_or_build_binary timed out') end

  if fatal_error then error(fatal_error) end
end

function M.get_binary_path()
  local plugin_dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':h:h')
  return get_binary_path(plugin_dir)
end

function M.get_binary_cpath_component()
  local plugin_dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':h:h')
  local binary_dir = get_binary_dir(plugin_dir)
  local extension = system.get_lib_extension()
  return binary_dir .. '/lib?.' .. extension
end

return M
