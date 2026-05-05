//! Shared `ArcSwap`-backed storage for per-service energy coefficients.
//!
//! See `docs/design/05-GREENOPS-AND-CARBON.md` § "Energy state cache
//! coherency" for the read-heavy / write-rare design rationale and the
//! ArcSwap-vs-RwLock tradeoff.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;

/// One row in the shared state: a measured coefficient with a
/// freshness timestamp.
///
/// `last_update_ms` is monotonic milliseconds since process start ,
/// produced by [`crate::score::scaphandre::state::monotonic_ms`]. The
/// scoring snapshot uses the `staleness_ms` parameter to discard
/// entries older than `3 × scrape_interval` (so a hung scraper does
/// not silently return increasingly stale data).
#[derive(Debug, Clone, Copy)]
pub(crate) struct EnergyRow {
    pub(crate) energy_per_op_kwh: f64,
    pub(crate) last_update_ms: u64,
}

/// Shared storage for per-service energy coefficients with staleness
/// filtering.
///
/// Constructors and method signatures mirror the pre-existing
/// `ScaphandreState` / `CloudEnergyState` public surface so the
/// wrapping newtypes can delegate line-for-line.
#[derive(Debug, Default)]
pub(crate) struct AgedEnergyMap {
    inner: ArcSwap<HashMap<String, EnergyRow>>,
}

impl AgedEnergyMap {
    /// Synchronous snapshot of per-service coefficients, filtering out
    /// rows whose age is `>= staleness_ms`.
    ///
    /// The returned `HashMap<String, f64>` is owned: the scoring path
    /// hands this directly to `CarbonContext.energy_snapshot`. Keys are
    /// cloned once per fresh row, typically single digits of services.
    #[must_use]
    pub(crate) fn snapshot(&self, now_ms: u64, staleness_ms: u64) -> HashMap<String, f64> {
        let current = self.inner.load_full();
        current
            .iter()
            .filter_map(|(service, energy)| {
                // Saturating sub so a clock skew or monotonic-reset
                // event does not accidentally mark fresh rows as stale.
                let age = now_ms.saturating_sub(energy.last_update_ms);
                if age < staleness_ms {
                    Some((service.clone(), energy.energy_per_op_kwh))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Publish a fresh table, atomically replacing the previous one.
    /// Called by the scraper task after each successful scrape cycle.
    pub(crate) fn publish(&self, new_table: HashMap<String, EnergyRow>) {
        self.inner.store(Arc::new(new_table));
    }

    /// Produce an owned copy of the current table so the scraper can
    /// merge-update it before publishing the new version.
    pub(crate) fn current_owned(&self) -> HashMap<String, EnergyRow> {
        (*self.inner.load_full()).clone()
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
        let mut current = self.current_owned();
        current.insert(
            service,
            EnergyRow {
                energy_per_op_kwh,
                last_update_ms,
            },
        );
        self.publish(current);
    }
}

/// Generate a nominally distinct energy-state wrapper around
/// [`AgedEnergyMap`]. Each invocation creates a new type that delegates
/// `new`, `snapshot`, `publish`, `current_owned`, and `insert_for_test`
/// to the shared storage. The types are intentionally NOT unified so the
/// daemon cannot accidentally swap a Scaphandre state for a cloud-energy
/// state (or vice versa) when tagging the energy model.
macro_rules! impl_energy_state {
    (
        $(#[$meta:meta])*
        $vis:vis struct $Name:ident;
    ) => {
        $(#[$meta])*
        $vis struct $Name {
            inner: $crate::score::energy_state::AgedEnergyMap,
        }

        impl $Name {
            #[must_use]
            $vis fn new() -> std::sync::Arc<Self> {
                std::sync::Arc::new(Self::default())
            }

            #[must_use]
            $vis fn snapshot(
                &self,
                now_ms: u64,
                staleness_ms: u64,
            ) -> std::collections::HashMap<String, f64> {
                self.inner.snapshot(now_ms, staleness_ms)
            }

            pub(super) fn publish(
                &self,
                new_table: std::collections::HashMap<String, ServiceEnergy>,
            ) {
                self.inner.publish(new_table);
            }

            pub(super) fn current_owned(
                &self,
            ) -> std::collections::HashMap<String, ServiceEnergy> {
                self.inner.current_owned()
            }

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
    };
}

pub(crate) use impl_energy_state;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_returns_empty_snapshot() {
        let state = AgedEnergyMap::default();
        assert!(state.snapshot(1000, 5000).is_empty());
    }

    #[test]
    fn fresh_entry_appears_in_snapshot() {
        let state = AgedEnergyMap::default();
        state.insert_for_test("svc-a".into(), 1e-7, 100);
        let snap = state.snapshot(200, 500);
        assert_eq!(snap.len(), 1);
        assert!((snap["svc-a"] - 1e-7).abs() < 1e-15);
    }

    #[test]
    fn stale_entry_filtered_out() {
        let state = AgedEnergyMap::default();
        state.insert_for_test("svc-a".into(), 1e-7, 100);
        // now=700, staleness=500 → age 600 >= 500 → stale
        assert!(state.snapshot(700, 500).is_empty());
    }

    #[test]
    fn mixed_fresh_and_stale() {
        let state = AgedEnergyMap::default();
        state.insert_for_test("fresh".into(), 2e-7, 500);
        state.insert_for_test("stale".into(), 3e-7, 100);
        let snap = state.snapshot(600, 200);
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key("fresh"));
        assert!(!snap.contains_key("stale"));
    }

    #[test]
    fn saturating_sub_protects_against_clock_skew() {
        let state = AgedEnergyMap::default();
        // Row at t=1000, read at t=500 (time went backwards).
        // saturating_sub gives 0 → age 0 < staleness → fresh.
        state.insert_for_test("svc".into(), 5e-7, 1000);
        let snap = state.snapshot(500, 200);
        assert_eq!(snap.len(), 1);
    }

    #[test]
    fn current_owned_returns_independent_copy() {
        let state = AgedEnergyMap::default();
        state.insert_for_test("svc".into(), 1e-7, 100);
        let mut owned = state.current_owned();
        owned.clear();
        // Original state must be unaffected.
        assert_eq!(state.snapshot(200, 500).len(), 1);
    }
}
