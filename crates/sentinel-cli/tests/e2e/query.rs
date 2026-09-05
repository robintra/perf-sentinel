//! `perf-sentinel query incidents` against a hand-rolled mock daemon.
//!
//! The mock is a one-shot HTTP/1.1 server on `127.0.0.1:0` that records
//! the request line and the `X-API-Key` header, then answers a scripted
//! status and body. Same convention as `tests/cli_ack.rs`, trimmed to
//! what a GET needs.

#![cfg(feature = "daemon")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// What the mock saw: the request line and the `X-API-Key` value.
struct Seen {
    request_line: String,
    api_key: Option<String>,
}

/// Serve one request with `status`/`body` and hand back what was seen.
fn spawn_mock(
    status: u16,
    reason: &'static str,
    body: &'static str,
) -> (u16, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut request_line = String::new();
        let _ = reader.read_line(&mut request_line);
        let mut api_key = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                break;
            }
            if let Some(rest) = line.to_ascii_lowercase().strip_prefix("x-api-key:") {
                api_key = Some(rest.trim().to_string());
            }
        }
        let _ = tx.send(Seen {
            request_line: request_line.trim_end().to_string(),
            api_key,
        });
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    (port, rx)
}

fn run_query_incidents(port: u16, extra: &[&str], key_env: Option<&str>) -> std::process::Output {
    let url = format!("http://127.0.0.1:{port}");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"));
    cmd.args(["query", "--daemon", &url, "incidents"])
        .args(extra)
        .stdin(Stdio::null())
        .env_remove("PERF_SENTINEL_DAEMON_API_KEY")
        .env_remove("PERF_SENTINEL_DAEMON_URL");
    if let Some(key) = key_env {
        cmd.env("PERF_SENTINEL_DAEMON_API_KEY", key);
    }
    cmd.output().expect("failed to execute perf-sentinel")
}

const TWO_INCIDENTS: &str = r#"[
  {"id":"0123456789abcdef0123456789abcdef","service":"cart-svc","kind":"oom_kill",
   "at_ms":1700000400000,"detail":"container exceeded its memory limit",
   "window_from_ms":1700000100000,"window_to_ms":1700000460000,"oldest_finding_ms":1700000050000,
   "findings":[{"finding":{"type":"n_plus_one_sql","severity":"critical","trace_id":"t1",
     "service":"cart-svc","source_endpoint":"GET /cart",
     "pattern":{"template":"select * from items where id = ?","occurrences":40,"window_ms":100,"distinct_params":40},
     "suggestion":"batch the lookups","first_timestamp":"2026-09-01T14:00:00Z","last_timestamp":"2026-09-01T14:02:00Z",
     "confidence":"daemon_production"},"stored_at_ms":1700000300000,"first_seen_ms":1700000100000,"seen_count":12}]},
  {"id":"fedcba9876543210fedcba9876543210","service":"gateway-svc","kind":"restart",
   "at_ms":1700000900000,"ended_at_ms":1700000960000,
   "window_from_ms":1700000600000,"window_to_ms":1700000960000,"oldest_finding_ms":1700000700000,"findings":[]}
]"#;

#[test]
fn cli_query_incidents_help_documents_the_key_and_paging() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["query", "incidents", "--help"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "--api-key-file",
        "PERF_SENTINEL_DAEMON_API_KEY",
        "read_api_key",
        "--offset",
        "--limit",
        "--service",
        "--format",
    ] {
        assert!(
            stdout.contains(expected),
            "query incidents --help must mention {expected}, got:\n{stdout}"
        );
    }
}

#[test]
fn cli_query_incidents_sends_the_key_and_names_a_401() {
    let (port, seen) = spawn_mock(
        401,
        "Unauthorized",
        r#"{"error":"missing or invalid X-API-Key"}"#,
    );
    let output = run_query_incidents(
        port,
        &["--service", "cart-svc", "--limit", "5"],
        Some("read-key-123456"),
    );
    let seen = seen
        .recv_timeout(Duration::from_secs(5))
        .expect("mock saw the request");
    assert_eq!(seen.api_key.as_deref(), Some("read-key-123456"));
    assert!(
        seen.request_line
            .starts_with("GET /api/incidents?offset=0&limit=5&service=cart-svc "),
        "request line: {}",
        seen.request_line
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(stderr.contains("(401)"), "stderr:\n{stderr}");
    assert!(stderr.contains("--api-key-file"), "stderr:\n{stderr}");
    assert!(
        stderr.contains("read_api_key suffices"),
        "stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("Is `perf-sentinel watch` running?"),
        "a refusal is not an unreachable daemon:\n{stderr}"
    );
}

#[test]
fn cli_query_incidents_names_a_disabled_store() {
    let (port, _seen) = spawn_mock(
        503,
        "Service Unavailable",
        r#"{"error":"incident store disabled"}"#,
    );
    let output = run_query_incidents(port, &[], None);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(
        stderr.contains("[daemon.incidents] enabled = false"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn cli_query_incidents_renders_text_and_json() {
    let (port, _seen) = spawn_mock(200, "OK", TWO_INCIDENTS);
    let output = run_query_incidents(port, &[], None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("daemon incidents (2)"), "stdout:\n{stdout}");
    assert!(stdout.contains("#1 oom_kill"), "stdout:\n{stdout}");
    assert!(stdout.contains("cart-svc"), "stdout:\n{stdout}");
    assert!(stdout.contains("capture complete"), "stdout:\n{stdout}");
    assert!(stdout.contains("Found 1 finding(s)"), "stdout:\n{stdout}");
    assert!(stdout.contains("#2 restart"), "stdout:\n{stdout}");
    assert!(stdout.contains("capture partial"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("No findings in the window."),
        "stdout:\n{stdout}"
    );

    let (port, _seen) = spawn_mock(200, "OK", TWO_INCIDENTS);
    let output = run_query_incidents(port, &["--format", "json"], None);
    assert_eq!(output.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("pretty JSON");
    assert_eq!(json.as_array().map(Vec::len), Some(2));
    assert_eq!(json[1]["kind"], "restart");
}

#[test]
fn cli_query_incidents_refuses_a_malformed_body() {
    // A detector this build does not know, or a proxy's HTML page, must
    // never read as "No incidents recorded".
    let (port, _seen) = spawn_mock(
        200,
        "OK",
        Box::leak(
            TWO_INCIDENTS
                .replace("n_plus_one_sql", "future_kind")
                .into_boxed_str(),
        ),
    );
    let output = run_query_incidents(port, &[], None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stdout:\n{stdout}");
    assert!(stderr.contains("malformed response"), "stderr:\n{stderr}");
    assert!(stderr.contains("future_kind"), "stderr:\n{stderr}");
    assert!(
        !stdout.contains("No incidents recorded"),
        "stdout:\n{stdout}"
    );
}
