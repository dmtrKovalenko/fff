"""Tests for fff Python bindings."""

from __future__ import annotations

import importlib.metadata as metadata
import tempfile
import time
from pathlib import Path

import pytest

import fff
from fff import FFFException, FileFinder, GrepCursor, MixedDirItem, MixedFileItem


def rel(path: str) -> str:
    return path.replace("\\", "/")


def wait_for_content_index(finder: FileFinder, timeout: float = 10.0) -> None:
    # wait_for_scan only covers the file walk; the bigram index lands a bit later
    # and changes which files a grep page spans, so cursors must not straddle it.
    deadline = time.monotonic() + timeout
    while not finder.scan_progress.is_warmup_complete:
        assert time.monotonic() < deadline, "content index did not finish"
        time.sleep(0.01)


@pytest.fixture
def sample_dir() -> str:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "docs").mkdir()
        (root / "src" / "main.py").write_text(
            "from utils import helper\n\n"
            "def main():\n"
            "    value = helper()\n"
            "    return value\n"
        )
        (root / "src" / "utils.py").write_text(
            "def helper():\n"
            "    return 'alpha'\n"
        )
        (root / "src" / "profile.ts").write_text(
            "class Profile {}\n"
            "function renderProfile() { return new Profile(); }\n"
        )
        (root / "docs" / "guide.txt").write_text(
            "alpha line before\n"
            "needle target\n"
            "omega line after\n"
        )
        (root / "README.md").write_text("# Sample project\n")
        yield str(root)


def test_imports_and_package_version() -> None:
    assert fff.__version__ == metadata.version("fff-search")
    assert GrepCursor(12).offset == 12
    assert "GrepCursor" in fff.__all__


def test_pathlib_base_path(sample_dir: str) -> None:
    with FileFinder(Path(sample_dir), watch=False, enable_content_indexing=False) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)
        assert finder.closed is False
        assert finder.base_path is not None
        assert finder.scan_progress.scanned_files_count >= 1
        result = finder.search("main")
        assert result.total_matched >= 1


async def test_wait_for_scan_async(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=False) as finder:
        assert await finder.wait_for_scan(timeout_ms=5000) is True
        assert finder.is_scanning() is False
        result = finder.search("main")
        assert result.total_matched >= 1


async def test_wait_for_scan_async_does_not_block_loop(sample_dir: str) -> None:
    import asyncio

    ticks = 0

    async def ticker() -> None:
        nonlocal ticks
        while True:
            ticks += 1
            await asyncio.sleep(0.01)

    with FileFinder(sample_dir, watch=False, enable_content_indexing=True) as finder:
        background = asyncio.ensure_future(ticker())
        try:
            assert await finder.wait_for_scan(timeout_ms=5000) is True
        finally:
            background.cancel()
    # the loop kept running other tasks while we awaited the scan
    assert ticks > 0


async def test_wait_for_scan_blocking_and_async_agree(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=False) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000) is True
        # already finished, so the async wait resolves immediately to True
        assert await finder.wait_for_scan(timeout_ms=5000) is True


def test_keyword_only_options_and_cursor_constructor(sample_dir: str) -> None:
    with pytest.raises(TypeError):
        GrepCursor()

    with pytest.raises(TypeError):
        FileFinder(Path(sample_dir), None)

    with FileFinder(Path(sample_dir), watch=False, enable_content_indexing=True) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)

        with pytest.raises(TypeError):
            finder.search("main", None)
        with pytest.raises(TypeError):
            finder.glob("*.py", None)
        with pytest.raises(TypeError):
            finder.directory_search("src", None)
        with pytest.raises(TypeError):
            finder.mixed_search("src", None)
        with pytest.raises(TypeError):
            finder.grep("needle", "plain")
        with pytest.raises(TypeError):
            finder.multi_grep(["needle"], None)


def test_close_and_context_manager(sample_dir: str) -> None:
    finder = FileFinder(sample_dir, watch=False, enable_content_indexing=False)
    assert finder.wait_for_scan_blocking(timeout_ms=5000)
    assert finder.closed is False
    assert finder.base_path is not None
    finder.close()
    assert finder.closed is True

    with pytest.raises(FFFException, match="File picker not initialized"):
        finder.search("main")

    with FileFinder(sample_dir, watch=False, enable_content_indexing=False) as ctx_finder:
        assert ctx_finder.wait_for_scan_blocking(timeout_ms=5000)
        assert ctx_finder.closed is False

    assert ctx_finder.closed is True
    with pytest.raises(FFFException, match="File picker not initialized"):
        ctx_finder.search("main")

    fresh = FileFinder(sample_dir, watch=False, enable_content_indexing=False)
    assert fresh.wait_for_scan_blocking(timeout_ms=5000)
    fresh.close()
    assert fresh.closed is True
    with pytest.raises(FFFException, match="File picker not initialized"):
        fresh.search("main")


def test_reprs(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=False) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)
        assert repr(finder).startswith("FileFinder(")
        result = finder.search("main")
        assert repr(result).startswith("SearchResult(")
        assert repr(result.items[0]).startswith("FileItem(")
        assert repr(result.scores[0]).startswith("Score(")

        grep_result = finder.grep("needle")
        assert repr(grep_result).startswith("GrepResult(")
        assert repr(grep_result.items[0]).startswith("GrepMatch(")
        assert repr(grep_result.items[0].match_ranges[0]).startswith("MatchRange(")

        dir_result = finder.directory_search("src")
        assert repr(dir_result).startswith("DirSearchResult(")
        assert repr(dir_result.items[0]).startswith("DirItem(")

        mixed = finder.mixed_search("src", page_size=10)
        assert repr(mixed).startswith("MixedSearchResult(")

        cursor = GrepCursor(42)
        assert repr(cursor) == "GrepCursor(offset=42)"

        progress = finder.scan_progress
        assert repr(progress).startswith("ScanProgress(")


def test_file_search_scores_and_pagination(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=False) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)
        result = finder.search("main", page_size=1)
        assert result.total_matched >= 1
        assert len(result) == len(result.items) == 1
        assert bool(result) is True
        assert any("main.py" in rel(item.relative_path) for item in result.items)

        score = result.scores[0]
        assert isinstance(score.total, int)
        assert isinstance(score.exact_match, bool)
        assert isinstance(score.match_type, str)

        second_page = finder.search("", page_index=1, page_size=1)
        assert len(second_page) == len(second_page.items) == 1

        empty = finder.search("definitely_no_such_file_xyz")
        assert len(empty) == 0
        assert bool(empty) is False


def test_glob_variants(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=False) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)

        py_files = finder.glob("*.py")
        assert {Path(rel(item.relative_path)).name for item in py_files.items} == {
            "main.py",
            "utils.py",
        }

        src_files = finder.glob("src/*.py")
        assert src_files.total_matched == 2

        md_files = finder.glob("*.md")
        assert md_files.total_matched == 1
        assert rel(md_files.items[0].relative_path) == "README.md"


def test_directory_and_mixed_search(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=False) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)

        dirs = finder.directory_search("src")
        assert dirs.total_matched >= 1
        assert len(dirs) == len(dirs.items)
        assert bool(dirs) is True
        assert any(rel(item.relative_path).startswith("src") for item in dirs.items)

        mixed = finder.mixed_search("src", page_size=10)
        assert mixed.total_matched >= 3
        assert len(mixed) == len(mixed.items)
        assert bool(mixed) is True
        assert any(isinstance(item, MixedDirItem) for item in mixed.items)
        assert any(isinstance(item, MixedFileItem) for item in mixed.items)

        # max_access_frecency is the same field on both dir item types, so the
        # values must agree for a shared directory regardless of which search
        # produced them.
        dir_frecency = {
            rel(item.relative_path): item.max_access_frecency for item in dirs.items
        }
        for item in mixed.items:
            if isinstance(item, MixedDirItem):
                path = rel(item.relative_path)
                if path in dir_frecency:
                    assert item.max_access_frecency == dir_frecency[path]


def test_grep_plain_regex_fuzzy_and_context(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=True) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)

        plain = finder.grep("needle", before_context=1, after_context=1)
        assert plain.total_matched == 1
        assert len(plain) == len(plain.items) == 1
        assert bool(plain) is True
        match = plain.items[0]
        assert rel(match.relative_path) == "docs/guide.txt"
        assert match.line_content == "needle target"
        assert match.context_before == ["alpha line before"]
        assert match.context_after == ["omega line after"]
        assert [(r.start, r.end) for r in match.match_ranges] == [(0, 6)]

        regex = finder.grep(r"def \w+", mode="regex")
        assert regex.total_matched == 2
        assert {Path(rel(m.relative_path)).name for m in regex.items} == {
            "main.py",
            "utils.py",
        }

        fuzzy = finder.grep("df mn", mode="fuzzy")
        assert fuzzy.total_matched >= 1
        assert any(rel(m.relative_path) == "src/main.py" for m in fuzzy.items)
        assert any(m.fuzzy_score is not None for m in fuzzy.items)

        invalid = finder.grep("[", mode="regex")
        assert invalid.regex_fallback_error is not None


def test_grep_invalid_mode_raises(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=True) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)
        with pytest.raises(FFFException, match="invalid grep mode"):
            finder.grep("needle", mode="typo")
        with pytest.raises(FFFException, match="invalid grep mode"):
            finder.multi_grep(["needle"], mode="typo")


def test_grep_cursor_paginates_by_file(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=True) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)
        wait_for_content_index(finder)

        first = finder.grep("def", page_limit=1)
        assert first.total_matched >= 1
        assert first.next_file_offset > 0
        assert first.has_more is True
        assert first.next_cursor() is not None
        assert first.next_cursor().offset == first.next_file_offset

        second = finder.grep("def", cursor=GrepCursor(first.next_file_offset), page_limit=1)
        assert second.total_matched >= 1

        first_paths = {rel(m.relative_path) for m in first.items}
        second_paths = {rel(m.relative_path) for m in second.items}
        assert first_paths.isdisjoint(second_paths)

        exhausted = finder.grep("nonexistent_xyz")
        assert exhausted.has_more is False
        assert exhausted.next_cursor() is None
        assert len(exhausted) == 0
        assert bool(exhausted) is False


def test_multi_grep_and_error_handling(sample_dir: str) -> None:
    with FileFinder(sample_dir, watch=False, enable_content_indexing=True) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)

        result = finder.multi_grep(("def main", "def helper"))
        assert result.total_matched == 2
        assert {Path(rel(m.relative_path)).name for m in result.items} == {
            "main.py",
            "utils.py",
        }

        with pytest.raises(FFFException, match="patterns must not be empty"):
            finder.multi_grep([])


def test_query_history_persists(sample_dir: str, tmp_path: Path) -> None:
    history_db = tmp_path / "history"
    selected_file = Path(sample_dir) / "src" / "main.py"

    with FileFinder(
        sample_dir,
        history_db_path=history_db,
        watch=False,
        enable_content_indexing=False,
    ) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)
        assert finder.track_query("main", selected_file)
        assert finder.get_historical_query(0) == "main"

    with FileFinder(
        sample_dir,
        history_db_path=history_db,
        watch=False,
        enable_content_indexing=False,
    ) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)
        assert finder.get_historical_query(0) == "main"


def test_reindex_and_health_check(sample_dir: str, tmp_path: Path) -> None:
    other = tmp_path / "other-project"
    other.mkdir()
    (other / "other.py").write_text("def other():\n    return 42\n")

    frecency_db = tmp_path / "frecency"
    history_db = tmp_path / "history"
    with FileFinder(
        sample_dir,
        frecency_db_path=frecency_db,
        history_db_path=history_db,
        watch=False,
        enable_content_indexing=False,
    ) as finder:
        assert finder.wait_for_scan_blocking(timeout_ms=5000)

        health = finder.health_check(Path(sample_dir))
        assert health["file_picker"]["initialized"] is True
        assert health["frecency"]["initialized"] is True
        assert health["query_tracker"]["initialized"] is True

        # With no explicit path, the check inspects the indexed base path
        # rather than the process cwd.
        default_health = finder.health_check()
        assert default_health["file_picker"]["base_path"] is not None
        assert "error" not in default_health.get("git", {}) or default_health["git"][
            "error"
        ] != "could not determine current directory"

        finder.reindex(str(other))
        assert finder.wait_for_scan_blocking(timeout_ms=5000)
        result = finder.search("other")
        assert result.total_matched == 1
        assert rel(result.items[0].relative_path) == "other.py"

        finder.reindex(Path(other))
        assert finder.wait_for_scan_blocking(timeout_ms=5000)
        result2 = finder.search("other")
        assert result2.total_matched == 1
