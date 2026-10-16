use fff_query_parser::Constraint;
use rmcp::{model::ErrorData, schemars};
use std::path::{Component, Path};

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchScope {
    /// Files or directories relative to the checkout root. Search any of these paths.
    pub paths: Option<Vec<String>>,
    /// Include glob patterns (OR). Prefix a glob with ! to exclude matching paths.
    pub globs: Option<Vec<String>>,
    /// Include hidden indexed paths (default true). Explicit paths and include globs override false.
    pub hidden: Option<bool>,
}

impl SearchScope {
    pub fn patterns(&self, picker: &fff::FilePicker) -> Result<Vec<(String, bool)>, ErrorData> {
        let base = picker.base_path();
        let mut result = Vec::new();
        let mut roots = Vec::new();
        if let Some(paths) = &self.paths {
            let mut patterns = Vec::new();
            let mut all = false;
            for path in paths {
                let path = Path::new(path);
                let path = if path.is_absolute() {
                    path.strip_prefix(base).map_err(|_| {
                        ErrorData::invalid_params("paths must be inside the indexed checkout", None)
                    })?
                } else {
                    path
                };
                if path.components().any(|c| c == Component::ParentDir) {
                    return Err(ErrorData::invalid_params(
                        "paths must not contain '..'",
                        None,
                    ));
                }
                let path = path
                    .components()
                    .filter_map(|c| match c {
                        Component::Normal(s) => Some(s.to_string_lossy()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/");
                if path.is_empty() {
                    all = true;
                } else {
                    let escaped = escape_glob(&path);
                    patterns.push(escaped.clone());
                    patterns.push(format!("{escaped}/**"));
                }
                roots.push(path);
            }
            if !all && !patterns.is_empty() {
                result.push((alternatives(&patterns), false));
            }
        }
        let mut includes = Vec::new();
        for glob in self.globs.iter().flatten() {
            let (pattern, exclude) = match glob.strip_prefix('!') {
                Some(pattern) => (pattern, true),
                None => (glob.as_str(), false),
            };
            if pattern.is_empty() {
                return Err(ErrorData::invalid_params("glob must not be empty", None));
            }
            let pattern = if let Some(rooted) = pattern.strip_prefix('/') {
                rooted.to_string()
            } else if !pattern.contains('/') {
                format!("**/{pattern}")
            } else {
                pattern.to_string()
            };
            if exclude {
                let pattern = if pattern.ends_with("/**") {
                    descendant_glob(&pattern)
                } else {
                    format!("{{{pattern},{pattern}/**}}")
                };
                result.push((pattern, true));
            } else {
                includes.push(descendant_glob(&pattern));
            }
        }
        if !includes.is_empty() {
            result.push((alternatives(&includes), false));
        }
        if self.hidden == Some(false) {
            let explicit_hidden = roots
                .iter()
                .any(|root| root.split('/').any(|part| part.starts_with('.')));
            if includes.is_empty() && !explicit_hidden {
                result.push(("{**/.*,**/.*/**}".into(), true));
            } else {
                if includes.is_empty() {
                    let mut visible = vec!["**/[!.]*".to_string()];
                    visible.extend(
                        roots
                            .iter()
                            .filter(|root| !root.is_empty())
                            .map(|root| escape_glob(root)),
                    );
                    result.push((alternatives(&visible), false));
                }
                let mut builder = globset::GlobSetBuilder::new();
                for pattern in &includes {
                    builder.add(compile_glob(pattern)?);
                }
                let whitelist = builder
                    .build()
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid glob: {e}"), None))?;
                let mut excluded = Vec::new();
                let mut overrides_hidden_directory = false;
                for dir in picker.get_dirs() {
                    let path = dir.relative_path(picker);
                    let path = path.trim_end_matches('/');
                    if !path
                        .rsplit('/')
                        .next()
                        .is_some_and(|name| name.starts_with('.'))
                    {
                        continue;
                    }
                    if whitelist.is_match(path)
                        || includes
                            .iter()
                            .any(|pattern| names_hidden_dir(pattern, path))
                        || roots.iter().any(|root| Path::new(root).starts_with(path))
                    {
                        overrides_hidden_directory = true;
                    } else {
                        excluded.push(format!("{}/**", escape_glob(path)));
                    }
                }
                if !overrides_hidden_directory {
                    result.push(("**/.*/**/?*".into(), true));
                } else if !excluded.is_empty() {
                    result.push((alternatives(&excluded), true));
                }
            }
        }
        for (pattern, _) in &result {
            compile_glob(pattern)?;
        }
        Ok(result)
    }

    pub fn is_explicit(&self) -> bool {
        self.paths.is_some() || self.globs.is_some()
    }
}

fn compile_glob(pattern: &str) -> Result<globset::Glob, ErrorData> {
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .map_err(|e| ErrorData::invalid_params(format!("Invalid glob: {e}"), None))
}

pub(crate) fn constraints(patterns: &[(String, bool)]) -> impl Iterator<Item = Constraint<'_>> {
    patterns.iter().map(|(pattern, exclude)| {
        let constraint = Constraint::Glob(pattern);
        if *exclude {
            Constraint::Not(Box::new(constraint))
        } else {
            constraint
        }
    })
}

fn alternatives(patterns: &[String]) -> String {
    if patterns.len() == 1 {
        patterns[0].clone()
    } else {
        format!("{{{}}}", patterns.join(","))
    }
}

// Character classes preserve literal names without Windows path-separator escapes.
pub(crate) fn escape_glob(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '*' | '?' | '[' | ']' | '{' | '}' | ',' | '\\' => escaped.extend(['[', c, ']']),
            _ => escaped.push(c),
        }
    }
    escaped
}

// True when a pattern spells out the hidden directory's own name, like `.github/**`.
fn names_hidden_dir(pattern: &str, dir: &str) -> bool {
    fn walk(pattern: &[&str], dir: &[&str]) -> bool {
        let (Some((&part, rest)), Some(&name)) = (pattern.split_first(), dir.first()) else {
            return false;
        };
        if part == "**" {
            return (dir.len() > 1 && walk(pattern, &dir[1..])) || walk(rest, dir);
        }
        let matches = globset::Glob::new(part)
            .map(|glob| glob.compile_matcher().is_match(name))
            .unwrap_or(false);
        match dir {
            [_] => matches && part.starts_with('.') && !rest.is_empty(),
            _ => matches && walk(rest, &dir[1..]),
        }
    }
    !pattern.contains('{')
        && walk(
            &pattern.split('/').collect::<Vec<_>>(),
            &dir.split('/').collect::<Vec<_>>(),
        )
}

pub(crate) fn descendant_glob(pattern: &str) -> String {
    // zlob's compiled matcher lets a trailing /** consume zero path components.
    if pattern.ends_with("/**") {
        format!("{pattern}/?*")
    } else {
        pattern.to_string()
    }
}
