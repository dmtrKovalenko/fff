---@diagnostic disable: undefined-field, need-check-nil, param-type-mismatch
local version = require('fff.utils.version')

describe('fff.utils.version', function()
  local repo_root

  before_each(function()
    repo_root = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':h:h')
    if vim.fn.isdirectory(repo_root) ~= 1 then repo_root = vim.fn.getcwd() end
  end)

  describe('read_base_version', function()
    it('should read version from Cargo.toml', function()
      local ver = version.read_base_version(repo_root)
      assert.is_not_nil(ver)
      assert.is_string(ver)
      assert.is_truthy(ver:match('^%d+%.%d+%.%d+'), 'expected semver, got: ' .. ver)
    end)

    it(
      'should return nil for missing directory',
      function() assert.is_nil(version.read_base_version('/nonexistent_path_12345')) end
    )

    it('should parse version from a temp Cargo.toml', function()
      local tmp = vim.fn.tempname()
      vim.fn.mkdir(tmp .. '/crates/fff-core', 'p')

      local f = io.open(tmp .. '/crates/fff-core/Cargo.toml', 'w')
      f:write('[package]\nname = "test"\nversion = "1.2.3"\n')
      f:close()

      assert.are.equal('1.2.3', version.read_base_version(tmp))
      vim.fn.delete(tmp, 'rf')
    end)
  end)

  describe('current_release_tag', function()
    it('should return a string or nil for the real repo', function()
      local tag = version.current_release_tag(repo_root)
      -- On CI the commit has a tag; locally it might not
      if tag then
        assert.is_string(tag)
      else
        assert.is_nil(tag)
      end
    end)

    it('should return nil for a repo with no tags', function()
      local tmp = vim.fn.tempname()
      vim.fn.mkdir(tmp, 'p')

      vim.fn.system({ 'git', 'init', '-q', tmp })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.email', 'test@test.com' })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.name', 'Test' })

      local f = io.open(tmp .. '/file.txt', 'w')
      f:write('hello')
      f:close()

      vim.fn.system({ 'git', '-C', tmp, 'add', '.' })
      vim.fn.system({ 'git', '-C', tmp, 'commit', '-q', '-m', 'init' })

      assert.is_nil(version.current_release_tag(tmp))
      vim.fn.delete(tmp, 'rf')
    end)

    it('should prefer v* tags over nightly/dev/legacy', function()
      local tmp = vim.fn.tempname()
      vim.fn.mkdir(tmp, 'p')

      vim.fn.system({ 'git', 'init', '-q', tmp })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.email', 'test@test.com' })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.name', 'Test' })

      local f = io.open(tmp .. '/file.txt', 'w')
      f:write('hello')
      f:close()

      vim.fn.system({ 'git', '-C', tmp, 'add', '.' })
      vim.fn.system({ 'git', '-C', tmp, 'commit', '-q', '-m', 'init' })

      -- Add multiple tags on the same commit
      vim.fn.system({ 'git', '-C', tmp, 'tag', '0.4.0-dev.abc1234' })
      vim.fn.system({ 'git', '-C', tmp, 'tag', '0.4.0-nightly.abc1234' })
      vim.fn.system({ 'git', '-C', tmp, 'tag', 'v0.4.0' })
      vim.fn.system({ 'git', '-C', tmp, 'tag', 'deadbeef' })

      assert.are.equal('v0.4.0', version.current_release_tag(tmp))
      vim.fn.delete(tmp, 'rf')
    end)

    it('should prefer nightly over dev', function()
      local tmp = vim.fn.tempname()
      vim.fn.mkdir(tmp, 'p')

      vim.fn.system({ 'git', 'init', '-q', tmp })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.email', 'test@test.com' })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.name', 'Test' })

      local f = io.open(tmp .. '/file.txt', 'w')
      f:write('hello')
      f:close()

      vim.fn.system({ 'git', '-C', tmp, 'add', '.' })
      vim.fn.system({ 'git', '-C', tmp, 'commit', '-q', '-m', 'init' })

      vim.fn.system({ 'git', '-C', tmp, 'tag', '0.4.0-dev.abc1234' })
      vim.fn.system({ 'git', '-C', tmp, 'tag', '0.4.0-nightly.abc1234' })

      assert.are.equal('0.4.0-nightly.abc1234', version.current_release_tag(tmp))
      vim.fn.delete(tmp, 'rf')
    end)
  end)

  describe('resolve', function()
    it('should resolve a version from the real repo', function()
      local info, err = version.resolve(repo_root)
      assert.is_nil(err)
      assert.is_not_nil(info)
      assert.is_string(info.version)
      assert.is_string(info.release_tag)
      assert.is_string(info.npm_tag)
      assert.is_not_nil(info.is_release)
    end)

    it('should produce a version higher than Cargo.toml base', function()
      local info = version.resolve(repo_root)
      local base = version.read_base_version(repo_root)
      assert.is_not_nil(info)
      assert.is_not_nil(base)
      -- For prereleases, patch is bumped: 0.4.0 → 0.4.1-nightly.{sha}
      if not info.is_release then
        local base_major, base_minor, base_patch = base:match('^(%d+)%.(%d+)%.(%d+)')
        local expected_patch = tostring(tonumber(base_patch) + 1)
        assert.is_truthy(
          info.version:find(base_major .. '%.' .. base_minor .. '%.' .. expected_patch),
          'version "' .. info.version .. '" should have bumped patch from base "' .. base .. '"'
        )
      end
    end)

    it('should return dev on a non-main branch', function()
      local tmp = vim.fn.tempname()
      vim.fn.mkdir(tmp .. '/crates/fff-core', 'p')

      vim.fn.system({ 'git', 'init', '-q', tmp })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.email', 'test@test.com' })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.name', 'Test' })

      local f = io.open(tmp .. '/crates/fff-core/Cargo.toml', 'w')
      f:write('[package]\nname = "test"\nversion = "1.0.0"\nedition = "2024"\n')
      f:close()

      vim.fn.system({ 'git', '-C', tmp, 'add', '.' })
      vim.fn.system({ 'git', '-C', tmp, 'commit', '-q', '-m', 'init' })
      vim.fn.system({ 'git', '-C', tmp, 'checkout', '-b', 'feature-x' })

      local info = version.resolve(tmp)
      assert.is_not_nil(info)
      assert.are.equal('dev', info.npm_tag)
      assert.is_false(info.is_release)
      assert.is_truthy(info.version:find('-dev%.'), 'expected dev prerelease, got: ' .. info.version)
      assert.is_truthy(info.version:find('^1%.0%.1%-'), 'expected bumped patch 1.0.1, got: ' .. info.version)

      vim.fn.delete(tmp, 'rf')
    end)

    it('should return nightly on main branch', function()
      local tmp = vim.fn.tempname()
      vim.fn.mkdir(tmp .. '/crates/fff-core', 'p')

      vim.fn.system({ 'git', 'init', '-q', '-b', 'main', tmp })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.email', 'test@test.com' })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.name', 'Test' })

      local f = io.open(tmp .. '/crates/fff-core/Cargo.toml', 'w')
      f:write('[package]\nname = "test"\nversion = "2.0.0"\nedition = "2024"\n')
      f:close()

      vim.fn.system({ 'git', '-C', tmp, 'add', '.' })
      vim.fn.system({ 'git', '-C', tmp, 'commit', '-q', '-m', 'init' })

      local info = version.resolve(tmp)
      assert.is_not_nil(info)
      assert.are.equal('nightly', info.npm_tag)
      assert.is_false(info.is_release)
      assert.is_truthy(info.version:find('2%.0%.1%-nightly%.'), 'expected 2.0.1-nightly, got: ' .. info.version)

      vim.fn.delete(tmp, 'rf')
    end)

    it('should use a 7 char sha even when git abbreviates longer', function()
      local tmp = vim.fn.tempname()
      vim.fn.mkdir(tmp .. '/crates/fff-core', 'p')

      vim.fn.system({ 'git', 'init', '-q', '-b', 'main', tmp })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.email', 'test@test.com' })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.name', 'Test' })
      -- a full clone abbreviates longer than CI's shallow checkout that creates the tags
      vim.fn.system({ 'git', '-C', tmp, 'config', 'core.abbrev', '12' })

      local f = io.open(tmp .. '/crates/fff-core/Cargo.toml', 'w')
      f:write('[package]\nname = "test"\nversion = "2.0.0"\nedition = "2024"\n')
      f:close()

      vim.fn.system({ 'git', '-C', tmp, 'add', '.' })
      vim.fn.system({ 'git', '-C', tmp, 'commit', '-q', '-m', 'init' })

      local head = vim.trim(vim.fn.system({ 'git', '-C', tmp, 'rev-parse', 'HEAD' }))
      local info = version.resolve(tmp)
      assert.is_not_nil(info)
      assert.are.equal('2.0.1-nightly.' .. head:sub(1, 7), info.release_tag)

      vim.fn.delete(tmp, 'rf')
    end)

    it('should return stable release for v* tagged commit', function()
      local tmp = vim.fn.tempname()
      vim.fn.mkdir(tmp .. '/crates/fff-core', 'p')

      vim.fn.system({ 'git', 'init', '-q', tmp })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.email', 'test@test.com' })
      vim.fn.system({ 'git', '-C', tmp, 'config', 'user.name', 'Test' })

      local f = io.open(tmp .. '/crates/fff-core/Cargo.toml', 'w')
      f:write('[package]\nname = "test"\nversion = "3.0.0"\nedition = "2024"\n')
      f:close()

      vim.fn.system({ 'git', '-C', tmp, 'add', '.' })
      vim.fn.system({ 'git', '-C', tmp, 'commit', '-q', '-m', 'release' })
      vim.fn.system({ 'git', '-C', tmp, 'tag', 'v3.0.0' })

      local info = version.resolve(tmp)
      assert.is_not_nil(info)
      assert.are.equal('v3.0.0', info.release_tag)
      assert.are.equal('3.0.0', info.version)
      assert.are.equal('latest', info.npm_tag)
      assert.is_true(info.is_release)

      vim.fn.delete(tmp, 'rf')
    end)

    it('should fail for a non-git directory', function()
      local tmp = vim.fn.tempname()
      vim.fn.mkdir(tmp .. '/crates/fff-core', 'p')

      local f = io.open(tmp .. '/crates/fff-core/Cargo.toml', 'w')
      f:write('[package]\nname = "test"\nversion = "1.0.0"\n')
      f:close()

      local info, err = version.resolve(tmp)
      assert.is_nil(info)
      assert.is_not_nil(err)

      vim.fn.delete(tmp, 'rf')
    end)
  end)
end)
