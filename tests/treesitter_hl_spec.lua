local assert = require('luassert')
local highlights = require('fff.treesitter_hl')
local language = vim.treesitter.language

describe('grep parser detection', function()
  local add = language.add

  local function detect_with(loader)
    language.add = loader
    return highlights.lang_from_filename('test.lua')
  end

  after_each(function() language.add = add end)

  it('skips parsers that fail to load', function()
    assert.is_nil(detect_with(function() return nil, 'no parser' end))
    assert.is_nil(detect_with(function() error('no parser') end))
  end)

  it('accepts both current and Neovim 0.10 success values', function()
    assert.are.equal('lua', detect_with(function() return true end))
    assert.are.equal('lua', detect_with(function() end))
  end)
end)
