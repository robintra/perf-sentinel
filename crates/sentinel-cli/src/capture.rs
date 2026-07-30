//! `capture` runner: receive OTLP into a trace file `analyze --ci` can gate on.
//!
//! Two shapes, one subcommand. Without a trailing command it runs alongside an
//! existing test step and stops on a signal, which is the drop-in replacement
//! for a Collector in a pipeline that already exists. With `-- <command>` it
//! wraps the test step, which removes the start-up race and the question of
//! when to stop.

use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

use sentinel_core::capture::{CaptureConfig, CaptureStats};

/// Exit code when the capture itself failed (port taken, unwritable file).
const EXIT_CAPTURE_FAILED: i32 = 1;
/// Exit code when the trace file is short of the run. Distinct from a gate
/// breach: nothing was measured wrong, the measurement itself is incomplete.
const EXIT_INCOMPLETE: i32 = 2;

/// Run the capture, returning the process exit code.
///
/// The wrapped command's code wins, even when the capture also failed: a
/// failed test run is the more important signal. An incomplete file overrides
/// a successful command, since it would otherwise gate falsely clean.
pub async fn cmd_capture(
    output: &Path,
    listen_address: String,
    port_grpc: u16,
    port_http: u16,
    max_file_size_mb: u64,
    grace_ms: u64,
    command: &[String],
) -> i32 {
    let cfg = CaptureConfig {
        listen_addr: listen_address,
        port_grpc,
        port_http,
        output: output.to_path_buf(),
        max_file_bytes: capped_file_bytes(max_file_size_mb),
        grace: Duration::from_millis(grace_ms),
    };

    let (result, command_code) = match command {
        [] => (sentinel_core::capture::run_until_signal(&cfg).await, None),
        [program, args @ ..] => run_wrapped(&cfg, program, args).await,
    };

    let stats = match result {
        Ok(stats) => stats,
        Err(e) => {
            eprintln!("Capture error: {e}");
            return command_code
                .filter(|c| *c != 0)
                .unwrap_or(EXIT_CAPTURE_FAILED);
        }
    };
    report(&cfg, &stats);

    if let Some(code) = command_code
        && code != 0
    {
        return code;
    }
    if stats.is_incomplete() {
        return EXIT_INCOMPLETE;
    }
    0
}

/// Capture for the lifetime of `program`, which inherits this process's
/// stdout and stderr: its logs must reach the CI console exactly as they do
/// without capture, unbuffered and uncoloured by us.
async fn run_wrapped(
    cfg: &CaptureConfig,
    program: &str,
    args: &[String],
) -> (
    Result<CaptureStats, sentinel_core::capture::CaptureError>,
    Option<i32>,
) {
    // Bind and open BEFORE spawning: no export can hit a listener that is not
    // up yet, and a bind failure cannot orphan a running test suite.
    let capture = match sentinel_core::capture::start(cfg).await {
        Ok(capture) => capture,
        Err(e) => return (Err(e), None),
    };

    let spawned = tokio::process::Command::new(program).args(args).spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            eprintln!("Capture error: cannot run {program}: {e}");
            let _ = capture.finish().await;
            return (Ok(CaptureStats::default()), Some(EXIT_CAPTURE_FAILED));
        }
    };

    // A cancelled CI job signals us, not the child. Handling it here flushes
    // the writer and kills the child rather than orphaning it.
    let code = tokio::select! {
        waited = child.wait() => match waited {
            Ok(status) => Some(exit_code_of(status)),
            Err(e) => {
                eprintln!("Capture error: waiting on {program} failed: {e}");
                Some(EXIT_CAPTURE_FAILED)
            }
        },
        () = sentinel_core::capture::shutdown_signal() => {
            eprintln!("Capture: stopping on signal, terminating {program}");
            let _ = child.kill().await;
            Some(EXIT_CAPTURE_FAILED)
        }
    };
    (capture.finish().await, code.or(Some(0)))
}

/// Clamp to what the batch readers will accept, so capture cannot write a
/// file `analyze` then refuses to read.
fn capped_file_bytes(requested_mb: u64) -> u64 {
    let requested = requested_mb.saturating_mul(1024 * 1024);
    let cap = crate::limits::MAX_BATCH_INPUT_BYTES as u64;
    if requested > cap {
        eprintln!(
            "Capture: --max-file-size {requested_mb} MiB exceeds the {} MiB \
             analyze can read, capping there.",
            cap / (1024 * 1024)
        );
        return cap;
    }
    requested
}

/// A signal-terminated command is a failure, not a success. `code()` is
/// `None` for that case on Unix, and reporting 0 would turn an OOM-killed
/// test suite into a green build.
fn exit_code_of(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            // Shell convention: what running the command under sh reports.
            return 128 + signal;
        }
    }
    EXIT_CAPTURE_FAILED
}

/// One-line summary on stderr. Never stdout: in wrapper mode that stream
/// belongs to the wrapped command, and a CI script may be parsing it.
fn report(cfg: &CaptureConfig, stats: &CaptureStats) {
    if stats.requests == 0 && !stats.is_incomplete() {
        eprintln!(
            "Capture: no traces received. Is the application exporting to \
             {}:{} (gRPC) or {}:{} (HTTP)?",
            cfg.listen_addr, cfg.port_grpc, cfg.listen_addr, cfg.port_http
        );
        return;
    }
    eprintln!(
        "Capture: {} spans in {} requests written to {}",
        stats.spans,
        stats.requests,
        cfg.output.display()
    );
    // Neither cause is skipped when nothing was written: a run fully dropped
    // by the cap must not read as "your exporter is misconfigured".
    if stats.truncated {
        eprintln!(
            "Capture: size limit reached, {} is incomplete. Raise \
             --max-file-size or narrow the test scope.",
            cfg.output.display()
        );
    }
    if stats.rejected > 0 {
        eprintln!(
            "Capture: {} requests could not be queued and were refused, {} is \
             incomplete. The exporter was faster than the writer.",
            stats.rejected,
            cfg.output.display()
        );
    }
}
