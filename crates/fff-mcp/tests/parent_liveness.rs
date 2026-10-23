use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_fff-mcp");

#[test]
fn stays_alive_while_parent_alive_despite_idle_timeout() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

    let mut child = Command::new(BIN)
        .arg(dir.path())
        .args([
            "--no-update-check",
            "--no-warmup",
            "--no-watch",
            "--idle-timeout-secs",
            "1",
            "--indexing",
            "eager",
        ])
        .arg("--frecency-db")
        .arg(dir.path().join("frecency"))
        .arg("--log-file")
        .arg(dir.path().join("test.log"))
        .env("FFF_MCP_TEST_WATCHDOG_INTERVAL_MS", "100")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout_lines = spawn_line_reader(child.stdout.take().unwrap());
    do_handshake(&mut stdin, &stdout_lines);
    assert!(dir.path().join("frecency").exists());

    // Wait past the idle timeout and several watchdog ticks.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        child.try_wait().unwrap().is_none(),
        "fff-mcp exited on idle timeout even though its parent is alive"
    );

    // Closing stdin ends the transport; the server must still shut down cleanly.
    drop(stdin);
    wait_for_exit(&mut child, Duration::from_secs(15));
}

#[test]
fn serves_stateless_requests_from_multiple_callers_and_exits_on_eof() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
    let mut child = Command::new(BIN)
        .arg(dir.path())
        .args([
            "--no-update-check",
            "--no-warmup",
            "--no-watch",
            "--indexing",
            "lazy",
        ])
        .arg("--frecency-db")
        .arg(dir.path().join("frecency"))
        .arg("--log-file")
        .arg(dir.path().join("test.log"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let lines = spawn_line_reader(child.stdout.take().unwrap());
    let meta = serde_json::json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "protocol-test", "version": "1"}
    });
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0", "id": 0, "method": "server/discover", "params": {"_meta": meta}
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    let discovery: serde_json::Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert!(
        discovery["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("2026-07-28")),
        "{discovery}"
    );
    assert_eq!(
        discovery["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "fff"
    );
    assert!(!dir.path().join("frecency").exists());

    // Pipeline real searches from distinct callers over one stateless connection.
    for id in 1..=32 {
        let mut caller = meta.clone();
        caller["threadId"] = serde_json::json!(format!("child-{id}"));
        caller["x-codex-turn-metadata"] = serde_json::json!({"parent_thread_id": "root"});
        let (name, arguments) = match id % 3 {
            0 => ("find_files", serde_json::json!({"query": "a.txt"})),
            1 => ("grep", serde_json::json!({"query": "hello"})),
            _ => ("multi_grep", serde_json::json!({"patterns": ["hello"]})),
        };
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": name, "arguments": arguments, "_meta": caller}
            })
        )
        .unwrap();
    }
    stdin.flush().unwrap();
    let mut received = std::collections::BTreeSet::new();
    for _ in 1..=32 {
        let response: serde_json::Value =
            serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("a.txt")
        );
        assert!(received.insert(response["id"].as_u64().unwrap()));
    }
    assert_eq!(received, (1..=32).collect());
    assert!(dir.path().join("frecency").exists());

    // Later calls reuse the initial scan, including when watching is disabled.
    std::fs::write(dir.path().join("late-added.txt"), "late").unwrap();
    writeln!(stdin, "{}", serde_json::json!({
        "jsonrpc": "2.0", "id": 34, "method": "tools/call",
        "params": {"name": "find_files", "arguments": {"query": "late-added.txt", "mode": "glob"}, "_meta": meta}
    })).unwrap();
    stdin.flush().unwrap();
    let response: serde_json::Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("0 matching paths"),
        "{response}"
    );

    let mut unsupported = meta;
    unsupported["io.modelcontextprotocol/protocolVersion"] = serde_json::json!("2099-01-01");
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0", "id": 33, "method": "tools/list", "params": {"_meta": unsupported}
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    let response: serde_json::Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert_eq!(response["error"]["code"], -32022);

    drop(stdin);
    wait_for_exit(&mut child, Duration::from_secs(15));
    assert!(child.wait().unwrap().success());
}

#[test]
fn discovery_does_not_prevent_a_legacy_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(BIN)
        .arg(dir.path())
        .args([
            "--no-update-check",
            "--no-warmup",
            "--no-watch",
            "--indexing",
            "lazy",
        ])
        .arg("--frecency-db")
        .arg(dir.path().join("frecency"))
        .arg("--log-file")
        .arg(dir.path().join("test.log"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let lines = spawn_line_reader(child.stdout.take().unwrap());
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0", "id": "discover", "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    let discovery: serde_json::Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert!(discovery.get("result").is_some(), "{discovery}");
    do_handshake(&mut stdin, &lines);
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    let response: serde_json::Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert!(
        response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "find_files")
    );
    assert!(!dir.path().join("frecency").exists());
    drop(stdin);
    wait_for_exit(&mut child, Duration::from_secs(15));
    assert!(child.wait().unwrap().success());
}

#[test]
fn lazy_index_failure_is_reported_without_hanging_or_retrying_setup() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    let mut child = Command::new(BIN)
        .arg(&missing)
        .args([
            "--no-update-check",
            "--no-warmup",
            "--no-watch",
            "--indexing",
            "lazy",
        ])
        .arg("--log-file")
        .arg(dir.path().join("test.log"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let lines = spawn_line_reader(child.stdout.take().unwrap());
    do_handshake(&mut stdin, &lines);
    for id in 2..=3 {
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": "find_files", "arguments": {"query": "a.txt"}}
            })
        )
        .unwrap();
        stdin.flush().unwrap();
        let response: serde_json::Value =
            serde_json::from_str(&lines.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        assert_eq!(response["error"]["code"], -32603, "{response}");
        if id == 2 {
            std::fs::create_dir(&missing).unwrap();
            std::fs::write(missing.join("a.txt"), "hello").unwrap();
        }
    }
    drop(stdin);
    wait_for_exit(&mut child, Duration::from_secs(15));
    assert!(child.wait().unwrap().success());
}

#[cfg(unix)]
#[test]
fn exits_when_parent_dies_even_without_idle_timeout() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
    let log_path = dir.path().join("test.log");
    let exit_signal = dir.path().join("exit-parent");

    // Intermediary parent: sh backgrounds fff-mcp and waits until the handshake
    // completes before dying and orphaning it.
    let mut sh = Command::new("sh")
        .arg("-c")
        .arg(
            // Preserve stdin before POSIX shells assign /dev/null to background jobs.
            r#"exec 3<&0
                "$1" "$2" --no-update-check --no-warmup --no-watch \
                --idle-timeout-secs 0 --log-file "$3" --frecency-db "$2/frecency" <&3 &
                while [ ! -e "$4" ]; do sleep 0.1; done"#,
        )
        .arg("sh")
        .arg(BIN)
        .arg(dir.path())
        .arg(&log_path)
        .arg(&exit_signal)
        .env("FFF_MCP_TEST_WATCHDOG_INTERVAL_MS", "100")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = sh.stdin.take().unwrap();
    let stdout_lines = spawn_line_reader(sh.stdout.take().unwrap());
    do_handshake(&mut stdin, &stdout_lines);
    // A freshly spawned parent selects eager indexing in auto mode.
    assert!(dir.path().join("frecency").exists());

    std::fs::write(exit_signal, "").unwrap();
    sh.wait().unwrap();

    // We still hold the stdin write end, so the only exit path is the parent
    // liveness check. EOF on stdout means fff-mcp closed it by exiting.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match stdout_lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("fff-mcp did not exit within 5s of its parent dying")
            }
        }
    }
    drop(stdin);

    let logs = read_session_logs(dir.path());
    assert!(
        logs.contains("Parent process") && logs.contains("exited, shutting down"),
        "expected parent-death exit reason in logs, got:\n{}",
        logs
    );
}

#[cfg(unix)]
#[test]
fn exits_when_parent_dies_after_discovery_without_initializing() {
    let dir = tempfile::tempdir().unwrap();
    let exit_signal = dir.path().join("exit-parent");
    let mut sh = Command::new("sh")
        .arg("-c")
        .arg(
            r#"exec 3<&0
            "$1" "$2" --no-update-check --no-warmup --no-watch \
            --idle-timeout-secs 0 --log-file "$3" --indexing lazy <&3 &
            while [ ! -e "$4" ]; do sleep 0.1; done"#,
        )
        .arg("sh")
        .arg(BIN)
        .arg(dir.path())
        .arg(dir.path().join("test.log"))
        .arg(&exit_signal)
        .env("FFF_MCP_TEST_WATCHDOG_INTERVAL_MS", "100")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = sh.stdin.take().unwrap();
    let lines = spawn_line_reader(sh.stdout.take().unwrap());
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    let discovery: serde_json::Value =
        serde_json::from_str(&lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
    assert!(discovery.get("result").is_some(), "{discovery}");
    std::fs::write(exit_signal, "").unwrap();
    sh.wait().unwrap();
    // Keep stdin open: only parent monitoring can end this discovery-only connection.
    assert_eq!(
        lines.recv_timeout(Duration::from_secs(5)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
    drop(stdin);
}

fn do_handshake(stdin: &mut ChildStdin, stdout_lines: &mpsc::Receiver<String>) {
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "parent-liveness-test", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{}", initialize).unwrap();
    stdin.flush().unwrap();

    let response = stdout_lines
        .recv_timeout(Duration::from_secs(30))
        .expect("no initialize response within 30s");
    assert!(
        response.contains("\"serverInfo\""),
        "unexpected initialize response: {}",
        response
    );

    writeln!(
        stdin,
        "{}",
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
    )
    .unwrap();
    stdin.flush().unwrap();
}

fn spawn_line_reader(stdout: std::process::ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn wait_for_exit(child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.kill().ok();
    panic!(
        "fff-mcp did not exit within {:?} after stdin closed",
        timeout
    );
}

#[cfg(unix)]
fn read_session_logs(dir: &std::path::Path) -> String {
    let mut combined = String::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("test") && name.ends_with(".log") {
            combined.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    combined
}
