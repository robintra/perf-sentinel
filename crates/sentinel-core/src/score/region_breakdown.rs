//! Per-region accumulation, folding, model-tag selection, and final
//! [`CarbonReport`] assembly. Consumed by [`super::carbon_compute`].

use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::carbon::{
    self, CO2_MODEL, CO2_MODEL_CLOUD_SPECPOWER, CO2_MODEL_EMAPS, CO2_MODEL_SCAPHANDRE,
    CO2_MODEL_V2, CO2_MODEL_V3, CarbonEstimate, CarbonReport, GENERIC_PUE, IntensitySource,
    REGION_STATUS_KNOWN, REGION_STATUS_OUT_OF_TABLE, REGION_STATUS_UNRESOLVED, RegionBreakdown,
    UNKNOWN_REGION, lookup_region_lower,
};

/// Per-region CO₂ accumulator. `intensity_sum_per_op / total_ops`
/// gives the ops-weighted mean intensity for the breakdown row.
#[derive(Default)]
pub(super) struct RegionAccumulator {
    pub(super) co2_gco2: f64,
    pub(super) total_ops: usize,
    /// Sum of per-op intensities (NOT `ops * intensity`). Mean = sum / `total_ops`.
    pub(super) intensity_sum_per_op: f64,
    /// Highest-fidelity intensity source seen for this region.
    pub(super) max_intensity_source: IntensitySource,
    pub(super) any_scaphandre: bool,
    pub(super) any_cloud_specpower: bool,
    /// Whether any span in this region used a calibrated proxy energy.
    pub(super) any_calibrated: bool,
}

/// Aggregated "which intensity sources / measured-energy backends did
/// the report see?" flags, collected in [`build_region_breakdowns`]
/// and consumed by [`select_co2_model_tag`].
///
/// The six booleans are independent observations that can freely
/// combine (e.g. a single run can see both hourly and cloud `SPECpower`
/// at the same time), so a state machine or enum would not model the
/// domain faithfully. Kept as a flat flag bag with a targeted
/// `struct_excessive_bools` allow.
#[derive(Default, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct ReportFlags {
    any_hourly: bool,
    any_monthly_hourly: bool,
    any_scaphandre: bool,
    any_cloud_specpower: bool,
    any_calibrated: bool,
    any_realtime: bool,
}

/// Fold the per-region accumulators into a breakdown vector, collect
/// the cross-region flags needed for model-tag selection, and compute
/// the total operational CO₂. Regions whose name is not in the
/// embedded carbon table get a dedicated out-of-table row with the
/// generic PUE fallback; the synthetic "unknown" bucket is appended
/// if there are any unresolvable ops.
pub(super) fn build_region_breakdowns(
    per_region: BTreeMap<String, RegionAccumulator>,
    unknown_ops: usize,
) -> (Vec<RegionBreakdown>, ReportFlags, f64) {
    let mut regions: Vec<RegionBreakdown> = Vec::with_capacity(per_region.len() + 1);
    let mut flags = ReportFlags::default();
    let mut operational_gco2: f64 = 0.0;
    for (region, acc) in per_region {
        operational_gco2 += acc.co2_gco2;
        update_flags_from_accumulator(&mut flags, &acc);
        regions.push(build_single_region_row(region, &acc));
    }
    if unknown_ops > 0 {
        tracing::debug!(
            "{unknown_ops} I/O ops had no resolvable region and were excluded \
             from operational CO₂ estimates. Set [green] default_region or \
             [green.service_regions] to attribute them."
        );
        regions.push(RegionBreakdown {
            status: REGION_STATUS_UNRESOLVED,
            region: UNKNOWN_REGION.to_string(),
            grid_intensity_gco2_kwh: 0.0,
            pue: 0.0,
            io_ops: unknown_ops,
            co2_gco2: 0.0,
            intensity_source: IntensitySource::Annual,
        });
    }
    (regions, flags, operational_gco2)
}

/// Merge a single accumulator's intensity source and measured-energy
/// flags into the aggregate [`ReportFlags`].
fn update_flags_from_accumulator(flags: &mut ReportFlags, acc: &RegionAccumulator) {
    match acc.max_intensity_source {
        IntensitySource::RealTime => {
            flags.any_realtime = true;
        }
        IntensitySource::MonthlyHourly => {
            flags.any_monthly_hourly = true;
            flags.any_hourly = true;
        }
        IntensitySource::Hourly => {
            flags.any_hourly = true;
        }
        IntensitySource::Annual => {}
    }
    flags.any_scaphandre |= acc.any_scaphandre;
    flags.any_cloud_specpower |= acc.any_cloud_specpower;
    flags.any_calibrated |= acc.any_calibrated;
}

/// Build a single `RegionBreakdown` row from a finished accumulator.
/// Known regions (present in the embedded carbon table) use the
/// canonical PUE; out-of-table regions use the generic PUE when a
/// custom profile produced non-zero CO₂, and a zeroed row otherwise.
fn build_single_region_row(region: String, acc: &RegionAccumulator) -> RegionBreakdown {
    if let Some((_, pue)) = lookup_region_lower(&region) {
        // Time-weighted mean intensity for display. Guaranteed non-zero
        // ops because the accumulator was only inserted after
        // incrementing total_ops.
        let mean_intensity = acc.intensity_sum_per_op / acc.total_ops as f64;
        let intensity_source = acc.max_intensity_source;
        maybe_warn_eu_central_1_profile(&region, intensity_source);
        return RegionBreakdown {
            status: REGION_STATUS_KNOWN,
            region,
            grid_intensity_gco2_kwh: mean_intensity,
            pue,
            io_ops: acc.total_ops,
            co2_gco2: acc.co2_gco2,
            intensity_source,
        };
    }
    // Out-of-table region: name resolved but not in our table. When a
    // custom hourly profile produced non-zero CO₂ (via the generic PUE
    // fallback), report the actual accumulated values so
    // sum(regions[].co2_gco2) == operational_gco2.
    let has_co2 = acc.co2_gco2 > 0.0;
    if !has_co2 {
        tracing::debug!(
            "Region '{region}' is not in the embedded carbon table; \
             {ops} I/O ops contribute 0 to operational CO₂. \
             See docs/CONFIGURATION.md for the list of supported regions.",
            ops = acc.total_ops
        );
    }
    let mean_intensity = if acc.total_ops > 0 && has_co2 {
        acc.intensity_sum_per_op / acc.total_ops as f64
    } else {
        0.0
    };
    let pue_display = if has_co2 { GENERIC_PUE } else { 0.0 };
    RegionBreakdown {
        status: REGION_STATUS_OUT_OF_TABLE,
        region,
        grid_intensity_gco2_kwh: mean_intensity,
        pue: pue_display,
        io_ops: acc.total_ops,
        co2_gco2: acc.co2_gco2,
        intensity_source: if has_co2 {
            acc.max_intensity_source
        } else {
            IntensitySource::Annual
        },
    }
}

/// One-shot operator hint for the `eu-central-1` hourly profile, whose
/// mean is ~31% above the flat annual value. Logged once per process
/// to help users understand v1 -> v2 / v3 divergences.
fn maybe_warn_eu_central_1_profile(region: &str, intensity_source: IntensitySource) {
    if intensity_source > IntensitySource::Annual && region == "eu-central-1" {
        use std::sync::Once;
        static WARN: Once = Once::new();
        WARN.call_once(|| {
            tracing::debug!(
                region = "eu-central-1",
                annual_gco2_kwh = 338.0,
                hourly_mean_gco2_kwh = 442.0,
                "Hourly carbon profile for eu-central-1 (DE) averages \
                 ~31% above the flat annual value. This reflects recent \
                 grid data. Disable with [green] use_hourly_profiles = false \
                 to use the annual baseline instead."
            );
        });
    }
}

/// Pick the top-level `co2.model` tag given the aggregate flags.
/// Precedence: real-time > Scaphandre > cloud `SPECpower` > monthly/hourly
/// proxy > annual proxy. When a calibration factor applied to the proxy
/// path, the `+cal` variant is returned.
pub(super) fn select_co2_model_tag(flags: ReportFlags) -> &'static str {
    let cal = flags.any_calibrated && !flags.any_scaphandre && !flags.any_cloud_specpower;
    if flags.any_realtime {
        CO2_MODEL_EMAPS
    } else if flags.any_scaphandre {
        CO2_MODEL_SCAPHANDRE
    } else if flags.any_cloud_specpower {
        CO2_MODEL_CLOUD_SPECPOWER
    } else if flags.any_monthly_hourly {
        if cal {
            carbon::CO2_MODEL_V3_CAL
        } else {
            CO2_MODEL_V3
        }
    } else if flags.any_hourly {
        if cal {
            carbon::CO2_MODEL_V2_CAL
        } else {
            CO2_MODEL_V2
        }
    } else if cal {
        carbon::CO2_MODEL_V1_CAL
    } else {
        CO2_MODEL
    }
}

/// Final assembly of the [`CarbonReport`] struct: SCI v1.0 numerator
/// (E × I + M + T), avoidable CO₂ via region-blind ratio, and the
/// methodology tag that shifts when network transport is included.
#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_carbon_report(
    traces_len: usize,
    operational_gco2: f64,
    total_transport_gco2: f64,
    total_io_ops: usize,
    avoidable_io_ops: usize,
    unknown_ops: usize,
    embodied_per_request_gco2: f64,
    model: &'static str,
) -> CarbonReport {
    // SCI v1.0 embodied carbon term M = traces × per-trace constant.
    // Region-independent, emitted unconditionally when we have at least
    // one trace (empty case is early-returned in the caller).
    let embodied_gco2 = traces_len as f64 * embodied_per_request_gco2;

    // Total = SCI v1.0 numerator (E × I) + M, summed over all analyzed
    // traces. NOT the SCI per-R intensity: consumers compute that
    // themselves as total_mid / traces.len() if needed.
    let total_mid = operational_gco2 + embodied_gco2 + total_transport_gco2;

    // Avoidable CO₂ via region-blind ratio. Denominator excludes the
    // unknown bucket (operational_gco2 already excludes it, so we match).
    // Embodied is NOT included in avoidable: hardware emissions are fixed
    // regardless of whether the application does N+1 queries.
    let accounted_io_ops = total_io_ops.saturating_sub(unknown_ops);
    let avoidable_mid = if accounted_io_ops > 0 {
        // Clamp ratio to 1.0: in pathological cases (avoidable ops from
        // unknown-region spans exceeding accounted ops) the ratio can
        // exceed 1.0, which would produce avoidable > operational.
        let ratio = (avoidable_io_ops as f64 / accounted_io_ops as f64).min(1.0);
        operational_gco2 * ratio
    } else {
        0.0
    };

    let transport_gco2 = if total_transport_gco2 > 0.0 {
        Some(total_transport_gco2)
    } else {
        None
    };
    let total_methodology = if transport_gco2.is_some() {
        carbon::METHODOLOGY_SCI_NUMERATOR_TRANSPORT
    } else {
        carbon::METHODOLOGY_SCI_NUMERATOR
    };
    CarbonReport {
        total: CarbonEstimate::new_with_model(total_mid, model, total_methodology),
        avoidable: CarbonEstimate::operational_ratio_with_model(avoidable_mid, model),
        operational_gco2,
        embodied_gco2,
        transport_gco2,
    }
}

/// Sort the per-region breakdown by `co2_gco2` descending with an
/// alphabetical tiebreak. BTreeMap-based accumulation gives stable
/// f64 sums upstream; this final sort is purely cosmetic and the
/// result stays deterministic.
pub(super) fn sort_regions_by_co2_desc(regions: &mut [RegionBreakdown]) {
    regions.sort_by(|a, b| {
        b.co2_gco2
            .partial_cmp(&a.co2_gco2)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.region.cmp(&b.region))
    });
}
