//! Cursor store for grep pagination.
//!
//! Maintains an in-memory map of opaque cursor IDs to file offsets.
//! Cursors are evicted LRU-style when the store exceeds capacity.

use std::collections::{HashMap, VecDeque};

const MAX_CURSORS: usize = 20;

#[derive(Clone, Copy, Default)]
pub(crate) struct GrepCursor {
    pub file_offset: usize,
    pub stage: GrepStage,
    pub found_matches: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum GrepStage {
    #[default]
    Normal,
    Broadened,
    Definitions,
    References,
    FuzzyDefinitions,
    FuzzyMatches,
}

impl GrepStage {
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Definitions => Some(Self::References),
            Self::References => Some(Self::FuzzyDefinitions),
            Self::FuzzyDefinitions => Some(Self::FuzzyMatches),
            _ => None,
        }
    }

    pub fn is_fuzzy(self) -> bool {
        matches!(self, Self::FuzzyDefinitions | Self::FuzzyMatches)
    }

    pub fn definition_mode(self) -> fff::DefinitionMode {
        match self {
            Self::Definitions | Self::FuzzyDefinitions => fff::DefinitionMode::DefinitionOnly,
            Self::References | Self::FuzzyMatches => fff::DefinitionMode::UsageOnly,
            Self::Normal | Self::Broadened => fff::DefinitionMode::Classify,
        }
    }

    pub fn notice(self) -> Option<&'static str> {
        match self {
            Self::References => Some("No matching definitions found. Showing other matches:"),
            Self::FuzzyDefinitions => {
                Some("No exact matches found. Maybe you meant these definitions:")
            }
            Self::FuzzyMatches => Some(
                "No exact matches or approximate definitions found. Maybe you meant these other matches:",
            ),
            _ => None,
        }
    }
}

/// Stores cursor state for paginated grep results.
pub struct CursorStore {
    counter: u64,
    /// Map from cursor ID string → file offset for next page.
    cursors: HashMap<String, GrepCursor>,
    /// Insertion order for LRU eviction.
    insertion_order: VecDeque<String>,
}

impl CursorStore {
    pub fn new() -> Self {
        Self {
            counter: 0,
            cursors: HashMap::new(),
            insertion_order: VecDeque::new(),
        }
    }

    /// Store a cursor and return its opaque ID string.
    pub fn store(&mut self, file_offset: usize) -> String {
        self.store_grep(GrepCursor {
            file_offset,
            ..Default::default()
        })
    }

    pub(crate) fn store_grep(&mut self, cursor: GrepCursor) -> String {
        self.counter = self.counter.wrapping_add(1);
        let id = self.counter.to_string();

        self.cursors.insert(id.clone(), cursor);
        self.insertion_order.push_back(id.clone());

        // Evict oldest cursors
        while self.cursors.len() > MAX_CURSORS {
            if let Some(oldest) = self.insertion_order.pop_front() {
                self.cursors.remove(&oldest);
            } else {
                break;
            }
        }

        id
    }

    /// Retrieve the file offset for a cursor ID.
    pub fn get(&self, id: &str) -> Option<usize> {
        self.get_grep(id).map(|cursor| cursor.file_offset)
    }

    pub(crate) fn get_grep(&self, id: &str) -> Option<GrepCursor> {
        self.cursors.get(id).copied()
    }
}
