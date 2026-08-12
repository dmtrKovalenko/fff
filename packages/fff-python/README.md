# fff-search

Python bindings for [FFF (Fast File Finder)](https://github.com/dmtrKovalenko/fff), built with [PyO3](https://pyo3.rs/) and [Maturin](https://www.maturin.rs/). Install with `pip install fff-search`; import as `fff`.

## Requirements

- Python >= 3.10
- Rust toolchain (to build the native extension)
- [uv](https://docs.astral.sh/uv/) (recommended)

## Development setup

```bash
cd packages/fff-python
uv sync --all-extras
uv run maturin develop --release
```

## Running tests

```bash
cd packages/fff-python
uv run pytest -v
```

## Standalone example

```bash
cd packages/fff-python
uv run python examples/basic.py .
```

## Basic usage

```python
from fff import FileFinder

with FileFinder("/path/to/project", watch=False) as finder:
    finder.wait_for_scan_blocking(timeout_ms=5000)
    print(f"Indexed under {finder.base_path}")

    result = finder.search("main")
    if result:
        print(f"Showing {len(result)} of {result.total_matched} matches")
    for item, score in zip(result.items, result.scores):
        print(f"{item.relative_path}: {score.total}")
```

### Async usage

`wait_for_scan` is a coroutine that polls the scan status and yields to the
event loop, so it never blocks other tasks. Use `wait_for_scan_blocking` from
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

### Watching files

Subscribe to filesystem changes with a glob, an exact path, or a directory
subtree (requires `watch=True`, the default). The callback receives normalized
batches of up to 128 events on a dedicated callback thread. Each path appears
at most once; avoid long-running work so later callbacks are not delayed.

```python
from fff import FileFinder

with FileFinder("/path/to/project") as finder:
    finder.wait_for_scan_blocking(timeout_ms=5000)

    def on_change(events):
        for e in events:
            print(e.kind, e.path)  # created | modified | removed | renamed | rescan

    # Globs are relative to the project root; wildcard-free patterns resolve
    # inside the indexed tree — an existing directory watches its whole
    # subtree, anything else is an exact file path.
    sub = finder.watch("src/**/*.py", on_change)
    ...
    sub.unsubscribe()  # non-blocking; the callback never runs after this

    # No pattern (None) watches the entire indexed tree
    with finder.watch(None, on_change):
        ...

    # Directory subtree with per-subscription excludes (parcel-watcher style)
    with finder.watch("src", on_change, ignore=["*.log", "src/vendor"]):
        ...
```

A `renamed` event carries both paths: `e.path` is the destination and
`e.from_path` the source. It replaces the removed/created pair, so a move is
one event. Moves in or out of the indexed tree stay a plain `created`/`removed`.

A `rescan` event means individual changes were lost (index overflow or an
ignore-file change) — re-check anything you care about.

## Building wheels

```bash
cd packages/fff-python
uv run maturin build --release
```

The produced wheel is `abi3` compatible with Python 3.10+.
