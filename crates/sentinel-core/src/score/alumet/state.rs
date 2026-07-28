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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Cumulative database energy: the scraper (single writer) adds each
/// scrape window's kWh, the event loop takes the delta per scored
/// batch. Shed batches never take, so their energy carries over. f64
/// as `AtomicU64` bit patterns, `SeqCst`, far off any hot path.
#[derive(Debug, Default)]
pub struct DbEnergyState {
    cumulative_kwh_bits: AtomicU64,
    consumed_kwh_bits: AtomicU64,
    last_update_ms: AtomicU64,
    /// Last scrape that actually carried this workload's label, as
    /// opposed to `last_update_ms` which any successful scrape refreshes.
    last_sample_ms: AtomicU64,
    /// Whether a labelled sample ever landed. A flag rather than a zero
    /// timestamp, which `monotonic_ms()` legitimately returns at startup.
    ever_sampled: AtomicBool,
}

impl DbEnergyState {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Scraper side: refresh liveness without touching the balance.
    /// Called on every successful scrape while a database is declared,
    /// whether or not its label appeared, so banked energy survives an
    /// idle database and a label rename alike.
    pub(crate) fn mark_alive(&self, now_ms: u64) {
        self.last_update_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Scraper side: add one scrape window's energy.
    pub(crate) fn add_window_kwh(&self, kwh: f64, now_ms: u64) {
        // Atomic read-modify-write, not load+store: a second writer
        // (apply_scrape is a public path) must not silently drop a
        // window's energy.
        let _ = self
            .cumulative_kwh_bits
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |bits| {
                Some((f64::from_bits(bits) + kwh).to_bits())
            });
        self.last_update_ms.store(now_ms, Ordering::SeqCst);
        self.last_sample_ms.store(now_ms, Ordering::SeqCst);
        self.ever_sampled.store(true, Ordering::SeqCst);
    }

    /// Consumer side: whether this workload's own series was seen recently
    /// enough for the measurement to own its slice of the timeline.
    ///
    /// Stricter than the liveness [`Self::take_window_kwh`] uses, and the
    /// distinction is load-bearing: see `docs/design/05-GREENOPS-AND-CARBON.md`.
    #[must_use]
    pub fn has_recent_sample(&self, now_ms: u64, staleness_ms: u64) -> bool {
        // Without this an unscraped state reads fresh for the first
        // staleness window of every process.
        if !self.ever_sampled.load(Ordering::SeqCst) {
            return false;
        }
        let last = self.last_sample_ms.load(Ordering::SeqCst);
        now_ms.saturating_sub(last) <= staleness_ms
    }

    /// Consumer side: drop what accumulated without billing it, for when
    /// another source already billed the same wall clock.
    pub fn discard_pending(&self) {
        let cumulative = self.cumulative_kwh_bits.load(Ordering::SeqCst);
        self.consumed_kwh_bits.store(cumulative, Ordering::SeqCst);
    }

    /// Consumer side: energy accumulated since the previous take.
    ///
    /// `None` when the last label-bearing scrape is older than
    /// `staleness_ms` (the consumed marker is not advanced, so the
    /// energy is delivered once the scraper recovers) or when nothing
    /// accumulated.
    pub fn take_window_kwh(&self, now_ms: u64, staleness_ms: u64) -> Option<f64> {
        // No never-updated sentinel needed here: an untouched state has a
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
