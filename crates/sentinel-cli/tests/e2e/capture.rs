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
fn cli_capture_does_not_report_success_for_a_signal_killed_command() {
    // A test JVM killed by the OOM killer must not read as a green build.
    // `ExitStatus::code()` is None for a signal death, and reporting 0 there
    // would let a half-run suite pass the gate.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("traces.json");
    let mut argv = args(out.to_str().unwrap(), 34325, 34326);
    argv.extend([
        "--".into(),
        "sh".into(),
        "-c".into(),
        "kill -TERM $$".into(),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(&argv)
        .output()
        .expect("spawn capture");

    let code = output.status.code();
    assert_ne!(code, Some(0), "a signal-killed command must not exit 0");
    assert_eq!(code, Some(143), "128 + SIGTERM, the shell convention");
}

#[test]
fn cli_capture_does_not_start_the_command_when_the_port_is_taken() {
    // The bind must happen before the spawn. Otherwise a bind failure leaves
    // the test suite running detached while perf-sentinel exits.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("traces.json");
    let marker = dir.path().join("command-ran");
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = squatter.local_addr().unwrap().port();

    let mut argv = args(out.to_str().unwrap(), taken, 34328);
    argv.extend([
        "--".into(),
        "touch".into(),
        marker.to_str().unwrap().to_string(),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(&argv)
        .output()
        .expect("spawn capture");

    assert!(!output.status.success());
    assert!(
        !marker.exists(),
        "the wrapped command must never start when the capture cannot listen"
    );
}

#[cfg(unix)]
#[test]
fn cli_capture_stops_the_whole_command_tree_on_signal() {
    // `mvn` forks a test JVM. Signalling only the direct child leaves that
    // fork holding its port and its database connection on an agent that
    // thinks the step is over.
    use std::io::Read;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("traces.json");
    // The grandchild records its own pid, then sleeps well past the test. A
    // witness file written *after* the sleep would prove nothing, it would be
    // absent either way within the test's lifetime.
    let pidfile = dir.path().join("grandchild.pid");
    let mut argv = args(out.to_str().unwrap(), 34329, 34330);
    argv.extend([
        "--".into(),
        "sh".into(),
        "-c".into(),
        // No `exec`: the shell must stay as a middle process, so `sleep` is a
        // real grandchild. That is the shape `mvn` plus its Failsafe fork has,
        // and the one that survives when only the direct child is signalled.
        format!("sleep 120 & echo $! > {}; wait", pidfile.display()),
    ]);

    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(&argv)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn capture");

    // Wait for the listeners, then signal the capture the way a cancelled CI
    // job would.
    let mut stderr = child.stderr.take().expect("stderr piped");
    let mut seen = String::new();
    let mut buf = [0u8; 512];
    while !seen.contains("capture listening") {
        let n = stderr.read(&mut buf).expect("read stderr");
        assert!(n > 0, "capture exited before listening: {seen}");
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    // The pid of the backgrounded `sleep`, written a moment after the
    // listeners come up, hence the wait.
    let mut grandchild = None;
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(&pidfile)
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            grandchild = Some(pid);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let grandchild = grandchild.expect("grandchild wrote its pid");

    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let status = child.wait().expect("wait capture");
    assert!(!status.success(), "a signalled run is not a success");

    std::thread::sleep(std::time::Duration::from_millis(500));
    // Signal 0 only probes: alive means the group was never signalled, and in
    // a real job that would be a Failsafe fork still holding its port.
    let alive = unsafe { libc::kill(grandchild, 0) } == 0;
    if alive {
        unsafe { libc::kill(grandchild, libc::SIGKILL) };
    }
    assert!(
        !alive,
        "grandchild {grandchild} survived the signal, it would hold the agent's ports"
    );
}

#[test]
fn cli_capture_blames_the_exporter_not_the_writer_on_an_unusable_request() {
    // A wrong Content-Type is a misconfigured exporter, not backpressure.
    // Reporting it as "faster than the writer" sends the operator to resize a
    // queue that is not the problem.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("traces.json");
    let mut argv = args(out.to_str().unwrap(), 34331, 34332);
    argv.extend([
        "--".into(),
        "sh".into(),
        "-c".into(),
        "printf 'POST /v1/traces HTTP/1.1\\r\\nHost: h\\r\\nContent-Type: application/json\\r\\nContent-Length: 2\\r\\nConnection: close\\r\\n\\r\\n{}' | nc 127.0.0.1 34332 || true".into(),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(&argv)
        .output()
        .expect("spawn capture");

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.contains("refused as unusable") {
        // `nc` is not everywhere; skip rather than fail on a missing tool.
        assert!(
            stderr.contains("no traces received"),
            "unexpected capture output: {stderr}"
        );
        return;
    }
    assert!(
        stderr.contains("OTEL_EXPORTER_OTLP_PROTOCOL"),
        "the message must name the setting to fix: {stderr}"
    );
    assert!(
        !stderr.contains("faster than the writer"),
        "an unusable request is not backpressure: {stderr}"
    );
    assert_eq!(output.status.code(), Some(2));
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
