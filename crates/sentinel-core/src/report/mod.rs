//! Report stage: outputs analysis results.
//!
//! # Deserialization invariant (baseline round-trip)
//!
//! The full [`Report`] tree derives `Deserialize` so `perf-sentinel
//! report --before <baseline.json>` can feed a stored baseline back in.
//! Every saved baseline from a past release must keep parsing after a
//! minor version bump, so the following rule is load-bearing:
//!
//! **New fields added to `Report`, `Analysis`, `GreenSummary`,
//! `QualityGate`, `Finding`, `Pattern`, `TopOffender`, `CarbonReport`,
//! `CarbonEstimate`, `RegionBreakdown` or any nested type must be
//! either `Option<T>` or carry `#[serde(default)]` with a sensible
//! `Default` impl.** A required field added to any of these types
//! breaks every stored baseline and every downstream consumer that
//! deserializes via the same JSON.
//!
//! Removed fields should stay in the struct for at least one minor
//! version with `#[serde(default)]` so incoming JSON from the previous
//! version does not fail on unknown-field attempts to re-read them.
//!
//! We deliberately do NOT add `#[serde(deny_unknown_fields)]`. The
//! trade-off is that a typo like `findigs:` silently deserializes as
//! the default (empty vec), so production pipelines should validate
//! baseline shapes upstream when they care.

pub mod html;
pub mod interpret;
pub mod json;
pub mod metrics;
pub mod periodic;
pub mod sarif;
pub mod warnings;

pub use self::warnings::Warning;

use crate::correlate::Trace;
use crate::detect::Finding;
use crate::detect::correlate_cross::CrossTraceCorrelation;
use crate::report::interpret::InterpretationLevel;
use crate::score::carbon::{CarbonReport, RegionBreakdown, ScoringConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Read `detection_config` without ever failing the enclosing report: a
/// shape this binary cannot parse becomes `None`.
fn lenient_detection_config<'de, D>(
    deserializer: D,
) -> Result<Option<crate::detect::DetectConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// A complete analysis report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub analysis: Analysis,
    pub findings: Vec<Finding>,
    pub green_summary: GreenSummary,
    pub quality_gate: QualityGate,
    /// Raw I/O operation count per `(service, endpoint)`. Populated by
    /// the pipeline regardless of `[green] enabled`, so the `diff`
    /// subcommand works even with green scoring off. Sorted by `service`
    /// then `endpoint` for deterministic JSON output. Empty when no
    /// traces were analyzed.
    ///
    /// Lives on `Report` rather than on `GreenSummary` because it is a
    /// raw telemetry counter, not a green metric, and is filled in
    /// regardless of the green configuration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_endpoint_io_ops: Vec<PerEndpointIoOps>,
    /// Cross-trace temporal correlations produced by the daemon's
    /// correlator. Always empty in the batch pipeline (the correlator
    /// runs over a rolling window that batch mode does not maintain).
    /// The HTML dashboard's Correlations tab lights up when this field
    /// is non-empty, i.e. when a daemon-produced Report is fed into
    /// `perf-sentinel report --input <daemon.json>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub correlations: Vec<CrossTraceCorrelation>,
    /// Snapshot- or analysis-level warnings surfaced to consumers. The
    /// daemon's `/api/export/report` cold-start path populates this with
    /// `"daemon has not yet processed any events"` so consumers can
    /// distinguish "daemon is empty" from "daemon emitted zero findings"
    /// without resorting to a 5xx HTTP status. Empty in CLI batch
    /// output. Additive on pre-0.5.16 baselines via `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Structured snapshot warnings (0.5.19+). Coexists with the legacy
    /// `warnings: Vec<String>` field. Each entry carries a stable
    /// `kind` (suitable for alerting / aggregation) and a
    /// human-readable `message`. Renderers prefer this field when
    /// non-empty, fall back to `warnings` otherwise. Additive on
    /// pre-0.5.19 baselines via `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warning_details: Vec<Warning>,
    /// Findings filtered out by the user's acknowledgments file
    /// (`.perf-sentinel-acknowledgments.toml`), paired with the matching
    /// ack metadata. Cleared from the wire payload by default; the CLI
    /// only retains it when `--show-acknowledged` is set so audit output
    /// stays opt-in. Additive on pre-0.5.17 baselines via `serde(default)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acknowledged_findings: Vec<AcknowledgedFinding>,
    /// `CARGO_PKG_VERSION` of the binary that wrote this report. Empty
    /// on reports written by binaries that predate this field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub binary_version: String,
    /// Avoidable energy/carbon tiers (operator + canonical threshold), set
    /// only by the daemon archive path (the periodic aggregator reads them).
    /// `None` in batch and live outputs. Additive via `serde(default)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosure_waste: Option<DisclosureWaste>,
    /// The `[detection]` thresholds the producing run detected with, so
    /// consumers can state values, not only key names. Absent on
    /// pre-0.9.25 baselines, and on any baseline whose shape this binary
    /// cannot read: the field is informational, so a renamed threshold or
    /// an unknown enum variant degrades to `None` rather than failing the
    /// whole report parse. Half-read thresholds would be worse than none,
    /// consumers display them as the values that produced the findings.
    #[serde(default, deserialize_with = "lenient_detection_config")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detection_config: Option<crate::detect::DetectConfig>,
}

/// A finding paired with the acknowledgment that suppressed it.
///
/// Surfaced under [`Report::acknowledged_findings`] when the operator
/// asks for `--show-acknowledged`. The CLI clears this vector from the
/// emitted payload otherwise so the default audit trail is opt-in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcknowledgedFinding {
    pub finding: Finding,
    pub acknowledgment: crate::acknowledgments::Acknowledgment,
}

/// Avoidable energy/carbon at one N+1 threshold, archived per window.
/// `avoidable_kwh`/`avoidable_gco2` are the energy/carbon shares of the
/// avoidable I/O ops. The aggregator sums these and derives ratio/efficiency
/// into the period-aggregate `periodic::schema::WasteTier` (gCO₂ → kg there).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AvoidableTier {
    pub n_plus_one_threshold: u32,
    pub avoidable_io_ops: usize,
    pub avoidable_kwh: f64,
    pub avoidable_gco2: f64,
}

/// The two avoidable tiers archived with a daemon window: `canonical` at the
/// binary-pinned threshold (non-manipulable), `operational` at the operator's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DisclosureWaste {
    pub canonical: AvoidableTier,
    pub operational: AvoidableTier,
    /// Database-side waste for the window, both tiers. `None` when the
    /// window produced no [`DatabaseWaste`] figure. Absent on archives
    /// predating the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<DisclosureDbWaste>,
    /// Broker-side waste for the window, both tiers. `None` when the
    /// window produced no messaging figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging: Option<DisclosureMsgWaste>,
}

/// Window database waste at both thresholds. The canonical figure uses
/// the same measured or estimated energy with the SQL ratio recomputed
/// at the binary-pinned N+1 threshold, so the published number cannot
/// be shrunk by raising the operator threshold.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DisclosureDbWaste {
    /// Window energy of the database figure (measured or estimated).
    pub energy_kwh: f64,
    /// Provenance tag of that energy (`alumet_rapl` measured,
    /// `estimated` fallback).
    pub model: String,
    pub operational_waste_kwh: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operational_waste_gco2: Option<f64>,
    pub canonical_waste_kwh: f64,
    /// Total carbon of the subsystem for the window, not just its waste.
    /// `None` when the window energy had no carbon conversion. Reported
    /// beside the totals, never inside them: this is a different scope
    /// from the instrumented services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_gco2: Option<f64>,
    /// Window `energy_gco2` scaled by the canonical SQL ratio; `None`
    /// when the window energy had no carbon conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_waste_gco2: Option<f64>,
}

/// Window messaging waste at both thresholds. Wire-identical to the
/// database block by design, so one struct serves both fields and the two
/// can never drift, the same reasoning as `MessagingWasteAggregate`.
pub type DisclosureMsgWaste = DisclosureDbWaste;

/// Analysis metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub duration_ms: u64,
    pub events_processed: usize,
    pub traces_analyzed: usize,
    /// Batch OTLP ingest tally: spans received vs filtered before the
    /// pipeline ran. `None` when the input was not OTLP (native, Jaeger,
    /// Zipkin carry no per-reason classification yet) or on reports from
    /// versions predating the field. Without it a thin report cannot be
    /// told apart from unusable instrumentation, see
    /// `docs/LIMITATIONS.md` "Instrumentation quality bounds findings".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest: Option<IngestStats>,
}

/// Span-level ingest tally embedded in [`Analysis`], the batch-report
/// mirror of the daemon's `perf_sentinel_otlp_spans_*` Prometheus pair.
/// Field names follow the stable `reason` label values of
/// `perf_sentinel_otlp_spans_filtered_total` (see `docs/METRICS.md`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IngestStats {
    /// Every span present in the input, before any filtering.
    pub spans_received: u64,
    /// Spans skipped as non-analyzable, all reasons together.
    pub spans_filtered: u64,
    pub filtered_not_io: u64,
    pub filtered_missing_db_statement: u64,
    pub filtered_missing_http_url: u64,
    pub filtered_non_sql_datastore: u64,
    pub filtered_merged_db_span: u64,
    /// Share of I/O-shaped spans that were analyzable (retained over
    /// retained plus the attribute-gap drops). `None` when no I/O-shaped
    /// span was seen. Semantics owned by
    /// [`crate::ingest::otlp::SpanConversionStats::usable_span_ratio`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usable_span_ratio: Option<f64>,
}

impl From<crate::ingest::otlp::SpanConversionStats> for IngestStats {
    fn from(stats: crate::ingest::otlp::SpanConversionStats) -> Self {
        Self {
            spans_received: stats.received,
            spans_filtered: stats.received.saturating_sub(stats.retained()),
            filtered_not_io: stats.filtered_not_io,
            filtered_missing_db_statement: stats.filtered_missing_db_statement,
            filtered_missing_http_url: stats.filtered_missing_http_url,
            filtered_non_sql_datastore: stats.filtered_non_sql_datastore,
            filtered_merged_db_span: stats.filtered_merged_db_span,
            usable_span_ratio: stats.usable_span_ratio(),
        }
    }
}

/// `GreenOps` summary of I/O waste.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GreenSummary {
    pub total_io_ops: usize,
    pub avoidable_io_ops: usize,
    /// SQL share of `total_io_ops`. Together with `avoidable_sql_io_ops`
    /// this lets operators apply the SQL-only waste ratio to a measured
    /// database energy reading (e.g. Alumet on the database cgroup).
    /// `0` on baselines from versions before this field existed.
    #[serde(default)]
    pub total_sql_io_ops: usize,
    /// SQL share of `avoidable_io_ops`, same dedup semantics restricted
    /// to the SQL finding types (`n_plus_one_sql`, `redundant_sql`).
    /// `0` on baselines from versions before this field existed.
    #[serde(default)]
    pub avoidable_sql_io_ops: usize,
    /// Messaging share of `total_io_ops`, same construction as the SQL pair.
    /// `0` on baselines predating the field.
    #[serde(default)]
    pub total_messaging_io_ops: usize,
    /// Messaging share of `avoidable_io_ops`, restricted to
    /// `n_plus_one_messaging`. `0` on baselines predating the field.
    #[serde(default)]
    pub avoidable_messaging_io_ops: usize,
    /// Region-resolved I/O ops (`total_io_ops` minus the unknown bucket): the
    /// denominator behind `co2.avoidable`. In-process only (`serde(skip)`),
    /// read by the daemon to rescale avoidable at the canonical threshold.
    #[serde(skip)]
    pub accounted_io_ops: usize,
    pub io_waste_ratio: f64,
    /// Classification band for `io_waste_ratio`
    /// (`healthy` / `moderate` / `high` / `critical`).
    ///
    /// Computed by [`InterpretationLevel::for_waste_ratio`]. The enum
    /// values are stable across versions, the thresholds behind them
    /// are versioned with the binary. See the [`interpret`] module for
    /// the stability contract.
    pub io_waste_ratio_band: InterpretationLevel,
    pub top_offenders: Vec<TopOffender>,
    /// Structured CO₂ report. Includes 2× multiplicative uncertainty
    /// bracket, SCI v1.0 methodology tags, and operational + embodied terms.
    /// `None` when green scoring is disabled or when no events were analyzed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub co2: Option<CarbonReport>,
    /// Per-region operational CO₂ breakdown sorted by `co2_gco2` descending.
    /// Empty when green scoring is disabled or no events were analyzed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<RegionBreakdown>,
    /// Network transport CO₂ (gCO₂eq). Present when at least one
    /// cross-region HTTP call carried response size data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_gco2: Option<f64>,
    /// Settings that shaped the carbon numbers: the applied coefficients
    /// on every run, plus the Electricity Maps dimensions when that API
    /// is configured (read `electricity_maps` before naming it, presence
    /// alone no longer implies it). Surfaced for Scope 2 audit trails so
    /// reporters can verify which model produced the numbers without
    /// reading the operator's TOML config. `None` when green scoring is
    /// off. Additive on pre-0.5.12 baselines via `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoring_config: Option<ScoringConfig>,
    /// Total energy consumed by the workload during the scoring window
    /// in kWh, runtime-calibrated. Sum of per-service energy when
    /// service-level measurement is available, falls back to the
    /// operational proxy (`total_io_ops × ENERGY_PER_IO_OP_KWH`) when
    /// not. `0.0` on pre-carbon-attribution baselines via `serde(default)`.
    #[serde(default)]
    pub energy_kwh: f64,
    /// Energy model used to compute `energy_kwh`. One of
    /// `"alumet_rapl"`, `"scaphandre_rapl"`, `"kepler_ebpf"`,
    /// `"redfish_bmc"`, `"cloud_specpower"`, `"io_proxy_v3"`,
    /// `"io_proxy_v2"`, `"io_proxy_v1"`, with optional `+cal` suffix
    /// when per-service calibration factors are active. Reflects the
    /// highest-fidelity model observed in the window (not weighted by
    /// energy consumption). Empty string on pre-carbon-attribution
    /// baselines.
    #[serde(default)]
    pub energy_model: String,
    /// Operational carbon per service in kgCO2eq. Excludes the embodied
    /// term (which stays in `co2.total` only) and the transport term.
    /// Built at scoring time using the runtime-resolved
    /// `service → region` mapping and the per-region grid intensity
    /// (Electricity Maps real-time when available). Sum is
    /// approximately `co2.operational_gco2 / 1000.0` up to
    /// floating-point rounding. Empty on pre-carbon-attribution baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_carbon_kgco2eq: BTreeMap<String, f64>,
    /// Operational energy per service in kWh. Built at scoring time
    /// using the runtime-resolved energy entries (Scaphandre per-process
    /// RAPL when available, cloud `SPECpower` interpolation otherwise,
    /// proxy fallback). Sum is approximately `energy_kwh` up to
    /// floating-point rounding. Empty on pre-carbon-attribution
    /// baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_energy_kwh: BTreeMap<String, f64>,
    /// Per-service region attribution snapshot at scoring time. Surfaces
    /// the `service → region` mapping that produced the per-service
    /// carbon, using `"unknown"` for services that could not be resolved
    /// to a region. Empty on pre-carbon-attribution baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_region: BTreeMap<String, String>,
    /// Per-service energy model tag. Same value set as `energy_model`
    /// (window-level), per-service this time so auditors can verify which
    /// services benefited from Alumet, Scaphandre, Kepler, Redfish, or
    /// cloud `SPECpower` during this window. Presence of any measured tag
    /// (`"alumet_rapl"`, `"scaphandre_rapl"`, `"kepler_ebpf"`,
    /// `"redfish_bmc"`, `"cloud_specpower"`) indicates that at least one span of the
    /// service hit a measured energy source, not that 100% of the
    /// service's spans were measured.
    /// Read together with `per_service_measured_ratio` for the share of
    /// spans that benefited from the measured model. Services without any
    /// measured span inherit the window-level proxy tag; the `+cal` suffix
    /// on that inherited tag reflects window-wide calibration state, not
    /// whether a calibration factor applied to this specific service.
    /// Empty on pre-per-service-model baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_energy_model: BTreeMap<String, String>,
    /// Fraction of spans whose energy was resolved by Scaphandre or
    /// cloud `SPECpower` (versus proxy fallback) per service, in `[0.0,
    /// 1.0]`. `1.0` means every span had measured energy, `0.0` means
    /// the service fell back to proxy entirely. Pair with
    /// `per_service_energy_model` to assess fidelity. The aggregator
    /// surfaces a simple arithmetic mean of these per-window ratios
    /// under `aggregate.per_service_measured_ratio`, not a span-weighted
    /// average. Empty on pre-per-service-ratio baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_measured_ratio: BTreeMap<String, f64>,
    /// Database-side waste figure, on every run. Measured on the
    /// declared `[green.alumet.database]` cgroup when a reading landed
    /// (daemon), otherwise estimated from the modeled energy of the
    /// window's SQL spans; `model` says which. Never summed into
    /// `energy_kwh`/`co2` (the estimated energy is a re-presented share
    /// of them), published in the disclosure only as the separate
    /// `aggregate.database_waste` block (`docs/METHODOLOGY.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_waste: Option<DatabaseWaste>,
    /// Broker-side avoidable energy, same construction and same status.
    /// `None` when no broker energy was available for the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_waste: Option<MessagingWaste>,
}

/// `model` value on the estimated [`DatabaseWaste`] path: the figure is
/// built from the modeled energy of the SQL spans, not a measurement of
/// the database. The measured path carries `alumet_rapl` instead.
pub const DB_WASTE_MODEL_ESTIMATED: &str = "estimated";

/// Provenance tag of a broker figure built from a declared cluster and
/// the embedded `SPECpower` table, distinct from `cloud_specpower`,
/// which means a CPU scrape rather than a declaration.
pub const BROKER_WASTE_MODEL_SPECPOWER: &str = "broker_specpower";

/// Database window energy × the SQL-only waste ratio. Informational,
/// never summed into the report totals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatabaseWaste {
    /// Window energy of the database cgroup in kWh (CPU share only).
    pub energy_kwh: f64,
    /// `energy_kwh × sql_waste_ratio`.
    pub waste_kwh: f64,
    /// `waste_kwh` × declared region intensity × PUE. `None` without a
    /// declared or known region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waste_gco2: Option<f64>,
    /// gCO₂ of the whole `energy_kwh` (region-resolved population on
    /// the estimated path). Ratio-independent base the disclosure's
    /// canonical tier rescales from, so an operator threshold cannot
    /// zero the canonical carbon figure. `None` without a conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_gco2: Option<f64>,
    /// Operator-declared region of the database host. `None` on the
    /// estimated path (no database was declared).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// `avoidable_sql_io_ops / total_sql_io_ops`, in `[0, 1]`.
    pub sql_waste_ratio: f64,
    /// Provenance tag: `alumet_rapl` when the energy is measured on the
    /// declared database cgroup, otherwise the window's energy model
    /// (`io_proxy_v3`, ...) for the estimated fallback built from the
    /// modeled energy of the window's SQL spans. Empty on baselines
    /// predating the field.
    #[serde(default)]
    pub model: String,
}

/// Broker-side avoidable energy for the window, the messaging twin of
/// [`DatabaseWaste`]. Informational, never folded into the report
/// totals. Which way it errs depends on `model`, the three sources do
/// not agree: `alumet_rapl` reads CPU only and so under-counts,
/// `broker_specpower` bounds the declared vCPUs at full load without
/// storage or network, `estimated` re-presents a share of the totals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagingWaste {
    /// Window energy of the broker, measured or declared.
    pub energy_kwh: f64,
    /// `energy_kwh` × `messaging_waste_ratio`.
    pub waste_kwh: f64,
    /// `waste_kwh` converted with the declared region intensity × PUE.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waste_gco2: Option<f64>,
    /// gCO₂ of the whole `energy_kwh`, the ratio-independent base the
    /// disclosure's canonical tier rescales from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_gco2: Option<f64>,
    /// Operator-declared region of the broker. `None` on the estimated path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// `avoidable_messaging_io_ops / total_messaging_io_ops`, in `[0, 1]`.
    pub messaging_waste_ratio: f64,
    /// Provenance: `alumet_rapl` measured on the declared cgroup,
    /// `broker_specpower` for a declared cluster, `estimated` for the
    /// fallback built from the modeled energy of the window's publishes.
    #[serde(default)]
    pub model: String,
}

/// Raw I/O operation count for a single `(service, endpoint)` pair.
///
/// Stable JSON shape: field names will not be renamed or removed in a
/// minor release. The `(service, endpoint)` pair is the
/// primary key so the same endpoint path served by two different
/// services produces two distinct entries (microservices commonly share
/// generic paths like `/health`, `/metrics`, `/api/users`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerEndpointIoOps {
    pub service: String,
    pub endpoint: String,
    pub io_ops: usize,
}

/// Single-pass per-endpoint I/O op counter. Returns the counts sorted by
/// `(service, endpoint)` for deterministic output. O(N) over the total
/// span count.
///
/// Used by the pipeline to populate `Report.per_endpoint_io_ops` when
/// green scoring is **disabled**. When green scoring is enabled,
/// [`crate::score::score_green`] returns the same data as part of its
/// own single-pass span iteration, so this helper is not called and the
/// hot path stays a single O(N) walk.
#[must_use]
pub fn compute_per_endpoint_io_ops(traces: &[Trace]) -> Vec<PerEndpointIoOps> {
    // BTreeMap so the resulting Vec is naturally sorted by key without
    // a separate sort pass. Key is `(service, endpoint)` so two traces
    // for the same endpoint on different services stay distinct.
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for trace in traces {
        for span in &trace.spans {
            let key = (
                span.event.service.as_ref(),
                span.event.source.endpoint.as_str(),
            );
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .map(|((service, endpoint), io_ops)| PerEndpointIoOps {
            service: service.to_string(),
            endpoint: endpoint.to_string(),
            io_ops,
        })
        .collect()
}

impl GreenSummary {
    /// Create a `GreenSummary` with only `total_io_ops` set (green scoring disabled).
    #[must_use]
    pub fn disabled(total_io_ops: usize) -> Self {
        Self {
            total_io_ops,
            avoidable_io_ops: 0,
            total_sql_io_ops: 0,
            avoidable_sql_io_ops: 0,
            total_messaging_io_ops: 0,
            avoidable_messaging_io_ops: 0,
            accounted_io_ops: total_io_ops,
            io_waste_ratio: 0.0,
            io_waste_ratio_band: InterpretationLevel::Healthy,
            top_offenders: vec![],
            co2: None,
            regions: vec![],
            transport_gco2: None,
            scoring_config: None,
            energy_kwh: 0.0,
            energy_model: String::new(),
            per_service_carbon_kgco2eq: BTreeMap::new(),
            per_service_energy_kwh: BTreeMap::new(),
            per_service_region: BTreeMap::new(),
            per_service_energy_model: BTreeMap::new(),
            per_service_measured_ratio: BTreeMap::new(),
            database_waste: None,
            messaging_waste: None,
        }
    }
}

/// A top offender endpoint ranked by I/O Intensity Score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopOffender {
    pub endpoint: String,
    pub service: String,
    pub io_intensity_score: f64,
    /// Classification band for `io_intensity_score`. Stable enum values
    /// across versions, thresholds versioned with the binary. See the
    /// [`interpret`] module for the stability contract.
    pub io_intensity_band: InterpretationLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub co2_grams: Option<f64>,
}

/// Quality gate result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGate {
    pub passed: bool,
    pub rules: Vec<QualityRule>,
}

/// A single quality gate rule check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityRule {
    pub rule: String,
    pub threshold: f64,
    pub actual: f64,
    pub passed: bool,
}

/// Trait for report output sinks.
pub trait ReportSink {
    type Error: std::error::Error;

    /// # Errors
    ///
    /// Returns an error if the report cannot be written to the output sink.
    fn emit(&self, report: &Report) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_summary_pre_0512_baseline_loads_without_scoring_config() {
        // Hand-crafted JSON shaped like a pre-0.5.12 baseline (no
        // scoring_config field). The Option must default to None,
        // ensuring `report --before <old.json>` still works after the
        // additive change.
        let json = r#"{
            "total_io_ops": 0,
            "avoidable_io_ops": 0,
            "io_waste_ratio": 0.0,
            "io_waste_ratio_band": "healthy",
            "top_offenders": []
        }"#;
        let summary: GreenSummary = serde_json::from_str(json).expect("backward-compat parse");
        assert!(summary.scoring_config.is_none());
    }

    #[test]
    fn green_summary_disabled_factory_has_no_scoring_config() {
        let summary = GreenSummary::disabled(0);
        assert!(summary.scoring_config.is_none());
    }

    #[test]
    fn green_summary_skips_scoring_config_when_none() {
        let summary = GreenSummary::disabled(42);
        let json = serde_json::to_string(&summary).unwrap();
        assert!(
            !json.contains("scoring_config"),
            "scoring_config should be skipped when None, got: {json}"
        );
    }

    fn minimal_report_json_without_warning_details() -> String {
        // Shaped like a 0.5.18 Report (no warning_details key). Used to
        // verify that the new field defaults to empty when absent, so a
        // pre-0.5.19 baseline replayed via `report --before <old.json>`
        // still parses cleanly.
        r#"{
            "analysis": {"duration_ms": 0, "events_processed": 0, "traces_analyzed": 0},
            "findings": [],
            "green_summary": {
                "total_io_ops": 0,
                "avoidable_io_ops": 0,
                "io_waste_ratio": 0.0,
                "io_waste_ratio_band": "healthy",
                "top_offenders": []
            },
            "quality_gate": {"passed": true, "rules": []},
            "warnings": ["legacy warning text"]
        }"#
        .to_string()
    }

    #[test]
    fn report_warning_details_default_empty_when_absent() {
        let report: Report =
            serde_json::from_str(&minimal_report_json_without_warning_details()).expect("parse");
        assert!(report.warning_details.is_empty());
    }

    #[test]
    fn report_legacy_warnings_field_still_parses() {
        let report: Report =
            serde_json::from_str(&minimal_report_json_without_warning_details()).expect("parse");
        assert_eq!(report.warnings, vec!["legacy warning text".to_string()]);
        assert!(report.warning_details.is_empty());
    }

    #[test]
    fn report_warning_details_skipped_in_serialize_when_empty() {
        let report = crate::test_helpers::empty_report();
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(
            !json.contains("warning_details"),
            "warning_details should be skipped when empty, got: {json}"
        );
    }

    #[test]
    fn report_warning_details_serialized_when_present() {
        let mut report = crate::test_helpers::empty_report();
        report.warning_details = vec![
            Warning::new("cold_start", "msg one"),
            Warning::new("ingestion_drops", "msg two"),
        ];
        let json = serde_json::to_string(&report).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let array = parsed
            .get("warning_details")
            .and_then(|v| v.as_array())
            .expect("warning_details array");
        assert_eq!(array.len(), 2);
        assert_eq!(array[0]["kind"], "cold_start");
        assert_eq!(array[1]["kind"], "ingestion_drops");
    }

    #[test]
    fn green_summary_roundtrip_with_new_carbon_attribution_fields() {
        let mut per_service_carbon = BTreeMap::new();
        per_service_carbon.insert("checkout".to_string(), 0.42);
        per_service_carbon.insert("catalog".to_string(), 0.11);
        let mut per_service_energy = BTreeMap::new();
        per_service_energy.insert("checkout".to_string(), 0.0021);
        per_service_energy.insert("catalog".to_string(), 0.0005);
        let mut per_service_region = BTreeMap::new();
        per_service_region.insert("checkout".to_string(), "eu-west-3".to_string());
        per_service_region.insert("catalog".to_string(), "unknown".to_string());
        let mut per_service_energy_model = BTreeMap::new();
        per_service_energy_model.insert("checkout".to_string(), "scaphandre_rapl".to_string());
        per_service_energy_model.insert("catalog".to_string(), "io_proxy_v3+cal".to_string());
        let mut per_service_measured_ratio = BTreeMap::new();
        per_service_measured_ratio.insert("checkout".to_string(), 0.75);
        per_service_measured_ratio.insert("catalog".to_string(), 0.0);

        let summary = GreenSummary {
            energy_kwh: 0.0026,
            energy_model: "scaphandre_rapl+cal".to_string(),
            per_service_carbon_kgco2eq: per_service_carbon.clone(),
            per_service_energy_kwh: per_service_energy.clone(),
            per_service_region: per_service_region.clone(),
            per_service_energy_model: per_service_energy_model.clone(),
            per_service_measured_ratio: per_service_measured_ratio.clone(),
            ..GreenSummary::disabled(0)
        };
        let json = serde_json::to_string(&summary).expect("serialize");
        let parsed: GreenSummary = serde_json::from_str(&json).expect("deserialize");

        assert!((parsed.energy_kwh - 0.0026).abs() < 1e-12);
        assert_eq!(parsed.energy_model, "scaphandre_rapl+cal");
        assert_eq!(parsed.per_service_carbon_kgco2eq, per_service_carbon);
        assert_eq!(parsed.per_service_energy_kwh, per_service_energy);
        assert_eq!(parsed.per_service_region, per_service_region);
        assert_eq!(parsed.per_service_energy_model, per_service_energy_model);
        assert_eq!(
            parsed.per_service_measured_ratio,
            per_service_measured_ratio
        );
    }

    #[test]
    fn green_summary_legacy_baseline_deserializes_with_default_carbon_attribution() {
        // A pre-carbon-attribution archive line carries `GreenSummary`
        // without `energy_kwh`, `energy_model`, or the per_service_*
        // maps. Deserialization must fill them with the documented
        // defaults so the aggregator can detect the absence and fall
        // back to the proxy path.
        let legacy = serde_json::json!({
            "total_io_ops": 100,
            "avoidable_io_ops": 5,
            "io_waste_ratio": 0.05,
            "io_waste_ratio_band": "healthy",
            "top_offenders": []
        });
        let parsed: GreenSummary = serde_json::from_value(legacy).expect("deserialize legacy");
        assert!(parsed.energy_kwh.abs() < f64::EPSILON);
        assert!(parsed.energy_model.is_empty());
        assert!(parsed.per_service_carbon_kgco2eq.is_empty());
        assert!(parsed.per_service_energy_kwh.is_empty());
        assert!(parsed.per_service_region.is_empty());
        assert!(parsed.per_service_energy_model.is_empty());
        assert!(parsed.per_service_measured_ratio.is_empty());
    }
}
