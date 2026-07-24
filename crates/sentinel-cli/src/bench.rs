//! `perf-sentinel bench` subcommand: timed pipeline runs over a trace
//! file or synthetic events, reporting latency percentiles, throughput
//! and RSS.

use sentinel_core::config::Config;
use sentinel_core::pipeline;

use crate::{ingest_json_or_exit, limits, read_events};

pub(crate) fn cmd_bench(
    input: Option<&std::path::Path>,
    iterations: u32,
    synthetic_events: Option<usize>,
    services: usize,
    seed: u64,
) {
    // Invalid argument values are usage errors: exit 2, matching clap's
    // usage-error code, not the runtime tooling code 75 used for the input
    // and serialization failures below. See docs/CI.md "Exit codes".
    if iterations == 0 {
        eprintln!("Error: iterations must be >= 1");
        std::process::exit(2);
    }

    let config = Config::default();
    let events = if let Some(target) = synthetic_events {
        if target == 0 {
            eprintln!("Error: --synthetic-events must be >= 1");
            std::process::exit(2);
        }
        sentinel_core::synth::generate_target_events(
            target,
            services.max(1),
            &sentinel_core::synth::PatternMix::default(),
            seed,
        )
    } else {
        let raw = read_events(input, limits::MAX_BATCH_INPUT_BYTES);
        ingest_json_or_exit(&raw, limits::MAX_BATCH_INPUT_BYTES)
        // `raw` drops here: holding the file bytes during the timed runs
        // would inflate every RSS sample by the input size.
    };

    let event_count = events.len();
    if event_count == 0 {
        eprintln!("Error: no events to benchmark");
        std::process::exit(crate::EXIT_TOOLING_ERROR);
    }

    let rss_before = current_rss_bytes();
    let mut durations_ns: Vec<u64> = Vec::with_capacity(iterations as usize);

    for _ in 0..iterations {
        // Clone inside the loop, before the timer starts: the clone cost
        // stays excluded from timing while the working set stays at two
        // copies instead of `iterations` copies (the previous pre-clone
        // harness peaked at iterations x events of RSS).
        let batch = events.clone();
        let start = std::time::Instant::now();
        let _ = pipeline::analyze(batch, &config);
        let elapsed = start.elapsed();
        durations_ns.push(elapsed.as_nanos() as u64);
    }

    // OS high-water mark, not a post-iteration sample: sampling current
    // RSS after analyze returns misses the in-flight allocation peak
    // (the analysis output is already dropped at the sampling point).
    let rss_peak = peak_rss_bytes();

    let (p50_us, p99_us) = compute_latency_percentiles(&durations_ns, event_count);
    let (throughput, total_elapsed_ms) = compute_throughput(&durations_ns, event_count, iterations);

    #[derive(serde::Serialize)]
    struct BenchReport {
        iterations: u32,
        events_per_iteration: usize,
        throughput_events_per_sec: f64,
        latency_per_event_us: LatencyPercentiles,
        rss_before_bytes: Option<usize>,
        rss_peak_bytes: Option<usize>,
        total_elapsed_ms: u64,
        durations_ns: Vec<u64>,
    }

    #[derive(serde::Serialize)]
    struct LatencyPercentiles {
        p50: f64,
        p99: f64,
    }

    let report = BenchReport {
        iterations,
        events_per_iteration: event_count,
        throughput_events_per_sec: throughput,
        latency_per_event_us: LatencyPercentiles {
            p50: p50_us,
            p99: p99_us,
        },
        rss_before_bytes: rss_before,
        rss_peak_bytes: rss_peak,
        total_elapsed_ms,
        durations_ns,
    };

    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("Error serializing bench report: {e}");
            std::process::exit(crate::EXIT_TOOLING_ERROR);
        }
    }
}

/// Compute the per-event p50 and p99 latency in microseconds from a slice
/// of per-iteration nanosecond durations.
pub(crate) fn compute_latency_percentiles(durations_ns: &[u64], event_count: usize) -> (f64, f64) {
    if durations_ns.is_empty() {
        return (0.0, 0.0);
    }
    let mut per_event_ns: Vec<f64> = durations_ns
        .iter()
        .map(|&d| d as f64 / event_count as f64)
        .collect();
    per_event_ns.sort_by(f64::total_cmp);

    let len = per_event_ns.len();
    let last = len - 1;
    let p50_idx = ((len as f64 * 0.50).ceil() as usize)
        .saturating_sub(1)
        .min(last);
    let p99_idx = ((len as f64 * 0.99).ceil() as usize)
        .saturating_sub(1)
        .min(last);
    (
        per_event_ns[p50_idx] / 1000.0,
        per_event_ns[p99_idx] / 1000.0,
    )
}

fn compute_throughput(durations_ns: &[u64], event_count: usize, iterations: u32) -> (f64, u64) {
    let elapsed_nanos: u64 = durations_ns.iter().sum();
    let total_elapsed_ms: u64 = elapsed_nanos / 1_000_000;
    let total_events = event_count as f64 * f64::from(iterations);
    let total_seconds = elapsed_nanos as f64 / 1_000_000_000.0;
    let throughput = if total_seconds > 0.0 {
        total_events / total_seconds
    } else {
        0.0
    };
    (throughput, total_elapsed_ms)
}

/// Parse a kB-valued field of `/proc/self/status` into bytes.
#[cfg(target_os = "linux")]
fn proc_status_bytes(field: &str) -> Option<usize> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines().find(|l| l.starts_with(field)).and_then(|l| {
        l.split_whitespace()
            .nth(1)?
            .parse::<usize>()
            .ok()
            .map(|kb| kb * 1024)
    })
}

/// Process-lifetime peak RSS in bytes (high-water mark). Linux reads
/// `VmHWM`; on macOS `ru_maxrss` already is the lifetime peak, so this
/// matches [`current_rss_bytes`] there. The lifetime scope means the
/// value includes input generation/parsing before the measured loop.
fn peak_rss_bytes() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        proc_status_bytes("VmHWM:")
    }
    #[cfg(target_os = "macos")]
    {
        current_rss_bytes()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Get current RSS (Resident Set Size) in bytes. Best-effort, platform-specific.
/// On macOS `ru_maxrss` is the process-lifetime PEAK, not current usage,
/// so the `rss_before` reading is only a true "current" value on Linux.
#[allow(clippy::missing_const_for_fn)] // not const on Linux (reads /proc)
fn current_rss_bytes() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        proc_status_bytes("VmRSS:")
    }
    #[cfg(target_os = "windows")]
    {
        // Not implemented on Windows: always None, callers skip the RSS delta.
        None
    }
    #[cfg(target_os = "macos")]
    {
        use std::mem;
        // SAFETY: libc::rusage is a C struct of numeric fields, zeroing it is valid initialization.
        let mut usage: libc::rusage = unsafe { mem::zeroed() };
        // SAFETY: getrusage is a POSIX syscall that writes into the provided rusage pointer.
        // The pointer is valid (stack-allocated) and the return value is checked below.
        let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) };
        if ret == 0 {
            // On macOS, ru_maxrss is in bytes
            Some(usage.ru_maxrss as usize)
        } else {
            None
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}
