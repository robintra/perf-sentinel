//! Shared cloud energy state and snapshot access.
//!
//! Mirrors [`super::super::scaphandre::state`]: an [`ArcSwap`]-backed
//! `HashMap` of per-service energy coefficients with monotonic-clock
//! staleness filtering. The scraper publishes fresh data; the scoring
//! path reads a zero-contention snapshot.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;

// Reuse the monotonic clock from the Scaphandre module so both
// scrapers and the scoring path share a single time source.
pub use crate::score::scaphandre::state::monotonic_ms;

/// One row in the shared state: a measured coefficient with a
/// freshness timestamp.
#[derive(Debug, Clone, Copy)]
pub(super) struct ServiceEnergy {
    pub(super) energy_per_op_kwh: f64,
    pub(super) last_update_ms: u64,
}

/// Runtime state shared between the cloud energy scraper and the
/// scoring path.
///
/// Same design as [`crate::score::scaphandre::state::ScaphandreState`]:
/// read-heavy / write-rare, zero-contention via [`ArcSwap`].
#[derive(Debug, Default)]
pub struct CloudEnergyState {
    inner: ArcSwap<HashMap<String, ServiceEnergy>>,
}

impl CloudEnergyState {
    /// Build a new, empty shared state wrapped in `Arc`.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Synchronous snapshot of per-service coefficients, filtering out
    /// stale rows (age >= `staleness_ms`).
    #[must_use]
    pub fn snapshot(&self, now_ms: u64, staleness_ms: u64) -> HashMap<String, f64> {
        let current = self.inner.load_full();
        current
            .iter()
            .filter_map(|(service, energy)| {
                let age = now_ms.saturating_sub(energy.last_update_ms);
                if age < staleness_ms {
                    Some((service.clone(), energy.energy_per_op_kwh))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Publish a fresh table. Called by the scraper after each
    /// successful scrape cycle.
    pub(super) fn publish(&self, new_table: HashMap<String, ServiceEnergy>) {
        self.inner.store(Arc::new(new_table));
    }

    /// Clone the current table for merge-update. Typically small
    /// (one entry per configured service).
    pub(super) fn current_owned(&self) -> HashMap<String, ServiceEnergy> {
        (*self.inner.load_full()).clone()
    }

    /// Test-only helper: insert an entry directly.
    #[cfg(test)]
    pub(crate) fn insert_for_test(
        &self,
        service: String,
        energy_per_op_kwh: f64,
        last_update_ms: u64,
    ) {
        let mut current = self.current_owned();
        current.insert(
            service,
            ServiceEnergy {
                energy_per_op_kwh,
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
        let state = CloudEnergyState::new();
        let snap = state.snapshot(1000, 5000);
        assert!(snap.is_empty());
    }

    #[test]
    fn fresh_entry_appears_in_snapshot() {
        let state = CloudEnergyState::new();
        state.insert_for_test("svc-a".into(), 1e-7, 100);
        let snap = state.snapshot(200, 500);
        assert_eq!(snap.len(), 1);
        assert!((snap["svc-a"] - 1e-7).abs() < 1e-15);
    }

    #[test]
    fn stale_entry_filtered_out() {
        let state = CloudEnergyState::new();
        state.insert_for_test("svc-a".into(), 1e-7, 100);
        // now=700, staleness=500 → age 600 >= 500 → stale
        let snap = state.snapshot(700, 500);
        assert!(snap.is_empty());
    }

    #[test]
    fn mixed_fresh_and_stale() {
        let state = CloudEnergyState::new();
        state.insert_for_test("fresh".into(), 2e-7, 500);
        state.insert_for_test("stale".into(), 3e-7, 100);
        let snap = state.snapshot(600, 200);
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key("fresh"));
        assert!(!snap.contains_key("stale"));
    }
}
