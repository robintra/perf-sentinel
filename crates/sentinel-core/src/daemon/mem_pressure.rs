//! Memory-pressure admission control: poll the pod's cgroup v2 memory
//! usage and flip a shared flag when it crosses the configured high-water
//! mark, so the ingest listeners reject work and bound RSS independently
//! of the queue-depth shed path (which stays green when analysis keeps up).
//!
//! cgroup v2 only (modern k8s / k3s). On a host without cgroup v2 or
//! without an enforced memory limit the reader returns `None` and the
//! watcher stays inert: a no-op guard rather than a spurious one. The
//! same fail-open contract applies at runtime: if the cgroup becomes
//! unreadable or unlimited after the flag was set, the flag clears.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::report::metrics::MetricsState;

/// cgroup v2 usage / limit / stat files, named constants so the exact
/// paths the watcher reads live in one greppable place.
const CGROUP_CURRENT: &str = "/sys/fs/cgroup/memory.current";
const CGROUP_MAX: &str = "/sys/fs/cgroup/memory.max";
const CGROUP_STAT: &str = "/sys/fs/cgroup/memory.stat";

/// Poll cadence. Fixed: the spike that OOMs the pod develops in seconds
/// and a sub-second sysfs read is cheap.
// ponytail: 1s fixed cadence; make it configurable only if a workload needs it
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Hysteresis band in percentage points below the high-water mark: the
/// flag clears only once usage falls this far back, so ingest does not
/// flap on and off around the boundary. Config validation rejects
/// non-zero `memory_high_water_pct` values at or below this band, so the
/// low bound always stays above zero and the flag always has a
/// reachable clear condition.
pub(crate) const HYSTERESIS_PCT: f64 = 5.0;

/// Extract the `inactive_file` byte count from a cgroup v2 `memory.stat`
/// dump. Reclaimable page cache: `memory.current` includes it, but the
/// kernel drops it under pressure, so counting it would latch the guard
/// on archive-heavy pods that hover on cache without any OOM risk.
/// kubelet's working-set metric subtracts it for the same reason.
fn parse_inactive_file(stat: &str) -> Option<f64> {
    stat.lines().find_map(|line| {
        line.strip_prefix("inactive_file ")
            .and_then(|rest| rest.trim().parse().ok())
    })
}

/// Compute the working-set ratio `(current - inactive_file) / max`, or
/// `None` when there is no enforced limit (`memory.max` is the literal
/// `max`) or either value fails to parse. Pure and file-free so it is
/// unit-testable.
fn parse_ratio(current: &str, max: &str, inactive_file: f64) -> Option<f64> {
    let max = max.trim();
    if max == "max" {
        return None; // no limit set: nothing to protect against
    }
    let max: f64 = max.parse().ok()?;
    if max <= 0.0 {
        return None;
    }
    let current: f64 = current.trim().parse().ok()?;
    Some((current - inactive_file).max(0.0) / max)
}

/// Read the live cgroup v2 working-set ratio, or `None` on any
/// read/parse failure (non-Linux, cgroup v1, no limit set). A missing
/// or unparsable `memory.stat` degrades to the raw usage ratio rather
/// than disabling the guard.
// ponytail: cgroup v2 only; add a v1 fallback if a cgroup-v1 host ever ships
fn read_cgroup_usage_ratio() -> Option<f64> {
    let current = std::fs::read_to_string(CGROUP_CURRENT).ok()?;
    let max = std::fs::read_to_string(CGROUP_MAX).ok()?;
    let inactive_file = std::fs::read_to_string(CGROUP_STAT)
        .ok()
        .and_then(|stat| parse_inactive_file(&stat))
        .unwrap_or(0.0);
    parse_ratio(&current, &max, inactive_file)
}

/// Next flag state given the previous one, the current ratio, and the
/// high/low thresholds (fractions in `[0, 1]`). Pure hysteresis: set at or
/// above `high`, clear below `low`, hold in between.
fn next_high_water(prev: bool, ratio: f64, high: f64, low: f64) -> bool {
    if ratio >= high {
        true
    } else if ratio < low {
        false
    } else {
        prev
    }
}

/// Aborts the watcher task on drop, so an early `?` return in the daemon
/// startup path (listener bind failure, archive open failure) cannot
/// leak a detached, forever-polling task.
pub(super) struct WatcherGuard(JoinHandle<()>);

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Spawn the cgroup memory watcher when the guard is enabled, else `None`.
/// Every [`POLL_INTERVAL`] the task reads the cgroup working-set ratio and
/// updates `metrics.set_memory_high_water`. Warns once if the cgroup is
/// unreadable while the guard is enabled, so an operator on an unsupported
/// host learns the guard is inert. `high_pct` is the caller-validated
/// percentage (`0` disables the guard; validation enforces
/// `> HYSTERESIS_PCT` otherwise).
pub(super) fn spawn_if_enabled(metrics: &Arc<MetricsState>, high_pct: u8) -> Option<WatcherGuard> {
    if high_pct == 0 {
        return None;
    }
    let metrics = Arc::clone(metrics);
    let high = f64::from(high_pct) / 100.0;
    let low = (f64::from(high_pct) - HYSTERESIS_PCT) / 100.0;
    Some(WatcherGuard(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        let mut flag = false;
        let mut unreadable_warned = false;
        loop {
            ticker.tick().await;
            if let Some(ratio) = read_cgroup_usage_ratio() {
                flag = next_high_water(flag, ratio, high, low);
                metrics.set_memory_high_water(flag);
            } else {
                // Fail open: a cgroup that becomes unreadable or
                // unlimited after the flag was set (limit removed, k8s
                // in-place resize to unlimited, permission change) must
                // not leave ingest latched shut.
                if flag {
                    flag = false;
                    metrics.set_memory_high_water(false);
                }
                if !unreadable_warned {
                    tracing::warn!(
                        "memory_high_water_pct is set but the cgroup v2 memory limit is \
                         unreadable; the ingest memory guard is inert on this host"
                    );
                    unreadable_warned = true;
                }
            }
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::{next_high_water, parse_inactive_file, parse_ratio};

    #[test]
    fn parse_ratio_handles_unlimited_and_bad_input() {
        assert_eq!(parse_ratio("100", "max", 0.0), None);
        assert_eq!(parse_ratio("100", "0", 0.0), None);
        assert_eq!(parse_ratio("not-a-number", "100", 0.0), None);
        assert_eq!(parse_ratio("100", "not-a-number", 0.0), None);
    }

    #[test]
    fn parse_ratio_computes_fraction_and_trims() {
        // 200 MiB used out of a 256 MiB limit.
        let r = parse_ratio("209715200\n", " 268435456 \n", 0.0).expect("ratio");
        assert!((r - 0.78125).abs() < 1e-6, "got {r}");
    }

    #[test]
    fn parse_ratio_subtracts_reclaimable_page_cache() {
        // 200 MiB current, but 120 MiB is inactive file cache the kernel
        // reclaims under pressure: working set is 80 MiB of 256 MiB.
        let r = parse_ratio("209715200", "268435456", 125_829_120.0).expect("ratio");
        assert!((r - 0.3125).abs() < 1e-6, "got {r}");
    }

    #[test]
    fn parse_ratio_clamps_working_set_at_zero() {
        // inactive_file above current (stat raced against current):
        // clamp to 0 instead of going negative.
        let r = parse_ratio("100", "1000", 500.0).expect("ratio");
        assert!((r - 0.0).abs() < f64::EPSILON, "got {r}");
    }

    #[test]
    fn parse_inactive_file_finds_the_line() {
        let stat = "anon 52428800\nfile 130023424\ninactive_file 125829120\nactive_file 4194304\n";
        assert_eq!(parse_inactive_file(stat), Some(125_829_120.0));
        assert_eq!(parse_inactive_file("anon 1\nfile 2\n"), None);
        assert_eq!(parse_inactive_file("inactive_file not-a-number\n"), None);
    }

    #[test]
    fn next_high_water_has_hysteresis() {
        // high = 0.80, low = 0.75.
        assert!(!next_high_water(false, 0.70, 0.80, 0.75), "below both: off");
        assert!(
            !next_high_water(false, 0.78, 0.80, 0.75),
            "in band from off: hold off"
        );
        assert!(
            next_high_water(false, 0.82, 0.80, 0.75),
            "at/above high: on"
        );
        assert!(
            next_high_water(true, 0.78, 0.80, 0.75),
            "in band from on: hold on"
        );
        assert!(!next_high_water(true, 0.74, 0.80, 0.75), "below low: off");
    }
}
