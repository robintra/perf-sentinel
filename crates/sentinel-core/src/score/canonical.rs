//! Canonical-threshold avoidable computation for periodic disclosure.
//!
//! The operator's `n_plus_one_threshold` decides which N+1 patterns become
//! findings, so raising it shrinks the avoidable energy/carbon a disclosure
//! would report. To keep the public figure non-manipulable, the daemon
//! archives the avoidable at a fixed canonical threshold
//! ([`DISCLOSURE_N_PLUS_ONE_THRESHOLD`]) alongside the
//! operator-threshold one. Daemon-only (the `disclose` subcommand reads
//! pre-computed tiers); the anti-gaming invariant tests run under the
//! `daemon` feature (`cargo test -p perf-sentinel-core --features daemon`).

use crate::correlate::Trace;
use crate::detect::{DISCLOSURE_N_PLUS_ONE_THRESHOLD, DetectConfig, n_plus_one, redundant};
use crate::report::{AvoidableTier, DisclosureDbWaste, DisclosureWaste, GreenSummary};

use super::AvoidableIoOps;
use super::dedup_avoidable_io_ops;
use super::region_breakdown::avoidable_share;

/// Re-run N+1 at [`DISCLOSURE_N_PLUS_ONE_THRESHOLD`] (then redundant against
/// that set) over every trace, and dedup the avoidable I/O ops (total and
/// SQL-only). Only the N+1 threshold is overridden; window and sanitizer
/// mode stay as configured.
#[must_use]
pub(crate) fn compute_canonical_avoidable(
    traces: &[Trace],
    detect_config: &DetectConfig,
) -> AvoidableIoOps {
    let mut findings = Vec::new();
    for trace in traces {
        let mut n1 = n_plus_one::detect_n_plus_one(
            trace,
            DISCLOSURE_N_PLUS_ONE_THRESHOLD,
            detect_config.window_ms,
            detect_config.sanitizer_aware_classification,
        );
        let mut redundant = redundant::detect_redundant(trace, &n1);
        findings.append(&mut n1);
        findings.append(&mut redundant);
    }
    dedup_avoidable_io_ops(&findings)
}

/// Build both avoidable tiers from the scored operational [`GreenSummary`]
/// plus a canonical detection pass. Operational carbon reuses the summary's
/// `co2.avoidable`; canonical carbon is rescaled from `operational_gco2` via
/// [`avoidable_share`] (same denominator). No second carbon pass.
#[must_use]
pub(crate) fn compute_disclosure_waste(
    traces: &[Trace],
    operational: &GreenSummary,
    detect_config: &DetectConfig,
) -> DisclosureWaste {
    let accounted = operational.accounted_io_ops;
    let energy_kwh = operational.energy_kwh;
    let operational_gco2 = operational.co2.as_ref().map_or(0.0, |r| r.operational_gco2);
    let operational_avoidable_gco2 = operational.co2.as_ref().map_or(0.0, |r| r.avoidable.mid);

    let canonical = compute_canonical_avoidable(traces, detect_config);
    let canonical_io = canonical.total;

    DisclosureWaste {
        canonical: AvoidableTier {
            n_plus_one_threshold: DISCLOSURE_N_PLUS_ONE_THRESHOLD,
            avoidable_io_ops: canonical_io,
            avoidable_kwh: avoidable_share(energy_kwh, canonical_io, accounted),
            avoidable_gco2: avoidable_share(operational_gco2, canonical_io, accounted),
        },
        operational: AvoidableTier {
            n_plus_one_threshold: detect_config.n_plus_one_threshold,
            avoidable_io_ops: operational.avoidable_io_ops,
            avoidable_kwh: avoidable_share(energy_kwh, operational.avoidable_io_ops, accounted),
            avoidable_gco2: operational_avoidable_gco2,
        },
        database: build_db_waste_tiers(operational, canonical.sql),
    }
}

/// Both database-waste tiers from the window's operational figure. The
/// canonical tier reuses the same energy with the SQL ratio recomputed
/// at the canonical threshold; its gCO₂ scales the ratio-independent
/// `energy_gco2` base, so an operator threshold that zeroes the
/// operational figure cannot zero the canonical carbon leg.
fn build_db_waste_tiers(
    operational: &GreenSummary,
    canonical_sql_avoidable: usize,
) -> Option<DisclosureDbWaste> {
    let db = operational.database_waste.as_ref()?;
    let total_sql = operational.total_sql_io_ops;
    let canonical_ratio = if total_sql == 0 {
        0.0
    } else {
        (canonical_sql_avoidable as f64 / total_sql as f64).min(1.0)
    };
    Some(DisclosureDbWaste {
        energy_kwh: db.energy_kwh,
        model: db.model.clone(),
        operational_waste_kwh: db.waste_kwh,
        operational_waste_gco2: db.waste_gco2,
        canonical_waste_kwh: db.energy_kwh * canonical_ratio,
        canonical_waste_gco2: db.energy_gco2.map(|g| g * canonical_ratio),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::score::carbon::{CarbonEstimate, CarbonReport};
    use crate::test_helpers::{make_n_plus_one_events, make_trace};

    fn detect_config(n_plus_one_threshold: u32) -> DetectConfig {
        let mut cfg = DetectConfig::from(&Config::default());
        cfg.n_plus_one_threshold = n_plus_one_threshold;
        cfg
    }

    /// Anti-gaming invariant: the canonical avoidable count is identical
    /// whether the operator's threshold is sensitive (2) or so high it finds
    /// nothing (50). The 6-occurrence fixture yields `6 - 1 = 5` avoidable
    /// ops at the canonical threshold regardless.
    #[test]
    fn canonical_avoidable_independent_of_operational_threshold() {
        let traces = vec![make_trace(make_n_plus_one_events())];

        let low = compute_canonical_avoidable(&traces, &detect_config(2));
        let high = compute_canonical_avoidable(&traces, &detect_config(50));

        assert_eq!(
            low, high,
            "canonical count must not depend on operator config"
        );
        assert_eq!(low.total, 5);
        assert_eq!(low.sql, 5);
    }

    /// With the operator threshold at 50 the operational tier sees zero
    /// avoidable, while the canonical tier still reports the waste with its
    /// energy and carbon shares.
    #[test]
    fn disclosure_waste_keeps_canonical_when_operational_hidden() {
        let traces = vec![make_trace(make_n_plus_one_events())];
        let operational = GreenSummary {
            total_io_ops: 6,
            avoidable_io_ops: 0,
            accounted_io_ops: 6,
            energy_kwh: 2.0,
            co2: Some(CarbonReport {
                total: CarbonEstimate::sci_numerator(12.0),
                avoidable: CarbonEstimate::operational_ratio(0.0),
                operational_gco2: 12.0,
                embodied_gco2: 0.0,
                transport_gco2: None,
                sci_per_trace: None,
                functional_unit: String::new(),
            }),
            ..GreenSummary::disabled(0)
        };

        let waste = compute_disclosure_waste(&traces, &operational, &detect_config(50));

        // Operator tier is empty (threshold 50 finds nothing).
        assert_eq!(waste.operational.n_plus_one_threshold, 50);
        assert_eq!(waste.operational.avoidable_io_ops, 0);
        assert!(waste.operational.avoidable_gco2.abs() < 1e-12);
        assert!(waste.operational.avoidable_kwh.abs() < 1e-12);

        // Canonical tier still reports the waste: 5 ops out of 6 accounted.
        assert_eq!(waste.canonical.n_plus_one_threshold, 2);
        assert_eq!(waste.canonical.avoidable_io_ops, 5);
        // 12.0 gCO2 * (5 / 6)
        assert!((waste.canonical.avoidable_gco2 - 10.0).abs() < 1e-9);
        // 2.0 kWh * (5 / 6)
        assert!((waste.canonical.avoidable_kwh - (2.0 * 5.0 / 6.0)).abs() < 1e-9);
    }

    /// Same anti-gaming property for the database block: an operator
    /// threshold that hides every finding zeroes the operational figure
    /// but the canonical tier recomputes the SQL ratio at the pinned
    /// threshold against the same energy.
    #[test]
    fn disclosure_database_waste_recomputes_canonical_ratio() {
        let traces = vec![make_trace(make_n_plus_one_events())];
        let mut operational = GreenSummary::disabled(6);
        operational.total_sql_io_ops = 6;
        operational.database_waste = Some(crate::report::DatabaseWaste {
            energy_kwh: 1.2,
            waste_kwh: 0.0,
            waste_gco2: None,
            // gCO2 of the whole energy: the base the canonical carbon
            // leg scales, immune to the zeroed operational ratio.
            energy_gco2: Some(6.0),
            region: None,
            sql_waste_ratio: 0.0,
            model: "alumet_rapl".to_string(),
        });

        let waste = compute_disclosure_waste(&traces, &operational, &detect_config(50));

        let db = waste.database.expect("database tiers");
        assert_eq!(db.model, "alumet_rapl");
        assert!((db.energy_kwh - 1.2).abs() < 1e-12);
        // Operator threshold 50 finds nothing: operational waste is zero.
        assert!(db.operational_waste_kwh.abs() < 1e-12);
        // Canonical: 5 avoidable SQL of 6 total SQL against 1.2 kWh.
        assert!((db.canonical_waste_kwh - 1.2 * 5.0 / 6.0).abs() < 1e-9);
        // The canonical carbon leg survives the zeroed operator ratio:
        // 6.0 gCO2 of energy × the canonical 5/6 ratio.
        assert!((db.canonical_waste_gco2.expect("carbon leg") - 5.0).abs() < 1e-9);
    }

    #[test]
    fn db_waste_tiers_zero_sql_yields_zero_canonical() {
        // total_sql_io_ops == 0: the canonical ratio is 0, so both canonical
        // legs collapse to zero without dividing by zero.
        let mut operational = GreenSummary::disabled(0);
        operational.total_sql_io_ops = 0;
        operational.database_waste = Some(crate::report::DatabaseWaste {
            energy_kwh: 1.0,
            waste_kwh: 0.0,
            waste_gco2: None,
            energy_gco2: Some(3.0),
            region: None,
            sql_waste_ratio: 0.0,
            model: "estimated".to_string(),
        });
        let db = build_db_waste_tiers(&operational, 0).expect("tiers");
        assert!(db.canonical_waste_kwh.abs() < 1e-12);
        assert!(db.canonical_waste_gco2.expect("carbon leg").abs() < 1e-12);
    }
}
