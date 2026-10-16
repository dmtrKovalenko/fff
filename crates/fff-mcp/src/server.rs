use crate::cursor::{CursorStore, GrepCursor, GrepStage};
use crate::file_search::FilePatternMode;
use crate::output::{GrepFormatter, OutputMode, file_suffix};
use crate::scope::SearchScope;
use fff::grep::{GrepMode, GrepSearchOptions, has_regex_metacharacters};
use fff::types::{FileItem, PaginationArgs};
use fff::{FuzzySearchOptions, QueryParser, SharedFilePicker};
use fff_query_parser::{AiGrepConfig, Constraint, FFFQuery, glob_detect::has_wildcards};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const SCAN_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const MAX_CONTEXT_LINES: usize = 100;

const FUZZY_STAGE_BUDGET_MS: u64 = 2_000;

fn normalize_max_results(raw: Option<f64>, default: usize) -> usize {
    match raw {
        None => default,
        Some(v) if v <= 0.0 || !v.is_finite() => default,
        Some(v) => (v.round() as usize).clamp(1, 10_000),
    }
}

// Context lines are copied per match, so a bogus float must not saturate to usize::MAX.
fn normalize_context(raw: Option<f64>) -> Option<usize> {
    let v = raw?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    Some((v.round() as usize).min(MAX_CONTEXT_LINES))
}

fn cleanup_fuzzy_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !matches!(c, ':' | '-' | '_') {
            out.extend(c.to_lowercase());
        }
    }
    out
}

fn make_grep_options(
    output_mode: OutputMode,
    mode: GrepMode,
    file_offset: usize,
    context: Option<usize>,
) -> (GrepSearchOptions, bool) {
    let is_usage = output_mode == OutputMode::Usage;
    let matches_per_file = match output_mode {
        OutputMode::FilesWithMatches => 1,
        _ if is_usage => 8,
        _ => 10,
    };
    let ctx_lines = if is_usage {
        context.unwrap_or(1)
    } else {
        context.unwrap_or(0)
    };
    let auto_expand = !is_usage && context.is_none();
    let after_ctx = if auto_expand { 8 } else { ctx_lines };

    (
        GrepSearchOptions {
            max_file_size: 10 * 1024 * 1024,
            max_matches_per_file: matches_per_file,
            smart_case: true,
            casing: None,
            file_offset,
            page_limit: 50,
            mode,
            time_budget_ms: 0,
            enforce_time_budget: false,
            before_context: if auto_expand { 8 } else { ctx_lines },
            after_context: after_ctx,
            classify_definitions: true.into(),
            trim_whitespace: false,
            abort_signal: None,
        },
        auto_expand,
    )
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindFilesParams {
    /// Fuzzy filename or path query. Put known module constraints in path.
    // `pattern` alias for consistency with grep's alias and the common
    // file-search parameter name (#311).
    #[serde(alias = "pattern")]
    pub query: String,
    /// Directory, filename, or one glob to search. Set this from the request's module context.
    /// Examples: "src/net", "client.py", "**/*billing*client*/**".
    pub path: Option<String>,
    #[serde(flatten)]
    pub(crate) scope: SearchScope,
    /// fuzzy (default): path fragments, treating hyphens and underscores alike, then typo matches. glob or regex: exact path filtering.
    pub(crate) mode: Option<FilePatternMode>,
    /// Additional regex filters on relative paths (AND), like rg --files piped through rg.
    #[serde(default)]
    pub filters: Vec<String>,
    /// Max results (default 20).
    #[serde(rename = "maxResults")]
    // this has to be float because llms are stupid
    pub max_results: Option<f64>,
    /// Cursor from previous result. Only use if previous results weren't sufficient.
    pub cursor: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GrepParams {
    /// Content pattern. Use mode="regex" for alternatives such as deadline|timeout.
    // `pattern` alias: LLMs that have seen multi_grep (which uses `patterns`)
    // routinely call grep with `pattern`; accept it instead of erroring out
    // with an unhelpful "missing field `query`" (#311).
    #[serde(alias = "pattern")]
    pub query: String,
    /// Complete directory name, filename, or glob. Use **/*transport*/** for a partial module name.
    pub path: Option<String>,
    #[serde(flatten)]
    pub(crate) scope: SearchScope,
    /// Pattern syntax: auto (default), regex, or literal. Explicit modes preserve query verbatim.
    pub(crate) mode: Option<PatternMode>,
    /// sensitive, insensitive, or smart. Explicit pattern modes default to sensitive; auto uses smart.
    pub(crate) case: Option<CaseMode>,
    /// Max matching lines per page (default 100). More matches remain available through cursor.
    #[serde(rename = "maxResults")]
    pub max_results: Option<f64>, // this has to be float because llms are stupid
    /// Cursor from previous result. Only use if previous results weren't sufficient.
    pub cursor: Option<String>,
    /// Output format (default 'content').
    pub output_mode: Option<String>,
    /// Lines before and after each match. Omit for bounded automatic context, or use 0 for matching lines only.
    pub context: Option<f64>,
    /// Context lines before each match; overrides context (rg -B).
    pub before_context: Option<f64>,
    /// Context lines after each match; overrides context (rg -A).
    pub after_context: Option<f64>,
    /// Stop after this many matches per file (rg -m). Omit to keep all matches pageable.
    pub max_matches_per_file: Option<usize>,
    /// Filter matches to declaration names. Leave false when searching behavior, comments, or fields.
    #[serde(default)]
    pub definitions_only: bool,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PatternMode {
    Auto,
    Regex,
    Literal,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaseMode {
    Smart,
    Sensitive,
    Insensitive,
}

fn deserialize_patterns<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct PatternsVisitor;

    impl<'de> de::Visitor<'de> for PatternsVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string, an array of strings, or a stringified JSON array")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            // Try to parse as JSON array first
            if v.starts_with('[')
                && let Ok(parsed) = serde_json::from_str::<Vec<String>>(v)
            {
                return Ok(parsed);
            }
            Ok(vec![v.to_string()])
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            if v.starts_with('[')
                && let Ok(parsed) = serde_json::from_str::<Vec<String>>(&v)
            {
                return Ok(parsed);
            }
            Ok(vec![v])
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_any(PatternsVisitor)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MultiGrepParams {
    /// Patterns to match (OR logic). Include all naming conventions: snake_case, PascalCase, camelCase.
    #[serde(deserialize_with = "deserialize_patterns")]
    pub patterns: Vec<String>,
    /// Known directory, module glob, filename, or extension, e.g. "src/net/" or "**/billing/**".
    pub constraints: Option<String>,
    /// Max matching lines (default 20).
    #[serde(rename = "maxResults")]
    pub max_results: Option<f64>,
    /// Cursor from previous result.
    pub cursor: Option<String>,
    /// Output format (default 'content').
    pub output_mode: Option<String>,
    /// Context lines before/after each match.
    pub context: Option<f64>,
    /// Return matching declarations. If none are found, report that and return other matches.
    #[serde(default)]
    pub definitions_only: bool,
}

#[derive(Clone)]
pub struct FffServer {
    picker: SharedFilePicker,
    cursor_store: Arc<Mutex<CursorStore>>,
    update_notice_sent: Arc<AtomicBool>,
    last_activity: Arc<AtomicU64>,
    scan_ready: Arc<AtomicBool>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl FffServer {
    pub fn new(picker: SharedFilePicker) -> Self {
        Self {
            picker,
            cursor_store: Arc::new(Mutex::new(CursorStore::new())),
            update_notice_sent: Arc::new(AtomicBool::new(false)),
            last_activity: Arc::new(AtomicU64::new(now_secs())),
            scan_ready: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn last_activity(&self) -> Arc<AtomicU64> {
        self.last_activity.clone()
    }

    fn bump_activity(&self) {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
    }

    fn wait_for_scan(&self, timeout: std::time::Duration) -> Result<(), ErrorData> {
        if self.scan_ready.load(Ordering::Relaxed) {
            return Ok(());
        }

        let deadline = std::time::Instant::now() + timeout;

        loop {
            let is_scanning = self
                .picker
                .read()
                .ok()
                .as_ref()
                .and_then(|g| g.as_ref())
                .map(|p| p.is_scan_active())
                .unwrap_or(true);

            if !is_scanning {
                self.scan_ready.store(true, Ordering::Relaxed);
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(ErrorData::internal_error(
                    "Index is still building; retry shortly",
                    None,
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn lock_cursors(&self) -> Result<std::sync::MutexGuard<'_, CursorStore>, ErrorData> {
        self.cursor_store.lock().map_err(|e| {
            ErrorData::internal_error(format!("Failed to acquire cursor store lock: {e}"), None)
        })
    }

    fn maybe_append_update_notice(&self, result: &mut CallToolResult) {
        if self.update_notice_sent.swap(true, Ordering::Relaxed) {
            return;
        }
        let notice = crate::update_check::get_update_notice();
        if notice.is_empty() {
            // Reset so the next call can try again (check may still be in flight)
            self.update_notice_sent.store(false, Ordering::Relaxed);
            return;
        }
        result.content.push(Content::text(notice));
    }

    fn perform_grep(
        &self,
        params: &GrepParams,
        mode: GrepMode,
    ) -> Result<CallToolResult, ErrorData> {
        let query = params.query.as_str();
        let max_results = normalize_max_results(params.max_results, 100);
        let output_mode = OutputMode::new(params.output_mode.as_deref());
        let context = normalize_context(params.context);
        let mut cursor = params
            .cursor
            .as_deref()
            .and_then(|id| self.cursor_store.lock().ok()?.get_grep(id))
            .unwrap_or(GrepCursor {
                stage: if params.definitions_only {
                    GrepStage::Definitions
                } else {
                    GrepStage::Normal
                },
                ..Default::default()
            });
        let prefer_definitions = !matches!(cursor.stage, GrepStage::Normal | GrepStage::Broadened);
        let broadened_query = (cursor.stage == GrepStage::Broadened).then(|| {
            query
                .split_whitespace()
                .skip(1)
                .collect::<Vec<_>>()
                .join(" ")
        });

        let (mut options, mut auto_expand) =
            make_grep_options(output_mode, mode, cursor.file_offset, context);
        options.page_limit = cursor.page_limit(max_results);
        options.max_matches_per_file = if output_mode == OutputMode::FilesWithMatches {
            1
        } else {
            params
                .max_matches_per_file
                .filter(|n| *n > 0)
                .unwrap_or(usize::MAX)
                .min(options.page_limit)
        };
        options.casing = params
            .case
            .or_else(|| {
                matches!(params.mode, Some(PatternMode::Regex | PatternMode::Literal))
                    .then_some(CaseMode::Sensitive)
            })
            .map(|case| match case {
                CaseMode::Smart => fff::grep::Casing::Smart,
                CaseMode::Sensitive => fff::grep::Casing::Sensitive,
                CaseMode::Insensitive => fff::grep::Casing::Insensitive,
            });
        if params.before_context.is_some() || params.after_context.is_some() {
            options.before_context = normalize_context(params.before_context)
                .or(context)
                .unwrap_or(0);
            options.after_context = normalize_context(params.after_context)
                .or(context)
                .unwrap_or(0);
            auto_expand = false;
        }
        let ctx_lines = options.before_context;
        let show_context = !auto_expand && (ctx_lines > 0 || options.after_context > 0);

        // Acquire picker lock once for the entire operation.
        let guard = self.picker.read().map_err(|e| {
            ErrorData::internal_error(format!("Failed to acquire picker lock: {e}"), None)
        })?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("File picker not initialized", None))?;

        let parser = QueryParser::new(AiGrepConfig);
        let path_scope = path_scope(params.path.as_deref(), picker)?;
        let scope = path_scope.as_ref().map(|scope| scope.glob.as_str());
        let patterns = params.scope.patterns(picker)?;
        let verbatim = params.scope.is_explicit()
            || matches!(params.mode, Some(PatternMode::Regex | PatternMode::Literal));
        let mut original = if verbatim {
            FFFQuery {
                raw_query: "",
                fuzzy_query: fff_query_parser::FuzzyQuery::Text(
                    if query.is_empty() && mode == GrepMode::Regex {
                        "^"
                    } else {
                        query
                    },
                ),
                constraints: scope.map(Constraint::Glob).into_iter().collect(),
                location: None,
            }
        } else {
            parse_scoped_grep(&parser, query, scope)
        };
        original
            .constraints
            .extend(crate::scope::constraints(&patterns));
        if params.scope.is_explicit() {
            original.raw_query = "";
        }
        if let Some(shorter) = &broadened_query {
            original.fuzzy_query = parser.parse(shorter).fuzzy_query;
            original.raw_query = "";
        }
        let mode = if broadened_query.is_some() && has_regex_metacharacters(&original.grep_text()) {
            GrepMode::Regex
        } else {
            mode
        };
        let fresh = cursor.stage == GrepStage::Definitions && cursor.file_offset == 0;
        let missing_path = file_path_constraint(&original)
            .filter(|path| cursor.missing_path || (fresh && !indexes_file_path(picker, path)));
        if missing_path.is_some() {
            original
                .constraints
                .retain(|c| !matches!(c, Constraint::FilePath(_)));
            cursor.missing_path = true;
        }
        let path_notice = path_scope
            .as_ref()
            .and_then(|scope| scope.notice.clone())
            .or_else(|| {
                missing_path.map(|path| {
                    format!("No indexed file matches '{path}'. Searched all files instead.")
                })
            });
        let respond = |text: String, definitions_fallback: bool| {
            with_notice(
                grep_response(text, definitions_fallback),
                path_notice.clone(),
            )
        };
        let fuzzy_query = prefer_definitions.then(|| cleanup_fuzzy_query(&original.grep_text()));
        let mut fuzzy = None;
        let mut result = loop {
            options.classify_definitions = cursor.stage.definition_mode();
            options.file_offset = cursor.file_offset;
            options.mode = if cursor.stage.is_fuzzy() {
                GrepMode::Fuzzy
            } else {
                mode
            };
            // Fuzzy stages read every candidate file, so bound each one on large indexes.
            (options.time_budget_ms, options.enforce_time_budget) = if cursor.stage.is_fuzzy() {
                (FUZZY_STAGE_BUDGET_MS, true)
            } else {
                (0, false)
            };
            let parsed = if cursor.stage.is_fuzzy() {
                fuzzy.get_or_insert_with(|| {
                    let mut parsed =
                        parse_scoped_grep(&parser, fuzzy_query.as_deref().unwrap_or(query), scope);
                    parsed.constraints = original.constraints.clone();
                    parsed.raw_query = "";
                    parsed
                })
            } else {
                &original
            };
            let mut result = picker.grep(parsed, &options);
            if matches!(params.mode, Some(PatternMode::Regex))
                && let Some(error) = &result.regex_fallback_error
            {
                return Err(ErrorData::invalid_params(
                    format!(
                        "Invalid regex: {error}. Use mode=literal to search this text literally."
                    ),
                    None,
                ));
            }
            if prefer_definitions && result.literal_fallback {
                // A literal retry drops exclusions, so treat it as an empty stage.
                result.matches.clear();
                result.files.clear();
                result.next_file_offset = 0;
                result.literal_fallback = false;
            }
            if cursor.stage.is_fuzzy() && result.matches.is_empty() {
                // A budget spent without matches ends the stage like an empty full search.
                result.next_file_offset = 0;
            }
            if result.matches.is_empty()
                && result.next_file_offset == 0
                && !cursor.found_matches
                && let Some(next) = cursor.stage.next()
            {
                cursor.stage = next;
                cursor.file_offset = 0;
                continue;
            }
            break result;
        };
        if !prefer_definitions
            && result.matches.is_empty()
            && result.next_file_offset == 0
            && let Some(scope) = scope
            && original.constraints.len() > 1
            && !query.trim().is_empty()
        {
            // Core's literal retry would drop the path scope, so retry inside it here.
            let literal = FFFQuery {
                raw_query: "",
                constraints: std::iter::once(Constraint::Glob(scope)).collect(),
                fuzzy_query: fff_query_parser::FuzzyQuery::Text(query.trim()),
                location: None,
            };
            let retry = picker.grep(&literal, &options);
            if !retry.matches.is_empty() {
                result = retry;
                result.literal_fallback = true;
            }
        }

        if cursor.stage == GrepStage::Normal
            && result.matches.is_empty()
            && result.next_file_offset == 0
            && options.file_offset == 0
        {
            // Auto-retry: try broadening multi-word queries by dropping first non-constraint word
            let parts: Vec<&str> = query.split_whitespace().collect();
            if !verbatim && parts.len() >= 2 {
                let first_word = parts[0];
                let is_valid_constraint = first_word.starts_with('!')
                    || first_word.starts_with('*')
                    || first_word.ends_with('/');

                if !is_valid_constraint {
                    let rest_query = parts[1..].join(" ");
                    let mut rest_parsed = parse_scoped_grep(&parser, &rest_query, scope);
                    rest_parsed.constraints = original.constraints.clone();
                    rest_parsed.raw_query = "";

                    let rest_text = rest_parsed.grep_text();
                    let retry_mode = if has_regex_metacharacters(&rest_text) {
                        GrepMode::Regex
                    } else {
                        mode
                    };

                    let mut retry_options = options.clone();
                    retry_options.mode = retry_mode;
                    retry_options.file_offset = 0;
                    let mut retry_result = picker.grep(&rest_parsed, &retry_options);

                    if !retry_result.matches.is_empty() && retry_result.matches.len() <= 10 {
                        cursor.stage = GrepStage::Broadened;
                        cursor.paginate(&mut retry_result, max_results, picker);
                        let mut cs = self.lock_cursors()?;
                        let text = &GrepFormatter {
                            matches: &retry_result.matches,
                            files: &retry_result.files,
                            total_matched: retry_result.matches.len(),
                            cursor,
                            output_mode,
                            max_results,
                            show_context,
                            auto_expand_defs: auto_expand,
                            picker,
                        }
                        .format(&mut cs);
                        return Ok(respond(
                            format!(
                                "0 matches for '{}'. Auto-broadened to '{}':\n{}",
                                query, rest_query, text
                            ),
                            false,
                        ));
                    }
                }
            }

            // Fuzzy fallback for typo tolerance
            let fuzzy_query = cleanup_fuzzy_query(&original.grep_text());
            let (mut fuzzy_options, _) =
                make_grep_options(output_mode, GrepMode::Fuzzy, 0, Some(0));
            fuzzy_options.classify_definitions = options.classify_definitions;
            fuzzy_options.casing = options.casing;
            let mut fuzzy_parsed = parse_scoped_grep(&parser, &fuzzy_query, scope);
            fuzzy_parsed.constraints = original.constraints.clone();
            fuzzy_parsed.raw_query = "";
            let mut fuzzy_result = if original
                .grep_text()
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '-'))
            {
                picker.grep(&fuzzy_parsed, &fuzzy_options)
            } else {
                fff::GrepResult::default()
            };
            fuzzy_result
                .matches
                .retain(|m| strong_suggestion(m, fuzzy_query.len()));
            fuzzy_result
                .matches
                .sort_by_key(|m| std::cmp::Reverse(m.fuzzy_score));

            if !fuzzy_result.matches.is_empty() {
                let mut lines: Vec<String> = Vec::new();
                lines.push(format!(
                    "0 exact matches. {} approximate:",
                    fuzzy_result.matches.len()
                ));
                let mut current_file = String::new();
                for m in fuzzy_result.matches.iter().take(3) {
                    let file = fuzzy_result.files[m.file_index];
                    let file_rel = file.relative_path(picker);
                    if file_rel != current_file {
                        current_file = file_rel;
                        lines.push(current_file.to_string());
                    }
                    lines.push(format!(" {}: {}", m.line_number, m.line_content));
                }
                return Ok(respond(lines.join("\n"), false));
            }

            // File path fallback: if query looks like a path, suggest the matching file
            if query.contains('/') {
                let file_parser = QueryParser::default();
                let mut file_query = file_parser.parse(query);
                file_query.constraints.extend(scope.map(Constraint::Glob));
                file_query
                    .constraints
                    .extend(crate::scope::constraints(&patterns));
                let file_opts = FuzzySearchOptions {
                    max_threads: 0,
                    current_file: None,
                    project_path: Some(picker.base_path()),
                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: 1,
                    },
                };
                let file_result = picker.fuzzy_search(&file_query, None, file_opts);
                if let (Some(top), Some(score)) =
                    (file_result.items.first(), file_result.scores.first())
                {
                    // Only suggest when the match is strong enough.
                    let query_len = query.len() as i32;
                    if score.base_score > query_len * 10 {
                        return Ok(respond(
                            format!(
                                "0 content matches. But there is a relevant file path: {}",
                                top.relative_path(picker)
                            ),
                            false,
                        ));
                    }
                }
            }

            return Ok(respond(
                suggest_other_files(
                    picker,
                    &original,
                    mode,
                    options.effective_casing(),
                    params.path.as_deref(),
                )
                .unwrap_or_else(|| "0 matches.".into()),
                false,
            ));
        }

        if result.matches.is_empty() && result.next_file_offset == 0 {
            if prefer_definitions
                && !cursor.found_matches
                && let Some(suggestions) = suggest_other_files(
                    picker,
                    &original,
                    mode,
                    options.effective_casing(),
                    params.path.as_deref(),
                )
            {
                return Ok(respond(suggestions, false));
            }
            return Ok(respond(
                "0 matches.".into(),
                prefer_definitions && !cursor.found_matches,
            ));
        }

        let notice = if !cursor.found_matches || cursor.stage.is_fuzzy() {
            cursor.stage.notice()
        } else {
            None
        };
        cursor.paginate(&mut result, max_results, picker);
        if result.matches.is_empty() && !cursor.has_more() {
            return Ok(respond("0 matches.".into(), false));
        }

        let mut cs = self.lock_cursors()?;
        let text = GrepFormatter {
            matches: &result.matches,
            files: &result.files,
            total_matched: result.matches.len(),
            cursor,
            output_mode,
            max_results,
            show_context,
            auto_expand_defs: auto_expand,
            picker,
        }
        .format(&mut cs);

        let text = match (broadened_query, notice) {
            (Some(shorter), _) => {
                format!("0 matches for '{query}'. Auto-broadened to '{shorter}':\n{text}")
            }
            (_, Some(notice)) => format!("{notice}\n{text}"),
            _ => text,
        };
        Ok(respond(text, false))
    }
}

#[tool_router]
impl FffServer {
    #[tool(
        name = "find_files",
        description = "Find indexed files by path fragment, glob, or regex. Fragments treat hyphens and underscores alike before fuzzy suggestions. Put a known directory in path. An empty query lists files in scope.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn find_files(
        &self,
        Parameters(params): Parameters<FindFilesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.bump_activity();
        self.wait_for_scan(SCAN_READY_TIMEOUT)?;

        let max_results = normalize_max_results(params.max_results, 20);
        let query = &params.query;

        let page_offset = params
            .cursor
            .as_deref()
            .and_then(|id| self.cursor_store.lock().ok()?.get(id))
            .unwrap_or(0);

        let guard = self.picker.read().map_err(|e| {
            ErrorData::internal_error(format!("Failed to acquire picker lock: {e}"), None)
        })?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("File picker not initialized", None))?;
        let base_path = picker.base_path();
        let make_opts = |offset: usize| FuzzySearchOptions {
            max_threads: 0,
            current_file: None,
            project_path: Some(base_path),
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,
            pagination: PaginationArgs {
                offset,
                limit: max_results,
            },
        };

        let parser = QueryParser::default();
        let path_scope = path_scope(params.path.as_deref(), picker)?;
        let scope = path_scope.as_ref().map(|scope| scope.glob.as_str());
        let notice = path_scope.as_ref().and_then(|scope| scope.notice.clone());
        let mut fff_query = parser.parse(query);
        let mut patterns = params.scope.patterns(picker)?;
        if let Some(scope) = scope {
            patterns.push((scope.to_owned(), false));
        }
        let explicit_mode = matches!(
            params.mode,
            Some(FilePatternMode::Glob | FilePatternMode::Regex)
        );
        if explicit_mode || !params.filters.is_empty() || fff_query.constraints.is_empty() {
            let (items, total) = crate::file_search::search(
                picker,
                query,
                params.mode.unwrap_or(FilePatternMode::Fuzzy),
                &patterns,
                &params.filters,
                page_offset,
                max_results,
            )?;
            if explicit_mode || !params.filters.is_empty() || total > 0 {
                let mut lines = items
                    .iter()
                    .map(|file| file.relative_path(picker))
                    .collect::<Vec<_>>();
                if total == 0 {
                    lines.push(format!("0 matching paths ({} indexed). Globs cannot include files outside the index. Try a broader path or mode=fuzzy for filename suggestions.", picker.live_file_count()));
                } else if page_offset + items.len() < total {
                    lines.insert(0, format!("{}/{} paths", items.len(), total));
                    let id = self.lock_cursors()?.store(page_offset + items.len());
                    lines.push(format!("cursor: {id}"));
                }
                let result = CallToolResult::success(vec![Content::text(lines.join("\n"))]);
                return Ok(with_notice(result, notice));
            }
        }
        fff_query
            .constraints
            .extend(crate::scope::constraints(&patterns));
        let result = picker.fuzzy_search(&fff_query, None, make_opts(page_offset));
        let total_files = result.total_files;

        // Auto-retry with fewer terms if 3+ words return 0 results
        let words: Vec<&str> = query.split_whitespace().collect();
        let shorter = words.get(..2).map(|w| w.join(" "));

        let (items, total_matched) =
            if result.items.is_empty() && words.len() >= 3 && page_offset == 0 {
                if let Some(shorter) = &shorter {
                    let mut shorter_query = parser.parse(shorter);
                    shorter_query.constraints = fff_query.constraints.clone();
                    let retry = picker.fuzzy_search(&shorter_query, None, make_opts(0));

                    (retry.items, retry.total_matched)
                } else {
                    (result.items, result.total_matched)
                }
            } else {
                (result.items, result.total_matched)
            };

        if items.is_empty() {
            let text = format!("0 results ({} indexed)", total_files);
            return Ok(with_notice(
                CallToolResult::success(vec![Content::text(text)]),
                notice,
            ));
        }

        let mut lines: Vec<String> = vec!["Approximate filename matches:".into()];

        let next_offset = page_offset + items.len();
        let has_more = next_offset < total_matched;

        if has_more {
            lines.push(format!("{}/{} matches", items.len(), total_matched));
        }

        for item in &items {
            lines.push(format!(
                "{}{}",
                item.relative_path(picker),
                file_suffix(item.git_status, item.total_frecency_score())
            ));
        }

        if has_more {
            let mut cs = self.lock_cursors()?;
            let cursor_id = cs.store(next_offset);
            lines.push(format!("cursor: {}", cursor_id));
        }

        let mut result = CallToolResult::success(vec![Content::text(lines.join("\n"))]);
        self.maybe_append_update_notice(&mut result);
        Ok(result)
    }

    #[tool(
        name = "grep",
        description = "Search source code with line numbers and surrounding context. For content questions, search directly with a module scope in path. Returned source verifies locations when it contains the required evidence.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn grep(
        &self,
        Parameters(mut params): Parameters<GrepParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.bump_activity();
        self.wait_for_scan(SCAN_READY_TIMEOUT)?;

        let parsed = QueryParser::new(AiGrepConfig).parse(&params.query);
        let grep_text = parsed.grep_text();

        let mode = match params.mode {
            Some(PatternMode::Regex) => GrepMode::Regex,
            // Plain grep reads `\n` as a newline, so keep literal backslashes in a regex.
            Some(PatternMode::Literal) if params.query.contains("\\n") => {
                params.query = literal_regex(&params.query);
                GrepMode::Regex
            }
            Some(PatternMode::Literal) => GrepMode::PlainText,
            _ if has_regex_metacharacters(&grep_text) => GrepMode::Regex,
            _ => GrepMode::PlainText,
        };

        let mut result = self.perform_grep(&params, mode)?;
        self.maybe_append_update_notice(&mut result);
        Ok(result)
    }

    /// Search file contents for lines matching ANY of multiple patterns (OR logic).
    /// Patterns are literal text — NEVER escape special characters.
    #[tool(
        name = "multi_grep",
        description = "Search for any of several literal identifiers in one call. Set constraints to the directory, module, filename, or glob from the request. Set definitions_only=true for declarations. Results contain source lines and line numbers.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn multi_grep(
        &self,
        Parameters(params): Parameters<MultiGrepParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.bump_activity();
        self.wait_for_scan(SCAN_READY_TIMEOUT)?;

        let mut result = self.multi_grep_inner(params)?;
        self.maybe_append_update_notice(&mut result);
        Ok(result)
    }
}

impl FffServer {
    fn multi_grep_inner(&self, params: MultiGrepParams) -> Result<CallToolResult, ErrorData> {
        let max_results = normalize_max_results(params.max_results, 20);
        let context = normalize_context(params.context);
        let output_mode = OutputMode::new(params.output_mode.as_deref());

        let mut cursor = params
            .cursor
            .as_deref()
            .and_then(|id| self.cursor_store.lock().ok()?.get_grep(id))
            .unwrap_or(GrepCursor {
                stage: if params.definitions_only {
                    GrepStage::Definitions
                } else {
                    GrepStage::Normal
                },
                ..Default::default()
            });

        let (mut options, auto_expand) = make_grep_options(
            output_mode,
            GrepMode::PlainText,
            cursor.file_offset,
            context,
        );
        options.page_limit = cursor.page_limit(max_results);
        options.max_matches_per_file = if output_mode == OutputMode::FilesWithMatches {
            1
        } else {
            options.page_limit
        };
        if cursor.stage == GrepStage::Definitions {
            options.classify_definitions = fff::DefinitionMode::DefinitionOnly;
        }

        let ctx_lines = options.before_context;
        let constraint_query = params.constraints.as_deref().unwrap_or("");
        let guard = self.picker.read().map_err(|e| {
            ErrorData::internal_error(format!("Failed to acquire picker lock: {e}"), None)
        })?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("File picker not initialized", None))?;
        let patterns_refs: Vec<&str> = params.patterns.iter().map(|s| s.as_str()).collect();

        let parser = QueryParser::new(AiGrepConfig);
        let constraints = parser.parse_constraints(constraint_query);

        let mut result = picker.multi_grep(&patterns_refs, &constraints, &options);
        let definitions_fallback = cursor.stage == GrepStage::Definitions
            && !cursor.found_matches
            && result.matches.is_empty()
            && result.next_file_offset == 0;
        if definitions_fallback {
            cursor = GrepCursor {
                stage: GrepStage::References,
                ..Default::default()
            };
            options.file_offset = 0;
            options.classify_definitions = fff::DefinitionMode::Classify;
            result = picker.multi_grep(&patterns_refs, &constraints, &options);
        }
        let file_refs: Vec<&FileItem> = result.files.to_vec();

        if result.matches.is_empty() && result.next_file_offset == 0 {
            return Ok(grep_response("0 matches.".into(), definitions_fallback));
        }

        cursor.paginate(&mut result, max_results, picker);
        if result.matches.is_empty() && !cursor.has_more() {
            return Ok(grep_response("0 matches.".into(), false));
        }
        let mut cs = self.lock_cursors()?;
        let text = GrepFormatter {
            matches: &result.matches,
            files: &file_refs,
            total_matched: result.matches.len(),
            cursor,
            output_mode,
            max_results,
            show_context: !auto_expand && ctx_lines > 0,
            auto_expand_defs: auto_expand,
            picker,
        }
        .format(&mut cs);

        Ok(grep_response(text, definitions_fallback))
    }
}

#[tool_handler]
impl ServerHandler for FffServer {
    fn get_info(&self) -> ServerInfo {
        let notice = crate::update_check::get_update_notice();
        let instructions = if notice.is_empty() {
            crate::MCP_INSTRUCTIONS.to_string()
        } else {
            format!("{}{}", crate::MCP_INSTRUCTIONS, notice)
        };

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("fff", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions)
    }
}

fn grep_response(text: String, definitions_fallback: bool) -> CallToolResult {
    let text = if definitions_fallback && text == "0 matches." {
        "No matching definitions or other matches found.".to_string()
    } else if definitions_fallback {
        format!("No matching definitions found. Showing other matches:\n{text}")
    } else {
        text
    };
    CallToolResult::success(vec![Content::text(text)])
}

fn literal_regex(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            // `[\\]` matches a backslash without spelling the `\n` escape core rewrites.
            '\\' => "[\\\\]".to_string(),
            c => regex::escape(c.encode_utf8(&mut [0; 4])),
        })
        .collect()
}

fn with_notice(mut response: CallToolResult, notice: Option<String>) -> CallToolResult {
    if let Some(notice) = notice
        && let Some(RawContent::Text(text)) = response.content.first_mut().map(|c| &mut c.raw)
    {
        text.text = format!("{notice}\n{}", text.text);
    }
    response
}

fn file_path_constraint<'a>(query: &FFFQuery<'a>) -> Option<&'a str> {
    query.constraints.iter().find_map(|c| match c {
        Constraint::FilePath(path) => Some(*path),
        _ => None,
    })
}

// Case-insensitive like core's file-path constraint, so `readme.md` finds README.md.
fn indexes_file_path(picker: &fff::FilePicker, suffix: &str) -> bool {
    let suffix = suffix.replace('\\', "/").to_ascii_lowercase();
    picker.get_files().iter().any(|file| {
        let path = file
            .relative_path(picker)
            .replace('\\', "/")
            .to_ascii_lowercase();
        !file.is_deleted()
            && path
                .strip_suffix(suffix.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.ends_with('/'))
    })
}

fn parse_scoped_grep<'a>(
    parser: &QueryParser<AiGrepConfig>,
    query: &'a str,
    scope: Option<&'a str>,
) -> FFFQuery<'a> {
    let mut parsed = parser.parse(query);
    if let Some(scope) = scope {
        parsed.constraints.push(Constraint::Glob(scope));
        // A separate path must survive retries that drop ambiguous inline constraints.
        parsed.raw_query = "";
    }
    parsed
}

fn suggest_other_files(
    picker: &fff::FilePicker,
    query: &FFFQuery<'_>,
    mode: GrepMode,
    casing: fff::grep::Casing,
    path: Option<&str>,
) -> Option<String> {
    let mut broader = query.clone();
    broader.raw_query = "";
    broader
        .constraints
        .retain(|c| matches!(c, Constraint::Exclude(_) | Constraint::Not(_)));
    if broader.constraints.len() < query.constraints.len() {
        let started = std::time::Instant::now();
        let mut options = GrepSearchOptions {
            mode,
            casing: Some(casing),
            max_matches_per_file: 3,
            page_limit: 8,
            before_context: 8,
            after_context: 8,
            classify_definitions: true.into(),
            trim_whitespace: false,
            time_budget_ms: 100,
            enforce_time_budget: true,
            ..Default::default()
        };
        if let Some(fragment) = path.map(str::trim).filter(|path| {
            !path.is_empty()
                && path
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        }) {
            let glob = format!("{{**/*{fragment}*,**/*{fragment}*/**}}");
            let mut nearby = broader.clone();
            nearby.constraints.push(Constraint::Glob(&glob));
            let result = picker.grep(&nearby, &options);
            if !result.matches.is_empty() {
                return Some(format_broader_matches(
                    picker,
                    &result,
                    &format!(
                        "No matches in the requested scope. Suggestions from paths containing '{fragment}' (broader scope):"
                    ),
                ));
            }
        }
        options.time_budget_ms = 100_u64.saturating_sub(started.elapsed().as_millis() as u64);
        if options.time_budget_ms > 0 {
            let result = picker.grep(&broader, &options);
            if !result.matches.is_empty() {
                return Some(format_broader_matches(
                    picker,
                    &result,
                    "No matches in the requested scope. Suggestions elsewhere in the indexed project:",
                ));
            }
        }
    }
    let text = cleanup_fuzzy_query(&query.grep_text());
    if text.is_empty() {
        return None;
    }
    broader.fuzzy_query = fff_query_parser::FuzzyQuery::Text(&text);
    let result = picker.fuzzy_search(
        &broader,
        None,
        FuzzySearchOptions {
            pagination: PaginationArgs {
                offset: 0,
                limit: 3,
            },
            ..Default::default()
        },
    );
    let files = result
        .items
        .iter()
        .zip(&result.scores)
        .filter(|(_, score)| score.base_score > text.len() as i32 * 10)
        .map(|(file, _)| file.relative_path(picker))
        .collect::<Vec<_>>();
    (!files.is_empty()).then(|| format!("No matching definitions or other matches found in the requested scope. These filenames may be relevant; their contents are unverified:\n{}", files.join("\n")))
}

fn format_broader_matches(
    picker: &fff::FilePicker,
    result: &fff::GrepResult<'_>,
    notice: &str,
) -> String {
    let text = GrepFormatter {
        matches: &result.matches,
        files: &result.files,
        total_matched: result.matches.len(),
        cursor: GrepCursor::default(),
        output_mode: OutputMode::Content,
        max_results: 8,
        show_context: false,
        auto_expand_defs: true,
        picker,
    }
    .format(&mut CursorStore::new());
    format!("{notice}\n{text}\nSuggestions are limited; search a suggested path for more matches.")
}

fn strong_suggestion(m: &fff::GrepMatch, needle_len: usize) -> bool {
    let Some((start, _)) = m.match_byte_offsets.first() else {
        return false;
    };
    let Some((_, end)) = m.match_byte_offsets.last() else {
        return false;
    };
    let matched: usize = m
        .match_byte_offsets
        .iter()
        .map(|(start, end)| (end - start) as usize)
        .sum();
    let typos = if needle_len < 9 { 1 } else { 2 };
    matched + typos >= needle_len && (*end - *start) as usize <= needle_len + 2
}

#[derive(Debug, PartialEq)]
struct PathScope {
    glob: String,
    notice: Option<String>,
}

fn path_scope(
    path: Option<&str>,
    picker: &fff::FilePicker,
) -> Result<Option<PathScope>, ErrorData> {
    let Some(raw) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    let given = std::path::Path::new(raw);
    let absolute = given.is_absolute();
    let path = if absolute {
        strip_root(given, picker.base_path())?
    } else {
        given.to_path_buf()
    };
    if path
        .components()
        .any(|part| part == std::path::Component::ParentDir)
    {
        return Err(ErrorData::invalid_params(
            "path must not contain '..' components",
            None,
        ));
    }
    let path = path
        .to_str()
        .ok_or_else(|| ErrorData::invalid_params("path must be UTF-8", None))?;
    #[cfg(windows)]
    let normalized = path.replace('\\', "/");
    #[cfg(windows)]
    let path = normalized.as_str();
    let path = path
        .strip_prefix("./")
        .unwrap_or(path)
        .trim_end_matches('/');
    if path.is_empty() || path == "." {
        return Ok(None);
    }
    let escaped = crate::scope::escape_glob(path);
    let scope = |glob: String, found: bool| PathScope {
        glob,
        notice: (!found).then(|| format!("Path '{raw}' matches no indexed file or directory.")),
    };
    let found = lookup_path(picker, path);
    if absolute || found.anchored {
        return Ok(Some(scope(
            format!("{{{escaped},{escaped}/**}}"),
            found.anchored,
        )));
    }
    // A literal name such as `src` or `[id]` at any depth wins over reading it as a glob.
    if found.nested || !has_wildcards(path) {
        return Ok(Some(scope(
            format!("{{**/{escaped},**/{escaped}/**}}"),
            found.nested,
        )));
    }
    Ok(Some(scope(crate::scope::descendant_glob(path), true)))
}

fn strip_root(
    path: &std::path::Path,
    base: &std::path::Path,
) -> Result<std::path::PathBuf, ErrorData> {
    if let Ok(relative) = path.strip_prefix(base) {
        return Ok(relative.to_path_buf());
    }
    // Symlinked or short-name spellings of the root still name the checkout; missing tails are kept.
    let (existing, real) = path
        .ancestors()
        .find_map(|dir| Some((dir, std::fs::canonicalize(dir).ok()?)))
        .ok_or_else(|| {
            ErrorData::invalid_params("path must be inside the indexed checkout", None)
        })?;
    let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    real.strip_prefix(&base)
        .ok()
        .zip(path.strip_prefix(existing).ok())
        .map(|(inside, missing)| inside.join(missing))
        .ok_or_else(|| ErrorData::invalid_params("path must be inside the indexed checkout", None))
}

#[derive(Default)]
struct PathLookup {
    anchored: bool,
    nested: bool,
}

// One pass over the index: an exact checkout path wins, otherwise note a match at any depth.
fn lookup_path(picker: &fff::FilePicker, path: &str) -> PathLookup {
    let mut found = PathLookup::default();
    let dirs = picker
        .get_dirs()
        .iter()
        .map(|dir| dir.relative_path(picker));
    let files = picker
        .get_files()
        .iter()
        .filter(|file| !file.is_deleted())
        .map(|file| file.relative_path(picker));
    for candidate in dirs.chain(files) {
        #[cfg(windows)]
        let candidate = candidate.replace('\\', "/");
        match candidate.trim_end_matches('/').strip_suffix(path) {
            Some("") => {
                found.anchored = true;
                break;
            }
            Some(rest) if rest.ends_with('/') => found.nested = true,
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_max_results_none_uses_default() {
        assert_eq!(normalize_max_results(None, 20), 20);
    }

    #[test]
    fn normalize_max_results_zero_uses_default() {
        // Issue #400: `maxResults: 0` must not return zero items for grep
        // while `find_files` returns the full set. Both tools now map 0 to
        // the default limit.
        assert_eq!(normalize_max_results(Some(0.0), 20), 20);
    }

    #[test]
    fn normalize_max_results_negative_uses_default() {
        assert_eq!(normalize_max_results(Some(-5.0), 20), 20);
    }

    #[test]
    fn normalize_max_results_non_finite_uses_default() {
        assert_eq!(normalize_max_results(Some(f64::NAN), 20), 20);
        assert_eq!(normalize_max_results(Some(f64::INFINITY), 20), 20);
    }

    #[test]
    fn normalize_max_results_rounds_and_clamps() {
        assert_eq!(normalize_max_results(Some(0.4), 20), 1);
        assert_eq!(normalize_max_results(Some(10.0), 20), 10);
        assert_eq!(normalize_max_results(Some(10.7), 20), 11);
        assert_eq!(normalize_max_results(Some(1e308), 20), 10_000);
    }

    #[test]
    fn grep_params_accepts_pattern_alias() {
        // Issue #311: LLMs flip between `query` and `pattern`; accept both.
        let via_query: GrepParams =
            serde_json::from_str(r#"{"query":"foo"}"#).expect("query field");
        assert_eq!(via_query.query, "foo");
        assert!(via_query.path.is_none());

        let via_pattern: GrepParams =
            serde_json::from_str(r#"{"pattern":"foo"}"#).expect("pattern alias");
        assert_eq!(via_pattern.query, "foo");
    }

    #[test]
    fn normalize_context_rejects_bogus_and_caps() {
        assert_eq!(normalize_context(None), None);
        assert_eq!(normalize_context(Some(3.4)), Some(3));
        assert_eq!(normalize_context(Some(-1.0)), None);
        assert_eq!(normalize_context(Some(f64::NAN)), None);
        assert_eq!(normalize_context(Some(1e308)), Some(MAX_CONTEXT_LINES));
    }

    #[test]
    fn grep_params_parses_context() {
        let params: GrepParams =
            serde_json::from_str(r#"{"query":"foo","context":3}"#).expect("context field");
        assert_eq!(params.context, Some(3.0));
    }

    #[test]
    fn find_files_params_accepts_pattern_alias() {
        let via_query: FindFilesParams =
            serde_json::from_str(r#"{"query":"foo"}"#).expect("query field");
        assert_eq!(via_query.query, "foo");
        assert!(via_query.path.is_none());

        let via_pattern: FindFilesParams =
            serde_json::from_str(r#"{"pattern":"foo"}"#).expect("pattern alias");
        assert_eq!(via_pattern.query, "foo");
    }

    #[test]
    fn agent_schemas_expose_only_the_definition_flag() {
        for schema in [
            schemars::schema_for!(GrepParams),
            schemars::schema_for!(MultiGrepParams),
        ] {
            let value = serde_json::to_value(schema).unwrap();
            let properties = &value["properties"];
            assert_eq!(properties["definitions_only"]["type"], "boolean");
            assert_eq!(properties["definitions_only"]["default"], false);
            assert!(properties.get("classifyDefinitions").is_none());
            assert!(properties.get("classify_definitions").is_none());
        }
    }

    #[test]
    fn definition_flag_returns_declarations_or_notified_scoped_fallback() {
        let (_dir, server) = definition_server(&[
            ("definition.rs".into(), "fn Needle() {}"),
            ("references.txt".into(), "Needle();"),
        ]);
        let params = serde_json::from_value(serde_json::json!({
            "query": "Needle", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("fn Needle"), "{text}");
        assert!(!text.contains("references.txt"), "{text}");
        assert!(!text.contains("No matching definitions"), "{text}");

        let params = serde_json::from_value(serde_json::json!({
            "query": "references.txt Needle", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(
            text.contains("No matching definitions found. Showing other matches:"),
            "{text}"
        );
        assert!(text.contains("references.txt"), "{text}");
        assert!(!text.contains("definition.rs"), "{text}");

        let params = serde_json::from_value(serde_json::json!({
            "patterns": ["Needle"], "constraints": "references.txt", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.multi_grep_inner(params).unwrap());
        assert!(
            text.contains("No matching definitions found. Showing other matches:"),
            "{text}"
        );
        assert!(text.contains("references.txt"), "{text}");
        assert!(!text.contains("definition.rs"), "{text}");
    }

    #[test]
    fn literal_mode_keeps_backslash_n_verbatim() {
        let (_dir, server) = definition_server(&[
            ("a.rs".into(), "let parts = text.split(\"\\n\");\n"),
            ("b.rs".into(), "let parts = text.split(\"\n\");\n"),
        ]);
        let request = serde_json::json!({"query": "split(\"\\n\")", "mode": "literal"});
        let text = response_text(
            server
                .grep(Parameters(serde_json::from_value(request).unwrap()))
                .unwrap(),
        );
        assert!(text.contains("a.rs"), "{text}");
        assert!(!text.contains("b.rs"), "{text}");
    }

    #[test]
    fn stale_in_file_cursor_does_not_skip_other_files() {
        let lines = |name: &str, count: usize| -> String {
            (0..count)
                .map(|i| format!("Needle({name}, {i});\n"))
                .collect()
        };
        let (a, b) = (lines("a", 150), lines("b", 150));
        let (dir, server) =
            definition_server(&[("a.rs".into(), a.as_str()), ("b.rs".into(), b.as_str())]);
        let grep = |cursor: Option<&str>| {
            let params = serde_json::from_value(serde_json::json!({
                "query": "Needle", "maxResults": 100, "context": 0, "cursor": cursor,
            }))
            .unwrap();
            response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap())
        };
        let first = grep(None);
        let (shown, other) = if first.contains("Needle(a, 0)") {
            ("a", "b")
        } else {
            ("b", "a")
        };
        assert!(first.contains(&format!("Needle({shown}, 99)")), "{first}");
        let cursor = first.rsplit("cursor: ").next().unwrap().trim().to_owned();
        std::fs::write(dir.path().join(format!("{shown}.rs")), lines(shown, 20)).unwrap();

        let second = grep(Some(&cursor));
        assert!(second.contains(&format!("Needle({other}, 0)")), "{second}");
        assert!(!second.contains(&format!("Needle({shown},")), "{second}");
    }

    #[test]
    fn default_multi_grep_does_not_print_collected_context() {
        let body: String = (0..20)
            .map(|i| format!("call_needle({i});\n{}", "let filler = 0;\n".repeat(20)))
            .collect();
        let (_dir, server) = definition_server(&[("a.rs".into(), body.as_str())]);
        let params =
            serde_json::from_value(serde_json::json!({"patterns": ["call_needle"]})).unwrap();
        let text = response_text(server.multi_grep_inner(params).unwrap());
        assert!(text.contains("call_needle(19)"), "{text}");
        assert!(!text.contains("filler"), "{text}");
        assert!(text.len() < 5000, "{}", text.len());
    }

    #[test]
    fn explicit_context_is_budgeted_without_hiding_matches() {
        let body: String = (0..400)
            .map(|i| format!("needle_{i}();\n{}", "let filler = 0;\n".repeat(10)))
            .collect();
        let (_dir, server) = definition_server(&[("a.rs".into(), body.as_str())]);
        let params = serde_json::from_value(serde_json::json!({
            "query": "needle_", "context": 5, "maxResults": 400,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.starts_with("Context shown for "), "{}", &text[..200]);
        assert!(text.contains(" 2- let filler = 0;"), "{}", &text[..400]);
        let last = text
            .lines()
            .filter(|line| line.contains(": needle_"))
            .count();
        assert_eq!(last, text.matches("needle_").count());
        assert!(text.len() < 25_000, "{}", text.len());
    }

    #[test]
    fn definition_context_budget_does_not_hide_matches() {
        let content: Vec<_> = (0..20)
            .map(|i| {
                (format!("module{i}.rs"), format!(
                    "struct Needle{i} {{\n    first_field_with_a_long_name: String,\n    second_field_with_a_long_name: String,\n    third_field_with_a_long_name: String,\n}}"
                ))
            })
            .collect();
        let files: Vec<_> = content
            .iter()
            .map(|(p, s)| (p.clone(), s.as_str()))
            .collect();
        let (_dir, server) = definition_server(&files);
        let params = serde_json::from_value(serde_json::json!({
            "query": "Needle", "definitions_only": true, "maxResults": 50,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("Details omitted for the last"), "{text}");
        assert!(
            text.contains("  2|     first_field_with_a_long_name"),
            "{text}"
        );
        for i in 0..20 {
            assert!(text.contains(&format!("struct Needle{i} {{")), "{text}");
        }
        assert!(!text.contains("→ Read"), "{text}");
        assert!(!text.contains("cursor:"), "{text}");

        let params = serde_json::from_value(serde_json::json!({
            "query": "module0.rs Needle", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("first_field_with_a_long_name"), "{text}");
    }

    #[test]
    fn fallback_cursor_keeps_returning_ordinary_matches() {
        let files: Vec<_> = (0..60)
            .map(|i| (format!("reference_{i:02}.rs"), "Needle();"))
            .collect();
        let (_dir, server) = definition_server(&files);
        let params = serde_json::from_value(serde_json::json!({
            "query": "Needle", "definitions_only": true, "maxResults": 50,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("No matching definitions found"), "{text}");
        let id = text.split("cursor: ").nth(1).unwrap().trim();
        let cursor = server.cursor_store.lock().unwrap().get_grep(id).unwrap();
        assert_eq!(cursor.stage, GrepStage::References);
        assert!(cursor.file_offset > 0);

        let params = serde_json::from_value(serde_json::json!({
            "query": "Needle", "definitions_only": true, "cursor": id,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("Needle();"), "{text}");
        assert!(!text.contains("No matching definitions"), "{text}");
    }

    #[test]
    fn exact_references_precede_fuzzy_definitions() {
        let (_dir, server) = definition_server(&[
            ("definition.rs".into(), "fn Needle() {}"),
            ("reference.txt".into(), "Nedle();"), // spellchecker:disable-line
        ]);
        let params = serde_json::from_value(serde_json::json!({
            "query": "Nedle", "definitions_only": true, // spellchecker:disable-line
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("Showing other matches:"), "{text}");
        assert!(text.contains("reference.txt"), "{text}");
        assert!(!text.contains("definition.rs"), "{text}");
        assert!(!text.contains("Maybe you meant"), "{text}");
    }

    #[test]
    fn fuzzy_definitions_precede_fuzzy_references_and_keep_scope() {
        let (_dir, server) = definition_server(&[
            ("definition.rs".into(), "fn Needle() {}"),
            ("reference.txt".into(), "Needle();"),
        ]);
        for (query, expected, absent, notice) in [
            (
                "Nedle", // spellchecker:disable-line
                "definition.rs",
                "reference.txt",
                "Maybe you meant these definitions:",
            ),
            (
                "reference.txt Nedle", // spellchecker:disable-line
                "reference.txt",
                "definition.rs",
                "Maybe you meant these other matches:",
            ),
            (
                "!*.rs Nedle", // spellchecker:disable-line
                "reference.txt",
                "definition.rs",
                "Maybe you meant these other matches:",
            ),
        ] {
            let params = serde_json::from_value(serde_json::json!({
                "query": query, "definitions_only": true,
            }))
            .unwrap();
            let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            assert!(text.contains(notice), "{text}");
            assert!(text.contains(expected), "{text}");
            assert!(!text.contains(absent), "{text}");
        }
    }

    #[test]
    fn fuzzy_fallback_cursors_keep_their_stage() {
        for definitions in [true, false] {
            let files: Vec<_> = (0..60)
                .map(|i| {
                    (
                        format!("file_{i:02}.rs"),
                        if definitions {
                            "fn Needle() {}"
                        } else {
                            "Needle();"
                        },
                    )
                })
                .collect();
            let (_dir, server) = definition_server(&files);
            let params = serde_json::from_value(serde_json::json!({
                "query": "Nedle", "definitions_only": true, "maxResults": 50, // spellchecker:disable-line
            }))
            .unwrap();
            let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            assert!(text.contains("Maybe you meant"), "{text}");
            let id = text.split("cursor: ").nth(1).unwrap().trim();
            let cursor = server.cursor_store.lock().unwrap().get_grep(id).unwrap();
            assert_eq!(
                cursor.stage,
                if definitions {
                    GrepStage::FuzzyDefinitions
                } else {
                    GrepStage::FuzzyMatches
                }
            );
            assert!(cursor.found_matches);
            let params = serde_json::from_value(serde_json::json!({
                "query": "Nedle", "cursor": id, "maxResults": 50, // spellchecker:disable-line
            }))
            .unwrap();
            let next = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            assert!(next.contains("Needle"), "{next}");
            assert!(next.contains("Maybe you meant"), "{next}");
            assert!(!next.contains("cursor:"), "{next}");
        }
    }

    #[test]
    fn empty_definition_fallback_reports_no_matches() {
        let (_dir, server) = definition_server(&[("a.rs".into(), "fn unrelated() {}")]);
        let params = serde_json::from_value(serde_json::json!({
            "query": "Needle", "definitions_only": true,
        }))
        .unwrap();
        assert_eq!(
            response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap()),
            "No matching definitions or other matches found."
        );
    }

    #[test]
    fn empty_scopes_only_broaden_with_a_file_suggestion_notice() {
        let (_dir, server) = definition_server(&[
            ("a.rs".into(), "fn Needle() {}\nNeedle();\n*.txt Needle\n"),
            ("b.txt".into(), "unrelated\n"),
        ]);
        for query in ["*.txt Needle", "!*.rs Needle"] {
            let params = serde_json::from_value(serde_json::json!({
                "query": query, "definitions_only": true,
            }))
            .unwrap();
            let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            if query.starts_with('!') {
                assert_eq!(
                    text, "No matching definitions or other matches found.",
                    "{query}"
                );
            } else {
                assert!(
                    text.contains("elsewhere in the indexed project"),
                    "{query}: {text}"
                );
                assert!(text.contains("a.rs\n 1: fn Needle"), "{query}: {text}");
                assert!(!text.contains("cursor:"), "{query}: {text}");
            }
        }
    }

    #[test]
    fn definition_search_reports_and_drops_a_missing_file_path() {
        let (_dir, server) = definition_server(&[
            ("src/a.rs".into(), "fn Needle() {}\nNeedle();\n"),
            ("b.rs".into(), "unrelated();\n"),
        ]);
        let notice = "No indexed file matches 'missing.rs'. Searched all files instead.";
        let params = serde_json::from_value(serde_json::json!({
            "query": "missing.rs Needle", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.starts_with(notice), "{text}");
        assert!(
            text.contains("src/a.rs") && text.contains("fn Needle()"),
            "{text}"
        );

        for query in ["a.rs Needle", "src/a.rs Needle"] {
            let params = serde_json::from_value(serde_json::json!({
                "query": query, "definitions_only": true,
            }))
            .unwrap();
            let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            assert!(!text.contains("No indexed file"), "{query}: {text}");
            assert!(text.contains("fn Needle()"), "{query}: {text}");
        }

        let id = server.cursor_store.lock().unwrap().store_grep(GrepCursor {
            stage: GrepStage::References,
            missing_path: true,
            ..Default::default()
        });
        let params = serde_json::from_value(serde_json::json!({
            "query": "missing.rs Needle", "cursor": id,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.starts_with(notice), "{text}");
        assert!(text.contains("Needle();"), "{text}");
    }

    #[test]
    fn completed_pages_do_not_restart_a_fallback_after_returning_matches() {
        let (_dir, server) = definition_server(&[
            ("a.rs".into(), "fn Needle() {}"),
            ("b.rs".into(), "unrelated();"),
        ]);
        for stage in [
            GrepStage::Definitions,
            GrepStage::References,
            GrepStage::FuzzyDefinitions,
            GrepStage::FuzzyMatches,
        ] {
            let id = server.cursor_store.lock().unwrap().store_grep(GrepCursor {
                file_offset: 1,
                stage,
                found_matches: true,
                ..Default::default()
            });
            let params = serde_json::from_value(serde_json::json!({
                "query": "Needle", "cursor": id,
            }))
            .unwrap();
            assert_eq!(
                response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap()),
                "0 matches.",
                "{stage:?}"
            );
        }
    }

    #[test]
    fn empty_resumed_stages_restart_the_next_stage_at_the_first_file() {
        for (stage, query, source, notice) in [
            (
                GrepStage::Definitions,
                "Needle",
                "Needle();",
                "Showing other matches:",
            ),
            (
                GrepStage::References,
                "Nedle", // spellchecker:disable-line
                "fn Needle() {}",
                "Maybe you meant these definitions:",
            ),
            (
                GrepStage::FuzzyDefinitions,
                "Nedle", // spellchecker:disable-line
                "Needle();",
                "Maybe you meant these other matches:",
            ),
        ] {
            let (_dir, server) =
                definition_server(&[("a.rs".into(), source), ("b.rs".into(), "unrelated();")]);
            let id = server.cursor_store.lock().unwrap().store_grep(GrepCursor {
                file_offset: 1,
                stage,
                found_matches: false,
                ..Default::default()
            });
            let params =
                serde_json::from_value(serde_json::json!({"query": query, "cursor": id})).unwrap();
            let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            assert!(text.contains(notice), "{stage:?}: {text}");
            assert!(text.contains(source), "{stage:?}: {text}");
        }
    }

    #[test]
    fn named_path_keeps_grep_fallbacks_scoped() {
        let (_dir, server) = definition_server(&[
            ("scope_a/defs.rs".into(), "fn Needle() {}"),
            ("scope_a/uses.txt".into(), "Needle();"),
            ("scope_b/defs.rs".into(), "fn Needle() {}"),
        ]);
        for path in ["scope_a", "scope_a/", "**/scope_a/**", "scope_a/defs.rs"] {
            for definitions_only in [false, true] {
                for query in ["Needle", "Nedle"] {
                    let params = serde_json::from_value(serde_json::json!({
                        "query": query, "path": path, "definitions_only": definitions_only,
                    }))
                    .unwrap();
                    let text =
                        response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
                    assert!(text.contains("scope_a/defs.rs"), "{query} {path}: {text}");
                    assert!(!text.contains("scope_b"), "{query} {path}: {text}");
                }
            }
        }
        for (query, path) in [("wrong Needle", "scope_a"), ("Needle", "missing.rs")] {
            let params =
                serde_json::from_value(serde_json::json!({"query": query, "path": path})).unwrap();
            let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            if path == "scope_a" {
                assert!(!text.contains("scope_b"), "{text}");
                assert!(text.contains("scope_a"), "{text}");
            } else {
                assert!(text.contains("elsewhere in the indexed project"), "{text}");
                assert!(text.contains("1: fn Needle"), "{text}");
                assert!(
                    text.starts_with(&format!(
                        "Path '{path}' matches no indexed file or directory.\nNo matches in the requested scope."
                    )),
                    "{text}"
                );
            }
        }
        let params = serde_json::from_value(serde_json::json!({
            "query": "*.txt Needle", "path": "scope_a", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("scope_a/uses.txt"), "{text}");
        assert!(!text.contains("defs.rs"), "{text}");
    }

    #[test]
    fn named_path_supports_literal_spaces_and_commas() {
        let (_dir, server) = definition_server(&[
            ("space dir/a,b.rs".into(), "fn Needle() {}"),
            ("other.rs".into(), "fn Needle() {}"),
        ]);
        for path in ["space dir", "space dir/a,b.rs", "a,b.rs"] {
            let params = serde_json::from_value(serde_json::json!({
                "query": "Needle", "path": path, "definitions_only": true,
            }))
            .unwrap();
            let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            assert!(text.contains("space dir/a,b.rs"), "{path}: {text}");
            assert!(!text.contains("other.rs"), "{path}: {text}");
        }
    }

    #[test]
    fn named_path_scopes_file_search_and_its_retry() {
        let (_dir, server) = definition_server(&[
            ("scope_a/defs.rs".into(), "fn Needle() {}"),
            ("scope_b/defs.rs".into(), "fn Needle() {}"),
        ]);
        for query in ["", "defs", "de fs unfindable"] {
            let params =
                serde_json::from_value(serde_json::json!({"query": query, "path": "scope_a"}))
                    .unwrap();
            let text = response_text(server.find_files(Parameters(params)).unwrap());
            assert!(text.contains("scope_a/defs.rs"), "{text}");
            assert!(!text.contains("scope_b"), "{text}");
        }
    }

    #[test]
    fn named_paths_preserve_literal_punctuation() {
        for (path, included, excluded) in [
            ("a,b.rs", "a,b.rs", "axb.rs"),
            ("a,b.rs", "nested/a,b.rs", "nested/axb.rs"),
            ("pkg/{client}", "pkg/{client}/a.rs", "pkg/XclientY/a.rs"),
            ("pkg/{a,b}", "pkg/{a,b}/a.rs", "pkg/XaXbY/a.rs"),
            ("[id]", "nested/[id]/a.rs", "nested/i/a.rs"),
            #[cfg(unix)]
            (r"a\b.rs", r"a\b.rs", "axb.rs"),
        ] {
            let (_dir, server) = definition_server(&[
                (included.into(), "fn Needle() {}"),
                (excluded.into(), "fn Needle() {}"),
            ]);
            for query in ["Needle", "Nedle"] {
                let params = serde_json::from_value(serde_json::json!({
                    "query": query, "path": path, "definitions_only": true,
                }))
                .unwrap();
                let text =
                    response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
                assert!(text.contains(included), "{path}: {text}");
                assert!(!text.contains(excluded), "{path}: {text}");
            }
            let params =
                serde_json::from_value(serde_json::json!({"query": "", "path": path})).unwrap();
            let text = response_text(server.find_files(Parameters(params)).unwrap());
            assert!(text.contains(included), "{path}: {text}");
            assert!(!text.contains(excluded), "{path}: {text}");
        }
    }

    #[test]
    fn named_paths_stay_inside_the_checkout() {
        let (dir, server) = definition_server(&[("src/a.rs".into(), "fn Needle() {}")]);
        let guard = server.picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let base = dir.path();
        let outside = base.parent().unwrap().join("outside-fff-index");
        assert_eq!(path_scope(None, picker).unwrap(), None);
        assert_eq!(path_scope(Some("."), picker).unwrap(), None);
        assert_eq!(path_scope(base.to_str(), picker).unwrap(), None);
        assert_eq!(
            path_scope(base.join("src").to_str(), picker).unwrap(),
            path_scope(Some("src/"), picker).unwrap()
        );
        assert!(path_scope(Some("../other"), picker).is_err());
        assert!(path_scope(outside.to_str(), picker).is_err());
        for missing in [base.join("missing"), "missing".into()] {
            let scope = path_scope(missing.to_str(), picker).unwrap().unwrap();
            assert!(
                scope
                    .notice
                    .unwrap()
                    .contains("matches no indexed file or directory"),
                "{missing:?}"
            );
        }

        #[cfg(unix)]
        {
            let link = tempfile::tempdir().unwrap();
            let linked_root = link.path().join("root");
            std::os::unix::fs::symlink(base, &linked_root).unwrap();
            assert_eq!(
                path_scope(linked_root.join("src").to_str(), picker).unwrap(),
                path_scope(Some("src"), picker).unwrap()
            );
            // A missing tail under another spelling of the root stays inside the checkout.
            let scope = path_scope(linked_root.join("missing").to_str(), picker).unwrap();
            assert_eq!(scope.unwrap().glob, "{missing,missing/**}");
            let outside = link.path().join("outside");
            assert!(path_scope(outside.join("missing").to_str(), picker).is_err());
        }
        drop(guard);

        let params =
            serde_json::from_value(serde_json::json!({"query": "Needle", "path": "missing"}))
                .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(
            text.starts_with("Path 'missing' matches no indexed"),
            "{text}"
        );
        let params =
            serde_json::from_value(serde_json::json!({"query": "a", "path": "missing"})).unwrap();
        let text = response_text(server.find_files(Parameters(params)).unwrap());
        assert!(
            text.starts_with("Path 'missing' matches no indexed"),
            "{text}"
        );
    }

    #[test]
    fn named_paths_are_literal_and_anchored_when_indexed() {
        let (dir, server) = definition_server(&[
            ("app/blog/[slug]/page.tsx".into(), "Needle"),
            ("app/blog/{a,b}.ts".into(), "Needle"),
            ("app/[id]/page.tsx".into(), "Needle"),
            ("src/lib.rs".into(), "Needle"),
            ("packages/x/src/lib.rs".into(), "Needle"),
            ("styles/a.css".into(), "color: red !important;"),
            ("other/a.css".into(), "color: blue !important;"),
        ]);
        let grep = |query: &str, path: &str| {
            let params =
                serde_json::from_value(serde_json::json!({"query": query, "path": path})).unwrap();
            response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap())
        };
        for path in ["app/blog/[slug]", "app/blog/[slug]/page.tsx"] {
            let text = grep("Needle", path);
            assert!(text.contains("app/blog/[slug]/page.tsx"), "{path}: {text}");
            assert!(!text.contains("lib.rs"), "{path}: {text}");
        }
        let text = grep("Needle", "[id]");
        assert!(text.contains("app/[id]/page.tsx"), "{text}");
        assert!(!text.contains("lib.rs"), "{text}");
        let text = grep("Needle", "app/blog/{a,b}.ts");
        assert!(text.contains("app/blog/{a,b}.ts"), "{text}");

        let root_src = dir.path().join("src");
        for path in ["src", root_src.to_str().unwrap()] {
            let text = grep("Needle", path);
            assert!(text.contains("src/lib.rs"), "{path}: {text}");
            assert!(!text.contains("packages/x"), "{path}: {text}");
        }
        let text = grep("Needle", "x/src");
        assert!(text.contains("packages/x/src/lib.rs"), "{text}");
        let text = grep("Needle", "**/x/**");
        assert!(text.contains("packages/x/src/lib.rs"), "{text}");
        assert!(!text.contains("page.tsx"), "{text}");

        let text = grep("!important", "styles");
        assert!(text.contains("styles/a.css"), "{text}");
        assert!(!text.contains("other/a.css"), "{text}");
    }

    #[test]
    fn named_path_is_optional_in_search_schemas() {
        for schema in [
            schemars::schema_for!(GrepParams),
            schemars::schema_for!(FindFilesParams),
        ] {
            let value = serde_json::to_value(schema).unwrap();
            assert!(value["properties"].get("path").is_some());
            assert!(
                !value["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|key| key == "path")
            );
        }
    }

    #[test]
    fn empty_definition_search_labels_broader_source_and_unverified_filenames() {
        let (_dir, server) = definition_server(&[
            ("other/target.rs".into(), "fn Needle() {}"),
            ("excluded/target.rs".into(), "fn Needle() {}"),
            ("NeedleGuide.md".into(), "Unrelated content"),
        ]);
        let params = serde_json::from_value(serde_json::json!({
            "query": "!excluded/ Needle", "path": "missing", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("elsewhere in the indexed project"), "{text}");
        assert!(text.contains("other/target.rs"), "{text}");
        assert!(!text.contains("excluded/target.rs"), "{text}");
        assert!(text.contains("1: fn Needle"), "{text}");
        assert!(
            text.starts_with(
                "Path 'missing' matches no indexed file or directory.\nNo matches in the requested scope."
            ),
            "{text}"
        );
        assert!(!text.contains("cursor:"), "{text}");

        let params = serde_json::from_value(serde_json::json!({
            "query": "!other/ !excluded/ NeedleGuide", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("contents are unverified"), "{text}");
        assert!(text.contains("NeedleGuide.md"), "{text}");
    }

    #[test]
    fn grep_pages_preserve_every_matching_line_and_import() {
        let content: Vec<_> = (0..7)
            .map(|file| {
                let text = (0..37)
                    .map(|line| format!("use Needle{file}_{line};\n"))
                    .collect::<String>();
                (format!("file{file}.rs"), text)
            })
            .collect();
        let files = content
            .iter()
            .map(|(p, text)| (p.clone(), text.as_str()))
            .collect::<Vec<_>>();
        let (_dir, server) = definition_server(&files);
        for multi in [false, true] {
            let mut request = serde_json::json!({"query":"Needle", "patterns":["Needle"], "maxResults": 13, "context":0});
            let mut found = std::collections::HashSet::new();
            for _ in 0..100 {
                let result = if multi {
                    server
                        .multi_grep_inner(serde_json::from_value(request.clone()).unwrap())
                        .unwrap()
                } else {
                    server
                        .grep(Parameters(serde_json::from_value(request.clone()).unwrap()))
                        .unwrap()
                };
                let text = response_text(result);
                let mut page_count = 0;
                for line in text.lines().filter(|line| line.contains("use Needle")) {
                    assert!(found.insert(line.to_string()), "duplicate {line}");
                    page_count += 1;
                }
                assert!(page_count <= 13, "{text}");
                let Some(cursor) = text.lines().find_map(|line| line.strip_prefix("cursor: "))
                else {
                    break;
                };
                assert!(page_count > 0, "empty page with cursor: {text}");
                request["cursor"] = cursor.into();
            }
            assert_eq!(found.len(), 7 * 37, "multi={multi}");
        }
    }

    #[test]
    fn broadened_grep_cursors_preserve_query_scope_and_every_match() {
        for (file_count, matches_per_file) in [(5, 1), (1, 12), (5, 7)] {
            for regex in [false, true] {
                let mut content = (0..file_count)
                    .map(|file| {
                        let text = (0..matches_per_file)
                            .map(|line| format!("before\nuse Needle{file}_{line};\nafter\n"))
                            .collect::<String>();
                        (format!("src/file{file}.rs"), text)
                    })
                    .collect::<Vec<_>>();
                content.push(("src/excluded/a.rs".into(), "Needle excluded".into()));
                content.push(("other/a.rs".into(), "Needle outside scope".into()));
                let files = content
                    .iter()
                    .map(|(path, text)| (path.clone(), text.as_str()))
                    .collect::<Vec<_>>();
                let (_dir, server) = definition_server(&files);
                let query = if regex {
                    "unused Needle[0-9] !excluded/"
                } else {
                    "unused Needle !excluded/"
                };
                let mut request = serde_json::json!({
                    "query": query, "path": "src", "maxResults": 2,
                    "before_context": 1, "after_context": 0,
                });
                let mut found = std::collections::HashSet::new();
                for _ in 0..50 {
                    let text = response_text(
                        server
                            .grep(Parameters(serde_json::from_value(request.clone()).unwrap()))
                            .unwrap(),
                    );
                    assert!(text.starts_with("0 matches for 'unused"), "{text}");
                    assert!(text.contains("Auto-broadened to"), "{text}");
                    assert!(text.contains("before"), "{text}");
                    assert!(!text.contains("after"), "{text}");
                    assert!(!text.contains("excluded/a.rs"), "{text}");
                    assert!(!text.contains("other/a.rs"), "{text}");
                    let mut page_count = 0;
                    for line in text.lines().filter(|line| line.contains("use Needle")) {
                        assert!(found.insert(line.to_string()), "duplicate {line}");
                        page_count += 1;
                    }
                    assert!((1..=2).contains(&page_count), "{text}");
                    let Some(cursor) = text.lines().find_map(|line| line.strip_prefix("cursor: "))
                    else {
                        break;
                    };
                    request["cursor"] = cursor.into();
                }
                assert_eq!(found.len(), file_count * matches_per_file);
            }
        }
    }

    #[test]
    fn explicit_patterns_preserve_whitespace_and_filename_tokens() {
        let (_dir, server) = definition_server(&[
            (
                "src/a.rs".into(),
                "    pub fn Needle() {}\nlet x = \"config.rs  literal\";\nneedle\nNeedle",
            ),
            ("src/b.py".into(), "config.rs  literal\nNeedle"),
            ("tests/test.rs".into(), "Needle"),
            ("config.rs".into(), "unrelated"),
        ]);
        for (query, mode, expected) in [
            ("^    pub fn Needle", "regex", "    pub fn Needle"),
            ("config.rs  literal", "literal", "config.rs  literal"),
        ] {
            let params = serde_json::from_value(
                serde_json::json!({"query":query,"mode":mode,"context":0,"path":"src/a.rs"}),
            )
            .unwrap();
            let text = response_text(server.grep(Parameters(params)).unwrap());
            assert!(text.contains(expected), "{text}");
        }
        let params = serde_json::from_value(serde_json::json!({
            "query":"Needle", "mode":"regex", "case":"sensitive", "context":0,
            "paths":["src", "tests"], "globs":["*.rs","*.py","!tests/**"],
        }))
        .unwrap();
        let text = response_text(server.grep(Parameters(params)).unwrap());
        assert!(
            text.contains("src/a.rs") && text.contains("src/b.py"),
            "{text}"
        );
        assert!(
            !text.contains("tests/test.rs") && !text.contains("needle"),
            "{text}"
        );
    }

    #[test]
    fn explicit_context_is_retained_and_invalid_regex_is_reported() {
        let (_dir, server) = definition_server(&[("a.rs".into(), "before\nneedle\nafter\nlast")]);
        let params = serde_json::from_value(
            serde_json::json!({"query":"needle", "before_context":1, "after_context":2}),
        )
        .unwrap();
        let text = response_text(server.grep(Parameters(params)).unwrap());
        for line in ["before", "needle", "after", "last"] {
            assert!(text.contains(line), "{text}");
        }
        let params =
            serde_json::from_value(serde_json::json!({"query":"[", "mode":"regex"})).unwrap();
        assert!(
            server
                .grep(Parameters(params))
                .unwrap_err()
                .message
                .contains("Invalid regex")
        );
    }

    #[test]
    fn filename_search_distinguishes_literal_regex_glob_and_fuzzy() {
        let (_dir, server) = definition_server(&[
            ("utils/image.rs".into(), "x"),
            ("tests/image.py".into(), "x"),
            ("irrelevant_mangled.rs".into(), "x"),
            ("nested/utils/image.rs".into(), "x"),
        ]);
        for request in [
            serde_json::json!({"query":"image", "paths":["utils"]}),
            serde_json::json!({"query":"image\\.rs$", "mode":"regex", "paths":["utils"]}),
            serde_json::json!({"query":"*.rs", "mode":"glob", "paths":["utils"]}),
            serde_json::json!({"query":"", "globs":["*.rs","*.py","!tests/**"], "paths":["utils"]}),
        ] {
            let text = response_text(
                server
                    .find_files(Parameters(serde_json::from_value(request).unwrap()))
                    .unwrap(),
            );
            assert_eq!(text, "utils/image.rs");
        }
        let request = serde_json::json!({"query":"image"});
        let text = response_text(
            server
                .find_files(Parameters(serde_json::from_value(request).unwrap()))
                .unwrap(),
        );
        assert!(!text.contains("irrelevant"), "{text}");
        assert!(
            text.contains("utils/image.rs") && text.contains("tests/image.py"),
            "{text}"
        );
    }

    #[test]
    fn context_windows_merge_without_hiding_matching_lines() {
        let (_dir, server) =
            definition_server(&[("a.rs".into(), "before\nNeedle one\nNeedle two\nafter")]);
        let params =
            serde_json::from_value(serde_json::json!({"query":"Needle", "context":1})).unwrap();
        let text = response_text(server.grep(Parameters(params)).unwrap());
        for source in ["before", "Needle one", "Needle two", "after"] {
            assert_eq!(text.matches(source).count(), 1, "{text}");
        }
        assert!(
            text.contains("2: Needle one") && text.contains("3: Needle two"),
            "{text}"
        );
    }

    #[test]
    fn small_content_results_include_source_context_unless_disabled() {
        let (_dir, server) = definition_server(&[(
            "src/lib.rs".into(),
            "/// Internal outcome.\npub struct Outcome {\n    pub success: Option<bool>,\n}",
        )]);
        for (context, includes_declaration) in [(None, true), (Some(0), false)] {
            let mut request = serde_json::json!({"query":"success", "mode":"literal"});
            if let Some(context) = context {
                request["context"] = context.into();
            }
            let text = response_text(
                server
                    .grep(Parameters(serde_json::from_value(request).unwrap()))
                    .unwrap(),
            );
            assert_eq!(
                text.contains("2- pub struct Outcome"),
                includes_declaration,
                "{text}"
            );
            assert!(text.contains("3:     pub success"), "{text}");
        }
    }

    #[test]
    fn module_globs_require_a_descendant_file() {
        let (_dir, server) = definition_server(&[
            ("core/image_tests.rs".into(), "Needle"),
            ("utils/image/lib.rs".into(), "Needle"),
            ("utils/image/tests/deep.rs".into(), "Needle"),
        ]);
        for scope in [
            serde_json::json!({"path":"**/*image*/**"}),
            serde_json::json!({"globs":["**/*image*/**"]}),
        ] {
            let mut request = serde_json::json!({"query":"Needle", "mode":"literal", "context":0});
            request
                .as_object_mut()
                .unwrap()
                .extend(scope.as_object().unwrap().clone());
            let text = response_text(
                server
                    .grep(Parameters(serde_json::from_value(request).unwrap()))
                    .unwrap(),
            );
            assert!(!text.contains("core/image_tests.rs"), "{text}");
            assert!(text.contains("utils/image/lib.rs"), "{text}");
            assert!(text.contains("utils/image/tests/deep.rs"), "{text}");
        }
    }

    #[test]
    fn filename_fragments_match_separator_variants_but_exact_modes_do_not() {
        let (_dir, server) = definition_server(&[
            ("src/network_client.rs".into(), ""),
            ("network-client/src/lib.rs".into(), ""),
            ("network-client/src/hosts.rs".into(), ""),
            ("other/client.rs".into(), ""),
        ]);
        for mode in ["fuzzy", "regex", "glob"] {
            let query = if mode == "glob" {
                "*network_client*"
            } else {
                "network_client"
            };
            let request = serde_json::json!({"query":query,"mode":mode});
            let text = response_text(
                server
                    .find_files(Parameters(serde_json::from_value(request).unwrap()))
                    .unwrap(),
            );
            assert!(text.contains("src/network_client.rs"), "{mode}: {text}");
            assert_eq!(
                text.contains("network-client/src/lib.rs"),
                mode == "fuzzy",
                "{mode}: {text}"
            );
            assert!(!text.contains("other/client.rs"), "{mode}: {text}");
        }
    }

    #[test]
    fn broader_suggestions_try_module_fragments_and_preserve_exclusions() {
        let (_dir, server) = definition_server(&[
            (
                "network-client/src/lib.rs".into(),
                "fn delay() {\n    jitter();\n}",
            ),
            ("network-client/generated.rs".into(), "jitter();"),
            ("other/lib.rs".into(), "jitter();"),
        ]);
        let request = serde_json::json!({
            "query":"jitter", "mode":"literal", "path":"network",
            "globs":["!**/generated.rs"],
        });
        let text = response_text(
            server
                .grep(Parameters(serde_json::from_value(request).unwrap()))
                .unwrap(),
        );
        assert!(text.contains("broader scope"), "{text}");
        assert!(text.contains("network-client/src/lib.rs"), "{text}");
        assert!(text.contains("1- fn delay()"), "{text}");
        assert!(
            !text.contains("generated.rs") && !text.contains("other/lib.rs"),
            "{text}"
        );
        assert!(!text.contains("cursor:"), "{text}");
    }

    #[test]
    fn excluded_directory_globs_and_chained_filename_filters() {
        let (_dir, server) = definition_server(&[
            ("src/image.rs".into(), "Needle"),
            ("src/image.py".into(), "Needle"),
            ("src/other.rs".into(), "Needle"),
            ("node_modules/image.rs".into(), "Needle"),
        ]);
        let params = serde_json::from_value(serde_json::json!({
            "query":"Needle", "mode":"literal", "globs":["!node_modules"], "context":0,
        }))
        .unwrap();
        let text = response_text(server.grep(Parameters(params)).unwrap());
        assert!(!text.contains("node_modules"), "{text}");
        assert!(text.contains("src/image.rs"), "{text}");
        let params = serde_json::from_value(serde_json::json!({
            "query":"", "mode":"glob", "filters":["image", "\\.rs$"], "globs":["!node_modules"],
        }))
        .unwrap();
        assert_eq!(
            response_text(server.find_files(Parameters(params)).unwrap()),
            "src/image.rs"
        );
    }

    #[test]
    fn structured_scopes_survive_fuzzy_retry_and_suggestions_reject_scattered_matches() {
        let (_dir, server) = definition_server(&[
            (
                "src/noise.rs".into(),
                "internal_chat_message_metadata_passthrough: None,",
            ),
            ("other/backoff.rs".into(), "fn jitter() {}"),
            ("src/typo.rs".into(), "fn Needle() {}"),
        ]);
        let params = serde_json::from_value(
            serde_json::json!({"query":"jitter", "mode":"literal", "paths":["src"]}),
        )
        .unwrap();
        let text = response_text(server.grep(Parameters(params)).unwrap());
        assert!(text.contains("elsewhere in the indexed project"), "{text}");
        assert!(text.contains("other/backoff.rs"), "{text}");
        assert!(!text.contains("noise.rs"), "{text}");
        let params = serde_json::from_value(
            serde_json::json!({"query":"Nedle", "mode":"literal", "paths":["src"]}),
        )
        .unwrap();
        let text = response_text(server.grep(Parameters(params)).unwrap());
        assert!(
            text.contains("approximate") && text.contains("src/typo.rs"),
            "{text}"
        );
        let params = serde_json::from_value(
            serde_json::json!({"query":"Nedle", "mode":"literal", "paths":["missing"]}),
        )
        .unwrap();
        let text = response_text(server.grep(Parameters(params)).unwrap());
        assert!(!text.contains("fn Needle"), "{text}");
    }

    #[test]
    fn include_globs_allow_hidden_matches_without_entering_other_hidden_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .arg(dir.path())
                .status()
                .unwrap()
                .success()
        );
        let (_dir, server) = definition_server_at(
            dir,
            &[
                ("src/visible.rs".into(), "Needle"),
                ("src/.dot.rs".into(), "Needle"),
                ("src/.hidden/a.rs".into(), "Needle"),
                ("src/.hidden/.nested/b.rs".into(), "Needle"),
                ("other/.hidden/c.rs".into(), "Needle"),
                (".github/workflows/ci.rs".into(), "Needle"),
            ],
        );
        for (globs, expected) in [
            (vec![".github/**"], vec![".github/workflows/ci.rs"]),
            (
                vec![".github/workflows/*.rs"],
                vec![".github/workflows/ci.rs"],
            ),
            (vec!["**/.github/**"], vec![".github/workflows/ci.rs"]),
            (
                vec!["src/**"],
                vec![
                    "src/visible.rs",
                    "src/.dot.rs",
                    "src/.hidden/a.rs",
                    "src/.hidden/.nested/b.rs",
                ],
            ),
            (vec!["*.rs"], vec!["src/visible.rs", "src/.dot.rs"]),
            (
                vec!["src/.hidden", "*.rs"],
                vec!["src/visible.rs", "src/.dot.rs", "src/.hidden/a.rs"],
            ),
            (vec![], vec!["src/visible.rs"]),
        ] {
            for grep in [false, true] {
                let request = serde_json::json!({
                    "query": if grep { "Needle" } else { "" },
                    "mode": if grep { "literal" } else { "glob" },
                    "globs":globs, "hidden":false, "context":0,
                });
                let response = if grep {
                    server.grep(Parameters(serde_json::from_value(request).unwrap()))
                } else {
                    server.find_files(Parameters(serde_json::from_value(request).unwrap()))
                };
                let text = response_text(response.unwrap());
                let actual = text
                    .lines()
                    .filter(|line| line.ends_with(".rs"))
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    actual,
                    expected.iter().copied().collect(),
                    "{globs:?}: {text}"
                );
            }
        }
        for (paths, expected) in [
            (vec!["src/.hidden"], vec!["src/.hidden/a.rs"]),
            (
                vec!["src", "src/.hidden"],
                vec!["src/visible.rs", "src/.hidden/a.rs"],
            ),
            (
                vec!["src", "src/.dot.rs"],
                vec!["src/visible.rs", "src/.dot.rs"],
            ),
            (
                vec!["src/.hidden/.nested/b.rs"],
                vec!["src/.hidden/.nested/b.rs"],
            ),
        ] {
            let request =
                serde_json::json!({"query":"", "mode":"glob", "paths":paths, "hidden":false});
            let text = response_text(
                server
                    .find_files(Parameters(serde_json::from_value(request).unwrap()))
                    .unwrap(),
            );
            assert_eq!(
                text.lines().collect::<std::collections::BTreeSet<_>>(),
                expected.iter().copied().collect(),
                "{paths:?}: {text}"
            );
        }
    }

    #[test]
    fn broadened_cursors_resume_the_shortened_query() {
        let (_dir, server) = definition_server(&[
            ("a.rs".into(), "before\nuse Needle1;\nafter\n"),
            ("b.rs".into(), "before\nuse Needle2;\nafter\n"),
            ("excluded.txt".into(), "Needle outside scope"),
        ]);
        for query in ["unused Needle *.rs", "unused Needle[0-9] *.rs"] {
            let id = server.cursor_store.lock().unwrap().store_grep(GrepCursor {
                file_offset: 1,
                stage: GrepStage::Broadened,
                found_matches: true,
                ..Default::default()
            });
            let params = serde_json::from_value(serde_json::json!({
                "query": query, "cursor": id, "context": 1,
            }))
            .unwrap();
            let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            assert!(text.starts_with("0 matches for 'unused"), "{text}");
            assert!(text.contains("Auto-broadened to"), "{text}");
            assert!(text.contains("use Needle"), "{text}");
            assert!(text.contains("before"), "{text}");
            assert!(text.contains("after"), "{text}");
            assert!(!text.contains("excluded.txt"), "{text}");
        }
    }

    #[test]
    fn widened_reference_stage_keeps_definitions_found_elsewhere() {
        let (_dir, server) = indexed_server(&[
            ("a.rs", "fn other() {}\n"),
            (
                "src/b.rs",
                "pub struct NeedleWidget {}\nimpl NeedleWidget {}\n",
            ),
        ]);
        let params = serde_json::from_value(serde_json::json!({
            "query": "a.rs NeedleWidget", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(text.contains("pub struct NeedleWidget {}"), "{text}");
    }

    #[test]
    fn file_guesses_match_indexed_paths_case_insensitively() {
        let (_dir, server) = definition_server(&[("README.md".into(), "fn Needle() {}\n")]);
        let params = serde_json::from_value(serde_json::json!({
            "query": "readme.md Needle", "definitions_only": true,
        }))
        .unwrap();
        let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
        assert!(!text.contains("No indexed file matches"), "{text}");
    }

    fn indexed_server(files: &[(&str, &str)]) -> (tempfile::TempDir, FffServer) {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let shared = SharedFilePicker::default();
        fff::FilePicker::new_with_shared_state(
            shared.clone(),
            fff::SharedFrecency::default(),
            fff::FilePickerOptions {
                base_path: dir.path().to_string_lossy().into_owned(),
                enable_mmap_cache: false,
                enable_content_indexing: true,
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !shared
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|p| !p.is_scan_active() && p.bigram_index().is_some())
        {
            assert!(
                std::time::Instant::now() < deadline,
                "bigram index was not built"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        (dir, FffServer::new(shared))
    }

    fn definition_server(files: &[(String, &str)]) -> (tempfile::TempDir, FffServer) {
        let dir = tempfile::tempdir().unwrap();
        definition_server_at(dir, files)
    }

    fn definition_server_at(
        dir: tempfile::TempDir,
        files: &[(String, &str)],
    ) -> (tempfile::TempDir, FffServer) {
        for (name, content) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let mut picker = fff::FilePicker::new(fff::FilePickerOptions {
            base_path: dir.path().to_string_lossy().into_owned(),
            watch: false,
            enable_content_indexing: false,
            enable_mmap_cache: false,
            ..Default::default()
        })
        .unwrap();
        picker.collect_files().unwrap();
        let shared = SharedFilePicker::default();
        *shared.write().unwrap() = Some(picker);
        (dir, FffServer::new(shared))
    }

    fn response_text(result: CallToolResult) -> String {
        serde_json::to_value(result).unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}
