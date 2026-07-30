//! `capture` runner: receive OTLP into a trace file `analyze --ci` can gate on.
//!
//! Two shapes, one subcommand. Without a trailing command it runs alongside an
//! existing test step and stops on a signal, which is the drop-in replacement
//! for a Collector in a pipeline that already exists. With `-- <command>` it
//! wraps the test step, which removes the start-up race and the question of
//! when to stop.

use std::path::Path;
use std::time::Duration;

use sentinel_core::capture::{CaptureConfig, CaptureStats};

/// Exit code when the capture itself failed (port taken, unwritable file).
const EXIT_CAPTURE_FAILED: i32 = 1;
/// Exit code when the trace file is incomplete. Distinct from a gate breach:
/// nothing was measured wrong, the measurement itself is short.
const EXIT_TRUNCATED: i32 = 2;

/// Run the capture, returning the process exit code.
///
/// In wrapper mode the wrapped command's exit code wins, because a failed test
/// run is the more important signal. A truncated capture still overrides a
/// successful command: a file that silently misses spans would produce a
/// falsely clean verdict downstream.
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
        max_file_bytes: max_file_size_mb.saturating_mul(1024 * 1024),
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
            return EXIT_CAPTURE_FAILED;
        }
    };
    report(&cfg, &stats);

    if let Some(code) = command_code
        && code != 0
    {
        return code;
    }
    if stats.truncated {
        return EXIT_TRUNCATED;
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
    let spawned = tokio::process::Command::new(program).args(args).spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            eprintln!("Capture error: cannot run {program}: {e}");
            return (Ok(CaptureStats::default()), Some(EXIT_CAPTURE_FAILED));
        }
    };

    // The listeners are already bound by the time `run` awaits this future,
    // so the command can never export into a port that is not up yet.
    let mut code = None;
    let result = sentinel_core::capture::run(cfg, async {
        match child.wait().await {
            Ok(status) => code = status.code(),
            Err(e) => {
                eprintln!("Capture error: waiting on {program} failed: {e}");
                code = Some(EXIT_CAPTURE_FAILED);
            }
        }
    })
    .await;
    (result, code.or(Some(0)))
}

/// One-line summary on stderr. Never stdout: in wrapper mode that stream
/// belongs to the wrapped command, and a CI script may be parsing it.
fn report(cfg: &CaptureConfig, stats: &CaptureStats) {
    if stats.requests == 0 {
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
    if stats.truncated {
        eprintln!(
            "Capture: size limit reached, {} is incomplete. Raise \
             --max-file-size or narrow the test scope.",
            cfg.output.display()
        );
    }
}
