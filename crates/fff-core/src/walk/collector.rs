use crate::types::FileItem;
use parking_lot::Mutex;
use std::sync::OnceLock;

// Keep adjacent workers' locks and vector headers on separate cache lines.
#[repr(align(128))]
#[derive(Default)]
struct WorkerFiles {
    files: Vec<(FileItem, String)>,
    dirs: Vec<String>,
}

// Sink both walker backends push into, one shard per walker thread.
#[derive(Default)]
pub(crate) struct WalkCollector {
    shards: OnceLock<Box<[Mutex<WorkerFiles>]>>,
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
        let mut shards: Vec<_> = self.shards().iter().map(|shard| shard.lock()).collect();
        let mut files = Vec::with_capacity(shards.iter().map(|shard| shard.files.len()).sum());
        let mut dirs = Vec::with_capacity(shards.iter().map(|shard| shard.dirs.len()).sum());
        for shard in &mut shards {
            files.append(&mut shard.files);
            dirs.append(&mut shard.dirs);
        }
        (files, dirs)
    }

    #[inline]
    fn shards(&self) -> &[Mutex<WorkerFiles>] {
        self.shards.get().map_or(&[], |shards| shards)
    }
}
