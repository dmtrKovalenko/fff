use crate::grep::{GrepReuse, fuzzy_grep_max_typos};
use crate::types::FileItem;
use crate::{GrepMode, GrepSearchOptions};
use fff_query_parser::{FFFQuery, FuzzyQuery};
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

/// The search engine that produced a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchMode {
    Files,
    Directories,
    Mixed,
    Grep(GrepMode),
}

/// The query that produced the last page written to a [`SearchBuffer`].
#[derive(Debug)]
#[non_exhaustive]
pub struct SearchMetadata {
    /// Start of the scan that supplied these candidates, retained across refinements.
    pub scanned_at: Instant,
    pub raw_query: String,
    /// Pattern after parsing. Multipart path patterns are joined with spaces.
    pub effective_pattern: String,
    /// Engine used, including a fallback from invalid regex to plain text.
    pub mode: SearchMode,
    /// Typo budget for fuzzy matching. Match positions are on the result page.
    pub max_typos: Option<u16>,
    /// Files skipped because an unexpired previous grep ruled them out.
    pub grep_files_reused: usize,
}

/// Caller-owned storage for a search. Swap two buffers to reuse their allocations.
#[derive(Debug)]
pub struct SearchBuffer {
    metadata: Option<SearchMetadata>,
    ttl: Duration,
    pub(crate) index_revision: Option<u64>,
    files: [Vec<u32>; 2],
    dirs: Vec<u32>,
    grep_misses: Vec<AtomicU64>,
    grep_insensitive: bool,
}

pub(crate) struct Candidates<'a, T> {
    pub previous: Option<&'a T>,
    pub next: &'a mut T,
}

pub(crate) struct FuzzyBuffers<'a> {
    previous: Option<&'a SearchBuffer>,
    next: &'a mut SearchBuffer,
}

impl Default for SearchBuffer {
    fn default() -> Self {
        Self::with_ttl(Duration::from_secs(15))
    }
}

impl SearchBuffer {
    /// Set the maximum age from the original scan. The default is 15 seconds. Zero disables reuse.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            metadata: None,
            index_revision: None,
            files: Default::default(),
            dirs: Vec::new(),
            grep_misses: Vec::new(),
            grep_insensitive: false,
        }
    }

    /// The query last written to this buffer, if it has been used.
    pub fn metadata(&self) -> Option<&SearchMetadata> {
        self.metadata.as_ref()
    }

    pub(crate) fn record_grep(
        &mut self,
        pattern: &str,
        mode: GrepMode,
        max_typos: Option<u16>,
        reused: usize,
    ) {
        let metadata = self
            .metadata
            .as_mut()
            .expect("begin_grep initializes metadata");
        metadata.mode = SearchMode::Grep(mode);
        metadata.effective_pattern.push_str(pattern);
        metadata.max_typos = max_typos;
        metadata.grep_files_reused = reused;
    }

    pub(crate) fn begin_grep<'a>(
        &'a mut self,
        previous: &'a Self,
        revision: u64,
        files: &'a [FileItem],
        raw_query: &str,
        pattern: &str,
        options: &GrepSearchOptions,
    ) -> Option<GrepReuse<'a>> {
        let insensitive = options.effective_casing().is_insensitive_for(pattern);
        let mode = SearchMode::Grep(options.mode);
        let capture = !self.ttl.is_zero() && !pattern.is_empty();
        let (metadata, old) = self.begin(previous, revision, raw_query, mode);
        let old = old.filter(|old| {
            previous.grep_insensitive == insensitive
                && !old.effective_pattern.is_empty()
                && grep_refines(&old.effective_pattern, pattern, options.mode)
        });
        if let Some(old) = old {
            metadata.scanned_at = old.scanned_at;
        }
        self.grep_insensitive = insensitive;
        self.grep_misses.clear();
        if !capture {
            return None;
        }
        self.grep_misses
            .resize_with(files.len().div_ceil(64), AtomicU64::default);
        Some(GrepReuse::new(
            files,
            if old.is_some() {
                &previous.grep_misses
            } else {
                &[]
            },
            &self.grep_misses,
        ))
    }

    pub(crate) fn begin_fuzzy<'a>(
        &'a mut self,
        previous: &'a Self,
        query: &FFFQuery<'_>,
        mode: SearchMode,
        index_revision: u64,
    ) -> Option<FuzzyBuffers<'a>> {
        let capture = !self.ttl.is_zero();
        let (metadata, old) = self.begin(previous, index_revision, query.raw_query, mode);
        let parts = match &query.fuzzy_query {
            FuzzyQuery::Text(text) => std::slice::from_ref(text),
            FuzzyQuery::Parts(parts) => parts.as_slice(),
            FuzzyQuery::Empty => &[],
        };
        for part in parts.iter().filter(|part| part.len() >= 2) {
            if !metadata.effective_pattern.is_empty() {
                metadata.effective_pattern.push(' ');
            }
            metadata.effective_pattern.push_str(part);
        }
        if !metadata.effective_pattern.is_empty() {
            metadata.max_typos = Some(fuzzy_max_typos(query));
        }

        let FuzzyQuery::Text(needle) = query.fuzzy_query else {
            return None;
        };
        if !capture
            || !query.constraints.is_empty()
            || needle.len() < 2
            || !needle.is_ascii()
            || mode == SearchMode::Mixed && dirs_only(query)
        {
            return None;
        }

        let old = old.filter(|old| {
            old.max_typos == metadata.max_typos
                && needle.starts_with(&old.effective_pattern)
                && has_uppercase(needle) == has_uppercase(&old.effective_pattern)
        });
        if let Some(old) = old {
            metadata.scanned_at = old.scanned_at;
        }
        Some(FuzzyBuffers {
            previous: old.map(|_| previous),
            next: self,
        })
    }

    fn begin<'a>(
        &mut self,
        previous: &'a Self,
        revision: u64,
        raw_query: &str,
        mode: SearchMode,
    ) -> (&mut SearchMetadata, Option<&'a SearchMetadata>) {
        let now = Instant::now();
        let old = previous.metadata().filter(|old| {
            previous.index_revision == Some(revision)
                && old.mode == mode
                && now.duration_since(old.scanned_at) < self.ttl.min(previous.ttl)
        });
        self.index_revision = None;
        self.files.iter_mut().for_each(Vec::clear);
        self.dirs.clear();
        let metadata = self.metadata.get_or_insert_with(|| SearchMetadata {
            scanned_at: now,
            raw_query: String::new(),
            effective_pattern: String::new(),
            mode,
            max_typos: None,
            grep_files_reused: 0,
        });
        metadata.scanned_at = now;
        metadata.raw_query.clear();
        metadata.raw_query.push_str(raw_query);
        metadata.effective_pattern.clear();
        metadata.mode = mode;
        metadata.max_typos = None;
        metadata.grep_files_reused = 0;
        (metadata, old)
    }
}

impl FuzzyBuffers<'_> {
    pub(crate) fn files(&mut self) -> Candidates<'_, [Vec<u32>; 2]> {
        Candidates {
            previous: self.previous.map(|previous| &previous.files),
            next: &mut self.next.files,
        }
    }

    pub(crate) fn dirs(&mut self) -> Candidates<'_, Vec<u32>> {
        Candidates {
            previous: self.previous.map(|previous| &previous.dirs),
            next: &mut self.next.dirs,
        }
    }
}

pub(crate) fn fuzzy_max_typos(query: &FFFQuery<'_>) -> u16 {
    let effective_query = match &query.fuzzy_query {
        FuzzyQuery::Text(text) => text,
        FuzzyQuery::Parts(parts) if !parts.is_empty() => parts[0],
        _ => query.raw_query.trim(),
    };
    (effective_query.len() as u16 / 4).clamp(2, 6)
}

pub(crate) fn dirs_only(query: &FFFQuery<'_>) -> bool {
    query.raw_query.ends_with(['/', std::path::MAIN_SEPARATOR])
}

fn has_uppercase(text: &str) -> bool {
    text.bytes().any(|byte| byte.is_ascii_uppercase())
}

fn grep_refines(old: &str, new: &str, mode: GrepMode) -> bool {
    if new == old {
        return true;
    }
    let Some(suffix) = new.strip_prefix(old) else {
        return false;
    };
    match mode {
        GrepMode::PlainText => !old.ends_with('\\'),
        GrepMode::Regex => {
            // A word appended to a line-local expression can only narrow its matches.
            !old.contains("(?")
                && old
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.+*?|()[]-".contains(&b))
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        }
        GrepMode::Fuzzy => new.is_ascii() && fuzzy_grep_max_typos(new) == fuzzy_grep_max_typos(old),
    }
}
