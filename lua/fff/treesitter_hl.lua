local utils = require('fff.utils')

local M = {}

--- Per-language scratch buffer cache
--- @type table<string, number>
local scratch_bufs = {}

--- Get or create a scratch buffer for a given treesitter language.
--- The buffer is reused across calls — content is overwritten each time.
--- @param lang string Treesitter language name
--- @return number buf Buffer handle
local function get_scratch_buf(lang)
  local buf = scratch_bufs[lang]
  if buf and vim.api.nvim_buf_is_valid(buf) then return buf end

  buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_buf_set_name(buf, 'fff://treesitter/' .. lang)
  vim.bo[buf].bufhidden = 'hide'
  vim.bo[buf].buftype = 'nofile'
  vim.bo[buf].swapfile = false
  vim.bo[buf].undolevels = -1
  scratch_bufs[lang] = buf
  return buf
end

--- Resolve a filename to a treesitter language.
--- Returns nil if no parser is available.
--- @param filename string File name (e.g. "foo.rs")
--- @return string|nil lang Treesitter language name, or nil
function M.lang_from_filename(filename)
  if not filename or filename == '' then return nil end

  local ft = utils.detect_filetype(filename) or 'text'
  local lang_ok, lang = pcall(vim.treesitter.language.get_lang, ft)
  if not lang_ok or not lang then lang = ft end

  -- Neovim 0.10 returns nothing on success. Newer versions return nil and an error on failure.
  local ok, loaded, err = pcall(vim.treesitter.language.add, lang)
  if not ok or loaded == false or err then return nil end

  return lang
end

--- Extract treesitter highlights for a single line of code.
--- Returns an array of { col, end_col, hl_group } tables where col/end_col
--- are 0-based byte offsets within the input string.
---
--- @param text string The line of code to highlight
--- @param lang string Treesitter language name (from lang_from_filename)
--- @return table[] highlights Array of { col: number, end_col: number, hl_group: string }
function M.get_line_highlights(text, lang)
  if not text or text == '' or not lang then return {} end

  local buf = get_scratch_buf(lang)

  -- Write the single line into the scratch buffer
  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, { text })
  vim.bo[buf].modifiable = false

  -- Parse with treesitter
  local ok, parser = pcall(vim.treesitter.get_parser, buf, lang)
  if not ok or not parser then return {} end

  local parse_ok = pcall(parser.parse, parser, true)
  if not parse_ok then return {} end

  local highlights = {}

  parser:for_each_tree(function(tstree, tree)
    if not tstree then return end
    local root = tstree:root()
    if not root then return end

    local tree_lang = tree:lang()
    local query_ok, query = pcall(vim.treesitter.query.get, tree_lang, 'highlights')
    if not query_ok or not query then return end

    for capture, node, _ in query:iter_captures(root, buf, 0, 1) do
      local name = query.captures[capture]
      if name and name ~= 'spell' and name ~= 'conceal' then
        local start_row, start_col, end_row, end_col = node:range()
        -- Only process highlights on line 0 (our single line)
        if start_row == 0 then
          if end_row > 0 then end_col = #text end -- multi-line node: clamp to line end
          if start_col < end_col then
            highlights[#highlights + 1] = {
              col = start_col,
              end_col = end_col,
              hl_group = '@' .. name .. '.' .. tree_lang,
            }
          end
        end
      end
    end
  end)

  return highlights
end

--- Clean up all scratch buffers.
--- Called when the picker closes.
function M.cleanup()
  for lang, buf in pairs(scratch_bufs) do
    if buf and vim.api.nvim_buf_is_valid(buf) then pcall(vim.api.nvim_buf_delete, buf, { force = true }) end
    scratch_bufs[lang] = nil
  end
end

return M
