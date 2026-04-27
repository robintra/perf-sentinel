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

use crate::score::carbon::RealTimeIntensityEntry;
pub use crate::score::scaphandre::state::monotonic_ms;

/// One row in the shared state: a real-time intensity reading with
/// a freshness timestamp and optional `Electricity Maps` estimation
/// metadata. `Clone` (not `Copy`) because `estimation_method` is a
/// `String`.
#[derive(Debug, Clone)]
pub(super) struct IntensityReading {
    pub(super) gco2_per_kwh: f64,
    pub(super) last_update_ms: u64,
    pub(super) is_estimated: Option<bool>,
    pub(super) estimation_method: Option<String>,
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
    /// Returns `cloud_region -> gCO2/kWh`. Estimation metadata is
    /// dropped, callers needing it must use [`Self::snapshot_with_metadata`].
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

    /// Same as [`Self::snapshot`] but propagates the `Electricity Maps`
    /// `isEstimated` and `estimationMethod` metadata fields. Used by the
    /// daemon scoring path so the `green_summary.regions[]` rows can
    /// surface whether the intensity was measured or estimated.
    #[must_use]
    pub fn snapshot_with_metadata(
        &self,
        now_ms: u64,
        staleness_ms: u64,
    ) -> HashMap<String, RealTimeIntensityEntry> {
        let current = self.inner.load_full();
        current
            .iter()
            .filter_map(|(region, reading)| {
                let age = now_ms.saturating_sub(reading.last_update_ms);
                if age < staleness_ms {
                    Some((
                        region.clone(),
                        RealTimeIntensityEntry {
                            gco2_per_kwh: reading.gco2_per_kwh,
                            is_estimated: reading.is_estimated,
                            estimation_method: reading.estimation_method.clone(),
                        },
                    ))
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

    /// Test-only helper: insert a measured-value entry (no estimation
    /// metadata).
    #[cfg(test)]
    pub(crate) fn insert_for_test(&self, region: String, gco2_per_kwh: f64, last_update_ms: u64) {
        let mut current = self.current_owned();
        current.insert(
            region,
            IntensityReading {
                gco2_per_kwh,
                last_update_ms,
                is_estimated: None,
                estimation_method: None,
            },
        );
        self.publish(current);
    }

    /// Test-only helper: insert an entry with estimation metadata.
    #[cfg(test)]
    pub(crate) fn insert_with_metadata_for_test(
        &self,
        region: String,
        gco2_per_kwh: f64,
        last_update_ms: u64,
        is_estimated: Option<bool>,
        estimation_method: Option<String>,
    ) {
        let mut current = self.current_owned();
        current.insert(
            region,
            IntensityReading {
                gco2_per_kwh,
                last_update_ms,
                is_estimated,
                estimation_method,
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

    #[test]
    fn snapshot_with_metadata_propagates_estimation_flags() {
        let state = ElectricityMapsState::new();
        state.insert_with_metadata_for_test(
            "eu-west-3".into(),
            56.0,
            100,
            Some(true),
            Some("TIME_SLICER_AVERAGE".into()),
        );
        let snap = state.snapshot_with_metadata(200, 500);
        assert_eq!(snap.len(), 1);
        let entry = snap.get("eu-west-3").unwrap();
        assert!((entry.gco2_per_kwh - 56.0).abs() < 1e-10);
        assert_eq!(entry.is_estimated, Some(true));
        assert_eq!(
            entry.estimation_method.as_deref(),
            Some("TIME_SLICER_AVERAGE")
        );
    }

    #[test]
    fn snapshot_with_metadata_preserves_none_when_metadata_absent() {
        let state = ElectricityMapsState::new();
        state.insert_for_test("eu-west-3".into(), 56.0, 100);
        let snap = state.snapshot_with_metadata(200, 500);
        let entry = snap.get("eu-west-3").unwrap();
        assert!((entry.gco2_per_kwh - 56.0).abs() < 1e-10);
        assert_eq!(entry.is_estimated, None);
        assert_eq!(entry.estimation_method, None);
    }

    #[test]
    fn snapshot_with_metadata_reflects_latest_publish_when_estimation_changes() {
        // Two consecutive ticks for the same region with different
        // is_estimated values: the snapshot must reflect the latest
        // publish, not get stuck on the first reading.
        let state = ElectricityMapsState::new();
        state.insert_with_metadata_for_test(
            "eu-west-3".into(),
            56.0,
            100,
            Some(true),
            Some("TIME_SLICER_AVERAGE".into()),
        );
        let first = state.snapshot_with_metadata(200, 500);
        assert_eq!(first.get("eu-west-3").unwrap().is_estimated, Some(true));

        // Second tick: same region, measured value this time.
        state.insert_with_metadata_for_test("eu-west-3".into(), 60.0, 300, Some(false), None);
        let second = state.snapshot_with_metadata(400, 500);
        let entry = second.get("eu-west-3").unwrap();
        assert!((entry.gco2_per_kwh - 60.0).abs() < 1e-10);
        assert_eq!(entry.is_estimated, Some(false));
        assert_eq!(entry.estimation_method, None);
    }

    #[test]
    fn snapshot_with_metadata_filters_stale_rows() {
        let state = ElectricityMapsState::new();
        state.insert_with_metadata_for_test("fresh".into(), 56.0, 500, Some(false), None);
        state.insert_with_metadata_for_test(
            "stale".into(),
            56.0,
            100,
            Some(true),
            Some("TSA".into()),
        );
        let snap = state.snapshot_with_metadata(600, 200);
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key("fresh"));
        assert!(!snap.contains_key("stale"));
    }
}
