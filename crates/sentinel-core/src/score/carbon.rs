//! `GreenOps` gCO₂eq conversion: static region-based carbon intensity table.
//!
//! Embeds carbon intensity values per region (gCO₂eq/kWh) and cloud provider PUE.
//! No network calls, all data is embedded at compile time.
//! Sources: Cloud Carbon Footprint (CCF), Electricity Maps annual averages.

use std::collections::HashMap;

use serde::Serialize;

use crate::event::SpanEvent;

/// Estimated energy consumed per I/O operation in kWh.
///
/// This is a rough order-of-magnitude approximation (~0.1 µWh per I/O op).
/// It accounts for a typical database query or HTTP round-trip on cloud
/// infrastructure, including CPU, memory, and network overhead.
///
/// **This is NOT a measured value.** The actual energy depends on I/O type,
/// latency, payload size, and hardware. This constant is used to convert
/// I/O operation counts into estimated gCO₂eq as an indicative metric,
/// not a precise measurement.
///
/// For SCI (ISO/IEC 21031:2024) compliance, this approximation must be
/// disclosed as methodology in reports and documentation.
pub const ENERGY_PER_IO_OP_KWH: f64 = 0.000_000_1;

/// Lower bound factor for the CO₂ confidence interval (`low = mid × 0.5`).
///
/// Combined with [`CO2_HIGH_FACTOR`] this produces a **2× multiplicative
/// uncertainty** around the mid estimate: `low = mid/2`, `high = mid×2`.
/// This is a log-symmetric interval (geometric mean of low and high
/// equals mid), not a symmetric ±50% window — the I/O proxy model's
/// true uncertainty is wider than ±50%, so a 2× multiplicative factor
/// is more honest.
pub const CO2_LOW_FACTOR: f64 = 0.5;

/// Upper bound factor for the CO₂ confidence interval (`high = mid × 2.0`).
/// See [`CO2_LOW_FACTOR`] for rationale on the 2× multiplicative framing.
pub const CO2_HIGH_FACTOR: f64 = 2.0;

/// Identifier for the carbon estimation model used.
///
/// Versioned so that future improvements (per-operation weighting,
/// hourly carbon profiles, RAPL integration) can be tracked in reports
/// without breaking downstream consumers.
pub const CO2_MODEL: &str = "io_proxy_v1";

/// Methodology tag for the **total** carbon estimate.
///
/// `total` holds the SCI v1.0 numerator `(E × I) + M` summed over all
/// analyzed traces — it is NOT the per-R intensity score that the SCI
/// specification defines as "SCI". To get the per-trace intensity,
/// downstream consumers compute `total.mid / analysis.traces_analyzed`
/// themselves.
pub const METHODOLOGY_SCI_NUMERATOR: &str = "sci_v1_numerator";

/// Methodology tag for the **avoidable** carbon estimate.
///
/// `avoidable` is computed as `operational_gco2 × (avoidable_io_ops / accounted_io_ops)`,
/// a region-blind global ratio that deliberately excludes embodied carbon
/// (hardware manufacturing emissions are fixed per request regardless of
/// application efficiency). The tag signals the approximation at the data
/// layer so downstream consumers don't misread it as a per-region
/// attribution.
pub const METHODOLOGY_OPERATIONAL_RATIO: &str = "sci_v1_operational_ratio";

/// Default embodied carbon term `M` per request (per trace) in gCO₂eq.
///
/// Represents amortized hardware manufacturing emissions over the server
/// lifecycle, divided by the typical request volume.
///
/// **Derivation:** a modern x86 server has an embodied footprint of
/// ~1000 kgCO₂eq over a 4-year lifecycle (sources: Boavizta API lifecycle
/// assessments, Cloud Carbon Footprint methodology). At 1 request/second
/// continuously, that amortizes to ~8×10⁻⁶ g/req. Real servers see lower
/// request volumes and mixed utilization (~20-40%), pushing the effective
/// embodied cost to the 10⁻⁵ to 10⁻³ g/req range depending on workload
/// density.
///
/// **Default `0.001` g/req is a conservative upper bound** for
/// lightly-loaded microservice servers. Users with measured infrastructure
/// data should override via `[green] embodied_carbon_per_request_gco2`
/// in the configuration. Full methodology documented in
/// `docs/design/05-GREENOPS-AND-CARBON.md`.
pub const DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2: f64 = 0.001;

/// Synthetic region label used when no region can be resolved for an event.
///
/// Events that fall into this bucket contribute to `total_io_ops` and to the
/// breakdown row but do NOT contribute to operational CO₂ (no carbon
/// intensity is known). A `tracing::debug!` is emitted by the scoring stage
/// when this bucket is non-empty so users can spot the misconfiguration
/// with `RUST_LOG=debug`. The breakdown row is the primary user-visible signal.
pub const UNKNOWN_REGION: &str = "unknown";

/// Status tag on a [`RegionBreakdown`] row: the region is present in the
/// embedded carbon table and contributes non-zero operational CO₂.
pub const REGION_STATUS_KNOWN: &str = "known";

/// Status tag on a [`RegionBreakdown`] row: the region name resolved (from
/// `event.cloud_region`, `[green.service_regions]`, or `default_region`) but
/// is **not** in the embedded carbon table. The row carries `io_ops` but
/// `co2_gco2 = 0.0` since no intensity is known.
pub const REGION_STATUS_OUT_OF_TABLE: &str = "out_of_table";

/// Status tag on a [`RegionBreakdown`] row: the synthetic `"unknown"` bucket
/// aggregating events where no region resolved at all. `co2_gco2 = 0.0`.
pub const REGION_STATUS_UNRESOLVED: &str = "unresolved";

/// CO₂ point estimate with a low/high multiplicative uncertainty interval.
///
/// Reported as part of [`CarbonReport`]. The `low`/`high` bounds reflect
/// **aggregate model uncertainty** (~2× multiplicative), not per-endpoint
/// variance. See [`CO2_LOW_FACTOR`] / [`CO2_HIGH_FACTOR`] for the framing.
///
/// The `methodology` field tags which SCI term this estimate represents:
/// - [`METHODOLOGY_SCI_NUMERATOR`] for the `total` field of [`CarbonReport`]
///   — the `(E × I) + M` numerator summed over traces.
/// - [`METHODOLOGY_OPERATIONAL_RATIO`] for the `avoidable` field — the
///   region-blind `operational × (avoidable/accounted)` approximation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CarbonEstimate {
    /// Lower bound of the multiplicative uncertainty interval (gCO₂eq).
    /// Equal to `mid × 0.5`.
    pub low: f64,
    /// Mid-point estimate (gCO₂eq). Best estimate at the model's central value.
    pub mid: f64,
    /// Upper bound of the multiplicative uncertainty interval (gCO₂eq).
    /// Equal to `mid × 2.0`.
    pub high: f64,
    /// Identifier of the estimation model (versioned for forward-compat
    /// as the model improves). Currently always [`CO2_MODEL`].
    pub model: &'static str,
    /// SCI v1.0 methodology tag: which term of the SCI formula this
    /// estimate represents. Either [`METHODOLOGY_SCI_NUMERATOR`] or
    /// [`METHODOLOGY_OPERATIONAL_RATIO`].
    pub methodology: &'static str,
}

impl CarbonEstimate {
    /// Private helper deriving `low`/`high` from a midpoint using the
    /// multiplicative uncertainty factors. Single source of truth for the
    /// two public constructors below.
    const fn new(mid: f64, methodology: &'static str) -> Self {
        Self {
            low: mid * CO2_LOW_FACTOR,
            mid,
            high: mid * CO2_HIGH_FACTOR,
            model: CO2_MODEL,
            methodology,
        }
    }

    /// Build a [`CarbonEstimate`] for the **SCI v1.0 numerator** `(E × I) + M`
    /// summed over all analyzed traces. Used for [`CarbonReport::total`].
    ///
    /// This is NOT the per-R intensity score. Consumers who need the
    /// SCI intensity compute `mid / analysis.traces_analyzed` themselves.
    #[must_use]
    pub const fn sci_numerator(mid: f64) -> Self {
        Self::new(mid, METHODOLOGY_SCI_NUMERATOR)
    }

    /// Build a [`CarbonEstimate`] for the **avoidable** estimate computed
    /// via the region-blind operational ratio
    /// `operational_gco2 × (avoidable_io_ops / accounted_io_ops)`.
    /// Used for [`CarbonReport::avoidable`].
    ///
    /// The methodology tag signals to downstream consumers that this
    /// value uses a global ratio (not per-region bucketing) and excludes
    /// the embodied carbon term by design.
    #[must_use]
    pub const fn operational_ratio(mid: f64) -> Self {
        Self::new(mid, METHODOLOGY_OPERATIONAL_RATIO)
    }
}

/// Structured carbon report aligned with the SCI v1.0 model.
///
/// Carries the per-run carbon estimate with two SCI-aligned views:
/// `total` is the SCI numerator `(E × I) + M` summed over analyzed traces,
/// `avoidable` is the region-blind operational ratio approximation.
/// Each estimate carries a 2× multiplicative uncertainty bracket.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarbonReport {
    /// Total estimated CO₂ (operational + embodied) with confidence interval.
    pub total: CarbonEstimate,
    /// Estimated CO₂ that could be saved by eliminating I/O waste.
    /// Excludes the embodied term (you can't optimize away manufactured
    /// silicon by fixing N+1 queries).
    pub avoidable: CarbonEstimate,
    /// SCI `O = E × I` term: operational emissions from running the workload.
    pub operational_gco2: f64,
    /// SCI `M` term: embodied hardware emissions amortized per request.
    /// Region-independent.
    pub embodied_gco2: f64,
}

/// Per-region operational CO₂ breakdown row.
///
/// Emitted in `green_summary.regions[]` when carbon scoring is enabled.
/// The [`Self::status`] field distinguishes three kinds of row:
///
/// - `"known"` ([`REGION_STATUS_KNOWN`]) — region is in the embedded carbon
///   table, `co2_gco2 > 0`, intensity and PUE populated.
/// - `"out_of_table"` ([`REGION_STATUS_OUT_OF_TABLE`]) — region name resolved
///   (from `cloud.region`, `service_regions`, or `default_region`) but is
///   not in the embedded table. `co2_gco2 = 0.0`, intensity and PUE are 0.
/// - `"unresolved"` ([`REGION_STATUS_UNRESOLVED`]) — synthetic [`UNKNOWN_REGION`]
///   bucket aggregating events whose region couldn't resolve at all.
///   `co2_gco2 = 0.0`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegionBreakdown {
    /// Status tag: `"known"` / `"out_of_table"` / `"unresolved"`. Lets
    /// downstream consumers distinguish the three kinds of zero-CO₂ row
    /// without string-matching on the region name.
    pub status: &'static str,
    /// Region identifier (lowercased). May be the synthetic
    /// [`UNKNOWN_REGION`] when no region resolves for some events.
    pub region: String,
    /// Grid carbon intensity in gCO₂eq per kWh from the embedded table,
    /// or `0.0` if the region is not in the table or is unknown.
    pub grid_intensity_gco2_kwh: f64,
    /// Power Usage Effectiveness for this region's cloud provider,
    /// or `0.0` if the region is not in the table or is unknown.
    pub pue: f64,
    /// Number of I/O ops attributed to this region.
    pub io_ops: usize,
    /// Operational CO₂ contribution from this region (gCO₂eq).
    /// Always `0.0` for unknown / out-of-table regions.
    pub co2_gco2: f64,
}

/// Configuration bundle passed to `score::score_green` for carbon scoring.
///
/// Owns its data so the scoring function doesn't need lifetime parameters.
/// Cloned once per analysis run from the parsed [`crate::config::Config`].
///
/// **Note on `Default`:** the derived `default()` yields
/// `embodied_per_request_gco2 = 0.0`, **not**
/// [`DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2`]. Real code paths build
/// `CarbonContext` from `Config`, which applies the correct default.
/// The `Default` impl is intended for ad-hoc test construction where the
/// caller explicitly sets the fields they care about.
#[derive(Debug, Clone, Default)]
pub struct CarbonContext {
    /// Fallback region used when neither the span's `cloud_region` attribute
    /// nor the per-service mapping resolves a region.
    pub default_region: Option<String>,
    /// Per-service region overrides for environments where `OTel`
    /// `cloud.region` is not set (e.g. `Jaeger`/`Zipkin` ingestion).
    ///
    /// Keys are lowercased at config load time; lookup is case-insensitive.
    pub service_regions: HashMap<String, String>,
    /// SCI `M` term: embodied carbon per request (per trace) in gCO₂eq.
    /// See [`DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2`].
    pub embodied_per_request_gco2: f64,
}

/// Resolve the effective region for a span event.
///
/// Resolution chain (first match wins):
/// 1. `event.cloud_region` (from `OTel` `cloud.region` attribute — most authoritative)
/// 2. `ctx.service_regions[event.service.to_lowercase()]` (per-service config override, case-insensitive)
/// 3. `ctx.default_region` (fallback)
///
/// Returns `None` if all three are absent — the caller (scoring stage)
/// buckets such events under [`UNKNOWN_REGION`].
#[must_use]
pub fn resolve_region<'a>(event: &'a SpanEvent, ctx: &'a CarbonContext) -> Option<&'a str> {
    if let Some(region) = event.cloud_region.as_deref() {
        return Some(region);
    }
    // N3: short-circuit when the service_regions map is empty (mono-region
    // common case). Skips the per-span `to_ascii_lowercase` allocation that
    // would otherwise happen on every probe with no chance of hitting.
    if !ctx.service_regions.is_empty()
        && let Some(region) = ctx.service_regions.get(&event.service.to_ascii_lowercase())
    {
        return Some(region.as_str());
    }
    ctx.default_region.as_deref()
}

/// Validate a region identifier (`OTel` `cloud.region` attribute value or
/// a config-provided region key).
///
/// Acceptance rule: **ASCII alphanumeric + `-` + `_`, length 1-64**.
/// Covers all cloud-provider region naming conventions (`eu-west-3`,
/// `us-east-1`, `europe-west9`, `francecentral`) and ISO country codes
/// (`fr`, `de`, `us`) while rejecting control characters (log-forging
/// protection), spaces, and oversized inputs (memory-exhaustion
/// protection).
///
/// Used at the OTLP ingestion boundary (fail-silent: invalid values are
/// replaced with `None`) and at config load time (fail-loud: invalid
/// values cause a config error).
#[must_use]
pub(crate) fn is_valid_region_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Cloud provider identifier for PUE lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    Aws,
    Gcp,
    Azure,
    Generic,
}

impl Provider {
    /// Power Usage Effectiveness for this provider.
    const fn pue(self) -> f64 {
        match self {
            Self::Aws => 1.135,
            Self::Gcp => 1.10,
            Self::Azure => 1.185,
            Self::Generic => 1.2,
        }
    }
}

/// Static carbon intensity table: (`region_key`, gCO₂eq/kWh, provider).
///
/// Region keys are lowercase for case-insensitive matching.
/// Data from CCF and Electricity Maps (2023-2024 annual averages).
static CARBON_TABLE: &[(&str, f64, Provider)] = &[
    // AWS regions
    ("us-east-1", 379.0, Provider::Aws),
    ("us-east-2", 410.0, Provider::Aws),
    ("us-west-1", 200.0, Provider::Aws),
    ("us-west-2", 89.0, Provider::Aws),
    ("eu-west-1", 296.0, Provider::Aws),      // Ireland
    ("eu-west-2", 231.0, Provider::Aws),      // London
    ("eu-west-3", 56.0, Provider::Aws),       // Paris
    ("eu-central-1", 338.0, Provider::Aws),   // Frankfurt
    ("eu-north-1", 8.0, Provider::Aws),       // Stockholm
    ("ap-northeast-1", 462.0, Provider::Aws), // Tokyo
    ("ap-southeast-1", 408.0, Provider::Aws), // Singapore
    ("ap-southeast-2", 550.0, Provider::Aws), // Sydney
    ("ap-south-1", 708.0, Provider::Aws),     // Mumbai
    ("ca-central-1", 13.0, Provider::Aws),    // Canada
    ("sa-east-1", 62.0, Provider::Aws),       // São Paulo
    // GCP regions
    ("us-central1", 426.0, Provider::Gcp),
    ("us-east1", 379.0, Provider::Gcp),
    ("us-west1", 89.0, Provider::Gcp),
    ("europe-west1", 187.0, Provider::Gcp),    // Belgium
    ("europe-west4", 328.0, Provider::Gcp),    // Netherlands
    ("europe-west9", 56.0, Provider::Gcp),     // Paris
    ("europe-north1", 8.0, Provider::Gcp),     // Finland
    ("asia-northeast1", 462.0, Provider::Gcp), // Tokyo
    // Azure regions
    ("eastus", 379.0, Provider::Azure),
    ("westus2", 89.0, Provider::Azure),
    ("westeurope", 328.0, Provider::Azure),  // Netherlands
    ("northeurope", 296.0, Provider::Azure), // Ireland
    ("francecentral", 56.0, Provider::Azure),
    ("uksouth", 231.0, Provider::Azure),
    // Country / ISO codes (generic PUE)
    ("fr", 56.0, Provider::Generic),
    ("de", 338.0, Provider::Generic),
    ("gb", 231.0, Provider::Generic),
    ("uk", 231.0, Provider::Generic),
    ("us", 379.0, Provider::Generic),
    ("ie", 296.0, Provider::Generic),
    ("se", 8.0, Provider::Generic),
    ("no", 7.0, Provider::Generic),
    ("ca", 13.0, Provider::Generic),
    ("jp", 462.0, Provider::Generic),
    ("in", 708.0, Provider::Generic),
    ("au", 550.0, Provider::Generic),
    ("br", 62.0, Provider::Generic),
    ("sg", 408.0, Provider::Generic),
    ("nl", 328.0, Provider::Generic),
    ("be", 187.0, Provider::Generic),
    ("fi", 8.0, Provider::Generic),
];

/// Pre-built map for O(1) region lookup (keys are lowercase).
static REGION_MAP: std::sync::LazyLock<HashMap<&'static str, (f64, Provider)>> =
    std::sync::LazyLock::new(|| {
        CARBON_TABLE
            .iter()
            .map(|&(key, intensity, provider)| (key, (intensity, provider)))
            .collect()
    });

/// Look up carbon intensity for a region string.
///
/// Returns `(carbon_intensity_gco2_per_kwh, pue)` if the region is found.
/// Matching is case-insensitive (input is lowercased before lookup).
#[must_use]
pub fn lookup_region(region: &str) -> Option<(f64, f64)> {
    let lower = region.to_ascii_lowercase();
    lookup_region_lower(&lower)
}

/// Look up carbon intensity for a **pre-lowercased** region string.
///
/// Use this when the caller has already lowercased the region to avoid
/// a redundant allocation. Exposed as `pub(crate)` for the scoring stage,
/// which lowercases regions once when bucketing and then probes multiple times.
#[must_use]
pub(crate) fn lookup_region_lower(region: &str) -> Option<(f64, f64)> {
    REGION_MAP
        .get(region)
        .map(|(intensity, provider)| (*intensity, provider.pue()))
}

/// Compute operational CO₂ in gCO₂eq from raw I/O operation count, grid
/// carbon intensity, and provider PUE.
///
/// Single source of truth for the formula
/// `gCO₂eq = io_ops × ENERGY_PER_IO_OP_KWH × carbon_intensity × PUE`,
/// used by both [`io_ops_to_co2_grams`] (public convenience) and the
/// multi-region scoring stage in `score::compute_carbon_report`.
#[must_use]
pub(crate) fn compute_operational_gco2(io_ops: usize, intensity: f64, pue: f64) -> f64 {
    io_ops as f64 * ENERGY_PER_IO_OP_KWH * intensity * pue
}

/// Convert I/O operations to estimated gCO₂eq for a **pre-lowercased** region.
///
/// Formula: `gCO₂eq = io_ops × ENERGY_PER_IO_OP_KWH × carbon_intensity × PUE`
/// (see [`compute_operational_gco2`]).
///
/// Returns `None` if the region is not recognized.
#[must_use]
pub fn io_ops_to_co2_grams(io_ops: usize, region: &str) -> Option<f64> {
    let (intensity, pue) = lookup_region_lower(region)?;
    Some(compute_operational_gco2(io_ops, intensity, pue))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_aws_region() {
        let result = lookup_region("eu-west-3");
        assert!(result.is_some());
        let (intensity, pue) = result.unwrap();
        assert!((intensity - 56.0).abs() < f64::EPSILON);
        assert!((pue - 1.135).abs() < f64::EPSILON);
    }

    #[test]
    fn lookup_known_gcp_region() {
        let result = lookup_region("europe-west9");
        assert!(result.is_some());
        let (intensity, pue) = result.unwrap();
        assert!((intensity - 56.0).abs() < f64::EPSILON);
        assert!((pue - 1.10).abs() < f64::EPSILON);
    }

    #[test]
    fn lookup_country_code() {
        let result = lookup_region("FR");
        assert!(result.is_some());
        let (intensity, pue) = result.unwrap();
        assert!((intensity - 56.0).abs() < f64::EPSILON);
        assert!((pue - 1.2).abs() < f64::EPSILON);
    }

    #[test]
    fn lookup_case_insensitive() {
        assert!(lookup_region("EU-WEST-3").is_some());
        assert!(lookup_region("Us-East-1").is_some());
        assert!(lookup_region("fr").is_some());
        assert!(lookup_region("FR").is_some());
    }

    #[test]
    fn lookup_unknown_region_returns_none() {
        assert!(lookup_region("unknown-region").is_none());
        assert!(lookup_region("").is_none());
    }

    #[test]
    fn io_ops_to_co2_known_region() {
        let co2 = io_ops_to_co2_grams(1000, "eu-west-3");
        assert!(co2.is_some());
        let val = co2.unwrap();
        // 1000 * 0.0000001 * 56.0 * 1.135 = 0.006356
        assert!((val - 0.006_356).abs() < 1e-9);
    }

    #[test]
    fn io_ops_to_co2_unknown_region() {
        assert!(io_ops_to_co2_grams(1000, "mars-1").is_none());
    }

    #[test]
    fn io_ops_to_co2_zero_ops() {
        let co2 = io_ops_to_co2_grams(0, "eu-west-3");
        assert!(co2.is_some());
        assert!((co2.unwrap() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn high_carbon_region_vs_low() {
        let high = io_ops_to_co2_grams(1000, "ap-south-1").unwrap(); // India, 708
        let low = io_ops_to_co2_grams(1000, "eu-north-1").unwrap(); // Stockholm, 8
        assert!(high > low * 10.0, "India should be much higher than Sweden");
    }

    #[test]
    fn lookup_azure_region() {
        let result = lookup_region("eastus");
        assert!(result.is_some());
        let (_, pue) = result.unwrap();
        assert!(
            (pue - 1.185).abs() < f64::EPSILON,
            "Azure PUE should be 1.185"
        );
    }

    // ----- Phase 5a: CarbonEstimate / CarbonReport / resolve_region tests -----

    use crate::event::{EventSource, EventType, SpanEvent};

    fn make_event(service: &str, cloud_region: Option<&str>) -> SpanEvent {
        SpanEvent {
            timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            trace_id: "trace-1".to_string(),
            span_id: "span-1".to_string(),
            parent_span_id: None,
            service: service.to_string(),
            cloud_region: cloud_region.map(str::to_string),
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: "SELECT 1".to_string(),
            duration_us: 1000,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::method".to_string(),
            },
            status_code: None,
        }
    }

    #[test]
    fn carbon_estimate_sci_numerator_labels() {
        let est = CarbonEstimate::sci_numerator(0.000_100);
        assert!((est.mid - 0.000_100).abs() < f64::EPSILON);
        assert!((est.low - 0.000_050).abs() < f64::EPSILON);
        assert!((est.high - 0.000_200).abs() < f64::EPSILON);
        assert_eq!(est.model, "io_proxy_v1");
        assert_eq!(est.methodology, "sci_v1_numerator");
    }

    #[test]
    fn carbon_estimate_operational_ratio_labels() {
        let est = CarbonEstimate::operational_ratio(0.000_050);
        assert!((est.mid - 0.000_050).abs() < f64::EPSILON);
        assert!((est.low - 0.000_025).abs() < f64::EPSILON);
        assert!((est.high - 0.000_100).abs() < f64::EPSILON);
        assert_eq!(est.model, "io_proxy_v1");
        assert_eq!(est.methodology, "sci_v1_operational_ratio");
    }

    #[test]
    fn carbon_estimate_methodology_constants_are_distinct() {
        assert_ne!(METHODOLOGY_SCI_NUMERATOR, METHODOLOGY_OPERATIONAL_RATIO);
        assert_eq!(METHODOLOGY_SCI_NUMERATOR, "sci_v1_numerator");
        assert_eq!(METHODOLOGY_OPERATIONAL_RATIO, "sci_v1_operational_ratio");
    }

    #[test]
    fn carbon_estimate_from_zero_midpoint() {
        let est = CarbonEstimate::sci_numerator(0.0);
        assert!((est.low - 0.0).abs() < f64::EPSILON);
        assert!((est.mid - 0.0).abs() < f64::EPSILON);
        assert!((est.high - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn confidence_interval_factors_are_2x_multiplicative() {
        // The constants encode a 2× multiplicative uncertainty bracket
        // (not a symmetric ±50% window): low = mid/2, high = mid×2.
        // The geometric mean of low and high equals mid, making the
        // interval log-symmetric around the midpoint.
        let mid = 12.34_f64;
        let est = CarbonEstimate::sci_numerator(mid);
        assert!((est.low - mid * CO2_LOW_FACTOR).abs() < f64::EPSILON);
        assert!((est.high - mid * CO2_HIGH_FACTOR).abs() < f64::EPSILON);
        assert!((CO2_LOW_FACTOR - 0.5).abs() < f64::EPSILON);
        assert!((CO2_HIGH_FACTOR - 2.0).abs() < f64::EPSILON);
        // Geometric mean of low and high ≈ mid (log-symmetric).
        let geo_mean = (est.low * est.high).sqrt();
        assert!((geo_mean - mid).abs() < 1e-9);
    }

    #[test]
    fn compute_operational_gco2_matches_expected() {
        // Hand-computed: 1000 ops × 1e-7 kWh × 56 gCO₂/kWh × 1.135 PUE = 6.356e-3 g
        let result = compute_operational_gco2(1000, 56.0, 1.135);
        assert!((result - 0.006_356).abs() < 1e-9);
    }

    #[test]
    fn compute_operational_gco2_zero_ops() {
        assert!((compute_operational_gco2(0, 56.0, 1.135) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn io_ops_to_co2_grams_delegates_to_helper() {
        // Cross-check: the public scalar API and the internal helper
        // must produce the same result for the same inputs.
        let scalar = io_ops_to_co2_grams(1000, "eu-west-3").unwrap();
        let (intensity, pue) = lookup_region_lower("eu-west-3").unwrap();
        let helper = compute_operational_gco2(1000, intensity, pue);
        assert!((scalar - helper).abs() < f64::EPSILON);
    }

    #[test]
    fn is_valid_region_id_accepts_valid() {
        assert!(is_valid_region_id("eu-west-3"));
        assert!(is_valid_region_id("us-east-1"));
        assert!(is_valid_region_id("europe-west9"));
        assert!(is_valid_region_id("francecentral"));
        assert!(is_valid_region_id("fr"));
        assert!(is_valid_region_id("unknown"));
        assert!(is_valid_region_id("mars-1"));
        assert!(is_valid_region_id("my_region_42"));
    }

    #[test]
    fn is_valid_region_id_rejects_invalid() {
        assert!(!is_valid_region_id(""), "empty string");
        assert!(!is_valid_region_id(&"a".repeat(65)), "too long");
        assert!(!is_valid_region_id("eu west 3"), "space");
        assert!(!is_valid_region_id("eu.west.3"), "dot");
        assert!(!is_valid_region_id("eu/west/3"), "slash");
        assert!(!is_valid_region_id("eu-west-3\n"), "newline");
        assert!(!is_valid_region_id("eu-west-3\0"), "null byte");
        assert!(!is_valid_region_id("région"), "non-ASCII");
    }

    #[test]
    fn is_valid_region_id_accepts_exact_64_chars() {
        let max_len = "a".repeat(64);
        assert!(is_valid_region_id(&max_len));
    }

    #[test]
    fn resolve_region_prefers_event_attribute() {
        let mut service_regions = HashMap::new();
        service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            embodied_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
        };
        let event = make_event("order-svc", Some("ap-south-1"));
        assert_eq!(resolve_region(&event, &ctx), Some("ap-south-1"));
    }

    #[test]
    fn resolve_region_falls_back_to_service_map() {
        let mut service_regions = HashMap::new();
        service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions,
            embodied_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
        };
        let event = make_event("order-svc", None);
        assert_eq!(resolve_region(&event, &ctx), Some("us-east-1"));
    }

    #[test]
    fn resolve_region_falls_back_to_default() {
        let ctx = CarbonContext {
            default_region: Some("eu-west-3".to_string()),
            service_regions: HashMap::new(),
            embodied_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
        };
        let event = make_event("unknown-svc", None);
        assert_eq!(resolve_region(&event, &ctx), Some("eu-west-3"));
    }

    #[test]
    fn resolve_region_returns_none_when_all_unset() {
        let ctx = CarbonContext::default();
        let event = make_event("any-svc", None);
        assert_eq!(resolve_region(&event, &ctx), None);
    }

    #[test]
    fn resolve_region_service_map_does_not_shadow_event_attribute() {
        // Even if the service has a config override, the span's own
        // cloud.region should win (most authoritative source).
        let mut service_regions = HashMap::new();
        service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
        let ctx = CarbonContext {
            default_region: None,
            service_regions,
            embodied_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
        };
        let event = make_event("order-svc", Some("eu-north-1"));
        assert_eq!(resolve_region(&event, &ctx), Some("eu-north-1"));
    }

    #[test]
    fn resolve_region_service_map_is_case_insensitive() {
        // Config loader lowercases service_regions keys. Incoming span
        // events may carry mixed-case service names (e.g. "Order-Svc" from
        // an older .NET SDK). resolve_region lowercases event.service
        // before lookup so they still match.
        let mut service_regions = HashMap::new();
        service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
        let ctx = CarbonContext {
            default_region: None,
            service_regions,
            embodied_per_request_gco2: 0.0,
        };
        // Mixed-case service name on the event — should still match.
        let event = make_event("Order-Svc", None);
        assert_eq!(resolve_region(&event, &ctx), Some("us-east-1"));
        // Upper-case service name.
        let event_upper = make_event("ORDER-SVC", None);
        assert_eq!(resolve_region(&event_upper, &ctx), Some("us-east-1"));
    }
}
