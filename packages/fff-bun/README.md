# fff - Fast File Finder

High-performance fuzzy file finder for Bun, powered by Rust. Extremely fast live file, content, and directory search with a typo-resistant algorithm. As well as regex, plain-text, multi-occurrence and typo-resistant content search.

Comes with built-in git status support, frecency access tracking, and a real-time file watcher, content indexing and many more! Designed for LLM agent tools that search through codebases or agentic RAG document search.

Faster than ripgrep & fzf on any workflow that runs more than once per process.

## Installation

```bash
bun add @ff-labs/fff-bun
```

The correct native binary for your platform is installed automatically via platform-specific packages (e.g. `@ff-labs/fff-bin-darwin-arm64`, `@ff-labs/fff-bin-linux-x64-gnu`)

### Supported Platforms

| Platform | Architecture          | Package                             |
| -------- | --------------------- | ----------------------------------- |
| macOS    | ARM64 (Apple Silicon) | `@ff-labs/fff-bin-darwin-arm64`     |
| macOS    | x64 (Intel)           | `@ff-labs/fff-bin-darwin-x64`       |
| Linux    | x64 (glibc)           | `@ff-labs/fff-bin-linux-x64-gnu`    |
| Linux    | ARM64 (glibc)         | `@ff-labs/fff-bin-linux-arm64-gnu`  |
| Linux    | x64 (musl)            | `@ff-labs/fff-bin-linux-x64-musl`   |
| Linux    | ARM64 (musl)          | `@ff-labs/fff-bin-linux-arm64-musl` |
| Windows  | x64                   | `@ff-labs/fff-bin-win32-x64`        |
| Windows  | ARM64                 | `@ff-labs/fff-bin-win32-arm64`      |

If the platform package isn't available, the postinstall script will attempt to download from GitHub releases as a fallback.

### Standalone executables (`bun build --compile`)

`@ff-labs/fff-bun` embeds the native library into single-file executables built
with `bun build --compile`. macOS and Windows work with no extra flags:

```bash
bun build --compile ./app.ts --outfile myapp
```

On **Linux** the C library's libc (glibc vs musl) can't be detected at build
time, so you must pass it as a build constant for the lib to embed:

```bash
bun build --compile --define FFF_LIBC='"gnu"'  ./app.ts --outfile myapp   # glibc
bun build --compile --define FFF_LIBC='"musl"' ./app.ts --outfile myapp   # musl / Alpine
```

Without the define on Linux the library is resolved at runtime instead of being
embedded, which works under `bun run` but not in a standalone binary.

## Quick Start

Each `FileFinder` instance owns an independent native index. Create one, wait
for the initial scan, then run as many searches as you like.

```typescript
import { FileFinder } from "@ff-labs/fff-bun";

// Create an instance bound to a directory
const created = FileFinder.create({ basePath: "/path/to/project" });
if (!created.ok) throw new Error(created.error);

const finder = created.value;

// Wait for the initial scan (non-blocking)
await finder.waitForScan(5000);

// 1. Fuzzy file search (typo resistant)
const files = finder.fileSearch("typescropt.ts", { pageSize: 10 });
if (files.ok) {
  for (const item of files.value.items) {
    console.log(item.relativePath, item.gitStatus);
  }
}

// 2. Glob filter — no fuzzy matching, 100% compatible with npm `glob`
const globbed = finder.glob("src/**/*.ts");
if (globbed.ok) console.log(`${globbed.value.totalMatched} TypeScript files`);

// 3. Content search (live grep) with pagination
const grep = finder.grep("TODO", { mode: "plain", pageSize: 20 });
if (grep.ok) {
  for (const m of grep.value.items) {
    console.log(`${m.relativePath}:${m.lineNumber}: ${m.lineContent}`);
  }
}

// 4. Directory search based on the query (typo resistant)
const dirs = finder.directorySearch("components");
if (dirs.ok) console.log(dirs.value.items.map((d) => d.relativePath));

// Free the resources when you don't need a file picker anymore
finder.destroy();
```


## Watching files

Subscribe to filesystem changes with a glob, an exact path, or a directory
subtree. Events reflect applied index changes and are delivered in batches of
up to 128, so callbacks stay cheap even under heavy churn.

```typescript
// Each path appears at most once per batch
const sub = finder.watch("src/**/*.ts", (events) => {
  for (const e of events) console.log(e.kind, e.path); // created | modified | removed | rescan
});

// No pattern: watch the entire indexed tree
const all = finder.watch((events) => {
  for (const e of events) console.log(e.kind, e.path);
});

// Unsubscribe: call the handle
if (sub.ok) sub.value();
```

Directory subtrees are watched by passing the directory itself (parcel-watcher
style), with per-subscription excludes:

```typescript
const dirSub = finder.watch(
  projectRoot,
  (events) => {
    for (const e of events) console.log(e.kind, e.path);
  },
  { ignore: ["node_modules", "*.log"] },
);
```

Notes:

- Globs are matched against the base-path-relative path; absolute globs must
  live under `basePath`. Wildcard-free patterns resolve inside the indexed
  tree: an existing directory watches its whole subtree, anything else is an
  exact file path.
- `ignore` entries exclude matches per subscription: wildcards are globs,
  everything else is a path prefix (a file or a whole subtree).
- Gitignored paths never produce events.
- A `rescan` event means changes were lost (index overflow, ignore-file
  change) — re-stat anything you care about.
- Unsubscribing takes effect synchronously on the JS thread: once it
  returns, the callback will not be invoked again.
- Watching requires the instance to be created with watching enabled
  (the default).

## Index layers

The path index is a stack: layer 0 is the base scan, on top of it sit up to
four append-only overlays. Layer 1 is opened automatically and collects
everything the watcher indexes during the session. Layers only hold paths;
file contents are never layered.

Add paths that the scanner can't see (generated files, remote checkouts,
virtual trees) and persist any layer — the base scan included — to a file:

```typescript
// Open a new layer and append to it. Duplicates of already indexed paths are skipped.
const layer = finder.createLayer("generated");
finder.addPaths(["build/out/a.js", { path: "build/out/b.js", size: 1024, modified: 1_700_000_000 }]);

// Persist it, or the base scan (layer 0), to disk
finder.saveLayer(layer.value, "/tmp/generated.fff");
finder.saveLayer(0, "/tmp/base.fff");

// Later: attach a saved layer, or restore the base without rescanning
finder.loadLayer("/tmp/generated.fff");
finder.loadBase("/tmp/base.fff"); // runs like a rescan; await finder.waitForScan()

// Build a layer file without an instance (e.g. in a worker)
FileFinder.buildLayerFile(paths, "/tmp/prebuilt.fff", "prebuilt");

for (const l of finder.listLayers().value ?? []) {
  console.log(l.id, l.label, l.liveFileCount, l.isOpen);
}
```

Notes:

- Batch `addPaths` calls: each call is one FFI crossing, so 100 paths per
  call is already within a few percent of a single 100k batch.
- Loading a layer that overlaps the index dedups per path (~350 ns each), so
  loading 100k already-indexed paths costs ~35 ms.
- Once the stack is full, the two newest overlays are compacted into one;
  explicit layers survive a rescan, only the base and session layer rebuild.
- Layer files are ~45 bytes per path; loading 100k paths takes ~10 ms vs a
  fresh scan of the same tree.

## API Reference

Verify the latest API in the local interface at [`./src/fff-api.ts`](./src/fff-api.ts). Every field and type is documented.

### Result Types

All methods return a `Result<T>` type for explicit error handling:


```typescript
type Result<T> = { ok: true; value: T } | { ok: false; error: string };

const result = finder.fileSearch("foo");

if (result.ok) {
  // result.value is SearchResult
} else {
  // result.error is string error message
}
```

This SDK calls a native compiled library for your platform at runtime. This is generally safe — fff is battle-tested and stable, and written in a memory-safe language — but there is a class of errors that can't be caught at the Bun/Node level. If you hit one, please report an issue!

## Building from Source

If prebuilt binaries aren't available for your platform:

```bash
# Clone the repository
git clone https://github.com/dmtrKovalenko/fff
cd fff

# Build the C library
cargo build --release -p fff-c

# The binary will be at target/release/libfff_c.{so,dylib,dll}
```

## CLI examples

```bash
# Download binary manually (fallback if npm package unavailable)
bunx fff download [tag]

# Show platform info and binary location
bunx fff info
```

## License

MIT
