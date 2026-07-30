//! `capture` subcommand: wrapper mode, service mode, and what happens when
//! nothing was exported.
//!
//! Receiving actual OTLP is covered by the core's `capture` tests, which own
//! a protobuf encoder. What matters here is the process contract a CI job
//! depends on: exit codes, streams, and the file left behind.

use std::process::{Command, Stdio};

/// Ports well away from the 4317/4318 defaults so these tests never collide
/// with a daemon, or with each other.
fn args(output: &str, grpc: u16, http: u16) -> Vec<String> {
    vec![
        "capture".to_string(),
        "--output".to_string(),
        output.to_string(),
        "--listen-port-grpc".to_string(),
        grpc.to_string(),
        "--listen-port-http".to_string(),
        http.to_string(),
        "--grace-ms".to_string(),
        "100".to_string(),
    ]
}

#[test]
fn cli_capture_propagates_the_wrapped_command_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("traces.json");
    let mut argv = args(out.to_str().unwrap(), 34317, 34318);
    argv.extend(["--".into(), "sh".into(), "-c".into(), "exit 3".into()]);

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(&argv)
        .output()
        .expect("spawn capture");

    assert_eq!(
        output.status.code(),
        Some(3),
        "a failing test step must stay a failing job; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_capture_leaves_the_wrapped_command_output_untouched() {
    // The wrapped command owns stdout. perf-sentinel must not interpose on
    // it, reformat it, or add to it, or a CI script parsing that stream
    // breaks the day capture is switched on.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("traces.json");
    let mut argv = args(out.to_str().unwrap(), 34319, 34320);
    argv.extend([
        "--".into(),
        "sh".into(),
        "-c".into(),
        "echo out-line; echo err-line >&2".into(),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(&argv)
        .output()
        .expect("spawn capture");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "out-line\n",
        "stdout must carry the command's output and nothing else"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("err-line"), "stderr={stderr}");
}

#[test]
fn cli_capture_says_so_when_nothing_was_exported() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("traces.json");
    let mut argv = args(out.to_str().unwrap(), 34321, 34322);
    argv.extend(["--".into(), "true".into()]);

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(&argv)
        .output()
        .expect("spawn capture");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no traces received"),
        "an empty capture must be called out, not left to the reader; stderr={stderr}"
    );
    assert!(out.exists(), "the file is created even when empty");
}

#[test]
fn cli_analyze_rejects_an_empty_capture_rather_than_passing_it() {
    // The trap this closes: a capture that received nothing, analyzed with
    // --ci, must not report a clean gate. Zero measured spans is a tooling
    // failure, not a passing build.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("traces.json");
    std::fs::write(&out, "").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--ci", "--input", out.to_str().unwrap()])
        .output()
        .expect("spawn analyze");

    assert!(
        !output.status.success(),
        "an empty trace file must not pass"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("trace file is empty"),
        "the message must name the cause; stderr={stderr}"
    );
}

#[test]
fn cli_capture_fails_clearly_when_the_port_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("traces.json");
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = squatter.local_addr().unwrap().port();

    let mut argv = args(out.to_str().unwrap(), taken, 34324);
    argv.extend(["--".into(), "true".into()]);
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(&argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn capture");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot bind"),
        "the operator needs to know which port; stderr={stderr}"
    );
}
