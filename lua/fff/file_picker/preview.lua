local utils = require('fff.utils')
local file_picker = require('fff.file_picker')
local image = require('fff.file_picker.image')
local location_utils = require('fff.location_utils')
local rust = require('fff.rust')
local file_info_renderer = require('fff.file_picker.file_info')

local M = {}

-- Namespace dedicated to the file info panel highlights.
M.file_info_ns = vim.api.nvim_create_namespace('fff_file_info')

local function set_buffer_lines(bufnr, lines)
  if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then return end

  vim.api.nvim_set_option_value('modifiable', true, { buf = bufnr })
  vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)
  vim.api.nvim_set_option_value('modifiable', false, { buf = bufnr })
end

local function append_buffer_lines(bufnr, lines)
  if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then return end
  if not lines or #lines == 0 then return end

  vim.api.nvim_set_option_value('modifiable', true, { buf = bufnr })
  local current_lines = vim.api.nvim_buf_line_count(bufnr)
  vim.api.nvim_buf_set_lines(bufnr, current_lines, current_lines, false, lines)
  vim.api.nvim_set_option_value('modifiable', false, { buf = bufnr })
end

local function show_preview_unavailable(bufnr) set_buffer_lines(bufnr, { 'No preview available' }) end

local function set_preview_filetype(bufnr, filetype)
  -- FileType hooks can throw or mutate global v:errmsg during preview setup.
  -- pcall lets us restore it; callers treat failures as preview-local.
  local errmsg = vim.v.errmsg
  local ok, err = pcall(vim.api.nvim_set_option_value, 'filetype', filetype, { buf = bufnr })
  vim.v.errmsg = errmsg
  return ok, err
end

local function find_existing_buffer(file_path)
  local abs_path = vim.fn.resolve(vim.fn.fnamemodify(file_path, ':p'))

  for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(bufnr) then
      local buf_name = vim.api.nvim_buf_get_name(bufnr)
      if buf_name ~= '' then
        local buf_path = vim.fn.resolve(vim.fn.fnamemodify(buf_name, ':p'))
        if buf_path == abs_path then return bufnr end
      end
    end
  end
  return nil
end

local function cleanup_file_operation()
  if M.state.file_operation then
    if M.state.file_operation.fd then pcall(vim.uv.fs_close, M.state.file_operation.fd) end
    M.state.file_operation = nil
  end
end

--- Process raw chunk data into complete lines, joining any leftover bytes
--- from the previous chunk and storing any trailing partial line for the next.
--- @param data string Raw chunk data
--- @return string[] Complete lines (may be empty if the entire chunk is a partial line)
local function split_chunk_with_remainder(data)
  if not data or data == '' then return {} end

  local fo = M.state.file_operation
  local prefix = fo and fo.remainder or ''
  local combined = prefix .. data

  local lines = vim.split(combined, '\n', { plain = true })

  if combined:sub(-1) ~= '\n' then
    -- Data doesn't end on a line boundary: last element is a partial line
    local partial = table.remove(lines) or ''
    if fo then fo.remainder = partial end
  else
    -- Data ends on a line boundary: remove the trailing empty element
    if #lines > 0 and lines[#lines] == '' then table.remove(lines) end
    if fo then fo.remainder = '' end
  end

  return lines
end

local function init_dynamic_loading_async(file_path, callback)
  cleanup_file_operation()

  M.state.loaded_lines = 0
  M.state.total_file_lines = nil
  M.state.has_more_content = true
  M.state.is_loading = false

  local generation = M.state.preview_generation

  vim.uv.fs_open(file_path, 'r', 438, function(err, fd)
    vim.schedule(function()
      -- Stale callback: preview moved on to a different file
      if M.state.preview_generation ~= generation then
        if fd then pcall(vim.uv.fs_close, fd) end
        return
      end

      if err or not fd then
        callback(false, 'Failed to open file: ' .. (err or 'unknown error'))
        return
      end

      M.state.file_operation = {
        fd = fd,
        file_path = file_path,
        position = 0,
        remainder = '',
      }

      callback(true)
    end)
  end)
end

local function load_forward_chunk_async(target_size, callback)
  if not M.state.file_operation or not M.state.file_operation.fd then
    callback('', 'No file handle available')
    return
  end

  M.state.is_loading = true
  local chunk_size = target_size or (M.config.chunk_size or 16384)
  local generation = M.state.preview_generation

  vim.uv.fs_read(M.state.file_operation.fd, chunk_size, M.state.file_operation.position, function(err, data)
    vim.schedule(function()
      -- Stale callback: a newer preview has started, discard this result
      if M.state.preview_generation ~= generation then return end

      M.state.is_loading = false

      if err then
        callback('', 'Read error: ' .. err)
        return
      end

      if not data or #data == 0 then
        M.state.has_more_content = false
        -- Flush any remaining partial line as the final piece of data
        local final_remainder = M.state.file_operation and M.state.file_operation.remainder or ''
        cleanup_file_operation()
        if final_remainder ~= '' then
          callback(final_remainder .. '\n', nil)
        else
          callback('', nil)
        end
        return
      end

      if M.state.file_operation then M.state.file_operation.position = M.state.file_operation.position + #data end

      callback(data, nil)
    end)
  end)
end

local function load_next_chunk_async(chunk_size, callback)
  if not M.state.file_operation or not M.state.has_more_content or M.state.is_loading then
    callback('', nil)
    return
  end
  load_forward_chunk_async(chunk_size, callback)
end

-- Forward declaration for ensure_content_loaded_async (used in read_file_streaming_async callback)
local ensure_content_loaded_async

local function read_file_streaming_async(file_path, callback)
  local generation = M.state.preview_generation

  init_dynamic_loading_async(file_path, function(success, error_msg)
    if M.state.preview_generation ~= generation then return end

    if not success then
      callback(nil, error_msg)
      return
    end

    -- Calculate initial chunk size based on location information
    local initial_chunk_size = M.config.chunk_size
    if M.state.location then
      local target_line = location_utils.get_target_line(M.state.location)
      if target_line then
        -- Estimate bytes needed: assume ~100 bytes per line average
        -- Add some buffer (50%) to account for variation in line lengths
        local estimated_bytes = target_line * 100 * 1.5
        -- Cap at reasonable maximum to avoid memory issues
        local max_initial_chunk = M.config.max_size or (10 * 1024 * 1024) -- 10MB default
        initial_chunk_size = math.min(estimated_bytes, max_initial_chunk)
        -- Ensure we don't go below the standard chunk size
        initial_chunk_size = math.max(initial_chunk_size, M.config.chunk_size)
      end
    end

    load_next_chunk_async(initial_chunk_size, function(data, err)
      if M.state.preview_generation ~= generation then return end

      if data and data ~= '' then
        local lines = split_chunk_with_remainder(data)
        M.state.loaded_lines = #lines
        M.state.content_height = #lines

        -- If we have a location and didn't load enough lines, try to load more
        local loading_more = false
        if M.state.location then
          local target_line = location_utils.get_target_line(M.state.location)
          if target_line and #lines < target_line and M.state.has_more_content then
            loading_more = true
            vim.schedule(function()
              if M.state.preview_generation == generation then ensure_content_loaded_async(target_line) end
            end)
          end
        end

        callback(lines, err, loading_more)
      else
        callback(nil, err)
      end
    end)
  end)
end

ensure_content_loaded_async = function(target_line)
  if not M.state.bufnr or not vim.api.nvim_buf_is_valid(M.state.bufnr) then return end
  if not M.state.has_more_content or M.state.is_loading then return end
  -- Guard against missing file handle: without it load_next_chunk_async returns
  -- synchronously with empty data, which triggers apply_location_highlighting
  -- -> ensure_content_loaded_async again, causing infinite recursion (stack overflow).
  if not M.state.file_operation then
    M.state.has_more_content = false
    return
  end

  local current_buffer_lines = vim.api.nvim_buf_line_count(M.state.bufnr)
  local buffer_needed = target_line + 50

  if current_buffer_lines >= buffer_needed then return end

  local generation = M.state.preview_generation

  -- Use a larger chunk to reach the target faster instead of many small 8KB reads
  local lines_needed = buffer_needed - current_buffer_lines
  local estimated_bytes = math.max(M.config.chunk_size, lines_needed * 120)

  load_next_chunk_async(estimated_bytes, function(data, err)
    -- Stale callback: preview moved on to a different file
    if M.state.preview_generation ~= generation then return end
    if not M.state.bufnr or not vim.api.nvim_buf_is_valid(M.state.bufnr) then return end

    if err then return end

    if data and data ~= '' then
      local chunk_lines = split_chunk_with_remainder(data)
      if #chunk_lines > 0 then append_buffer_lines(M.state.bufnr, chunk_lines) end

      M.state.content_height = vim.api.nvim_buf_line_count(M.state.bufnr)
      M.state.loaded_lines = M.state.content_height

      -- If we still haven't loaded enough, schedule another chunk
      if M.state.loaded_lines < buffer_needed and M.state.has_more_content then
        vim.schedule(function()
          if M.state.preview_generation == generation then ensure_content_loaded_async(target_line) end
        end)
      else
        -- Enough content loaded — re-apply location highlighting so the
        -- preview scrolls to the correct line now that it exists in the buffer
        M.apply_location_highlighting(M.state.bufnr)
      end
    else
      -- EOF with no additional data — mark loading as finished to prevent
      -- apply_location_highlighting -> ensure_content_loaded_async recursion,
      -- then apply highlighting with whatever content we have.
      M.state.has_more_content = false
      M.apply_location_highlighting(M.state.bufnr)
    end
  end)
end

local function link_buffer_content(source_bufnr, target_bufnr)
  local lines = vim.api.nvim_buf_get_lines(source_bufnr, 0, -1, false)
  set_buffer_lines(target_bufnr, lines)

  local source_ft = vim.api.nvim_get_option_value('filetype', { buf = source_bufnr })
  if source_ft ~= '' then
    local ok = set_preview_filetype(target_bufnr, source_ft)
    if not ok then return false end
  end

  M.state.has_more_content = false
  M.state.total_file_lines = #lines
  M.state.loaded_lines = #lines
  M.state.content_height = #lines

  return true
end

M.config = nil

M.state = {
  bufnr = nil,
  winid = nil,
  current_file = nil,
  scroll_offset = 0,
  content_height = 0,
  loaded_lines = 0,
  total_file_lines = nil,
  loading_chunk_size = 1000,
  is_loading = false,
  has_more_content = true,
  file_handle = nil, ---@type uv.uv_fs_t|nil
  file_operation = nil, -- Ongoing file operation: {fd?: any, file_path?: string, position?: number}
  location = nil, -- Current location data for highlighting
  location_namespace = nil, -- Namespace for location highlighting
  preview_generation = 0, -- Monotonically increasing token to detect stale async callbacks
  is_binary_preview = false, -- Whether the current preview is a hex dump
  hex_byte_offset = 0, -- Next byte offset for hex dump paging
}

--- Setup preview configuration
--- @param config table Configuration options
function M.setup(config)
  M.config = config or {}
  -- Create namespace for location highlighting
  if not M.state.location_namespace then
    M.state.location_namespace = vim.api.nvim_create_namespace('fff_preview_location')
  end
end

--- Check if file is too big for initial preview (inspired by snacks.nvim)
--- @param file_path string Path to the file
--- @return boolean True if file is too big for initial preview
function M.is_big_file(file_path)
  -- Only check file size for early detection - no line limits with dynamic loading
  local stat = vim.uv.fs_stat(file_path)
  if stat and stat.size > M.config.max_size then return true end

  return false
end

--- Get file information
--- @param file_path string Path to the file
--- @return table | nil File information
function M.get_file_info(file_path)
  local stat = vim.uv.fs_stat(file_path)
  if not stat then return nil end

  local info = {
    name = vim.fn.fnamemodify(file_path, ':t'),
    path = file_path,
    size = stat.size,
    modified = stat.mtime.sec,
    accessed = stat.atime.sec,
    type = stat.type,
  }

  info.extension = vim.fn.fnamemodify(file_path, ':e'):lower()
  info.filetype = utils.detect_filetype(file_path) or 'text'
  info.size_formatted = utils.format_file_size(info.size)
  info.modified_formatted = os.date('%Y-%m-%d %H:%M:%S', info.modified)
  info.accessed_formatted = os.date('%Y-%m-%d %H:%M:%S', info.accessed)

  return info
end

--- Preview a regular file
--- @param file_path string Path to the file
--- @param bufnr number Buffer number for preview
--- @return boolean Success status
function M.preview_file(file_path, bufnr)
  -- Early size detection to prevent memory issues
  if M.is_big_file(file_path) then
    local info = M.get_file_info(file_path)
    local lines = {
      'File too large for preview',
      string.format(
        'Size: %s (max: %s)',
        info and info.size_formatted or 'Unknown',
        string.format('%.1fMB', M.config.max_size / 1024 / 1024)
      ),
      '',
      'Use a text editor to view this file.',
    }
    set_buffer_lines(bufnr, lines)
    return true
  end

  local info = M.get_file_info(file_path)
  if not info then return false end

  -- if the buffer is already opened for this file we reuse the buffer directly
  local existing_bufnr = find_existing_buffer(file_path)

  if existing_bufnr then
    local success = link_buffer_content(existing_bufnr, bufnr)
    if success then
      local file_config = M.get_file_config(file_path)

      vim.api.nvim_set_option_value('modifiable', false, { buf = bufnr })
      vim.api.nvim_set_option_value('readonly', true, { buf = bufnr })
      vim.api.nvim_set_option_value('buftype', 'nofile', { buf = bufnr })
      if M.state.winid and vim.api.nvim_win_is_valid(M.state.winid) then
        vim.api.nvim_set_option_value('wrap', file_config.wrap_lines or M.config.wrap_lines, { win = M.state.winid })
      end

      M.state.scroll_offset = 0

      -- Apply location highlighting if available (delayed to ensure buffer is ready)
      local gen = M.state.preview_generation
      vim.schedule(function()
        if M.state.preview_generation == gen then M.apply_location_highlighting(bufnr) end
      end)

      return true
    end
  end

  M.state.current_file = file_path
  M.state.bufnr = bufnr
  local generation = M.state.preview_generation

  read_file_streaming_async(file_path, function(content, err, loading_more)
    if M.state.preview_generation ~= generation then
      -- Preview moved on to a different file, discard
      cleanup_file_operation()
      return
    end

    if err or not content then
      if M.state.current_file == file_path then
        set_buffer_lines(bufnr, { 'Failed to load file: ' .. (err or 'unknown error') })
      end
      return
    end

    if M.state.current_file == file_path then
      -- Guard against buffer being destroyed while async read was in-flight
      if not vim.api.nvim_buf_is_valid(bufnr) then return end

      M.clear_preview_visual_state(bufnr)
      set_buffer_lines(bufnr, content)

      local file_config = M.get_file_config(file_path)
      local ok = set_preview_filetype(bufnr, info.filetype)
      if not ok then
        show_preview_unavailable(bufnr)
        return
      end
      vim.api.nvim_set_option_value('modifiable', false, { buf = bufnr })
      vim.api.nvim_set_option_value('readonly', true, { buf = bufnr })
      vim.api.nvim_set_option_value('buftype', 'nofile', { buf = bufnr })
      if M.state.winid and vim.api.nvim_win_is_valid(M.state.winid) then
        vim.api.nvim_set_option_value('wrap', file_config.wrap_lines or M.config.wrap_lines, { win = M.state.winid })
      end

      M.state.content_height = #content
      M.state.scroll_offset = 0

      -- Apply location highlighting if available (delayed to ensure buffer is ready).
      -- Skip when more content is being loaded asynchronously to reach the target line —
      -- ensure_content_loaded_async will re-apply highlighting once the target is in the buffer.
      if not loading_more then
        vim.schedule(function()
          if M.state.preview_generation == generation then M.apply_location_highlighting(bufnr) end
        end)
      end
    end
  end)

  return true
end

-- Hex preview highlight support: dynamically create hl groups from "#rrggbb"
local hex_ns = nil
local hex_hl_cache = {}

local function ensure_hex_ns()
  if not hex_ns then hex_ns = vim.api.nvim_create_namespace('fff_hex_preview') end
  return hex_ns
end

local function get_hex_hl_group(hex_color)
  local cached = hex_hl_cache[hex_color]
  if cached then return cached end
  local group = 'FffHex_' .. hex_color:sub(2)
  vim.api.nvim_set_hl(0, group, { fg = hex_color })
  hex_hl_cache[hex_color] = group
  return group
end

--- Apply hex highlight spans to a buffer
--- @param bufnr number Buffer number
--- @param highlights table Array of {line_0idx, col_start, col_end, "#rrggbb"}
--- @param line_offset number Lines to add to each highlight line index (for header)
local function apply_hex_highlights(bufnr, highlights, line_offset)
  if not highlights or not vim.api.nvim_buf_is_valid(bufnr) then return end
  local ns = ensure_hex_ns()
  for _, hl in ipairs(highlights) do
    pcall(vim.api.nvim_buf_set_extmark, bufnr, ns, hl[1] + line_offset, hl[2], {
      end_col = hl[3],
      hl_group = get_hex_hl_group(hl[4]),
    })
  end
end

--- Load a page of hex dump content from the Rust backend
--- @param file_path string Path to the binary file
--- @param byte_offset number Byte offset to start reading from
--- @return table|nil Result with lines, highlights, has_more, next_offset
local function load_hex_page(file_path, byte_offset)
  local ok, result = pcall(rust.hex_dump, file_path, byte_offset, 4096)
  if ok and result then return result end
  return nil
end

--- Load more hex content when scrolling near the end of the buffer
local function load_more_hex_content()
  if not M.state.bufnr or not vim.api.nvim_buf_is_valid(M.state.bufnr) then return end
  if not M.state.has_more_content or not M.state.current_file then return end

  local current_lines = vim.api.nvim_buf_line_count(M.state.bufnr)
  local result = load_hex_page(M.state.current_file, M.state.hex_byte_offset)
  if result and result.lines and #result.lines > 0 then
    append_buffer_lines(M.state.bufnr, result.lines)
    M.state.hex_byte_offset = result.next_offset
    M.state.has_more_content = result.has_more
    M.state.content_height = vim.api.nvim_buf_line_count(M.state.bufnr)
    M.state.loaded_lines = M.state.content_height
    apply_hex_highlights(M.state.bufnr, result.highlights, current_lines)
  else
    M.state.has_more_content = false
  end
end

--- Preview a binary file using hexyl-powered hex dump with paging
--- @param file_path string Path to the file
--- @param bufnr number Buffer number for preview
--- @return boolean Success status
function M.preview_binary_file(file_path, bufnr)
  local info = M.get_file_info(file_path)
  local lines = {}

  M.state.is_binary_preview = true
  M.state.hex_byte_offset = 0

  -- Build header synchronously (file -b is fast, typically <10ms)
  if vim.fn.executable('file') == 1 then
    local output = vim.fn.system({ 'file', '-b', file_path })
    if vim.v.shell_error == 0 and output then
      local file_type = output:gsub('\n', '')
      table.insert(lines, 'Binary file: ' .. file_type)
      if info and info.size_formatted then table.insert(lines, 'Size: ' .. info.size_formatted) end
      table.insert(lines, '')
    end
  end

  local hex_result = load_hex_page(file_path, 0)
  if hex_result and hex_result.lines then
    for _, hex_line in ipairs(hex_result.lines) do
      table.insert(lines, hex_line)
    end
    M.state.hex_byte_offset = hex_result.next_offset
    M.state.has_more_content = hex_result.has_more
  end

  set_buffer_lines(bufnr, lines)
  local ok = set_preview_filetype(bufnr, 'text')
  if not ok then
    show_preview_unavailable(bufnr)
    return false
  end
  vim.api.nvim_set_option_value('modifiable', false, { buf = bufnr })
  vim.api.nvim_set_option_value('readonly', true, { buf = bufnr })

  M.state.content_height = #lines
  M.state.loaded_lines = #lines

  if hex_result and hex_result.highlights then
    local header_lines = #lines - (hex_result.lines and #hex_result.lines or 0)
    apply_hex_highlights(bufnr, hex_result.highlights, header_lines)
  end

  return true
end

--- Get file-specific configuration
--- @param file_path string Path to the file
--- @return table Configuration for the file
function M.get_file_config(file_path)
  if not M.config or not M.config.filetypes then return {} end

  local filetype = utils.detect_filetype(file_path) or 'text'
  return M.config.filetypes[filetype] or {}
end

--- @param file_path string Path to the file or directory
--- @param bufnr number Buffer number for preview
--- @param location table|nil Optional location data for highlighting
--- @param is_binary boolean|nil Whether the file is binary (from Rust indexer)
--- @return boolean if the preview was successful
function M.preview(file_path, bufnr, location, is_binary)
  if not file_path or file_path == '' then return false end

  -- Bump generation to invalidate any in-flight async callbacks from previous previews
  M.state.preview_generation = M.state.preview_generation + 1

  if M.state.file_handle then
    ---@diagnostic disable-next-line: undefined-field
    M.state.file_handle:close()
    M.state.file_handle = nil
  end

  M.state.loaded_lines = 0
  M.state.total_file_lines = nil
  M.state.has_more_content = true
  M.state.is_loading = false
  M.state.hex_byte_offset = 0

  M.state.is_binary_preview = false

  M.state.current_file = file_path
  M.state.bufnr = bufnr
  M.state.location = location

  if image.is_image(file_path) then
    M.clear_buffer(bufnr)

    if not M.state.winid or not vim.api.nvim_win_is_valid(M.state.winid) then return false end

    return image.display_image(file_path, bufnr)
  elseif is_binary then
    return M.preview_binary_file(file_path, bufnr)
  else
    return M.preview_file(file_path, bufnr)
  end
end

function M.scroll(lines)
  if not M.state.bufnr or not vim.api.nvim_buf_is_valid(M.state.bufnr) then return end
  if not M.state.winid or not vim.api.nvim_win_is_valid(M.state.winid) then return end

  local win_height = vim.api.nvim_win_get_height(M.state.winid)
  local current_buffer_lines = vim.api.nvim_buf_line_count(M.state.bufnr)

  local current_offset = M.state.scroll_offset or 0
  local new_offset = current_offset + lines

  -- If scrolling down and approaching end of loaded content, try to load more
  if lines > 0 and not M.state.is_loading then
    local target_line = new_offset + win_height
    local buffer_needed = target_line + 20

    if current_buffer_lines < buffer_needed and M.state.has_more_content then
      if M.state.is_binary_preview then
        load_more_hex_content()
        -- Re-read line count after loading more
        current_buffer_lines = vim.api.nvim_buf_line_count(M.state.bufnr)
      else
        ensure_content_loaded_async(target_line)
      end
    end
  end

  local content_height = current_buffer_lines
  local half_screen = math.floor(win_height / 2)
  local max_scroll = math.max(0, content_height + half_screen - win_height)

  new_offset = math.max(0, math.min(max_scroll, new_offset))
  if new_offset ~= current_offset then
    M.state.scroll_offset = new_offset
    M.state.content_height = content_height

    local target_line = math.min(content_height, math.max(1, new_offset + 1))

    vim.api.nvim_win_call(M.state.winid, function()
      vim.api.nvim_win_set_cursor(M.state.winid, { target_line, 0 })
      vim.fn.winrestview({ topline = target_line })
    end)
  end
end

--- Set preview window
--- @param winid number Window ID for the preview
function M.set_preview_window(winid) M.state.winid = winid end

local function resolve_absolute_path(rel)
  if not rel or rel == '' then return nil end
  if rel:sub(1, 1) == '/' then return rel end
  local base = require('fff.conf').get().base_path
  if base and base ~= '' then return (base:sub(-1) == '/') and (base .. rel) or (base .. '/' .. rel) end
  return vim.fn.fnamemodify(rel, ':p')
end

local function build_file_info_data(file, file_metadata, file_index)
  local conf = require('fff.conf').get()
  local is_grep = file.line_number ~= nil
  local sections = (conf.debug and conf.debug.show_file_info)
    or { file_info = true, score_breakdown = true, timings = true, full_path = true }

  -- Grep matches don't have a per-file fuzzy score, so the breakdown would
  -- be all "N/A". Skip it but keep the rest of the panel identical.
  if is_grep then sections = vim.tbl_extend('force', sections, { score_breakdown = false }) end

  local abs_path = resolve_absolute_path(file.relative_path)
  local times_opened = 0
  if abs_path then
    local ok, n = pcall(rust.get_file_access_count, abs_path)
    if ok and type(n) == 'number' then times_opened = n end
  end

  local file_view = {
    relative_path = file.relative_path,
    absolute_path = abs_path or file.absolute_path or file_metadata.path,
    size_formatted = file_metadata.size_formatted,
    filetype = file_metadata.filetype or 'text',
    git_status = file.git_status or 'clean',
    access_frecency_score = file.access_frecency_score or 0,
    modification_frecency_score = file.modification_frecency_score or 0,
    times_opened = times_opened,
    modified_formatted = file_metadata.modified_formatted,
    accessed_formatted = file_metadata.accessed_formatted,
  }

  local score = nil
  if not is_grep and file_index then score = file_picker.get_file_score(file_index) end

  return {
    file = file_view,
    score = score,
    sections = sections,
    hls = conf.hl,
  }
end

local function apply_file_info_extmarks(bufnr, extmarks)
  vim.api.nvim_buf_clear_namespace(bufnr, M.file_info_ns, 0, -1)
  for _, m in ipairs(extmarks) do
    local opts = {}
    if m.hl_group then
      opts.hl_group = m.hl_group
      if m.end_col then opts.end_col = m.end_col end
    end
    if m.virt_text then
      opts.virt_text = m.virt_text
      opts.virt_text_pos = m.virt_text_pos or 'overlay'
    end
    pcall(vim.api.nvim_buf_set_extmark, bufnr, M.file_info_ns, m.row, m.col or 0, opts)
  end
end

local function resize_file_info(file_info_win, target_h, preview_win)
  if not (file_info_win and vim.api.nvim_win_is_valid(file_info_win)) then return end
  local current_h = vim.api.nvim_win_get_height(file_info_win)
  local delta = target_h - current_h
  if delta == 0 then return end
  pcall(vim.api.nvim_win_set_config, file_info_win, { height = target_h })
  if preview_win and vim.api.nvim_win_is_valid(preview_win) then
    local cfg = vim.api.nvim_win_get_config(preview_win)
    if cfg.row and cfg.height then
      local new_row = (type(cfg.row) == 'table' and cfg.row[false] or cfg.row) + delta
      pcall(vim.api.nvim_win_set_config, preview_win, {
        relative = cfg.relative or 'editor',
        row = new_row,
        col = cfg.col,
        width = cfg.width,
        height = math.max(3, cfg.height - delta),
      })
    end
  end
end

--- Update file info buffer
--- @param file table
--- @param bufnr number
--- @param file_index number|nil
--- @param preview_win number|nil Preview window — shifted to keep flush with file_info
--- @return boolean
function M.update_file_info_buffer(file, bufnr, file_index, preview_win)
  if not file then
    set_buffer_lines(bufnr, { 'No file selected' })
    return false
  end

  local info = M.get_file_info(file.relative_path)
  if not info then
    set_buffer_lines(bufnr, { 'File info unavailable' })
    return false
  end

  local width = 80
  local target_win
  local wins = vim.fn.win_findbuf(bufnr)
  for _, win in ipairs(wins) do
    if vim.api.nvim_win_is_valid(win) then
      width = vim.api.nvim_win_get_width(win)
      target_win = win
      break
    end
  end

  local data = build_file_info_data(file, info, file_index)
  data.width = width
  local result = file_info_renderer.build(data)

  set_buffer_lines(bufnr, result.lines)
  apply_file_info_extmarks(bufnr, result.extmarks)

  vim.api.nvim_set_option_value('modifiable', false, { buf = bufnr })
  vim.api.nvim_set_option_value('readonly', true, { buf = bufnr })
  vim.api.nvim_set_option_value('buftype', 'nofile', { buf = bufnr })

  for _, win in ipairs(wins) do
    if vim.api.nvim_win_is_valid(win) then vim.api.nvim_set_option_value('wrap', false, { win = win }) end
  end

  if result.height > 0 then resize_file_info(target_win, result.height, preview_win) end

  return true
end

function M.clear_preview_visual_state(bufnr)
  if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then return end

  -- Only clear visual state, don't affect buffer functionality
  -- Clear namespaces and extmarks for this buffer only
  vim.api.nvim_buf_clear_namespace(bufnr, -1, 0, -1)

  -- Clear location highlights
  if M.state.location_namespace then location_utils.clear_location_highlights(bufnr, M.state.location_namespace) end

  -- no `normal!` here: it drops the prompt out of insert mode
  for _, win in ipairs(vim.fn.win_findbuf(bufnr)) do
    vim.api.nvim_set_option_value('foldenable', false, { win = win, scope = 'local' })
  end

  image.clear_buffer_images(bufnr)
end

function M.clear_buffer(bufnr)
  if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then return end

  cleanup_file_operation()
  M.clear_preview_visual_state(bufnr)

  pcall(vim.treesitter.stop, bufnr)

  vim.api.nvim_set_option_value('modifiable', true, { buf = bufnr })
  set_preview_filetype(bufnr, '')
  vim.api.nvim_set_option_value('syntax', '', { buf = bufnr })
  vim.api.nvim_set_option_value('buftype', 'nofile', { buf = bufnr })

  set_buffer_lines(bufnr, {})
end

function M.clear()
  -- Bump generation to invalidate any in-flight async callbacks
  M.state.preview_generation = M.state.preview_generation + 1

  cleanup_file_operation()

  M.state.loaded_lines = 0
  M.state.total_file_lines = nil
  M.state.has_more_content = true
  M.state.is_loading = false

  if M.state.bufnr and vim.api.nvim_buf_is_valid(M.state.bufnr) then M.clear_buffer(M.state.bufnr) end

  M.state.current_file = nil
  M.state.scroll_offset = 0
  M.state.content_height = 0
  M.state.location = nil
  M.state.is_binary_preview = false
  M.state.hex_byte_offset = 0
end

--- Apply location highlighting to the preview buffer
--- @param bufnr number Buffer number
function M.apply_location_highlighting(bufnr)
  -- Ensure namespace is created
  if not M.state.location_namespace then
    M.state.location_namespace = vim.api.nvim_create_namespace('fff_preview_location')
  end

  -- Always clear previous location highlights first
  if vim.api.nvim_buf_is_valid(bufnr) then
    location_utils.clear_location_highlights(bufnr, M.state.location_namespace)
  end

  if not M.state.location then return end

  location_utils.highlight_location(bufnr, M.state.location, M.state.location_namespace)

  if M.state.winid and vim.api.nvim_win_is_valid(M.state.winid) then
    local target_line = location_utils.get_target_line(M.state.location)
    if target_line then
      local buffer_lines = vim.api.nvim_buf_line_count(bufnr)
      if target_line > buffer_lines and M.state.has_more_content then
        -- Target line is beyond loaded content — load more first.
        -- ensure_content_loaded_async will re-apply highlighting when done.
        ensure_content_loaded_async(target_line)
        return
      end
      M.scroll_to_line(target_line)
    end
  end
end

--- Scroll preview to a specific line
--- @param line number Target line number (1-indexed)
function M.scroll_to_line(line)
  if not M.state.winid or not vim.api.nvim_win_is_valid(M.state.winid) then return end
  if not M.state.bufnr or not vim.api.nvim_buf_is_valid(M.state.bufnr) then return end

  local win_height = vim.api.nvim_win_get_height(M.state.winid)
  local buffer_lines = vim.api.nvim_buf_line_count(M.state.bufnr)
  local target_line = math.max(1, math.min(line, buffer_lines))

  local half_screen = math.floor(win_height / 2)
  local new_offset = math.max(0, target_line - half_screen)

  M.state.scroll_offset = new_offset
  pcall(vim.api.nvim_win_call, M.state.winid, function()
    vim.api.nvim_win_set_cursor(M.state.winid, { target_line, 0 })
    vim.fn.winrestview({ topline = target_line })
  end)
end

return M
