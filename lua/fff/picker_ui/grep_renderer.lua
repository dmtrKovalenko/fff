--- Grep search bridge and renderer.
--- Wraps the Rust `live_grep` FFI function with file-based pagination state tracking.
--- Also provides renderer for live grep results with file grouping.
local M = {
  supports_cursor_rerender = true,
}

local fuzzy = require('fff.fuzzy')
local file_renderer = require('fff.picker_ui.file_renderer')
local tresitter_highlight = require('fff.treesitter_hl')

---@class fff.grep.SearchResult
---@field items table[] Array of grep match items
---@field total_matched number Total matches found in this call
---@field total_files_searched number Files actually searched in this call
---@field total_files number Total indexed files
---@field filtered_file_count number Total searchable files after filtering
---@field next_file_offset number File offset to pass for the next page (0 = no more results)
---@field regex_fallback_error string|nil Error message if regex compilation failed and search fell back to literal

local last_result = nil

--- Perform a grep search.
---@param query string The search query (may contain file constraints like *.rs)
---@param file_offset? number Index into sorted file list to start from (default 0)
---@param page_size? number Max matches to collect (default 50)
---@param config? table Grep configuration overrides
---@param grep_mode? string Search mode: "plain" (default), "regex", or "fuzzy"
---@return fff.grep.SearchResult
function M.search(query, file_offset, page_size, config, grep_mode)
  local conf = config or {}
  last_result = fuzzy.live_grep(
    query or '',
    file_offset or 0,
    page_size or 50,
    conf.max_file_size,
    conf.max_matches_per_file,
    conf.smart_case,
    grep_mode or 'plain',
    conf.time_budget_ms,
    conf.trim_whitespace,
    conf.enforce_time_budget
  )
  return last_result
end

--- Get metadata from the last search result.
---@return { total_matched: number, total_files_searched: number, total_files: number, next_file_offset: number }
function M.get_search_metadata()
  if not last_result then
    return { total_matched = 0, total_files_searched = 0, total_files = 0, next_file_offset = 0 }
  end
  return {
    total_matched = last_result.total_matched or 0,
    total_files_searched = last_result.total_files_searched or 0,
    total_files = last_result.total_files or 0,
    next_file_offset = last_result.next_file_offset or 0,
  }
end

-- ===== Renderer =====

--- Build the file group header line using the same layout as file_renderer.
--- Delegates to file_renderer.render_line (with combo disabled).
---@param item FileItem Grep match
---@param ctx table Render context
---@return string The header line string
local function build_group_header(item, ctx)
  local lines = file_renderer.render_line(item, ctx)
  return lines[1]
end

--- Apply highlights for a file group header line using file_renderer.
---@param item FileItem Grep match item
---@param ctx ListRenderContext Render context
---@param buf number Buffer handle
---@param ns_id number Namespace id
---@param row number 0-based row in buffer (header line)
local function apply_group_header_highlights(item, ctx, buf, ns_id, row)
  local line_content = vim.api.nvim_buf_get_lines(buf, row, row + 1, false)[1] or ''
  local saved_cursor = ctx.cursor
  ctx.cursor = -1
  file_renderer.apply_highlights(item, ctx, 0, buf, ns_id, row + 1, line_content)
  ctx.cursor = saved_cursor
end

--- Format a grep match location string.
---@param item table Grep match item
---@param ctx table Render context
---@return string
local function format_location(item, ctx)
  local fmt = (ctx.config and ctx.config.grep and ctx.config.grep.location_format) or ':%d:%d'
  local ok, str = pcall(string.format, fmt, item.line_number or 0, (item.col or 0) + 1)
  if not ok then str = string.format(':%d:%d', item.line_number or 0, (item.col or 0) + 1) end
  return str
end

local BINARY_PLACEHOLDER = '<binary content>'

--- Render a single grep match line.
---@param item table Grep match item
---@param ctx table Render context
---@return string
local function render_match_line(item, ctx)
  local location = format_location(item, ctx)
  local separator = '  '
  local raw_content = item.line_content
  if type(raw_content) ~= 'string' then raw_content = raw_content and tostring(raw_content) or '' end
  local content = raw_content
  if item.is_binary_content then content = BINARY_PLACEHOLDER end

  local indent = ' '
  local prefix_display_w = #indent + #location + #separator
  local available = ctx.win_width - prefix_display_w - 2
  local content_display_w = vim.fn.strdisplaywidth(content)

  if content_display_w > available and available > 3 then
    local nchars = vim.fn.strchars(content)
    local lo, hi = 0, nchars
    while lo < hi do
      local mid = math.floor((lo + hi + 1) / 2)
      if vim.fn.strdisplaywidth(vim.fn.strcharpart(content, 0, mid)) <= available - 1 then
        lo = mid
      else
        hi = mid - 1
      end
    end
    content = vim.fn.strcharpart(content, 0, lo) .. '…'
  end

  local line = indent .. location .. separator .. content
  local padding = math.max(0, ctx.win_width - vim.fn.strdisplaywidth(line) + 5)

  item._match_indent = #indent
  item._content_offset = prefix_display_w
  item._trimmed_content = content

  return line .. string.rep(' ', padding)
end

--- Apply highlights for a grouped match line.
---@param item table Grep match item
---@param item_idx number 1-based item index
---@param buf number Buffer handle
---@param ns_id number Namespace id
---@param row number 0-based row in buffer
---@param line_content string The rendered line text
local function apply_match_highlights(item, item_idx, buf, ns_id, row, line_content, ctx)
  local config = ctx.config
  local is_cursor = item_idx == ctx.cursor
  local indent = item._match_indent or 1

  if is_cursor then
    vim.api.nvim_buf_set_extmark(buf, ns_id, row, 0, {
      end_col = 0,
      end_row = row + 1,
      hl_group = config.hl.cursor,
      hl_eol = true,
      priority = 100,
    })
  end

  local location_str = format_location(item, ctx)
  local loc_start = indent
  local loc_end = loc_start + #location_str
  if loc_end <= #line_content then
    pcall(vim.api.nvim_buf_set_extmark, buf, ns_id, row, loc_start, {
      end_col = loc_end,
      hl_group = config.hl.grep_line_number or 'LineNr',
      priority = 150,
    })
  end

  local sep_start = loc_end
  local sep_end = sep_start + 2
  if sep_end <= #line_content then
    pcall(vim.api.nvim_buf_set_extmark, buf, ns_id, row, sep_start, {
      end_col = sep_end,
      hl_group = 'Comment',
      priority = 150,
    })
  end

  local content_start = sep_end

  if item.is_binary_content then
    local content_end = content_start + #BINARY_PLACEHOLDER
    if content_end <= #line_content then
      pcall(vim.api.nvim_buf_set_extmark, buf, ns_id, row, content_start, {
        end_col = content_end,
        hl_group = 'Comment',
        priority = 150,
      })
    end
  elseif item._trimmed_content and item.name then
    ctx._ts_lang_cache = ctx._ts_lang_cache or {}
    local lang = ctx._ts_lang_cache[item.name]
    if lang == nil then
      lang = tresitter_highlight.lang_from_filename(item.name) or false
      ctx._ts_lang_cache[item.name] = lang
    end

    if lang then
      local highlights = tresitter_highlight.get_line_highlights(item._trimmed_content, lang)
      for _, hl in ipairs(highlights) do
        local hl_start = content_start + hl.col
        local hl_end = content_start + hl.end_col
        if hl_start < #line_content and hl_end <= #line_content then
          pcall(vim.api.nvim_buf_set_extmark, buf, ns_id, row, hl_start, {
            end_col = hl_end,
            hl_group = hl.hl_group,
            priority = 120,
          })
        end
      end
    end
  end

  if item.match_ranges and not item.is_binary_content then
    for _, range in ipairs(item.match_ranges) do
      local raw_start = range[1] or 0
      local raw_end = range[2] or 0

      if raw_end > 0 then
        raw_start = math.max(0, raw_start)
        local hl_start = content_start + raw_start
        local hl_end = content_start + raw_end
        if hl_start < #line_content and hl_end <= #line_content then
          pcall(vim.api.nvim_buf_set_extmark, buf, ns_id, row, hl_start, {
            end_col = hl_end,
            hl_group = config.hl.grep_match or 'IncSearch',
            priority = 200,
          })
        end
      end
    end
  end

  if ctx.selected_items then
    local key = string.format('%s:%d:%d', item.relative_path, item.line_number or 0, item.col or 0)
    if ctx.selected_items[key] then
      vim.api.nvim_buf_set_extmark(buf, ns_id, row, 0, {
        sign_text = '▊',
        sign_hl_group = config.hl.selected or 'FFFSelected',
        priority = 1001,
      })
    end
  end
end

--- Render a single item's lines (called by list_renderer).
--- Returns 2 lines [header, match] for the first match of a file group,
--- or 1 line [match] for subsequent matches in the same file.
---@param item FileItem Grep match item
---@param ctx table Render context
---@param item_idx number 1-based item index in ctx.items
---@return string[]
function M.render_line(item, ctx, item_idx)
  -- First rendered item in this pass always gets header — fixes missing header
  -- when paginating backward in multi-page grep results (ctx is fresh per render).
  local is_first_visible = (item_idx == ctx.iter_start)
  local is_new_group = is_first_visible or (item.relative_path ~= ctx.grep_last_file)
  ctx.grep_last_file = item.relative_path

  local match_line = render_match_line(item, ctx)

  if is_new_group then
    ---@diagnostic disable-next-line: inject-field
    item._has_group_header = true
    local header_line = build_group_header(item, ctx)
    return { header_line, match_line }
  else
    ---@diagnostic disable-next-line: inject-field
    item._has_group_header = false
    return { match_line }
  end
end

--- Apply highlights for rendered lines (called by list_renderer).
---@param item FileItem Grep match item
---@param ctx ListRenderContext Render context
---@param item_idx number 1-based item index
---@param buf number Buffer handle
---@param ns_id number Namespace id
---@param line_idx number 1-based line index of the match line
---@param line_content string The rendered match line text
function M.apply_highlights(item, ctx, item_idx, buf, ns_id, line_idx, line_content)
  local row = line_idx - 1

  apply_match_highlights(item, item_idx, buf, ns_id, row, line_content, ctx)

  ---@diagnostic disable-next-line: undefined-field
  if item._has_group_header then apply_group_header_highlights(item, ctx, buf, ns_id, row - 1) end
end

return M
