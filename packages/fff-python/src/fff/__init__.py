"""Python bindings for FFF (Fast File Finder)."""

from __future__ import annotations

import asyncio

from fff._fff_python import (
    DirItem,
    DirSearchResult,
    FFFException,
    FileItem,
    GrepMatch,
    GrepResult,
    GrepCursor,
    MatchRange,
    MixedDirItem,
    MixedFileItem,
    MixedSearchResult,
    ScanProgress,
    Score,
    SearchResult,
    WatchEvent,
    WatchSubscription,
)
from fff._fff_python import FileFinder as _FileFinder

_SCAN_POLL_INTERVAL = 0.05


class FileFinder(_FileFinder):
    """File finder with an async, event-loop-friendly scan wait.

    Inherits every method from the native finder; only adds the async
    ``wait_for_scan`` on top. Use ``wait_for_scan_blocking`` when a
    synchronous wait is acceptable.
    """

    async def wait_for_scan(self, timeout_ms: int = 5000) -> bool:
        """Wait for the initial scan without blocking the event loop.

        Polls ``is_scanning`` and yields to the loop between checks.
        Returns ``True`` if the scan completed, ``False`` on timeout.
        """
        loop = asyncio.get_event_loop()
        deadline = loop.time() + timeout_ms / 1000
        while self.is_scanning():
            if loop.time() >= deadline:
                return False
            await asyncio.sleep(_SCAN_POLL_INTERVAL)
        return True


__version__ = "0.11.0"

__all__ = [
    "FFFException",
    "FileFinder",
    "FileItem",
    "DirItem",
    "Score",
    "SearchResult",
    "DirSearchResult",
    "MixedFileItem",
    "MixedDirItem",
    "MixedSearchResult",
    "MatchRange",
    "GrepMatch",
    "GrepResult",
    "GrepCursor",
    "ScanProgress",
    "WatchEvent",
    "WatchSubscription",
    "__version__",
]
