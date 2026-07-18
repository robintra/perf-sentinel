//! Shared Alumet state. Thin wrapper around `AgedEnergyMap` via the
//! `impl_energy_state!` macro, with `AlumetState` kept as a distinct
//! nominal type so the daemon cannot accidentally swap a Scaphandre or
//! Kepler state for an Alumet one.

/// Row type used by [`super::apply::apply_scrape`] when constructing
/// fresh entries. Aliased to the shared [`EnergyRow`] so every energy
/// state has one definition.
///
/// [`EnergyRow`]: crate::score::energy_state::EnergyRow
pub(super) use crate::score::energy_state::EnergyRow as ServiceEnergy;

// Re-export the monotonic clock the same way `kepler::state` does, so
// consumers can `use super::state::monotonic_ms` instead of reaching
// across to the Scaphandre module path.
pub(super) use crate::score::scaphandre::state::monotonic_ms;

crate::score::energy_state::impl_energy_state! {
    /// Runtime state shared between the Alumet scraper task and the
    /// scoring path.
    #[derive(Debug, Default)]
    pub struct AlumetState;
}

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cumulative database energy: the scraper (single writer) adds each
/// scrape window's kWh, the event loop takes the delta per scored
/// batch. Shed batches never take, so their energy carries over. f64
/// as `AtomicU64` bit patterns, `SeqCst`, far off any hot path.
#[derive(Debug, Default)]
pub struct DbEnergyState {
    cumulative_kwh_bits: AtomicU64,
    consumed_kwh_bits: AtomicU64,
    last_update_ms: AtomicU64,
}

impl DbEnergyState {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Scraper side: add one scrape window's energy. A `0.0` add marks
    /// scrape liveness without changing the balance, so an idle database
    /// does not read as a dead scraper.
    pub(crate) fn add_window_kwh(&self, kwh: f64, now_ms: u64) {
        // fetch_update, not load+store: a second writer (apply_scrape is
        // a public path) must not silently drop a window's energy.
        let _ = self
            .cumulative_kwh_bits
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |bits| {
                Some((f64::from_bits(bits) + kwh).to_bits())
            });
        self.last_update_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Consumer side: energy accumulated since the previous take.
    ///
    /// `None` when the last label-bearing scrape is older than
    /// `staleness_ms` (the consumed marker is not advanced, so the
    /// energy is delivered once the scraper recovers) or when nothing
    /// accumulated.
    pub fn take_window_kwh(&self, now_ms: u64, staleness_ms: u64) -> Option<f64> {
        // No never-updated sentinel needed: an untouched state has a
        // zero cumulative, so the delta check below returns None.
        let last = self.last_update_ms.load(Ordering::SeqCst);
        if now_ms.saturating_sub(last) > staleness_ms {
            return None;
        }
        let cumulative_bits = self.cumulative_kwh_bits.load(Ordering::SeqCst);
        let previous_bits = self
            .consumed_kwh_bits
            .swap(cumulative_bits, Ordering::SeqCst);
        let delta = f64::from_bits(cumulative_bits) - f64::from_bits(previous_bits);
        (delta > 0.0 && delta.is_finite()).then_some(delta)
    }
}
