use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

use rand::rngs::SmallRng;
use rand::{RngCore, SeedableRng};

use fff_search::file_picker::{FFFMode, FilePicker, FuzzySearchOptions};
use fff_search::grep::{GrepMode, GrepSearchOptions, parse_grep_query};
use fff_search::{
    FilePickerOptions, PaginationArgs, QueryParser, SharedFilePicker, SharedFrecency,
};

const DOMAINS: &[&str] = &[
    r#"
use std::net::{TcpStream, SocketAddr};
fn establish_connection(addr: SocketAddr) -> Result<TcpStream, std::io::Error> {
    let stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    Ok(stream)
}
fn parse_http_header(raw: &[u8]) -> Option<(&str, &str)> {
    let line = std::str::from_utf8(raw).ok()?;
    let (key, val) = line.split_once(':')?;
    Some((key.trim(), val.trim()))
}
"#,
    r#"
use sqlx::{PgPool, Row};
async fn query_users(pool: &PgPool, limit: i64) -> Vec<String> {
    sqlx::query("SELECT name FROM users ORDER BY created_at DESC LIMIT $1")
        .bind(limit)
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get("name"))
        .collect()
}
async fn insert_record(pool: &PgPool, name: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO records (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}
"#,
    r#"
fn verify_jwt_token(token: &str, secret: &[u8]) -> Result<Claims, AuthError> {
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() != 3 { return Err(AuthError::MalformedToken); }
    let payload = base64_decode(parts[1])?;
    let signature = hmac_sha256(secret, &format!("{}.{}", parts[0], parts[1]));
    if signature != base64_decode(parts[2])? { return Err(AuthError::InvalidSignature); }
    serde_json::from_slice(&payload).map_err(AuthError::Deserialize)
}
fn hash_password(password: &str, salt: &[u8]) -> Vec<u8> {
    argon2::hash_encoded(password.as_bytes(), salt, &argon2::Config::default())
        .unwrap().into_bytes()
}
"#,
    r#"
struct Renderer { framebuffer: Vec<u32>, width: usize, height: usize }
impl Renderer {
    fn clear(&mut self, color: u32) { self.framebuffer.fill(color); }
    fn draw_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x < self.width && y < self.height {
            self.framebuffer[y * self.width + x] = color;
        }
    }
    fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32) {
        let dx = (x1 - x0).abs(); let dy = -(y1 - y0).abs();
        let mut err = dx + dy;
        let (mut cx, mut cy) = (x0, y0);
        loop {
            self.draw_pixel(cx as usize, cy as usize, color);
            if cx == x1 && cy == y1 { break; }
            let e2 = 2 * err;
            if e2 >= dy { err += dy; cx += if x0 < x1 { 1 } else { -1 }; }
            if e2 <= dx { err += dx; cy += if y0 < y1 { 1 } else { -1 }; }
        }
    }
}
"#,
    r#"
use serde::{Serialize, Deserialize};
#[derive(Serialize, Deserialize)]
struct ConfigFile { log_level: String, max_retries: u32, timeout_ms: u64 }
fn load_config(path: &std::path::Path) -> Result<ConfigFile, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    let config: ConfigFile = toml::from_str(&contents)?;
    Ok(config)
}
fn merge_configs(base: ConfigFile, overlay: ConfigFile) -> ConfigFile {
    ConfigFile {
        log_level: if overlay.log_level.is_empty() { base.log_level } else { overlay.log_level },
        max_retries: overlay.max_retries.max(base.max_retries),
        timeout_ms: overlay.timeout_ms.max(base.timeout_ms),
    }
}
"#,
    r#"
struct PhysicsBody { position: [f64; 3], velocity: [f64; 3], mass: f64 }
fn apply_gravity(bodies: &mut [PhysicsBody], dt: f64) {
    let gravity_constant = 6.674e-11;
    let len = bodies.len();
    let mut forces = vec![[0.0f64; 3]; len];
    for i in 0..len {
        for j in (i+1)..len {
            let dx = bodies[j].position[0] - bodies[i].position[0];
            let dy = bodies[j].position[1] - bodies[i].position[1];
            let dz = bodies[j].position[2] - bodies[i].position[2];
            let dist_sq = dx*dx + dy*dy + dz*dz;
            let force_mag = gravity_constant * bodies[i].mass * bodies[j].mass / dist_sq;
            let dist = dist_sq.sqrt();
            for k in 0..3 {
                let f = force_mag * [dx, dy, dz][k] / dist;
                forces[i][k] += f; forces[j][k] -= f;
            }
        }
    }
    for (body, force) in bodies.iter_mut().zip(forces.iter()) {
        for k in 0..3 {
            body.velocity[k] += force[k] / body.mass * dt;
            body.position[k] += body.velocity[k] * dt;
        }
    }
}
"#,
    r#"
use std::collections::BTreeMap;
struct CacheEntry<V> { value: V, frequency: u64, last_access: u64 }
struct LFUCache<K: Ord, V> { map: BTreeMap<K, CacheEntry<V>>, capacity: usize, clock: u64 }
impl<K: Ord, V> LFUCache<K, V> {
    fn new(capacity: usize) -> Self { Self { map: BTreeMap::new(), capacity, clock: 0 } }
    fn get(&mut self, key: &K) -> Option<&V> {
        self.clock += 1;
        let entry = self.map.get_mut(key)?;
        entry.frequency += 1;
        entry.last_access = self.clock;
        Some(&entry.value)
    }
    fn insert(&mut self, key: K, value: V) {
        self.clock += 1;
        if self.map.len() >= self.capacity { self.evict(); }
        self.map.insert(key, CacheEntry { value, frequency: 1, last_access: self.clock });
    }
    fn evict(&mut self) {
        if let Some(victim) = self.map.keys().min_by_key(|k| {
            let e = &self.map[*k]; (e.frequency, e.last_access)
        }).cloned() { self.map.remove(&victim); }
    }
}
"#,
    r#"
fn tokenize_expression(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&ch) = chars.peek() {
        match ch {
            '0'..='9' => {
                let mut num = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() || d == '.' { num.push(d); chars.next(); }
                    else { break; }
                }
                tokens.push(Token::Number(num.parse().unwrap()));
            }
            '+' => { tokens.push(Token::Plus); chars.next(); }
            '-' => { tokens.push(Token::Minus); chars.next(); }
            '*' => { tokens.push(Token::Star); chars.next(); }
            '/' => { tokens.push(Token::Slash); chars.next(); }
            '(' => { tokens.push(Token::LParen); chars.next(); }
            ')' => { tokens.push(Token::RParen); chars.next(); }
            _ if ch.is_whitespace() => { chars.next(); }
            _ => { chars.next(); }
        }
    }
    tokens
}
"#,
    r#"
use std::sync::mpsc;
use std::thread;
fn parallel_map<T: Send + 'static, R: Send + 'static>(
    items: Vec<T>, num_threads: usize, f: fn(T) -> R
) -> Vec<R> {
    let chunk_size = (items.len() + num_threads - 1) / num_threads;
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for (chunk_idx, chunk) in items.into_iter().collect::<Vec<_>>()
        .chunks(chunk_size).enumerate()
    {
        let tx = tx.clone();
        let chunk = chunk.to_vec();
        handles.push(thread::spawn(move || {
            for (i, item) in chunk.into_iter().enumerate() {
                tx.send((chunk_idx * chunk_size + i, f(item))).unwrap();
            }
        }));
    }
    drop(tx);
    let mut results: Vec<Option<R>> = vec![None; handles.len() * chunk_size];
    for (idx, result) in rx { if idx < results.len() { results[idx] = Some(result); } }
    for h in handles { h.join().unwrap(); }
    results.into_iter().flatten().collect()
}
"#,
    r#"
struct Compressor { window: Vec<u8>, window_size: usize }
impl Compressor {
    fn new(window_size: usize) -> Self {
        Self { window: Vec::with_capacity(window_size), window_size }
    }
    fn find_longest_match(&self, data: &[u8], pos: usize) -> (usize, usize) {
        let mut best_offset = 0; let mut best_length = 0;
        let start = pos.saturating_sub(self.window_size);
        for offset in start..pos {
            let mut length = 0;
            while pos + length < data.len()
                && data[offset + length] == data[pos + length]
                && length < 258
            { length += 1; }
            if length > best_length { best_offset = pos - offset; best_length = length; }
        }
        (best_offset, best_length)
    }
    fn compress(&mut self, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            let (offset, length) = self.find_longest_match(data, pos);
            if length >= 3 {
                output.push(1); output.extend_from_slice(&(offset as u16).to_le_bytes());
                output.push(length as u8); pos += length;
            } else { output.push(0); output.push(data[pos]); pos += 1; }
        }
        output
    }
}
"#,
];

struct FileState {
    name: String,
    token: String,
    #[allow(dead_code)]
    is_base: bool,
    /// Epoch second when this file was last written (used to detect same-second
    /// re-edits that wouldn't bump mtime and thus wouldn't invalidate the mmap).
    last_write_sec: u64,
}

#[test]
fn fuzz_file_operations_stress() {
    const SEED: u64 = 0xDEAD_BEEF_CAFE_1234;
    const INITIAL_FILE_COUNT: usize = 40;
    const NUM_ROUNDS: usize = 20;

    let mut rng = SmallRng::seed_from_u64(SEED);

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // Timing accumulators.
    let mut t_sleep = Duration::ZERO;
    let mut t_git = Duration::ZERO;
    let mut t_bigram_wait = Duration::ZERO;
    let mut t_grep_plain = Duration::ZERO;
    let mut t_grep_regex = Duration::ZERO;
    let mut t_fuzzy = Duration::ZERO;
    let mut t_dead_check = Duration::ZERO;
    let grep_plain_calls = std::sync::atomic::AtomicUsize::new(0);
    let grep_regex_calls = std::sync::atomic::AtomicUsize::new(0);
    let fuzzy_calls = std::sync::atomic::AtomicUsize::new(0);
    let dead_calls = std::sync::atomic::AtomicUsize::new(0);
    let test_start = std::time::Instant::now();

    let mut live_files: Vec<FileState> = Vec::with_capacity(INITIAL_FILE_COUNT + NUM_ROUNDS);
    let mut dead_tokens: Vec<String> = Vec::new();
    let mut next_file_id: usize = 0;

    for i in 0..INITIAL_FILE_COUNT {
        let name = format!("seed_{i:04}.rs");
        let token = format!("FUZZ_SEED_{i:04}");
        write_diverse_file(base, &name, &token, i);
        live_files.push(FileState {
            name,
            token,
            is_base: true,
            last_write_sec: 0, // set before index build, doesn't matter
        });
        next_file_id += 1;
    }

    let t0 = std::time::Instant::now();
    git_init_and_commit(base);
    t_git += t0.elapsed();

    let shared_picker = SharedFilePicker::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        SharedFrecency::noop(),
        FilePickerOptions {
            watch: false, // we do not need the backgrodun monitor
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: true,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            ..Default::default()
        },
    )
    .expect("Failed to create FilePicker");

    let t0 = std::time::Instant::now();
    wait_for_bigram(&shared_picker);
    t_bigram_wait += t0.elapsed();

    // Sanity: all initial tokens findable via plain grep.
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        for fs in &live_files {
            assert!(
                grep_plain_count(picker, &fs.token) >= 1,
                "initial sanity: plain grep should find token {} in {}",
                fs.token,
                fs.name
            );
        }
    }

    // Sleep so mtime advances past the scan snapshot timestamp.
    let t0 = std::time::Instant::now();
    std::thread::sleep(Duration::from_millis(1100));
    t_sleep += t0.elapsed();

    let mut op_counter: usize = 0;

    for round in 0..NUM_ROUNDS {
        let roll: u32 = rng.next_u32() % 100;

        if roll < 40 && !live_files.is_empty() {
            // ── EDIT existing file (40%) ──
            let idx = rng.next_u32() as usize % live_files.len();

            // on_create_or_modify uses mtime (seconds granularity) to decide
            // whether to invalidate the mmap cache. If we re-edit a file in
            // the same second it was last written, the mtime won't change and
            // the stale cached content will be returned. Sleep to advance mtime.
            let now_sec = epoch_secs();
            if live_files[idx].last_write_sec >= now_sec {
                let t0 = std::time::Instant::now();
                std::thread::sleep(Duration::from_millis(1100));
                t_sleep += t0.elapsed();
            }

            let old_token = live_files[idx].token.clone();
            let new_token = format!("FUZZ_{round:02}_{op_counter:04}");
            let name = &live_files[idx].name;
            let domain_idx = rng.next_u32() as usize % DOMAINS.len();
            write_diverse_file_with_domain(base, name, &new_token, domain_idx);

            {
                let mut guard = shared_picker.write().unwrap();
                let picker = guard.as_mut().unwrap();
                assert!(
                    picker.handle_create_or_modify(base.join(name)).is_some(),
                    "round {round}: on_create_or_modify({name}) should succeed for edit"
                );
            }

            dead_tokens.push(old_token);
            live_files[idx].token = new_token;
            live_files[idx].last_write_sec = epoch_secs();
            op_counter += 1;
        } else if roll < 60 {
            // ── CREATE new file (20%) ──
            let name = format!("created_{next_file_id:04}.rs");
            let token = format!("FUZZ_{round:02}_{op_counter:04}");
            let domain_idx = rng.next_u32() as usize % DOMAINS.len();
            write_diverse_file_with_domain(base, &name, &token, domain_idx);

            {
                let mut guard = shared_picker.write().unwrap();
                let picker = guard.as_mut().unwrap();
                assert!(
                    picker.handle_create_or_modify(base.join(&name)).is_some(),
                    "round {round}: on_create_or_modify({name}) should succeed for create"
                );
            }

            live_files.push(FileState {
                name,
                token,
                is_base: false,
                last_write_sec: epoch_secs(),
            });
            next_file_id += 1;
            op_counter += 1;
        } else if roll < 75 && !live_files.is_empty() {
            // ── DELETE existing file (15%) ──
            let idx = rng.next_u32() as usize % live_files.len();
            let removed = live_files.swap_remove(idx);
            let path = base.join(&removed.name);
            fs::remove_file(&path).unwrap();

            {
                let mut guard = shared_picker.write().unwrap();
                let picker = guard.as_mut().unwrap();
                assert!(
                    picker.remove_file_by_path(&path),
                    "round {round}: remove_file_by_path({}) should succeed",
                    removed.name
                );
            }

            dead_tokens.push(removed.token);
            op_counter += 1;
        } else if roll < 85 && !live_files.is_empty() {
            // ── RENAME file (10%) ──
            let idx = rng.next_u32() as usize % live_files.len();
            let old_name = live_files[idx].name.clone();
            let old_path = base.join(&old_name);
            let content = fs::read_to_string(&old_path).unwrap();

            // Remove old file from disk + picker.
            fs::remove_file(&old_path).unwrap();
            {
                let mut guard = shared_picker.write().unwrap();
                let picker = guard.as_mut().unwrap();
                picker.remove_file_by_path(&old_path);
            }

            // Create new file with same content but different name.
            let new_name = format!("renamed_{next_file_id:04}.rs");
            fs::write(base.join(&new_name), &content).unwrap();
            {
                let mut guard = shared_picker.write().unwrap();
                let picker = guard.as_mut().unwrap();
                assert!(
                    picker
                        .handle_create_or_modify(base.join(&new_name))
                        .is_some(),
                    "round {round}: on_create_or_modify({new_name}) should succeed for rename"
                );
            }

            live_files[idx].name = new_name;
            live_files[idx].is_base = false;
            live_files[idx].last_write_sec = epoch_secs();
            next_file_id += 1;
            op_counter += 1;
        }
        // else: no-op / read-only (15%) — just run verification below.

        // ── VERIFY after every round ──
        {
            let guard = shared_picker.read().unwrap();
            let picker = guard.as_ref().unwrap();

            for fs in &live_files {
                // Plain text grep: every live token must be found.
                let t0 = std::time::Instant::now();
                let plain_count = grep_plain_count(picker, &fs.token);
                t_grep_plain += t0.elapsed();
                grep_plain_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                assert!(
                    plain_count >= 1,
                    "round {round}: plain grep should find live token {} in {} (got {plain_count})",
                    fs.token,
                    fs.name
                );

                // Regex grep: search with `{first5}.*{last5}` pattern.
                let regex_pattern = build_regex_pattern(&fs.token);
                let t0 = std::time::Instant::now();
                let regex_count = grep_regex_count(picker, &regex_pattern);
                t_grep_regex += t0.elapsed();
                grep_regex_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                assert!(
                    regex_count >= 1,
                    "round {round}: regex grep '{}' should find live token {} in {} (got {regex_count})",
                    regex_pattern,
                    fs.token,
                    fs.name
                );

                // Fuzzy file search: every live file must be findable by name.
                let stem = extract_stem(&fs.name);
                let t0 = std::time::Instant::now();
                let fuzzy_results = fuzzy_search_paths(picker, &stem);
                t_fuzzy += t0.elapsed();
                fuzzy_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                assert!(
                    fuzzy_results.iter().any(|p| p.contains(&fs.name)),
                    "round {round}: fuzzy search '{}' should find file {} in results: {:?}",
                    stem,
                    fs.name,
                    fuzzy_results
                );
            }

            // Dead tokens must return 0 grep results.
            for dead in &dead_tokens {
                let t0 = std::time::Instant::now();
                let count = grep_plain_count(picker, dead);
                t_dead_check += t0.elapsed();
                dead_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                assert_eq!(
                    count, 0,
                    "round {round}: dead token {dead} should NOT be findable (got {count})"
                );
            }
        }
    }

    let total = test_start.elapsed();
    let t_overhead = t_sleep + t_bigram_wait + t_git;
    let t_search = t_grep_plain + t_grep_regex + t_fuzzy + t_dead_check;
    let t_mutations = total.saturating_sub(t_overhead + t_search);
    let n_grep_plain = grep_plain_calls.load(std::sync::atomic::Ordering::Relaxed);
    let n_grep_regex = grep_regex_calls.load(std::sync::atomic::Ordering::Relaxed);
    let n_fuzzy = fuzzy_calls.load(std::sync::atomic::Ordering::Relaxed);
    let n_dead = dead_calls.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!("\n╔══════════════════════════════════════════════════════╗");
    eprintln!("║  Fuzz Test Performance Breakdown                     ║");
    eprintln!("╠══════════════════════════════════════════════════════╣");
    eprintln!(
        "║  Total wall time:          {:>8.1}ms                ║",
        total.as_secs_f64() * 1000.0
    );
    eprintln!("║  ── Overhead ─────────────────────────────────────── ║");
    eprintln!(
        "║  Sleep (mtime waits):      {:>8.1}ms                ║",
        t_sleep.as_secs_f64() * 1000.0
    );
    eprintln!(
        "║  Git init+commit:          {:>8.1}ms                ║",
        t_git.as_secs_f64() * 1000.0
    );
    eprintln!(
        "║  Bigram index build+scan:  {:>8.1}ms                ║",
        t_bigram_wait.as_secs_f64() * 1000.0
    );
    eprintln!(
        "║  ── Search ({:>3} live files, {:>3} dead tokens) ────── ║",
        live_files.len(),
        dead_tokens.len()
    );
    eprintln!(
        "║  Plain grep:  {:>4} calls   {:>8.1}ms  ({:>6.1}µs/call) ║",
        n_grep_plain,
        t_grep_plain.as_secs_f64() * 1000.0,
        t_grep_plain.as_secs_f64() * 1_000_000.0 / n_grep_plain.max(1) as f64
    );
    eprintln!(
        "║  Regex grep:  {:>4} calls   {:>8.1}ms  ({:>6.1}µs/call) ║",
        n_grep_regex,
        t_grep_regex.as_secs_f64() * 1000.0,
        t_grep_regex.as_secs_f64() * 1_000_000.0 / n_grep_regex.max(1) as f64
    );
    eprintln!(
        "║  Fuzzy find:  {:>4} calls   {:>8.1}ms  ({:>6.1}µs/call) ║",
        n_fuzzy,
        t_fuzzy.as_secs_f64() * 1000.0,
        t_fuzzy.as_secs_f64() * 1_000_000.0 / n_fuzzy.max(1) as f64
    );
    eprintln!(
        "║  Dead checks: {:>4} calls   {:>8.1}ms  ({:>6.1}µs/call) ║",
        n_dead,
        t_dead_check.as_secs_f64() * 1000.0,
        t_dead_check.as_secs_f64() * 1_000_000.0 / n_dead.max(1) as f64
    );
    eprintln!("║  ── Other ────────────────────────────────────────── ║");
    eprintln!(
        "║  Mutations + FS I/O:       {:>8.1}ms                ║",
        t_mutations.as_secs_f64() * 1000.0
    );
    eprintln!("╚══════════════════════════════════════════════════════╝");
}

fn write_diverse_file(dir: &Path, name: &str, token: &str, index: usize) {
    let domain_idx = index % DOMAINS.len();
    write_diverse_file_with_domain(dir, name, token, domain_idx);
}

fn write_diverse_file_with_domain(dir: &Path, name: &str, token: &str, domain_idx: usize) {
    let domain = DOMAINS[domain_idx % DOMAINS.len()];
    let content = format!(
        "// File: {name}\n\
         // Domain content for bigram diversity\n\
         {domain}\n\
         // === Unique searchable token below ===\n\
         const MARKER: &str = \"{token}\";\n\
         fn marker_function_{token}() {{ println!(\"{token}\"); }}\n"
    );

    if let Some(parent) = PathBuf::from(name).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(dir.join(parent)).unwrap();
        }
    }
    fs::write(dir.join(name), content).unwrap();
}

// ═══════════════════════════════════════════════════════════════════════
// Search helpers
// ═══════════════════════════════════════════════════════════════════════

fn grep_plain_opts() -> GrepSearchOptions {
    GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 500,
        mode: GrepMode::PlainText,
        time_budget_ms: 0,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    }
}

fn grep_regex_opts() -> GrepSearchOptions {
    GrepSearchOptions {
        mode: GrepMode::Regex,
        ..grep_plain_opts()
    }
}

fn grep_plain_count(picker: &FilePicker, query: &str) -> usize {
    let parsed = parse_grep_query(query);
    picker.grep(&parsed, &grep_plain_opts()).matches.len()
}

fn grep_regex_count(picker: &FilePicker, regex_query: &str) -> usize {
    let parsed = parse_grep_query(regex_query);
    picker.grep(&parsed, &grep_regex_opts()).matches.len()
}

/// Build a regex pattern from a token: `{first5}.*{last5}`.
/// For tokens shorter than 10 chars, just use the literal (escaped).
fn build_regex_pattern(token: &str) -> String {
    if token.len() >= 10 {
        let first5 = &token[..5];
        let last5 = &token[token.len() - 5..];
        format!("{}.*{}", regex_escape(first5), regex_escape(last5))
    } else {
        regex_escape(token)
    }
}

/// Escape regex metacharacters in a string.
fn regex_escape(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '\\' | '^' | '$' | '|' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Extract a fuzzy-searchable stem from a filename.
/// Strips the extension and any leading path components, keeping the bare name.
fn extract_stem(name: &str) -> String {
    let p = PathBuf::from(name);
    p.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

fn fuzzy_search_paths(picker: &FilePicker, query: &str) -> Vec<String> {
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let result = picker.fuzzy_search(
        &parsed,
        None,
        FuzzySearchOptions {
            max_threads: 1,
            pagination: PaginationArgs {
                offset: 0,
                limit: 200,
            },
            ..Default::default()
        },
    );
    result
        .items
        .iter()
        .map(|f| f.relative_path(picker))
        .collect()
}

fn wait_for_bigram(shared_picker: &SharedFilePicker) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let ready = shared_picker
            .read()
            .ok()
            .map(|guard| {
                guard
                    .as_ref()
                    .map_or(false, |p| !p.is_scan_active() && p.bigram_index().is_some())
            })
            .unwrap_or(false);
        if ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for bigram build"
        );
    }
}

fn git_run(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap_or_else(|e| panic!("git {:?} failed: {}", args, e));
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn git_init_and_commit(dir: &Path) {
    git_run(dir, &["init"]);
    git_run(dir, &["add", "-A"]);
    git_run(dir, &["commit", "-m", "initial"]);
}

/// Proves that dropping the picker while post-scan (warmup + bigram build)
/// is actively iterating raw pointers does NOT segfault. The Drop impl
/// sets `cancelled`, waits for `post_scan_indexing_active` to clear, and
/// only then frees the backing Vec.
///
/// Runs 10 iterations to exercise the race window reliably.
#[test]
fn drop_during_post_scan_does_not_crash() {
    let mut caught_active = 0u32;

    for round in 0..10 {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        // Create enough files so bigram build takes measurable time
        for i in 0..2000 {
            let dir = base.join(format!("d_{:02}", i % 20));
            fs::create_dir_all(&dir).unwrap();
            let content = format!(
                "fn func_{i}() {{ let x = {i}; println!(\"{{x}}\"); }}\n\
                 const T_{i}: &str = \"TOKEN_{i}\";\n"
            );
            fs::write(dir.join(format!("f_{i:04}.rs")), content).unwrap();
        }

        git_init_and_commit(base);

        let shared_picker = SharedFilePicker::default();

        FilePicker::new_with_shared_state(
            shared_picker.clone(),
            SharedFrecency::noop(),
            FilePickerOptions {
                base_path: base.to_string_lossy().to_string(),
                enable_mmap_cache: true,
                enable_content_indexing: true,
                watch: false,
                mode: FFFMode::Neovim,
                ..Default::default()
            },
        )
        .unwrap();

        // Wait for scan but NOT for bigram — drop while post-scan is active
        shared_picker.wait_for_scan(Duration::from_secs(10));

        // Poll until post_scan_indexing_active is true (bigram started)
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut was_active = false;
        loop {
            if let Ok(guard) = shared_picker.read() {
                if let Some(picker) = guard.as_ref() {
                    if picker.is_post_scan_active() {
                        was_active = true;
                        break;
                    }
                }
            }
            if std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        if was_active {
            caught_active += 1;
        }

        // Drop the picker while post_scan_indexing_active is set.
        // Take it out of the shared handle first, then drop outside the lock —
        // Drop spins until post-scan finishes, which needs the write lock for
        // bigram install, so we can't hold it during Drop.
        let old_picker = shared_picker.write().unwrap().take();
        drop(old_picker); // Drop fires here — spins until post-scan exits

        assert!(
            shared_picker.read().unwrap().is_none(),
            "round {round}: picker should be None after drop"
        );
    }

    // The primary invariant — dropping while post-scan may be active must not
    // crash — is exercised every round regardless. Catching the active window
    // is timing-dependent: with a fast walker/scan the post-scan phase can
    // complete before the poll observes it, especially on loaded CI runners.
    // So we only warn (not fail) if no round observed it.
    if caught_active == 0 {
        eprintln!(
            "warning: never observed post_scan_indexing_active=true; \
             drop-safety was still exercised in all rounds ({caught_active}/10)"
        );
    }
    eprintln!("Caught post-scan active in {caught_active}/10 rounds");
}
