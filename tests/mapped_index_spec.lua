local fff = require('fff')

describe('mapped directory index', function()
  local tmp, root, index

  before_each(function()
    tmp = vim.fn.tempname()
    root = tmp .. '/project'
    index = tmp .. '/project.fff'
    vim.fn.mkdir(root, 'p')
    vim.fn.writefile({ 'original' }, root .. '/mapped_marker.rs')
    vim.g.fff =
      { base_path = root, frecency = { db_path = tmp .. '/frecency' }, history = { db_path = tmp .. '/history' } }
  end)

  after_each(function()
    fff.clear_cache('all')
    vim.fn.delete(tmp, 'rf')
  end)

  it('uses the saved file list and reads current contents', function()
    assert.equals(1, #fff.file_search('mapped_marker', { wait_for_index_ms = 10000 }).items)
    assert.is_true(fff.save_index(index) > 32)
    assert.is_true(fff.clear_cache('files'))
    vim.fn.writefile({ 'uniqueCurrentContents' }, root .. '/mapped_marker.rs')
    vim.fn.writefile({ 'late' }, root .. '/lateaddition.rs')
    require('fff.conf').get().mapped_index = index
    assert.equals(1, #fff.file_search('mapped_marker').items)
    assert.equals(0, #fff.file_search('lateaddition').items)
    assert.equals(1, #fff.content_search('uniqueCurrentContents').items)
  end)
end)
