local utils = require('fff.utils')

local M = {}

local function fetch_rust_checkhealth(rust_module, test_path)
  if not rust_module.health_check then
    return nil, 'health_check function not available in rust module (binary may be outdated)'
  end

  local ok, result = pcall(rust_module.health_check, test_path)
  if not ok then return nil, 'Failed to call health_check: ' .. tostring(result) end

  return result, nil
end

-- Report health for a rust-side LMDB section (frecency / query_tracker).
-- opts.populate(section, db_info) copies DB-specific counters into health_section.
-- opts.format_healthy_msg(db_info) returns the message for the healthy path;
-- the unhealthy path is handled uniformly here as a critical error.
local function report_db_health(rust_section, health_section, messages, label, opts)
  if not rust_section then return end

  health_section.initialized = rust_section.initialized
  health_section.error = rust_section.error

  if not rust_section.initialized then
    table.insert(messages, {
      level = 'info',
      msg = label .. ' not initialized (will initialize on first use)',
    })
    return
  end

  local db_info = rust_section.db_healthcheck
  if not db_info then
    if rust_section.db_healthcheck_error then
      table.insert(messages, {
        level = 'warn',
        msg = label .. ' initialized but health check failed: ' .. rust_section.db_healthcheck_error,
      })
    else
      table.insert(messages, { level = 'ok', msg = label .. ' initialized' })
    end
    return
  end

  health_section.db_path = db_info.path
  health_section.disk_size = db_info.disk_size
  health_section.healthy = db_info.healthy
  if opts.populate then opts.populate(health_section, db_info) end

  if db_info.healthy == false then
    table.insert(messages, {
      level = 'error',
      msg = string.format(
        '%s UNRESPONSIVE — CRITICAL ERROR (see logs; try removing %s/lock.mdb to unblock)',
        label,
        db_info.path or 'unknown'
      ),
    })
    return
  end

  table.insert(messages, {
    level = 'ok',
    msg = opts.format_healthy_msg(db_info),
  })
end

--- Check snacks.nvim image preview availability
--- @return table image_preview_info
local function check_image_preview()
  local ok, image = pcall(require, 'fff.file_picker.image')
  if not ok then
    return {
      available = false,
      snacks_available = false,
      snacks_image_available = false,
      terminal_supported = false,
      error = 'failed to load image module',
    }
  end

  return image.get_preview_status()
end

--- Check icon provider availability
--- @return table icon_provider_info
local function check_icon_provider()
  local ok, icons = pcall(require, 'fff.file_picker.icons')
  if not ok then return {
    available = false,
    name = nil,
    supports_directories = false,
  } end

  return icons.get_provider_info()
end

--- Run the health check and return structured results
--- @param opts? { test_path?: string } Options for health check
--- @return table health_result
function M.run(opts)
  opts = opts or {}

  local health = {
    ok = true,
    binary = {
      available = false,
      path = nil,
      error = nil,
    },
    rust = {
      version = nil,
      git = {
        available = false,
        repository_found = false,
        workdir = nil,
        libgit2_version = nil,
        error = nil,
      },
      file_picker = {
        initialized = false,
        base_path = nil,
        is_scanning = false,
        indexed_files = 0,
        error = nil,
      },
      frecency = {
        initialized = false,
        db_path = nil,
        disk_size = nil,
        entries = nil,
        healthy = nil,
        error = nil,
      },
      query_tracker = {
        initialized = false,
        db_path = nil,
        disk_size = nil,
        query_file_entries = nil,
        query_history_entries = nil,
        healthy = nil,
        error = nil,
      },
    },
    image_preview = {
      available = false,
      snacks_available = false,
      snacks_image_available = false,
      terminal_supported = false,
      error = nil,
    },
    icon_provider = {
      available = false,
      name = nil,
      supports_directories = false,
    },
    messages = {},
  }

  -- Check binary availability
  local download = require('fff.download')
  health.binary.path = download.get_binary_path()

  local binary_ok, rust_module = pcall(require, 'fff.rust')
  if not binary_ok then
    health.ok = false
    health.binary.available = false
    health.binary.error = tostring(rust_module)
    table.insert(health.messages, {
      level = 'error',
      msg = 'Binary not available: ' .. tostring(rust_module),
    })
    return health
  end

  health.binary.available = true
  table.insert(health.messages, {
    level = 'ok',
    msg = 'Binary loaded successfully from: ' .. health.binary.path,
  })

  local rust_health, rust_err = fetch_rust_checkhealth(rust_module, opts.test_path)
  if rust_health then
    health.rust.version = rust_health.version
    table.insert(health.messages, {
      level = 'ok',
      msg = 'fff.nvim version: ' .. (rust_health.version or 'unknown'),
    })

    if rust_health.git then
      health.rust.git.available = rust_health.git.available
      health.rust.git.repository_found = rust_health.git.repository_found
      health.rust.git.workdir = rust_health.git.workdir
      health.rust.git.libgit2_version = rust_health.git.libgit2_version
      health.rust.git.error = rust_health.git.error

      if rust_health.git.available then
        table.insert(health.messages, {
          level = 'ok',
          msg = 'libgit2 available (version: ' .. (rust_health.git.libgit2_version or 'unknown') .. ')',
        })

        if rust_health.git.repository_found then
          table.insert(health.messages, {
            level = 'ok',
            msg = 'Git repository found: ' .. (rust_health.git.workdir or 'unknown'),
          })
        else
          table.insert(health.messages, {
            level = 'info',
            msg = 'No git repository found in current directory'
              .. (rust_health.git.error and (': ' .. rust_health.git.error) or ''),
          })
        end
      else
        table.insert(health.messages, {
          level = 'warn',
          msg = 'libgit2 not available',
        })
      end
    end

    if rust_health.file_picker then
      health.rust.file_picker.initialized = rust_health.file_picker.initialized
      health.rust.file_picker.base_path = rust_health.file_picker.base_path
      health.rust.file_picker.is_scanning = rust_health.file_picker.is_scanning
      health.rust.file_picker.indexed_files = rust_health.file_picker.indexed_files
      health.rust.file_picker.error = rust_health.file_picker.error

      if rust_health.file_picker.initialized then
        local status = rust_health.file_picker.is_scanning and 'scanning' or 'ready'

        table.insert(health.messages, {
          level = 'ok',
          msg = string.format(
            'File picker initialized (%s, %d files indexed, base: %s)',
            status,
            rust_health.file_picker.indexed_files or 0,
            rust_health.file_picker.base_path or 'unknown'
          ),
        })

        local gpu = rust_health.file_picker.gpu
        if gpu then
          health.rust.file_picker.gpu = gpu
          if not gpu.compiled then
            table.insert(health.messages, { level = 'info', msg = 'GPU matcher not compiled in (build with `make build-gpu`)' })
          elseif not gpu.enabled then
            table.insert(health.messages, { level = 'info', msg = 'GPU matcher compiled but disabled (FFF_GPU=0)' })
          elseif gpu.adapter then
            table.insert(health.messages, {
              level = 'ok',
              msg = string.format('GPU matcher active on %s (%d files indexed)', gpu.adapter, gpu.indexed_files or 0),
            })
          else
            table.insert(health.messages, { level = 'ok', msg = 'GPU matcher enabled (index builds on first search)' })
          end
        end
      else
        table.insert(health.messages, {
          level = 'info',
          msg = 'File picker not initialized (will initialize on first use)',
        })
      end
    end

    report_db_health(rust_health.frecency, health.rust.frecency, health.messages, 'Frecency database', {
      populate = function(section, db_info) section.entries = db_info.absolute_frecency_entries end,
      format_healthy_msg = function(db_info)
        return string.format(
          'Frecency database operational (%d entries, %s, path: %s)',
          db_info.absolute_frecency_entries or 0,
          utils.format_file_size(db_info.disk_size or 0),
          db_info.path or 'unknown'
        )
      end,
    })

    report_db_health(rust_health.query_tracker, health.rust.query_tracker, health.messages, 'Query cache database', {
      populate = function(section, db_info)
        section.query_file_entries = db_info.query_file_entries
        section.query_history_entries = db_info.query_history_entries
      end,
      format_healthy_msg = function(db_info)
        return string.format(
          'Query cache database operational (%d query-file mappings, %d history entries, %s, path: %s)',
          db_info.query_file_entries or 0,
          db_info.query_history_entries or 0,
          utils.format_file_size(db_info.disk_size or 0),
          db_info.path or 'unknown'
        )
      end,
    })
  else
    health.ok = false
    table.insert(health.messages, {
      level = 'error',
      msg = rust_err or 'Unknown error getting rust health data',
    })
    return health
  end

  local image_info = check_image_preview()
  health.image_preview.snacks_available = image_info.snacks_available
  health.image_preview.snacks_image_available = image_info.snacks_image_available
  health.image_preview.terminal_supported = image_info.terminal_supported
  health.image_preview.error = image_info.error
  health.image_preview.available = image_info.available

  if image_info.available then
    table.insert(health.messages, {
      level = 'ok',
      msg = 'Image preview available via snacks.nvim',
    })
  elseif image_info.snacks_available and image_info.snacks_image_available then
    table.insert(health.messages, {
      level = 'info',
      msg = 'Image preview not available: ' .. (image_info.error or 'terminal does not support images'),
    })
  elseif image_info.snacks_available then
    table.insert(health.messages, {
      level = 'info',
      msg = 'Image preview not available: snacks.image module not found',
    })
  else
    table.insert(health.messages, {
      level = 'info',
      msg = 'Image preview not available: snacks.nvim not installed',
    })
  end

  local icon_info = check_icon_provider()
  health.icon_provider.available = icon_info.available
  health.icon_provider.name = icon_info.name
  health.icon_provider.supports_directories = icon_info.supports_directories

  if icon_info.available then
    table.insert(health.messages, {
      level = 'ok',
      msg = 'Filetype icons available via ' .. icon_info.name,
    })
  else
    table.insert(health.messages, {
      level = 'info',
      msg = 'Filetype icons not available (install nvim-web-devicons or mini.icons)',
    })
  end

  return health
end

function M.check()
  vim.health.start('fff.nvim')

  local result = M.run()

  for _, msg in ipairs(result.messages) do
    if msg.level == 'ok' then
      vim.health.ok(msg.msg)
    elseif msg.level == 'warn' then
      vim.health.warn(msg.msg)
    elseif msg.level == 'error' then
      vim.health.error(msg.msg)
    elseif msg.level == 'info' then
      vim.health.info(msg.msg)
    end
  end

  if not result.binary.available then
    vim.health.info('To install the binary, run:')
    vim.health.info('  :lua require("fff.download").download_or_build_binary()')
    vim.health.info('Or build from source with:')
    vim.health.info('  cargo build --release')
  end
end

return M
