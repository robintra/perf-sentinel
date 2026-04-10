//! Scoring stage: computes `GreenOps` I/O intensity scores.

pub mod carbon;
pub(crate) mod carbon_profiles;
pub mod cloud_energy;
pub mod scaphandre;

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::correlate::Trace;
use crate::detect::{Finding, GreenImpact};
use crate::report::{GreenSummary, TopOffender};
use carbon::{
    CO2_MODEL, CO2_MODEL_CLOUD_SPECPOWER, CO2_MODEL_SCAPHANDRE, CO2_MODEL_V2, CO2_MODEL_V3,
    CarbonContext, CarbonEstimate, CarbonReport, ENERGY_PER_IO_OP_KWH, GENERIC_PUE,
    IntensitySource, REGION_STATUS_KNOWN, REGION_STATUS_OUT_OF_TABLE, REGION_STATUS_UNRESOLVED,
    RegionBreakdown, UNKNOWN_REGION, energy_coefficient, extract_hostname,
    hourly_profile_for_region_lower, is_valid_region_id, lookup_region_lower, per_op_gco2,
    resolve_region,
};

/// Maximum number of distinct regions allowed in a single scoring pass.
///
/// Caps `compute_carbon_report`'s per-region `BTreeMap` to prevent memory
/// exhaustion from attacker-controlled `cloud.region` values. 256 is far
/// beyond any realistic cloud deployment footprint. Overflow events are
/// folded into the synthetic `unknown` bucket.
const MAX_REGIONS: usize = 256;

/// Per-endpoint statistics accumulated during scoring.
struct EndpointStats<'a> {
    total_io_ops: usize,
    invocation_count: usize,
    service: &'a str,
}

/// Count I/O ops per endpoint and invocations (distinct traces per endpoint).
fn count_endpoint_stats(traces: &[Trace]) -> (HashMap<&str, EndpointStats<'_>>, usize) {
    let mut endpoint_stats: HashMap<&str, EndpointStats<'_>> =
        HashMap::with_capacity(traces.len().min(64));
    let mut total_io_ops: usize = 0;
    let mut seen_endpoints: HashSet<&str> = HashSet::new();

    for trace in traces {
        seen_endpoints.clear();
        for span in &trace.spans {
            total_io_ops += 1;
            let key = span.event.source.endpoint.as_str();
            let stats = endpoint_stats.entry(key).or_insert_with(|| EndpointStats {
                total_io_ops: 0,
                invocation_count: 0,
                service: span.event.service.as_str(),
            });
            stats.total_io_ops += 1;
            seen_endpoints.insert(key);
        }
        for &ep in &seen_endpoints {
            if let Some(stats) = endpoint_stats.get_mut(ep) {
                stats.invocation_count += 1;
            }
        }
    }

    (endpoint_stats, total_io_ops)
}

/// Compute `GreenOps` scores: enrich findings with `green_impact` and produce a `GreenSummary`.
///
/// I/O operation counts are used as a proxy for energy consumption.
/// This is an approximation; actual energy depends on I/O type, latency,
/// and infrastructure, and is not measured directly.
///
/// When `carbon` is `Some`, the function additionally computes:
/// - **Operational CO₂** per region using the SCI `O = E × I` term, with
///   per-region bucketing via [`resolve_region`].
/// - **Embodied CO₂** via the SCI `M` term (`traces.len() × embodied_per_request_gco2`).
/// - **Confidence intervals** (low/mid/high) — 2× multiplicative uncertainty
///   bracket around the I/O proxy midpoint.
/// - **Avoidable CO₂** via the region-blind ratio
///   `operational × (avoidable_io_ops / accounted_io_ops)`, where
///   `accounted_io_ops` excludes the synthetic unknown bucket.
///
/// When `carbon` is `None`, no CO₂ is computed; the deprecated scalar
/// fields and the new `co2` / `regions` fields are all left empty.
///
/// Algorithm:
/// 1. Count I/O ops per source endpoint across all traces.
/// 2. Compute IIS (I/O Intensity Score) per endpoint.
/// 3. Dedup avoidable I/O ops using max per trace/template pair.
/// 4. Populate `green_impact` on each finding.
/// 5. Build top offenders ranking sorted by IIS descending.
///    Per-offender `co2_grams` is `None` when multi-region scoring is active.
/// 6. (If `carbon` is `Some` and `traces` non-empty) bucket I/O ops per region,
///    compute operational + embodied CO₂, build the structured `CarbonReport`
///    and the per-region breakdown (sorted by `co2_gco2` DESC).
#[must_use]
#[allow(clippy::too_many_lines)] // Pipeline stage with carbon scoring branches; splitting
// would obscure the data flow.
pub fn score_green(
    traces: &[Trace],
    findings: Vec<Finding>,
    carbon: Option<&CarbonContext>,
) -> (Vec<Finding>, GreenSummary) {
    let (endpoint_stats, total_io_ops) = count_endpoint_stats(traces);

    // Dedup avoidable I/O ops by (trace_id, template, source_endpoint), taking max.
    // Slow findings are excluded: slow queries are not "avoidable" I/O, they are
    // necessary operations that happen to be slow.
    let mut dedup: HashMap<(&str, &str, &str), usize> = HashMap::with_capacity(findings.len());
    for f in &findings {
        if !f.finding_type.is_avoidable_io() {
            continue;
        }
        let avoidable = f.pattern.occurrences.saturating_sub(1);
        let entry = dedup
            .entry((&f.trace_id, &f.pattern.template, &f.source_endpoint))
            .or_insert(0);
        *entry = (*entry).max(avoidable);
    }
    let avoidable_io_ops: usize = dedup.values().sum();

    // Compute IIS per endpoint (cached for finding enrichment)
    let iis_map: HashMap<&str, f64> = endpoint_stats
        .iter()
        .map(|(&ep, stats)| {
            let invocations = stats.invocation_count.max(1) as f64;
            (ep, stats.total_io_ops as f64 / invocations)
        })
        .collect();

    // Enrich findings with green_impact
    let mut enriched = findings;
    for f in &mut enriched {
        let iis = iis_map
            .get(f.source_endpoint.as_str())
            .copied()
            .unwrap_or(0.0);

        let extra = if f.finding_type.is_avoidable_io() {
            f.pattern.occurrences.saturating_sub(1)
        } else {
            0
        };
        f.green_impact = Some(GreenImpact {
            estimated_extra_io_ops: extra,
            io_intensity_score: iis,
        });
    }

    // Multi-region carbon scoring (before top offenders so we can
    // reuse the multi_region_active flag it computes). Only runs when a
    // CarbonContext is provided. Builds the per-region breakdown using a
    // BTreeMap for deterministic accumulation order.
    //
    // Multi-region detection is folded into compute_carbon_report's
    // single span pass.
    let (co2, regions, multi_region_active) = match carbon {
        Some(ctx) => compute_carbon_report(traces, ctx, total_io_ops, avoidable_io_ops),
        None => (None, Vec::new(), false),
    };

    // Top-offender co2_grams uses the flat ENERGY_PER_IO_OP_KWH, so it's
    // only emitted in mono-region mode with per-op coefficients disabled.
    // Otherwise the scalar would be inconsistent with the per-region breakdown.
    let per_op_active = carbon.is_some_and(|ctx| ctx.per_operation_coefficients);
    let default_region_lower = if multi_region_active || per_op_active {
        None
    } else {
        carbon
            .and_then(|ctx| ctx.default_region.as_deref())
            .map(str::to_ascii_lowercase)
    };
    let mut top_offenders: Vec<TopOffender> = endpoint_stats
        .iter()
        .map(|(endpoint, stats)| {
            let iis = iis_map.get(endpoint).copied().unwrap_or(0.0);
            let co2_grams = default_region_lower
                .as_deref()
                .and_then(|r| carbon::io_ops_to_co2_grams(stats.total_io_ops, r));
            TopOffender {
                endpoint: (*endpoint).to_string(),
                service: stats.service.to_string(),
                io_intensity_score: iis,
                co2_grams,
            }
        })
        .collect();
    top_offenders.sort_by(|a, b| {
        b.io_intensity_score
            .partial_cmp(&a.io_intensity_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.endpoint.cmp(&b.endpoint))
    });

    let green_summary = GreenSummary {
        total_io_ops,
        avoidable_io_ops,
        io_waste_ratio: if total_io_ops > 0 {
            avoidable_io_ops as f64 / total_io_ops as f64
        } else {
            0.0
        },
        top_offenders,
        // Hoisted from co2.transport_gco2 for top-level JSON visibility:
        // consumers can read transport_gco2 without navigating into the
        // nested co2 object. The canonical value lives in CarbonReport.
        transport_gco2: co2.as_ref().and_then(|r| r.transport_gco2),
        co2,
        regions,
    };

    (enriched, green_summary)
}

/// Per-region CO₂ accumulator. `intensity_sum_per_op / total_ops`
/// gives the ops-weighted mean intensity for the breakdown row.
#[derive(Default)]
struct RegionAccumulator {
    co2_gco2: f64,
    total_ops: usize,
    /// Sum of per-op intensities (NOT `ops * intensity`). Mean = sum / `total_ops`.
    intensity_sum_per_op: f64,
    /// Highest-fidelity intensity source seen for this region.
    max_intensity_source: IntensitySource,
    any_scaphandre: bool,
    any_cloud_specpower: bool,
}

/// Compute carbon report, per-region breakdown, and multi-region flag.
/// Single-pass over spans with interleaved hourly/measured/proxy paths.
/// See `docs/design/05-GREENOPS-AND-CARBON.md` for the full algorithm.
#[allow(clippy::too_many_lines)]
fn compute_carbon_report(
    traces: &[Trace],
    ctx: &CarbonContext,
    total_io_ops: usize,
    avoidable_io_ops: usize,
) -> (Option<CarbonReport>, Vec<RegionBreakdown>, bool) {
    // Multi-region flag seeded from config; updated
    // from span attributes during the main loop below.
    let mut multi_region_active = !ctx.service_regions.is_empty();

    // Empty-traces early return. No events → nothing meaningful to report.
    // Still propagate the config-based multi_region_active so that an empty
    // batch with a configured service_regions map is consistent with a
    // non-empty batch from the same config.
    if traces.is_empty() {
        return (None, Vec::new(), multi_region_active);
    }

    // Bucket I/O ops per resolved region. Region keys are lowercased so
    // case variants (e.g. "EU-West-3" vs "eu-west-3") collapse into one bucket.
    // BTreeMap gives deterministic iteration order → stable f64 sums across runs.
    let mut per_region: BTreeMap<String, RegionAccumulator> = BTreeMap::new();
    let mut unknown_ops: usize = 0;
    let mut overflow_warned = false;
    let mut total_transport_gco2: f64 = 0.0;
    for trace in traces {
        for span in &trace.spans {
            // Detect multi-region by span attribute in the same pass.
            if span.event.cloud_region.is_some() {
                multi_region_active = true;
            }
            let Some(region_ref) = resolve_region(&span.event, ctx) else {
                unknown_ops += 1;
                continue;
            };

            // Defense-in-depth invariant. All regions reaching this
            // loop should have been validated at the ingestion boundary
            // (is_valid_region_id in ingest/otlp.rs and ingest/json.rs)
            // or rejected at config load. Assert the invariant in debug
            // builds so any future ingestion gap fails loudly in
            // test/dev instead of silently reopening log-forging.
            debug_assert!(
                is_valid_region_id(region_ref),
                "unvalidated region '{region_ref}' reached compute_carbon_report; \
                 ingestion boundary should have sanitized it"
            );

            // Probe-before-allocate. We still need to
            // allocate the lowercase key for the accumulator lookup
            // because the accumulator is a BTreeMap<String, _>, but we
            // can skip the allocation for the cap check by comparing
            // against `needs_lowercase`.
            let needs_lowercase = region_ref.bytes().any(|b| b.is_ascii_uppercase());
            let region_key: Option<String> = if needs_lowercase {
                Some(region_ref.to_ascii_lowercase())
            } else {
                None
            };

            // Region cardinality cap check.
            let region_key_borrow: &str = region_key.as_deref().unwrap_or(region_ref);
            if per_region.len() >= MAX_REGIONS && !per_region.contains_key(region_key_borrow) {
                unknown_ops += 1;
                if !overflow_warned {
                    tracing::debug!(
                        "Region cardinality cap ({MAX_REGIONS}) exceeded; \
                         additional distinct regions folded into 'unknown'."
                    );
                    overflow_warned = true;
                }
                continue;
            }

            // Single-probe: look up the profile reference once and reuse
            // it for both the "has profile?" check, the PUE fallback,
            // and the intensity read, avoiding redundant HashMap probes
            // on the hot path.
            let custom_profile = ctx
                .custom_hourly_profiles
                .as_ref()
                .and_then(|m| m.get(region_key_borrow));

            // Look up annual intensity + PUE. Regions not in the table
            // get (0.0, generic_pue) so their CO₂ from annual intensity
            // is zero but they still produce a breakdown row. When a
            // custom hourly profile exists for an out-of-table region,
            // a generic PUE (1.2) is used so the profile's intensity
            // is not zeroed by pue=0.
            let (annual_intensity, pue) =
                lookup_region_lower(region_key_borrow).unwrap_or_else(|| {
                    let fallback_pue = if custom_profile.is_some() {
                        GENERIC_PUE
                    } else {
                        0.0
                    };
                    (0.0, fallback_pue)
                });

            // Hourly intensity lookup. Consulted when the region has an
            // embedded or custom hourly profile and the span timestamp
            // parses to a valid UTC hour. parse_utc_hour returns None
            // for non-UTC offsets and non-ISO-8601 shapes, in which case
            // we fall back to the flat annual intensity rather than
            // silently using a default hour (which would skew the
            // estimate systematically).
            let embedded_profile = if custom_profile.is_none() {
                hourly_profile_for_region_lower(region_key_borrow)
            } else {
                None
            };
            let region_has_hourly =
                ctx.use_hourly_profiles && (custom_profile.is_some() || embedded_profile.is_some());
            let (hour_opt, month_opt) = if region_has_hourly {
                (
                    crate::time::parse_utc_hour(&span.event.timestamp),
                    crate::time::parse_utc_month(&span.event.timestamp),
                )
            } else {
                (None, None)
            };
            let (intensity_used, span_source) = match hour_opt {
                Some(h) => {
                    // Use the cached profile reference directly instead
                    // of re-probing the HashMap via resolve_hourly_intensity.
                    // NOTE: this logic mirrors resolve_hourly_intensity() in
                    // carbon.rs but uses pre-cached refs to avoid redundant
                    // HashMap probes on the hot path.
                    if let Some(cp) = custom_profile {
                        let val = cp.intensity_at(h, month_opt);
                        let src = if cp.is_monthly() {
                            IntensitySource::MonthlyHourly
                        } else {
                            IntensitySource::Hourly
                        };
                        (val, src)
                    } else if let Some(ep) = embedded_profile {
                        let val = ep.intensity_at(h, month_opt);
                        let src = if ep.is_monthly() {
                            IntensitySource::MonthlyHourly
                        } else {
                            IntensitySource::Hourly
                        };
                        (val, src)
                    } else {
                        // Invariant: region_has_hourly implies custom or
                        // embedded is Some. This branch is unreachable.
                        debug_assert!(false, "region_has_hourly was true but no profile found");
                        (annual_intensity, IntensitySource::Annual)
                    }
                }
                None => (annual_intensity, IntensitySource::Annual),
            };

            // Energy (kWh) per op: measured from the energy snapshot
            // (Scaphandre RAPL or cloud SPECpower) if the snapshot is
            // configured and maps this service, else the proxy constant
            // (optionally weighted by operation type).
            let proxy_energy_kwh = if ctx.per_operation_coefficients {
                ENERGY_PER_IO_OP_KWH * energy_coefficient(&span.event)
            } else {
                ENERGY_PER_IO_OP_KWH
            };
            let (energy_kwh, measured_model) = match &ctx.energy_snapshot {
                Some(snapshot) => match snapshot.get(&span.event.service) {
                    Some(entry) => (entry.energy_per_op_kwh, Some(entry.model_tag)),
                    None => (proxy_energy_kwh, None),
                },
                None => (proxy_energy_kwh, None),
            };

            // Per-op CO₂ via the single-source helper.
            let op_co2 = per_op_gco2(energy_kwh, intensity_used, pue);

            // Obtain or insert the accumulator. Three paths to minimize
            // allocations on the hot per-span loop:
            //   1. region_key is Some  -> already allocated the lowercase
            //      key, use entry() which moves the owned String into the map.
            //   2. region_key is None AND key exists -> single get_mut()
            //      with a borrowed &str, zero allocation.
            //   3. region_key is None AND key absent -> allocate once via
            //      entry(region_ref.to_string()).
            // This avoids allocating a String for every span when the region
            // is already lowercase and present (the common case).
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
                Some(CO2_MODEL_SCAPHANDRE) => acc.any_scaphandre = true,
                Some(CO2_MODEL_CLOUD_SPECPOWER) => acc.any_cloud_specpower = true,
                _ => {}
            }

            // Network transport energy: cross-region HTTP calls only.
            // Reuse `region_ref` (already resolved above) as the caller region
            // to avoid a redundant `resolve_region` call on the hot path.
            if ctx.include_network_transport
                && span.event.event_type == crate::event::EventType::HttpOut
                && let Some(bytes) = span.event.response_size_bytes
            {
                // Probe-before-allocate: only lowercase the hostname when
                // it contains uppercase bytes (same pattern as region keys).
                let callee_region = extract_hostname(&span.event.target)
                    .and_then(|host| {
                        if host.bytes().any(|b| b.is_ascii_uppercase()) {
                            ctx.service_regions.get(&host.to_ascii_lowercase())
                        } else {
                            ctx.service_regions.get(host)
                        }
                    })
                    .map(String::as_str);

                if let Some(callee) = callee_region
                    && !region_ref.eq_ignore_ascii_case(callee)
                {
                    // `bytes` is the response body only (request body is not
                    // available in standard OTel HTTP semantic conventions).
                    // We use the caller's grid intensity and PUE as a proxy
                    // for the network infrastructure's actual grid mix, which
                    // is distributed and unknown. This is a known approximation
                    // documented in LIMITATIONS.md.
                    let transport_energy = bytes as f64 * ctx.network_energy_per_byte_kwh;
                    let transport_co2 = transport_energy * intensity_used * pue;
                    total_transport_gco2 += transport_co2;
                }
            }
        }
    }

    // Build per-region breakdown rows. Regions whose name isn't in the
    // embedded carbon table get a zeroed row + debug log (so users notice
    // the misconfiguration without losing the I/O op count).
    let mut regions: Vec<RegionBreakdown> = Vec::with_capacity(per_region.len() + 1);
    let mut operational_gco2: f64 = 0.0;
    let mut any_hourly_report = false;
    let mut any_monthly_hourly_report = false;
    let mut any_scaphandre_report = false;
    let mut any_cloud_specpower_report = false;
    for (region, acc) in per_region {
        operational_gco2 += acc.co2_gco2;
        match acc.max_intensity_source {
            IntensitySource::MonthlyHourly => {
                any_monthly_hourly_report = true;
                any_hourly_report = true;
            }
            IntensitySource::Hourly => {
                any_hourly_report = true;
            }
            IntensitySource::Annual => {}
        }
        any_scaphandre_report |= acc.any_scaphandre;
        any_cloud_specpower_report |= acc.any_cloud_specpower;

        if let Some((_, pue)) = lookup_region_lower(&region) {
            // Time-weighted mean intensity for display. Guaranteed
            // non-zero ops because the loop above only inserts into
            // the accumulator after incrementing total_ops.
            let mean_intensity = acc.intensity_sum_per_op / acc.total_ops as f64;
            let intensity_source = acc.max_intensity_source;

            // The eu-central-1 hourly profile has a mean ~31% above
            // the flat annual value (442 vs 338 gCO2/kWh). Log once
            // so users notice the divergence when comparing v1/v2/v3.
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

            regions.push(RegionBreakdown {
                status: REGION_STATUS_KNOWN,
                region,
                grid_intensity_gco2_kwh: mean_intensity,
                pue,
                io_ops: acc.total_ops,
                co2_gco2: acc.co2_gco2,
                intensity_source,
            });
        } else {
            // Out-of-table region: name resolved but not in our table.
            // When a custom hourly profile produced non-zero CO2 (via
            // the generic PUE fallback), report the actual accumulated
            // values so sum(regions[].co2_gco2) == operational_gco2.
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
            regions.push(RegionBreakdown {
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
            });
        }
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

    // SCI v1.0 embodied carbon term M = traces × per-trace constant.
    // Region-independent — emitted unconditionally when we have at least
    // one trace (empty case was early-returned above).
    let embodied_gco2 = traces.len() as f64 * ctx.embodied_per_request_gco2;

    // Total = SCI v1.0 numerator (E × I) + M, summed over all analyzed traces.
    // This is NOT the SCI per-R intensity — consumers compute that themselves
    // as total_mid / traces.len() if they need it.
    let total_mid = operational_gco2 + embodied_gco2 + total_transport_gco2;

    // Avoidable CO₂ via region-blind ratio. Denominator excludes the
    // unknown bucket (operational_gco2 already excludes it, so we match).
    // Embodied is NOT included in avoidable — hardware emissions are fixed
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

    // Top-level model tag. Most precise model wins.
    let model = if any_scaphandre_report {
        CO2_MODEL_SCAPHANDRE
    } else if any_cloud_specpower_report {
        CO2_MODEL_CLOUD_SPECPOWER
    } else if any_monthly_hourly_report {
        CO2_MODEL_V3
    } else if any_hourly_report {
        CO2_MODEL_V2
    } else {
        CO2_MODEL
    };

    let transport_gco2 = if total_transport_gco2 > 0.0 {
        Some(total_transport_gco2)
    } else {
        None
    };

    let report = CarbonReport {
        total: CarbonEstimate::sci_numerator_with_model(total_mid, model),
        avoidable: CarbonEstimate::operational_ratio_with_model(avoidable_mid, model),
        operational_gco2,
        embodied_gco2,
        transport_gco2,
    };

    // Sort regions vec by CO₂ descending with alphabetical tiebreak.
    // BTreeMap-based accumulation above gives stable float sums; this final
    // sort is on a Vec and purely cosmetic — determinism is preserved.
    regions.sort_by(|a, b| {
        b.co2_gco2
            .partial_cmp(&a.co2_gco2)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.region.cmp(&b.region))
    });

    (Some(report), regions, multi_region_active)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Confidence, FindingType, Pattern, Severity};
    use crate::event::SpanEvent;
    use crate::test_helpers::{make_http_event, make_sql_event, make_trace};

    #[test]
    fn empty_input_returns_empty_summary() {
        let (findings, summary) = score_green(&[], vec![], None);
        assert!(findings.is_empty());
        assert_eq!(summary.total_io_ops, 0);
        assert_eq!(summary.avoidable_io_ops, 0);
        assert!((summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);
        assert!(summary.top_offenders.is_empty());
    }

    #[test]
    fn single_trace_computes_iis() {
        // 6 SQL events in 1 trace -> IIS = 6/1 = 6.0
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
        };

        let (findings, summary) = score_green(&[trace], vec![finding], None);

        assert_eq!(summary.total_io_ops, 6);
        assert_eq!(summary.avoidable_io_ops, 5);
        assert!((summary.io_waste_ratio - 5.0 / 6.0).abs() < f64::EPSILON);
        assert_eq!(summary.top_offenders.len(), 1);
        assert!((summary.top_offenders[0].io_intensity_score - 6.0).abs() < f64::EPSILON);

        assert_eq!(findings.len(), 1);
        let impact = findings[0].green_impact.as_ref().unwrap();
        assert_eq!(impact.estimated_extra_io_ops, 5);
        assert!((impact.io_intensity_score - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn multiple_traces_same_endpoint() {
        // 2 traces, each with 3 events on the same endpoint -> IIS = 6/2 = 3.0
        let events_t1: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event(
                    "trace-A",
                    &format!("span-a{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let events_t2: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event(
                    "trace-B",
                    &format!("span-b{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {}", i + 10),
                    &format!("2025-07-10T14:32:02.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace_a = make_trace(events_t1);
        let trace_b = make_trace(events_t2);

        let (_, summary) = score_green(&[trace_a, trace_b], vec![], None);
        assert_eq!(summary.total_io_ops, 6);
        assert_eq!(summary.top_offenders.len(), 1);
        assert!((summary.top_offenders[0].io_intensity_score - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn top_offenders_sorted_by_iis_desc() {
        // Endpoint A: 6 events in 1 trace -> IIS = 6.0
        // Endpoint B: 2 events in 1 trace -> IIS = 2.0
        let mut events_a: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-a{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let mut events_b: Vec<SpanEvent> = (1..=2)
            .map(|i| {
                let mut e = make_sql_event(
                    "trace-1",
                    &format!("span-b{i}"),
                    &format!("SELECT * FROM orders WHERE user_id = {i}"),
                    &format!("2025-07-10T14:32:02.{:03}Z", i * 50),
                );
                e.source.endpoint = "GET /api/orders".to_string();
                e
            })
            .collect();

        let mut all_events = Vec::new();
        all_events.append(&mut events_a);
        all_events.append(&mut events_b);
        let trace = make_trace(all_events);

        let (_, summary) = score_green(&[trace], vec![], None);

        assert_eq!(summary.top_offenders.len(), 2);
        assert_eq!(
            summary.top_offenders[0].endpoint,
            "POST /api/orders/42/submit"
        );
        assert_eq!(summary.top_offenders[1].endpoint, "GET /api/orders");
        assert!(
            summary.top_offenders[0].io_intensity_score
                >= summary.top_offenders[1].io_intensity_score
        );
    }

    #[test]
    fn green_impact_populated_on_findings() {
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
        };

        let (findings, _) = score_green(&[trace], vec![finding], None);

        let impact = findings[0].green_impact.as_ref().unwrap();
        assert_eq!(impact.estimated_extra_io_ops, 5);
        assert!((impact.io_intensity_score - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dedup_avoidable_across_finding_types() {
        // Same trace, same template: N+1 (6 occurrences, avoidable=5) + redundant (3 occurrences, avoidable=2)
        // After dedup: max(5, 2) = 5
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let template = "SELECT * FROM order_item WHERE order_id = ?".to_string();
        let findings = vec![
            Finding {
                finding_type: FindingType::NPlusOneSql,
                severity: Severity::Warning,
                trace_id: "trace-1".to_string(),
                service: "order-svc".to_string(),
                source_endpoint: "POST /api/orders/42/submit".to_string(),
                pattern: Pattern {
                    template: template.clone(),
                    occurrences: 6,
                    window_ms: 250,
                    distinct_params: 6,
                },
                suggestion: "batch".to_string(),
                first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
                last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
                green_impact: None,
                confidence: Confidence::default(),
            },
            Finding {
                finding_type: FindingType::RedundantSql,
                severity: Severity::Info,
                trace_id: "trace-1".to_string(),
                service: "order-svc".to_string(),
                source_endpoint: "POST /api/orders/42/submit".to_string(),
                pattern: Pattern {
                    template,
                    occurrences: 3,
                    window_ms: 100,
                    distinct_params: 1,
                },
                suggestion: "cache".to_string(),
                first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
                last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
                green_impact: None,
                confidence: Confidence::default(),
            },
        ];

        let (_, summary) = score_green(&[trace], findings, None);
        // max(5, 2) = 5
        assert_eq!(summary.avoidable_io_ops, 5);
    }

    #[test]
    fn clean_traces_zero_waste() {
        // 4 events, no findings -> waste ratio = 0
        let events = vec![
            make_sql_event(
                "trace-1",
                "span-1",
                "SELECT * FROM users WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
            ),
            make_sql_event(
                "trace-1",
                "span-2",
                "SELECT * FROM orders WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
            ),
            make_http_event(
                "trace-1",
                "span-3",
                "http://svc:5000/api/health",
                "2025-07-10T14:32:01.100Z",
            ),
            make_sql_event(
                "trace-1",
                "span-4",
                "INSERT INTO logs (msg) VALUES ('ok')",
                "2025-07-10T14:32:01.150Z",
            ),
        ];
        let trace = make_trace(events);

        let (findings, summary) = score_green(&[trace], vec![], None);

        assert!(findings.is_empty());
        assert_eq!(summary.total_io_ops, 4);
        assert_eq!(summary.avoidable_io_ops, 0);
        assert!((summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);
        assert_eq!(summary.top_offenders.len(), 1); // 1 endpoint
    }

    /// Build a [`CarbonContext`] with a single default region and zero embodied
    /// term — used by tests that want to verify operational CO₂ in isolation.
    fn ctx_with_region(region: &str) -> CarbonContext {
        // these legacy helper-built contexts disable hourly
        // profiles so existing-era tests keep asserting the
        // v1 model tag. Tests that need the hourly path build their
        // own context inline (see `hourly_profile_flips_model_to_v2`).
        CarbonContext {
            default_region: Some(region.to_string()),
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: false,
            energy_snapshot: None,
            // Disable per-op coefficients so legacy tests asserting exact
            // CO2 values against the flat ENERGY_PER_IO_OP_KWH stay valid.
            per_operation_coefficients: false,
            ..CarbonContext::default()
        }
    }

    #[test]
    fn co2_computed_when_region_set() {
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
        };

        let ctx = ctx_with_region("eu-west-3");
        let (_, summary) = score_green(&[trace], vec![finding], Some(&ctx));

        // Structured CO₂ report present.
        let co2 = summary.co2.as_ref().expect("co2 should be present");
        assert!(co2.total.mid > 0.0);
        assert!(co2.avoidable.mid > 0.0);
        assert_eq!(co2.total.model, "io_proxy_v1");
        // Methodology field replaces sci_version,
        // with distinct values for total (numerator) vs avoidable (ratio).
        assert_eq!(co2.total.methodology, "sci_v1_numerator");
        assert_eq!(co2.avoidable.methodology, "sci_v1_operational_ratio");
        // 2× multiplicative uncertainty bracket.
        assert!((co2.total.low - co2.total.mid * 0.5).abs() < f64::EPSILON);
        assert!((co2.total.high - co2.total.mid * 2.0).abs() < f64::EPSILON);

        // Per-region breakdown contains exactly the configured region.
        assert_eq!(summary.regions.len(), 1);
        assert_eq!(summary.regions[0].region, "eu-west-3");
        assert_eq!(summary.regions[0].io_ops, 6);
        assert!(summary.regions[0].co2_gco2 > 0.0);

        // Top offender still carries the scalar CO₂ for ranking in mono-region mode.
        assert_eq!(summary.top_offenders.len(), 1);
        assert!(summary.top_offenders[0].co2_grams.is_some());
        assert!(summary.top_offenders[0].co2_grams.unwrap() > 0.0);
    }

    #[test]
    fn co2_none_when_no_carbon_context() {
        // When `score_green` is called with `None` (green disabled at the call
        // site, e.g. via `pipeline::analyze` when `green_enabled = false`),
        // no CO₂ data is produced.
        let events: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let (_, summary) = score_green(&[trace], vec![], None);

        assert!(summary.co2.is_none());
        assert!(summary.regions.is_empty());
        for offender in &summary.top_offenders {
            assert!(offender.co2_grams.is_none());
        }
    }

    #[test]
    fn unknown_region_yields_zero_operational_but_keeps_embodied() {
        // behavior: an unknown region (not in the embedded carbon
        // table) bucketed under the configured name produces zero operational
        // CO₂. Embodied carbon is still emitted because it is region-independent.
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT 1",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);

        let ctx = CarbonContext {
            default_region: Some("mars-1".to_string()),
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.001,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));

        let co2 = summary.co2.as_ref().expect("co2 should be present");
        assert!(
            (co2.operational_gco2 - 0.0).abs() < f64::EPSILON,
            "unknown region contributes 0 to operational"
        );
        assert!(
            (co2.embodied_gco2 - 0.001).abs() < f64::EPSILON,
            "1 trace × 0.001 g/req embodied"
        );
        assert!(
            (co2.total.mid - 0.001).abs() < f64::EPSILON,
            "total = operational (0) + embodied (0.001)"
        );
        // The unknown-region row exists in the breakdown with the user's name.
        assert!(summary.regions.iter().any(|r| r.region == "mars-1"));
        let mars = summary
            .regions
            .iter()
            .find(|r| r.region == "mars-1")
            .unwrap();
        assert_eq!(mars.io_ops, 1);
        assert!((mars.co2_gco2 - 0.0).abs() < f64::EPSILON);
        // Top offender CO₂ stays None — the per-offender scalar uses
        // io_ops_to_co2_grams which returns None for unknown regions.
        for offender in &summary.top_offenders {
            assert!(offender.co2_grams.is_none());
        }
    }

    #[test]
    fn slow_findings_do_not_inflate_waste_ratio() {
        // 3 slow SQL events (same template) -> slow_sql finding with 3 occurrences
        // These should NOT count as avoidable I/O.
        use crate::test_helpers::make_sql_event_with_duration;
        let events: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM t WHERE id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                    600_000,
                )
            })
            .collect();
        let trace = make_trace(events);

        let slow_finding = Finding {
            finding_type: FindingType::SlowSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM t WHERE id = ?".to_string(),
                occurrences: 3,
                window_ms: 100,
                distinct_params: 3,
            },
            suggestion: "Consider adding an index".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.150Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
        };

        let (findings, summary) = score_green(&[trace], vec![slow_finding], None);

        // Slow findings should NOT contribute to avoidable ops
        assert_eq!(summary.avoidable_io_ops, 0, "slow ops are not avoidable");
        assert!((summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);

        // green_impact.estimated_extra_io_ops should be 0 for slow findings
        let impact = findings[0].green_impact.as_ref().unwrap();
        assert_eq!(impact.estimated_extra_io_ops, 0);
    }

    #[test]
    fn slow_and_n_plus_one_waste_separate() {
        // Mix: 6 N+1 events + 3 slow events on same trace, different templates
        // Only N+1 should contribute to waste, not slow.
        use crate::test_helpers::make_sql_event_with_duration;
        let mut events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        for i in 7..=9 {
            events.push(make_sql_event_with_duration(
                "trace-1",
                &format!("span-{i}"),
                &format!("SELECT * FROM slow_table WHERE id = {}", i - 6),
                &format!("2025-07-10T14:32:02.{:03}Z", (i - 6) * 50),
                600_000,
            ));
        }
        let trace = make_trace(events);

        let n1_finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
        };
        let slow_finding = Finding {
            finding_type: FindingType::SlowSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM slow_table WHERE id = ?".to_string(),
                occurrences: 3,
                window_ms: 100,
                distinct_params: 3,
            },
            suggestion: "Consider adding an index".to_string(),
            first_timestamp: "2025-07-10T14:32:02.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:02.150Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
        };

        let (findings, summary) = score_green(&[trace], vec![n1_finding, slow_finding], None);

        // Only the N+1 finding's occurrences - 1 = 5 should be avoidable
        assert_eq!(summary.avoidable_io_ops, 5);
        // N+1 finding should have extra_io_ops = 5
        let n1 = findings
            .iter()
            .find(|f| f.finding_type == FindingType::NPlusOneSql)
            .unwrap();
        assert_eq!(n1.green_impact.as_ref().unwrap().estimated_extra_io_ops, 5);
        // Slow finding should have extra_io_ops = 0
        let slow = findings
            .iter()
            .find(|f| f.finding_type == FindingType::SlowSql)
            .unwrap();
        assert_eq!(
            slow.green_impact.as_ref().unwrap().estimated_extra_io_ops,
            0
        );
    }

    // ----- SCI / multi-region / parity tests -----

    /// Build a trace where every span carries the given `cloud_region` attribute,
    /// so per-region bucketing tests don't depend on config defaults.
    fn make_trace_with_region(trace_id: &str, region: &str, count: usize) -> Trace {
        let mut events = Vec::with_capacity(count);
        for i in 1..=count {
            let mut event = make_sql_event(
                trace_id,
                &format!("span-{i}"),
                &format!("SELECT * FROM t WHERE id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
            );
            event.cloud_region = Some(region.to_string());
            events.push(event);
        }
        make_trace(events)
    }

    #[test]
    fn co2_includes_embodied_term() {
        // 6 spans in eu-west-3 (intensity 56 g/kWh, AWS PUE 1.135).
        // disable hourly profiles so the expected_op
        // calculation below (using the flat 56 g/kWh) stays exact.
        // The hourly path is exercised by dedicated tests below.
        let trace = make_trace_with_region("t1", "eu-west-3", 6);
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.001, // 1 trace × 0.001 = 0.001 g embodied
            use_hourly_profiles: false,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();

        // Operational: 6 ops × 1e-7 kWh × 56 × 1.135 = 3.8136e-5 g
        let expected_op = 6.0 * 0.000_000_1 * 56.0 * 1.135;
        assert!((co2.operational_gco2 - expected_op).abs() < 1e-12);
        // Embodied: 1 trace × 0.001 = 0.001
        assert!((co2.embodied_gco2 - 0.001).abs() < f64::EPSILON);
        // Total = operational + embodied
        assert!((co2.total.mid - (expected_op + 0.001)).abs() < 1e-12);
    }

    #[test]
    fn avoidable_excludes_embodied() {
        // 6 spans, 5 marked avoidable via N+1 finding. Avoidable should equal
        // operational × (5/6), with NO embodied term — embodied is fixed and
        // can't be eliminated by fixing query patterns.
        let trace = make_trace_with_region("t1", "eu-west-3", 6);
        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "t1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM t WHERE id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
        };
        // disable hourly profiles so avoidable ratio math
        // stays deterministic (the test compares to operational × 5/6).
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.5, // intentionally large to detect leakage
            use_hourly_profiles: false,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![finding], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();

        // avoidable = operational × (5/6), no embodied.
        let expected_avoidable = co2.operational_gco2 * (5.0 / 6.0);
        assert!((co2.avoidable.mid - expected_avoidable).abs() < 1e-12);
        // Sanity: avoidable strictly less than operational + embodied (= total).
        assert!(co2.avoidable.mid < co2.total.mid);
        // Sanity: avoidable strictly less than operational + 0.5 embodied.
        assert!(co2.avoidable.mid < co2.operational_gco2 + 0.5);
    }

    #[test]
    fn multi_region_bucketing_distinct_per_region() {
        // 3 spans in eu-west-3 + 2 spans in us-east-1 = 2 region buckets.
        // Regions are sorted by co2_gco2 DESC:
        // us-east-1 (2 ops × 379 × 1.135 × 1e-7 ≈ 8.6e-5)
        //  vs eu-west-3 (3 ops × 56 × 1.135 × 1e-7 ≈ 1.9e-5)
        // → us-east-1 appears first despite having fewer ops.
        let trace_eu = make_trace_with_region("t1", "eu-west-3", 3);
        let trace_us = make_trace_with_region("t2", "us-east-1", 2);
        let ctx = ctx_with_region("eu-west-3");
        let (_, summary) = score_green(&[trace_eu, trace_us], vec![], Some(&ctx));

        assert_eq!(summary.regions.len(), 2);
        // CO₂-descending order: us-east-1 (higher total) before eu-west-3.
        assert_eq!(summary.regions[0].region, "us-east-1");
        assert_eq!(summary.regions[0].io_ops, 2);
        assert_eq!(summary.regions[1].region, "eu-west-3");
        assert_eq!(summary.regions[1].io_ops, 3);
        // Per-row CO₂ is descending.
        assert!(summary.regions[0].co2_gco2 > summary.regions[1].co2_gco2);

        // Sum of per-region CO₂ equals operational total.
        let co2 = summary.co2.as_ref().unwrap();
        let sum: f64 = summary.regions.iter().map(|r| r.co2_gco2).sum();
        assert!((sum - co2.operational_gco2).abs() < 1e-12);
    }

    #[test]
    fn region_resolution_chain_priority() {
        // Three spans, three resolution paths:
        // span-1: cloud_region = "ap-south-1" (event attribute)
        // span-2: service "order-svc" → service_regions["order-svc"] = "us-east-1"
        // span-3: no event attr, no service map → default_region = "eu-west-3"
        let mut span1 = make_sql_event("t1", "s1", "SELECT 1", "2025-07-10T14:32:01.001Z");
        span1.cloud_region = Some("ap-south-1".to_string());
        let mut span2 = make_sql_event("t1", "s2", "SELECT 2", "2025-07-10T14:32:01.002Z");
        span2.service = "order-svc".to_string();
        let mut span3 = make_sql_event("t1", "s3", "SELECT 3", "2025-07-10T14:32:01.003Z");
        span3.service = "other-svc".to_string();
        let trace = make_trace(vec![span1, span2, span3]);

        let mut service_regions = HashMap::new();
        service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));

        // Expect three distinct region buckets.
        let region_names: Vec<&str> = summary.regions.iter().map(|r| r.region.as_str()).collect();
        assert!(region_names.contains(&"ap-south-1"));
        assert!(region_names.contains(&"us-east-1"));
        assert!(region_names.contains(&"eu-west-3"));
        // Each bucket has exactly 1 span.
        for region in &summary.regions {
            assert_eq!(region.io_ops, 1, "{}", region.region);
        }
    }

    #[test]
    fn unknown_bucket_for_unresolvable_events() {
        // Span with no cloud_region, no service mapping, no default region.
        let trace = make_trace(vec![make_sql_event(
            "t1",
            "s1",
            "SELECT 1",
            "2025-07-10T14:32:01.001Z",
        )]);
        let ctx = CarbonContext::default(); // no default_region, no service_regions
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));

        // The "unknown" synthetic bucket is present, with the orphan span.
        let unknown = summary
            .regions
            .iter()
            .find(|r| r.region == UNKNOWN_REGION)
            .expect("unknown bucket should exist");
        assert_eq!(unknown.io_ops, 1);
        assert!((unknown.co2_gco2 - 0.0).abs() < f64::EPSILON);

        // Embodied still emitted (region-independent).
        let co2 = summary.co2.as_ref().unwrap();
        assert!((co2.operational_gco2 - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn regions_sorted_by_co2_desc() {
        // The regions breakdown is sorted by co2_gco2
        // descending (with alphabetical tiebreak) for actionability —
        // users see the highest-impact regions first. BTreeMap accumulation
        // keeps the per-region float sums deterministic; the final Vec sort
        // is purely cosmetic ordering.
        //
        // 1 op per region, intensity values from CARBON_TABLE:
        //   ap-south-1: 708 gCO₂/kWh × 1.135 PUE ≈ 803 → 8.04e-5 g
        //   us-east-1:  379 gCO₂/kWh × 1.135 PUE ≈ 430 → 4.30e-5 g
        //   eu-west-3:   56 gCO₂/kWh × 1.135 PUE ≈ 63.6 → 6.36e-6 g
        // Expected DESC order: ap-south-1 → us-east-1 → eu-west-3.
        let trace_us = make_trace_with_region("t1", "us-east-1", 1);
        let trace_eu = make_trace_with_region("t2", "eu-west-3", 1);
        let trace_ap = make_trace_with_region("t3", "ap-south-1", 1);
        let ctx = ctx_with_region("eu-west-3");
        // Pass traces in non-sorted order on purpose.
        let (_, summary) = score_green(&[trace_us, trace_eu, trace_ap], vec![], Some(&ctx));

        let names: Vec<&str> = summary.regions.iter().map(|r| r.region.as_str()).collect();
        assert_eq!(names, vec!["ap-south-1", "us-east-1", "eu-west-3"]);
        // CO₂ values strictly descending.
        assert!(summary.regions[0].co2_gco2 > summary.regions[1].co2_gco2);
        assert!(summary.regions[1].co2_gco2 > summary.regions[2].co2_gco2);
    }

    #[test]
    fn regions_output_deterministic_under_permutation() {
        // Explicitly verify that feeding the
        // same logical workload in two different input orders produces
        // identical `regions` output. BTreeMap accumulation + the final
        // CO₂-DESC sort jointly guarantee this.
        let ctx = ctx_with_region("eu-west-3");

        let order_a = vec![
            make_trace_with_region("t1", "us-east-1", 2),
            make_trace_with_region("t2", "eu-west-3", 3),
            make_trace_with_region("t3", "ap-south-1", 1),
        ];
        let order_b = vec![
            make_trace_with_region("t3", "ap-south-1", 1),
            make_trace_with_region("t1", "us-east-1", 2),
            make_trace_with_region("t2", "eu-west-3", 3),
        ];

        let (_, sa) = score_green(&order_a, vec![], Some(&ctx));
        let (_, sb) = score_green(&order_b, vec![], Some(&ctx));
        assert_eq!(sa.regions, sb.regions);
        assert_eq!(
            sa.co2.as_ref().map(|c| c.operational_gco2),
            sb.co2.as_ref().map(|c| c.operational_gco2)
        );
    }

    #[test]
    fn confidence_interval_factors_match_constants() {
        let trace = make_trace_with_region("t1", "eu-west-3", 100);
        let ctx = ctx_with_region("eu-west-3");
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();

        // Total
        assert!((co2.total.low - co2.total.mid * 0.5).abs() < f64::EPSILON);
        assert!((co2.total.high - co2.total.mid * 2.0).abs() < f64::EPSILON);
        // Avoidable (with no findings, mid is 0 → low and high are 0 too)
        assert!((co2.avoidable.low - co2.avoidable.mid * 0.5).abs() < f64::EPSILON);
        assert!((co2.avoidable.high - co2.avoidable.mid * 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn co2_methodology_labels_set() {
        // `total` is tagged as the SCI numerator,
        // `avoidable` is tagged as the region-blind operational ratio.
        // The two distinct methodology strings signal the semantic
        // difference to downstream consumers at the data layer.
        let trace = make_trace_with_region("t1", "eu-west-3", 1);
        let ctx = ctx_with_region("eu-west-3");
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "io_proxy_v1");
        assert_eq!(co2.total.methodology, "sci_v1_numerator");
        assert_eq!(co2.avoidable.model, "io_proxy_v1");
        assert_eq!(co2.avoidable.methodology, "sci_v1_operational_ratio");
    }

    #[test]
    fn cloud_region_attribute_beats_service_mapping() {
        // Even when [green.service_regions] maps the service, an explicit
        // cloud.region on the span itself should win (most authoritative).
        let mut event = make_sql_event("t1", "s1", "SELECT 1", "2025-07-10T14:32:01.001Z");
        event.service = "order-svc".to_string();
        event.cloud_region = Some("ap-south-1".to_string());
        let trace = make_trace(vec![event]);

        let mut service_regions = HashMap::new();
        service_regions.insert("order-svc".to_string(), "eu-west-3".to_string());
        let ctx = CarbonContext {
            default_region: Some("us-east-1".to_string()),
            service_regions,
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));

        // Only one region: ap-south-1 (the span attribute wins).
        assert_eq!(summary.regions.len(), 1);
        assert_eq!(summary.regions[0].region, "ap-south-1");
        assert_eq!(summary.regions[0].io_ops, 1);
    }

    // ----- Multi-region guard, cap, denominator -----

    #[test]
    fn top_offender_co2_some_in_single_region_mode() {
        // Baseline: with only default_region set (no service_regions, no
        // cloud.region on spans), top offenders carry a scalar co2_grams.
        let trace = make_trace_with_region_no_cloud("t1", 6);
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        assert!(!summary.top_offenders.is_empty());
        assert!(
            summary.top_offenders[0].co2_grams.is_some(),
            "single-region mode should populate TopOffender.co2_grams"
        );
    }

    #[test]
    fn top_offender_co2_none_when_multi_region_via_service_regions() {
        // When [green.service_regions] is non-empty, multi-region is
        // active → TopOffender.co2_grams must be None (the scalar would be
        // inconsistent with the per-region breakdown).
        let trace = make_trace_with_region_no_cloud("t1", 6);
        let mut service_regions = HashMap::new();
        service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        assert!(!summary.top_offenders.is_empty());
        for offender in &summary.top_offenders {
            assert!(
                offender.co2_grams.is_none(),
                "multi-region via service_regions should null TopOffender.co2_grams, got {:?}",
                offender.co2_grams
            );
        }
    }

    #[test]
    fn top_offender_co2_none_when_multi_region_via_span_attribute() {
        // When any span carries cloud.region, multi-region is active.
        let trace = make_trace_with_region("t1", "ap-south-1", 6);
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        assert!(!summary.top_offenders.is_empty());
        for offender in &summary.top_offenders {
            assert!(
                offender.co2_grams.is_none(),
                "multi-region via cloud.region attribute should null TopOffender.co2_grams"
            );
        }
    }

    #[test]
    fn region_cardinality_cap_folds_overflow_into_unknown() {
        // Cap at 256 distinct regions. Feed 260 distinct region tags;
        // expect at most 256 in the breakdown + an "unknown" row with the
        // overflow count.
        let mut events = Vec::with_capacity(260);
        for i in 0..260 {
            let mut event = make_sql_event(
                "t1",
                &format!("span-{i}"),
                "SELECT 1",
                "2025-07-10T14:32:01.001Z",
            );
            // Use out-of-table region names so the per-region CO₂ is 0
            // but the bucket still counts. Keeps the test focused on the cap.
            event.cloud_region = Some(format!("test-region-{i:04}"));
            events.push(event);
        }
        let trace = make_trace(events);
        let ctx = CarbonContext::default();
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));

        // Tighten the assertions. With 260 distinct region names fed
        // in insertion order and MAX_REGIONS = 256, exactly 256 known rows
        // should be bucketed + exactly 4 ops folded into the unknown bucket.
        let non_unknown_rows: Vec<&RegionBreakdown> = summary
            .regions
            .iter()
            .filter(|r| r.region != UNKNOWN_REGION)
            .collect();
        assert_eq!(
            non_unknown_rows.len(),
            256,
            "cap should produce exactly 256 known rows, got {}",
            non_unknown_rows.len()
        );
        // All of those rows are out_of_table (not in the carbon table).
        for row in &non_unknown_rows {
            assert_eq!(row.status, "out_of_table");
        }

        // The unknown bucket exists with exactly the overflow count.
        let unknown = summary
            .regions
            .iter()
            .find(|r| r.region == UNKNOWN_REGION)
            .expect("unknown bucket should exist when cap is exceeded");
        assert_eq!(
            unknown.io_ops, 4,
            "exactly 260 - 256 = 4 ops should land in unknown"
        );
        assert_eq!(unknown.status, "unresolved");

        // Conservation: sum of all region ops = 260.
        let total_bucketed: usize = summary.regions.iter().map(|r| r.io_ops).sum();
        assert_eq!(total_bucketed, 260);
    }

    #[test]
    fn avoidable_ratio_excludes_unknown_bucket_from_denominator() {
        // When the unknown bucket has io_ops, the avoidable ratio
        // uses (total_io_ops - unknown_ops) as the denominator, so the
        // numerator and denominator are consistent (both exclude unknown).
        //
        // Setup: 10 ops in eu-west-3 + 5 ops with no resolvable region,
        // plus an N+1 finding flagging 3 avoidable ops on the eu-west-3 trace.
        use crate::test_helpers::make_sql_event as helper;
        let mut events_eu = Vec::new();
        for i in 1..=10 {
            let mut e = helper(
                "trace-eu",
                &format!("s{i}"),
                &format!("SELECT * FROM t WHERE id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", i * 20),
            );
            e.cloud_region = Some("eu-west-3".to_string());
            events_eu.push(e);
        }
        let trace_eu = make_trace(events_eu);

        let mut events_orphan = Vec::new();
        for i in 1..=5 {
            // No cloud_region, service doesn't match any service_regions,
            // no default_region → lands in unknown_ops bucket.
            events_orphan.push(helper(
                "trace-orphan",
                &format!("o{i}"),
                "SELECT 1",
                "2025-07-10T14:32:02.000Z",
            ));
        }
        let trace_orphan = make_trace(events_orphan);

        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-eu".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM t WHERE id = ?".to_string(),
                occurrences: 4, // 4 occurrences → 3 avoidable
                window_ms: 250,
                distinct_params: 4,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
        };

        let ctx = CarbonContext {
            default_region: None, // no fallback → orphan trace goes to unknown
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace_eu, trace_orphan], vec![finding], Some(&ctx));

        assert_eq!(summary.total_io_ops, 15);
        assert_eq!(summary.avoidable_io_ops, 3);

        let co2 = summary.co2.as_ref().unwrap();
        // Denominator is 15 - 5 = 10 (excludes orphan bucket), not 15.
        // avoidable.mid = operational × (3/10), NOT operational × (3/15).
        let expected = co2.operational_gco2 * (3.0 / 10.0);
        assert!(
            (co2.avoidable.mid - expected).abs() < 1e-12,
            "avoidable.mid = {} vs expected {} (denominator 10)",
            co2.avoidable.mid,
            expected
        );
    }

    /// Helper: a trace where each span carries NO `cloud_region` attribute.
    /// Used by tests verifying single-region-mode behavior vs multi-region.
    fn make_trace_with_region_no_cloud(trace_id: &str, count: usize) -> Trace {
        let mut events = Vec::with_capacity(count);
        for i in 1..=count {
            events.push(make_sql_event(
                trace_id,
                &format!("span-{i}"),
                &format!("SELECT * FROM t WHERE id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
            ));
        }
        make_trace(events)
    }

    // ----- Out-of-table and unknown region tests -----

    #[test]
    fn empty_traces_with_carbon_context_returns_no_co2() {
        // Explicit test for the early-return branch inside
        // compute_carbon_report. Previous coverage only hit the outer
        // `None` arm via `score_green(..., None)`.
        let ctx = ctx_with_region("eu-west-3");
        let (_, summary) = score_green(&[], vec![], Some(&ctx));
        assert!(
            summary.co2.is_none(),
            "empty traces must not emit an all-zeros co2 object"
        );
        assert!(summary.regions.is_empty());
    }

    #[test]
    fn region_breakdown_distinguishes_out_of_table_from_unresolved() {
        // `mars-1` resolves (via default_region) but isn't in the carbon
        // table → status "out_of_table". A second span with no resolvable
        // region → status "unresolved" in the "unknown" bucket.
        let mut span_mars = make_sql_event("t1", "s1", "SELECT 1", "2025-07-10T14:32:01.001Z");
        span_mars.cloud_region = Some("mars-1".to_string());
        let span_orphan = make_sql_event("t1", "s2", "SELECT 2", "2025-07-10T14:32:01.002Z");
        let trace = make_trace(vec![span_mars, span_orphan]);

        // No default_region, no service_regions: the orphan span has no way
        // to resolve. The mars-1 span resolves via its own cloud_region attr.
        let ctx = CarbonContext::default();
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));

        let mars = summary
            .regions
            .iter()
            .find(|r| r.region == "mars-1")
            .expect("mars-1 row should exist");
        assert_eq!(mars.status, "out_of_table");
        assert!((mars.co2_gco2 - 0.0).abs() < f64::EPSILON);

        let unknown = summary
            .regions
            .iter()
            .find(|r| r.region == "unknown")
            .expect("unknown row should exist");
        assert_eq!(unknown.status, "unresolved");
        assert!((unknown.co2_gco2 - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn region_breakdown_status_known_for_in_table_region() {
        // Baseline: eu-west-3 is in the carbon table, status = known.
        let trace = make_trace_with_region("t1", "eu-west-3", 3);
        let ctx = ctx_with_region("eu-west-3");
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));

        assert_eq!(summary.regions.len(), 1);
        assert_eq!(summary.regions[0].status, "known");
        assert!(summary.regions[0].co2_gco2 > 0.0);
    }

    // --- hourly profiles and Scaphandre snapshot integration ---

    /// Build 6 spans at the same UTC hour in the given region.
    /// The template, trace id and endpoint are the same so one N+1 finding
    /// can be attached; differing `order_id` values give 6 distinct params.
    fn make_trace_at_hour(trace_id: &str, region: &str, hour: u8, count: usize) -> Trace {
        let mut events = Vec::with_capacity(count);
        for i in 1..=count {
            let mut event = make_sql_event(
                trace_id,
                &format!("span-{i}"),
                &format!("SELECT * FROM t WHERE id = {i}"),
                &format!("2025-07-10T{hour:02}:00:00.{i:03}Z"),
            );
            event.cloud_region = Some(region.to_string());
            events.push(event);
        }
        make_trace(events)
    }

    fn ctx_hourly(use_hourly: bool) -> CarbonContext {
        CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: use_hourly,
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        }
    }

    #[test]
    fn hourly_profile_different_co2_night_vs_evening_fr() {
        // eu-west-3 at 03:00 UTC (night, lower nuclear + no evening demand)
        // must produce strictly less CO₂ than the same 6 ops at 19:00 UTC
        // (evening peak).
        let trace_night = make_trace_at_hour("t_night", "eu-west-3", 3, 6);
        let trace_evening = make_trace_at_hour("t_evening", "eu-west-3", 19, 6);

        let ctx = ctx_hourly(true);
        let (_, night) = score_green(&[trace_night], vec![], Some(&ctx));
        let (_, evening) = score_green(&[trace_evening], vec![], Some(&ctx));

        let co2_night = night.co2.as_ref().unwrap().operational_gco2;
        let co2_evening = evening.co2.as_ref().unwrap().operational_gco2;
        assert!(
            co2_night < co2_evening,
            "night ({co2_night}) should be less than evening ({co2_evening}) in eu-west-3"
        );
    }

    #[test]
    fn hourly_profile_flips_model_to_v3_for_monthly_region() {
        // A report using a monthly hourly profile (eu-west-3) must tag
        // model = io_proxy_v3 and intensity_source = MonthlyHourly.
        let trace = make_trace_at_hour("t1", "eu-west-3", 14, 6);
        let ctx = ctx_hourly(true);
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "io_proxy_v3");
        assert_eq!(co2.avoidable.model, "io_proxy_v3");
        // Per-region breakdown tag matches.
        assert_eq!(summary.regions.len(), 1);
        assert_eq!(
            summary.regions[0].intensity_source,
            IntensitySource::MonthlyHourly
        );
    }

    #[test]
    fn hourly_profile_disabled_stays_on_v1() {
        // use_hourly_profiles = false → never flip to v2 even for
        // regions with hourly data.
        let trace = make_trace_at_hour("t1", "eu-west-3", 14, 6);
        let ctx = ctx_hourly(false);
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "io_proxy_v1");
        assert_eq!(summary.regions[0].intensity_source, IntensitySource::Annual);
    }

    #[test]
    fn hourly_profile_fallback_to_annual_for_region_without_profile() {
        // us-central1 (GCP) is in CARBON_TABLE but has no hourly profile.
        // Even with use_hourly_profiles = true, the report should use
        // the flat annual path and tag model = io_proxy_v1.
        let trace = make_trace_at_hour("t1", "us-central1", 10, 6);
        let ctx = ctx_hourly(true);
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "io_proxy_v1");
        assert_eq!(summary.regions[0].intensity_source, IntensitySource::Annual);
    }

    #[test]
    fn de_flat_annual_numerical_regression() {
        // Regression guard for eu-central-1 (Germany): this profile's
        // hourly mean diverges materially from the flat annual value in
        // CARBON_TABLE (~442 vs 338 g/kWh), so a future edit that
        // accidentally couples the flat path to hourly data would
        // produce wrong numbers here. Pin the flat-annual model
        // explicitly and assert the closed-form formula.
        //
        // Formula: 6 ops × ENERGY_PER_IO_OP_KWH × 338 × 1.135
        //        = 6 × 1e-7 × 338 × 1.135
        //        = 2.30178e-4
        //
        // NOTE: if CARBON_TABLE[eu-central-1] is ever recalibrated, this
        // test will fail loudly — that's the point. Update both the
        // constant here and the hourly profile invariant comment in
        // carbon.rs at the same time.
        let trace = make_trace_at_hour("t_de", "eu-central-1", 12, 6);
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: false, // pin to flat annual
            energy_snapshot: None,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        // Model tag must stay v1 when hourly is disabled, even for a
        // region that has a hourly profile.
        assert_eq!(co2.total.model, "io_proxy_v1");
        // Exact CO₂ from the flat annual intensity (338) and AWS PUE (1.135).
        let expected = 6.0 * 1e-7 * 338.0 * 1.135;
        assert!(
            (co2.operational_gco2 - expected).abs() < 1e-12,
            "DE flat-annual math drifted: expected {expected}, got {}",
            co2.operational_gco2
        );
        // Per-region breakdown row should report the annual intensity
        // directly (not a time-weighted mean) because hourly is disabled.
        assert_eq!(summary.regions.len(), 1);
        assert_eq!(summary.regions[0].region, "eu-central-1");
        assert_eq!(summary.regions[0].intensity_source, IntensitySource::Annual);
        assert!((summary.regions[0].grid_intensity_gco2_kwh - 338.0).abs() < f64::EPSILON);
    }

    #[test]
    fn mixed_report_monthly_hourly_and_annual_tags_v3_per_row() {
        // Two traces: one in eu-west-3 (Monthly profile) and one in
        // us-central1 (no hourly profile). Top-level model should be v3
        // because at least one region used monthly hourly. Per-region
        // breakdown should show the correct intensity_source.
        let trace_eu = make_trace_at_hour("t_eu", "eu-west-3", 12, 3);
        let trace_us = make_trace_at_hour("t_us", "us-central1", 12, 3);
        let ctx = ctx_hourly(true);
        let (_, summary) = score_green(&[trace_eu, trace_us], vec![], Some(&ctx));

        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(
            co2.total.model, "io_proxy_v3",
            "mixed report with any monthly hourly region tags model = v3"
        );

        let eu_row = summary
            .regions
            .iter()
            .find(|r| r.region == "eu-west-3")
            .expect("eu-west-3 row");
        assert_eq!(eu_row.intensity_source, IntensitySource::MonthlyHourly);

        let us_row = summary
            .regions
            .iter()
            .find(|r| r.region == "us-central1")
            .expect("us-central1 row");
        assert_eq!(us_row.intensity_source, IntensitySource::Annual);
    }

    #[test]
    fn mixed_report_flat_hourly_and_annual_tags_v2() {
        // eu-west-1 (FlatYear hourly) + us-central1 (no hourly).
        // Top-level model = v2 (not v3, because eu-west-1 is FlatYear).
        let trace_ie = make_trace_at_hour("t_ie", "eu-west-1", 12, 3);
        let trace_us = make_trace_at_hour("t_us", "us-central1", 12, 3);
        let ctx = ctx_hourly(true);
        let (_, summary) = score_green(&[trace_ie, trace_us], vec![], Some(&ctx));

        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "io_proxy_v2");

        let ie_row = summary
            .regions
            .iter()
            .find(|r| r.region == "eu-west-1")
            .expect("eu-west-1 row");
        assert_eq!(ie_row.intensity_source, IntensitySource::Hourly);
    }

    #[test]
    fn hourly_row_intensity_is_time_weighted_mean() {
        // All 6 ops at the same hour (03 UTC) and month (July, month 6,
        // from timestamp "2025-07-10T03:00:...Z"). The weighted mean
        // should equal the single hourly value used.
        let trace = make_trace_at_hour("t1", "eu-west-3", 3, 6);
        let ctx = ctx_hourly(true);
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let row = &summary.regions[0];
        // eu-west-3, July (month 6), hour 3 UTC = 38.0 g/kWh per the
        // Monthly profile in carbon_profiles.rs.
        let expected = crate::score::carbon::resolve_hourly_intensity(
            "eu-west-3",
            3,
            Some(6), // July
            None,
        )
        .unwrap()
        .0;
        assert!(
            (row.grid_intensity_gco2_kwh - expected).abs() < f64::EPSILON,
            "expected {expected} g/kWh at hour 3 UTC July, got {}",
            row.grid_intensity_gco2_kwh
        );
    }

    #[test]
    fn custom_profile_overrides_embedded_in_scoring_loop() {
        // Provide a custom FlatYear profile for eu-west-3 with a constant
        // 999.0 intensity. The scoring loop should use 999.0 instead of
        // the embedded Monthly profile values.
        let trace = make_trace_at_hour("t1", "eu-west-3", 12, 6);
        let mut custom = HashMap::new();
        custom.insert(
            "eu-west-3".to_string(),
            carbon::HourlyProfile::FlatYear([999.0; 24]),
        );
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: false,
            custom_hourly_profiles: Some(std::sync::Arc::new(custom)),
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let row = &summary.regions[0];
        // Custom FlatYear profile: all hours are 999.0.
        assert!(
            (row.grid_intensity_gco2_kwh - 999.0).abs() < f64::EPSILON,
            "expected custom intensity 999.0, got {}",
            row.grid_intensity_gco2_kwh
        );
        // Custom FlatYear profile => IntensitySource::Hourly (not MonthlyHourly).
        assert_eq!(row.intensity_source, IntensitySource::Hourly);
        // Model tag: FlatYear custom => io_proxy_v2.
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "io_proxy_v2");
    }

    #[test]
    fn custom_profile_on_out_of_table_region_uses_generic_pue() {
        // "my-datacenter" is not in CARBON_TABLE. A custom profile should
        // still produce non-zero CO2 via the generic PUE fallback (1.2).
        let trace = make_trace_at_hour("t1", "my-datacenter", 12, 6);
        let mut custom = HashMap::new();
        custom.insert(
            "my-datacenter".to_string(),
            carbon::HourlyProfile::FlatYear([500.0; 24]),
        );
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: false,
            custom_hourly_profiles: Some(std::sync::Arc::new(custom)),
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        // 6 ops * 1e-7 kWh/op * 500 gCO2/kWh * 1.2 PUE = 3.6e-4 gCO2
        let expected = 6.0 * 1e-7 * 500.0 * 1.2;
        assert!(
            (co2.operational_gco2 - expected).abs() < 1e-12,
            "expected {expected}, got {}",
            co2.operational_gco2
        );
        assert!(
            co2.operational_gco2 > 0.0,
            "custom profile on out-of-table region must produce non-zero CO2"
        );
        // Verify breakdown row reflects actual CO2, not zeros.
        assert_eq!(summary.regions.len(), 1);
        let row = &summary.regions[0];
        assert_eq!(row.region, "my-datacenter");
        assert!(
            (row.co2_gco2 - expected).abs() < 1e-12,
            "breakdown row co2 must match accumulated value"
        );
        assert!(
            (row.pue - 1.2).abs() < f64::EPSILON,
            "out-of-table region with custom profile should use generic PUE 1.2"
        );
        assert!(
            (row.grid_intensity_gco2_kwh - 500.0).abs() < f64::EPSILON,
            "breakdown row should report the custom profile intensity"
        );
        assert_eq!(row.intensity_source, IntensitySource::Hourly);
        // Conservation: sum of breakdown rows == operational total.
        let sum: f64 = summary.regions.iter().map(|r| r.co2_gco2).sum();
        assert!(
            (sum - co2.operational_gco2).abs() < 1e-12,
            "breakdown sum ({sum}) must equal operational_gco2 ({})",
            co2.operational_gco2
        );
    }

    #[test]
    fn scaphandre_snapshot_flips_model_and_replaces_coefficient() {
        // Service "order-svc" (make_sql_event default) in eu-west-3,
        // Scaphandre snapshot maps it to 5e-7 kWh/op (5× the proxy).
        // The report should:
        // 1. Tag the top-level model as scaphandre_rapl
        // 2. Compute operational CO₂ with the measured coefficient
        let trace = make_trace_at_hour("t1", "eu-west-3", 12, 6);
        let mut snapshot = HashMap::new();
        snapshot.insert(
            "order-svc".to_string(),
            carbon::EnergyEntry::scaphandre(5e-7),
        );
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: false, // isolate Scaphandre effect
            energy_snapshot: Some(snapshot),
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        // Top-level model is scaphandre_rapl (takes precedence over v1/v2).
        assert_eq!(co2.total.model, "scaphandre_rapl");
        // Operational CO₂ = 6 ops × 5e-7 kWh × 56 g/kWh × 1.135 PUE.
        let expected = 6.0 * 5e-7 * 56.0 * 1.135;
        assert!(
            (co2.operational_gco2 - expected).abs() < 1e-12,
            "expected {expected}, got {}",
            co2.operational_gco2
        );
    }

    #[test]
    fn scaphandre_empty_snapshot_stays_on_proxy() {
        // When the snapshot is Some but EMPTY (no services mapped),
        // every op falls back to the proxy and the model tag stays v1.
        let trace = make_trace_at_hour("t1", "eu-west-3", 12, 6);
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: false,
            energy_snapshot: Some(HashMap::new()),
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "io_proxy_v1");
    }

    #[test]
    fn scaphandre_takes_precedence_over_hourly_in_model_tag() {
        // Both hourly AND scaphandre active -> scaphandre_rapl wins.
        let trace = make_trace_at_hour("t1", "eu-west-3", 3, 6);
        let mut snapshot = HashMap::new();
        snapshot.insert(
            "order-svc".to_string(),
            carbon::EnergyEntry::scaphandre(3e-7),
        );
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: Some(snapshot),
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "scaphandre_rapl");
        // The region row still reports intensity_source = MonthlyHourly
        // because the monthly hourly path was used for the intensity
        // even though Scaphandre supplied the energy coefficient.
        assert_eq!(
            summary.regions[0].intensity_source,
            IntensitySource::MonthlyHourly
        );
    }

    // ------------------------------------------------------------------
    // Cloud SPECpower model tag tests
    // ------------------------------------------------------------------

    #[test]
    fn cloud_snapshot_flips_model_to_cloud_specpower() {
        let trace = make_trace_at_hour("t1", "eu-west-3", 12, 6);
        let mut snapshot = HashMap::new();
        snapshot.insert("order-svc".to_string(), carbon::EnergyEntry::cloud(5e-7));
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: false,
            energy_snapshot: Some(snapshot),
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "cloud_specpower");
        // Operational CO2 = 6 ops * 5e-7 kWh * 56 g/kWh * 1.135 PUE.
        let expected = 6.0 * 5e-7 * 56.0 * 1.135;
        assert!(
            (co2.operational_gco2 - expected).abs() < 1e-12,
            "expected {expected}, got {}",
            co2.operational_gco2
        );
    }

    #[test]
    fn cloud_takes_precedence_over_hourly_in_model_tag() {
        let trace = make_trace_at_hour("t1", "eu-west-3", 3, 6);
        let mut snapshot = HashMap::new();
        snapshot.insert("order-svc".to_string(), carbon::EnergyEntry::cloud(3e-7));
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: Some(snapshot),
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "cloud_specpower");
    }

    #[test]
    fn scaphandre_takes_precedence_over_cloud_in_model_tag() {
        // Mixed snapshot: one service with Scaphandre, another with cloud.
        // Scaphandre should win for the top-level model tag.
        let trace = make_trace_at_hour("t1", "eu-west-3", 12, 6);
        let mut snapshot = HashMap::new();
        snapshot.insert(
            "order-svc".to_string(),
            carbon::EnergyEntry::scaphandre(5e-7),
        );
        snapshot.insert("other-svc".to_string(), carbon::EnergyEntry::cloud(3e-7));
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: false,
            energy_snapshot: Some(snapshot),
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        // "order-svc" (the service in the trace) has Scaphandre entry.
        assert_eq!(co2.total.model, "scaphandre_rapl");
    }

    #[test]
    fn cloud_empty_snapshot_stays_on_proxy() {
        let trace = make_trace_at_hour("t1", "eu-west-3", 12, 6);
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: false,
            energy_snapshot: Some(HashMap::new()),
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();
        assert_eq!(co2.total.model, "io_proxy_v1");
    }

    // ── Per-operation coefficient integration tests ────────────────

    #[test]
    fn per_op_coefficients_select_lower_than_insert() {
        // SELECT uses 0.5x, INSERT uses 1.5x. An INSERT-heavy trace
        // should produce higher CO2 than a SELECT-heavy trace.
        let select_events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "t1",
                    &format!("s{i}"),
                    &format!("SELECT * FROM users WHERE id = {i}"),
                    &format!("2025-07-10T14:32:01.{i:03}Z"),
                )
            })
            .collect();
        let insert_events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "t2",
                    &format!("s{i}"),
                    &format!("INSERT INTO users (name) VALUES ('user{i}')"),
                    &format!("2025-07-10T14:32:01.{i:03}Z"),
                )
            })
            .collect();
        let trace_select = make_trace(select_events);
        let trace_insert = make_trace(insert_events);

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            use_hourly_profiles: false,
            per_operation_coefficients: true,
            ..CarbonContext::default()
        };

        let (_, summary_select) = score_green(&[trace_select], vec![], Some(&ctx));
        let (_, summary_insert) = score_green(&[trace_insert], vec![], Some(&ctx));

        let co2_select = summary_select.co2.as_ref().unwrap().operational_gco2;
        let co2_insert = summary_insert.co2.as_ref().unwrap().operational_gco2;

        assert!(
            co2_insert > co2_select,
            "INSERT CO2 ({co2_insert}) should be > SELECT CO2 ({co2_select})"
        );
        // Ratio should be 1.5 / 0.5 = 3.0
        let ratio = co2_insert / co2_select;
        assert!(
            (ratio - 3.0).abs() < 1e-6,
            "INSERT/SELECT ratio should be 3.0, got {ratio}"
        );
    }

    #[test]
    fn per_op_coefficients_disabled_uses_flat() {
        // With per_operation_coefficients=false, SELECT and INSERT
        // should produce the same CO2 (both use flat ENERGY_PER_IO_OP_KWH).
        let select_events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "t1",
                    &format!("s{i}"),
                    &format!("SELECT * FROM users WHERE id = {i}"),
                    &format!("2025-07-10T14:32:01.{i:03}Z"),
                )
            })
            .collect();
        let insert_events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "t2",
                    &format!("s{i}"),
                    &format!("INSERT INTO users (name) VALUES ('user{i}')"),
                    &format!("2025-07-10T14:32:01.{i:03}Z"),
                )
            })
            .collect();
        let trace_select = make_trace(select_events);
        let trace_insert = make_trace(insert_events);

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            use_hourly_profiles: false,
            per_operation_coefficients: false,
            ..CarbonContext::default()
        };

        let (_, summary_select) = score_green(&[trace_select], vec![], Some(&ctx));
        let (_, summary_insert) = score_green(&[trace_insert], vec![], Some(&ctx));

        let co2_select = summary_select.co2.as_ref().unwrap().operational_gco2;
        let co2_insert = summary_insert.co2.as_ref().unwrap().operational_gco2;

        assert!(
            (co2_insert - co2_select).abs() < 1e-15,
            "with per_op disabled, SELECT ({co2_select}) and INSERT ({co2_insert}) should match"
        );
    }

    #[test]
    fn per_op_coefficients_measured_energy_ignores_coefficient() {
        // Scaphandre snapshot overrides the per-op coefficient.
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "t1",
                    &format!("s{i}"),
                    &format!("SELECT * FROM users WHERE id = {i}"),
                    &format!("2025-07-10T14:32:01.{i:03}Z"),
                )
            })
            .collect();
        let trace = make_trace(events);

        let measured_energy = 5e-7;
        let mut snapshot = HashMap::new();
        snapshot.insert(
            "order-svc".to_string(),
            carbon::EnergyEntry::scaphandre(measured_energy),
        );

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            use_hourly_profiles: false,
            per_operation_coefficients: true,
            energy_snapshot: Some(snapshot),
            ..CarbonContext::default()
        };

        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let co2 = summary.co2.as_ref().unwrap();

        // With Scaphandre, energy is measured_energy, not ENERGY_PER_IO_OP_KWH * coeff.
        // 6 ops × 5e-7 kWh × 56 g/kWh × 1.135 PUE
        let expected = 6.0 * measured_energy * 56.0 * 1.135;
        assert!(
            (co2.operational_gco2 - expected).abs() < 1e-12,
            "expected {expected}, got {}",
            co2.operational_gco2
        );
    }

    // ── Transport CO2 integration tests ────────────────────────────

    #[test]
    fn transport_co2_cross_region_http() {
        use crate::test_helpers::make_http_event_with_size;
        // HTTP call from eu-west-3 to a host mapped to us-east-1.
        let mut event = make_http_event_with_size(
            "t1",
            "s1",
            "http://order-api:8080/api/orders",
            "2025-07-10T14:32:01.000Z",
            Some(100_000), // 100 KB
        );
        event.cloud_region = Some("eu-west-3".to_string());
        let trace = make_trace(vec![event]);

        let mut service_regions = HashMap::new();
        service_regions.insert("order-api".to_string(), "us-east-1".to_string());

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            use_hourly_profiles: false,
            per_operation_coefficients: false,
            include_network_transport: true,
            ..CarbonContext::default()
        };

        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        assert!(
            summary.transport_gco2.is_some(),
            "transport_gco2 should be present for cross-region HTTP"
        );
        assert!(
            summary.transport_gco2.unwrap() > 0.0,
            "transport_gco2 should be positive"
        );
    }

    #[test]
    fn transport_co2_same_region_zero() {
        use crate::test_helpers::make_http_event_with_size;
        // HTTP call within same region should not produce transport CO2.
        let mut event = make_http_event_with_size(
            "t1",
            "s1",
            "http://order-api:8080/api/orders",
            "2025-07-10T14:32:01.000Z",
            Some(100_000),
        );
        event.cloud_region = Some("eu-west-3".to_string());
        let trace = make_trace(vec![event]);

        let mut service_regions = HashMap::new();
        service_regions.insert("order-api".to_string(), "eu-west-3".to_string());

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            use_hourly_profiles: false,
            per_operation_coefficients: false,
            include_network_transport: true,
            ..CarbonContext::default()
        };

        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        assert!(
            summary.transport_gco2.is_none(),
            "transport_gco2 should be None for same-region calls"
        );
    }

    #[test]
    fn transport_co2_disabled_by_default() {
        use crate::test_helpers::make_http_event_with_size;
        let mut event = make_http_event_with_size(
            "t1",
            "s1",
            "http://order-api:8080/api/orders",
            "2025-07-10T14:32:01.000Z",
            Some(100_000),
        );
        event.cloud_region = Some("eu-west-3".to_string());
        let trace = make_trace(vec![event]);

        let mut service_regions = HashMap::new();
        service_regions.insert("order-api".to_string(), "us-east-1".to_string());

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            use_hourly_profiles: false,
            per_operation_coefficients: false,
            include_network_transport: false, // disabled
            ..CarbonContext::default()
        };

        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        assert!(
            summary.transport_gco2.is_none(),
            "transport_gco2 should be None when disabled"
        );
    }

    #[test]
    fn transport_co2_no_response_size() {
        use crate::test_helpers::make_http_event_with_size;
        // HTTP call without response_size_bytes should not contribute transport.
        let mut event = make_http_event_with_size(
            "t1",
            "s1",
            "http://order-api:8080/api/orders",
            "2025-07-10T14:32:01.000Z",
            None, // no size
        );
        event.cloud_region = Some("eu-west-3".to_string());
        let trace = make_trace(vec![event]);

        let mut service_regions = HashMap::new();
        service_regions.insert("order-api".to_string(), "us-east-1".to_string());

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            use_hourly_profiles: false,
            per_operation_coefficients: false,
            include_network_transport: true,
            ..CarbonContext::default()
        };

        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        assert!(
            summary.transport_gco2.is_none(),
            "transport_gco2 should be None when response_size_bytes is absent"
        );
    }

    #[test]
    fn transport_co2_sql_excluded() {
        // SQL spans should never contribute transport energy.
        let mut event = make_sql_event(
            "t1",
            "s1",
            "SELECT * FROM users WHERE id = 1",
            "2025-07-10T14:32:01.000Z",
        );
        event.cloud_region = Some("eu-west-3".to_string());
        event.response_size_bytes = Some(100_000);
        let trace = make_trace(vec![event]);

        let mut service_regions = HashMap::new();
        service_regions.insert("order-api".to_string(), "us-east-1".to_string());

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            use_hourly_profiles: false,
            per_operation_coefficients: false,
            include_network_transport: true,
            ..CarbonContext::default()
        };

        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        assert!(
            summary.transport_gco2.is_none(),
            "transport_gco2 should be None for SQL spans"
        );
    }

    #[test]
    fn transport_co2_numerical_value() {
        use crate::test_helpers::make_http_event_with_size;
        // Verify the exact transport CO2 value, not just > 0.
        // 100_000 bytes * 4e-11 kWh/byte * 56.0 gCO2/kWh * 1.135 PUE
        let response_bytes: u64 = 100_000;
        let mut event = make_http_event_with_size(
            "t1",
            "s1",
            "http://order-api:8080/api/orders",
            "2025-07-10T14:32:01.000Z",
            Some(response_bytes),
        );
        event.cloud_region = Some("eu-west-3".to_string());
        let trace = make_trace(vec![event]);

        let mut service_regions = HashMap::new();
        service_regions.insert("order-api".to_string(), "us-east-1".to_string());

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            use_hourly_profiles: false,
            per_operation_coefficients: false,
            include_network_transport: true,
            ..CarbonContext::default()
        };

        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        let transport = summary.transport_gco2.unwrap();
        // eu-west-3: intensity=56.0, PUE=1.135
        let expected =
            response_bytes as f64 * carbon::DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH * 56.0 * 1.135;
        assert!(
            (transport - expected).abs() < 1e-18,
            "expected {expected}, got {transport}"
        );
    }

    #[test]
    fn transport_co2_uppercase_hostname_matches() {
        use crate::test_helpers::make_http_event_with_size;
        // Target URL has uppercase hostname; service_regions keys are lowercase.
        let mut event = make_http_event_with_size(
            "t1",
            "s1",
            "http://Order-API:8080/api/orders",
            "2025-07-10T14:32:01.000Z",
            Some(50_000),
        );
        event.cloud_region = Some("eu-west-3".to_string());
        let trace = make_trace(vec![event]);

        let mut service_regions = HashMap::new();
        service_regions.insert("order-api".to_string(), "us-east-1".to_string());

        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            use_hourly_profiles: false,
            per_operation_coefficients: false,
            include_network_transport: true,
            ..CarbonContext::default()
        };

        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));
        assert!(
            summary.transport_gco2.is_some(),
            "uppercase hostname should match lowercase service_regions key"
        );
    }
}
