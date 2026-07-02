//! `watch` subcommand startup and shutdown.

use std::process::{Command, Stdio};

#[test]
fn cli_watch_starts_and_responds_to_sigterm() {
    use std::time::Duration;

    // Override the default 4318 / 4317 to a clearly non-default pair
    // distinct from `cli_watch_listen_address_override_starts_cleanly`
    // (24318 / 24317) so the two e2e watch tests can run in parallel
    // without colliding.
    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "watch",
            "--listen-address",
            "127.0.0.1",
            "--listen-port-http",
            "24320",
            "--listen-port-grpc",
            "24319",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn perf-sentinel watch");

    // Give it a moment to start the listeners
    std::thread::sleep(Duration::from_millis(500));

    // Assert the daemon was alive before we send the kill, otherwise a
    // silent bind failure on this side (exit 1) would still satisfy the
    // `!status.success()` check below and pass for the wrong reason.
    let still_running = child.try_wait().expect("try_wait failed").is_none();

    child.kill().expect("failed to kill watch process");
    let output = child.wait_with_output().expect("failed to wait");

    assert!(
        still_running,
        "daemon should have been running before SIGTERM; \
         exit status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(!output.status.success());
}

#[test]
fn cli_watch_help_documents_listen_address_override() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["watch", "--help"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success(), "watch --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--listen-address"),
        "watch --help should advertise --listen-address, got: {stdout}"
    );
    assert!(
        stdout.contains("--listen-port-http"),
        "watch --help should advertise --listen-port-http, got: {stdout}"
    );
    assert!(
        stdout.contains("--listen-port-grpc"),
        "watch --help should advertise --listen-port-grpc, got: {stdout}"
    );
}

#[test]
fn cli_watch_listen_address_override_starts_cleanly() {
    use std::time::Duration;

    // Use ports clearly outside both the default range (4318 / 4317)
    // and the +10000 dogfooding pattern (14318 / 14317) to avoid
    // colliding with a local daemon on a dev machine. The +20000 offset
    // is arbitrary but well outside production deployment territory.
    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "watch",
            "--listen-address",
            "127.0.0.1",
            "--listen-port-http",
            "24318",
            "--listen-port-grpc",
            "24317",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn perf-sentinel watch");

    std::thread::sleep(Duration::from_millis(500));
    let still_running = child.try_wait().expect("try_wait failed").is_none();
    child.kill().expect("failed to kill watch process");
    // Capture stdout / stderr so a failure surfaces the daemon's exit
    // log rather than the bare "daemon should still be running" message.
    let output = child.wait_with_output().expect("failed to wait");
    assert!(
        still_running,
        "daemon should still be running after overrides; \
         exit status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
