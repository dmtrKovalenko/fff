use crate::GrepMode;
use fff_query_parser::{FFFQuery, FuzzyQuery};

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
    pub raw_query: String,
    /// Pattern after parsing. Multipart path patterns are joined with spaces.
    pub effective_pattern: String,
    /// Engine used, including a fallback from invalid regex to plain text.
    pub mode: SearchMode,
    /// Typo budget for fuzzy matching. Match positions are on the result page.
    pub max_typos: Option<u16>,
}

/// Caller-owned storage for a search. Swap two buffers to reuse their allocations.
#[derive(Debug, Default)]
pub struct SearchBuffer {
    metadata: Option<SearchMetadata>,
    pub(crate) index_revision: Option<u64>,
    files: [Vec<u32>; 2],
    dirs: Vec<u32>,
}

pub(crate) struct Candidates<'a, T> {
    pub previous: Option<&'a T>,
    pub next: &'a mut T,
}

pub(crate) struct FuzzyBuffers<'a> {
    previous: Option<&'a SearchBuffer>,
    next: &'a mut SearchBuffer,
}

impl SearchBuffer {
    /// The query last written to this buffer, if it has been used.
    pub fn metadata(&self) -> Option<&SearchMetadata> {
        self.metadata.as_ref()
    }

    pub(crate) fn record_grep(
        &mut self,
        raw_query: &str,
        pattern: &str,
        mode: GrepMode,
        max_typos: Option<u16>,
    ) {
        let metadata = self.reset(raw_query, SearchMode::Grep(mode));
        metadata.effective_pattern.push_str(pattern);
        metadata.max_typos = max_typos;
    }

    pub(crate) fn begin_fuzzy<'a>(
        &'a mut self,
        previous: &'a Self,
        query: &FFFQuery<'_>,
        mode: SearchMode,
        index_revision: u64,
    ) -> Option<FuzzyBuffers<'a>> {
        let metadata = self.reset(query.raw_query, mode);
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
        if !query.constraints.is_empty()
            || needle.len() < 2
            || !needle.is_ascii()
            || mode == SearchMode::Mixed && dirs_only(query)
        {
            return None;
        }

        let reuse = previous.index_revision == Some(index_revision)
            && previous.metadata.as_ref().is_some_and(|old| {
                old.mode == mode
                    && old.max_typos == metadata.max_typos
                    && needle.starts_with(&old.effective_pattern)
                    && has_uppercase(needle) == has_uppercase(&old.effective_pattern)
            });
        Some(FuzzyBuffers {
            previous: reuse.then_some(previous),
            next: self,
        })
    }

    fn reset(&mut self, raw_query: &str, mode: SearchMode) -> &mut SearchMetadata {
        self.index_revision = None;
        self.files.iter_mut().for_each(Vec::clear);
        self.dirs.clear();
        let metadata = self.metadata.get_or_insert_with(|| SearchMetadata {
            raw_query: String::new(),
            effective_pattern: String::new(),
            mode,
            max_typos: None,
        });
        metadata.raw_query.clear();
        metadata.raw_query.push_str(raw_query);
        metadata.effective_pattern.clear();
        metadata.mode = mode;
        metadata.max_typos = None;
        metadata
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
