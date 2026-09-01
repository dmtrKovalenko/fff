<a href="./assets/logo-orange.png"><img alt="FFF" src="./assets/logo-orange.png" width="300"></a>

<p>
  <i>A file search toolkit for humans and AI agents. Really fast.</i>
</p>

Typo-resistant path and content search, frequency-ranked file access, a background watcher, and a lightweight in-memory content index. Way faster than CLIs like ripgrep and fzf in any long-running process that searches more than once.

Powers file search in [opencode](http://github.com/anomalyco/opencode/), [nushell](https://github.com/nushell/nushell), and many more amazing projects!

Originally started as [Neovim plugin](#neovim-plugin) people loved, but it turned out that plenty of AI harnesses and code editors need the same thing: accurate, fast file search as a library. That is what fff is.
<p>
<a href="https://trendshift.io/repositories/26711?utm_source=repository-badge&amp;utm_medium=badge&amp;utm_campaign=badge-repository-26711" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/repositories/26711" alt="dmtrKovalenko%2Ffff | Trendshift" width="250" height="55"/></a>
</p>

---

## Sponsors

fff is MIT and open source forever. Development is supported by these companies:

<table>
  <tr>
    <td align="center" width="110"><sub><b>💎<br>DIAMOND</b></sub></td>
    <td align="center" width="420"><a href="https://anoma.ly"><img alt="Anomaly" src="./assets/sponsors/anomaly.png" width="400"></a></td>
    <td><b><a href="https://anoma.ly">Anomaly</a></b><br><sub>The team behind <a href="https://opencode.ai">opencode</a>.</sub></td>
  </tr>
  <tr>
    <td align="center"><sub><b>🥇<br>GOLD</b></sub></td>
    <td align="center"><a href="https://mangoproxy.com/?utm_source=dmtrkovalenko&utm_medium=partner&utm_campaign=dmtrkovalenko_github"><img alt="Mango Proxy" src="./assets/sponsors/mango-proxy.png" width="280"></a></td>
    <td><b><a href="https://mangoproxy.com/?utm_source=dmtrkovalenko&utm_medium=partner&utm_campaign=dmtrkovalenko_github">Mango Proxy</a></b><br><sub>Fast, secure proxies for all the needs.</sub></td>
  </tr>
</table>

<sub>Use and enjoy fff? <a href="mailto:dmitriy@iusevimbtw.com">Become a sponsor to get your features/fixes the highest priority</a>.</sub>

---

Pick what you are interested in:

<details id="mcp-server">
<summary>
<h2>MCP server</h2>
</summary>

Works with Claude Code, Codex, OpenCode, Cursor, Cline, and any MCP-capable client. Fewer grep roundtrips, less wasted context, faster answers.

![Benchmark chart comparing FFF against the built-in AI file-search tools](./chart.png)

### One-line install

Linux / macOS:

```bash
curl -L https://dmtrkovalenko.dev/install-fff-mcp.sh | bash
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/dmtrKovalenko/fff/main/install-mcp.ps1 | iex
```

The scripts live at [`install-mcp.sh`](./install-mcp.sh) and [`install-mcp.ps1`](./install-mcp.ps1) if you want to read them first. They print the exact wiring instructions for your client.

### Homebrew (macOS / Linux)

```bash
brew install dmtrKovalenko/fff/fff-mcp
brew upgrade fff-mcp   # after new stable releases
```

Formula lives in [`Formula/fff-mcp.rb`](./Formula/fff-mcp.rb) in this repo and is **auto-bumped on every stable release** (see `bump-homebrew-formula` in [`.github/workflows/release.yaml`](./.github/workflows/release.yaml)). Installs the prebuilt `fff-mcp` binary from [GitHub releases](https://github.com/dmtrKovalenko/fff/releases).

### Codex setup

Register the installed binary using its absolute path, since Codex desktop sessions may not inherit your interactive shell's `PATH`.

Homebrew:

```bash
codex mcp add fff -- "$(brew --prefix)/bin/fff-mcp"
```

One-line installer:

```bash
codex mcp add fff -- "$HOME/.local/bin/fff-mcp"
```

This creates an entry in `~/.codex/config.toml` similar to:

```toml
[mcp_servers.fff]
command = "/opt/homebrew/bin/fff-mcp"
```

Use the actual installed path for your system, then restart Codex or start a new task so it loads the server.

Once the server is connected, ask the agent to "use fff" and it picks up the `ffgrep`, `fffind`, and `fff-multi-grep` tools.

### Recommended agent prompt

Drop this into your project's `CLAUDE.md` or equivalent:

```markdown
For any file search or grep in the current git-indexed directory, use fff tools.
```

### What changes

- Frecency memory. Files you actually open rank higher next time. Warm-up from git touch history runs automatically.
- Definition-first hinting. Lines that look like code definitions are classified on the Rust side, no regex overhead in your prompt.
- Smart-case with auto-fuzzy fallback. `IsOffTheRecord` finds snake_case variants; zero-match queries retry as fuzzy and surface the best approximate hits.
- Git-aware annotations. Modified, untracked, and staged files are tagged so the agent reaches for what you are actively changing.

Source: [`crates/fff-mcp/`](./crates/fff-mcp/).

</details>

The MCP server gives any agent a file search tool that is faster and more token-efficient than the built-in one.

<details id="pi-extension">
<summary>
<h2>Pi agent extension</h2>
</summary>

### Install

```bash
pi install npm:@ff-labs/pi-fff
```

### Modes

Four operating modes, switchable at runtime with `/fff-mode`:

| Mode                     | What it does                                                                      |
| ------------------------ | --------------------------------------------------------------------------------- |
| `tools-and-ui` (default) | Adds `ffgrep` and `fffind` tools, replaces `@`-mention autocomplete with FFF.     |
| `tools-only`             | Only tool injection. Keeps pi's native editor autocomplete.                       |
| `ui-only`                | Only `@`-file autocomplete with FFF; no FFF agent tools.                         |
| `override`               | Replaces pi's built-in `grep`, `find`, and `multi_grep` with FFF implementations. |

Env vars: `PI_FFF_MODE`, `FFF_FRECENCY_DB`, `FFF_HISTORY_DB`. Flags: `--fff-mode`, `--fff-frecency-db`, `--fff-history-db`. The databases default to your existing fff.nvim ones when present, otherwise `~/.pi/agent/fff/`.

### Agent-facing tools

- `ffgrep`. Content search. Accepts `path`, `exclude` (comma, space, or array; leading `!` optional), `caseSensitive`, `context`, and cursor pagination. Auto-detects regex, falls back to fuzzy on zero exact matches, rejects `.*`-style wildcard-only patterns up front.
- `fffind`. Path and filename search. Matches the whole repo-relative path, not just the filename. Frecency-aware. The weak-match detector flags scattered fuzzy noise before it floods the agent's context.

### Commands

- `/fff-mode [tools-and-ui | tools-only | ui-only | override]`. Show or switch the mode.
- `/fff-health`. Picker, frecency, and git integration status.
- `/fff-rescan`. Force a rescan.

Source: [`packages/pi-fff/`](./packages/pi-fff/).

</details>

The Pi extension swaps pi's native tools for FFF implementations and feeds the interactive editor's `@`-mention autocomplete from the frecency-ranked index.

<details id="neovim-plugin">
<summary>
<h2>fff.nvim</h2>
</summary>

Demo on the Linux kernel repo (100k files, 8GB):

https://github.com/user-attachments/assets/5d0e1ce9-642c-4c44-aa88-01b05bb86abb

### Installation

#### lazy.nvim

```lua
-- Package name changed from `fff.nvim` to `fff`. If you installed fff.nvim before, clean with `:Lazy clean`
{
  'dmtrKovalenko/fff',
  build = function()
    -- downloads a prebuilt binary or falls back to cargo build
    require("fff.download").download_or_build_binary()
  end,
  -- for nixos:
  -- build = "nix run .#release",
  opts = {
    debug = {
      enabled = true,
      show_scores = true,
    },
  },
  lazy = false, -- the plugin lazy-initialises itself
  keys = {
    { "ff", function() require('fff').find_files() end, desc = 'FFFind files' },
    { "fg", function() require('fff').live_grep() end, desc = 'LiFFFe grep' },
    { "fz",
      function() require('fff').live_grep({ grep = { modes = { 'fuzzy', 'plain' } } }) end,
      desc = 'Live fffuzy grep',
    },
    { "fw",
      function() require('fff').live_grep_under_cursor() end,
      mode = { 'n', 'x' },
      desc = 'Search current word / selection',
    },
  },
}
```

#### vim.pack

```lua
-- Package name changed from `fff.nvim` to `fff`. If you installed fff.nvim before, clean with `:packdel fff.nvim`
vim.pack.add({ 'https://github.com/dmtrKovalenko/fff' })

vim.api.nvim_create_autocmd('PackChanged', {
  callback = function(ev)
    local name, kind = ev.data.spec.name, ev.data.kind
    if name == 'fff' and (kind == 'install' or kind == 'update') then
      if not ev.data.active then vim.cmd.packadd('fff') end
      require('fff.download').download_or_build_binary()
    end
  end,
})

vim.g.fff = {
  lazy_sync = true,
  debug = { enabled = true, show_scores = true },
}

vim.keymap.set('n', 'ff', function() require('fff').find_files() end, { desc = 'FFFind files' })
```

### Public API

```lua
require('fff').find_files()                        -- find files in current repo
require('fff').live_grep()                         -- live content grep
require('fff').live_grep_under_cursor()            -- grep <cword> in normal, selection in visual
require('fff').scan_files()                        -- force rescan
require('fff').refresh_git_status()                -- refresh git status
require('fff').find_files_in_dir(path)             -- find in a specific dir
require('fff').change_indexing_directory(new_path) -- change root

-- Programmatic search (no UI). Useful for plugin integrations.
require('fff').file_search(query, opts)            -- fuzzy search files / dirs / mixed
require('fff').content_search(query, opts)         -- programmatic grep
```

#### `file_search(query, opts)`

Returns a structured result `{ items, scores, total_matched, total_files?, total_dirs?, location? }`. Each item has a `type` field (`"file"` or `"directory"`) and `name` / `relative_path`. File items also expose `size`, `modified`, `git_status`, `is_binary`, and frecency scores.

```lua
local r = require('fff').file_search('button', {
  mode             = 'mixed',  -- 'files' (default) | 'directories' | 'mixed'
  max_results      = 50,
  page             = 0,        -- 0-based pagination
  current_file     = nil,      -- path to deprioritize for distance scoring
  max_threads      = 4,
  cwd              = nil,      -- switch indexed root if different (see below)
  wait_for_index_ms = nil,     -- override the default scan wait timeout
})
for _, item in ipairs(r.items) do
  print(item.type, item.relative_path)
end
```

#### `content_search(query, opts)`

Returns a `GrepResult` `{ items, total_matched, total_files_searched, total_files, filtered_file_count, next_file_offset, regex_fallback_error? }`. Each match item has `relative_path`, `name`, `line_number`, `col`, `line_content`, `match_ranges`, plus the same file metadata as `file_search`.

```lua
local r = require('fff').content_search('TODO', {
  mode                  = 'plain',  -- 'plain' (default) | 'regex' | 'fuzzy'
  max_file_size         = 10 * 1024 * 1024,
  max_matches_per_file  = 100,
  smart_case            = true,
  page_size             = 50,
  file_offset           = 0,
  time_budget_ms        = 0,
  trim_whitespace       = false,
  cwd                   = nil,      -- switch indexed root if different
  wait_for_index_ms     = nil,      -- override the default scan wait timeout
})
for _, m in ipairs(r.items) do
  print(string.format('%s:%d %s', m.relative_path, m.line_number, m.line_content))
end
```

Both functions accept the same constraint syntax as the UI pickers (e.g. `git:modified`, `*.rs`, `!test/`, glob patterns).

#### `cwd` and indexing

Both `file_search` and `content_search` honour an optional `cwd` field. The first call to either function lazily initialises the picker at `config.base_path` (your Neovim cwd by default).

- If `cwd` matches the currently indexed root, the call returns immediately against the existing index.
- If `cwd` differs, the picker is re-indexed at the new root and the call **blocks** (default up to 10 s) until the new picker is installed and its initial scan completes — so callers always get results from the right tree.
- If the index is still warming up after a `change_indexing_directory`, you can pass `wait_for_index_ms = N` to block for up to `N` ms regardless of whether `cwd` triggered the swap. Pass `0` to skip waiting entirely (useful for fire-and-forget calls where partial results are acceptable).
- Invalid or non-existent `cwd` paths return an empty result and emit an error via `vim.notify`.

### Autocmds

The picker fires `User` autocmds when it opens and closes. `FFFOpen` runs with the prompt window focused, `FFFClose` runs after every picker window is gone — so hide global UI, not window-local options of the picker itself:

```lua
vim.api.nvim_create_autocmd('User', {
  pattern = { 'FFFOpen', 'FFFClose' },
  callback = function(ev) vim.o.showtabline = ev.match == 'FFFOpen' and 0 or 2 end,
})
```

### Commands

- `:FFFScan`. Rescan files.
- `:FFFRefreshGit`. Refresh git status.
- `:FFFClearCache [all|frecency|files]`. Clear caches.
- `:FFFHealth`. Health check.
- `:FFFDebug [on|off|toggle]`. Toggle the scoring display.
- `:FFFOpenLog`. Open `~/.local/state/nvim/log/fff.log`.

### Configuration

Defaults are sensible. Override only what you care about.

```lua
require('fff').setup({
  base_path = vim.fn.getcwd(),
  prompt = '🪿 ',
  title = 'FFFiles',
  max_results = 100,
  max_threads = 4,
  lazy_sync = true,
  prompt_vim_mode = false,
  wrap_around = false, -- true to wrap the cursor around when moving past the first/last item
  follow_symlinks = false,
  -- Allow indexing the user's $HOME directory. Enabled by default.
  -- Disable if you strictly sure you don't want this, as it makes whole fff error hard
  enable_home_dir_scanning = true,
  -- Allow indexing a filesystem root (e.g. `/`, `C:\`). Disabled by default
  enable_fs_root_scanning = false,
  layout = {
    height = 0.8,
    width = 0.8,
    prompt_position = 'bottom',   -- or 'top'
    preview_position = 'right',   -- 'left' | 'right' | 'top' | 'bottom'
    preview_size = 0.5,
    -- Border style for the picker windows. Leave unset (nil) to follow the
    -- global `vim.o.winborder`; set it to override fff's borders independently.
    border = nil, -- 'single' | 'double' | 'rounded' | 'solid' | 'shadow' | 'none'
    -- border = {
    --   { ' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ' },
    --   { ' ', ' ', ' ', ' ', ' ' },
    -- },

    flex = { size = 130, wrap = 'top' },
    min_list_height = 10, --  do not display anything except the list below this threshold
    show_scrollbar = true,
    path_shorten_strategy = 'middle', -- 'middle' | 'middle_number' | 'end' | 'start'
    -- 'center' | 'top' | 'bottom' | 'left' | 'right' | 'top_left' | 'top_right' | 'bottom_left' | 'bottom_right'
    anchor = 'center',
    show_path_first = false, -- true renders results as `path/to/file` instead of `file path/to`
  },
  -- find_files specific rendering
  file_picker = {
    current_file_label = '(current)', -- virtual text marking the buffer the picker was opened from
    fuzzy_query_highlighting = false, -- true to highlight fuzzy query matches, not just the literal query
  },
  preview = {
    enabled = true,
    max_size = 10 * 1024 * 1024,
    chunk_size = 8192,
    binary_file_threshold = 1024,
    imagemagick_info_format_str = '%m: %wx%h, %[colorspace], %q-bit',
    line_numbers = false,
    cursorlineopt = 'both',
    wrap_lines = false,
    filetypes = {
      svg = { wrap_lines = true },
      markdown = { wrap_lines = true },
      text = { wrap_lines = true },
    },
  },
  keymaps = {
    close = '<Esc>',
    select = '<CR>',
    select_split = '<C-s>',
    select_vsplit = '<C-v>',
    select_tab = '<C-t>',
    move_up = { '<Up>', '<C-p>' },
    move_down = { '<Down>', '<C-n>' },
    preview_scroll_up = '<C-u>',
    preview_scroll_down = '<C-d>',
    toggle_debug = '<F2>',
    cycle_grep_modes = '<S-Tab>',
    insert_newline_escape = '<C-CR>',
    -- grep mode only: jump cursor to first match of next/prev file group
    grep_jump_to_next_file = { '<C-A-n>', '<A-Down>' },
    grep_jump_to_prev_file = { '<C-A-p>', '<A-Up>' },
    cycle_previous_query = '<C-Up>',
    cycle_forward_query = '<C-Down>',
    -- unbound by default, wipes the whole input line
    -- clear_query = '<C-u>', -- overrides preview_scroll_up in insert mode
    toggle_select = '<Tab>',
    send_to_quickfix = '<C-q>',
    focus_list = '<leader>l',
    focus_preview = '<leader>p',
  },
  -- extra keymaps for the picker input, keyed by mode, applied over the built-ins
  mappings = {
    -- i = { ['<A-BS>'] = function() vim.api.nvim_input('<C-w>') end },
  },
  frecency = {
    enabled = true,
    db_path = vim.fn.stdpath('cache') .. '/fff_nvim',
  },
  history = {
    enabled = true,
    db_path = vim.fn.stdpath('data') .. '/fff_queries',
    min_combo_count = 3,
    combo_boost_score_multiplier = 100,
  },
  git = {
    status_text_color = false, -- true to color filenames by git status
  },
  select = {
    -- Return winid to open the chosen file in, or nil to open in the original window
    select_window = function(current_buf, action) --[[ default impl ]] end,
  },
  grep = {
    max_file_size = 10 * 1024 * 1024,
    max_matches_per_file = 100,
    smart_case = true,
    time_budget_ms = 150,
    modes = { 'plain', 'regex', 'fuzzy' },
    trim_whitespace = false,
    enable_filename_constraint = false, -- treat filename-like tokens (e.g. `score.rs`) in a grep query as a file-path filter scoping the search; off = searched as literal text
    location_format = ':%d:%d', -- printf format for line:col prefix in grep results, e.g. ':%d' for line-only
  },
  debug = {
    enabled = false, -- show the file info panel next to the preview
    show_scores = false, -- inline scores in the file list
    -- Per-section toggles for the file info panel. Accepts a boolean shorthand
    -- (`show_file_info = true|false`) to flip everything at once. The panel
    -- adapts to width: narrow renders sections vertically, wide renders them
    -- as a two-column grid. Disable a section to also shrink the panel.
    show_file_info = {
      file_info = true, -- size, type, git status, frecency
      score_breakdown = true, -- total + match type, bonuses, modifiers, penalty
      -- modified + accessed timestamps; pass a table to hide individual rows:
      --   timings = { modified = false, accessed = true }
      timings = true,
      full_path = true, -- relative path at the bottom (wraps if too long)
    },
  },
  logging = {
    enabled = true,
    -- logs will be written in a parent directory of this file path in files like
    -- `<stem>+<UTC-timestamp>+<pid>.<ext>`. Run :FFFOpenLog to open current one
    log_file = vim.fn.stdpath('log') .. '/fff.log',
    log_level = 'info',
    retain_runs = 20,
  },
})
```

### Live grep modes

`<S-Tab>` cycles between `plain`, `regex`, and `fuzzy`. The list is configurable via `grep.modes`, and single-mode setups hide the indicator entirely.

Per-call override:

```lua
require('fff').live_grep({ grep = { modes = { 'fuzzy', 'plain' } } })
require('fff').live_grep({ query = 'search term' }) -- pre-fill
```

### Constraints

Both find and grep accept these tokens to refine a query:

- `git:modified`. One of `modified`, `staged`, `deleted`, `renamed`, `untracked`, `ignored`.
- `test/`. Any deeply nested children of `test/`.
- `!something`, `!test/`, `!git:modified`. Exclusion. Text exclusions need at least 3 alphanumeric-containing characters, so operators like `!=` or `!==` work.
- `./**/*.{rs,lua}`. Any valid glob, powered by [zlob](https://github.com/dmtrKovalenko/zlob).

Grep-only:

- `*.md`, `*.{c,h}`. Extension filter.
- `src/main.rs`. Grep inside a single file.

Mix freely: `git:modified src/**/*.rs !src/**/mod.rs user controller`.

### Open in invoking window

By default fff.nvim will try to open a file in the most suitable window, so any non-file buffers are not affected. You can customize or disable this by providing:

```lua
require('fff').setup({
  select = {
    select_window = function(_current_buf, _action) return nil end,
  },
})
```

Caveat: the chosen file replaces the buffer in the invoking window even if it's a non-modifiable / special buftype. `winfixbuf` windows still fall back to `:split` to avoid `E1513`.

### Multi-select and quickfix

- `<Tab>`. Toggle selection (shows a thick `▊` in the signcolumn).
- `<C-q>`. Send selected files to the quickfix list and close the picker.

### Git status highlighting

Sign-column indicators are on by default. To color filename text by git status, set `git.status_text_color = true` and adjust the `hl.git_*` groups. See `:help fff.nvim` for the full list.

### Float colors

The picker maps its float content to `NormalFloat` (via `hl.normal`) and the border to `FloatBorder`. Default `FloatBorder` links to `NormalFloat`, so border and content share a background out of the box and the picker reads as a single popup. Override `hl.normal = 'Normal'` to make the picker blend with the editor instead.

For finer control, set `hl.winhl` to override the per-window `winhighlight`. It accepts either a single string applied to every picker window, or a table with optional `prompt`, `list`, `preview`, and `file_info` keys. Missing keys fall back to the default built from `hl.normal`, `hl.border`, and `hl.title`.

```lua
-- Apply the same winhighlight to all picker windows
hl = { winhl = 'Normal:NormalFloat,FloatBorder:FloatBorder,FloatTitle:Title' }

-- Or override specific windows only
hl = {
  winhl = {
    prompt  = 'Normal:Pmenu,FloatBorder:FloatBorder',
    list    = 'Normal:NormalFloat,FloatBorder:FloatBorder',
    preview = 'Normal:NormalFloat,FloatBorder:FloatBorder',
  },
}
```

### File info panel

Enable with `debug.enabled = true`. The panel sits above the preview and shows
file metadata, score breakdown, timestamps and the full absolute path. It
adapts to the panel width: at narrow widths sections stack vertically (B2),
at wide widths sections render as a two-column grid (H2). Each section can be
disabled individually via `debug.show_file_info`.

Customise the panel via `hl`:

| key                     | default           | used for                           |
| ----------------------- | ----------------- | ---------------------------------- |
| `file_info_section`     | `Title`           | section header label               |
| `file_info_separator`   | `FloatBorder`     | dashes that act as section borders |
| `file_info_label`       | `Comment`         | row labels (Size, Type, Git, ...)  |
| `file_info_value`       | `Normal` fg       | plain values                       |
| `file_info_value_dim`   | `NonText`         | dim values, separators inside rows |
| `file_info_size`        | `Number`          | file size value                    |
| `file_info_type`        | `Type`            | filetype value                     |
| `file_info_path`        | `Directory`       | full path                          |
| `file_info_total_score` | bold + `Number`   | total score (bold)                 |
| `file_info_match_type`  | bold + `Special`  | match type (bold)                  |
| `file_info_score_pos`   | `DiagnosticOk`    | positive score components          |
| `file_info_score_neg`   | `DiagnosticError` | negative score components          |

### File filtering

FFF honours `.gitignore`. For picker-only ignores that do not touch git, add a sibling `.ignore` file:

```gitignore
*.md
docs/archive/**/*.md
```

Run `:FFFScan` to force a rescan.

### Troubleshooting

- `:FFFHealth` verifies picker init, optional dependencies, and DB connectivity.
- `:FFFOpenLog` opens the current session's log file.
- Historical log files are stored near the main log file `<state>/log/fff+<UTC-timestamp>+<pid>.log` (up to 20 files)
- For a crash backtrace, run `lldb -- nvim` or `gdb -- nvim` and reproduce

</details>

The best file search picker for neovim. Period. Faster and more intuitive queries, frecency ranking, definition classification and much more.

<details id="node-sdk">
<summary>
<h2>Node & Bun SDK</h2>
</summary>

```bash
npm install @ff-labs/fff-node
# or
bun add @ff-labs/fff-node
```

```ts
import { FileFinder } from "@ff-labs/fff-node";

const finder = FileFinder.create({ basePath: process.cwd(), aiMode: true });
if (!finder.ok) throw new Error(finder.error);
await finder.value.waitForScan(10_000);

const files = finder.value.fileSearch("incognito profile", { pageSize: 20 });
const hits = finder.value.grep("GetOffTheRecordProfile", {
  mode: "plain",
  smartCase: true,
  beforeContext: 1,
  afterContext: 1,
  classifyDefinitions: true,
});

// Run extremely fast glob matching which is significantly (10-100 times) faster than Bun's and Node implementation
const rustFiles = finder.value.glob("**/*.rs", { pageSize: 100 });

finder.value.destroy();
```

Every method returns a `Result<T>` (`{ ok: true, value } | { ok: false, error }`). Full type reference: [`packages/fff-node/src/fff-api.ts`](./packages/fff-node/src/fff-api.ts).

</details>

TypeScript wrapper over the C library for nodejs and bun. Build custom agent tools, CLIs, or IDE integrations on top of FFF.

<details id="rust-crate">
<summary>
<h2>Rust crate</h2>
</summary>

### Add the dependency

FFF is written in Rust, so this is the lowest-overhead way to use it.

```toml
[dependencies]
fff-search = "0.6"
```

Full API documentation: [docs.rs/fff-search](https://docs.rs/fff-search/latest/fff_search/).

</details>

Native rust crate that is performing all the search. Stable and well documented.

<details id="c-library">
<summary>
<h2>C library</h2>
</summary>

### Build

```bash
# Builds only the C cdylib (fastest):
make build-c-lib

# or directly with cargo:
cargo build --release -p fff-c --features zlob
```

> The `zlob` feature (requires the [Zig](https://ziglang.org) toolchain) switches both
> glob matching **and** filesystem traversal to [zlob](https://github.com/dmtrKovalenko/zlob)'s
> native parallel walker. Without it, the default build uses the pure-Rust
> [`ignore`](https://crates.io/crates/ignore) (ripgrep) walker and `globset`.

The output is a `cdylib` (`libfff_c.so` / `libfff_c.dylib` / `fff_c.dll`). The header lives at [`crates/fff-c/include/fff.h`](./crates/fff-c/include/fff.h).

Prebuilt binaries for every version, including every commit on main, are on the [releases page](https://github.com/dmtrKovalenko/fff/releases). The same binaries also ship inside the `@ff-labs/fff-bin-*` npm packages.

### Install

```bash
# System-wide (needs sudo):
sudo make install

# User-local, no sudo:
make install PREFIX=$HOME/.local

# Staged install for packagers:
make install DESTDIR=/tmp/pkgroot PREFIX=/usr
```

Drops `libfff_c.{so,dylib,dll}` into `$(PREFIX)/lib` and the header into `$(PREFIX)/include/fff.h`. Remove with `make uninstall`, which honours the same `PREFIX` and `DESTDIR`.

Link against it after install:

```bash
cc my_app.c -lfff_c -o my_app
```

Ensure `$(PREFIX)/lib` is on your runtime library search path (`LD_LIBRARY_PATH` on Linux, `DYLD_LIBRARY_PATH` on macOS, or an entry in `/etc/ld.so.conf.d/`).

### Minimal example

```c
#include <fff.h>
#include <stdio.h>

int main(void) {
    FffResult *res = fff_create_instance(
        ".",        // base_path
        "",         // frecency_db_path (empty = default)
        "",         // history_db_path
        false,      // use_unsafe_no_lock
        true,       // enable_mmap_cache
        true,       // enable_content_indexing
        true,       // watch
        false       // ai_mode
    );
    if (!res->success) {
        fprintf(stderr, "init failed: %s\n", res->error);
        fff_free_result(res);
        return 1;
    }
    void *handle = res->handle;
    fff_free_result(res);

    // Search
    FffResult *search = fff_search(handle, "main.rs", "", 0, 0, 20, 100, 3);
    // ... read FffSearchResult from search->handle, then fff_free_search_result()

    fff_destroy(handle);
    return 0;
}
```

### Versioned options struct (preferred)

For instance creation use [`FffCreateOptions`](./crates/fff-c/include/fff.h) — a
versioned struct that evolves without ABI breaks. C99 designated
initializers keep call sites readable and zero-init unspecified fields:

```c
FffResult *res = fff_create_instance_with(&(FffCreateOptions){
    .version = FFF_CREATE_OPTIONS_VERSION,
    .base_path = "/path/to/repo",
    .ai_mode = true,
    .watch = true,
    .enable_fs_root_scanning = false,   // off by default
    .enable_home_dir_scanning = false,  // off by default
});
```

### Glob-only search

`fff_glob` filters indexed files by a single glob pattern, ranks by frecency,
paginates — bypasses the regular query parser entirely. Use this when you
already have a literal glob (`*.rs`, `**/*.test.ts`, `src/**`) and don't want
fuzzy matching layered on top.

```c
FffResult *res = fff_glob(handle, "**/*.rs", "", 0, 0, 100);
// FffSearchResult in res->handle, free with fff_free_search_result.
```

### Notes

- Every function returning `FffResult*` allocates with Rust's `Box`. Free with `fff_free_result`, do not use malloc's free
- Payloads (search results, grep results, scan progress) have their own dedicated free functions listed in the header.
- C strings returned in the `handle` field (e.g. from `fff_get_base_path`) are freed with `fff_free_string`.

Source: [`crates/fff-c/`](./crates/fff-c/).

</details>

Stable C ABI. Bind from C/C++, Zig, Go via cgo, Python via ctypes, or anything with C FFI.

<details id="python-bindings">
<summary>
<h2>Python bindings</h2>
</summary>

### Install

```bash
pip install fff-search
```

Or build and install from source:

```bash
cd packages/fff-python
uv sync --all-extras
uv run maturin develop --release
```

### Basic usage

```python
from fff import FileFinder

with FileFinder("/path/to/project", watch=False) as finder:
    finder.wait_for_scan_blocking(timeout_ms=5000)

    result = finder.search("main")
    for item, score in zip(result.items, result.scores):
        print(f"{item.relative_path}: {score.total}")

    hits = finder.grep("class Profile", mode="plain", before_context=1, after_context=1)
```

### Async usage

`wait_for_scan` is a coroutine that polls scan status and yields to the event
loop, so it never blocks other tasks. Use `wait_for_scan_blocking` from
synchronous code.

```python
import asyncio
from fff import FileFinder

async def main():
    with FileFinder("/path/to/project", watch=False) as finder:
        await finder.wait_for_scan(timeout_ms=5000)
        result = finder.search("main")
        print(result)

asyncio.run(main())
```

### What you get

- `search`, `glob`, `directory_search`, `mixed_search` — frecency-ranked fuzzy file/dir search
- `grep` / `multi_grep` — plain, regex, or fuzzy content search with context lines and cursor pagination
- `track_query` / `get_historical_query` — optional frecency and query-history databases
- `reindex`, `refresh_git_status`, `scan_progress`, `health_check` — lifecycle and diagnostics

Typed result objects (`FileItem`, `Score`, `GrepMatch`, …) with `py.typed`
stubs included. Ships as an `abi3` wheel compatible with Python 3.10+.

Source: [`packages/fff-python/`](./packages/fff-python/).

</details>

Native Python bindings built with PyO3. Use them for notebooks, agent scripts, or any Python tool that needs fast file search.

---

## What is FFF and why use it over ripgrep or fzf?

FFF is a file search library, not a CLI. Ripgrep and fzf are great tools, but they are command-line programs: every call forks a new process, re-reads `.gitignore`, re-stats directories, and rebuilds whatever state it needs in memory before it can answer. That is fine when you grep once from a shell. It is bad when an editor or an AI agent wants to run hundreds of searches per session.

FFF keeps the index and the file cache resident in one long-lived process and exposes the same Rust core through four thin layers: a native crate (`fff-search`), a C library (`libfff_c`), a Node/Bun SDK (`@ff-labs/fff-node`), and an MCP server. You call `FileFinder.create()` once, then every subsequent search hits warm memory. On a 500k-file Chromium checkout, that is the difference between 3-9 **SECONDS** per ripgrep spawn and sub-10 ms per FFF query.

Algorithm for fuzzy matching is much more comprehensive than fzf's algorithm. It is **typo-resistant** and we provide a query language with additional constraint parsing for prefiltering e.g. "\*.rs !test/ shcema" is a perfectly valid query for fff, but fzf wouldn't find anything even for a single typo in "shcema".

### Why a programmatic API matters

- No process spawn. Every call stays in-process and avoids the fork, exec, argv parsing, and stdout pipe setup that dominates short `rg` invocations.
- One FS walk, metadata collection, and parse of `.gitignore`. The ignore walker runs once at scan time and the result is reused for every search.
- Results come back as typed objects, not text you have to re-parse. The SDK gives you `{ relativePath, lineNumber, lineContent, gitStatus, totalFrecencyScore, isDefinition, ... }` directly.
- Cursor pagination that survives across calls. Ripgrep has no concept of "page 2 of these matches"; FFF does.
- A long-lived process opens up optimisations that a one-shot CLI cannot apply: warm caches, incremental re-indexing, cross-query frecency, and shared SIMD state.

### What the core actually does

- **Frecency-ranked fuzzy matching.** Every indexed file carries an access score and a modification score. Searches rank files you have opened recently and frequently above cold results. This is the same idea as VS Code's recently-opened list, but applied to every search result, not just a sidebar.
- **Typo-resistant matching for both paths and content.** Smith-Waterman fuzzy scoring is available on the grep path; path search uses SIMD-accelerated fuzzy matching (via the [`frizbee`](https://github.com/saghen/frizbee)-derived core) that survives dropped characters and reorderings.
- **Content grep with three modes.** Plain literal (SIMD memmem), regex (the Rust `regex` crate), and fuzzy (Smith-Waterman per line). Auto-detects which mode to use from the pattern, falls back to fuzzy when a plain search returns zero hits.
- **Multi-pattern OR search.** SIMD Aho-Corasick for "find any of these 20 identifiers at once", which is faster than regex alternation and a lot faster than 20 separate ripgrep runs.
- **Background file watcher.** The index updates as files change. You never pay for a rescan on the hot path.
- **Git status awareness.** Modified, staged, untracked, and ignored states are cached and returned with every result, so callers can sort or filter them without shelling out to git. The watcher talks to libgit2 directly instead of spawning the `git` CLI.
- **Definition classifier.** A byte-level scanner on the Rust side tags lines that start with `struct`, `fn`, `class`, `def`, `impl`, and friends.

### Performance choices that matter

- Efficient memory allocator and memory allocation strategy (see next paragraph). By default we use `mimaloc`
- Parallel multi thread search pipeline that is not contaganted by the orchistration logic
- SIMD first algorithms for everything. Efficinet & non-allocating sorting.
- Platform specific optimizations for FS ([getdents64](https://linux.die.net/man/2/getdents64), NTFS api on windows and others)
- Lightweight on the flight content index for realtime even typo resistant grep
- Memory mapped content cache. We store some of the files in virtual memory (the amount is limited)
- Single contiguous arena storage of string chunks. Significantly reduces the amount of memory to work with and dramatically increases CPU cache hits.

### Memory allocation

Yes, fff fundamentally requires more memory than calling a single child process. That is the primary source of the speedup. In practice, alongside one of the most popular file search pickers for Neovim, [fff ends up using less RAM than a burst of ripgrep invocations](https://x.com/neogoose_btw/status/2041606853155811442).

FFF also keeps a content index, around 360 bytes per indexed file, so roughly 36 MB for a 100k-file repo. Not every file is indexed - binaries, oversized files, and anything not eligible for grep are skipped. If even that footprint is too much, the index can be backed by a memory-mapped file instead of anonymous RAM.

### What this means in practice

If you are building an agent, an IDE extension, a pre-commit check, or any long-running tool that searches the same repository many times, calling FFF as a library is dramatically cheaper than shelling out to ripgrep. The tradeoff is real memory: FFF keeps the index in RAM and warms the content cache. On a 14k-file repo that costs about 26 MB resident. On a 500k-file repo like Chromium, expect a few hundred MB. In exchange, every single search is enriched with git status, frecency ranking, file metadata, timestamps of last access and edit and so on.

If you are running one grep from a terminal, `rg` is still the right tool. If you run dozens of them inside the same process, FFF will pay for itself starting from the second call. If you work on AI agent fff will finish preparation work before your AI will have a chance to call it.

### How it compares

- **ripgrep**: FFF uses the same underlying regex engine and more advanced plain text matching algorithms. Stores content index and file tree. Main wins on repeated-search workloads. Loses on "grep once from bash and exit."
- **fzf**: FFF's path search is fuzzy like fzf, but it is also frecency-aware and git-aware, and ships a more typo-tolerant algorithm. fzf is a pure match-and-filter tool; FFF ranks results by how often you actually open them.
- **Telescope / fzf-lua / snacks.picker**: FFF ships its own Neovim picker with the same ranking the MCP server and SDK use. The picker is optional; the core is the same.
- **Tantivy or other full-text search engines**: different class of tool. Tantivy indexes documents for query-time scoring at scale. FFF is scoped to one repository and optimised for sub-10 ms response. It does not persist an inverted index on disk.

---

## Repository layout

- `crates/fff-search`, `crates/fff-grep`, `crates/fff-query-parser` - Rust core.
- `crates/fff-c` - C FFI used by every language binding.
- `crates/fff-nvim` - Lua/mlua bindings for the Neovim plugin.
- `crates/fff-mcp` - MCP server binary.
- `packages/fff-node` - Node.js SDK (`@ff-labs/fff-node`).
- `packages/fff-bun` - Bun SDK (`@ff-labs/fff-bun`).
- `packages/pi-fff` - pi extension (`@ff-labs/pi-fff`).
- `lua/` - Neovim-side plugin code.

## Contributing

Bug reports and pull requests welcome. Agentic coding tools are welcome to be used, but human review is mandatory.

## License

[MIT](./LICENSE) & open source forever.

## FAQ

### What does FFF stand for?

There is intentionally no single canonical definition. Pick your favourite:

- **F**ast **F**ile **F**inder
- **F**uzzy **F**ile **F**inder
- will search **F**iles **F**or **F**ood

The brand hex is `#F87216`, not `#FFF`. Logo variants: [orange](./assets/logo-orange.png) · [dark](./assets/logo-dark.png) · [light](./assets/logo-light.png).
