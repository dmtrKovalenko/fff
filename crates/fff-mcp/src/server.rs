use crate::cursor::{CursorStore, GrepCursor, GrepStage};
use crate::output::{GrepFormatter, OutputMode, file_suffix};
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
        Some(v) => (v.round() as usize).max(1),
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
    let auto_expand = !is_usage && ctx_lines == 0;
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
            before_context: ctx_lines,
            after_context: after_ctx,
            classify_definitions: true.into(),
            trim_whitespace: true,
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
    /// Max results (default 20).
    #[serde(rename = "maxResults")]
    // this has to be float because llms are stupid
    pub max_results: Option<f64>,
    /// Cursor from previous result. Only use if previous results weren't sufficient.
    pub cursor: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GrepParams {
    /// Content text or regex. Put known module constraints in path.
    /// Inline path constraints are also supported.
    // `pattern` alias: LLMs that have seen multi_grep (which uses `patterns`)
    // routinely call grep with `pattern`; accept it instead of erroring out
    // with an unhelpful "missing field `query`" (#311).
    #[serde(alias = "pattern")]
    pub query: String,
    /// Directory, filename, or one glob to search. Set this from the request's module context.
    /// Examples: "src/net", "client.py", "**/*billing*client*/**".
    pub path: Option<String>,
    /// Max matching lines (default 20).
    #[serde(rename = "maxResults")]
    pub max_results: Option<f64>, // this has to be float because llms are stupid
    /// Cursor from previous result. Only use if previous results weren't sufficient.
    pub cursor: Option<String>,
    /// Output format (default 'content').
    pub output_mode: Option<String>,
    /// Context lines before/after each match.
    pub context: Option<f64>,
    /// Find declarations. If none match, return references or fuzzy matches with a fallback notice.
    #[serde(default)]
    pub definitions_only: bool,
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
        let max_results = normalize_max_results(params.max_results, 20);
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

        let (mut options, auto_expand) =
            make_grep_options(output_mode, mode, cursor.file_offset, context);
        let ctx_lines = options.before_context;

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
        let mut original = parse_scoped_grep(&parser, query, scope);
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
                    parsed
                })
            } else {
                &original
            };
            let mut result = picker.grep(parsed, &options);
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
            if parts.len() >= 2 {
                let first_word = parts[0];
                let is_valid_constraint = first_word.starts_with('!')
                    || first_word.starts_with('*')
                    || first_word.ends_with('/');

                if !is_valid_constraint {
                    let rest_query = parts[1..].join(" ");
                    let mut rest_parsed = parse_scoped_grep(&parser, &rest_query, scope);
                    rest_parsed.constraints = original.constraints.clone();

                    let rest_text = rest_parsed.grep_text();
                    let retry_mode = if has_regex_metacharacters(&rest_text) {
                        GrepMode::Regex
                    } else {
                        mode
                    };

                    let mut retry_options = options.clone();
                    retry_options.mode = retry_mode;
                    retry_options.file_offset = 0;
                    let retry_result = picker.grep(&rest_parsed, &retry_options);

                    if !retry_result.matches.is_empty() && retry_result.matches.len() <= 10 {
                        cursor.stage = GrepStage::Broadened;
                        let mut cs = self.lock_cursors()?;
                        let text = &GrepFormatter {
                            matches: &retry_result.matches,
                            files: &retry_result.files,
                            total_matched: retry_result.matches.len(),
                            cursor: GrepCursor {
                                file_offset: retry_result.next_file_offset,
                                ..cursor
                            },
                            output_mode,
                            max_results,
                            show_context: ctx_lines > 0,
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
            let mut fuzzy_parsed = parse_scoped_grep(&parser, &fuzzy_query, scope);
            fuzzy_parsed.constraints = original.constraints.clone();
            let fuzzy_result = picker.grep(&fuzzy_parsed, &fuzzy_options);

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

            return Ok(respond("0 matches.".into(), false));
        }

        if result.matches.is_empty() && result.next_file_offset == 0 {
            if prefer_definitions
                && !cursor.found_matches
                && let Some(suggestions) = suggest_other_files(picker, &original, mode)
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
        cursor.file_offset = result.next_file_offset;
        cursor.found_matches |= !result.matches.is_empty();

        let mut cs = self.lock_cursors()?;
        let text = GrepFormatter {
            matches: &result.matches,
            files: &result.files,
            total_matched: result.matches.len(),
            cursor,
            output_mode,
            max_results,
            show_context: ctx_lines > 0,
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
        description = "Find files by fuzzy name. Put directory or module scope in path and the filename in query. An empty query lists files in path.",
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
        let mut fff_query = parser.parse(query);
        fff_query.constraints.extend(scope.map(Constraint::Glob));
        let result = picker.fuzzy_search(&fff_query, None, make_opts(page_offset));
        let total_files = result.total_files;

        // Auto-retry with fewer terms if 3+ words return 0 results
        let words: Vec<&str> = query.split_whitespace().collect();
        let shorter = words.get(..2).map(|w| w.join(" "));

        let (items, scores, total_matched) =
            if result.items.is_empty() && words.len() >= 3 && page_offset == 0 {
                if let Some(shorter) = &shorter {
                    let mut shorter_query = parser.parse(shorter);
                    shorter_query.constraints = fff_query.constraints.clone();
                    let retry = picker.fuzzy_search(&shorter_query, None, make_opts(0));

                    (retry.items, retry.scores, retry.total_matched)
                } else {
                    (result.items, result.scores, result.total_matched)
                }
            } else {
                (result.items, result.scores, result.total_matched)
            };

        let notice = path_scope.and_then(|scope| scope.notice);
        if items.is_empty() {
            let text = format!("0 results ({} indexed)", total_files);
            return Ok(with_notice(
                CallToolResult::success(vec![Content::text(text)]),
                notice,
            ));
        }

        let mut lines: Vec<String> = Vec::new();
        let top_item = items[0];
        let is_exact_match = scores[0].exact_match;

        if page_offset == 0 {
            if is_exact_match {
                lines.push(format!(
                    "→ Read {} (exact match!)",
                    top_item.relative_path(picker)
                ));
            } else if scores.len() < 2 || scores[0].total > scores[1].total.saturating_mul(2) {
                lines.push(format!(
                    "→ Read {} (best match — Read this file directly)",
                    top_item.relative_path(picker)
                ));
            }
        }

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
        description = "Search source text or regex. Put directory or module scope in path and the content pattern in query. Use definitions_only=true for declarations. Returned source lines verify locations without another read.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn grep(
        &self,
        Parameters(params): Parameters<GrepParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.bump_activity();
        self.wait_for_scan(SCAN_READY_TIMEOUT)?;

        let parsed = QueryParser::new(AiGrepConfig).parse(&params.query);
        let grep_text = parsed.grep_text();

        let mode = if has_regex_metacharacters(&grep_text) {
            GrepMode::Regex
        } else {
            GrepMode::PlainText
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

        cursor.file_offset = result.next_file_offset;
        cursor.found_matches |= !result.matches.is_empty();
        let mut cs = self.lock_cursors()?;
        let text = GrepFormatter {
            matches: &result.matches,
            files: &file_refs,
            total_matched: result.matches.len(),
            cursor,
            output_mode,
            max_results,
            show_context: ctx_lines > 0,
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
) -> Option<String> {
    let mut broader = query.clone();
    broader.raw_query = "";
    broader
        .constraints
        .retain(|c| matches!(c, Constraint::Exclude(_) | Constraint::Not(_)));
    if broader.constraints.len() < query.constraints.len() {
        let result = picker.grep(
            &broader,
            &GrepSearchOptions {
                mode,
                max_matches_per_file: 1,
                page_limit: 3,
                time_budget_ms: 100,
                enforce_time_budget: true,
                ..Default::default()
            },
        );
        if !result.files.is_empty() {
            let files = result
                .files
                .iter()
                .take(3)
                .map(|file| file.relative_path(picker))
                .collect::<Vec<_>>();
            return Some(format!(
                "No matches in the requested scope. These files elsewhere in the indexed project contain the pattern:\n{}",
                files.join("\n")
            ));
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
    let path = path.trim_start_matches("./").trim_end_matches('/');
    if path.is_empty() || path == "." {
        return Ok(None);
    }
    let escaped = escape_glob(path);
    let scope = |glob: String, found: bool| PathScope {
        glob,
        notice: (!found).then(|| format!("Path '{raw}' matches no indexed file or directory.")),
    };
    if absolute || index_contains(picker, path, true) {
        let found = !absolute || index_contains(picker, path, true);
        return Ok(Some(scope(format!("{{{escaped},{escaped}/**}}"), found)));
    }
    if has_wildcards(path) {
        return Ok(Some(scope(path.to_owned(), true)));
    }
    let found = index_contains(picker, path, false);
    Ok(Some(scope(
        format!("{{**/{escaped},**/{escaped}/**}}"),
        found,
    )))
}

fn strip_root(
    path: &std::path::Path,
    base: &std::path::Path,
) -> Result<std::path::PathBuf, ErrorData> {
    if let Ok(relative) = path.strip_prefix(base) {
        return Ok(relative.to_path_buf());
    }
    // A symlinked spelling of the root still names the indexed checkout.
    std::fs::canonicalize(path)
        .ok()
        .zip(std::fs::canonicalize(base).ok())
        .and_then(|(path, base)| path.strip_prefix(base).ok().map(|p| p.to_path_buf()))
        .ok_or_else(|| ErrorData::invalid_params("path must be inside the indexed checkout", None))
}

fn index_contains(picker: &fff::FilePicker, path: &str, anchored: bool) -> bool {
    let matches = |candidate: String| {
        #[cfg(windows)]
        let candidate = candidate.replace('\\', "/");
        let candidate = candidate.trim_end_matches('/');
        candidate
            .strip_suffix(path)
            .is_some_and(|rest| rest.is_empty() || (!anchored && rest.ends_with('/')))
    };
    picker
        .get_dirs()
        .iter()
        .any(|dir| matches(dir.relative_path(picker)))
        || picker
            .get_files()
            .iter()
            .any(|file| !file.is_deleted() && matches(file.relative_path(picker)))
}

fn escape_glob(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for c in path.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | ',' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
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
        assert!(text.contains("Context omitted"), "{text}");
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
                assert!(text.ends_with("a.rs"), "{query}: {text}");
                assert!(!text.contains("fn Needle"), "{query}: {text}");
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
            assert!(!text.contains("scope_b"), "{text}");
            assert_eq!(text.contains("scope_a"), path == "scope_a", "{text}");
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
    fn empty_definition_search_suggests_files_without_claiming_declarations() {
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
        assert!(!text.contains("fn Needle"), "{text}");
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
