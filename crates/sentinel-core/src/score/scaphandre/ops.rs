//! Scaphandre power→energy math + state update.
//!
//! Holds the pieces specific to Scaphandre's per-process power gauge:
//! [`compute_energy_per_op_kwh`] turns `(microwatts, scrape_interval, ops)`
//! into kWh-per-op, and [`apply_scrape`] glues that together with the
//! config's `process_map` to publish a fresh [`ScaphandreState`].
//! The cross-scraper per-service op-delta tracker lives in
//! [`crate::score::ops_snapshot_diff`].

use std::collections::HashMap;

use super::config::ScaphandreConfig;
use super::parser::ProcessPower;
use super::state::{ScaphandreState, ServiceEnergy};

/// Convert a per-process power reading + observed op count into an
/// energy-per-op coefficient (kWh per op).
///
/// Formula:
/// ```text
///   energy_per_op_kwh =
///       (power_watts × scrape_interval_secs) / (ops × 3_600_000)
/// ```
///
/// The `3_600_000` constant converts joules (watt-seconds) to kWh
/// (1 kWh = 3.6 × 10⁶ J).
///
/// Returns `None` when `ops == 0` so the scraper can keep the
/// previous (still-valid) entry unchanged instead of producing a
/// division-by-zero or a coefficient that flaps every scrape. Keeping
/// stale-but-present is the user-validated decision against model-tag
/// flapping for idle services.
#[must_use]
pub fn compute_energy_per_op_kwh(
    power_microwatts: f64,
    scrape_interval_secs: f64,
    ops: u64,
) -> Option<f64> {
    if ops == 0 || !power_microwatts.is_finite() || power_microwatts < 0.0 {
        return None;
    }
    let power_watts = power_microwatts / 1_000_000.0;
    let joules = power_watts * scrape_interval_secs;
    // 1 kWh = 3.6e6 J.
    let kwh = joules / 3_600_000.0;
    Some(kwh / ops as f64)
}

/// Apply a freshly-scraped batch of process-power readings to a
/// [`ScaphandreState`], updating each mapped service's energy-per-op
/// coefficient.
///
/// Takes the already-parsed `power_readings` (from
/// [`super::parser::parse_scaphandre_metrics`]), the per-service op
/// delta (from
/// [`crate::score::ops_snapshot_diff::OpsSnapshotDiff::delta_and_advance`]),
/// and the scraper config. Iterates the config's `process_map` to find each
/// mapped service, looks up its process's current power, and calls
/// [`compute_energy_per_op_kwh`] to derive the coefficient. If the
/// op delta is 0 for a service, the existing entry (if any) is left
/// unchanged, see the rationale in [`compute_energy_per_op_kwh`].
///
/// Uses the `ArcSwap` pattern: builds a new `HashMap` starting from
/// the current published one (so previous entries that don't get
/// updated this tick are inherited), mutates it locally, and
/// publishes the result atomically at the end via
/// `ScaphandreState::publish`.
// `clippy::implicit_hasher` would want `op_deltas` generic over the
// hasher (`<S: BuildHasher>`). We take it as a concrete `HashMap`
// because the only caller is the scraper loop, which gets the map
// from `MetricsState::snapshot_service_io_ops` (default hasher).
// A hasher type parameter here would leak into every test fixture
// for no practical benefit.
#[allow(clippy::implicit_hasher)]
pub fn apply_scrape(
    state: &ScaphandreState,
    power_readings: &[ProcessPower],
    op_deltas: &HashMap<String, u64>,
    cfg: &ScaphandreConfig,
    now_ms: u64,
) {
    let interval_secs = cfg.scrape_interval.as_secs_f64();
    let mut next = state.current_owned();
    let mut any_change = false;
    for (service, exe_name) in &cfg.process_map {
        let Some(ops) = op_deltas.get(service).copied() else {
            continue; // service had no ops this window → keep previous entry
        };
        let Some(reading) = power_readings.iter().find(|p| &p.exe == exe_name) else {
            continue; // process not running (or not in Scaphandre output) → keep previous
        };
        let Some(energy_per_op) =
            compute_energy_per_op_kwh(reading.power_microwatts, interval_secs, ops)
        else {
            continue; // divide-by-zero or negative power → skip this service this tick
        };
        next.insert(
            service.clone(),
            ServiceEnergy {
                energy_per_op_kwh: energy_per_op,
                last_update_ms: now_ms,
            },
        );
        any_change = true;
    }
    if any_change {
        state.publish(next);
    }
}
