"""Tests for filesystem watch subscriptions."""

from __future__ import annotations

import sys
import tempfile
import threading
import time
from pathlib import Path

import pytest

from fff import FFFException, FileFinder, WatchEvent, WatchSubscription

WATCHER_SETTLE_SECONDS = 0.5
EVENT_TIMEOUT_SECONDS = 10.0
QUIET_PERIOD_SECONDS = 0.7


@pytest.fixture
def watch_dir() -> str:
    root = Path(tempfile.mkdtemp(prefix="fff-watch-test-")).resolve()
    (root / "docs").mkdir()
    (root / "docs" / "seed.txt").write_text("seed\n")
    (root / "main.py").write_text("print('hi')\n")
    yield str(root)


@pytest.fixture
def finder(watch_dir: str) -> FileFinder:
    with FileFinder(watch_dir, watch=True, enable_content_indexing=False) as f:
        assert f.wait_for_scan_blocking(timeout_ms=10000)
        wait_for_watcher(f)
        yield f


def wait_for_watcher(finder: FileFinder) -> None:
    deadline = time.monotonic() + EVENT_TIMEOUT_SECONDS
    while not finder.scan_progress.is_watcher_ready:
        assert time.monotonic() < deadline, "watcher never became ready"
        time.sleep(0.05)
    # the OS watcher needs a beat after readiness before events flow reliably
    time.sleep(WATCHER_SETTLE_SECONDS)


def wait_for_event(events: list[WatchEvent], lock: threading.Lock, suffix: str) -> WatchEvent:
    deadline = time.monotonic() + EVENT_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        with lock:
            for ev in events:
                if ev.path.endswith(suffix):
                    return ev
        time.sleep(0.05)
    with lock:
        raise AssertionError(f"no event for {suffix!r} within timeout, got: {events}")


def test_watch_delivers_events_and_unsubscribe_stops_them(
    finder: FileFinder, watch_dir: str
) -> None:
    events: list[WatchEvent] = []
    lock = threading.Lock()

    def on_events(batch: list[WatchEvent]) -> None:
        with lock:
            events.extend(batch)

    sub = finder.watch("**/*.txt", on_events)
    assert isinstance(sub, WatchSubscription)
    assert sub.active is True
    assert sub.id > 0
    assert repr(sub) == f"WatchSubscription(id={sub.id}, active=True)"

    target = Path(watch_dir) / "docs" / "note.txt"
    target.write_text("hello\n")

    ev = wait_for_event(events, lock, "note.txt")
    assert ev.path == str(target)
    # macOS FSEvents may report creations as modifications after debouncing
    assert ev.kind in ("created", "modified")
    assert repr(ev).startswith("WatchEvent(")

    # non-matching extensions never show up
    with lock:
        assert not any(ev.path.endswith(".py") for ev in events)

    assert sub.unsubscribe() is True
    assert sub.active is False
    assert sub.unsubscribe() is False  # idempotent

    with lock:
        seen = len(events)
    (Path(watch_dir) / "docs" / "after.txt").write_text("too late\n")
    time.sleep(QUIET_PERIOD_SECONDS)
    with lock:
        assert len(events) == seen


def test_watch_reports_removed_events(finder: FileFinder, watch_dir: str) -> None:
    events: list[WatchEvent] = []
    lock = threading.Lock()

    def on_events(batch: list[WatchEvent]) -> None:
        with lock:
            events.extend(batch)

    target = Path(watch_dir) / "docs" / "doomed.txt"
    with finder.watch("**/*.txt", on_events):
        target.write_text("short lived\n")
        wait_for_event(events, lock, "doomed.txt")
        with lock:
            events.clear()

        target.unlink()
        ev = wait_for_event(events, lock, "doomed.txt")
        assert ev.path == str(target)
        assert ev.kind == "removed"


def test_watch_reports_renames_with_both_paths(finder: FileFinder, watch_dir: str) -> None:
    events: list[WatchEvent] = []
    lock = threading.Lock()

    def on_events(batch: list[WatchEvent]) -> None:
        with lock:
            events.extend(batch)

    source = Path(watch_dir) / "docs" / "move-me.txt"
    target = Path(watch_dir) / "docs" / "moved.txt"
    with finder.watch("**/*.txt", on_events):
        source.write_text("move me\n")
        wait_for_event(events, lock, "move-me.txt")
        with lock:
            events.clear()

        source.rename(target)
        ev = wait_for_event(events, lock, "moved.txt")
        assert ev.kind == "renamed"
        assert ev.path == str(target)
        assert ev.from_path == str(source)
        # The move is one event, not a removal plus a creation.
        with lock:
            assert not [
                e for e in events if e.kind == "removed" and e.path == str(source)
            ]


def test_multiple_subscriptions_are_filtered_independently(
    finder: FileFinder, watch_dir: str
) -> None:
    txt_events: list[WatchEvent] = []
    py_events: list[WatchEvent] = []
    lock = threading.Lock()

    def on_txt(batch: list[WatchEvent]) -> None:
        with lock:
            txt_events.extend(batch)

    def on_py(batch: list[WatchEvent]) -> None:
        with lock:
            py_events.extend(batch)

    txt_sub = finder.watch("**/*.txt", on_txt)
    py_sub = finder.watch("**/*.py", on_py)
    assert txt_sub.id != py_sub.id

    try:
        (Path(watch_dir) / "both-a.txt").write_text("a\n")
        (Path(watch_dir) / "both-b.py").write_text("b = 1\n")

        wait_for_event(txt_events, lock, "both-a.txt")
        wait_for_event(py_events, lock, "both-b.py")

        # each subscription only sees paths matching its own pattern
        with lock:
            assert all(ev.path.endswith(".txt") for ev in txt_events), txt_events
            assert all(ev.path.endswith(".py") for ev in py_events), py_events

        # dropping one subscription must not affect the other
        assert txt_sub.unsubscribe() is True
        with lock:
            txt_seen = len(txt_events)

        (Path(watch_dir) / "late.txt").write_text("x\n")
        (Path(watch_dir) / "late.py").write_text("y = 2\n")
        wait_for_event(py_events, lock, "late.py")
        with lock:
            assert len(txt_events) == txt_seen
    finally:
        txt_sub.unsubscribe()
        py_sub.unsubscribe()


def test_watch_context_manager_unsubscribes(finder: FileFinder, watch_dir: str) -> None:
    events: list[WatchEvent] = []
    lock = threading.Lock()

    def on_events(batch: list[WatchEvent]) -> None:
        with lock:
            events.extend(batch)

    with finder.watch("**/*.txt", on_events) as sub:
        assert sub.active is True
        (Path(watch_dir) / "inside.txt").write_text("x\n")
        wait_for_event(events, lock, "inside.txt")

    assert sub.active is False
    with lock:
        seen = len(events)
    (Path(watch_dir) / "outside.txt").write_text("y\n")
    time.sleep(QUIET_PERIOD_SECONDS)
    with lock:
        assert len(events) == seen


def test_watch_callback_exception_does_not_crash(finder: FileFinder, watch_dir: str) -> None:
    unraisable: list[object] = []
    invoked = threading.Event()
    old_hook = sys.unraisablehook
    sys.unraisablehook = lambda args: (unraisable.append(args), invoked.set())

    def on_events(_batch: list[WatchEvent]) -> None:
        raise RuntimeError("boom from callback")

    try:
        with finder.watch("**/*.txt", on_events):
            (Path(watch_dir) / "explode.txt").write_text("x\n")
            assert invoked.wait(EVENT_TIMEOUT_SECONDS), "unraisable hook never fired"
    finally:
        sys.unraisablehook = old_hook

    # process survived; the finder still works after the callback raised
    assert finder.wait_for_scan_blocking(timeout_ms=5000)
    assert finder.search("main").total_matched >= 1


def test_watch_validates_inputs(finder: FileFinder, watch_dir: str) -> None:
    with pytest.raises(TypeError, match="callback must be callable"):
        finder.watch("**/*.txt", "not a callable")

    with pytest.raises(FFFException):
        finder.watch("/somewhere/else/**/*.txt", lambda batch: None)


def test_watch_without_pattern_watches_whole_tree(
    finder: FileFinder, watch_dir: str
) -> None:
    """`pattern=None` (and "") subscribes to the entire indexed tree."""
    events: list[WatchEvent] = []
    lock = threading.Lock()

    def on_events(batch: list[WatchEvent]) -> None:
        with lock:
            events.extend(batch)

    with finder.watch(None, on_events):
        (Path(watch_dir) / "anywhere.txt").write_text("x\n")
        (Path(watch_dir) / "other.rs").write_text("y\n")
        wait_for_event(events, lock, "anywhere.txt")
        wait_for_event(events, lock, "other.rs")


def test_watch_requires_open_finder(watch_dir: str) -> None:
    finder = FileFinder(watch_dir, watch=True, enable_content_indexing=False)
    assert finder.wait_for_scan_blocking(timeout_ms=10000)
    finder.close()
    with pytest.raises(FFFException):
        finder.watch("**/*.txt", lambda batch: None)


def test_watch_directory_with_ignore(finder: FileFinder, watch_dir: str) -> None:
    """A wildcard-free directory pattern subscribes to the whole subtree;
    `ignore` entries filter matches out (parcel-watcher style)."""
    events: list[WatchEvent] = []
    lock = threading.Lock()

    def on_events(batch: list[WatchEvent]) -> None:
        with lock:
            events.extend(batch)

    with finder.watch(watch_dir, on_events, ignore=["*.skiplog"]):
        (Path(watch_dir) / "subtree.txt").write_text("x\n")
        (Path(watch_dir) / "noise.skiplog").write_text("y\n")
        wait_for_event(events, lock, "subtree.txt")

    with lock:
        assert not any(e.path.endswith("noise.skiplog") for e in events), events


def test_unsubscribe_is_nonblocking_and_final(finder: FileFinder, watch_dir: str) -> None:
    """unsubscribe() must not block on an in-flight callback (it may finish
    concurrently), and no NEW callback invocation starts after it returns."""
    in_callback = threading.Event()
    calls: list[float] = []
    lock = threading.Lock()

    def slow_callback(_batch: list[WatchEvent]) -> None:
        with lock:
            calls.append(time.monotonic())
        in_callback.set()
        time.sleep(0.4)

    sub = finder.watch("**/*.txt", slow_callback)
    (Path(watch_dir) / "slow.txt").write_text("x\n")
    assert in_callback.wait(EVENT_TIMEOUT_SECONDS), "callback never started"

    start = time.monotonic()
    assert sub.unsubscribe() is True
    assert time.monotonic() - start < 0.3, "unsubscribe must not wait out the callback"

    # no new invocations after unsubscribe returned
    with lock:
        seen = len(calls)
    (Path(watch_dir) / "after-unsub.txt").write_text("y\n")
    time.sleep(QUIET_PERIOD_SECONDS)
    with lock:
        assert len(calls) == seen, "callback started after unsubscribe returned"


def test_unsubscribe_from_inside_callback_does_not_deadlock(
    finder: FileFinder, watch_dir: str
) -> None:
    unsubscribed = threading.Event()
    sub_holder: list[WatchSubscription] = []

    def one_shot(_batch: list[WatchEvent]) -> None:
        # self-unsubscribe from the dispatch thread (one-shot pattern)
        if sub_holder and sub_holder[0].unsubscribe():
            unsubscribed.set()

    sub_holder.append(finder.watch("**/*.txt", one_shot))
    (Path(watch_dir) / "once.txt").write_text("x\n")

    assert unsubscribed.wait(EVENT_TIMEOUT_SECONDS), "self-unsubscribe deadlocked"
    assert sub_holder[0].active is False
