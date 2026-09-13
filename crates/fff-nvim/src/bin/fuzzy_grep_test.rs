/// Fuzzy grep quality test against ~/dev/lightsource
///
/// Runs queries through the fuzzy grep pipeline and prints results
/// so we can verify match quality.
///
/// Usage:
///   cargo run --release --bin fuzzy_grep_test              # runs default test queries
///   cargo run --release --bin fuzzy_grep_test -- "query"   # runs a single user query
use fff::file_picker::FilePicker;
use fff::grep::{GrepMode, GrepSearchOptions, parse_grep_query};
use std::path::Path;
use std::time::Instant;

fn create_picker(path: &Path) -> FilePicker {
    let mut picker = FilePicker::new(fff::FilePickerOptions {
        base_path: path.to_string_lossy().to_string(),
        enable_mmap_cache: false,
        mode: fff::FFFMode::Neovim,
        ..Default::default()
    })
    .expect("Failed to create FilePicker");
    picker.collect_files().expect("Failed to collect files");
    picker
}

fn run_fuzzy_query(picker: &FilePicker, query: &str, label: &str) {
    let options = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 100,
        mode: GrepMode::Fuzzy,
        time_budget_ms: 0,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    };

    let parsed = parse_grep_query(query);
    let start = Instant::now();
    let result = picker.grep(&parsed, &options);
    let elapsed = start.elapsed();

    eprintln!("══════════════════════════════════════════════════════════════");
    eprintln!("  Query: \"{}\"  ({})", query, label);
    eprintln!(
        "  Results: {} matches in {} files ({:.2}ms)",
        result.matches.len(),
        result.total_files_searched,
        elapsed.as_secs_f64() * 1000.0,
    );
    eprintln!("══════════════════════════════════════════════════════════════");

    if result.matches.is_empty() {
        eprintln!("  (no matches)\n");
        return;
    }

    // Group by file for readability
    let mut current_file_idx = usize::MAX;
    for (i, m) in result.matches.iter().enumerate() {
        if m.file_index != current_file_idx {
            current_file_idx = m.file_index;
            let file = &result.files[m.file_index];
            eprintln!("\n  ┌─ {}", file.relative_path(picker));
        }

        // Truncate long lines for display
        let display_line = if m.line_content.len() > 100 {
            format!("{}...", &m.line_content[..100])
        } else {
            m.line_content.clone()
        };

        let score_str = m
            .fuzzy_score
            .map(|s| format!("score={}", s))
            .unwrap_or_else(|| "no-score".to_string());

        let offsets_str = if m.match_byte_offsets.is_empty() {
            String::new()
        } else {
            // Show what text fragments are highlighted
            let fragments: Vec<String> = m
                .match_byte_offsets
                .iter()
                .filter_map(|&(s, e)| {
                    m.line_content
                        .get(s as usize..e as usize)
                        .map(|frag| format!("\"{}\"", frag))
                })
                .collect();
            format!(" hl=[{}]", fragments.join(","))
        };

        eprintln!(
            "  │ L{:<5} [{}{}] {}",
            m.line_number,
            score_str,
            offsets_str,
            display_line.trim(),
        );

        // Cap output at 50 lines
        if i >= 49 {
            let remaining = result.matches.len() - 50;
            if remaining > 0 {
                eprintln!("  │ ... and {} more matches", remaining);
            }
            break;
        }
    }
    eprintln!();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let (repo_path, queries) = if let Some(idx) = args.iter().position(|a| a == "--path") {
        let path = args
            .get(idx + 1)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                eprintln!("--path requires an argument");
                std::process::exit(1);
            });
        let queries: Vec<String> = args
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != idx && *i != idx + 1)
            .map(|(_, s)| s.clone())
            .collect();
        (path, queries)
    } else {
        let path = std::path::PathBuf::from(
            std::env::var("HOME").unwrap_or_else(|_| "/Users/neogoose".to_string()),
        )
        .join("dev/lightsource");
        (path, args)
    };

    if !repo_path.exists() {
        eprintln!("Repository not found at: {:?}", repo_path);
        std::process::exit(1);
    }

    let canonical = fff::path_utils::canonicalize(&repo_path).expect("Failed to canonicalize path");
    eprintln!("=== Fuzzy Grep Quality Test ===");
    eprintln!("Repository: {:?}\n", canonical);

    eprintln!("Loading files...");
    let load_start = Instant::now();
    let picker = create_picker(&canonical);
    let files = picker.get_files();
    let non_binary = files.iter().filter(|f| !f.is_binary()).count();
    eprintln!(
        "Loaded {} files ({} non-binary) in {:.2}s\n",
        files.len(),
        non_binary,
        load_start.elapsed().as_secs_f64()
    );

    if queries.is_empty() {
        // Run default test queries
        run_fuzzy_query(&picker, "shcema", "transposition of 'schema'");
        run_fuzzy_query(&picker, "SortedMap", "should match SortedArrayMap");
        run_fuzzy_query(
            &picker,
            "struct SortedMap",
            "should NOT match SourcingProjectMetadataParts",
        );
    } else {
        // Run user-provided queries
        for query in &queries {
            run_fuzzy_query(&picker, query, "user query");
        }
    }

    eprintln!("=== Done ===");
}
