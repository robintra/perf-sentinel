//! Kepler joules-counter delta math + state update.
//!
//! Methodology (counter-reset semantics, scrape-mode trade-offs) lives in
//! `docs/design/05-GREENOPS-AND-CARBON.md` "Kepler and Redfish attribution
//! notes".

use std::collections::HashMap;

use super::config::KeplerConfig;
use super::parser::KeplerSample;
use super::state::{KeplerState, ServiceEnergy};

/// Convert a joules delta + ops delta into a kWh-per-op coefficient.
/// Returns `None` when the math is meaningless (zero ops, non-finite
/// or negative joules) so the caller can keep the previous entry.
#[must_use]
pub fn compute_energy_per_op_kwh(joules_delta: f64, ops: u64) -> Option<f64> {
    if ops == 0 || !joules_delta.is_finite() || joules_delta < 0.0 {
        return None;
    }
    // 1 kWh = 3.6e6 J.
    let kwh = joules_delta / 3_600_000.0;
    Some(kwh / ops as f64)
}

/// Compute per-service joule deltas vs the previous scrape, advance
/// the per-service `last_raw_joules` table in place, and return only
/// services with a strictly positive delta. Counter-reset and
/// first-observation semantics are documented in design doc 05.
#[allow(clippy::implicit_hasher)]
pub fn joules_deltas(
    samples: &[KeplerSample],
    service_mappings: &HashMap<String, String>,
    last_raw_joules: &mut HashMap<String, f64>,
) -> HashMap<String, f64> {
    // O(N) index over samples so the service loop stays O(N + M)
    // instead of O(N × M) on Kepler endpoints exposing hundreds of
    // containers per node.
    let by_label: HashMap<&str, f64> = samples
        .iter()
        .map(|s| (s.label_value.as_str(), s.joules_total))
        .collect();
    let mut out = HashMap::with_capacity(service_mappings.len());
    for (service, label_value) in service_mappings {
        let Some(&current) = by_label.get(label_value.as_str()) else {
            continue;
        };
        // Update raw-counter table without cloning the key in the
        // steady state.
        let previous = if let Some(slot) = last_raw_joules.get_mut(service) {
            let prev = *slot;
            *slot = current;
            Some(prev)
        } else {
            last_raw_joules.insert(service.clone(), current);
            None
        };
        if let Some(prev) = previous {
            let delta = current - prev;
            // Counter resets and non-finite values are filtered here.
            if delta > 0.0 && delta.is_finite() {
                out.insert(service.clone(), delta);
            }
        }
    }
    out
}

/// Apply a freshly-scraped Kepler batch to a [`KeplerState`]. Services
/// with no ops or no joules delta this window keep their previous entry.
#[allow(clippy::implicit_hasher)]
pub fn apply_scrape(
    state: &KeplerState,
    joules_deltas_map: &HashMap<String, f64>,
    op_deltas: &HashMap<String, u64>,
    now_ms: u64,
) {
    let mut next = state.current_owned();
    let mut any_change = false;
    for (service, &joules_delta) in joules_deltas_map {
        let Some(ops) = op_deltas.get(service).copied() else {
            continue;
        };
        let Some(energy_per_op) = compute_energy_per_op_kwh(joules_delta, ops) else {
            continue;
        };
        let row = ServiceEnergy {
            energy_per_op_kwh: energy_per_op,
            last_update_ms: now_ms,
        };
        // Steady-state update without re-cloning the service key.
        if let Some(slot) = next.get_mut(service.as_str()) {
            *slot = row;
        } else {
            next.insert(service.clone(), row);
        }
        any_change = true;
    }
    if any_change {
        state.publish(next);
    }
}

/// Convenience wrapper: compute deltas and apply in one call. Used by
/// the scraper loop.
#[allow(clippy::implicit_hasher)]
pub fn process_scrape(
    state: &KeplerState,
    samples: &[KeplerSample],
    op_deltas: &HashMap<String, u64>,
    cfg: &KeplerConfig,
    last_raw_joules: &mut HashMap<String, f64>,
    now_ms: u64,
) {
    let joules_deltas_map = joules_deltas(samples, &cfg.service_mappings, last_raw_joules);
    apply_scrape(state, &joules_deltas_map, op_deltas, now_ms);
}
