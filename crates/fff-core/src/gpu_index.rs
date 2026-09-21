use std::sync::atomic::{AtomicU64, Ordering};

// Bumped whenever a file's frecency or git status changes so the GPU's
// per-item boost table is re-uploaded before the next search.
static BOOST_GEN: AtomicU64 = AtomicU64::new(1);

#[inline]
pub fn bump_boost_gen() {
    BOOST_GEN.fetch_add(1, Ordering::Relaxed);
}

#[derive(Debug, Clone, Default)]
pub struct GpuStatus {
    pub compiled: bool,
    pub enabled: bool,
    pub adapter: Option<String>,
    pub indexed_files: Option<usize>,
}

#[cfg(feature = "gpu")]
mod imp {
    use super::*;
    use crate::constants::MAX_OVERFLOW_FILES;
    use crate::git::is_modified_status;
    use crate::simd_path::ArenaPtr;
    use crate::types::FileItem;
    use parking_lot::Mutex;
    use std::time::Instant;

    struct Index {
        matcher: fff_gpu::GpuMatcher,
        base_key: (usize, usize),
        indexed: usize,
        boost_gen: u64,
    }

    // GPU copy of the file list: base files plus overflow, appended as the
    // overflow grows; rebuilt when the base slice changes (rescan).
    #[derive(Default)]
    pub struct GpuState {
        index: Mutex<Option<Index>>,
    }

    pub struct Files<'a> {
        pub files: &'a [FileItem],
        pub base_count: usize,
        pub base_arena: ArenaPtr,
        pub overflow_arena: ArenaPtr,
    }

    impl Files<'_> {
        fn arena(&self, i: usize) -> ArenaPtr {
            if i < self.base_count {
                self.base_arena
            } else {
                self.overflow_arena
            }
        }

        fn paths(&self, range: std::ops::Range<usize>) -> (Vec<String>, Vec<u32>) {
            let mut paths = Vec::with_capacity(range.len());
            let mut fname = Vec::with_capacity(range.len());
            let mut buf = String::with_capacity(128);
            for i in range {
                let f = &self.files[i];
                f.write_relative_path_from_arena(self.arena(i), &mut buf);
                paths.push(buf.clone());
                fname.push(f.filename_offset_in_relative_path() as u32);
            }
            (paths, fname)
        }
    }

    impl std::fmt::Debug for GpuState {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("GpuState")
                .field("built", &self.index.lock().is_some())
                .finish()
        }
    }

    impl GpuState {
        pub fn enabled() -> bool {
            std::env::var("FFF_GPU").map(|v| v != "0").unwrap_or(true)
        }

        // Runs `f` against an index that covers all of `files`, with boosts current.
        pub fn with_index<R>(
            &self,
            files: &Files<'_>,
            f: impl FnOnce(&fff_gpu::GpuMatcher) -> R,
        ) -> Option<R> {
            if !Self::enabled() || files.files.is_empty() {
                return None;
            }
            let base = &files.files[..files.base_count];
            let base_key = (base.as_ptr() as usize, base.len());
            let n = files.files.len();
            let mut guard = self.index.lock();

            let mut fresh = false;
            if let Some(ix) = guard
                .as_ref()
                .filter(|ix| ix.base_key == base_key && ix.indexed <= n)
            {
                if ix.indexed < n {
                    let (paths, fname) = files.paths(ix.indexed..n);
                    if ix.matcher.append(&paths, Some(&fname)).is_ok() {
                        guard.as_mut().unwrap().indexed = n;
                        fresh = true;
                    } else {
                        *guard = None;
                    }
                }
            } else {
                *guard = None;
            }
            if guard.is_none() {
                let t = Instant::now();
                let (paths, fname) = files.paths(0..n);
                let matcher = fff_gpu::GpuMatcher::with_reserve(
                    &paths,
                    Some(&fname),
                    fff_gpu::Kernel::Scalar,
                    fff_gpu::Reserve {
                        items: MAX_OVERFLOW_FILES,
                        bytes: MAX_OVERFLOW_FILES * 512,
                    },
                );
                tracing::info!(files = n, elapsed = ?t.elapsed(), adapter = %matcher.adapter_name, "gpu index built");
                *guard = Some(Index {
                    matcher,
                    base_key,
                    indexed: n,
                    boost_gen: 0,
                });
                fresh = true;
            }

            let ix = guard.as_mut().unwrap();
            let generation = BOOST_GEN.load(Ordering::Relaxed);
            if fresh || ix.boost_gen != generation {
                let boosts: Vec<i32> = files
                    .files
                    .iter()
                    .map(|f| {
                        f.total_frecency_score()
                            + if f.git_status.is_some_and(is_modified_status) {
                                15
                            } else {
                                0
                            }
                    })
                    .collect();
                ix.matcher.set_boosts(&boosts);
                ix.boost_gen = generation;
            }
            Some(f(&ix.matcher))
        }

        pub fn status(&self) -> GpuStatus {
            let guard = self.index.lock();
            GpuStatus {
                compiled: true,
                enabled: Self::enabled(),
                adapter: guard.as_ref().map(|i| i.matcher.adapter_name.clone()),
                indexed_files: guard.as_ref().map(|i| i.indexed),
            }
        }
    }
}

#[cfg(feature = "gpu")]
pub use imp::{Files, GpuState};

#[cfg(not(feature = "gpu"))]
#[derive(Default, Debug)]
pub struct GpuState;

#[cfg(not(feature = "gpu"))]
impl GpuState {
    pub fn status(&self) -> GpuStatus {
        GpuStatus::default()
    }
}
