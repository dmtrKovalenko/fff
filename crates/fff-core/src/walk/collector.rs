use crate::types::FileItem;
use parking_lot::{Condvar, Mutex};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const IDLE_WAIT: Duration = Duration::from_millis(1);
// Smaller pulls wait one idle tick so segments do not degenerate into a few files.
const MIN_BATCH: usize = 256;

// Keep adjacent workers' locks and vector headers on separate cache lines.
#[repr(align(128))]
#[derive(Default)]
struct WorkerFiles {
    files: Vec<(FileItem, String)>,
    dirs: Vec<String>,
}

// Sink both walker backends push into, one shard per walker thread. Lazy
// searches subscribe a cursor and read the files while the walk is running.
#[derive(Default)]
pub(crate) struct WalkCollector {
    shards: OnceLock<Box<[Mutex<WorkerFiles>]>>,
    // Lock order: `readers` first, then the shards. Walker threads never take it.
    readers: Mutex<Readers>,
    changed: Condvar,
}

#[derive(Default)]
struct Readers {
    // Per cursor: how many files of every shard it has read.
    cursors: Vec<(u64, Vec<usize>)>,
    next_cursor_id: u64,
    end: Option<WalkEnd>,
    outcome: WalkOutcome,
}

struct WalkEnd {
    totals: Vec<usize>,
    tail_starts: Vec<usize>,
    // Per shard: the files no subscribed cursor had read when the walk finished.
    tails: Option<Arc<Vec<WalkBatch>>>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum WalkOutcome {
    #[default]
    Pending,
    Committed,
    Aborted,
}

impl WalkCollector {
    // Has to run before the first push; `workers` bounds the worker ids.
    pub(crate) fn start(&self, workers: usize) {
        self.shards
            .get_or_init(|| (0..workers.max(1)).map(|_| Mutex::default()).collect());
    }

    // Returns the number of files the worker has pushed so far.
    #[inline]
    pub(crate) fn push_file(&self, worker: usize, file: FileItem, relative_path: String) -> usize {
        let mut shard = self.shards()[worker].lock();
        shard.files.push((file, relative_path));
        shard.files.len()
    }

    #[inline]
    pub(crate) fn push_dir(&self, worker: usize, relative_path: String) {
        self.shards()[worker].lock().dirs.push(relative_path);
    }

    #[cfg_attr(not(feature = "zlob"), allow(dead_code))]
    pub(crate) fn file_count(&self) -> usize {
        self.shards()
            .iter()
            .map(|shard| shard.lock().files.len())
            .sum()
    }

    // Ends the walk and hands the collected entries to the index builder.
    pub(crate) fn finish(&self) -> (Vec<(FileItem, String)>, Vec<String>) {
        let mut readers = self.readers.lock();
        let mut shards: Vec<_> = self.shards().iter().map(|shard| shard.lock()).collect();

        let totals: Vec<usize> = shards.iter().map(|shard| shard.files.len()).collect();
        let tail_starts: Vec<usize> = totals
            .iter()
            .enumerate()
            .map(|(shard, &total)| {
                let read = |(_, read): &(u64, Vec<usize>)| read.get(shard).copied().unwrap_or(0);
                readers.cursors.iter().map(read).min().unwrap_or(total)
            })
            .collect();
        let tails = (!readers.cursors.is_empty()).then(|| {
            let tails = shards.iter().zip(&tail_starts).map(|(shard, &start)| {
                let mut tail = WalkBatch::default();
                tail.append_entries(&shard.files[start..]);
                tail
            });
            Arc::new(tails.collect())
        });
        readers.end = Some(WalkEnd {
            totals,
            tail_starts,
            tails,
        });

        let mut files = Vec::with_capacity(shards.iter().map(|shard| shard.files.len()).sum());
        let mut dirs = Vec::with_capacity(shards.iter().map(|shard| shard.dirs.len()).sum());
        // Taken, not drained: the collector outlives the walk and must not keep the buffers.
        for shard in &mut shards {
            files.append(&mut std::mem::take(&mut shard.files));
            dirs.append(&mut std::mem::take(&mut shard.dirs));
        }
        drop(shards);
        drop(readers);
        self.changed.notify_all();
        (files, dirs)
    }

    // The walked files are searchable through the picker from now on.
    pub(crate) fn commit(&self) {
        self.settle(WalkOutcome::Committed);
    }

    // No-op once committed, so it is safe to call on every scan exit path.
    pub(crate) fn abort(&self) {
        self.settle(WalkOutcome::Aborted);
    }

    pub(crate) fn subscribe(self: &Arc<Self>) -> WalkCursor {
        let mut readers = self.readers.lock();
        let id = readers.next_cursor_id;
        readers.next_cursor_id += 1;
        readers.cursors.push((id, Vec::new()));
        drop(readers);

        WalkCursor {
            collector: Arc::clone(self),
            id,
            read: Vec::new(),
        }
    }

    fn settle(&self, outcome: WalkOutcome) {
        let mut readers = self.readers.lock();
        if readers.outcome == WalkOutcome::Pending {
            readers.outcome = outcome;
        }
        drop(readers);
        self.changed.notify_all();
    }

    #[inline]
    fn shards(&self) -> &[Mutex<WorkerFiles>] {
        self.shards.get().map_or(&[], |shards| shards)
    }
}

pub(crate) enum WalkPull {
    Files(WalkBatch),
    // The walk ended and this cursor has read every file of it.
    Complete,
    // The deadline passed before new files arrived.
    Idle,
    // The cursor can not see the whole walk: subscribed too late or the scan aborted.
    Lost,
}

pub(crate) struct WalkCursor {
    collector: Arc<WalkCollector>,
    id: u64,
    read: Vec<usize>,
}

impl WalkCursor {
    // Blocks until the walker produced new files, the walk ended, or `deadline` passed.
    pub(crate) fn next(&mut self, max_files: usize, deadline: Option<Instant>) -> WalkPull {
        let mut waited = false;
        let mut readers = self.collector.readers.lock();
        loop {
            if let Some(end) = &readers.end {
                self.read.resize(end.totals.len(), 0);
                if self.read == end.totals {
                    return WalkPull::Complete;
                }
                let missed = self.read.iter().zip(&end.tail_starts).any(|(r, s)| r < s);
                let Some(tails) = end.tails.clone().filter(|_| !missed) else {
                    return WalkPull::Lost;
                };

                let skips: Vec<usize> = self
                    .read
                    .iter()
                    .zip(&end.tail_starts)
                    .map(|(read, start)| read - start)
                    .collect();
                self.read.clone_from(&end.totals);
                drop(readers);

                let mut batch = WalkBatch::default();
                for (tail, skip) in tails.iter().zip(skips) {
                    batch.append_batch(tail, skip);
                }
                return WalkPull::Files(batch);
            }
            if readers.outcome == WalkOutcome::Aborted {
                return WalkPull::Lost;
            }

            let shards = self.collector.shards();
            self.read.resize(shards.len(), 0);
            let available: usize = shards
                .iter()
                .zip(&self.read)
                .map(|(shard, read)| shard.lock().files.len() - read)
                .sum();

            if available >= MIN_BATCH || (available > 0 && waited) {
                let mut batch = WalkBatch::default();
                for (shard, read) in shards.iter().zip(&mut self.read) {
                    let shard = shard.lock();
                    let room = max_files.max(1).saturating_sub(batch.len());
                    let end = shard.files.len().min(read.saturating_add(room));
                    batch.append_entries(&shard.files[*read..end]);
                    *read = end;
                }

                let id = self.id;
                if let Some(cursor) = readers.cursors.iter_mut().find(|(c, _)| *c == id) {
                    cursor.1.clone_from(&self.read);
                }
                return WalkPull::Files(batch);
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return WalkPull::Idle;
            }
            self.collector.changed.wait_for(&mut readers, IDLE_WAIT);
            waited = true;
        }
    }

    // Blocks until the scan either committed its index or gave up.
    pub(crate) fn wait_settled(&self) {
        let mut readers = self.collector.readers.lock();
        while readers.outcome == WalkOutcome::Pending {
            self.collector.changed.wait(&mut readers);
        }
    }
}

impl Drop for WalkCursor {
    fn drop(&mut self) {
        let mut readers = self.collector.readers.lock();
        readers.cursors.retain(|(id, _)| *id != self.id);
        if readers.cursors.is_empty()
            && let Some(end) = readers.end.as_mut()
        {
            end.tails = None;
        }
    }
}

// Flat copy of a walked range: one path buffer instead of a string per file.
#[derive(Default)]
pub(crate) struct WalkBatch {
    files: Vec<FileItem>,
    paths: String,
    path_ends: Vec<usize>,
}

impl WalkBatch {
    pub(crate) fn len(&self) -> usize {
        self.files.len()
    }

    // Hands out the files after `visit` saw each one with its relative path.
    pub(crate) fn into_files(self, mut visit: impl FnMut(&mut FileItem, &str)) -> Vec<FileItem> {
        let mut files = self.files;
        let mut start = 0;
        for (file, &end) in files.iter_mut().zip(&self.path_ends) {
            visit(file, &self.paths[start..end]);
            start = end;
        }
        files
    }

    fn append_entries(&mut self, entries: &[(FileItem, String)]) {
        self.files.reserve(entries.len());
        self.path_ends.reserve(entries.len());
        self.paths
            .reserve(entries.iter().map(|(_, path)| path.len()).sum());
        for (file, path) in entries {
            self.files.push(file.clone());
            self.paths.push_str(path);
            self.path_ends.push(self.paths.len());
        }
    }

    fn append_batch(&mut self, other: &WalkBatch, skip: usize) {
        let skip = skip.min(other.files.len());
        let offset = if skip == 0 {
            0
        } else {
            other.path_ends[skip - 1]
        };
        let base = self.paths.len();
        self.files.extend_from_slice(&other.files[skip..]);
        self.paths.push_str(&other.paths[offset..]);
        self.path_ends.extend(
            other.path_ends[skip..]
                .iter()
                .map(|end| end - offset + base),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collector(workers: usize) -> Arc<WalkCollector> {
        let collector = Arc::new(WalkCollector::default());
        collector.start(workers);
        collector
    }

    fn push(collector: &WalkCollector, worker: usize, name: &str) {
        let file = FileItem::new_raw(0, 1, 0, None, false);
        collector.push_file(worker, file, name.to_string());
    }

    fn paths(pull: WalkPull) -> Vec<String> {
        let WalkPull::Files(batch) = pull else {
            panic!("expected files");
        };
        let mut paths = Vec::new();
        batch.into_files(|_, path| paths.push(path.to_string()));
        paths.sort();
        paths
    }

    #[test]
    fn cursor_reads_everything_walked_before_and_after_finish() {
        let collector = collector(2);
        let mut cursor = collector.subscribe();
        push(&collector, 0, "a.rs");
        push(&collector, 1, "b.rs");
        push(&collector, 1, "c.rs");

        assert_eq!(paths(cursor.next(1, None)), ["a.rs"]);

        push(&collector, 0, "d.rs");
        let (walked, _) = collector.finish();
        assert_eq!(walked.len(), 4);

        assert_eq!(
            paths(cursor.next(usize::MAX, None)),
            ["b.rs", "c.rs", "d.rs"]
        );
        assert!(matches!(cursor.next(usize::MAX, None), WalkPull::Complete));
    }

    #[test]
    fn late_cursor_is_lost_and_unblocked_by_commit() {
        let collector = collector(1);
        push(&collector, 0, "a.rs");
        collector.finish();

        let mut cursor = collector.subscribe();
        assert!(matches!(cursor.next(usize::MAX, None), WalkPull::Lost));
        collector.commit();
        cursor.wait_settled();
    }

    #[test]
    fn idle_cursor_returns_at_the_deadline() {
        let collector = collector(1);
        let mut cursor = collector.subscribe();
        let deadline = Instant::now() + Duration::from_millis(5);
        assert!(matches!(
            cursor.next(usize::MAX, Some(deadline)),
            WalkPull::Idle
        ));
    }
}
