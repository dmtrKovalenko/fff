--- Test fixtures for fff.nvim snapshot tests.
---
--- Builds a deterministic, throwaway directory tree on disk and tears it down
--- after each test. Also scopes frecency/history DBs to temp paths so picker
--- ordering is reproducible regardless of the developer's history.

local M = {}

--- Files used by every snapshot test. Sized to overflow the list page so
--- snapshots reflect realistic scrolling/clipping behaviour.
local FIXTURE_FILES = {
  ['README.md'] = '# Fixture\n',
  ['src/main.rs'] = 'fn main() {}\n',
  ['src/main_helper.rs'] = 'pub fn helper() {}\n',
  ['src/main_utils.rs'] = 'pub fn util() {}\n',
  ['src/main_runner.rs'] = 'pub fn run() {}\n',
  ['src/main_loop.rs'] = 'pub fn loop_() {}\n',
  ['src/lib.rs'] = 'pub fn it_works() -> i32 { 42 }\n',
  ['src/utils.rs'] = 'pub fn helper() {}\n',
  ['src/parser.rs'] = 'pub fn parse() {}\n',
  ['src/runner.rs'] = 'pub fn run() {}\n',
  ['src/state.rs'] = 'pub struct State {}\n',
  ['src/config.rs'] = 'pub struct Config {}\n',
  ['src/error.rs'] = 'pub enum Error {}\n',
  ['src/types.rs'] = 'pub type Id = u64;\n',
  ['src/store.rs'] = 'pub struct Store {}\n',
  ['src/api.rs'] = 'pub fn api() {}\n',
  ['src/cli.rs'] = 'pub fn cli() {}\n',
  ['src/components/button.tsx'] = 'export const Button = () => null\n',
  ['src/components/input.tsx'] = 'export const Input = () => null\n',
  ['src/components/dialog.tsx'] = 'export const Dialog = () => null\n',
  ['src/components/menu.tsx'] = 'export const Menu = () => null\n',
  ['src/components/list.tsx'] = 'export const List = () => null\n',
  ['src/components/table.tsx'] = 'export const Table = () => null\n',
  ['docs/intro.md'] = '# Intro\n',
  ['docs/guide.md'] = '# Guide\n',
  ['docs/reference.md'] = '# Reference\n',
  ['docs/changelog.md'] = '# Changelog\n',
  ['docs/contributing.md'] = '# Contributing\n',
  ['docs/license.md'] = '# License\n',
  ['tests/main_test.rs'] = '#[test] fn main_test() {}\n',
  ['tests/integration.rs'] = '#[test] fn it() {}\n',
  ['tests/regression.rs'] = '#[test] fn regress() {}\n',
}

--- @class fff.snapshot.Fixture
--- @field root string  absolute path to fixture root
--- @field frecency_db string
--- @field history_db string

--- Create a fresh fixture: deterministic file tree, scoped DBs, fff config
--- pointing at the temp DB paths.
--- @param extra_files table<string, string>|nil Additional files for one test set
--- @return fff.snapshot.Fixture
function M.create(extra_files)
  local raw_root = vim.fn.tempname() .. '_fff_snap_fixture'
  vim.fn.mkdir(raw_root, 'p')
  -- Resolve symlinks (on macOS /tmp → /private/tmp). Without this, picker_ui's
  -- change_indexing_directory canonicalises the path and silently reinitialises
  -- the FilePicker, which invalidates query-tracker entries we've trained
  -- against the original path.
  local root = vim.fn.resolve(raw_root)

  for rel, content in pairs(vim.tbl_extend('force', FIXTURE_FILES, extra_files or {})) do
    local path = root .. '/' .. rel
    vim.fn.mkdir(vim.fn.fnamemodify(path, ':h'), 'p')
    local f = assert(io.open(path, 'w'))
    f:write(content)
    f:close()
  end

  -- Initialise as a git repo so the picker exercises its git-status path the
  -- same way it would in real use.
  vim.fn.system({ 'git', '-C', root, 'init', '-q' })
  vim.fn.system({ 'git', '-C', root, '-c', 'user.email=t@t', '-c', 'user.name=t', 'add', '-A' })
  vim.fn.system({ 'git', '-C', root, '-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-q', '-m', 'init' })

  local frecency_db = vim.fn.tempname() .. '_fff_snap_frecency'
  local history_db = vim.fn.tempname() .. '_fff_snap_history'

  return { root = root, frecency_db = frecency_db, history_db = history_db }
end

--- Apply fff config that uses the fixture's scoped DBs and sane defaults for
--- snapshot tests (no debounce, fixed sizes).
--- @param fixture fff.snapshot.Fixture
function M.configure(fixture)
  ---@diagnostic disable-next-line: missing-fields
  vim.g.fff = {
    frecency = { enabled = true, db_path = fixture.frecency_db },
    ---@diagnostic disable-next-line: missing-fields
    history = { enabled = true, db_path = fixture.history_db },
  }
  -- Reload config so the new vim.g.fff is picked up.
  package.loaded['fff.conf'] = nil
end

--- Destroy fixture artefacts on disk and reset module state.
--- @param fixture fff.snapshot.Fixture
function M.cleanup(fixture)
  local rust_ok, fff_rust = pcall(require, 'fff.rust')
  if rust_ok then
    pcall(fff_rust.stop_background_monitor)
    pcall(fff_rust.cleanup_file_picker)
    pcall(fff_rust.destroy_frecency_db)
    pcall(fff_rust.destroy_query_db)
  end

  if fixture.root then vim.fn.delete(fixture.root, 'rf') end
  if fixture.frecency_db then vim.fn.delete(fixture.frecency_db, 'rf') end
  if fixture.history_db then vim.fn.delete(fixture.history_db, 'rf') end

  vim.g.fff = nil
  package.loaded['fff.conf'] = nil
end

--- Initialise the picker against the fixture and wait for the initial scan.
--- @param fixture fff.snapshot.Fixture
function M.init_picker(fixture)
  vim.cmd('cd ' .. vim.fn.fnameescape(fixture.root))
  local fff_rust = require('fff.rust')
  assert(fff_rust.init_db(fixture.frecency_db, fixture.history_db, true), 'init_db failed')
  assert(fff_rust.init_file_picker(fixture.root), 'init_file_picker failed')
  fff_rust.wait_for_initial_scan(10000)
end

return M
