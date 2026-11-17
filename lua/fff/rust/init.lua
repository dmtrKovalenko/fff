local download = require('fff.download')

local is_windows = jit.os:lower() == 'windows'

--- @return string
local function get_lib_extension()
  if jit.os:lower() == 'mac' or jit.os:lower() == 'osx' then return '.dylib' end
  if is_windows then return '.dll' end
  return '.so'
end

--- Resolve a path to an absolute, clean form with native separators.
--- Resolves `..` components and on Windows converts forward slashes to
--- backslashes so that Windows APIs (LoadLibraryEx) can find the file.
--- @param path string
--- @return string
local function resolve_path(path)
  local resolved = vim.fn.fnamemodify(path, ':p')
  if is_windows then resolved = resolved:gsub('/', '\\') end
  return resolved
end

-- Determine base_path from the location of this Lua file
local info = debug.getinfo(1, 'S')
-- Match both forward and backslash directory separators for cross-platform support
local base_path = info and info.source and info.source:match('@?(.*[/\\])') or ''

-- Fallback: if base_path is empty, use vim APIs
if not base_path or base_path == '' then
  base_path = vim.fn.fnamemodify(vim.fn.resolve(vim.fn.expand('<sfile>:p')), ':h') .. '/'
end

local paths = {
  download.get_binary_cpath_component(),
  base_path .. '../../../target/release/lib?' .. get_lib_extension(),
  base_path .. '../../../target/release/?' .. get_lib_extension(),
}

local cargo_target_dir = os.getenv('CARGO_TARGET_DIR')
if cargo_target_dir then
  table.insert(paths, cargo_target_dir .. '/release/lib?' .. get_lib_extension())
  table.insert(paths, cargo_target_dir .. '/release/?' .. get_lib_extension())
end

-- Instead of using require (which can find the wrong lib due to cpath pollution),
-- load the library directly from the first valid path we find
local function try_load_library()
  for _, path_pattern in ipairs(paths) do
    local actual_path = resolve_path(path_pattern:gsub('%?', 'fff_nvim'))
    local stat = vim.uv.fs_stat(actual_path)
    if stat and stat.type == 'file' then
      local loader, err = package.loadlib(actual_path, 'luaopen_fff_nvim')
      if err then return nil, string.format('Error loading library from %s: %s', actual_path, err) end
      if loader then return loader() end
    end
  end
  return nil, 'No valid library found in any search path'
end

local backend, load_err = try_load_library()
if not backend or load_err then
  local resolved = {}
  for _, p in ipairs(paths) do
    table.insert(resolved, resolve_path(p:gsub('%?', 'fff_nvim')))
  end

  local err_msg = string.format(
    'Failed to load fff rust backend.\nError: %s\nSearched paths:\n%s\nMake sure binary exists or make it exists using \n `:lua require("fff.download").download_or_build_binary()`\nor\n`cargo build --release -p fff-nvim`\n(and rerun neovim after)',
    tostring(load_err),
    vim.inspect(resolved)
  )

  error(err_msg)
end

return backend
