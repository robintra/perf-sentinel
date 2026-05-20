//! Redfish node-level power → per-service energy-per-op attribution.
//! Formula and node-level granularity trade-offs documented in design
//! doc 05 "Kepler and Redfish attribution notes" and
//! `docs/LIMITATIONS.md` "Redfish BMC precision bounds".

use std::collections::HashMap;

use super::state::ServiceEnergy;

/// Build the `chassis_id → Vec<service>` reverse index once at scraper
/// startup. Avoids walking the full `service_mappings` per chassis per
/// tick (was O(C × S)).
#[must_use]
#[allow(clippy::implicit_hasher)]
pub(crate) fn build_chassis_services(
    service_mappings: &HashMap<String, String>,
) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for (svc, chassis_id) in service_mappings {
        out.entry(chassis_id.clone()).or_default().push(svc.clone());
    }
    out
}

/// Apply one chassis observation to an in-progress next-state table
/// (caller hoists `state.current_owned()` and `state.publish()` so
/// multi-chassis ticks pay them once, not C times). Returns whether
/// the table was modified.
///
/// Services with zero ops, or chassis with zero total ops, leave the
/// table unchanged (no division by zero, no flapping).
#[allow(clippy::implicit_hasher)]
pub(crate) fn apply_chassis_scrape(
    next: &mut HashMap<String, ServiceEnergy>,
    chassis_services: &[String],
    chassis_watts: f64,
    scrape_interval_secs: f64,
    op_deltas: &HashMap<String, u64>,
    now_ms: u64,
) -> bool {
    if !chassis_watts.is_finite()
        || chassis_watts <= 0.0
        || !scrape_interval_secs.is_finite()
        || scrape_interval_secs <= 0.0
        || chassis_services.is_empty()
    {
        return false;
    }
    // Single-pass collect of services with positive ops this window.
    // Each service costs one `op_deltas.get(...)` lookup instead of
    // the two-walk pattern (sum then filter) plus a redundant lookup
    // inside the publish loop.
    let mut contributors: Vec<(&str, u64)> = Vec::with_capacity(chassis_services.len());
    let mut total_ops: u64 = 0;
    for svc in chassis_services {
        let ops = op_deltas.get(svc.as_str()).copied().unwrap_or(0);
        if ops > 0 {
            contributors.push((svc.as_str(), ops));
            total_ops = total_ops.saturating_add(ops);
        }
    }
    if total_ops == 0 {
        return false;
    }
    // chassis_joules = watts × seconds; kWh = J / 3.6e6.
    // Per-service contribution is implicitly proportional to its ops
    // since the published coefficient × service_ops sums back to
    // chassis_joules across the mapped set.
    let chassis_kwh = (chassis_watts * scrape_interval_secs) / 3_600_000.0;
    let energy_per_op = chassis_kwh / total_ops as f64;
    if !energy_per_op.is_finite() || energy_per_op <= 0.0 {
        return false;
    }
    let row = ServiceEnergy {
        energy_per_op_kwh: energy_per_op,
        last_update_ms: now_ms,
    };
    for (svc, _) in &contributors {
        // Steady-state path: update in place to skip the key clone.
        if let Some(slot) = next.get_mut(*svc) {
            *slot = row;
        } else {
            next.insert((*svc).to_string(), row);
        }
    }
    !contributors.is_empty()
}
