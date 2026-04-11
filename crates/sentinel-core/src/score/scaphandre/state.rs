//! Shared Scaphandre state and monotonic timestamp helper.
//!
//! Thin wrapper around [`AgedEnergyMap`];
//! see that module for the `ArcSwap`-backed storage design. This file
//! keeps `ScaphandreState` as a distinct nominal type (so the daemon
//! code cannot accidentally swap a cloud-energy state for a Scaphandre
//! one) while delegating all storage behavior to the shared impl.
//!
//! Read-heavy / write-rare:
//! - Writes: once per scrape interval (default 5 s), by a single task.
//! - Reads: once per `process_traces` tick (typically multiple per
//!   second under real OTLP load).

use std::collections::HashMap;
use std::sync::Arc;

use crate::score::energy_state::AgedEnergyMap;

/// Row type expected by [`super::ops::apply_scrape`] when constructing
/// fresh entries. Aliased to the shared [`EnergyRow`] so both states
/// share one definition.
///
/// [`EnergyRow`]: crate::score::energy_state::EnergyRow
pub(super) use crate::score::energy_state::EnergyRow as ServiceEnergy;

/// Runtime state shared between the scraper task and the scoring path.
///
/// Nominally distinct from
/// [`crate::score::cloud_energy::CloudEnergyState`] so the daemon can
/// accept one without accidentally receiving the other. Both wrap the
/// same [`AgedEnergyMap`] storage under the hood.
#[derive(Debug, Default)]
pub struct ScaphandreState {
    inner: AgedEnergyMap,
}

impl ScaphandreState {
    /// Build a new, empty shared state. Wrapped in `Arc` for
    /// cross-task sharing; the daemon gets one `Arc` clone for the
    /// scraper spawn and keeps another for the scoring snapshot path.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Synchronous snapshot of per-service coefficients, filtering out
    /// rows whose age is `>= staleness_ms`. See
    /// [`AgedEnergyMap::snapshot`] for the full contract.
    #[must_use]
    pub fn snapshot(&self, now_ms: u64, staleness_ms: u64) -> HashMap<String, f64> {
        self.inner.snapshot(now_ms, staleness_ms)
    }

    /// Publish a fresh table. Called by [`super::ops::apply_scrape`]
    /// after each successful scrape.
    pub(super) fn publish(&self, new_table: HashMap<String, ServiceEnergy>) {
        self.inner.publish(new_table);
    }

    /// Produce an owned copy of the current table so the scraper can
    /// merge-update it before publishing the new version.
    pub(super) fn current_owned(&self) -> HashMap<String, ServiceEnergy> {
        self.inner.current_owned()
    }

    /// Test-only helper: insert an entry directly without running the
    /// full scrape loop.
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

/// Monotonic milliseconds since process start.
///
/// Uses `std::time::Instant` so the clock is immune to wall-clock
/// adjustments (NTP slew, manual date change). The scraper and the
/// scoring snapshot both call this function so their timestamps are
/// comparable without cross-clock drift.
///
/// Returns 0 for the first call of the process (when `START` is
/// lazily initialized) and increases monotonically afterwards.
#[must_use]
pub fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    Instant::now().duration_since(*start).as_millis() as u64
}
