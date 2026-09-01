--- Reproducer for SIGSEGV when :cd is issued during post-scan.
--- Run with: nvim --headless -l tests/test_cd_during_post_scan.lua
---
--- Uses ~/dev/chromium (large repo) so bigram build takes 5-10s,
--- then immediately reinits on the fff source dir.

-- Setup runtimepath so fff.rust can be found
local script_path = debug.getinfo(1, 'S').source:sub(2)
local plugin_dir = vim.fn.fnamemodify(script_path, ':h:h')
vim.opt.runtimepath:prepend(plugin_dir)

local fff_rust = require('fff.rust')

local big_repo = vim.fn.expand('~/dev/chromium')
if vim.fn.isdirectory(big_repo) ~= 1 then
  print('SKIP: ~/dev/chromium not found')
  os.exit(0)
end

print('Init picker on ' .. big_repo .. ' (500K+ files, slow bigram)...')
local ok = fff_rust.init_file_picker(big_repo)
assert(ok, 'init_file_picker failed')

-- Wait for scan to finish but NOT bigram (bigram is the slow part ~5-10s)
vim.wait(100, function() return false end)
fff_rust.wait_for_initial_scan(120000)

print('Scan done. Immediately reinit on fff source (simulates :cd)...')
fff_rust.restart_index_in_path(plugin_dir)

-- The reinit waits for chromium's post-scan to finish (Drop spin-wait),
-- then installs the new picker. Give it enough time.
local deadline = vim.uv.hrtime() + 30e9 -- 30s
while true do
  vim.wait(500, function() return false end)
  local searched, result = pcall(fff_rust.fuzzy_search_files, 'lib', 2, nil, 100, 3, 0, 10)
  if searched and result and #result.items > 0 then
    print('PASS: :cd during post-scan did not crash (' .. #result.items .. ' results found)')
    break
  end
  if vim.uv.hrtime() > deadline then error('TIMEOUT: new picker never became available') end
end

-- Cleanup
pcall(fff_rust.stop_background_monitor)
pcall(fff_rust.cleanup_file_picker)
