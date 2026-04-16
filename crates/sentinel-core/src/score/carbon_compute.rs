//! Single-pass carbon scoring over a batch of traces. Produces the
//! [`CarbonReport`], the per-region breakdown, and the multi-region flag.
//!
//! The main entry point is [`compute_carbon_report`]; everything else in
//! this module is a per-span helper that keeps the hot loop in
//! [`process_span_for_carbon`] readable.

use std::collections::BTreeMap;

use crate::correlate::Trace;

use super::carbon::{
    CarbonContext, CarbonReport, ENERGY_PER_IO_OP_KWH, GENERIC_PUE, IntensitySource,
    RegionBreakdown, energy_coefficient, extract_hostname, hourly_profile_for_region_lower,
    is_valid_region_id, lookup_region_lower, per_op_gco2, resolve_region,
};
use super::carbon_profiles;
use super::region_breakdown::{
    RegionAccumulator, build_region_breakdowns, finalize_carbon_report, select_co2_model_tag,
    sort_regions_by_co2_desc,
};

/// Maximum number of distinct regions allowed in a single scoring pass.
///
/// Caps `compute_carbon_report`'s per-region `BTreeMap` to prevent memory
/// exhaustion from attacker-controlled `cloud.region` values. 256 is far
/// beyond any realistic cloud deployment footprint. Overflow events are
/// folded into the synthetic `unknown` bucket.
const MAX_REGIONS: usize = 256;

/// Mutable state threaded through the span-processing loop in
/// [`compute_carbon_report`]. Broken out so the per-span and
/// per-region helpers can take a single `&mut` argument instead of
/// 5 separate ones.
#[derive(Default)]
struct CarbonRunState {
    per_region: BTreeMap<String, RegionAccumulator>,
    unknown_ops: usize,
    overflow_warned: bool,
    total_transport_gco2: f64,
    multi_region_active: bool,
}

/// Per-span intensity lookup result: pre-cached profile references + the
/// flat annual intensity + PUE fallback, all derived once to feed the
/// hot-path intensity selection without repeat `HashMap` probes.
///
/// `custom_profile` is a borrow into the user-supplied profile map
/// (lifetime tied to `ctx.custom_hourly_profiles`). `embedded_profile`
/// is a `HourlyProfileRef<'static>` built from the `'static` table
/// constants in [`carbon_profiles`]. The two types
/// expose the same `intensity_at` / `is_monthly` API but are not
/// interchangeable: hence the two fields.
struct SpanRegionContext<'a> {
    region_key: Option<String>,
    region_ref: &'a str,
    custom_profile: Option<&'a carbon_profiles::HourlyProfile>,
    embedded_profile: Option<carbon_profiles::HourlyProfileRef<'static>>,
    annual_intensity: f64,
    pue: f64,
}

/// Compute carbon report, per-region breakdown, and multi-region flag.
/// Single-pass over spans with interleaved hourly/measured/proxy paths.
/// See `docs/design/05-GREENOPS-AND-CARBON.md` for the full algorithm.
pub(super) fn compute_carbon_report(
    traces: &[Trace],
    ctx: &CarbonContext,
    total_io_ops: usize,
    avoidable_io_ops: usize,
) -> (Option<CarbonReport>, Vec<RegionBreakdown>, bool) {
    // Multi-region flag seeded from config; updated from span attributes
    // during the main loop below.
    let mut state = CarbonRunState {
        multi_region_active: !ctx.service_regions.is_empty(),
        ..Default::default()
    };

    // Empty-traces early return. No events → nothing meaningful to report.
    // Still propagate the config-based multi_region_active so that an empty
    // batch with a configured service_regions map is consistent with a
    // non-empty batch from the same config.
    if traces.is_empty() {
        return (None, Vec::new(), state.multi_region_active);
    }

    for trace in traces {
        for span in &trace.spans {
            process_span_for_carbon(&mut state, span, ctx);
        }
    }

    let (mut regions, flags, operational_gco2) =
        build_region_breakdowns(state.per_region, state.unknown_ops);
    let model = select_co2_model_tag(flags);
    let report = finalize_carbon_report(
        traces.len(),
        operational_gco2,
        state.total_transport_gco2,
        total_io_ops,
        avoidable_io_ops,
        state.unknown_ops,
        ctx.embodied_per_request_gco2,
        model,
    );
    sort_regions_by_co2_desc(&mut regions);
    (Some(report), regions, state.multi_region_active)
}

/// Single-span update for the main scoring loop. Resolves the region,
/// intensity, and energy, then accumulates the per-op CO₂ into the
/// right bucket of `state.per_region`. Unknown-region and capped-region
/// cases bump `state.unknown_ops`; the multi-region flag is updated in
/// the same pass.
fn process_span_for_carbon(
    state: &mut CarbonRunState,
    span: &crate::normalize::NormalizedEvent,
    ctx: &CarbonContext,
) {
    // Detect multi-region by span attribute in the same pass.
    if span.event.cloud_region.is_some() {
        state.multi_region_active = true;
    }
    let Some(region_ctx) = resolve_span_region(span, ctx, state) else {
        return;
    };
    let (intensity_used, span_source) = resolve_span_intensity(span, &region_ctx, ctx);
    let (energy_kwh, measured_model, calibrated) = resolve_span_energy(span, ctx);
    let op_co2 = per_op_gco2(energy_kwh, intensity_used, region_ctx.pue);

    let region_ref = region_ctx.region_ref;
    let pue = region_ctx.pue;
    accumulate_span_into_region(
        &mut state.per_region,
        region_ctx,
        op_co2,
        intensity_used,
        span_source,
        measured_model,
        calibrated,
    );

    state.total_transport_gco2 +=
        network_transport_contribution(span, region_ref, intensity_used, pue, ctx);
}

/// Resolve the region for a span, apply the cardinality cap, and
/// pre-cache the profile references + annual intensity + PUE. Returns
/// `None` when the region is unresolvable or the cap rejected it, in
/// which case the caller must bump `unknown_ops`.
fn resolve_span_region<'a>(
    span: &'a crate::normalize::NormalizedEvent,
    ctx: &'a CarbonContext,
    state: &mut CarbonRunState,
) -> Option<SpanRegionContext<'a>> {
    let Some(region_ref) = resolve_region(&span.event, ctx) else {
        state.unknown_ops += 1;
        return None;
    };

    // Defense-in-depth invariant. All regions reaching this loop should
    // have been validated at the ingestion boundary (is_valid_region_id
    // in ingest/otlp.rs and ingest/json.rs) or rejected at config load.
    // Assert the invariant in debug builds so any future ingestion gap
    // fails loudly in test/dev instead of silently reopening log-forging.
    debug_assert!(
        is_valid_region_id(region_ref),
        "unvalidated region '{region_ref}' reached compute_carbon_report; \
         ingestion boundary should have sanitized it"
    );

    // Probe-before-allocate. We still need to allocate the lowercase key
    // for the accumulator lookup because the accumulator is a
    // BTreeMap<String, _>, but we can skip the allocation for the cap
    // check by comparing against `needs_lowercase`.
    let needs_lowercase = region_ref.bytes().any(|b| b.is_ascii_uppercase());
    let region_key: Option<String> = if needs_lowercase {
        Some(region_ref.to_ascii_lowercase())
    } else {
        None
    };
    let region_key_borrow: &str = region_key.as_deref().unwrap_or(region_ref);

    // Region cardinality cap check.
    if state.per_region.len() >= MAX_REGIONS && !state.per_region.contains_key(region_key_borrow) {
        state.unknown_ops += 1;
        if !state.overflow_warned {
            tracing::debug!(
                "Region cardinality cap ({MAX_REGIONS}) exceeded; \
                 additional distinct regions folded into 'unknown'."
            );
            state.overflow_warned = true;
        }
        return None;
    }

    // Single-probe: look up the profile reference once and reuse it for
    // both the "has profile?" check, the PUE fallback, and the intensity
    // read, avoiding redundant `HashMap` probes on the hot path.
    let custom_profile = ctx
        .custom_hourly_profiles
        .as_ref()
        .and_then(|m| m.get(region_key_borrow));

    // Look up annual intensity + PUE. Regions not in the table get
    // (0.0, generic_pue) so their CO₂ from annual intensity is zero but
    // they still produce a breakdown row. When a custom hourly profile
    // or real-time intensity exists for an out-of-table region, a
    // generic PUE (1.2) is used so the intensity is not zeroed by pue=0.
    let has_realtime = ctx
        .real_time_intensity
        .as_ref()
        .is_some_and(|rt| rt.contains_key(region_key_borrow));
    let (annual_intensity, pue) = lookup_region_lower(region_key_borrow).unwrap_or_else(|| {
        let fallback_pue = if custom_profile.is_some() || has_realtime {
            GENERIC_PUE
        } else {
            0.0
        };
        (0.0, fallback_pue)
    });

    let embedded_profile = if custom_profile.is_none() {
        hourly_profile_for_region_lower(region_key_borrow)
    } else {
        None
    };

    Some(SpanRegionContext {
        region_key,
        region_ref,
        custom_profile,
        embedded_profile,
        annual_intensity,
        pue,
    })
}

/// Pick the best-available intensity (g/kWh) + its source tag for a
/// single span. Precedence: Electricity Maps real-time > custom hourly
/// > embedded hourly > flat annual.
///
/// Mirrors `resolve_hourly_intensity` in carbon.rs but uses the
/// pre-cached profile refs from [`SpanRegionContext`] to avoid
/// redundant `HashMap` probes on the hot path.
fn resolve_span_intensity(
    span: &crate::normalize::NormalizedEvent,
    region_ctx: &SpanRegionContext<'_>,
    ctx: &CarbonContext,
) -> (f64, IntensitySource) {
    // Real-time intensity from Electricity Maps takes highest precedence.
    let region_key_borrow: &str = region_ctx
        .region_key
        .as_deref()
        .unwrap_or(region_ctx.region_ref);
    let real_time_val = ctx
        .real_time_intensity
        .as_ref()
        .and_then(|rt| rt.get(region_key_borrow));
    if let Some(&rt_intensity) = real_time_val {
        return (rt_intensity, IntensitySource::RealTime);
    }

    let region_has_hourly = ctx.use_hourly_profiles
        && (region_ctx.custom_profile.is_some() || region_ctx.embedded_profile.is_some());
    if !region_has_hourly {
        return (region_ctx.annual_intensity, IntensitySource::Annual);
    }

    // Hourly intensity lookup. parse_utc_hour returns None for non-UTC
    // offsets and non-ISO-8601 shapes, in which case we fall back to the
    // flat annual intensity rather than silently using a default hour.
    let Some(hour) = crate::time::parse_utc_hour(&span.event.timestamp) else {
        return (region_ctx.annual_intensity, IntensitySource::Annual);
    };
    let month_opt = crate::time::parse_utc_month(&span.event.timestamp);

    if let Some(cp) = region_ctx.custom_profile {
        let val = cp.intensity_at(hour, month_opt);
        let src = if cp.is_monthly() {
            IntensitySource::MonthlyHourly
        } else {
            IntensitySource::Hourly
        };
        return (val, src);
    }
    if let Some(ep) = region_ctx.embedded_profile {
        let val = ep.intensity_at(hour, month_opt);
        let src = if ep.is_monthly() {
            IntensitySource::MonthlyHourly
        } else {
            IntensitySource::Hourly
        };
        return (val, src);
    }

    // Invariant: region_has_hourly implies custom or embedded is Some.
    // This branch is unreachable.
    debug_assert!(false, "region_has_hourly was true but no profile found");
    (region_ctx.annual_intensity, IntensitySource::Annual)
}

/// Pick the best-available energy (kWh) for a single span and report
/// whether it came from a measured snapshot and whether a calibration
/// factor was applied. Measured energy overrides the proxy model
/// (calibrated or not); if no snapshot entry exists, the proxy model
/// is used with optional per-operation weighting and optional per-service
/// calibration factor.
fn resolve_span_energy(
    span: &crate::normalize::NormalizedEvent,
    ctx: &CarbonContext,
) -> (f64, Option<&'static str>, bool) {
    let mut proxy_energy_kwh = if ctx.per_operation_coefficients {
        ENERGY_PER_IO_OP_KWH * energy_coefficient(&span.event)
    } else {
        ENERGY_PER_IO_OP_KWH
    };
    let calibrated = if let Some(ref cal) = ctx.calibration {
        if let Some(factor) = cal.factor_for(&span.event.service) {
            proxy_energy_kwh *= factor;
            true
        } else {
            false
        }
    } else {
        false
    };
    let (energy_kwh, measured_model) = match &ctx.energy_snapshot {
        Some(snapshot) => match snapshot.get(&span.event.service) {
            Some(entry) => (entry.energy_per_op_kwh, Some(entry.model_tag)),
            None => (proxy_energy_kwh, None),
        },
        None => (proxy_energy_kwh, None),
    };
    (energy_kwh, measured_model, calibrated)
}

/// Add the per-span CO₂ contribution to the right accumulator bucket.
/// Three allocation paths on the hot loop to minimize per-span work
/// when the region key is already lowercased and present.
fn accumulate_span_into_region(
    per_region: &mut BTreeMap<String, RegionAccumulator>,
    region_ctx: SpanRegionContext<'_>,
    op_co2: f64,
    intensity_used: f64,
    span_source: IntensitySource,
    measured_model: Option<&'static str>,
    calibrated: bool,
) {
    let SpanRegionContext {
        region_key,
        region_ref,
        ..
    } = region_ctx;
    // Obtain or insert the accumulator. Three paths to minimize
    // allocations on the hot per-span loop:
    //   1. region_key is Some  -> already allocated the lowercase key,
    //      use entry() which moves the owned String into the map.
    //   2. region_key is None AND key exists -> single get_mut() with a
    //      borrowed &str, zero allocation.
    //   3. region_key is None AND key absent -> allocate once via
    //      entry(region_ref.to_string()).
    let acc = if let Some(lowered) = region_key {
        per_region.entry(lowered).or_default()
    } else if let Some(existing) = per_region.get_mut(region_ref) {
        existing
    } else {
        per_region.entry(region_ref.to_string()).or_default()
    };
    acc.co2_gco2 += op_co2;
    acc.total_ops += 1;
    acc.intensity_sum_per_op += intensity_used;
    if span_source > acc.max_intensity_source {
        acc.max_intensity_source = span_source;
    }
    match measured_model {
        Some(super::carbon::CO2_MODEL_SCAPHANDRE) => acc.any_scaphandre = true,
        Some(super::carbon::CO2_MODEL_CLOUD_SPECPOWER) => acc.any_cloud_specpower = true,
        _ => {
            if calibrated {
                acc.any_calibrated = true;
            }
        }
    }
}

/// Compute the network transport CO₂ contribution for a single span.
/// Returns 0 unless transport accounting is enabled, the span is an
/// HTTP-out call with a `response_size_bytes`, the callee's region is
/// mapped via `ctx.service_regions`, and the caller's region differs
/// from the callee's (cross-region only).
fn network_transport_contribution(
    span: &crate::normalize::NormalizedEvent,
    caller_region: &str,
    intensity_used: f64,
    pue: f64,
    ctx: &CarbonContext,
) -> f64 {
    if !ctx.include_network_transport || span.event.event_type != crate::event::EventType::HttpOut {
        return 0.0;
    }
    let Some(bytes) = span.event.response_size_bytes else {
        return 0.0;
    };
    // Probe-before-allocate: only lowercase the hostname when it contains
    // uppercase bytes (same pattern as region keys).
    let callee_region = extract_hostname(&span.event.target)
        .and_then(|host| {
            if host.bytes().any(|b| b.is_ascii_uppercase()) {
                ctx.service_regions.get(&host.to_ascii_lowercase())
            } else {
                ctx.service_regions.get(host)
            }
        })
        .map(String::as_str);
    let Some(callee) = callee_region else {
        return 0.0;
    };
    if caller_region.eq_ignore_ascii_case(callee) {
        return 0.0;
    }
    // `bytes` is the response body only (request body is not available
    // in standard OTel HTTP semantic conventions). We use the caller's
    // grid intensity and PUE as a proxy for the network infrastructure's
    // actual grid mix, which is distributed and unknown. Documented in
    // LIMITATIONS.md.
    let transport_energy = bytes as f64 * ctx.network_energy_per_byte_kwh;
    transport_energy * intensity_used * pue
}
