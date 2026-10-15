use crate::cursor::{CursorStore, GrepCursor, GrepStage};
use crate::output::{GrepFormatter, OutputMode, file_suffix};
use fff::grep::{GrepMode, GrepSearchOptions, has_regex_metacharacters};
use fff::types::{FileItem, PaginationArgs};
use fff::{FuzzySearchOptions, QueryParser, SharedFilePicker};
use fff_query_parser::AiGrepConfig;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const SCAN_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const MAX_CONTEXT_LINES: usize = 100;

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
    /// Fuzzy search query. Supports path prefixes and glob constraints.
    // `pattern` alias for consistency with grep's alias and the common
    // file-search parameter name (#311).
    #[serde(alias = "pattern")]
    pub query: String,
    /// Max results (default 20).
    #[serde(rename = "maxResults")]
    // this has to be float because llms are stupid
    pub max_results: Option<f64>,
    /// Cursor from previous result. Only use if previous results weren't sufficient.
    pub cursor: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GrepParams {
    /// Search text or regex query with optional constraint prefixes.
    /// Matches within single lines only — use ONE specific term, not multiple words.
    // `pattern` alias: LLMs that have seen multi_grep (which uses `patterns`)
    // routinely call grep with `pattern`; accept it instead of erroring out
    // with an unhelpful "missing field `query`" (#311).
    #[serde(alias = "pattern")]
    pub query: String,
    /// Max matching lines (default 20).
    #[serde(rename = "maxResults")]
    pub max_results: Option<f64>, // this has to be float because llms are stupid
    /// Cursor from previous result. Only use if previous results weren't sufficient.
    pub cursor: Option<String>,
    /// Output format (default 'content').
    pub output_mode: Option<String>,
    /// Context lines before/after each match.
    pub context: Option<f64>,
    /// Try declarations, exact matches, fuzzy declarations, then fuzzy matches. Report fallbacks.
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
    /// File constraints (e.g. '*.{ts,tsx} !test/'). ALWAYS provide when possible.
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
        let prefer_definitions = cursor.stage != GrepStage::Normal;

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
        let original = parser.parse(query);
        let fuzzy_query = prefer_definitions.then(|| cleanup_fuzzy_query(&original.grep_text()));
        let mut fuzzy = None;
        let result = loop {
            options.classify_definitions = cursor.stage.definition_mode();
            options.file_offset = cursor.file_offset;
            options.mode = if cursor.stage.is_fuzzy() {
                GrepMode::Fuzzy
            } else {
                mode
            };
            let parsed = if cursor.stage.is_fuzzy() {
                fuzzy.get_or_insert_with(|| {
                    let mut parsed = parser.parse(fuzzy_query.as_deref().unwrap_or(query));
                    parsed.constraints = original.constraints.clone();
                    parsed
                })
            } else {
                &original
            };
            let result = picker.grep(parsed, &options);
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
                    let rest_parsed = parser.parse(&rest_query);

                    let rest_text = rest_parsed.grep_text();
                    let retry_mode = if has_regex_metacharacters(&rest_text) {
                        GrepMode::Regex
                    } else {
                        mode
                    };

                    let (mut retry_options, _) =
                        make_grep_options(output_mode, retry_mode, 0, context);
                    retry_options.classify_definitions = options.classify_definitions;
                    let retry_result = picker.grep(&rest_parsed, &retry_options);

                    if !retry_result.matches.is_empty() && retry_result.matches.len() <= 10 {
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
                        return Ok(grep_response(
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
            let fuzzy_query = cleanup_fuzzy_query(query);
            let (mut fuzzy_options, _) =
                make_grep_options(output_mode, GrepMode::Fuzzy, 0, Some(0));
            fuzzy_options.classify_definitions = options.classify_definitions;
            let fuzzy_parsed = parser.parse(&fuzzy_query);
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
                return Ok(grep_response(lines.join("\n"), false));
            }

            // File path fallback: if query looks like a path, suggest the matching file
            if query.contains('/') {
                let file_parser = QueryParser::default();
                let file_query = file_parser.parse(query);
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
                        return Ok(grep_response(
                            format!(
                                "0 content matches. But there is a relevant file path: {}",
                                top.relative_path(picker)
                            ),
                            false,
                        ));
                    }
                }
            }

            return Ok(grep_response("0 matches.".into(), false));
        }

        if result.matches.is_empty() && result.next_file_offset == 0 {
            return Ok(grep_response(
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

        let text = match notice {
            Some(notice) => format!("{notice}\n{text}"),
            None => text,
        };
        Ok(grep_response(text, false))
    }
}

#[tool_router]
impl FffServer {
    /// Fuzzy file search by name. Searches FILE NAMES, not file contents.
    /// Use it when you need to find a file, not a definition.
    /// Use grep instead for searching code content (definitions, usage patterns).
    /// Supports fuzzy matching, path prefixes ('shc/'), and glob constraints.
    /// IMPORTANT: Keep queries SHORT — prefer 1-2 terms max.
    #[tool(
        name = "find_files",
        description = "Fuzzy file search by name. Searches FILE NAMES, not file contents. Use it when you need to find a file, not a definition. Use grep instead for searching code content (definitions, usage patterns). Supports fuzzy matching, path prefixes ('src/'), and glob constraints ('name **/src/*.{ts,tsx} !test/'). IMPORTANT: Keep queries SHORT — prefer 1-2 terms max. Multiple words are a waterfall (each narrows results), NOT OR. If unsure, start broad with 1 term and refine.",
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
        let fff_query = parser.parse(query);
        let result = picker.fuzzy_search(&fff_query, None, make_opts(page_offset));
        let total_files = result.total_files;

        // Auto-retry with fewer terms if 3+ words return 0 results
        let words: Vec<&str> = query.split_whitespace().collect();
        let shorter = words.get(..2).map(|w| w.join(" "));

        let (items, scores, total_matched) =
            if result.items.is_empty() && words.len() >= 3 && page_offset == 0 {
                if let Some(shorter) = &shorter {
                    let shorter_query = parser.parse(shorter);
                    let retry = picker.fuzzy_search(&shorter_query, None, make_opts(0));

                    (retry.items, retry.scores, retry.total_matched)
                } else {
                    (result.items, result.scores, result.total_matched)
                }
            } else {
                (result.items, result.scores, result.total_matched)
            };

        if items.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "0 results ({} indexed)",
                total_files
            ))]));
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

    /// Search file contents for text patterns. This is the DEFAULT search tool.
    /// Prefer plain text over regex. Filter files with constraints.
    #[tool(
        name = "grep",
        description = "Search file contents. Search for bare identifiers (e.g. 'InProgressQuote', 'ActorAuth'), NOT code syntax or regex. Set definitions_only to true to try declarations, exact matches, fuzzy declarations, then fuzzy matches; report fallbacks. Filter files with constraints (e.g. '*.rs query', 'src/ query'). Use filename, directory (ending with /) or glob expressions to prefilter. See server instructions for constraint syntax and core rules.",
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
        description = "Search file contents for lines matching ANY of multiple patterns (OR logic). IMPORTANT: This returns files where ANY query matches, NOT all patterns. Patterns are literal text — NEVER escape special characters (no \\( \\) \\. etc). Set definitions_only to true to find declarations; if none are found, return other matches with a notice. Faster than regex alternation for literal text. See server instructions for constraint syntax.",
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
    fn definition_fallback_does_not_leave_empty_file_constraints() {
        let (_dir, server) = definition_server(&[
            ("a.rs".into(), "fn Needle() {}\nNeedle();\n*.txt Needle\n"),
            ("b.txt".into(), "unrelated\n"),
        ]);
        for query in ["missing.rs Needle", "*.txt Needle", "!*.rs Needle"] {
            let params = serde_json::from_value(serde_json::json!({
                "query": query, "definitions_only": true,
            }))
            .unwrap();
            assert_eq!(
                response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap()),
                "No matching definitions or other matches found.",
                "{query}"
            );
        }
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
            });
            let params =
                serde_json::from_value(serde_json::json!({"query": query, "cursor": id})).unwrap();
            let text = response_text(server.perform_grep(&params, GrepMode::PlainText).unwrap());
            assert!(text.contains(notice), "{stage:?}: {text}");
            assert!(text.contains(source), "{stage:?}: {text}");
        }
    }

    fn definition_server(files: &[(String, &str)]) -> (tempfile::TempDir, FffServer) {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            std::fs::write(dir.path().join(name), content).unwrap();
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
