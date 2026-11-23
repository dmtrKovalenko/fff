local highlights = require('fff.treesitter_hl')
local language = vim.treesitter.language

describe('grep parser detection', function()
  local add = language.add

  after_each(function() language.add = add end)

  it('skips parsers that fail to load', function()
    language.add = function() return nil, 'no parser' end
    assert.is_nil(highlights.lang_from_filename('test.lua'))
    language.add = function() error('no parser') end
    assert.is_nil(highlights.lang_from_filename('test.lua'))
  end)

  it('accepts both current and Neovim 0.10 success values', function()
    language.add = function() return true end
    assert.are.equal('lua', highlights.lang_from_filename('test.lua'))
    language.add = function() end
    assert.are.equal('lua', highlights.lang_from_filename('test.lua'))
  end)
end)
