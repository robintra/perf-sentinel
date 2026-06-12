//! Scaphandre power→energy math + state update.
//!
//! Holds the pieces specific to Scaphandre's per-process power gauge:
//! [`compute_energy_per_op_kwh`] turns `(microwatts, scrape_interval, ops)`
//! into kWh-per-op, and [`apply_scrape`] glues that together with the
//! config's `process_map` to publish a fresh [`ScaphandreState`].
//! The cross-scraper per-service op-delta tracker lives in
//! [`crate::score::ops_snapshot_diff`].

use std::collections::{HashMap, HashSet};

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
/// coefficient via [`compute_energy_per_op_kwh`].
///
/// `multi_match_warned` is carried by the caller across ticks so the
/// ambiguous-matcher warning fires at most once per service per
/// misconfiguration streak (cleared when the service matches cleanly
/// again, so a freshly broken config re-warns). A zero op delta leaves
/// the service's existing entry unchanged, see
/// [`compute_energy_per_op_kwh`].
///
/// `ArcSwap` pattern: builds a new `HashMap` from the current published
/// one (entries not updated this tick are inherited) and publishes it
/// atomically via `ScaphandreState::publish`.
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
    multi_match_warned: &mut HashSet<String>,
    now_ms: u64,
) {
    let interval_secs = cfg.scrape_interval.as_secs_f64();
    let mut next = state.current_owned();
    let mut any_change = false;
    for (service, matcher) in &cfg.process_map {
        let Some(ops) = op_deltas.get(service).copied() else {
            continue; // service had no ops this window → keep previous entry
        };
        // Scaphandre concatenates argv without separators: `java -jar
        // /tmp/svc.jar` is emitted as `cmdline="java-jar/tmp/svc.jar"`.
        // The matcher uses substring containment on both labels, with
        // cmdline_contains optional for processes whose exe is already
        // unique (native binaries). Trichotomy via early-bail to avoid
        // a per-tick Vec allocation.
        let mut unique: Option<&ProcessPower> = None;
        let mut ambiguous = false;
        let mut candidate_count = 0usize;
        for p in power_readings.iter().filter(|p| {
            p.exe.contains(&matcher.exe_contains)
                && matcher
                    .cmdline_contains
                    .as_ref()
                    .is_none_or(|c| p.cmdline.contains(c))
        }) {
            candidate_count += 1;
            if unique.is_some() {
                ambiguous = true;
                break;
            }
            unique = Some(p);
        }
        let reading = match (ambiguous, unique) {
            (false, None) => continue, // process not running → keep previous
            (false, Some(p)) => {
                // Clean match: clear any prior ambiguity warn-latch so
                // a future flap re-emits the warning rather than going
                // silent on the second incident.
                multi_match_warned.remove(service);
                p
            }
            (true, _) => {
                if multi_match_warned.insert(service.clone()) {
                    tracing::warn!(
                        service = %service,
                        candidates = candidate_count,
                        exe_contains = %matcher.exe_contains,
                        cmdline_contains = ?matcher.cmdline_contains,
                        "[green.scaphandre] process_map matcher is ambiguous: \
                         several Scaphandre processes match this service. Add or \
                         tighten cmdline_contains to disambiguate. Skipping this \
                         tick. Subsequent ambiguous ticks for this service will \
                         log at debug level until the matcher resolves cleanly."
                    );
                } else {
                    tracing::debug!(
                        service = %service,
                        candidates = candidate_count,
                        "[green.scaphandre] process_map matcher still ambiguous"
                    );
                }
                continue;
            }
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
