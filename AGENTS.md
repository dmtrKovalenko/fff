# To Clankers

This repository contains **FFF.nvim (Fast File Finder)**, a high-performance file picker for Neovim inspired by blink.cmp's fuzzy matching technology. It's NOT a completion plugin, but rather a standalone file finder with advanced fuzzy search and frecency scoring. The project aims to be the drop-in replacement for telescope, fzf-lua, snacks.picker and similar plugins, focusing on speed, accuracy search and usability features.

## Development Commands

Always prefer Makefile commands listed to the cargo/bun/node if possible.

### Building

- `make build` - build everything

### Testing and Development Tools

This project does not have a traditional test suite. Testing is done through:

- Create e2e local test file for Neovim: Load any Lua test file with `nvim -l <test_file>`
- Write inline rust unit tests for any functionality that is standalone and scoped within a single function

### Code Quality

- `make lint` - Rust linting and code analysis
- `make format` - Format all code 
- `make test` - Run unit tests (limited coverage, primarily integration testing)

When doing code make sure to REDUCE SIZE OF COMMENTS. This is very important. Every comment should be concise 1-2 liner maximum 4 lines if describes really extensive and unnatural concept.

### Important coding rules

- Do not add doc comments to the private structs and functions.
- Do not make public structs if something can be private


## Style guide

- NO MODULES COMMENTS
- NO TOP FILE COMMENTS
- NO COMMENT LONGER THAN 2 LINES UNLESS ASKED EXPLICITLY
- UTILITY FUNCTIONS GO INTO THE END OF FILE

## Architecture

Everything that is performance critical happens in rust world, everything that is neovim specific happens in the lua code.

There are 3 main components:

- Rust binary with the global file picker state containing index of all files
- Background thread with the file system watcher that updates the index in real time
- Lua UI layer that renders the picker, handles user input, and calls the rust functions via FFI

There are 2 databases:

- Frecency database (LMDB) that tracks file access patterns for scoring
- Query history database used to track the user's previous search queries

### Key Files

- `lua/fff.lua` - Entry point, delegates to main.lua
- `lua/fff/main.lua` - Public API (find_files, search, change_directory)
- `lua/fff/core.lua` - Initialization, autocmds, global state management
- `lua/fff/picker_ui.lua` - UI rendering, layout calculation, keymaps
- `lua/fff/file_picker/preview.lua` - File preview with syntax highlighting
- `lua/fff/file_picker/image.lua` - Image preview (snacks.nvim integration)
- `lua/fff/conf.lua` - Default config
- `lua/fff/rust/init.lua` - Loads compiled Rust shared library

**Rust Side:**

- `lua/fff/rust/lib.rs` - FFI bindings, global state (FILE_PICKER, FRECENCY)
- `lua/fff/rust/file_picker.rs` - Core FilePicker struct, indexing, background watcher
- `lua/fff/rust/frecency.rs` - Frecency database (LMDB) and scoring
- `lua/fff/rust/query_tracker.rs` - Search query history tracking
- `lua/fff/rust/score.rs` - Fuzzy match scoring with frizbee integration
- `lua/fff/rust/git.rs` - Git status caching and repository detection
- `lua/fff/rust/background_watcher.rs` - File system watcher thread

### Scoring Algorithm

Located at the score.rs file

### Build System

- `Cargo.toml` - Rust dependencies and build configuration (package name: `fff_nvim`)
- `rust-toolchain.toml` - Specifies the stable Rust toolchain with required components
- `Cross.toml` - Cross-compilation settings using Zig for Linux targets
- **CI/CD Workflows**:
  - `.github/workflows/rust.yml` - Rust testing, formatting, and clippy checks
  - `.github/workflows/release.yaml` - Automated multi-platform builds
  - `.github/workflows/stylua.yaml` - Lua code formatting validation
  - `.github/workflows/nix.yml` - Nix build validation
- **Cross-compilation Support**: Uses `cross` tool with Zig backend for efficient cross-compilation

## Development Notes

### Working with Rust Code

- Prefer struct methods over functions
- If there is more than 2 impls in the file - create new file
- Smaller concise comments over giant comment blocks
- Do not add doc comments to the private functions/structs
- Be very careful around locking and better double check with the human if something is going to require potentially long lock on a mutex/rwlock

### Working with lua code

- Document the types of public functions in every module
- Use `vim.validate()` for validating user inputs in public functions
- Try to reuse as much of existing functions as possible
- When working on new features for the UI **IT IS EXTREMELY IMPORTANT** to keep the core functionality of navigating between files, selecting, and seeing the preview working as is. NEVER break anything from the core UI functionality, only add new features on top of the current UI.
- When making a large chunk of code make lua test that opens neovim at `~/dev/lightsource` and opens the picker to test the ui functionality across the actual code.
- When adding a new highlights or any new shortcuts and configurable UI options add them to the neovim config. AND IMPORTANT: update the README.md with the new configuration options.

### UI rendering

When working on the UI changeds IT IS EXTREMELY important for you to test it for both prompt_position="bottom" and prompt_position="top" as the rendering logic is different for both of them in both rust and lua world. When the prompt is positioed in the bottom everything should work the same way as the top but would be reversed in order. (though navigation is same for both)

## Top level API that can not introduce breaking changes under any circumstance

Top level rust, lua, C, and bun APIs can not be changed under any circumstance
