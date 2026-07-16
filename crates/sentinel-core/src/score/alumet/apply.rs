//! Alumet interval-energy math + state update.
//!
//! Methodology (why the reading is neither a power gauge nor a
//! cumulative counter) lives in `docs/design/05-GREENOPS-AND-CARBON.md`
//! "Alumet interval-energy attribution".

use std::collections::HashMap;

use super::config::AlumetConfig;
use super::state::{AlumetState, ServiceEnergy};
use crate::score::prom_parser::PromSample;

/// Convert one Alumet energy reading + observed op count into an
/// energy-per-op coefficient (kWh per op).
///
/// Formula:
/// ```text
///   watts            = joules_per_interval / energy_interval_secs
///   window_joules    = watts × scrape_interval_secs
///   energy_per_op_kwh = window_joules / (ops × 3_600_000)
/// ```
///
/// The division by `energy_interval_secs` is what makes an Alumet
/// reading comparable to a Scaphandre one: Alumet publishes the joules
/// burned during one source `poll_interval`, so the raw number is
/// meaningless until it is turned back into a rate. Summing raw readings
/// across scrapes would be wrong in both directions (double-counting
/// when scraping faster than Alumet flushes, dropped intervals when
/// scraping slower).
///
/// Returns `None` when the math is meaningless (zero ops, non-finite or
/// negative energy, non-positive intervals, or an overflowing product)
/// so the caller keeps the previous entry rather than publishing a
/// division-by-zero or a flapping coefficient for an idle service.
#[must_use]
pub fn compute_energy_per_op_kwh(
    joules_per_interval: f64,
    energy_interval_secs: f64,
    scrape_interval_secs: f64,
    ops: u64,
) -> Option<f64> {
    // `<= 0.0` rather than `< 0.0`: a zero reading means the exporter's
    // last flush caught the consumer idle, not that the work in this
    // scrape window was free. Publishing 0.0 would override every
    // lower-tier backend with a measured zero for a service that
    // demonstrably did I/O. Mirrors Kepler's `delta > 0.0` filter, the
    // caller keeps the previous entry instead.
    if ops == 0 || !joules_per_interval.is_finite() || joules_per_interval <= 0.0 {
        return None;
    }
    // Config validation already rejects these, re-checked here because
    // the math is a public entry point and a zero interval would divide
    // by zero into an infinite coefficient.
    if !energy_interval_secs.is_finite()
        || energy_interval_secs <= 0.0
        || !scrape_interval_secs.is_finite()
        || scrape_interval_secs <= 0.0
    {
        return None;
    }
    let watts = joules_per_interval / energy_interval_secs;
    let window_joules = watts * scrape_interval_secs;
    // 1 kWh = 3.6e6 J.
    let kwh = window_joules / 3_600_000.0;
    let per_op = kwh / ops as f64;
    per_op.is_finite().then_some(per_op)
}

/// Apply a freshly-scraped Alumet batch to an [`AlumetState`]. Services
/// with no ops this window keep their previous entry.
///
/// Unlike Kepler, there is no delta bookkeeping: the reading already is
/// an interval delta, so each scrape stands alone and an exporter
/// restart needs no counter-reset guard.
///
/// Returns how many mapped services found their label on the wire,
/// independent of whether they had ops. The caller uses it to tell
/// "the endpoint answered but nothing maps to my services" (a config
/// error) from "everything matched but the services were idle" (fine).
#[allow(clippy::implicit_hasher)]
pub fn apply_scrape(
    state: &AlumetState,
    samples: &[PromSample],
    op_deltas: &HashMap<String, u64>,
    cfg: &AlumetConfig,
    now_ms: u64,
) -> usize {
    // O(N) index over samples so the service loop stays O(N + M) on
    // endpoints exposing hundreds of series.
    //
    // Values are SUMMED per label, not overwritten: Alumet's
    // `label_key` is operator-chosen and collisions are routine rather
    // than exceptional. One `name="checkout-pod"` carries a row per
    // RAPL domain (package + dram), and `label_key = "domain"` on a
    // dual-socket host carries one `domain="package"` row per socket.
    // Energy is additive, so summing is the physically correct read;
    // `.collect()` would keep whichever row the exposition emitted last
    // and silently halve the figure under a `measured` provenance tag.
    // (Kepler's `joules_deltas` keeps its historical last-write-wins
    // read; its pinned label keys can collide too, e.g. one container
    // name repeated across pods, but that is shipped behavior out of
    // scope here.)
    // Per-row validation happens HERE, not only on the sum: the
    // Prometheus text format legitimately carries NaN, and one NaN row
    // would poison every row sharing its label, while a negative row
    // would subtract from an otherwise valid sum and understate the
    // published figure. Rejected rows still create the entry so the
    // label counts as matched (the series exists on the wire, the
    // mapping is not the problem).
    let mut by_label: HashMap<&str, f64> = HashMap::with_capacity(samples.len());
    for s in samples {
        let slot = by_label.entry(s.label_value.as_str()).or_insert(0.0);
        if s.value.is_finite() && s.value > 0.0 {
            *slot += s.value;
        }
    }
    let scrape_interval_secs = cfg.scrape_interval.as_secs_f64();
    let mut next = state.current_owned();
    let mut any_change = false;
    let mut matched = 0usize;
    for (service, label_value) in &cfg.service_mappings {
        let Some(&joules) = by_label.get(label_value.as_str()) else {
            continue;
        };
        // Counted before the ops gate: the label exists on the wire, so
        // the mapping is right even if the service happens to be idle.
        matched += 1;
        let Some(ops) = op_deltas.get(service).copied() else {
            continue;
        };
        let Some(energy_per_op) =
            compute_energy_per_op_kwh(joules, cfg.energy_interval_secs, scrape_interval_secs, ops)
        else {
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
    matched
}
