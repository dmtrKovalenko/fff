PLENARY_DIR ?= ../plenary.nvim
MINI_DIR ?= ../mini.nvim

PREFIX ?= /usr/local
LIBDIR ?= $(PREFIX)/lib
INCLUDEDIR ?= $(PREFIX)/include

# Compile-time cfg that gates the watcher + git-status fuzz stress test.
STRESS_RUSTFLAGS := --cfg stress
FFF_STRESS_DEFAULT_SEED ?= 0xDEADBEEFCAFEBABE

SHELL := bash
# Order matters: `-c` must be last so bash treats the recipe as the script
# string rather than the literal `-o` / `pipefail` tokens.
.SHELLFLAGS := -o pipefail -euc

.PHONY: build build-c-lib install uninstall test test-rust test-rescan test-rescan-known-defects rescan-probe test-c-smoke test-c-api test-lua test-lua-snap test-version test-bun test-node prepare-bun prepare-bun-packaged prepare-node set-npm-version header test-stress test-stress-seeded test-stress-random test-stress-regressions test-stress-repos test-node-stress sync-js-api sync-js-api-check bump-homebrew-formula bump-install-mcp-sh test-bun-compile

all: format test lint

SYNC_API_SRC := packages/shared/fff-api.ts
SYNC_API_TARGETS := packages/fff-node/src/fff-api.ts packages/fff-bun/src/fff-api.ts
SYNC_API_BANNER := // ----------------------------------------------------------------------------\n// GENERATED FILE - DO NOT EDIT.\n// Copied from: ${SYNC_API_SRC}\n// Run make sync-js-api from the repo root to regenerate.\n// ----------------------------------------------------------------------------\n\n

sync-js-api:
	@for target in $(SYNC_API_TARGETS); do \
		printf '$(SYNC_API_BANNER)' > "$$target"; \
		cat $(SYNC_API_SRC) >> "$$target"; \
		echo "synced: $$target"; \
	done

sync-js-api-check:
	@status=0; \
	for target in $(SYNC_API_TARGETS); do \
		tmp=$$(mktemp); \
		printf '$(SYNC_API_BANNER)' > "$$tmp"; \
		cat $(SYNC_API_SRC) >> "$$tmp"; \
		if ! cmp -s "$$tmp" "$$target"; then \
			echo "out of date: $$target (run make sync-js-api)"; status=1; \
		fi; \
		rm -f "$$tmp"; \
	done; \
	exit $$status

build:
	cargo build --release --no-default-features --features zlob

# Only the crates the e2e suites load (nvim lua tests + C/bun/node FFI tests),
# skipping fff-python (pyo3) and fff-mcp (tokio/rmcp) which e2e never touches.
build-e2e:
	cargo build --release -p fff-nvim -p fff-c --no-default-features --features zlob

build-c-lib:
	cargo build --release -p fff-c --no-default-features --features zlob

header:
	cbindgen --config crates/fff-c/cbindgen.toml --crate fff-c --output crates/fff-c/include/fff.h

# Install the C library and header under $(PREFIX) (default /usr/local).
# Override PREFIX for user-local installs, e.g. `make install PREFIX=$$HOME/.local`.
# DESTDIR is honoured for packagers.
install: build-c-lib
	install -d $(DESTDIR)$(LIBDIR)
	install -d $(DESTDIR)$(INCLUDEDIR)
	install -m 0644 crates/fff-c/include/fff.h $(DESTDIR)$(INCLUDEDIR)/fff.h
	@if [ -f target/release/libfff_c.dylib ]; then \
		install -m 0755 target/release/libfff_c.dylib $(DESTDIR)$(LIBDIR)/libfff_c.dylib; \
		echo "Installed $(DESTDIR)$(LIBDIR)/libfff_c.dylib"; \
	fi
	@if [ -f target/release/libfff_c.so ]; then \
		install -m 0755 target/release/libfff_c.so $(DESTDIR)$(LIBDIR)/libfff_c.so; \
		echo "Installed $(DESTDIR)$(LIBDIR)/libfff_c.so"; \
	fi
	@if [ -f target/release/fff_c.dll ]; then \
		install -m 0755 target/release/fff_c.dll $(DESTDIR)$(LIBDIR)/fff_c.dll; \
		echo "Installed $(DESTDIR)$(LIBDIR)/fff_c.dll"; \
	fi
	@echo "Installed header $(DESTDIR)$(INCLUDEDIR)/fff.h"

uninstall:
	rm -f $(DESTDIR)$(LIBDIR)/libfff_c.dylib
	rm -f $(DESTDIR)$(LIBDIR)/libfff_c.so
	rm -f $(DESTDIR)$(LIBDIR)/fff_c.dll
	rm -f $(DESTDIR)$(INCLUDEDIR)/fff.h
	@echo "Removed fff-c from $(DESTDIR)$(PREFIX)"

test-setup:
	@if [ ! -d "$(PLENARY_DIR)" ]; then \
		echo "Cloning plenary.nvim..."; \
		git clone --depth 1 https://github.com/nvim-lua/plenary.nvim $(PLENARY_DIR); \
	fi
	@if [ ! -d "$(MINI_DIR)" ]; then \
		echo "Cloning mini.nvim..."; \
		git clone --depth 1 https://github.com/echasnovski/mini.nvim $(MINI_DIR); \
	fi

test-rust:
	cargo test --workspace --no-default-features --features zlob --exclude fff-nvim --exclude fff-python

# Watcher rescan harness: asserts that editing, build output, git activity and
# preview reads all stay on the incremental path instead of re-walking the tree.
test-rescan:
	cargo test -p fff-search --no-default-features --features zlob \
		--lib --test rescan_regression -- rescan

# Live probe for watcher rescan requests and their causes.
# Usage: make rescan-probe DIR=~/some/repo [SECONDS=120]
rescan-probe:
	cargo run --release -p fff-nvim --bin rescan_probe \
		--no-default-features --features zlob,rescan-stats -- \
		$(or $(DIR),.) $(if $(SECONDS),--seconds $(SECONDS),)

# The same harness, restricted to cases that currently fail on purpose. Each
# `#[ignore]` reason names the code that causes the unnecessary rescan.
test-rescan-known-defects:
	cargo test --no-fail-fast -p fff-search --no-default-features --features zlob \
		--lib --test rescan_regression -- --ignored --nocapture

CC ?= cc
CFLAGS ?= -O0 -g -Wall -Wextra -std=c99
TARGET_DIR ?= target/release
SMOKE_BIN := $(TARGET_DIR)/fff_c_smoke
SMOKE_SRC := crates/fff-c/tests/smoke.c
SMOKE_INCLUDE := crates/fff-c/include

test-c-smoke: build-e2e
	$(CC) $(CFLAGS) -I $(SMOKE_INCLUDE) -L $(TARGET_DIR) \
		-Wl,-rpath,@loader_path/../target/release \
		-Wl,-rpath,$$(pwd)/$(TARGET_DIR) \
		$(SMOKE_SRC) -lfff_c -o $(SMOKE_BIN)
	$(SMOKE_BIN) .

# Alias kept for the `external-tests.yml` workflow naming.
test-c-api: test-c-smoke

# neovim instance swallows internal crashes and doesn't rise the the error exiting silently
# so check the stdout in case the sigsegv coming out of fff was printed (actual regression).
# Output is streamed live via `tee`; pipefail (set above) propagates nvim's exit.
test-lua: test-setup build-e2e
	@logfile=$$(mktemp); \
	trap 'rm -f "$$logfile"' EXIT; \
	nvim --headless -u tests/minimal_init.lua \
		-c "PlenaryBustedDirectory tests/ {minimal_init = 'tests/minimal_init.lua'}" 2>&1 \
		| tee "$$logfile"; \
	if grep -qE "SIG(SEGV|ABRT|BUS|FPE|ILL)" "$$logfile"; then \
		echo ""; \
		echo "FAIL: native crash detected during lua tests"; \
		exit 1; \
	fi

test-lua-snap: test-setup build-e2e
	@logfile=$$(mktemp); \
	trap 'rm -f "$$logfile"' EXIT; \
	nvim --headless -u tests/minimal_init.lua \
		-c "lua local ok,err=pcall(require('mini.test').run_file,'tests/picker_ui_snap.lua'); if not ok then io.stderr:write('mini.test failed to load: '..tostring(err)..'\\n'); vim.cmd('cquit 2') end" 2>&1 \
		| tee "$$logfile"; \
	if grep -qE "SIG(SEGV|ABRT|BUS|FPE|ILL)" "$$logfile"; then \
		echo ""; \
		echo "FAIL: native crash detected during snapshot tests"; \
		exit 1; \
	fi

test-version: test-setup
	nvim --headless -u tests/minimal_init.lua \
		-c "PlenaryBustedFile tests/version_spec.lua" 2>&1

prepare-bun: build-e2e sync-js-api
	mkdir -p packages/fff-bun/bin
	cp target/release/libfff_c.dylib packages/fff-bun/bin/ 2>/dev/null || true; \
	cp target/release/libfff_c.so packages/fff-bun/bin/ 2>/dev/null || true; \
	cp target/release/fff_c.dll packages/fff-bun/bin/ 2>/dev/null || true

prepare-node: build-e2e sync-js-api
	mkdir -p packages/fff-node/bin
	cp target/release/libfff_c.dylib packages/fff-node/bin/ 2>/dev/null || true; \
	cp target/release/libfff_c.so packages/fff-node/bin/ 2>/dev/null || true; \
	cp target/release/fff_c.dll packages/fff-node/bin/ 2>/dev/null || true

test-bun: prepare-bun
	cd packages/fff-bun && bun test test/
	cd packages/pi-fff && bun test test/

# Same as prepare-bun but puts the compiled binary into the actual npm package location
prepare-bun-packaged: prepare-bun
	@machine=$$(uname -m); \
	case "$$machine" in \
	  x86_64|amd64) arch=x64 ;; \
	  aarch64|arm64) arch=arm64 ;; \
	  *) echo "unsupported arch: $$machine" >&2; exit 1 ;; \
	esac; \
	case "$$(uname -s)" in \
	  Darwin) lib=libfff_c.dylib; pkg=fff-bin-darwin-$$arch ;; \
	  Linux) lib=libfff_c.so; \
	    if ldd --version 2>&1 | grep -qi musl; then libc=musl; else libc=gnu; fi; \
	    pkg=fff-bin-linux-$$arch-$$libc ;; \
	  MINGW*|MSYS*|CYGWIN*|Windows_NT) lib=fff_c.dll; pkg=fff-bin-win32-$$arch ;; \
	  *) echo "unsupported OS: $$(uname -s)" >&2; exit 1 ;; \
	esac; \
	src=target/release/$$lib; \
	[ -f "$$src" ] || { echo "missing built library: $$src" >&2; exit 1; }; \
	dest=packages/fff-bun/node_modules/@ff-labs/$$pkg; \
	rm -rf "$$dest"; mkdir -p "$$dest"; \
	cp "$$src" "$$dest/$$lib"; \
	printf '{ "name": "@ff-labs/%s", "version": "0.0.0", "main": "%s" }\n' "$$pkg" "$$lib" > "$$dest/package.json"

# Compile a bun example to a standalone executable and run it. Verifies the
# native libfff_c is embedded + loaded from a `bun build --compile` binary.
# The staged bin package is removed before running so success proves the lib
# was embedded, not resolved from disk.
test-bun-compile: prepare-bun-packaged
	cd packages/fff-bun && \
		if [ "$$(uname -s)" = "Linux" ]; then \
		  if ldd --version 2>&1 | grep -qi musl; then DEFINE='--define FFF_LIBC="musl"'; \
		  else DEFINE='--define FFF_LIBC="gnu"'; fi; \
		else DEFINE=""; fi; \
		bun build --compile $$DEFINE ./examples/glob-bench.ts --outfile ./glob-bench-bin && \
		EXE=./glob-bench-bin; [ -f "$$EXE.exe" ] && EXE="$$EXE.exe"; \
		rm -rf bin node_modules/@ff-labs; \
		"$$EXE" . '**/*.ts' 1 | tee /tmp/fff-compile-e2e.log && \
		grep -q 'fff.glob' /tmp/fff-compile-e2e.log
	rm -f packages/fff-bun/glob-bench-bin packages/fff-bun/glob-bench-bin.exe

test-node: prepare-node
	cd packages/fff-node && npm run build && node test/e2e.mjs && node test/watch.mjs

test-js: test-bun test-node

# Bug pinning stress test script over fff-node for issue #515
# Just keep it untouched because it's good enough + some stress for SDK
FFF_STRESS_ITERS ?= 50
test-node-stress: prepare-node
	cd packages/fff-node && npm run build && \
		FFF_STRESS_ITERS=$(FFF_STRESS_ITERS) node test/stress-515.mjs

test: test-rust test-lua test-lua-snap test-version test-bun test-node test-node-stress

test-stress-seeded:
	FFF_STRESS_SEED="$${FFF_STRESS_SEED:-$(FFF_STRESS_DEFAULT_SEED)}" \
	RUSTFLAGS="$(STRESS_RUSTFLAGS)" \
	cargo test --release \
		-p fff-search \
		--test fuzz_git_watcher_stress \
		--no-default-features --features zlob \
		-- --nocapture stress_seeded

test-stress-random:
	RUSTFLAGS="$(STRESS_RUSTFLAGS)" \
	cargo test --release \
		-p fff-search \
		--test fuzz_git_watcher_stress \
		--no-default-features --features zlob \
		-- --nocapture stress_random

test-stress-regressions:
	RUSTFLAGS="$(STRESS_RUSTFLAGS)" \
	cargo test --release \
		-p fff-search \
		--test fuzz_git_watcher_stress \
		--no-default-features --features zlob \
		-- --nocapture stress_regression stress_merge_conflict_convergence

test-stress-repos:
	RUSTFLAGS="$(STRESS_RUSTFLAGS)" \
	cargo test --release \
		-p fff-search \
		--test fuzz_real_repos \
		--no-default-features --features zlob \
		-- --nocapture

test-stress: test-stress-seeded test-stress-random test-stress-regressions test-stress-repos

# Update version in a package.json, including optionalDependencies.
# Usage: make set-npm-version PKG=packages/fff-bun VERSION=1.0.0-nightly.abc1234
set-npm-version:
	@test -n "$(PKG)" || (echo "PKG is required" && exit 1)
	@test -n "$(VERSION)" || (echo "VERSION is required" && exit 1)
	node scripts/set-npm-version.mjs "$(PKG)" "$(VERSION)"
	@echo "Set $(PKG) to $(VERSION)"

format-rust:
	cargo fmt --all
format-lua:
	stylua .
format-ts:
	cd packages && bun format

format: format-rust format-lua format-ts

lint-rust:
	cargo clippy --workspace --no-default-features --features zlob -- -D warnings
# Prefer luacheck from PATH (what CI uses). The luarocks shim is a fallback:
# it hardcodes the lua binary it was generated against and breaks whenever
# the interpreter is upgraded.
LUACHECK ?= $(shell command -v luacheck 2>/dev/null || echo ~/.luarocks/bin/luacheck)

lint-lua:
	$(LUACHECK) .
lint-ts:
	cd packages && bun lint

lint: lint-rust lint-lua lint-ts

check: format lint

FFF_RELEASE_REPO ?= dmtrKovalenko/fff
FFF_FORMULA_PATH ?= Formula/fff-mcp.rb
FFF_INSTALL_SCRIPT_PATH ?= install-mcp.sh

# Read the sha256 for $1 (filename, no .sha256 suffix). Reads from
# BINARIES_DIR/$1.sha256 when set; otherwise curls the GitHub release.
define fff_fetch_sha
	if [ -n "$$BINARIES_DIR" ]; then \
		awk '{print $$1}' "$$BINARIES_DIR/$$1.sha256" \
			|| { echo "Missing checksum file: $$BINARIES_DIR/$$1.sha256" >&2; exit 1; }; \
	else \
		curl -fsSL "https://github.com/$(FFF_RELEASE_REPO)/releases/download/v$(VERSION)/$$1.sha256" \
			| awk '{print $$1}'; \
	fi
endef

bump-homebrew-formula:
	@test -n "$(VERSION)" || (echo "VERSION is required. Usage: make bump-homebrew-formula VERSION=0.9.1 [BINARIES_DIR=./binaries]" && exit 1)
	@export BINARIES_DIR="$(BINARIES_DIR)"; \
	fetch_sha() { $(fff_fetch_sha); }; \
	sha_darwin_arm="$$(fetch_sha fff-mcp-aarch64-apple-darwin)"; \
	sha_darwin_intel="$$(fetch_sha fff-mcp-x86_64-apple-darwin)"; \
	sha_linux_arm="$$(fetch_sha fff-mcp-aarch64-unknown-linux-gnu)"; \
	sha_linux_intel="$$(fetch_sha fff-mcp-x86_64-unknown-linux-gnu)"; \
	sed -i.bak \
		-e 's/^  version "[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*"$$/  version "$(VERSION)"/' \
		-e '/fff-mcp-aarch64-apple-darwin"$$/{n;s/sha256 "[a-f0-9]*"/sha256 "'"$$sha_darwin_arm"'"/;}' \
		-e '/fff-mcp-x86_64-apple-darwin"$$/{n;s/sha256 "[a-f0-9]*"/sha256 "'"$$sha_darwin_intel"'"/;}' \
		-e '/fff-mcp-aarch64-unknown-linux-gnu"$$/{n;s/sha256 "[a-f0-9]*"/sha256 "'"$$sha_linux_arm"'"/;}' \
		-e '/fff-mcp-x86_64-unknown-linux-gnu"$$/{n;s/sha256 "[a-f0-9]*"/sha256 "'"$$sha_linux_intel"'"/;}' \
		"$(FFF_FORMULA_PATH)" && rm -f "$(FFF_FORMULA_PATH).bak"; \
	echo "Bumped $(FFF_FORMULA_PATH) to v$(VERSION)"

bump-install-mcp-sh:
	@test -n "$(VERSION)" || (echo "VERSION is required. Usage: make bump-install-mcp-sh VERSION=0.9.1 [BINARIES_DIR=./binaries]" && exit 1)
	@export BINARIES_DIR="$(BINARIES_DIR)"; \
	fetch_sha() { $(fff_fetch_sha); }; \
	sha_linux_intel="$$(fetch_sha fff-mcp-x86_64-unknown-linux-musl)"; \
	sha_linux_arm="$$(fetch_sha fff-mcp-aarch64-unknown-linux-musl)"; \
	sha_darwin_intel="$$(fetch_sha fff-mcp-x86_64-apple-darwin)"; \
	sha_darwin_arm="$$(fetch_sha fff-mcp-aarch64-apple-darwin)"; \
	sha_win_intel="$$(fetch_sha fff-mcp-x86_64-pc-windows-msvc.exe)"; \
	sha_win_arm="$$(fetch_sha fff-mcp-aarch64-pc-windows-msvc.exe)"; \
	sed -i.bak \
		-e 's|^PINNED_RELEASE_TAG=".*"|PINNED_RELEASE_TAG="v$(VERSION)"|' \
		-e 's|^SHA256_X86_64_UNKNOWN_LINUX_MUSL=".*"|SHA256_X86_64_UNKNOWN_LINUX_MUSL="'"$$sha_linux_intel"'"|' \
		-e 's|^SHA256_AARCH64_UNKNOWN_LINUX_MUSL=".*"|SHA256_AARCH64_UNKNOWN_LINUX_MUSL="'"$$sha_linux_arm"'"|' \
		-e 's|^SHA256_X86_64_APPLE_DARWIN=".*"|SHA256_X86_64_APPLE_DARWIN="'"$$sha_darwin_intel"'"|' \
		-e 's|^SHA256_AARCH64_APPLE_DARWIN=".*"|SHA256_AARCH64_APPLE_DARWIN="'"$$sha_darwin_arm"'"|' \
		-e 's|^SHA256_X86_64_PC_WINDOWS_MSVC=".*"|SHA256_X86_64_PC_WINDOWS_MSVC="'"$$sha_win_intel"'"|' \
		-e 's|^SHA256_AARCH64_PC_WINDOWS_MSVC=".*"|SHA256_AARCH64_PC_WINDOWS_MSVC="'"$$sha_win_arm"'"|' \
		"$(FFF_INSTALL_SCRIPT_PATH)" && rm -f "$(FFF_INSTALL_SCRIPT_PATH).bak"; \
	echo "Bumped $(FFF_INSTALL_SCRIPT_PATH) tag + checksums to v$(VERSION)"

CRATES_TO_PUBLISH= fff-grep fff-query-parser fff-search

publish-crates:
	@test -n "$(V)" || (echo "V is required. Usage: make publish-crates V=0.2.0" && exit 1)
	cargo install cargo-edit --force --locked
	cargo set-version $(V) || exit 1;
	@for crate in $(CRATES_TO_PUBLISH); do \
		cargo publish -p $$crate --allow-dirty $$(if [ -n "$$CI" ]; then echo "--no-verify"; fi) || exit 1; \
	done
