//! Shared cloud energy state and snapshot access.
//!
//! Thin wrapper around [`crate::score::energy_state::AgedEnergyMap`];
//! the same shared storage used by
//! [`crate::score::scaphandre::ScaphandreState`]. Kept as a distinct
//! nominal type so the daemon's `build_tick_ctx` cannot accidentally
//! confuse cloud-SPECpower readings with Scaphandre-RAPL readings when
//! it merges the snapshots.

use std::collections::HashMap;
use std::sync::Arc;

use crate::score::energy_state::AgedEnergyMap;

// Reuse the monotonic clock from the Scaphandre module so both
// scrapers and the scoring path share a single time source.
pub use crate::score::scaphandre::state::monotonic_ms;

/// Row type expected by the cloud energy scraper when constructing
/// fresh entries. Aliased to the shared [`EnergyRow`] so both the
/// cloud and Scaphandre states share one row definition.
///
/// [`EnergyRow`]: crate::score::energy_state::EnergyRow
pub(super) use crate::score::energy_state::EnergyRow as ServiceEnergy;

/// Runtime state shared between the cloud energy scraper and the
/// scoring path. Read-heavy / write-rare, zero-contention via
/// [`AgedEnergyMap`].
#[derive(Debug, Default)]
pub struct CloudEnergyState {
    inner: AgedEnergyMap,
}

impl CloudEnergyState {
    /// Build a new, empty shared state wrapped in `Arc`.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Synchronous snapshot of per-service coefficients, filtering out
    /// stale rows (age >= `staleness_ms`). See
    /// [`AgedEnergyMap::snapshot`] for the full contract.
    #[must_use]
    pub fn snapshot(&self, now_ms: u64, staleness_ms: u64) -> HashMap<String, f64> {
        self.inner.snapshot(now_ms, staleness_ms)
    }

    /// Publish a fresh table. Called by the scraper after each
    /// successful scrape cycle.
    pub(super) fn publish(&self, new_table: HashMap<String, ServiceEnergy>) {
        self.inner.publish(new_table);
    }

    /// Clone the current table for merge-update. Typically small
    /// (one entry per configured service).
    pub(super) fn current_owned(&self) -> HashMap<String, ServiceEnergy> {
        self.inner.current_owned()
    }

    /// Test-only helper: insert an entry directly.
    #[cfg(test)]
    pub(crate) fn insert_for_test(
        &self,
        service: String,
        energy_per_op_kwh: f64,
        last_update_ms: u64,
    ) {
        self.inner
            .insert_for_test(service, energy_per_op_kwh, last_update_ms);
    }
}
