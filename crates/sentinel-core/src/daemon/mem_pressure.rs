//! Memory-pressure admission control: poll the pod's cgroup v2 memory
//! usage and flip a shared flag when it crosses the configured high-water
//! mark, so the OTLP handlers reject ingest and bound RSS independently of
//! the queue-depth shed path (which stays green when analysis keeps up).
//!
//! cgroup v2 only (modern k8s / k3s). On a host without cgroup v2 or
//! without an enforced memory limit the reader returns `None` and the
//! watcher stays inert: a no-op guard rather than a spurious one.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::report::metrics::MetricsState;

/// cgroup v2 usage / limit files, named so the unit tests document the
/// exact paths the watcher reads at runtime.
const CGROUP_CURRENT: &str = "/sys/fs/cgroup/memory.current";
const CGROUP_MAX: &str = "/sys/fs/cgroup/memory.max";

/// Poll cadence. Fixed: the spike that OOMs the pod develops in seconds
/// and a sub-second sysfs read is cheap.
// ponytail: 1s fixed cadence; make it configurable only if a workload needs it
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Hysteresis band in percentage points below the high-water mark: the
/// flag clears only once usage falls this far back, so ingest does not
/// flap on and off around the boundary.
const HYSTERESIS_PCT: f64 = 5.0;

/// Compute `current / max` as a ratio, or `None` when there is no enforced
/// limit (`memory.max` is the literal `max`) or either value fails to
/// parse. Pure and file-free so it is unit-testable.
fn parse_ratio(current: &str, max: &str) -> Option<f64> {
    let max = max.trim();
    if max == "max" {
        return None; // no limit set: nothing to protect against
    }
    let max: f64 = max.parse().ok()?;
    if max <= 0.0 {
        return None;
    }
    let current: f64 = current.trim().parse().ok()?;
    Some(current / max)
}

/// Read the live cgroup v2 usage ratio, or `None` on any read/parse
/// failure (non-Linux, cgroup v1, no limit set).
// ponytail: cgroup v2 only; add a v1 fallback if a cgroup-v1 host ever ships
fn read_cgroup_usage_ratio() -> Option<f64> {
    let current = std::fs::read_to_string(CGROUP_CURRENT).ok()?;
    let max = std::fs::read_to_string(CGROUP_MAX).ok()?;
    parse_ratio(&current, &max)
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

/// Spawn the cgroup memory watcher when the guard is enabled, else `None`.
/// Every [`POLL_INTERVAL`] the task reads the cgroup usage ratio and
/// updates `metrics.set_memory_high_water`. Warns once if the cgroup is
/// unreadable while the guard is enabled, so an operator on an unsupported
/// host learns the guard is inert. `high_pct` is the caller-validated
/// `0..=100` percentage (`0` disables the guard).
pub(super) fn spawn_if_enabled(
    metrics: &Arc<MetricsState>,
    high_pct: u8,
) -> Option<JoinHandle<()>> {
    if high_pct == 0 {
        return None;
    }
    let metrics = Arc::clone(metrics);
    let high = f64::from(high_pct) / 100.0;
    let low = (f64::from(high_pct) - HYSTERESIS_PCT).max(0.0) / 100.0;
    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        let mut flag = false;
        let mut unreadable_warned = false;
        loop {
            ticker.tick().await;
            match read_cgroup_usage_ratio() {
                Some(ratio) => {
                    flag = next_high_water(flag, ratio, high, low);
                    metrics.set_memory_high_water(flag);
                }
                None if !unreadable_warned => {
                    tracing::warn!(
                        "memory_high_water_pct is set but the cgroup v2 memory limit is \
                         unreadable; the ingest memory guard is inert on this host"
                    );
                    unreadable_warned = true;
                }
                None => {}
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{next_high_water, parse_ratio};

    #[test]
    fn parse_ratio_handles_unlimited_and_bad_input() {
        assert_eq!(parse_ratio("100", "max"), None);
        assert_eq!(parse_ratio("100", "0"), None);
        assert_eq!(parse_ratio("not-a-number", "100"), None);
        assert_eq!(parse_ratio("100", "not-a-number"), None);
    }

    #[test]
    fn parse_ratio_computes_fraction_and_trims() {
        // 200 MiB used out of a 256 MiB limit.
        let r = parse_ratio("209715200\n", " 268435456 \n").expect("ratio");
        assert!((r - 0.78125).abs() < 1e-6, "got {r}");
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
