//! End-to-end integration tests for the `perf-sentinel ack` subcommand.
//!
//! Each test spawns a minimal hand-rolled HTTP/1.1 mock daemon on
//! `127.0.0.1:0`, runs the CLI as a subprocess targeting that mock,
//! then asserts on exit code, stdout and stderr. Hand-rolled rather
//! than using `wiremock` / `httpmock` to match the project convention
//! in `crates/sentinel-core/src/test_helpers.rs`.

#![cfg(feature = "daemon")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const DAEMON_BIN: &str = env!("CARGO_BIN_EXE_perf-sentinel");

/// Captured request: method, body, X-API-Key header value (if any).
/// `path` is parsed but not retained; tests assert on method+body which
/// are sufficient because each mock is one-shot.
#[derive(Default)]
struct CapturedRequest {
    method: String,
    body: String,
    api_key: Option<String>,
}

/// Configuration for the mock's response on each successive request.
struct ScriptedResponse {
    status: u16,
    reason: &'static str,
    body: &'static str,
    /// If `Some`, the mock returns 401 unless the request carries this
    /// X-API-Key value. Used to test the env-var auth path.
    require_api_key: Option<&'static str>,
}

impl ScriptedResponse {
    fn ok_201(body: &'static str) -> Self {
        Self {
            status: 201,
            reason: "Created",
            body,
            require_api_key: None,
        }
    }
    fn no_content_204() -> Self {
        Self {
            status: 204,
            reason: "No Content",
            body: "",
            require_api_key: None,
        }
    }
    fn ok_200(body: &'static str) -> Self {
        Self {
            status: 200,
            reason: "OK",
            body,
            require_api_key: None,
        }
    }
    fn status(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            body: "",
            require_api_key: None,
        }
    }
    fn require_key(self, key: &'static str) -> Self {
        Self {
            require_api_key: Some(key),
            ..self
        }
    }
}

/// Spawn a mock HTTP/1.1 daemon serving `script` consecutive requests.
/// Returns the port and the captured-request log. The thread shuts
/// down after serving the script.
fn spawn_mock(script: Vec<ScriptedResponse>) -> (u16, Arc<Mutex<Vec<CapturedRequest>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let log_clone = log.clone();

    thread::spawn(move || {
        for response in script {
            let (stream, _) = match listener.accept() {
                Ok(v) => v,
                Err(_) => break,
            };
            handle_one_request(stream, response, &log_clone);
        }
    });

    (port, log)
}

fn handle_one_request(
    mut stream: TcpStream,
    response: ScriptedResponse,
    log: &Arc<Mutex<Vec<CapturedRequest>>>,
) {
    // Hyper sends `Content-Length` whenever it serializes a `Full<Bytes>`
    // body, so this hand-rolled parser does not need a chunked-encoding
    // path. A 5s read timeout shields the test from a misbehaving
    // request that never sends the blank line.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let parts: Vec<&str> = request_line.trim_end().split(' ').collect();
    let method = parts.first().copied().unwrap_or("").to_string();

    let mut content_length: usize = 0;
    let mut api_key: Option<String> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
        if let Some(rest) = lower.strip_prefix("x-api-key:") {
            api_key = Some(rest.trim().to_string());
        }
    }

    let mut body_buf = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body_buf);
    }
    let body = String::from_utf8_lossy(&body_buf).to_string();

    log.lock().expect("log lock").push(CapturedRequest {
        method: method.clone(),
        body: body.clone(),
        api_key: api_key.clone(),
    });

    let (status, reason, response_body) = if let Some(expected_key) = response.require_api_key {
        if api_key.as_deref() == Some(expected_key) {
            (response.status, response.reason, response.body)
        } else {
            (401, "Unauthorized", "")
        }
    } else {
        (response.status, response.reason, response.body)
    };

    let response_text = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {response_body}",
        response_body.len()
    );
    let _ = stream.write_all(response_text.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(DAEMON_BIN)
        .args(args)
        .stdin(Stdio::null())
        .env_remove("PERF_SENTINEL_DAEMON_API_KEY")
        .env_remove("PERF_SENTINEL_DAEMON_URL")
        .output()
        .expect("failed to execute perf-sentinel")
}

fn run_cli_with_env(args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(DAEMON_BIN);
    cmd.args(args)
        .stdin(Stdio::null())
        .env_remove("PERF_SENTINEL_DAEMON_API_KEY")
        .env_remove("PERF_SENTINEL_DAEMON_URL");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to execute perf-sentinel")
}

fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

const TEST_SIG: &str = "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef";

#[test]
fn ack_create_success_returns_0() {
    let (port, _log) = spawn_mock(vec![ScriptedResponse::ok_201("")]);
    let url = url_for(port);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url,
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "deferred to next sprint",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected exit 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Acknowledgment created"),
        "stdout missing summary, got:\n{stdout}"
    );
    assert!(stdout.contains(TEST_SIG));
}

#[test]
fn ack_create_409_returns_2_with_hint() {
    let (port, _log) = spawn_mock(vec![ScriptedResponse::status(409, "Conflict")]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "duplicate",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("already acknowledged"));
    assert!(stderr.contains("perf-sentinel ack revoke"));
}

#[test]
fn ack_create_503_returns_3() {
    let (port, _log) = spawn_mock(vec![ScriptedResponse::status(503, "Service Unavailable")]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "x",
    ]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("daemon ack store is disabled"));
    assert!(stderr.contains("[daemon.ack] enabled = true"));
}

#[test]
fn ack_create_400_returns_2() {
    let (port, _log) = spawn_mock(vec![ScriptedResponse::status(400, "Bad Request")]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "create",
        "--signature",
        "bogus_signature_format",
        "--reason",
        "x",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid signature format"));
}

#[test]
fn ack_create_507_returns_3() {
    let (port, _log) = spawn_mock(vec![ScriptedResponse::status(507, "Insufficient Storage")]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "x",
    ]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("daemon ack store is full"));
}

#[test]
fn ack_create_401_with_env_api_key_succeeds() {
    let (port, log) = spawn_mock(vec![ScriptedResponse::ok_201("").require_key("secret123")]);
    let output = run_cli_with_env(
        &[
            "ack",
            "--daemon",
            &url_for(port),
            "create",
            "--signature",
            TEST_SIG,
            "--reason",
            "x",
        ],
        &[("PERF_SENTINEL_DAEMON_API_KEY", "secret123")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected exit 0, got stderr:\n{stderr}"
    );
    let captured = log.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].api_key.as_deref(), Some("secret123"));
}

#[test]
fn ack_create_401_without_api_key_returns_2() {
    let (port, _log) = spawn_mock(vec![ScriptedResponse::status(401, "Unauthorized")]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "x",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("daemon requires authentication"));
    assert!(stderr.contains("PERF_SENTINEL_DAEMON_API_KEY"));
    assert!(stderr.contains("--api-key-file"));
}

#[test]
fn ack_create_with_iso8601_expires_serialized_correctly() {
    let (port, log) = spawn_mock(vec![ScriptedResponse::ok_201("")]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "x",
        "--expires",
        "2026-05-15T00:00:00Z",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let captured = log.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert!(
        captured[0].body.contains("2026-05-15T00:00:00Z"),
        "request body missing expires_at, got:\n{}",
        captured[0].body
    );
}

#[test]
fn ack_create_with_relative_expires_resolves_to_future_datetime() {
    let (port, log) = spawn_mock(vec![ScriptedResponse::ok_201("")]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "x",
        "--expires",
        "1h",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let captured = log.lock().unwrap();
    assert!(
        captured[0].body.contains("expires_at"),
        "request body missing expires_at, got:\n{}",
        captured[0].body
    );
}

#[test]
fn ack_revoke_204_returns_0() {
    let (port, log) = spawn_mock(vec![ScriptedResponse::no_content_204()]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "revoke",
        "--signature",
        TEST_SIG,
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected exit 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("Acknowledgment revoked"));
    let captured = log.lock().unwrap();
    assert_eq!(captured[0].method, "DELETE");
}

#[test]
fn ack_revoke_404_returns_2() {
    let (port, _log) = spawn_mock(vec![ScriptedResponse::status(404, "Not Found")]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "revoke",
        "--signature",
        TEST_SIG,
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no active acknowledgment"));
}

#[test]
fn ack_list_text_format_renders_table() {
    let body = r#"[
        {"action":"ack","signature":"n_plus_one_sql:svc:_api:0123456789abcdef","by":"alice","reason":"deferred","at":"2026-05-05T12:00:00Z","expires_at":"2026-05-12T13:30:00Z"},
        {"action":"ack","signature":"slow_http:other:_api:fedcba9876543210","by":"bob","reason":null,"at":"2026-05-04T09:15:00Z","expires_at":null}
    ]"#;
    let (port, _log) = spawn_mock(vec![ScriptedResponse::ok_200(body)]);
    let output = run_cli(&["ack", "--daemon", &url_for(port), "list"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("SIGNATURE"));
    assert!(stdout.contains("alice"));
    assert!(stdout.contains("bob"));
    assert!(stdout.contains("never"));
    assert!(stdout.contains("2 daemon acknowledgments active (showing up to 1000)"));
    assert!(stdout.contains(".perf-sentinel-acknowledgments.toml"));
}

#[test]
fn ack_list_json_format_returns_array() {
    let body = r#"[{"action":"ack","signature":"x","by":"a","reason":null,"at":"2026-05-05T12:00:00Z","expires_at":null}]"#;
    let (port, _log) = spawn_mock(vec![ScriptedResponse::ok_200(body)]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "list",
        "--output",
        "json",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed.is_array());
}

#[test]
fn ack_list_empty_returns_friendly_message() {
    let (port, _log) = spawn_mock(vec![ScriptedResponse::ok_200("[]")]);
    let output = run_cli(&["ack", "--daemon", &url_for(port), "list"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("No active daemon acknowledgments"));
    assert!(stdout.contains(".perf-sentinel-acknowledgments.toml"));
}

#[test]
fn ack_create_network_error_returns_1() {
    // Bind to get a free port, then drop the listener so nothing accepts.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "x",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot reach daemon"));
    assert!(stderr.contains("perf-sentinel watch"));
}

#[test]
fn ack_create_invalid_url_returns_1() {
    let output = run_cli(&[
        "ack",
        "--daemon",
        "ftp://localhost",
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "x",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("scheme must be http or https"));
}

#[test]
fn ack_create_with_api_key_file_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    std::fs::write(&key_path, "topsecret\n").unwrap();
    let (port, log) = spawn_mock(vec![ScriptedResponse::ok_201("").require_key("topsecret")]);
    let output = run_cli(&[
        "ack",
        "--daemon",
        &url_for(port),
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "x",
        "--api-key-file",
        key_path.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected exit 0, got stderr:\n{stderr}"
    );
    let captured = log.lock().unwrap();
    assert_eq!(captured[0].api_key.as_deref(), Some("topsecret"));
}

#[test]
fn ack_create_with_missing_api_key_file_returns_1() {
    let output = run_cli(&[
        "ack",
        "--daemon",
        "http://127.0.0.1:1",
        "create",
        "--signature",
        TEST_SIG,
        "--reason",
        "x",
        "--api-key-file",
        "/nonexistent/path/key",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot read --api-key-file"));
}
