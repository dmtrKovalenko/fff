--- List Renderer, renders any type of list results produced by fff
local M = {}

--- @class ListRenderContext
--- @field config FffConfig User configuration
--- @field items table[] Array of data items to render
--- @field cursor number Current cursor position (1-based index into items)
--- @field win_height number Window height in lines
--- @field win_width number Window width in columns
--- @field max_path_width number Actual text area width (excluding signcolumn)
--- @field debug_enabled boolean Whether debug mode shows scores
--- @field prompt_position string 'top' or 'bottom'
--- @field has_combo boolean Whether combo boost is active
--- @field display_start number Start index for displayed items (1)
--- @field display_end number End index for displayed items (#items)
--- @field iter_start number Iteration start
--- @field iter_end number Iteration end
--- @field iter_step number Iteration step (1 or -1)
--- @field renderer table|nil Custom renderer with render_line/apply_highlights
--- @field query string Current search query
--- @field selected_files table<string, boolean> Selected file paths set
--- @field mode string|nil Current mode (nil or 'grep')
--- @field format_file_display function Helper for formatting file display
--- @field suggestion_source string|nil Active cross-mode suggestion source ('grep' or 'files')
--- @field after_separator boolean|nil Set right after the separator gap; renderers that group items start a new group

--- @class ItemLineMapping
--- @field first number First buffer line (1-based) this item occupies
--- @field last number Last buffer line (1-based) — the selectable content line
--- @field virtual_count number Number of virtual (header) lines before the content line

--- @class ListRenderResult
--- @field lines string[] All buffer lines (including virtual rows and padding)
--- @field item_to_lines table<number, ItemLineMapping> Maps item index -> line range
--- @field padding_offset number Number of empty lines prepended for bottom prompt
--- @field total_content_lines number Lines before padding was applied
--- @field separator_line number|nil Buffer line (1-based) where separator sits, nil if none

--- @class ListSeparator
--- @field idx number 1-based item index — separator visually sits between this item and the previous one in the iteration order
--- @field text string Label text rendered inside the separator
--- @field text_hl string|nil Highlight group for label
--- @field border_hl string|nil Highlight group for the dashes

--- Generate all display lines from items from the renderer ctx
--- @param ctx table
--- @return string[] lines Array of line strings
--- @return table<number, ItemLineMapping> item_to_lines
--- @return number|nil separator_line 1-based buffer line of the separator, nil if none
local function generate_item_lines(ctx)
  local lines = {}
  local item_to_lines = {}
  local separator_line = nil

  -- Cross-mode suggestion header: rendered above items visually.
  -- For top prompt that means before items; for bottom prompt after items
  -- (because bottom prompt iterates in reverse).
  local suggestion_header_lines = {}
  local has_suggestion_header = ctx.suggestion_source ~= nil and #ctx.items > 0
  if has_suggestion_header then
    table.insert(suggestion_header_lines, '')
    if ctx.mode == 'grep' and ctx.suggestion_source == 'files' then
      -- Grep mode with no results — hint about mode cycling to fuzzy search
      local config = require('fff.conf').get()
      local keybind = config.keymaps.cycle_grep_modes
      if type(keybind) == 'table' then keybind = keybind[1] or '<S-Tab>' end
      table.insert(suggestion_header_lines, '  No results, try ' .. keybind .. ' to fuzzy search')
    else
      local mode_label = ctx.suggestion_source == 'grep' and 'content matches' or 'file name matches'
      table.insert(suggestion_header_lines, '  No results found. Suggested ' .. mode_label .. ':')
    end
    table.insert(suggestion_header_lines, '')
  end

  -- For top prompt: suggestion header goes before items
  if has_suggestion_header and ctx.prompt_position ~= 'bottom' then
    for _, hline in ipairs(suggestion_header_lines) do
      table.insert(lines, hline)
    end
  end

  local renderer = ctx.renderer
  if not renderer then renderer = require('fff.picker_ui.file_renderer') end

  -- Insert a gap on the side of the anchor away from the prompt.
  -- Bottom prompt: gap BEFORE anchor in iter — anchor renders last with
  -- reverse iter, so gap lands just above anchor visually.
  -- Top prompt: gap AFTER anchor in iter — anchor renders first with
  -- forward iter, so gap lands just below anchor visually.
  -- Either way: anchor stays adjacent to the prompt, separator on far side.
  local gap_before_anchor = ctx.prompt_position == 'bottom'

  for i = ctx.iter_start, ctx.iter_end, ctx.iter_step do
    if ctx.separator and ctx.separator.idx == i and gap_before_anchor then
      table.insert(lines, '')
      separator_line = #lines
      ctx.after_separator = true
    end

    local item = ctx.items[i]
    local item_start_line = #lines + 1

    local item_lines = renderer.render_line(item, ctx, i)
    vim.list_extend(lines, item_lines)

    local item_end_line = #lines
    local virtual_count = item_end_line - item_start_line

    item_to_lines[i] = {
      first = item_start_line,
      last = item_end_line,
      virtual_count = virtual_count,
    }

    if ctx.separator and ctx.separator.idx == i and not gap_before_anchor then
      table.insert(lines, '')
      separator_line = #lines
      ctx.after_separator = true
    end
  end

  -- For bottom prompt: suggestion header goes after items (appears above visually)
  if has_suggestion_header and ctx.prompt_position == 'bottom' then
    for _, hline in ipairs(suggestion_header_lines) do
      table.insert(lines, hline)
    end
  end

  return lines, item_to_lines, separator_line
end

--- Apply bottom padding: prepend empty lines so content sits at the bottom.
--- Adjusts all line indices in item_to_lines accordingly.
--- @param lines string[] Lines array (mutated)
--- @param item_to_lines table<number, ItemLineMapping> Mapping (mutated)
--- @param ctx table
--- @param separator_line number|nil Pre-padding separator line (mutated via return value)
--- @return number padding_offset Number of empty lines prepended
--- @return number|nil shifted_separator_line
local function apply_bottom_padding(lines, item_to_lines, ctx, separator_line)
  if ctx.prompt_position ~= 'bottom' then return 0, separator_line end

  local total_content_lines = #lines
  local empty_lines_needed = math.max(0, ctx.win_height - total_content_lines)

  if empty_lines_needed > 0 then
    for _ = empty_lines_needed, 1, -1 do
      table.insert(lines, 1, string.rep(' ', ctx.win_width + 5))
    end

    for i = ctx.display_start, ctx.display_end do
      if item_to_lines[i] then
        item_to_lines[i].first = item_to_lines[i].first + empty_lines_needed
        item_to_lines[i].last = item_to_lines[i].last + empty_lines_needed
      end
    end

    if separator_line then separator_line = separator_line + empty_lines_needed end
  end

  return empty_lines_needed, separator_line
end

--- Write lines to the buffer and position the cursor on the correct line.
--- The cursor always targets the content line (last) of the current item,
--- never a virtual header line.
--- @param lines string[]
--- @param item_to_lines table<number, ItemLineMapping>
--- @param ctx table
--- @param list_buf number Buffer handle
--- @param list_win number Window handle
--- @param ns_id number Namespace id
local function update_buffer_and_cursor(lines, item_to_lines, ctx, list_buf, list_win, ns_id)
  -- Resolve cursor to a buffer line — always the content line (last), not virtual rows
  local cursor_line = 0
  if #ctx.items > 0 and ctx.cursor >= 1 and ctx.cursor <= #ctx.items then
    local cursor_item = item_to_lines[ctx.cursor]
    if cursor_item then cursor_line = cursor_item.last end
  end

  vim.api.nvim_set_option_value('modifiable', true, { buf = list_buf })
  vim.api.nvim_buf_set_lines(list_buf, 0, -1, false, lines)
  vim.api.nvim_set_option_value('modifiable', false, { buf = list_buf })

  vim.api.nvim_buf_clear_namespace(list_buf, ns_id, 0, -1)

  if #ctx.items > 0 and cursor_line > 0 and cursor_line <= #lines then
    vim.api.nvim_win_set_cursor(list_win, { cursor_line, 0 })
  end
end

--- Apply highlights for all items using the renderer's apply_highlights.
--- For each item, we pass the content line (last) to the renderer.
--- Renderers that emit virtual rows (grep_renderer) handle their own
--- header highlights internally via the item._has_group_header flag.
--- @param lines string[]
--- @param item_to_lines table<number, ItemLineMapping>
--- @param ctx table
--- @param list_buf number
--- @param ns_id number
local function apply_all_highlights(lines, item_to_lines, ctx, list_buf, ns_id)
  local renderer = ctx.renderer
  if not renderer then renderer = require('fff.picker_ui.file_renderer') end

  for i = ctx.display_start, ctx.display_end do
    local item = ctx.items[i]
    local item_lines = item_to_lines[i]

    if item_lines then
      -- The content line is always the last line in the mapping
      local line_idx = item_lines.last
      local line_content = lines[line_idx]

      if line_content then
        ---@diagnostic disable-next-line: param-type-mismatch
        renderer.apply_highlights(item, ctx, i, list_buf, ns_id, line_idx, line_content)
      end
    end
  end
end

--- Render the full item list into the buffer.
--- This is the main entry point — replaces the inline rendering in picker_ui.
---
--- @param ctx table Render context built by picker_ui
--- @param list_buf number List buffer handle
--- @param list_win number List window handle
--- @param ns_id number Highlight namespace
--- @return table<number, ItemLineMapping> item_to_lines
--- @return number|nil separator_line 1-based buffer line of the separator (post-padding), nil if none
function M.render(ctx, list_buf, list_win, ns_id)
  local lines, item_to_lines, separator_line = generate_item_lines(ctx)

  local _, shifted_separator_line = apply_bottom_padding(lines, item_to_lines, ctx, separator_line)
  separator_line = shifted_separator_line
  update_buffer_and_cursor(lines, item_to_lines, ctx, list_buf, list_win, ns_id)

  if #ctx.items > 0 then apply_all_highlights(lines, item_to_lines, ctx, list_buf, ns_id) end

  -- Highlight the suggestion header lines (if present)
  if ctx.suggestion_source and #ctx.items > 0 then
    local suggestion_hl = ctx.config.hl.suggestion_header or 'WarningMsg'
    for i = 0, #lines - 1 do
      local line = lines[i + 1]
      if line and (line:match('^%s+No results found') or line:match('^%s+No results,')) then
        pcall(
          vim.api.nvim_buf_set_extmark,
          list_buf,
          ns_id,
          i,
          0,
          { end_row = i + 1, end_col = 0, hl_group = suggestion_hl }
        )
      end
    end
  end

  return item_to_lines, separator_line
end

return M
