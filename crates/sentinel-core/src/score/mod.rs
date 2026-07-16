//! Scoring stage: computes `GreenOps` I/O intensity scores.

pub mod carbon;
// Generated rows (scripts/refresh-carbon-data.py), logic stays in `carbon`.
mod carbon_data;
pub(crate) mod carbon_profiles;
pub mod cloud_energy;
pub mod electricity_maps;
// `energy_state` is the shared `ArcSwap`-backed storage used by the
// Scaphandre and cloud SPECpower scrapers. It depends on the `arc-swap`
// crate which is optional (only pulled in under the `daemon` feature),
// and its only callers (`scaphandre::state` and `cloud_energy::state`)
// are themselves gated on `daemon`. Gating the module here keeps
// `cargo publish -p perf-sentinel-core` (default features off) green.
#[cfg(feature = "daemon")]
pub(crate) mod energy_state;
// Shared per-service ops-delta tracker used by every measured-energy
// scraper. Daemon-gated for the same reason as `energy_state`.
pub mod kepler;
#[cfg(feature = "daemon")]
pub(crate) mod ops_snapshot_diff;
// Shared Prometheus text-exposition parser, generic over the metric
// name and routing label key. Used by the Kepler and Alumet scrapers.
// Not daemon-gated: config validation compiles it in a bare build.
pub mod prom_parser;
pub mod redfish;
pub mod scaphandre;

// Daemon-only: the canonical avoidable pass runs at archive time. The
// `disclose` subcommand reads pre-computed tiers, it never recomputes them.
#[cfg(feature = "daemon")]
pub(crate) mod canonical;
mod carbon_compute;
mod region_breakdown;

use std::collections::HashMap;

use crate::correlate::Trace;
use crate::detect::{Finding, GreenImpact};
use crate::report::{GreenSummary, PerEndpointIoOps, TopOffender};
use carbon::CarbonContext;
#[cfg(test)]
use carbon::RegionBreakdown;

use carbon_compute::compute_carbon_report;

/// Per-endpoint statistics accumulated during scoring.
struct EndpointStats {
    total_io_ops: usize,
    invocation_count: usize,
    /// Index of the most recent trace in which this endpoint was seen,
    /// used by `count_endpoint_stats` as a sentinel to bump
    /// `invocation_count` only on the first span of a trace that hits
    /// this endpoint. Initialized to `usize::MAX` so trace index `0`
    /// still triggers the bump on first sight.
    last_seen_trace: usize,
}

/// Composite key `(service, endpoint)` for per-endpoint accumulation.
///
/// Two services serving the same path (e.g. `/health`, `/metrics`,
/// `/api/users` in a microservices deployment) produce distinct entries.
/// This is the primary key for both `top_offenders` and the
/// `per_endpoint_io_ops` raw counter, so the two views are joinable.
type EndpointKey<'a> = (&'a str, &'a str);

/// Count I/O ops per `(service, endpoint)` and invocations (distinct
/// traces per `(service, endpoint)`) in a single pass, using
/// [`EndpointStats::last_seen_trace`] as the per-trace sentinel.
fn count_endpoint_stats(traces: &[Trace]) -> (HashMap<EndpointKey<'_>, EndpointStats>, usize) {
    let mut endpoint_stats: HashMap<EndpointKey<'_>, EndpointStats> =
        HashMap::with_capacity(traces.len().min(64));
    let mut total_io_ops: usize = 0;

    for (trace_idx, trace) in traces.iter().enumerate() {
        for span in &trace.spans {
            total_io_ops += 1;
            let key: EndpointKey<'_> = (
                span.event.service.as_ref(),
                span.event.source.endpoint.as_str(),
            );
            let stats = endpoint_stats.entry(key).or_insert_with(|| EndpointStats {
                total_io_ops: 0,
                invocation_count: 0,
                last_seen_trace: usize::MAX,
            });
            stats.total_io_ops += 1;
            if stats.last_seen_trace != trace_idx {
                stats.invocation_count += 1;
                stats.last_seen_trace = trace_idx;
            }
        }
    }

    (endpoint_stats, total_io_ops)
}

/// Project the score-side `endpoint_stats` map into the public
/// [`PerEndpointIoOps`] vector consumed by `Report.per_endpoint_io_ops`.
/// Sorted by `(service, endpoint)` so the diff subcommand sees stable
/// ordering between runs. The `HashMap + sort` backing (rather than
/// `BTreeMap`) is motivated in `docs/design/05-GREENOPS-AND-CARBON.md`
/// section "Step 1 > Backing structure".
fn endpoint_stats_to_per_endpoint_io_ops(
    endpoint_stats: &HashMap<EndpointKey<'_>, EndpointStats>,
) -> Vec<PerEndpointIoOps> {
    // Sort over borrowed pairs so the comparator does not walk fresh
    // heap-allocated `String`s; owned strings are materialized after.
    let mut refs: Vec<(&str, &str, usize)> = endpoint_stats
        .iter()
        .map(|((service, endpoint), stats)| (*service, *endpoint, stats.total_io_ops))
        .collect();
    refs.sort_by(|a, b| a.0.cmp(b.0).then_with(|| a.1.cmp(b.1)));
    refs.into_iter()
        .map(|(service, endpoint, io_ops)| PerEndpointIoOps {
            service: service.to_string(),
            endpoint: endpoint.to_string(),
            io_ops,
        })
        .collect()
}

/// Compute `GreenOps` scores: enrich findings with `green_impact` and produce a `GreenSummary`.
///
/// I/O operation counts are a proxy for energy consumption, not a
/// measurement (actual energy depends on I/O type, latency and
/// infrastructure).
///
/// When `carbon` is `Some`, additionally computes operational CO₂ per
/// region (SCI `O = E × I`, bucketed via [`resolve_region`]), embodied
/// CO₂ (SCI `M`, `traces.len() × embodied_per_request_gco2`), 2×
/// multiplicative confidence intervals, and avoidable CO₂
/// (`operational × avoidable_io_ops / accounted_io_ops`, excluding the
/// synthetic unknown bucket). When `None`, the deprecated scalar fields
/// and the `co2` / `regions` fields are all left empty.
///
/// The step-by-step algorithm (count, IIS, dedup, enrich, rank, then
/// per-region carbon) is documented in
/// `docs/design/05-GREENOPS-AND-CARBON.md`.
#[must_use]
pub fn score_green(
    traces: &[Trace],
    findings: Vec<Finding>,
    carbon: Option<&CarbonContext>,
) -> (Vec<Finding>, GreenSummary, Vec<PerEndpointIoOps>) {
    let (endpoint_stats, total_io_ops) = count_endpoint_stats(traces);
    let per_endpoint_io_ops = endpoint_stats_to_per_endpoint_io_ops(&endpoint_stats);
    let avoidable_io_ops = dedup_avoidable_io_ops(&findings);
    let iis_map = build_iis_map(&endpoint_stats);
    let enriched = enrich_findings_with_iis(findings, &iis_map);

    let carbon_outputs = match carbon {
        Some(ctx) => compute_carbon_report(traces, ctx, total_io_ops, avoidable_io_ops),
        None => carbon_compute::CarbonComputeOutputs {
            report: None,
            regions: Vec::new(),
            multi_region_active: false,
            per_service: std::collections::BTreeMap::new(),
            window_model: "",
            accounted_io_ops: total_io_ops,
        },
    };

    let default_region_lower = top_offender_co2_region(carbon, carbon_outputs.multi_region_active);
    let top_offenders =
        build_top_offenders(&endpoint_stats, &iis_map, default_region_lower.as_deref());

    let io_waste_ratio = if total_io_ops > 0 {
        avoidable_io_ops as f64 / total_io_ops as f64
    } else {
        0.0
    };
    let window_model = carbon_outputs.window_model;
    let per_service = build_per_service_maps(carbon_outputs.per_service, window_model);
    let energy_model = if per_service.energy_kwh > 0.0 {
        window_model.to_string()
    } else {
        String::new()
    };

    let co2 = carbon_outputs.report;
    let green_summary = GreenSummary {
        total_io_ops,
        avoidable_io_ops,
        accounted_io_ops: carbon_outputs.accounted_io_ops,
        io_waste_ratio,
        io_waste_ratio_band: crate::report::interpret::InterpretationLevel::for_waste_ratio(
            io_waste_ratio,
        ),
        top_offenders,
        // Hoisted from co2.transport_gco2 for top-level JSON visibility so
        // consumers can read it without navigating the nested co2 object.
        // Canonical value lives in CarbonReport.
        transport_gco2: co2.as_ref().and_then(|r| r.transport_gco2),
        co2,
        regions: carbon_outputs.regions,
        scoring_config: carbon.and_then(|ctx| ctx.scoring_config.clone()),
        energy_kwh: per_service.energy_kwh,
        energy_model,
        per_service_carbon_kgco2eq: per_service.carbon_kgco2eq,
        per_service_energy_kwh: per_service.energy_kwh_by_service,
        per_service_region: per_service.region,
        per_service_energy_model: per_service.energy_model,
        per_service_measured_ratio: per_service.measured_ratio,
    };

    (enriched, green_summary, per_endpoint_io_ops)
}

/// Dedup avoidable I/O ops by (`trace_id`, template, `source_endpoint`),
/// taking max. Slow findings are not avoidable I/O, they are necessary
/// operations that happen to be slow.
pub(crate) fn dedup_avoidable_io_ops(findings: &[Finding]) -> usize {
    let capacity = findings
        .iter()
        .filter(|f| f.finding_type.is_avoidable_io())
        .count();
    let mut dedup: HashMap<(&str, &str, &str), usize> = HashMap::with_capacity(capacity);
    for f in findings {
        if !f.finding_type.is_avoidable_io() {
            continue;
        }
        let avoidable = f.pattern.occurrences.saturating_sub(1);
        let entry = dedup
            .entry((&f.trace_id, &f.pattern.template, &f.source_endpoint))
            .or_insert(0);
        *entry = (*entry).max(avoidable);
    }
    dedup.values().sum()
}

fn build_iis_map<'a>(
    endpoint_stats: &HashMap<EndpointKey<'a>, EndpointStats>,
) -> HashMap<EndpointKey<'a>, f64> {
    endpoint_stats
        .iter()
        .map(|(&key, stats)| {
            let invocations = stats.invocation_count.max(1) as f64;
            (key, stats.total_io_ops as f64 / invocations)
        })
        .collect()
}

fn enrich_findings_with_iis(
    mut findings: Vec<Finding>,
    iis_map: &HashMap<EndpointKey<'_>, f64>,
) -> Vec<Finding> {
    for f in &mut findings {
        let iis = iis_map
            .get(&(f.service.as_str(), f.source_endpoint.as_str()))
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
            io_intensity_band: crate::report::interpret::InterpretationLevel::for_iis(iis),
        });
    }
    findings
}

/// `TopOffender.co2_grams` uses the flat `ENERGY_PER_IO_OP_KWH`, so we
/// only emit it in mono-region mode with the proxy model and no
/// modifiers. Returns `Some(region)` when emission is safe, `None`
/// otherwise.
fn top_offender_co2_region(
    carbon: Option<&CarbonContext>,
    multi_region_active: bool,
) -> Option<String> {
    let per_op_active = carbon.is_some_and(|ctx| ctx.per_operation_coefficients);
    let has_energy_modifier = carbon.is_some_and(has_energy_modifier);
    if multi_region_active || per_op_active || has_energy_modifier {
        return None;
    }
    carbon
        .and_then(|ctx| ctx.default_region.as_deref())
        .map(str::to_ascii_lowercase)
}

fn has_energy_modifier(ctx: &CarbonContext) -> bool {
    ctx.energy_snapshot.as_ref().is_some_and(|s| !s.is_empty())
        || ctx.calibration.is_some()
        || ctx
            .real_time_intensity
            .as_ref()
            .is_some_and(|rt| !rt.is_empty())
}

fn build_top_offenders<'a>(
    endpoint_stats: &HashMap<EndpointKey<'a>, EndpointStats>,
    iis_map: &HashMap<EndpointKey<'a>, f64>,
    default_region_lower: Option<&str>,
) -> Vec<TopOffender> {
    let mut top_offenders: Vec<TopOffender> = endpoint_stats
        .iter()
        .map(|(&(service, endpoint), stats)| {
            let iis = iis_map.get(&(service, endpoint)).copied().unwrap_or(0.0);
            let co2_grams = default_region_lower
                .and_then(|r| carbon::io_ops_to_co2_grams(stats.total_io_ops, r));
            TopOffender {
                endpoint: endpoint.to_string(),
                service: service.to_string(),
                io_intensity_score: iis,
                io_intensity_band: crate::report::interpret::InterpretationLevel::for_iis(iis),
                co2_grams,
            }
        })
        .collect();
    top_offenders.sort_by(|a, b| {
        b.io_intensity_score
            .total_cmp(&a.io_intensity_score)
            .then_with(|| a.service.cmp(&b.service))
            .then_with(|| a.endpoint.cmp(&b.endpoint))
    });
    top_offenders
}

struct PerServiceMaps {
    energy_kwh: f64,
    energy_kwh_by_service: std::collections::BTreeMap<String, f64>,
    carbon_kgco2eq: std::collections::BTreeMap<String, f64>,
    region: std::collections::BTreeMap<String, String>,
    energy_model: std::collections::BTreeMap<String, String>,
    measured_ratio: std::collections::BTreeMap<String, f64>,
}

fn build_per_service_maps(
    per_service_runtime: std::collections::BTreeMap<
        String,
        carbon_compute::ServiceCarbonAccumulator,
    >,
    window_model: &'static str,
) -> PerServiceMaps {
    let mut out = PerServiceMaps {
        energy_kwh: 0.0,
        energy_kwh_by_service: std::collections::BTreeMap::new(),
        carbon_kgco2eq: std::collections::BTreeMap::new(),
        region: std::collections::BTreeMap::new(),
        energy_model: std::collections::BTreeMap::new(),
        measured_ratio: std::collections::BTreeMap::new(),
    };
    for (svc, acc) in per_service_runtime {
        out.energy_kwh += acc.energy_kwh;
        out.energy_kwh_by_service
            .insert(svc.clone(), acc.energy_kwh);
        out.carbon_kgco2eq
            .insert(svc.clone(), acc.operational_gco2 / 1000.0);
        let svc_tag = acc.measured_model.unwrap_or(window_model);
        out.energy_model.insert(svc.clone(), svc_tag.to_string());
        let ratio = if acc.total_ops == 0 {
            0.0
        } else {
            acc.measured_ops as f64 / acc.total_ops as f64
        };
        out.measured_ratio.insert(svc.clone(), ratio);
        out.region.insert(
            svc,
            if acc.region.is_empty() {
                carbon::UNKNOWN_REGION.to_string()
            } else {
                acc.region
            },
        );
    }
    out
}

#[cfg(test)]
mod tests;
