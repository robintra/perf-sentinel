//! Per-service op-delta snapshot shared by the measured-energy scrapers.
//!
//! Scaphandre, Kepler, Redfish, and the cloud `SPECpower` scraper all need to
//! compute the per-service I/O op count over a single scrape window so the
//! energy-per-op coefficient stays bounded as load changes. The daemon
//! exposes a monotonic counter per service via
//! `MetricsState::service_io_ops_total`, each scraper holds an
//! [`OpsSnapshotDiff`] to compute `delta = current - last_snapshot` without
//! resetting the upstream counter (which would race with the intake path
//! and break operator dashboards).
//!
//! The previous-snapshot table is stored as `Option<Arc<HashMap>>` and
//! advanced via an `Arc` promotion, so the per-tick update is a refcount
//! bump rather than a deep clone of the key set.

use std::collections::HashMap;
use std::sync::Arc;

/// Snapshot diff used by measured-energy scrapers to compute per-service
/// I/O op counts over a single scrape window.
///
/// The daemon increments `MetricsState::service_io_ops_total` on every
/// normalized event (see `daemon.rs`). Each scraper reads those counters
/// at every tick and calls [`delta_and_advance`] to derive the "ops in
/// the current scrape window" number needed for the
/// `energy_per_op = power × interval / ops_in_window` formula.
///
/// Using a snapshot diff instead of a parallel counter that gets reset
/// each scrape avoids counter-reset races with the event intake path
/// and gives Grafana users a monotonic per-service counter for free.
///
/// The previous-snapshot table is stored as `Option<Arc<HashMap>>` and
/// updated via `Arc::from(current)` on each advance. Each scraper owns
/// its own `OpsSnapshotDiff` exclusively so no atomic swap is needed,
/// but the `Arc` keeps the advance zero-copy.
///
/// [`delta_and_advance`]: OpsSnapshotDiff::delta_and_advance
#[derive(Debug, Default)]
pub struct OpsSnapshotDiff {
    last: Option<Arc<HashMap<String, u64>>>,
}

impl OpsSnapshotDiff {
    /// Compute the delta for each service vs the previous snapshot.
    /// Advances the internal "last" table to the passed-in `current`
    /// via a zero-copy `Arc` promotion.
    ///
    /// Services that went backwards (counter reset, restart) produce
    /// a delta of 0, this is safer than a huge wraparound number.
    ///
    /// The returned map only contains services with a strictly
    /// positive delta, so idle services are omitted and callers can
    /// skip them without extra filtering.
    pub fn delta_and_advance(&mut self, current: HashMap<String, u64>) -> HashMap<String, u64> {
        let mut out = HashMap::with_capacity(current.len());
        let previous = self.last.as_deref();
        for (service, &now) in &current {
            let before = previous.and_then(|p| p.get(service)).copied().unwrap_or(0);
            let delta = now.saturating_sub(before);
            if delta > 0 {
                out.insert(service.clone(), delta);
            }
        }
        // Promote `current` into an Arc and replace the previous
        // snapshot. No deep clone of the keys, the `Arc` just bumps
        // the refcount of the already-allocated HashMap.
        self.last = Some(Arc::new(current));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_call_counts_all() {
        let mut diff = OpsSnapshotDiff::default();
        let mut current = HashMap::new();
        current.insert("svc-a".to_string(), 5);
        current.insert("svc-b".to_string(), 12);
        let out = diff.delta_and_advance(current);
        assert_eq!(out.get("svc-a"), Some(&5));
        assert_eq!(out.get("svc-b"), Some(&12));
    }

    #[test]
    fn second_call_subtracts() {
        let mut diff = OpsSnapshotDiff::default();
        let mut first = HashMap::new();
        first.insert("svc-a".to_string(), 5);
        diff.delta_and_advance(first);

        let mut second = HashMap::new();
        second.insert("svc-a".to_string(), 9);
        let out = diff.delta_and_advance(second);
        assert_eq!(out.get("svc-a"), Some(&4));
    }

    #[test]
    fn no_change_produces_empty() {
        let mut diff = OpsSnapshotDiff::default();
        let mut first = HashMap::new();
        first.insert("svc-a".to_string(), 5);
        diff.delta_and_advance(first);

        let mut second = HashMap::new();
        second.insert("svc-a".to_string(), 5);
        let out = diff.delta_and_advance(second);
        assert!(out.is_empty());
    }

    #[test]
    fn counter_reset_omits_entry() {
        let mut diff = OpsSnapshotDiff::default();
        let mut first = HashMap::new();
        first.insert("svc-a".to_string(), 12);
        diff.delta_and_advance(first);

        let mut second = HashMap::new();
        second.insert("svc-a".to_string(), 3); // reset to a smaller value
        let out = diff.delta_and_advance(second);
        // Saturating sub clamps the negative to zero so the entry is
        // omitted entirely, not surfaced as a bogus 0.
        assert!(!out.contains_key("svc-a"));
    }
}
