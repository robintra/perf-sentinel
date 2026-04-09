//! Shared Scaphandre state and monotonic timestamp helper.
//!
//! The [`ScaphandreState`] is backed by an [`ArcSwap`] so
//! the scoring path reads are sync and zero-clone. The scraper task
//! builds a fresh `Arc<HashMap>` on each successful scrape and
//! atomically swaps it in via [`ArcSwap::store`]; readers do a single
//! `load_full()` to get their own `Arc` reference without contending
//! on a lock.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;

/// Runtime state shared between the scraper task and the scoring path.
///
/// The scraper task holds an `Arc<ScaphandreState>` and publishes a new
/// `Arc<HashMap<String, ServiceEnergy>>` on each successful scrape via
/// [`ArcSwap::store`]. Scoring takes a synchronous snapshot at the
/// start of each `process_traces` tick via [`Self::snapshot`], which
/// is a single `load_full()` + a filter pass — no async lock, no
/// per-tick `String` clone of the keys.
///
/// This is a read-heavy / write-rare pattern:
/// - Writes: once per scrape interval (default 5 s), by a single task.
/// - Reads: once per `process_traces` tick (typically multiple per
///   second under real OTLP load).
/// - Both sides see a consistent view of the full table: readers get
///   the `Arc` that was current when they called `load_full`, writers
///   don't block anyone.
#[derive(Debug, Default)]
pub struct ScaphandreState {
    // ArcSwap stores an `Arc<HashMap<...>>`. Default yields an
    // ArcSwap pointing at an empty HashMap, which is exactly what we
    // want for the "no successful scrape yet" initial state.
    inner: ArcSwap<HashMap<String, ServiceEnergy>>,
}

/// One row in the shared state: a measured coefficient with a
/// freshness timestamp.
///
/// `last_update_ms` is monotonic milliseconds since process start —
/// produced by [`monotonic_ms`]. The scoring snapshot uses the
/// `staleness_threshold_ms` parameter to discard entries older than
/// `3 × scrape_interval` (so a hung scraper doesn't silently return
/// increasingly stale data).
///
/// Visible as `pub(super)` so the sibling [`super::ops::apply_scrape`]
/// function can construct new rows. Fields stay private to this
/// module so the freshness invariant can only be updated via a
/// [`ScaphandreState::publish`] call.
#[derive(Debug, Clone, Copy)]
pub(super) struct ServiceEnergy {
    pub(super) energy_per_op_kwh: f64,
    pub(super) last_update_ms: u64,
}

impl ScaphandreState {
    /// Build a new, empty shared state. Wrapped in `Arc` for
    /// cross-task sharing; the daemon gets one `Arc` clone for the
    /// scraper spawn and keeps another for the scoring snapshot path.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Produce a synchronous `HashMap<String, f64>` snapshot of the
    /// current per-service coefficients, filtering out stale rows.
    ///
    /// Stale rows are defined as `now_ms - last_update_ms >= staleness_ms`.
    /// No async, no lock — a single `ArcSwap::load_full()` gives us an
    /// `Arc<HashMap>` that is guaranteed consistent (it's whatever the
    /// scraper last published) and we iterate it to filter stale rows.
    /// Keys are still cloned (once per fresh row) because the
    /// `CarbonContext.energy_snapshot` signature takes an owned
    /// `HashMap<String, f64>`; that's a typically tiny allocation
    /// (bounded by the number of mapped services, usually single
    /// digits).
    ///
    /// Used by the daemon right before `process_traces` to build the
    /// `CarbonContext.energy_snapshot` for the current tick.
    #[must_use]
    pub fn snapshot(&self, now_ms: u64, staleness_ms: u64) -> HashMap<String, f64> {
        let current = self.inner.load_full();
        current
            .iter()
            .filter_map(|(service, energy)| {
                // Saturating sub so a clock skew or monotonic-reset
                // event doesn't accidentally mark fresh rows as stale.
                let age = now_ms.saturating_sub(energy.last_update_ms);
                if age < staleness_ms {
                    Some((service.clone(), energy.energy_per_op_kwh))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Publish a fresh table. Called by [`super::ops::apply_scrape`]
    /// after each successful scrape; the old `Arc` is dropped when
    /// the last reader releases it.
    ///
    /// Takes the new map by value so the scraper's temporary
    /// `HashMap` doesn't need to be re-allocated after publish.
    pub(super) fn publish(&self, new_table: HashMap<String, ServiceEnergy>) {
        self.inner.store(Arc::new(new_table));
    }

    /// Produce an owned copy of the current table so the scraper can
    /// merge-update it before publishing the new version. The map is
    /// typically small (one entry per mapped service) so the clone is
    /// cheap compared to the alternative of holding a write lock.
    pub(super) fn current_owned(&self) -> HashMap<String, ServiceEnergy> {
        (*self.inner.load_full()).clone()
    }

    /// Test-only helper: insert an entry directly without running the
    /// full scrape loop. Keeps the `inner` field private while letting
    /// the integration tests build predictable snapshots.
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
