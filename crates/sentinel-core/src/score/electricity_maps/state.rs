//! Shared Electricity Maps state and snapshot access.
//!
//! Mirrors [`super::super::scaphandre::state`] and
//! [`super::super::cloud_energy::state`]: an [`ArcSwap`]-backed
//! `HashMap` of per-region carbon intensity values with
//! monotonic-clock staleness filtering. The scraper publishes fresh
//! data; the scoring path reads a zero-contention snapshot.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;

pub use crate::score::scaphandre::state::monotonic_ms;

/// One row in the shared state: a real-time intensity reading with
/// a freshness timestamp.
#[derive(Debug, Clone, Copy)]
pub(super) struct IntensityReading {
    pub(super) gco2_per_kwh: f64,
    pub(super) last_update_ms: u64,
}

/// Runtime state shared between the Electricity Maps scraper and the
/// scoring path.
///
/// Same design as [`crate::score::scaphandre::state::ScaphandreState`]:
/// read-heavy / write-rare, zero-contention via [`ArcSwap`].
#[derive(Debug, Default)]
pub struct ElectricityMapsState {
    inner: ArcSwap<HashMap<String, IntensityReading>>,
}

impl ElectricityMapsState {
    /// Build a new, empty shared state wrapped in `Arc`.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Synchronous snapshot of per-region intensities, filtering out
    /// stale rows (age >= `staleness_ms`).
    ///
    /// Returns `cloud_region -> gCO2/kWh`.
    #[must_use]
    pub fn snapshot(&self, now_ms: u64, staleness_ms: u64) -> HashMap<String, f64> {
        let current = self.inner.load_full();
        current
            .iter()
            .filter_map(|(region, reading)| {
                let age = now_ms.saturating_sub(reading.last_update_ms);
                if age < staleness_ms {
                    Some((region.clone(), reading.gco2_per_kwh))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Publish a fresh table. Called by the scraper after each
    /// successful scrape cycle.
    pub(super) fn publish(&self, new_table: HashMap<String, IntensityReading>) {
        self.inner.store(Arc::new(new_table));
    }

    /// Clone the current table for merge-update.
    pub(super) fn current_owned(&self) -> HashMap<String, IntensityReading> {
        (*self.inner.load_full()).clone()
    }

    /// Test-only helper: insert an entry directly.
    #[cfg(test)]
    pub(crate) fn insert_for_test(&self, region: String, gco2_per_kwh: f64, last_update_ms: u64) {
        let mut current = self.current_owned();
        current.insert(
            region,
            IntensityReading {
                gco2_per_kwh,
                last_update_ms,
            },
        );
        self.publish(current);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_returns_empty_snapshot() {
        let state = ElectricityMapsState::new();
        let snap = state.snapshot(1000, 5000);
        assert!(snap.is_empty());
    }

    #[test]
    fn fresh_entry_appears_in_snapshot() {
        let state = ElectricityMapsState::new();
        state.insert_for_test("eu-west-3".into(), 56.0, 100);
        let snap = state.snapshot(200, 500);
        assert_eq!(snap.len(), 1);
        assert!((snap["eu-west-3"] - 56.0).abs() < 1e-10);
    }

    #[test]
    fn stale_entry_filtered_out() {
        let state = ElectricityMapsState::new();
        state.insert_for_test("eu-west-3".into(), 56.0, 100);
        // now=700, staleness=500 -> age 600 >= 500 -> stale
        let snap = state.snapshot(700, 500);
        assert!(snap.is_empty());
    }

    #[test]
    fn mixed_fresh_and_stale() {
        let state = ElectricityMapsState::new();
        state.insert_for_test("fresh-region".into(), 100.0, 500);
        state.insert_for_test("stale-region".into(), 200.0, 100);
        let snap = state.snapshot(600, 200);
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key("fresh-region"));
        assert!(!snap.contains_key("stale-region"));
    }
}
