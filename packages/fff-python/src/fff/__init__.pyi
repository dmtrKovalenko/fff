"""Type stubs for fff Python bindings."""

from __future__ import annotations

from collections.abc import Callable, Sequence
from os import PathLike
from typing import Any, Literal, TypeAlias

_PathInput: TypeAlias = str | PathLike[str]
_GrepMode: TypeAlias = Literal["plain", "regex", "fuzzy"]

__version__: str

class FFFException(Exception):
    """Base exception for fff errors."""

class Score:
    total: int
    base_score: int
    filename_bonus: int
    special_filename_bonus: int
    frecency_boost: int
    distance_penalty: int
    current_file_penalty: int
    combo_match_boost: int
    path_alignment_bonus: int
    exact_match: bool
    match_type: str
    def __repr__(self) -> str: ...

class FileItem:
    relative_path: str
    file_name: str
    git_status: str
    size: int
    modified: int
    access_frecency_score: int
    modification_frecency_score: int
    total_frecency_score: int
    is_binary: bool
    def __repr__(self) -> str: ...

class DirItem:
    relative_path: str
    dir_name: str
    max_access_frecency: int
    def __repr__(self) -> str: ...

class MixedFileItem:
    relative_path: str
    file_name: str
    git_status: str
    size: int
    modified: int
    access_frecency_score: int
    modification_frecency_score: int
    total_frecency_score: int
    is_binary: bool
    def __repr__(self) -> str: ...

class MixedDirItem:
    relative_path: str
    dir_name: str
    max_access_frecency: int
    def __repr__(self) -> str: ...

class MatchRange:
    start: int
    end: int
    def __repr__(self) -> str: ...

class GrepMatch:
    relative_path: str
    file_name: str
    git_status: str
    line_content: str
    match_ranges: list[MatchRange]
    context_before: list[str]
    context_after: list[str]
    size: int
    modified: int
    total_frecency_score: int
    access_frecency_score: int
    modification_frecency_score: int
    line_number: int
    byte_offset: int
    col: int
    fuzzy_score: int | None
    is_definition: bool
    is_binary: bool
    def __repr__(self) -> str: ...

class SearchResult:
    items: list[FileItem]
    scores: list[Score]
    total_matched: int
    total_files: int
    def __len__(self) -> int: ...
    def __bool__(self) -> bool: ...
    def __repr__(self) -> str: ...

class DirSearchResult:
    items: list[DirItem]
    scores: list[Score]
    total_matched: int
    total_dirs: int
    def __len__(self) -> int: ...
    def __bool__(self) -> bool: ...
    def __repr__(self) -> str: ...

class MixedSearchResult:
    items: list[MixedFileItem | MixedDirItem]
    scores: list[Score]
    total_matched: int
    total_files: int
    total_dirs: int
    def __len__(self) -> int: ...
    def __bool__(self) -> bool: ...
    def __repr__(self) -> str: ...

class GrepResult:
    items: list[GrepMatch]
    total_matched: int
    total_files_searched: int
    total_files: int
    filtered_file_count: int
    next_file_offset: int
    regex_fallback_error: str | None
    def __len__(self) -> int: ...
    def __bool__(self) -> bool: ...
    @property
    def has_more(self) -> bool: ...
    def next_cursor(self) -> GrepCursor | None: ...
    def __repr__(self) -> str: ...

class ScanProgress:
    scanned_files_count: int
    is_scanning: bool
    is_watcher_ready: bool
    is_warmup_complete: bool
    def __repr__(self) -> str: ...

class GrepCursor:
    offset: int
    def __init__(self, offset: int) -> None: ...
    def __repr__(self) -> str: ...

class WatchEvent:
    path: str
    kind: Literal["created", "modified", "removed", "rescan", "renamed"]
    from_path: str | None
    def __repr__(self) -> str: ...

class WatchSubscription:
    @property
    def id(self) -> int: ...
    @property
    def active(self) -> bool: ...
    def unsubscribe(self) -> bool: ...
    def __enter__(self) -> WatchSubscription: ...
    def __exit__(self, exc_type: Any, exc_value: Any, traceback: Any) -> None: ...
    def __repr__(self) -> str: ...

class FileFinder:
    def __init__(
        self,
        base_path: _PathInput,
        *,
        frecency_db_path: _PathInput | None = None,
        history_db_path: _PathInput | None = None,
        enable_mmap_cache: bool = True,
        enable_content_indexing: bool = True,
        watch: bool = True,
        ai_mode: bool = False,
        log_file_path: _PathInput | None = None,
        log_level: str | None = None,
        cache_budget_max_files: int = 0,
        cache_budget_max_bytes: int = 0,
        cache_budget_max_file_size: int = 0,
        enable_fs_root_scanning: bool = False,
        enable_home_dir_scanning: bool = False,
        follow_symlinks: bool = False,
    ) -> None: ...
    def __enter__(self) -> FileFinder: ...
    def __exit__(self, exc_type: Any, exc_value: Any, traceback: Any) -> None: ...
    def __repr__(self) -> str: ...
    @property
    def closed(self) -> bool: ...
    @property
    def base_path(self) -> str | None: ...
    @property
    def scan_progress(self) -> ScanProgress: ...
    def close(self) -> None: ...
    def search(
        self,
        query: str,
        *,
        current_file: str | None = None,
        max_threads: int = 0,
        page_index: int = 0,
        page_size: int = 0,
        combo_boost_score_multiplier: int = 0,
        min_combo_count: int = 0,
    ) -> SearchResult: ...
    def glob(
        self,
        pattern: str,
        *,
        current_file: str | None = None,
        max_threads: int = 0,
        page_index: int = 0,
        page_size: int = 0,
    ) -> SearchResult: ...
    def directory_search(
        self,
        query: str,
        *,
        current_file: str | None = None,
        max_threads: int = 0,
        page_index: int = 0,
        page_size: int = 0,
    ) -> DirSearchResult: ...
    def mixed_search(
        self,
        query: str,
        *,
        current_file: str | None = None,
        max_threads: int = 0,
        page_index: int = 0,
        page_size: int = 0,
        combo_boost_score_multiplier: int = 0,
        min_combo_count: int = 0,
    ) -> MixedSearchResult: ...
    def grep(
        self,
        query: str,
        *,
        mode: _GrepMode = "plain",
        max_file_size: int = 0,
        max_matches_per_file: int = 0,
        smart_case: bool = True,
        cursor: GrepCursor | None = None,
        page_limit: int = 0,
        time_budget_ms: int = 0,
        before_context: int = 0,
        after_context: int = 0,
        classify_definitions: bool = False,
    ) -> GrepResult: ...
    def multi_grep(
        self,
        patterns: Sequence[str],
        *,
        constraints: str | None = None,
        mode: _GrepMode = "plain",
        max_file_size: int = 0,
        max_matches_per_file: int = 0,
        smart_case: bool = True,
        cursor: GrepCursor | None = None,
        page_limit: int = 0,
        time_budget_ms: int = 0,
        before_context: int = 0,
        after_context: int = 0,
        classify_definitions: bool = False,
    ) -> GrepResult: ...
    def scan_files(self) -> None: ...
    def is_scanning(self) -> bool: ...
    async def wait_for_scan(self, timeout_ms: int = 5000) -> bool: ...
    def wait_for_scan_blocking(self, timeout_ms: int = 5000) -> bool: ...
    def watch(
        self,
        pattern: str | None,
        callback: Callable[[list[WatchEvent]], object],
        *,
        ignore: list[str] | None = None,
    ) -> WatchSubscription: ...
    def reindex(self, new_path: _PathInput) -> None: ...
    def refresh_git_status(self) -> int: ...
    def track_query(self, query: str, selected_file_path: _PathInput) -> bool: ...
    def get_historical_query(self, offset: int) -> str | None: ...
    def health_check(self, test_path: _PathInput | None = None) -> dict[str, Any]: ...
