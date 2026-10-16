use fff::{FilePicker, types::FileItem};
use rmcp::{model::ErrorData, schemars};
use std::collections::BinaryHeap;

#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilePatternMode {
    Fuzzy,
    Glob,
    Regex,
}

pub(crate) fn search<'a>(
    picker: &'a FilePicker,
    pattern: &str,
    mode: FilePatternMode,
    constraints: &[(String, bool)],
    filters: &[String],
    offset: usize,
    limit: usize,
) -> Result<(Vec<&'a FileItem>, usize), ErrorData> {
    let filters = filters
        .iter()
        .map(|pattern| {
            regex::Regex::new(pattern)
                .map_err(|e| ErrorData::invalid_params(format!("Invalid path regex: {e}"), None))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let regex = if matches!(mode, FilePatternMode::Glob) {
        None
    } else {
        let pattern = if matches!(mode, FilePatternMode::Fuzzy) {
            fragment_pattern(pattern)
        } else {
            pattern.to_string()
        };
        Some(
            regex::RegexBuilder::new(&pattern)
                .case_insensitive(matches!(mode, FilePatternMode::Fuzzy))
                .build()
                .map_err(|e| ErrorData::invalid_params(format!("Invalid path regex: {e}"), None))?,
        )
    };
    let mut globs = constraints
        .iter()
        .map(|(p, exclude)| crate::scope::compile_glob(p).map(|g| (g.compile_matcher(), *exclude)))
        .collect::<Result<Vec<_>, _>>()?;
    if matches!(mode, FilePatternMode::Glob) && !pattern.is_empty() {
        let pattern = if pattern.contains('/') {
            pattern.to_string()
        } else {
            format!("**/{pattern}")
        };
        globs.push((
            crate::scope::compile_glob(&pattern)?.compile_matcher(),
            false,
        ));
    }
    let files = picker.get_files();
    let mut matches = BinaryHeap::new();
    let capacity = offset.saturating_add(limit);
    let mut count = 0;
    for (index, file) in files
        .iter()
        .enumerate()
        .filter(|(_, file)| !file.is_deleted())
    {
        let path = file.relative_path(picker);
        if regex.as_ref().is_some_and(|regex| !regex.is_match(&path)) {
            continue;
        }
        if filters.iter().any(|regex| !regex.is_match(&path)) {
            continue;
        }
        if globs
            .iter()
            .any(|(glob, exclude)| glob.is_match(&path) == *exclude)
        {
            continue;
        }
        count += 1;
        let rank = if matches!(mode, FilePatternMode::Fuzzy) {
            literal_rank(&path, pattern)
        } else {
            0
        };
        let depth = path.bytes().filter(|&b| b == b'/').count();
        let key = (
            rank,
            depth,
            std::cmp::Reverse(file.total_frecency_score()),
            path,
            index,
        );
        if matches.len() < capacity {
            matches.push(key);
        } else if matches.peek().is_some_and(|worst| key < *worst) {
            *matches.peek_mut().unwrap() = key;
        }
    }
    Ok((
        matches
            .into_sorted_vec()
            .into_iter()
            .skip(offset)
            .map(|(.., index)| &files[index])
            .collect(),
        count,
    ))
}

fn literal_rank(path: &str, pattern: &str) -> u8 {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    if equal_fragment(name, pattern) || equal_fragment(stem, pattern) {
        return 0;
    }
    if path.split('/').any(|part| equal_fragment(part, pattern)) {
        return 1;
    }
    2
}

fn fragment_pattern(pattern: &str) -> String {
    let mut escaped = String::with_capacity(pattern.len());
    for (i, part) in pattern.split(['-', '_']).enumerate() {
        if i > 0 {
            escaped.push_str("[-_]");
        }
        escaped.push_str(&regex::escape(part));
    }
    escaped
}

fn equal_fragment(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left.bytes().zip(right.bytes()).all(|(a, b)| {
            a.eq_ignore_ascii_case(&b) || (matches!(a, b'-' | b'_') && matches!(b, b'-' | b'_'))
        })
}
