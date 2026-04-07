//! Scoring stage: computes `GreenOps` I/O intensity scores.

pub mod carbon;

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::correlate::Trace;
use crate::detect::{Finding, GreenImpact};
use crate::report::{GreenSummary, TopOffender};
use carbon::{
    CarbonContext, CarbonEstimate, CarbonReport, REGION_STATUS_KNOWN, REGION_STATUS_OUT_OF_TABLE,
    REGION_STATUS_UNRESOLVED, RegionBreakdown, UNKNOWN_REGION, compute_operational_gco2,
    is_valid_region_id, lookup_region_lower, resolve_region,
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

    // Phase 2: Dedup avoidable I/O ops by (trace_id, template, source_endpoint), taking max.
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

    // Phase 3: Compute IIS per endpoint (cached for finding enrichment)
    let iis_map: HashMap<&str, f64> = endpoint_stats
        .iter()
        .map(|(&ep, stats)| {
            let invocations = stats.invocation_count.max(1) as f64;
            (ep, stats.total_io_ops as f64 / invocations)
        })
        .collect();

    // Phase 4: Enrich findings with green_impact
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

    // Phase 5: Multi-region carbon scoring (before top offenders so we can
    // reuse the multi_region_active flag it computes). Only runs when a
    // CarbonContext is provided. Builds the per-region breakdown using a
    // BTreeMap for deterministic accumulation order.
    //
    // N2: multi-region detection is folded into compute_carbon_report's
    // single span pass, saving one full O(n) walk vs the previous
    // detect_multi_region helper.
    let (co2, regions, multi_region_active) = match carbon {
        Some(ctx) => compute_carbon_report(traces, ctx, total_io_ops, avoidable_io_ops),
        None => (None, Vec::new(), false),
    };

    // Phase 6: Build top offenders sorted by IIS descending, with alphabetical tiebreaker.
    // Top-offender CO₂ uses the default region ONLY in mono-region mode. When
    // multi-region scoring is active, the per-offender scalar would be
    // inconsistent with the per-region breakdown, so we set it to None.
    let default_region_lower = if multi_region_active {
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
        co2,
        regions,
    };

    (enriched, green_summary)
}

/// Compute the structured carbon report, per-region breakdown, and the
/// `multi_region_active` flag for one analysis run.
///
/// This function walks the span set **once** to bucket I/O ops per resolved
/// region AND detect whether multi-region scoring is active (via
/// `service_regions` config or any span carrying `cloud.region`). Caps
/// region cardinality at [`MAX_REGIONS`] to prevent memory exhaustion from
/// attacker-controlled `cloud.region` values, looks up grid intensity + PUE
/// per region, computes operational and embodied CO₂, derives the avoidable
/// estimate via ratio (excluding the unknown bucket from the denominator
/// for consistency), and assembles the final [`CarbonReport`] +
/// [`RegionBreakdown`] vector sorted by `co2_gco2` descending with
/// alphabetical tiebreak.
///
/// Each [`RegionBreakdown`] row carries a `status` tag:
/// - [`REGION_STATUS_KNOWN`] — region is in the embedded carbon table
/// - [`REGION_STATUS_OUT_OF_TABLE`] — name resolved but not in the table
/// - [`REGION_STATUS_UNRESOLVED`] — synthetic `"unknown"` bucket
///
/// Returns `(None, vec![], multi_region_active)` for an empty `traces` slice.
/// The flag still reflects `ctx.service_regions` so downstream gating is
/// consistent even with zero events.
#[allow(clippy::too_many_lines)] // Scoring stage with interleaved bucketing, cap, status tagging,
// and invariant assertions; splitting would obscure the data flow.
fn compute_carbon_report(
    traces: &[Trace],
    ctx: &CarbonContext,
    total_io_ops: usize,
    avoidable_io_ops: usize,
) -> (Option<CarbonReport>, Vec<RegionBreakdown>, bool) {
    // N2: multi-region flag seeded from the cheap config signal; updated
    // from span attributes during the main loop below.
    let mut multi_region_active = !ctx.service_regions.is_empty();

    // F1: empty-traces early return. No events → nothing meaningful to report.
    // Still propagate the config-based multi_region_active so that an empty
    // batch with a configured service_regions map is consistent with a
    // non-empty batch from the same config.
    if traces.is_empty() {
        return (None, Vec::new(), multi_region_active);
    }

    // Bucket I/O ops per resolved region. Region keys are lowercased so
    // case variants (e.g. "EU-West-3" vs "eu-west-3") collapse into one bucket.
    // BTreeMap gives deterministic iteration order → stable f64 sums across runs.
    let mut per_region: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown_ops: usize = 0;
    let mut overflow_warned = false;
    for trace in traces {
        for span in &trace.spans {
            // N2: detect multi-region by span attribute in the same pass.
            if span.event.cloud_region.is_some() {
                multi_region_active = true;
            }
            match resolve_region(&span.event, ctx) {
                Some(region) => {
                    // C2: defense-in-depth invariant. All regions reaching
                    // this loop should have been validated at the ingestion
                    // boundary (is_valid_region_id in ingest/otlp.rs and
                    // ingest/json.rs) or rejected at config load. Assert
                    // the invariant in debug builds so any future ingestion
                    // gap fails loudly in test/dev instead of silently
                    // reopening log-forging.
                    debug_assert!(
                        is_valid_region_id(region),
                        "unvalidated region '{region}' reached compute_carbon_report; \
                         ingestion boundary should have sanitized it"
                    );

                    // N5: probe-before-allocate. Fast path when the region
                    // string is already lowercase (the common OTel and
                    // config-convention case): probe borrowed, allocate only
                    // on insert or miss. Fallback path lowercases explicitly.
                    let needs_lowercase = region.bytes().any(|b| b.is_ascii_uppercase());
                    if needs_lowercase {
                        let key = region.to_ascii_lowercase();
                        if per_region.len() >= MAX_REGIONS && !per_region.contains_key(&key) {
                            unknown_ops += 1;
                            if !overflow_warned {
                                tracing::debug!(
                                    "Region cardinality cap ({MAX_REGIONS}) exceeded; \
                                     additional distinct regions folded into 'unknown'."
                                );
                                overflow_warned = true;
                            }
                        } else {
                            *per_region.entry(key).or_insert(0) += 1;
                        }
                    } else if let Some(count) = per_region.get_mut(region) {
                        // Borrowed probe hit: increment without allocating.
                        *count += 1;
                    } else if per_region.len() >= MAX_REGIONS {
                        unknown_ops += 1;
                        if !overflow_warned {
                            tracing::debug!(
                                "Region cardinality cap ({MAX_REGIONS}) exceeded; \
                                 additional distinct regions folded into 'unknown'."
                            );
                            overflow_warned = true;
                        }
                    } else {
                        per_region.insert(region.to_string(), 1);
                    }
                }
                None => unknown_ops += 1,
            }
        }
    }

    // Build per-region breakdown rows. Regions whose name isn't in the
    // embedded carbon table get a zeroed row + debug log (so users notice
    // the misconfiguration without losing the I/O op count).
    let mut regions: Vec<RegionBreakdown> = Vec::with_capacity(per_region.len() + 1);
    let mut operational_gco2: f64 = 0.0;
    for (region, io_ops) in per_region {
        if let Some((intensity, pue)) = lookup_region_lower(&region) {
            let region_co2 = compute_operational_gco2(io_ops, intensity, pue);
            operational_gco2 += region_co2;
            regions.push(RegionBreakdown {
                status: REGION_STATUS_KNOWN,
                region,
                grid_intensity_gco2_kwh: intensity,
                pue,
                io_ops,
                co2_gco2: region_co2,
            });
        } else {
            // W6: out-of-table region — name resolved but not in our table.
            // Distinguishable from "unresolved" via the `status` field.
            tracing::debug!(
                "Region '{region}' is not in the embedded carbon table; \
                 {io_ops} I/O ops contribute 0 to operational CO₂. \
                 See docs/CONFIGURATION.md for the list of supported regions."
            );
            regions.push(RegionBreakdown {
                status: REGION_STATUS_OUT_OF_TABLE,
                region,
                grid_intensity_gco2_kwh: 0.0,
                pue: 0.0,
                io_ops,
                co2_gco2: 0.0,
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
        });
    }

    // SCI v1.0 embodied carbon term M = traces × per-trace constant.
    // Region-independent — emitted unconditionally when we have at least
    // one trace (empty case was early-returned above).
    let embodied_gco2 = traces.len() as f64 * ctx.embodied_per_request_gco2;

    // Total = SCI v1.0 numerator (E × I) + M, summed over all analyzed traces.
    // This is NOT the SCI per-R intensity — consumers compute that themselves
    // as total_mid / traces.len() if they need it.
    let total_mid = operational_gco2 + embodied_gco2;

    // D4: Avoidable CO₂ via region-blind ratio. Denominator excludes the
    // unknown bucket (operational_gco2 already excludes it, so we match).
    // Embodied is NOT included in avoidable — hardware emissions are fixed
    // regardless of whether the application does N+1 queries.
    let accounted_io_ops = total_io_ops.saturating_sub(unknown_ops);
    let avoidable_mid = if accounted_io_ops > 0 {
        operational_gco2 * (avoidable_io_ops as f64 / accounted_io_ops as f64)
    } else {
        0.0
    };

    let report = CarbonReport {
        total: CarbonEstimate::sci_numerator(total_mid),
        avoidable: CarbonEstimate::operational_ratio(avoidable_mid),
        operational_gco2,
        embodied_gco2,
    };

    // D5: sort regions vec by CO₂ descending with alphabetical tiebreak.
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
    use crate::detect::{FindingType, Pattern, Severity};
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
        CarbonContext {
            default_region: Some(region.to_string()),
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
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
        };

        let ctx = ctx_with_region("eu-west-3");
        let (_, summary) = score_green(&[trace], vec![finding], Some(&ctx));

        // Structured CO₂ report present.
        let co2 = summary.co2.as_ref().expect("co2 should be present");
        assert!(co2.total.mid > 0.0);
        assert!(co2.avoidable.mid > 0.0);
        assert_eq!(co2.total.model, "io_proxy_v1");
        // Phase 5a review fixes: methodology field replaces sci_version,
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
        // Phase 5a behavior: an unknown region (not in the embedded carbon
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

    // ----- Phase 5a: SCI / multi-region / parity tests -----

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
        let trace = make_trace_with_region("t1", "eu-west-3", 6);
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.001, // 1 trace × 0.001 = 0.001 g embodied
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
        };
        let ctx = CarbonContext {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.5, // intentionally large to detect leakage
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
        // After the Phase 5a review fix, regions are sorted by co2_gco2 DESC:
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
        // Phase 5a review fix: the regions breakdown is sorted by co2_gco2
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
        // Phase 5a review fix (D2): explicitly verify that feeding the
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
        // Phase 5a review fix: `total` is tagged as the SCI numerator,
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
        };
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));

        // Only one region: ap-south-1 (the span attribute wins).
        assert_eq!(summary.regions.len(), 1);
        assert_eq!(summary.regions[0].region, "ap-south-1");
        assert_eq!(summary.regions[0].io_ops, 1);
    }

    // ----- Phase 5a review fixes: multi-region guard, cap, denominator -----

    #[test]
    fn top_offender_co2_some_in_single_region_mode() {
        // Baseline: with only default_region set (no service_regions, no
        // cloud.region on spans), top offenders carry a scalar co2_grams.
        let trace = make_trace_with_region_no_cloud("t1", 6);
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
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
        // F6: when [green.service_regions] is non-empty, multi-region is
        // active → TopOffender.co2_grams must be None (the scalar would be
        // inconsistent with the per-region breakdown).
        let trace = make_trace_with_region_no_cloud("t1", 6);
        let mut service_regions = HashMap::new();
        service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            embodied_per_request_gco2: 0.0,
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
        // F6: when any span carries cloud.region, multi-region is active.
        let trace = make_trace_with_region("t1", "ap-south-1", 6);
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
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
        // F8: cap at 256 distinct regions. Feed 260 distinct region tags;
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

        // N4: tighten the assertions. With 260 distinct region names fed
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
        // D4: when the unknown bucket has io_ops, the avoidable ratio
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
        };

        let ctx = CarbonContext {
            default_region: None, // no fallback → orphan trace goes to unknown
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
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

    // ----- Review-fix round 4 tests -----

    #[test]
    fn empty_traces_with_carbon_context_returns_no_co2() {
        // W5: explicit test for the F1 early-return branch inside
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
        // W6: `mars-1` resolves (via default_region) but isn't in the carbon
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
        // W6: baseline — eu-west-3 is in the carbon table, status = known.
        let trace = make_trace_with_region("t1", "eu-west-3", 3);
        let ctx = ctx_with_region("eu-west-3");
        let (_, summary) = score_green(&[trace], vec![], Some(&ctx));

        assert_eq!(summary.regions.len(), 1);
        assert_eq!(summary.regions[0].status, "known");
        assert!(summary.regions[0].co2_gco2 > 0.0);
    }
}
