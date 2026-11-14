mod segment;
mod stream;

pub use segment::StreamedFiles;
pub(crate) use stream::LazyStream;

use crate::error::Error;
use crate::file_picker::{FilePicker, FuzzySearchOptions};
use crate::grep::{GrepResult, GrepSearchOptions};
use crate::query_tracker::QueryTracker;
use crate::scan::InitialScan;
use crate::shared::{SharedFilePicker, SharedFrecency};
use crate::simd_path::ArenaPtr;
use crate::types::{FFFStringStorage, FileItem, SearchResult};
use fff_query_parser::FFFQuery;
use std::sync::Arc;
use std::time::Duration;
use stream::{StreamedFuzzy, StreamedGrep};

/// Where a lazy page was answered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LazySource {
    /// The built index, exactly like the non-lazy search.
    Index,
    /// Files streamed by the running scan. `complete` is `false` when the
    /// search returned before the walk ended, so more results may exist.
    Stream { complete: bool },
}

/// Resolves the paths of the files in a lazy page.
#[derive(Clone, Copy)]
pub enum LazyStorage<'a> {
    Index(&'a FilePicker),
    Stream(&'a StreamedFiles),
}

/// One page of a lazy search together with the storage its files live in.
pub struct LazyPage<'a, T> {
    pub result: T,
    pub storage: LazyStorage<'a>,
    pub source: LazySource,
}

impl SharedFilePicker {
    /// Grep that does not wait for the index. While the initial scan is
    /// running (it is started here for a lazy picker) the walked files are
    /// searched as they arrive and `f` gets the first page as soon as it is
    /// full; indexing continues in the background. Once the index is built
    /// this is the regular [`FilePicker::grep`].
    ///
    /// Only the first page streams: a streamed page has no `next_file_offset`
    /// and a non-zero `options.file_offset` waits for the index.
    pub fn grep_lazy<R>(
        &self,
        shared_frecency: &SharedFrecency,
        query: &FFFQuery<'_>,
        options: &GrepSearchOptions,
        f: impl FnOnce(LazyPage<'_, GrepResult<'_>>) -> R,
    ) -> Result<R, Error> {
        let mut stream = {
            let guard = self.read()?;
            let picker = guard.as_ref().ok_or(Error::FilePickerMissing)?;
            match picker.lazy_stream(self, shared_frecency) {
                Some(stream) => stream,
                None => return Ok(f(LazyPage::indexed(picker.grep(query, options), picker))),
            }
        };

        if options.file_offset == 0
            && let Some(streamed) = stream.grep(query, options)
        {
            let StreamedGrep {
                store,
                page,
                complete,
            } = streamed;
            return Ok(f(LazyPage {
                result: page.into_result(&store),
                storage: LazyStorage::Stream(&store),
                source: LazySource::Stream { complete },
            }));
        }

        stream.wait_settled();
        drop(stream);
        let guard = self.read()?;
        let picker = guard.as_ref().ok_or(Error::FilePickerMissing)?;
        Ok(f(LazyPage::indexed(picker.grep(query, options), picker)))
    }

    /// Fuzzy file search that does not wait for the index. While the initial
    /// scan is running (it is started here for a lazy picker) the walked files
    /// are ranked as they arrive; indexing continues in the background. Once
    /// the index is built this is the regular [`FilePicker::fuzzy_search`].
    ///
    /// Without `time_budget` the page is returned when the walk ends, before
    /// the index is assembled. With it the best page seen so far is returned
    /// once the budget is spent, marked as incomplete.
    pub fn fuzzy_search_lazy<'q, R>(
        &self,
        shared_frecency: &SharedFrecency,
        query: &'q FFFQuery<'q>,
        query_tracker: Option<&QueryTracker>,
        options: FuzzySearchOptions<'q>,
        time_budget: Option<Duration>,
        f: impl FnOnce(LazyPage<'_, SearchResult<'_>>) -> R,
    ) -> Result<R, Error> {
        let mut stream = {
            let guard = self.read()?;
            let picker = guard.as_ref().ok_or(Error::FilePickerMissing)?;
            match picker.lazy_stream(self, shared_frecency) {
                Some(stream) => stream,
                None => {
                    let result = picker.fuzzy_search(query, query_tracker, options);
                    return Ok(f(LazyPage::indexed(result, picker)));
                }
            }
        };

        if options.pagination.offset == 0
            && let Some(streamed) = stream.fuzzy_search(query, query_tracker, options, time_budget)
        {
            let StreamedFuzzy {
                store,
                page,
                complete,
            } = streamed;
            return Ok(f(LazyPage {
                result: page.into_result(&store, query),
                storage: LazyStorage::Stream(&store),
                source: LazySource::Stream { complete },
            }));
        }

        stream.wait_settled();
        drop(stream);
        let guard = self.read()?;
        let picker = guard.as_ref().ok_or(Error::FilePickerMissing)?;
        let result = picker.fuzzy_search(query, query_tracker, options);
        Ok(f(LazyPage::indexed(result, picker)))
    }

    /// Starts the scan of a picker created with
    /// [`FilePicker::new_lazy_with_shared_state`] without searching.
    /// Returns `false` when the scan was already started.
    pub fn start_deferred_scan(&self, shared_frecency: &SharedFrecency) -> Result<bool, Error> {
        let guard = self.read()?;
        let picker = guard.as_ref().ok_or(Error::FilePickerMissing)?;

        let mut state = picker.initial_scan.lock();
        if !matches!(*state, InitialScan::Deferred) {
            return Ok(false);
        }
        let job = picker.begin_initial_scan(&mut state, self, shared_frecency);
        drop(state);
        job.spawn();
        Ok(true)
    }
}

impl FilePicker {
    // Subscribes to the initial walk, starting it for a lazy picker.
    // `None` once the index is built.
    fn lazy_stream(
        &self,
        shared_picker: &SharedFilePicker,
        shared_frecency: &SharedFrecency,
    ) -> Option<LazyStream> {
        let mut state = self.initial_scan.lock();
        let job = matches!(*state, InitialScan::Deferred)
            .then(|| self.begin_initial_scan(&mut state, shared_picker, shared_frecency));
        let cursor = match &*state {
            InitialScan::Walking(collector) => collector.subscribe(),
            _ => return None,
        };
        drop(state);
        // Subscribed first, so the cursor sees the walk from its first file.
        if let Some(job) = job {
            job.spawn();
        }

        Some(LazyStream {
            cursor,
            base_path: self.base_path.clone(),
            mode: self.mode,
            cancelled: Arc::clone(&self.signals.cancelled),
            frecency: shared_frecency.clone(),
        })
    }
}

impl<'a, T> LazyPage<'a, T> {
    fn indexed(result: T, picker: &'a FilePicker) -> Self {
        Self {
            result,
            storage: LazyStorage::Index(picker),
            source: LazySource::Index,
        }
    }
}

impl FFFStringStorage for LazyStorage<'_> {
    #[inline]
    fn arena_for(&self, file: &FileItem) -> ArenaPtr {
        match self {
            Self::Index(picker) => picker.arena_for(file),
            Self::Stream(files) => files.arena_for(file),
        }
    }

    #[inline]
    fn base_arena(&self) -> ArenaPtr {
        match self {
            Self::Index(picker) => picker.base_arena(),
            Self::Stream(files) => files.base_arena(),
        }
    }

    #[inline]
    fn overflow_arena(&self) -> ArenaPtr {
        match self {
            Self::Index(picker) => picker.overflow_arena(),
            Self::Stream(files) => files.overflow_arena(),
        }
    }
}
