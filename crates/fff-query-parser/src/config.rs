use crate::constraints::Constraint;
use crate::glob_detect::has_wildcards;

/// Check if a token looks like a filename/path for use as a `FilePath` constraint.
#[deprecated(note = "use `Constraint::is_filename_constraint_token`")]
pub fn is_filename_constraint_token(token: &str) -> bool {
    Constraint::is_filename_constraint_token(token)
}

/// Parser configuration trait - allows different picker types to customize parsing
pub trait ParserConfig {
    fn enable_glob(&self) -> bool {
        true
    }

    /// Should parse extension shortcuts (e.g., *.rs)
    fn enable_extension(&self) -> bool {
        true
    }

    /// Should parse exclusion patterns (e.g., !test)
    fn enable_exclude(&self) -> bool {
        true
    }

    /// Should parse path segments (e.g., /src/)
    fn enable_path_segments(&self) -> bool {
        true
    }

    /// Should parse type constraints (e.g., type:rust)
    fn enable_type_filter(&self) -> bool {
        true
    }

    /// Should parse git status (e.g., status:modified)
    fn enable_git_status(&self) -> bool {
        true
    }

    /// Should parse location suffixes (e.g., file:12, file:12:4)
    /// Disabled for grep modes where colon-number patterns like localhost:8080
    /// are search text, not file locations.
    fn enable_location(&self) -> bool {
        true
    }

    /// Determine whether a token should be treated as a glob constraint.
    ///
    /// The default implementation delegates to `zlob::has_wildcards` with
    /// `RECOMMENDED` flags, which recognises `*`, `?`, `[`, `{…}` etc.
    ///
    /// Override this in configs where some wildcard characters are common
    /// in search text (e.g. grep mode where `?` and `[` appear in code).
    fn is_glob_pattern(&self, token: &str) -> bool {
        has_wildcards(token)
    }

    /// If `true`, a PathSegment constraint that is the ONLY token in the
    /// query is demoted to fuzzy text to avoid over filtering
    fn treat_lone_path_as_text(&self) -> bool {
        true
    }

    /// If `true`, tokens that look like filenames (`score.rs`, `src/main.rs`)
    /// become `FilePath` constraints that scope the search to matching paths.
    /// Off by default: a partial filename like `vite.conf` would otherwise
    /// filter out `vite.config.ts` and yield zero results.
    fn enable_filename_constraint(&self) -> bool {
        false
    }

    /// Custom constraint parsers for picker-specific needs.
    ///
    /// Returns `None` by default. Filename-token detection is handled
    /// separately by the parser via `enable_filename_constraint`.
    fn parse_custom<'a>(&self, _input: &'a str) -> Option<Constraint<'a>> {
        None
    }
}

/// Default configuration for the file picker.
///
/// Filename-constraint detection is off (trait default), so partial filenames
/// like `vite.conf` don't silently filter out fuzzy matches. The Neovim layer
/// overrides `enable_filename_constraint` to opt in based on user config.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileSearchConfig;

impl ParserConfig for FileSearchConfig {}

/// Configuration for full-text search (grep) - file constraints enabled for
/// filtering which files to search, git status disabled since it's not useful
/// when searching file contents.
///
/// Glob detection is narrowed: only patterns containing a path separator (`/`)
/// or brace expansion (`{…}`) are treated as globs. Characters like `?` and
/// `[` are extremely common in source code and must remain literal search text.
#[derive(Debug, Clone, Copy, Default)]
pub struct GrepConfig;

impl ParserConfig for GrepConfig {
    fn enable_path_segments(&self) -> bool {
        true
    }

    fn enable_git_status(&self) -> bool {
        false
    }

    fn enable_location(&self) -> bool {
        false
    }

    /// Only recognise globs that are clearly directory/path oriented.
    ///
    /// Characters like `?`, `[`, and bare `*` (without `/`) are extremely
    /// common in source code (`foo?`, `arr[0]`, `*ptr`) and must NOT be
    /// consumed as glob constraints. We only treat a token as a glob when
    /// it contains path-oriented patterns:
    ///
    /// - Contains `/` → path glob (e.g. `src/**/*.rs`, `*/tests/*`)
    /// - Contains `{…}` → brace expansion (e.g. `{src,lib}`)
    fn is_glob_pattern(&self, token: &str) -> bool {
        // Must contain at least one glob wildcard character
        if !has_wildcards(token) {
            return false;
        }

        let bytes = token.as_bytes();

        // Contains path separator → clearly a path glob
        if bytes.contains(&b'/') {
            return true;
        }

        // Brace expansion → useful for directory alternatives.
        // Require a comma between `{` and `}` AND at least one letter to
        // distinguish real glob expansions like `{src,lib}` or `*.{ts,tsx}`
        // from code patterns like `format!("{}")` and regex quantifiers `{2,3}`.
        // Guard `open < close` so reversed braces like `}{` don't panic on slicing.
        if let Some(open) = bytes.iter().position(|&b| b == b'{')
            && let Some(close) = bytes.iter().rposition(|&b| b == b'}')
            && open < close
        {
            let inner = &bytes[open + 1..close];
            if inner.contains(&b',') && inner.iter().any(|b| b.is_ascii_alphabetic()) {
                return true;
            }
        }

        // Everything else (?, [, bare * without /) → treat as literal text
        false
    }
}

/// Configuration for AI-mode grep — extends `GrepConfig` behavior with
/// automatic file-path constraint detection.
///
/// Bare filenames with valid extensions (`schema.rs`) and path-prefixed
/// filenames (`libswscale/input.c`) are detected as `FilePath` constraints
/// so the search is scoped to matching files. The caller validates the
/// constraint against the index and drops it if no files match (fallback).
#[derive(Debug, Clone, Copy, Default)]
pub struct AiGrepConfig;

/// Configuration for directory and mixed search modes.
///
/// Disables path segment parsing so that trailing `/` is kept as fuzzy text
/// (e.g. `fff-core/` fuzzy-matches directory paths instead of becoming a
/// `PathSegment("fff-core")` constraint with an empty query). Extension and
/// filename constraints are also disabled since they don't apply to directories.
#[derive(Debug, Clone, Copy, Default)]
pub struct DirSearchConfig;

impl ParserConfig for AiGrepConfig {
    fn enable_path_segments(&self) -> bool {
        true
    }

    fn enable_git_status(&self) -> bool {
        false
    }

    fn enable_location(&self) -> bool {
        false
    }

    fn enable_filename_constraint(&self) -> bool {
        true
    }

    fn is_glob_pattern(&self, token: &str) -> bool {
        // First check GrepConfig's strict rules (path globs, brace expansion)
        if GrepConfig.is_glob_pattern(token) {
            return true;
        }

        // AI agents use `*text*` to scope file searches (e.g. `*quote* TODO`).
        // Recognise tokens that start AND end with `*` with non-empty text
        // between them as glob constraints. Bare `*` or `**` are excluded.
        if !has_wildcards(token) {
            return false;
        }
        let bytes = token.as_bytes();
        bytes.len() >= 3
            && bytes[0] == b'*'
            && bytes[bytes.len() - 1] == b'*'
            && !bytes[1..bytes.len() - 1].contains(&b'*')
    }
}

impl ParserConfig for DirSearchConfig {
    fn enable_path_segments(&self) -> bool {
        false
    }

    fn enable_extension(&self) -> bool {
        false
    }

    fn enable_type_filter(&self) -> bool {
        false
    }

    fn enable_git_status(&self) -> bool {
        false
    }
}

/// Configuration for mixed (files + directories) search.
///
/// Like `DirSearchConfig`, disables path segment parsing so trailing `/`
/// triggers dirs-only mode instead of becoming a constraint. Keeps git
/// status and extension filters enabled since files are part of the results.
#[derive(Debug, Clone, Copy, Default)]
pub struct MixedSearchConfig;

impl ParserConfig for MixedSearchConfig {
    fn enable_path_segments(&self) -> bool {
        false
    }
}
