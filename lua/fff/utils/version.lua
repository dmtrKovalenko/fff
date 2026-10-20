local M = {}

local is_windows = (package.config:sub(1, 1) == '\\')

--- Shell-quote a string for safe interpolation into a command.
---@param s string
---@return string
local function shell_quote(s)
  if is_windows then return '"' .. s:gsub('"', '\\"') .. '"' end
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

--- Run a git command in the given repository and return trimmed stdout.
---@param repo_root string
---@return string|nil output trimmed stdout, or nil on failure
local function git(repo_root, ...)
  local parts = { 'git', '-C', shell_quote(repo_root) }
  for i = 1, select('#', ...) do
    parts[#parts + 1] = shell_quote(select(i, ...))
  end

  local redirect = is_windows and ' 2>NUL' or ' 2>/dev/null'
  local handle = io.popen(table.concat(parts, ' ') .. redirect)
  if not handle then return nil end

  local output = handle:read('*a')
  handle:close()

  if not output or output:match('^%s*$') then return nil end
  return output:gsub('%s+$', '')
end

function M.current_release_tag(repo_root)
  local raw = git(repo_root, 'tag', '--points-at', 'HEAD')
  if not raw then return nil end

  -- A nightly commit carries both the per-sha tag (e.g. 0.9.2-nightly.abc1234,
  -- which owns a permanent release with that commit's binaries) and the rolling
  -- `nightly` alias. Prefer the per-sha tag so a pinned install downloads the
  -- binary built for its own commit, not whatever `nightly` currently points at.
  local stable, nightly_versioned, nightly_alias, dev, other
  for tag in raw:gmatch('[^\n]+') do
    if tag:match('^v%d') then
      stable = tag
    elseif tag:match('%-nightly%.') then
      nightly_versioned = tag
    elseif tag == 'nightly' then
      nightly_alias = tag
    elseif tag:match('%-dev%.') then
      dev = tag
    else
      other = tag
    end
  end

  return stable or nightly_versioned or dev or other or nightly_alias
end

function M.read_base_version(repo_root)
  local cargo_path = repo_root .. '/crates/fff-core/Cargo.toml'
  local f = io.open(cargo_path, 'r')
  if not f then return nil end

  for line in f:lines() do
    local ver = line:match('^version%s*=%s*"([^"]+)"')
    if ver then
      f:close()
      return ver
    end
  end

  f:close()
  return nil
end

---@class FFFVersionInfo
---@field version string semver version (e.g. "0.4.0" or "0.4.1-nightly.abc1234")
---@field release_tag string permanent GitHub release tag clients download from (per-sha for nightlies)
---@field is_release boolean true for tagged stable releases
---@field npm_tag string "latest"|"nightly"|"dev"

--- Bump the patch component of a semver string.
--- "1.2.3" → "1.2.4"
---@param version string
---@return string|nil bumped version, or nil if parsing fails
local function bump_patch(version)
  local major, minor, patch = version:match('^(%d+)%.(%d+)%.(%d+)')
  if not major then return nil end
  return string.format('%s.%s.%d', major, minor, tonumber(patch) + 1)
end

--- Compute the version for a new release based on git state.
--- Used by CI to determine what tag to create — NOT for downloads.
---
--- For prerelease versions the patch is bumped so that the result is
--- higher than the current Cargo.toml version in semver ordering.
--- This is required for `cargo set-version` / crates.io publishing
--- (0.4.1-nightly.x > 0.4.0, whereas 0.4.0-nightly.x < 0.4.0).
---
---   tagged release (v*)          → version from tag,          npm_tag = "latest"
---   main branch                  → {base+1}-nightly.{sha},   npm_tag = "nightly"
---   detached HEAD                → {base+1}-nightly.{sha},   npm_tag = "nightly"
---   other branch (PR / feature)  → {base+1}-dev.{sha},       npm_tag = "dev"
---
---@param repo_root string absolute path to the repository root
---@return FFFVersionInfo|nil info
---@return string|nil err
function M.resolve(repo_root)
  local tag = git(repo_root, 'describe', '--exact-match', '--tags', '--match', 'v*', 'HEAD')

  if tag and tag:match('^v%d') then
    return {
      version = tag:sub(2),
      release_tag = tag,
      is_release = true,
      npm_tag = 'latest',
    }
  end

  -- `--short` grows with the object count, so a full clone and CI's shallow one disagree
  local sha = git(repo_root, 'rev-parse', 'HEAD')
  if not sha then return nil, 'Failed to determine git SHA' end
  local short_sha = sha:sub(1, 7)

  local base_version = M.read_base_version(repo_root)
  if not base_version then return nil, 'Could not read base version from crates/fff-core/Cargo.toml' end

  local next_version = bump_patch(base_version)
  if not next_version then return nil, 'Could not parse base version: ' .. base_version end

  local branch = git(repo_root, 'symbolic-ref', '--short', 'HEAD')

  local prerelease_label, npm_tag
  if not branch or branch == 'main' then
    prerelease_label = 'nightly'
    npm_tag = 'nightly'
  else
    prerelease_label = 'dev'
    npm_tag = 'dev'
  end

  local version = string.format('%s-%s.%s', next_version, prerelease_label, short_sha)
  -- Each nightly gets its own permanent per-sha release/tag so a pinned or stale
  -- install always downloads the binary built for its exact commit. CI also moves
  -- the rolling `nightly` tag to HEAD for "give me latest" tooling, but clients
  -- never resolve to it when their commit has a per-sha tag.
  return {
    version = version,
    release_tag = version,
    is_release = false,
    npm_tag = npm_tag,
  }
end

return M
