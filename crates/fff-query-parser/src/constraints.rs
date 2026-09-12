use crate::glob_detect::has_wildcards;

/// Constraint types that can be extracted from a query
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint<'a> {
    /// Match file extension: *.rs -> Extension("rs")
    Extension(&'a str),

    /// Glob pattern: **/*.rs -> Glob("**/*.rs")
    Glob(&'a str),

    /// Multiple text search parts: ["src", "name"]
    /// Uses slice to avoid allocation
    Parts(&'a [&'a str]),

    /// Single text token (optimized case)
    Text(&'a str),

    /// Exclude pattern: !test -> Exclude(&["test"])
    Exclude(&'a [&'a str]),

    /// Path constraint: /src/ -> PathSegment("src")
    PathSegment(&'a str),

    /// File path constraint (AI mode): "libswscale/input.c" → FilePath("libswscale/input.c")
    /// Matches files whose relative path ends with this suffix at a `/` boundary.
    FilePath(&'a str),

    /// File type constraint: type:rust -> FileType("rust")
    FileType(&'a str),

    /// Git status constraint: status:modified -> GitStatus(Modified)
    GitStatus(GitStatusFilter),

    /// Negation constraint: !extension:rs -> Not(Extension("rs"))
    /// Negates the inner constraint
    Not(Box<Constraint<'a>>),
}

impl Constraint<'_> {
    #[inline(always)]
    pub fn is_filename_constraint_token(token: &str) -> bool {
        let bytes = token.as_bytes();

        // Must NOT end with / or .
        if token.is_empty() || (bytes.last() == Some(&b'/') && bytes.first() != Some(&b'.')) {
            return false;
        }

        // Must NOT contain wildcards (those are globs)
        if has_wildcards(token) {
            return false;
        }

        // Get the filename component (after last /)
        let filename = token.rsplit('/').next().unwrap_or(token);

        // Extension must exist and look like a real file extension:
        // starts with an ASCII letter (rejects version numbers like "v2.0"),
        // followed by alphanumeric chars, max 10 chars total.
        match filename.rfind('.') {
            None => false,
            Some(dot_idx) => {
                let extension = &filename[dot_idx + 1..];

                extension.starts_with(|c: char| c.is_ascii_alphabetic())
                    && extension.len() <= 10 // just an sassumption
                    && extension.bytes().all(|b| b.is_ascii_alphanumeric())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatusFilter {
    Modified,
    Untracked,
    Staged,
    Unmodified,
}

/// Buffer for text parts during query parsing.
pub(crate) type TextPartsBuffer<'a> = Vec<&'a str>;
