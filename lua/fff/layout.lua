local M = {}

local utils = require('fff.utils')
local file_info_renderer = require('fff.file_picker.file_info')

local BORDER_PRESETS = {
  single = { '┌', '─', '┐', '│', '┘', '─', '└', '│' },
  double = { '╔', '═', '╗', '║', '╝', '═', '╚', '║' },
  rounded = { '╭', '─', '╮', '│', '╯', '─', '╰', '│' },
  solid = { '▛', '▀', '▜', '▐', '▟', '▄', '▙', '▌' },
  shadow = { '', '', ' ', ' ', ' ', ' ', ' ', '' },
  none = { '', '', '', '', '', '', '', '' },
}

local T_JUNCTION_PRESETS = {
  single = { '├', '┤', '┬', '┴', '┼' },
  double = { '╠', '╣', '╦', '╩', '╬' },
  rounded = { '├', '┤', '┬', '┴', '┼' },
  solid = { '▌', '▐', '▀', '▄', '█' },
  shadow = { '', '', '', '', '' },
  none = { '', '', '', '', '' },
}

-- Resolve which border preset to draw. `config.layout.border` overrides the
-- global `winborder`; when unset we fall back to `vim.o.winborder` so fff keeps
-- respecting the user's global preference.
local function get_border_chars(config)
  local border = config and config.layout and config.layout.border
  if border == nil or border == '' then border = vim.o.winborder end
  if border == nil or border == '' then border = 'single' end

  if type(border) == 'table' then
    if #border == 2 and type(border[1]) == 'table' and type(border[2]) == 'table' then return border[1], border[2] end
  end
  if BORDER_PRESETS[border] then return BORDER_PRESETS[border], T_JUNCTION_PRESETS[border] end
  return BORDER_PRESETS.single, T_JUNCTION_PRESETS.single
end

--- Resolve a corner glyph based on adjacency flags. Each neighbour flag means
--- another float shares the edge meeting at that corner, so the corner needs
--- to extend a stem in that direction instead of being a plain corner.
local function resolve_corner(corner, chars, j, which, n)
  local stems = {
    tl = { down = chars[8] ~= '', right = chars[2] ~= '' },
    tr = { down = chars[4] ~= '', left = chars[2] ~= '' },
    bl = { up = chars[8] ~= '', right = chars[6] ~= '' },
    br = { up = chars[4] ~= '', left = chars[6] ~= '' },
  }
  local s = stems[which]
  local up = s.up or n.up
  local down = s.down or n.down
  local left = s.left or n.left
  local right = s.right or n.right
  local count = (up and 1 or 0) + (down and 1 or 0) + (left and 1 or 0) + (right and 1 or 0)
  if count >= 4 then return j[5] end
  if up and down and (left or right) then return left and j[2] or j[1] end
  if left and right and (up or down) then return up and j[4] or j[3] end
  return corner
end

local function resolve_prompt_position(config)
  if config and config.layout and config.layout.prompt_position then
    return utils.resolve_config_value(
      config.layout.prompt_position,
      vim.o.columns,
      vim.o.lines,
      function(value) return utils.is_one_of(value, { 'top', 'bottom' }) end,
      'bottom',
      'layout.prompt_position'
    )
  end
  return 'bottom'
end

M.resolve_prompt_position = resolve_prompt_position

function M.resolve_title_pos(value)
  if utils.is_one_of(value, { 'left', 'center', 'right' }) then return value end
  return 'left'
end

local function resolve_preview_position(config)
  if config and config.layout and config.layout.preview_position then
    local terminal_width = vim.o.columns
    local position = utils.resolve_config_value(
      config.layout.preview_position,
      terminal_width,
      vim.o.lines,
      function(value) return utils.is_one_of(value, { 'left', 'right', 'top', 'bottom' }) end,
      'right',
      'layout.preview_position'
    )

    -- Flex wrap: when the terminal is narrower than `flex.size`, swap a
    -- side-by-side preview to a stacked top/bottom one so columns stay legible.
    local flex = config.layout.flex
    if flex then
      local size = flex.size or 80
      local wrap = flex.wrap or 'top'
      if terminal_width < size then return wrap end
    end

    return position
  end
  return 'right'
end

--- @param cfg table Layout config produced by compute
--- @return table layout dimensions and positions
function M.calculate_dimensions(cfg)
  local BORDER_SIZE = 2
  local PROMPT_HEIGHT = 2
  local SEPARATOR_WIDTH = 1
  local SEPARATOR_HEIGHT = 1

  if not utils.is_one_of(cfg.preview_position, { 'left', 'right', 'top', 'bottom' }) then
    error('Invalid preview position: ' .. tostring(cfg.preview_position))
  end

  local layout = {}
  local preview_enabled = cfg.preview_enabled
  if preview_enabled == nil then preview_enabled = true end

  local total_width = math.max(0, cfg.total_width - BORDER_SIZE)
  local total_height = math.max(0, cfg.total_height - BORDER_SIZE - PROMPT_HEIGHT)

  if cfg.preview_position == 'left' then
    local separator_width = preview_enabled and SEPARATOR_WIDTH or 0
    local list_width = math.max(0, total_width - cfg.preview_width - separator_width)
    local list_height = total_height

    layout.list_col = cfg.start_col + cfg.preview_width + 2 -- +2 for borders (shared separator column)
    layout.list_width = list_width
    layout.list_height = list_height
    layout.input_col = layout.list_col
    layout.input_width = list_width

    if preview_enabled then
      layout.preview = {
        col = cfg.start_col + 1,
        row = cfg.start_row + 1,
        width = cfg.preview_width,
        height = list_height,
      }
    end
  elseif cfg.preview_position == 'right' then
    local separator_width = preview_enabled and SEPARATOR_WIDTH or 0
    local list_width = math.max(0, total_width - cfg.preview_width - separator_width)
    local list_height = total_height

    layout.list_col = cfg.start_col + 1
    layout.list_width = list_width
    layout.list_height = list_height
    layout.input_col = layout.list_col
    layout.input_width = list_width

    if preview_enabled then
      layout.preview = {
        col = cfg.start_col + list_width + 2, -- +2 for borders (shared separator column)
        row = cfg.start_row + 1,
        width = cfg.preview_width,
        height = list_height,
      }
    end
  elseif cfg.preview_position == 'top' then
    local separator_height = preview_enabled and SEPARATOR_HEIGHT or 0
    local list_height = math.max(0, total_height - cfg.preview_height - separator_height)

    layout.list_col = cfg.start_col + 1
    layout.list_width = total_width
    layout.list_height = list_height
    layout.input_col = layout.list_col
    layout.input_width = total_width
    layout.list_start_row = cfg.start_row + (preview_enabled and (cfg.preview_height + separator_height) or 0) + 1

    if preview_enabled then
      layout.preview = {
        col = cfg.start_col + 1,
        row = cfg.start_row + 1,
        width = total_width,
        height = cfg.preview_height,
      }
    end
  else
    local separator_height = preview_enabled and SEPARATOR_HEIGHT or 0
    local list_height = math.max(0, total_height - cfg.preview_height - separator_height)

    layout.list_col = cfg.start_col + 1
    layout.list_width = total_width
    layout.list_height = list_height
    layout.input_col = layout.list_col
    layout.input_width = total_width
    layout.list_start_row = cfg.start_row + 1

    if preview_enabled then
      layout.preview = {
        col = cfg.start_col + 1,
        width = total_width,
        height = cfg.preview_height,
      }
    end
  end

  if cfg.preview_position == 'left' or cfg.preview_position == 'right' then
    if cfg.prompt_position == 'top' then
      layout.input_row = cfg.start_row + 1
      layout.list_row = cfg.start_row + PROMPT_HEIGHT + 1
    else
      layout.list_row = cfg.start_row + 1
      layout.input_row = cfg.start_row + cfg.total_height - BORDER_SIZE
    end

    if layout.preview then
      layout.preview.row = cfg.start_row + 1
      layout.preview.height = cfg.total_height - BORDER_SIZE
    end
  else
    local list_start_row = layout.list_start_row
    if cfg.prompt_position == 'top' then
      layout.input_row = list_start_row
      layout.list_row = list_start_row + BORDER_SIZE
      layout.list_height = math.max(0, layout.list_height - BORDER_SIZE)
    else
      layout.list_row = list_start_row
      layout.input_row = list_start_row + layout.list_height + 1
    end

    if cfg.preview_position == 'bottom' and layout.preview then
      if cfg.prompt_position == 'top' then
        layout.preview.row = layout.list_row + layout.list_height + 1
      else
        layout.preview.row = layout.input_row + PROMPT_HEIGHT
      end
    end
  end

  -- Debug file_info panel. Only fits in side-by-side preview layouts; in
  -- compact stacked layouts squeezing it in makes both panels unreadable.
  local is_side_by_side = cfg.preview_position == 'left' or cfg.preview_position == 'right'
  if cfg.debug_enabled and preview_enabled and layout.preview and is_side_by_side then
    layout.file_info = {
      width = layout.preview.width,
      height = cfg.file_info_height,
      col = layout.preview.col,
      row = layout.preview.row,
    }
    -- Stack preview directly below file_info so they share a border row;
    -- the shared row resolves to ├──...──┤ T-junctions instead of two corners.
    local consumed = cfg.file_info_height + 1
    layout.preview.row = layout.preview.row + consumed
    layout.preview.height = math.max(3, layout.preview.height - consumed)
  end

  return layout
end

--- Build the per-window configs (for `nvim_open_win`) from a finished layout.
--- Handles all the corner/T-junction resolution so adjacent floats line up.
local function build_window_configs(layout, config, prompt_position, preview_position)
  local border_chars, t_junctions = get_border_chars(config)
  local has_preview = layout.preview ~= nil
  local title = ' ' .. (config.title or 'FFFiles') .. ' '
  local title_pos = M.resolve_title_pos(config.layout.title_position)
  local filename_pos = M.resolve_title_pos(config.layout.filename_position)

  local list_neighbour_input_top = prompt_position == 'top'
  local list_neighbour_input_bottom = prompt_position == 'bottom'
  local list_neighbour_preview_top = has_preview and preview_position == 'top'
  local list_neighbour_preview_bottom = has_preview and preview_position == 'bottom'
  local list_neighbour_preview_left = has_preview and preview_position == 'left'
  local list_neighbour_preview_right = has_preview and preview_position == 'right'

  local list_top_at_picker_top = (not list_neighbour_preview_top) and not list_neighbour_input_top
  local list_bottom_at_picker_bottom = (not list_neighbour_preview_bottom) and not list_neighbour_input_bottom

  local corners = {
    tl = resolve_corner(border_chars[1], border_chars, t_junctions, 'tl', {
      up = list_neighbour_preview_top or list_neighbour_input_top,
      down = list_neighbour_preview_left,
      left = list_neighbour_preview_left and list_top_at_picker_top,
    }),
    tr = resolve_corner(border_chars[3], border_chars, t_junctions, 'tr', {
      up = list_neighbour_preview_top or list_neighbour_input_top,
      down = list_neighbour_preview_right,
      right = list_neighbour_preview_right and list_top_at_picker_top,
    }),
    br = resolve_corner(border_chars[5], border_chars, t_junctions, 'br', {
      down = list_neighbour_preview_bottom or list_neighbour_input_bottom,
      up = list_neighbour_preview_right,
      right = list_neighbour_preview_right and list_bottom_at_picker_bottom,
    }),
    bl = resolve_corner(border_chars[7], border_chars, t_junctions, 'bl', {
      down = list_neighbour_preview_bottom or list_neighbour_input_bottom,
      up = list_neighbour_preview_left,
      left = list_neighbour_preview_left and list_bottom_at_picker_bottom,
    }),
  }

  local list_border = prompt_position == 'bottom'
      and { corners.tl, border_chars[2], corners.tr, border_chars[4], '', '', '', border_chars[8] }
    or {
      corners.tl,
      border_chars[2],
      corners.tr,
      border_chars[4],
      corners.br,
      border_chars[6],
      corners.bl,
      border_chars[8],
    }

  local list_cfg = {
    relative = 'editor',
    width = math.max(1, layout.list_width),
    height = math.max(1, layout.list_height),
    col = layout.list_col,
    row = layout.list_row,
    border = list_border,
    style = 'minimal',
    zindex = 52,
  }
  if prompt_position == 'bottom' then
    list_cfg.title = title
    list_cfg.title_pos = title_pos
  end

  local input_neighbour_preview_left = has_preview and preview_position == 'left'
  local input_neighbour_preview_right = has_preview and preview_position == 'right'
  local input_neighbour_preview_top = has_preview and preview_position == 'top' and prompt_position == 'top'
  local input_neighbour_preview_bottom = has_preview and preview_position == 'bottom' and prompt_position == 'bottom'

  local input_top_at_picker_top = prompt_position == 'top'
  local input_bottom_at_picker_bottom = prompt_position == 'bottom'

  local function tl_extends_up()
    if prompt_position == 'bottom' then return true end -- list above
    if input_neighbour_preview_top then return true end -- preview stack above
    return false
  end
  local function tr_extends_up() return tl_extends_up() end
  local function bl_extends_down()
    if prompt_position == 'top' then return true end -- list below
    if input_neighbour_preview_bottom then return true end -- preview stack below
    return false
  end
  local function br_extends_down() return bl_extends_down() end

  local ic = {
    tl = resolve_corner(border_chars[1], border_chars, t_junctions, 'tl', {
      up = tl_extends_up(),
      down = input_neighbour_preview_left and input_top_at_picker_top,
      left = input_neighbour_preview_left and input_top_at_picker_top,
    }),
    tr = resolve_corner(border_chars[3], border_chars, t_junctions, 'tr', {
      up = tr_extends_up(),
      down = input_neighbour_preview_right and input_top_at_picker_top,
      right = input_neighbour_preview_right and input_top_at_picker_top,
    }),
    br = resolve_corner(border_chars[5], border_chars, t_junctions, 'br', {
      down = br_extends_down(),
      up = input_neighbour_preview_right and input_bottom_at_picker_bottom,
      right = input_neighbour_preview_right and input_bottom_at_picker_bottom,
    }),
    bl = resolve_corner(border_chars[7], border_chars, t_junctions, 'bl', {
      down = bl_extends_down(),
      up = input_neighbour_preview_left and input_bottom_at_picker_bottom,
      left = input_neighbour_preview_left and input_bottom_at_picker_bottom,
    }),
  }
  -- Input always renders a full border. In top-prompt mode the bottom border
  -- coincides with the list's top border row; the corner glyphs (`├` / `┤`)
  -- are computed identically by both sides, but input is opened LAST so its
  -- corners win the zindex tie at the column shared with file_info's left
  -- vertical — without this, file_info's plain `│` would overdraw the
  -- T-junction and leave a disconnected corner.
  local input_border =
    { ic.tl, border_chars[2], ic.tr, border_chars[4], ic.br, border_chars[6], ic.bl, border_chars[8] }

  local input_cfg = {
    relative = 'editor',
    width = math.max(1, layout.input_width),
    height = 1,
    col = layout.input_col,
    row = layout.input_row,
    border = input_border,
    style = 'minimal',
    zindex = 53,
  }
  if prompt_position == 'top' then
    input_cfg.title = title
    input_cfg.title_pos = title_pos
  end

  local preview_cfg = nil
  if layout.preview then
    local has_file_info_above = layout.file_info ~= nil
    local pc = {
      tl = resolve_corner(border_chars[1], border_chars, t_junctions, 'tl', {
        right = preview_position == 'left',
        up = has_file_info_above,
      }),
      tr = resolve_corner(border_chars[3], border_chars, t_junctions, 'tr', {
        left = preview_position == 'right',
        up = has_file_info_above,
      }),
      br = resolve_corner(border_chars[5], border_chars, t_junctions, 'br', {
        left = preview_position == 'right',
      }),
      bl = resolve_corner(border_chars[7], border_chars, t_junctions, 'bl', {
        right = preview_position == 'left',
      }),
    }
    -- Top/bottom stacked previews share a row with the list; the matching
    -- horizontal edge gets T-junctions on both ends.
    if preview_position == 'top' then
      pc.bl = resolve_corner(border_chars[7], border_chars, t_junctions, 'bl', { down = true })
      pc.br = resolve_corner(border_chars[5], border_chars, t_junctions, 'br', { down = true })
    elseif preview_position == 'bottom' then
      pc.tl = resolve_corner(border_chars[1], border_chars, t_junctions, 'tl', { up = true })
      pc.tr = resolve_corner(border_chars[3], border_chars, t_junctions, 'tr', { up = true })
    end
    local preview_border =
      { pc.tl, border_chars[2], pc.tr, border_chars[4], pc.br, border_chars[6], pc.bl, border_chars[8] }
    preview_cfg = {
      relative = 'editor',
      width = math.max(1, layout.preview.width),
      height = math.max(1, layout.preview.height),
      col = layout.preview.col,
      row = layout.preview.row,
      style = 'minimal',
      border = preview_border,
      -- Title hidden when file_info renders above — its footer already says "Preview".
      title = layout.file_info and '' or ' Preview ',
      title_pos = filename_pos,
      zindex = 51,
    }
  end

  local file_info_cfg = nil
  if layout.file_info then
    local list_meets_fi_left = preview_position == 'right'
    local list_meets_fi_right = preview_position == 'left'
    local fc = {
      tl = resolve_corner(border_chars[1], border_chars, t_junctions, 'tl', { left = list_meets_fi_left }),
      tr = resolve_corner(border_chars[3], border_chars, t_junctions, 'tr', { right = list_meets_fi_right }),
      bl = resolve_corner(border_chars[7], border_chars, t_junctions, 'bl', { down = true }),
      br = resolve_corner(border_chars[5], border_chars, t_junctions, 'br', { down = true }),
    }
    local fi_border = { fc.tl, border_chars[2], fc.tr, border_chars[4], fc.br, border_chars[6], fc.bl, border_chars[8] }
    file_info_cfg = {
      relative = 'editor',
      width = math.max(1, layout.file_info.width),
      height = math.max(1, layout.file_info.height),
      col = layout.file_info.col,
      row = layout.file_info.row,
      style = 'minimal',
      border = fi_border,
      title = ' File Info ',
      title_pos = filename_pos,
      -- Above the list/preview zindex so its borders win the shared rows.
      zindex = 53,
    }
  end

  return {
    list = list_cfg,
    input = input_cfg,
    preview = preview_cfg,
    file_info = file_info_cfg,
  }
end

--- Compute the full layout + window configs for the picker. Auto-suppresses
--- the preview when the resulting list area would be smaller than
--- `config.layout.min_list_height`.
---
--- @param config table Resolved picker config (M.state.config)
--- @param preview_user_enabled boolean Whether the user's config has preview enabled
--- @return table { layout, win_configs, debug_enabled, preview_visible }
function M.compute(config, preview_user_enabled)
  local debug_user_enabled = preview_user_enabled and config.debug and config.debug.enabled or false

  local terminal_width = vim.o.columns
  local terminal_height = vim.o.lines

  local width_ratio = utils.resolve_config_value(
    config.layout.width,
    terminal_width,
    terminal_height,
    utils.is_valid_ratio,
    0.8,
    'layout.width'
  )
  local height_ratio = utils.resolve_config_value(
    config.layout.height,
    terminal_width,
    terminal_height,
    utils.is_valid_ratio,
    0.8,
    'layout.height'
  )

  local width = math.floor(terminal_width * width_ratio)
  local height = math.floor(terminal_height * height_ratio)

  -- Account for chrome (statusline, tabline, cmdheight) for edge-anchored positions
  local has_tabline = vim.o.showtabline == 2 or (vim.o.showtabline == 1 and #vim.api.nvim_list_tabpages() > 1)
  local has_statusline = vim.o.laststatus > 0
  local top_edge = has_tabline and 1 or 0
  local bottom_edge = terminal_height - vim.o.cmdheight - (has_statusline and 1 or 0)
  local usable_height = bottom_edge - top_edge
  height = math.min(height, usable_height)

  local anchor = utils.resolve_config_value(
    config.layout.anchor,
    terminal_width,
    terminal_height,
    function(v)
      return utils.is_one_of(v, {
        'center',
        'top_left',
        'top',
        'top_right',
        'left',
        'right',
        'bottom_left',
        'bottom',
        'bottom_right',
      })
    end,
    'center',
    'layout.anchor'
  )

  -- Edge-flush anchors compensate for the +1 offset added by calculate_dimensions.
  local center_col = math.floor((terminal_width - width) / 2)
  local center_row = top_edge + math.floor((usable_height - height) / 2)
  if width >= terminal_width then center_col = -1 end
  if height >= usable_height then center_row = top_edge - 1 end
  local anchor_positions = {
    center = { col = center_col, row = center_row },
    top_left = { col = -1, row = top_edge - 1 },
    top = { col = center_col, row = top_edge - 1 },
    top_right = { col = terminal_width - width - 2, row = top_edge - 1 },
    left = { col = -1, row = center_row },
    right = { col = terminal_width - width - 2, row = center_row },
    bottom_left = { col = -1, row = bottom_edge - height - 1 },
    bottom = { col = center_col, row = bottom_edge - height - 1 },
    bottom_right = { col = terminal_width - width - 2, row = bottom_edge - height - 1 },
  }

  local pos = anchor_positions[anchor] or anchor_positions.center
  local col = pos.col
  local row = pos.row

  -- Manual ratio overrides (backwards compat)
  if config.layout.col ~= nil then
    local col_ratio = utils.resolve_config_value(
      config.layout.col,
      terminal_width,
      terminal_height,
      utils.is_valid_ratio,
      col / terminal_width,
      'layout.col'
    )
    col = math.floor(terminal_width * col_ratio)
  end
  if config.layout.row ~= nil then
    local row_ratio = utils.resolve_config_value(
      config.layout.row,
      terminal_width,
      terminal_height,
      utils.is_valid_ratio,
      row / terminal_height,
      'layout.row'
    )
    row = math.floor(terminal_height * row_ratio)
  end

  local prompt_position = resolve_prompt_position(config)
  local preview_position = resolve_preview_position(config)

  local preview_size_ratio = utils.resolve_config_value(
    config.layout.preview_size,
    terminal_width,
    terminal_height,
    utils.is_valid_ratio,
    0.4,
    'layout.preview_size'
  )

  local is_fullscreen = width >= terminal_width and height >= usable_height

  -- Panel sits above preview, so its width matches preview width. Ask the
  -- renderer how many rows it'll draw so we don't reserve a gap row.
  local preview_width_predicted = preview_user_enabled and math.floor(width * preview_size_ratio) or 0
  local file_info_height = 0
  if debug_user_enabled then
    file_info_height = file_info_renderer.calculate_required_height(
      config.debug and config.debug.show_file_info,
      preview_width_predicted
    )
  end

  local dim_cfg = {
    total_width = width,
    -- Top/bottom preview with prompt-top has a 2-row chrome over-subtraction in
    -- calculate_dimensions (BORDER_SIZE is subtracted twice). Compensate at fullscreen.
    total_height = (
      is_fullscreen
      and prompt_position == 'top'
      and (preview_position == 'top' or preview_position == 'bottom')
    )
        and height + 2
      or height,
    start_col = col,
    start_row = row,
    preview_position = preview_position,
    prompt_position = prompt_position,
    debug_enabled = debug_user_enabled and file_info_height > 0,
    preview_enabled = preview_user_enabled,
    preview_width = preview_width_predicted,
    preview_height = preview_user_enabled and math.floor(height * preview_size_ratio) or 0,
    separator_width = 3,
    file_info_height = file_info_height,
  }

  local layout = M.calculate_dimensions(dim_cfg)

  -- Auto-hide preview when the list area would be too cramped to be useful.
  -- Recompute giving the list all the available space.
  local min_list_height = utils.resolve_config_value(
    config.layout.min_list_height,
    terminal_width,
    terminal_height,
    function(v) return type(v) == 'number' and v >= 0 end,
    10,
    'layout.min_list_height'
  )

  local debug_enabled = debug_user_enabled
  if preview_user_enabled and layout.preview and min_list_height > 0 and layout.list_height < min_list_height then
    dim_cfg.preview_enabled = false
    dim_cfg.preview_width = 0
    dim_cfg.preview_height = 0
    dim_cfg.debug_enabled = false
    layout = M.calculate_dimensions(dim_cfg)
    debug_enabled = false
  end

  local win_configs = build_window_configs(layout, config, prompt_position, preview_position)

  return {
    layout = layout,
    win_configs = win_configs,
    debug_enabled = debug_enabled,
    preview_visible = layout.preview ~= nil,
  }
end

return M
