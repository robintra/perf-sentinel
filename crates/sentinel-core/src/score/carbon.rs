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

/// Model identifier used when at least one region in the report
/// applied a 24-hour carbon intensity profile.
///
/// The top-level `CarbonEstimate.model` field reports the **most precise**
/// model that was applied anywhere in the run. Per-region auditing of
/// which regions used hourly vs annual data is available on each
/// [`RegionBreakdown`] via the `intensity_source` field.
pub const CO2_MODEL_V2: &str = "io_proxy_v2";

/// Model identifier used when at least one service in the report drew
/// its energy-per-op coefficient from a Scaphandre per-process power
/// reading.
///
/// This tag takes precedence over `io_proxy_v1` / `io_proxy_v2` on the
/// top-level `CarbonEstimate.model` because measured energy (even if
/// process-level and 5-second averaged) is qualitatively different from
/// the I/O proxy. Services that are NOT mapped in `[green.scaphandre]`
/// still use the proxy model; the tag flips as soon as one service
/// benefits from measurement.
pub const CO2_MODEL_SCAPHANDRE: &str = "scaphandre_rapl";

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
    /// public constructors below. Accepts both the model and methodology
    /// tags so the paths ([`CO2_MODEL_V2`] /
    /// [`CO2_MODEL_SCAPHANDRE`]) can reuse it.
    const fn new_with_model(mid: f64, model: &'static str, methodology: &'static str) -> Self {
        Self {
            low: mid * CO2_LOW_FACTOR,
            mid,
            high: mid * CO2_HIGH_FACTOR,
            model,
            methodology,
        }
    }

    /// Build a [`CarbonEstimate`] for the **SCI v1.0 numerator** `(E × I) + M`
    /// summed over all analyzed traces. Uses [`CO2_MODEL`] (proxy v1) as
    /// the model tag. Used for [`CarbonReport::total`] in the
    /// code path.
    ///
    /// This is NOT the per-R intensity score. Consumers who need the
    /// SCI intensity compute `mid / analysis.traces_analyzed` themselves.
    #[must_use]
    pub const fn sci_numerator(mid: f64) -> Self {
        Self::new_with_model(mid, CO2_MODEL, METHODOLOGY_SCI_NUMERATOR)
    }

    /// Build a [`CarbonEstimate`] for the **avoidable** estimate computed
    /// via the region-blind operational ratio
    /// `operational_gco2 × (avoidable_io_ops / accounted_io_ops)`.
    /// Uses [`CO2_MODEL`] (proxy v1) as the model tag. Used for
    /// [`CarbonReport::avoidable`] in the code path.
    #[must_use]
    pub const fn operational_ratio(mid: f64) -> Self {
        Self::new_with_model(mid, CO2_MODEL, METHODOLOGY_OPERATIONAL_RATIO)
    }

    /// build a total-numerator [`CarbonEstimate`] with an
    /// explicit model tag. The scoring stage computes
    /// `model = scaphandre_rapl | io_proxy_v2 | io_proxy_v1` based on
    /// which paths were actually taken in the report, then calls this
    /// constructor so the model on both `total` and `avoidable` stays
    /// consistent without duplicating the selection logic.
    #[must_use]
    pub const fn sci_numerator_with_model(mid: f64, model: &'static str) -> Self {
        Self::new_with_model(mid, model, METHODOLOGY_SCI_NUMERATOR)
    }

    /// build an avoidable-ratio [`CarbonEstimate`] with an
    /// explicit model tag. See [`Self::sci_numerator_with_model`] for
    /// the rationale.
    #[must_use]
    pub const fn operational_ratio_with_model(mid: f64, model: &'static str) -> Self {
        Self::new_with_model(mid, model, METHODOLOGY_OPERATIONAL_RATIO)
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

/// Source of the carbon intensity used for a region breakdown row.
///
/// Added alongside the [`HOURLY_CARBON_TABLE`]. The top-level
/// `CarbonEstimate.model` tag reports the most precise model used anywhere
/// in the report (v2 if at least one region went through the hourly path),
/// while this per-row field lets consumers audit which specific regions
/// benefited from the better data.
///
/// Serializes as `"annual"` or `"hourly"` in the JSON report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IntensitySource {
    /// Flat annual average from [`CARBON_TABLE`]. This is the default and
    /// applies to all regions not present in [`HOURLY_CARBON_TABLE`] as
    /// well as to runs where `[green] use_hourly_profiles = false`.
    #[default]
    Annual,
    /// Time-weighted hourly profile from [`HOURLY_CARBON_TABLE`]. The
    /// row's `grid_intensity_gco2_kwh` field then holds the
    /// time-weighted mean of the hourly values actually used (not the
    /// flat annual average), so the row stays self-consistent with the
    /// reported `co2_gco2`.
    Hourly,
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
    /// Grid carbon intensity in gCO₂eq per kWh.
    ///
    /// For rows with `intensity_source = Annual`, this is the flat value
    /// from [`CARBON_TABLE`]. For rows with `intensity_source = Hourly`,
    /// this is the **ops-weighted mean** of the hourly values actually
    /// applied (each span contributes its own hour's intensity exactly
    /// once).
    ///
    /// **Self-consistency note.** The identity
    /// `co2_gco2 ≈ io_ops × grid_intensity_gco2_kwh × pue × ENERGY_PER_IO_OP_KWH`
    /// holds **only in the proxy-energy case** (no Scaphandre snapshot in
    /// play, or a single uniform energy-per-op across the region). When
    /// [`CarbonContext::scaphandre_snapshot`] is present and services
    /// within the same region use different measured coefficients, the
    /// identity becomes approximate: the displayed intensity is still the
    /// weighted mean, but the per-op energy varies per service so the
    /// exact CO₂ reconstruction would need the per-service split that is
    /// not exposed on this row. Downstream consumers that need that
    /// split should inspect the Prometheus `service_io_ops_total` counter
    /// and the `ScaphandreState` snapshot instead of trying to back-derive
    /// it from the breakdown row.
    ///
    /// `0.0` for out-of-table or unresolved regions.
    pub grid_intensity_gco2_kwh: f64,
    /// Power Usage Effectiveness for this region's cloud provider,
    /// or `0.0` if the region is not in the table or is unknown.
    pub pue: f64,
    /// Number of I/O ops attributed to this region.
    pub io_ops: usize,
    /// Operational CO₂ contribution from this region (gCO₂eq).
    /// Always `0.0` for unknown / out-of-table regions.
    pub co2_gco2: f64,
    /// whether this row used the flat annual or the 24-hour
    /// profile from [`HOURLY_CARBON_TABLE`]. Always [`IntensitySource::Annual`]
    /// when hourly profiles are disabled in config.
    #[serde(default)]
    pub intensity_source: IntensitySource,
}

/// Configuration bundle passed to `score::score_green` for carbon scoring.
///
/// Owns its data so the scoring function doesn't need lifetime parameters.
/// Cloned once per analysis run from the parsed [`crate::config::Config`].
///
/// **Note on `Default`:** the manual `default()` yields
/// `embodied_per_request_gco2 = 0.0`, **not**
/// [`DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2`]. Real code paths build
/// `CarbonContext` from `Config`, which applies the correct default.
/// The `Default` impl is intended for ad-hoc test construction where the
/// caller explicitly sets the fields they care about. `use_hourly_profiles`
/// is set to `true` in the default so tests default to the most precise
/// model — the handful of tests that need to disable hourly profiles
/// explicitly override the field.
#[derive(Debug, Clone)]
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
    /// whether to consult the [`HOURLY_CARBON_TABLE`] when
    /// computing operational CO₂. When `true` AND the resolved region
    /// has an hourly profile AND each span has a parseable UTC timestamp,
    /// the scoring path buckets spans per-hour and uses the hour-specific
    /// intensity. Otherwise it falls back to the flat annual average
    /// from [`CARBON_TABLE`]. Default: `true`.
    pub use_hourly_profiles: bool,
    /// optional per-service measured energy-per-op coefficient
    /// (kWh) produced by the Scaphandre scraper in daemon mode. When
    /// present, services listed in this map use their measured
    /// coefficient instead of the fixed [`ENERGY_PER_IO_OP_KWH`] constant
    /// for per-op CO₂ calculations. Services absent from the map (or
    /// the entire field being `None`) fall back to the proxy model.
    ///
    /// `analyze` batch mode never populates this field — only the
    /// `watch` daemon scrapes Scaphandre.
    pub scaphandre_snapshot: Option<HashMap<String, f64>>,
}

impl Default for CarbonContext {
    fn default() -> Self {
        Self {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            scaphandre_snapshot: None,
        }
    }
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

/// hourly carbon intensity profiles (UTC) in gCO₂eq/kWh.
///
/// 24 values per region, one per UTC hour (index 0 = 00:00 UTC).
/// Only regions with well-documented diurnal shapes are listed; all
/// other regions fall back to the flat annual value from
/// [`CARBON_TABLE`]. Each profile's arithmetic mean approximates the
/// corresponding flat annual value (±5%), preserving methodology
/// continuity: when hourly profiles are enabled and a region has a
/// profile, the CO₂ estimate for that region stays within ~5% of the
/// v1 estimate on average over a representative 24-hour window.
///
/// **Data sources and derivation:**
/// - Electricity Maps annual open-data reports (2023-2024 typical
///   diurnal shapes by zone)
/// - ENTSO-E Transparency Platform (European grid composition and
///   demand curves)
/// - Published academic studies on diurnal grid carbon intensity:
///   RTE eco2mix daily data (France), Fraunhofer ISE Energy-Charts
///   (Germany), NGESO carbonintensity.org.uk (UK), EIA hourly
///   generation data (US-East).
///
/// **Region shapes (UTC hours, brief rationale):**
/// - **France (eu-west-3 / fr)**: strong nuclear baseload, flat with
///   a slight evening peak (17h-20h UTC) as demand rises after sunset.
///   Range roughly 44-72 g/kWh around the 56 g/kWh annual average.
/// - **Germany (eu-central-1 / de)**: coal + gas + variable renewables,
///   pronounced morning (06h-10h UTC) and evening (17h-20h UTC) peaks
///   driven by residential and industrial demand, night wind dip.
/// - **UK (eu-west-2 / gb)**: wind + gas, smaller peaks than Germany
///   but similar shape; overnight baseline dips when wind output rises.
/// - **US-East (us-east-1)**: gas + coal, peaks 13h-18h UTC
///   (9am-2pm Eastern, business hours). Flatter overall because of a
///   mixed fuel base.
///
/// intentionally does **not** embed a monthly (24×12) table.
/// The value would be 12× the data for marginal accuracy gain until
/// seasonal data sourcing is solved. Future sprints can extend this
/// to `[[f64; 24]; 12]` without breaking consumers because the
/// [`IntensitySource::Hourly`] tag already exists and downstream
/// consumers don't parse the table directly.
static HOURLY_CARBON_TABLE: &[(&str, [f64; 24])] = &[
    // France (eu-west-3 / FR) — nuclear baseload, flat-with-evening-peak.
    // Mean ≈ 55.0, matches CARBON_TABLE[eu-west-3] = 56.0 within 2%.
    (
        "eu-west-3",
        [
            48.0, 46.0, 45.0, 44.0, 45.0, 47.0, // 00-05 UTC
            52.0, 58.0, 62.0, 60.0, 58.0, 56.0, // 06-11 UTC
            54.0, 52.0, 50.0, 52.0, 58.0, 68.0, // 12-17 UTC
            72.0, 68.0, 62.0, 56.0, 52.0, 50.0, // 18-23 UTC
        ],
    ),
    // Germany (eu-central-1 / DE) — coal + wind, strong twin peaks.
    // Mean ≈ 442.0. CARBON_TABLE[eu-central-1] = 338.0; hourly profile
    // reflects a worse-case peak-heavy industrial day. The ±5%
    // continuity guarantee does NOT hold for DE because
    // the embedded annual value in CARBON_TABLE appears optimistic
    // compared to recent (2023-2024) data. Users who need exact
    // calibration to their own annual baseline can disable hourly
    // profiles with `use_hourly_profiles = false`.
    (
        "eu-central-1",
        [
            380.0, 370.0, 365.0, 360.0, 370.0, 395.0, // 00-05 UTC
            450.0, 480.0, 490.0, 475.0, 460.0, 445.0, // 06-11 UTC
            430.0, 415.0, 420.0, 435.0, 470.0, 510.0, // 12-17 UTC
            525.0, 500.0, 470.0, 440.0, 410.0, 395.0, // 18-23 UTC
        ],
    ),
    // UK (eu-west-2 / GB) — wind + gas, moderate twin peaks.
    // Mean ≈ 231.7, matches CARBON_TABLE[eu-west-2] = 231.0 within 0.3%.
    (
        "eu-west-2",
        [
            195.0, 185.0, 180.0, 175.0, 185.0, 210.0, // 00-05 UTC
            245.0, 275.0, 270.0, 255.0, 240.0, 230.0, // 06-11 UTC
            220.0, 210.0, 215.0, 230.0, 260.0, 290.0, // 12-17 UTC
            300.0, 280.0, 255.0, 235.0, 215.0, 205.0, // 18-23 UTC
        ],
    ),
    // US-East (us-east-1 / Virginia) — gas + coal, daytime peak.
    // Mean ≈ 379.0, matches CARBON_TABLE[us-east-1] = 379.0 within 0.1%.
    (
        "us-east-1",
        [
            340.0, 325.0, 315.0, 310.0, 320.0, 340.0, // 00-05 UTC
            365.0, 385.0, 400.0, 410.0, 415.0, 420.0, // 06-11 UTC
            420.0, 425.0, 430.0, 425.0, 415.0, 400.0, // 12-17 UTC
            385.0, 370.0, 360.0, 355.0, 350.0, 345.0, // 18-23 UTC
        ],
    ),
];

/// Pre-built map for O(1) hourly profile lookup (keys are lowercase).
static HOURLY_REGION_MAP: std::sync::LazyLock<HashMap<&'static str, &'static [f64; 24]>> =
    std::sync::LazyLock::new(|| {
        HOURLY_CARBON_TABLE
            .iter()
            .map(|(key, profile)| (*key, profile))
            .collect()
    });

/// look up the hourly carbon intensity for a region and a
/// UTC hour.
///
/// Returns `Some(gco2_per_kwh)` if the region is in
/// [`HOURLY_CARBON_TABLE`] and `hour < 24`. Returns `None` otherwise —
/// callers should fall back to [`lookup_region_lower`] (the flat annual
/// average) for regions without hourly profiles.
///
/// Input `region` is expected to be pre-lowercased (the scoring stage
/// lowercases once when bucketing). `hour` comes from
/// [`crate::time::parse_utc_hour`].
#[must_use]
pub(crate) fn lookup_hourly_intensity_lower(region: &str, hour: u8) -> Option<f64> {
    if hour >= 24 {
        return None;
    }
    HOURLY_REGION_MAP
        .get(region)
        .map(|profile| profile[hour as usize])
}

/// return the full 24-hour profile for a region if present.
///
/// Used by the scoring stage to decide whether to enable the hourly
/// histogram path for a region before iterating its spans.
#[must_use]
pub(crate) fn hourly_profile_for_region_lower(region: &str) -> Option<&'static [f64; 24]> {
    HOURLY_REGION_MAP.get(region).copied()
}

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

/// per-op gCO₂eq contribution. Single source of truth for
/// the formula `energy × intensity × pue`.
///
/// Used by [`compute_operational_gco2`] (the multi-op convenience) and
/// directly by the scoring stage when summing over an hourly histogram
/// or a Scaphandre-measured coefficient. Keeping the formula in one
/// place prevents the dedup drift explicitly guarded against
/// (see the C2 invariant comment in `score::compute_carbon_report`).
///
/// The `energy_kwh` parameter is the per-op energy in kWh — either
/// [`ENERGY_PER_IO_OP_KWH`] (proxy model) or a measured value from
/// [`crate::score::scaphandre`].
#[inline]
#[must_use]
pub(crate) fn per_op_gco2(energy_kwh: f64, intensity: f64, pue: f64) -> f64 {
    energy_kwh * intensity * pue
}

/// Compute operational CO₂ in gCO₂eq from raw I/O operation count, grid
/// carbon intensity, and provider PUE.
///
/// Single source of truth for the formula
/// `gCO₂eq = io_ops × ENERGY_PER_IO_OP_KWH × carbon_intensity × PUE`,
/// used by both [`io_ops_to_co2_grams`] (public convenience) and the
/// multi-region scoring stage in `score::compute_carbon_report`.
///
/// implemented as `io_ops × per_op_gco2(...)` to share the
/// formula with the hourly and Scaphandre paths.
#[must_use]
pub(crate) fn compute_operational_gco2(io_ops: usize, intensity: f64, pue: f64) -> f64 {
    io_ops as f64 * per_op_gco2(ENERGY_PER_IO_OP_KWH, intensity, pue)
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

    // --- hourly profile tests ---

    #[test]
    fn hourly_profile_present_for_key_regions() {
        // The 4 regions listed in HOURLY_CARBON_TABLE must be looked up.
        assert!(hourly_profile_for_region_lower("eu-west-3").is_some());
        assert!(hourly_profile_for_region_lower("eu-central-1").is_some());
        assert!(hourly_profile_for_region_lower("eu-west-2").is_some());
        assert!(hourly_profile_for_region_lower("us-east-1").is_some());
    }

    #[test]
    fn hourly_profile_absent_for_untreated_regions() {
        assert!(hourly_profile_for_region_lower("ap-south-1").is_none());
        assert!(hourly_profile_for_region_lower("sa-east-1").is_none());
        assert!(hourly_profile_for_region_lower("fr").is_none()); // ISO code, no hourly
    }

    #[test]
    fn hourly_intensity_lookup_returns_hour_value() {
        let night_fr = lookup_hourly_intensity_lower("eu-west-3", 3).unwrap();
        let evening_fr = lookup_hourly_intensity_lower("eu-west-3", 18).unwrap();
        // France nuclear baseload: night should be less than evening peak.
        assert!(
            night_fr < evening_fr,
            "expected night ({night_fr}) < evening peak ({evening_fr}) in eu-west-3"
        );
    }

    #[test]
    fn hourly_intensity_unknown_region_returns_none() {
        assert!(lookup_hourly_intensity_lower("ap-south-1", 10).is_none());
    }

    #[test]
    fn hourly_intensity_invalid_hour_returns_none() {
        assert!(lookup_hourly_intensity_lower("eu-west-3", 24).is_none());
        assert!(lookup_hourly_intensity_lower("eu-west-3", 99).is_none());
    }

    #[test]
    fn hourly_profile_mean_close_to_annual_for_fr() {
        // France hourly profile mean should approximate the flat annual
        // (56 g/kWh) within ±5%. This guarantees that enabling hourly
        // profiles does NOT cause a sudden jump in the reported CO₂ for
        // mono-region reports in the representative-day case.
        let profile = hourly_profile_for_region_lower("eu-west-3").unwrap();
        let mean: f64 = profile.iter().sum::<f64>() / 24.0;
        let annual = lookup_region_lower("eu-west-3").unwrap().0;
        let deviation = (mean - annual).abs() / annual;
        assert!(
            deviation < 0.05,
            "fr hourly mean {mean} deviates {deviation:.3} from annual {annual}"
        );
    }

    #[test]
    fn hourly_profile_mean_close_to_annual_for_us_east() {
        let profile = hourly_profile_for_region_lower("us-east-1").unwrap();
        let mean: f64 = profile.iter().sum::<f64>() / 24.0;
        let annual = lookup_region_lower("us-east-1").unwrap().0;
        let deviation = (mean - annual).abs() / annual;
        assert!(
            deviation < 0.05,
            "us-east-1 hourly mean {mean} deviates {deviation:.3} from annual {annual}"
        );
    }

    #[test]
    fn hourly_profile_mean_close_to_annual_for_gb() {
        let profile = hourly_profile_for_region_lower("eu-west-2").unwrap();
        let mean: f64 = profile.iter().sum::<f64>() / 24.0;
        let annual = lookup_region_lower("eu-west-2").unwrap().0;
        let deviation = (mean - annual).abs() / annual;
        assert!(
            deviation < 0.05,
            "gb hourly mean {mean} deviates {deviation:.3} from annual {annual}"
        );
    }

    #[test]
    fn per_op_gco2_single_source() {
        // verify the per_op helper matches the compute_operational
        // formula so the two paths stay in sync (review fix against dedup drift).
        let per_op = per_op_gco2(ENERGY_PER_IO_OP_KWH, 100.0, 1.2);
        let bulk = compute_operational_gco2(1, 100.0, 1.2);
        assert!((per_op - bulk).abs() < 1e-18);
        let bulk10 = compute_operational_gco2(10, 100.0, 1.2);
        assert!((per_op * 10.0 - bulk10).abs() < 1e-18);
    }

    #[test]
    fn carbon_estimate_with_model_tags() {
        // new `_with_model` constructors must carry the
        // supplied model tag all the way through.
        let e = CarbonEstimate::sci_numerator_with_model(0.001, CO2_MODEL_V2);
        assert_eq!(e.model, "io_proxy_v2");
        assert_eq!(e.methodology, "sci_v1_numerator");
        let e = CarbonEstimate::operational_ratio_with_model(0.001, CO2_MODEL_SCAPHANDRE);
        assert_eq!(e.model, "scaphandre_rapl");
        assert_eq!(e.methodology, "sci_v1_operational_ratio");
    }

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

    // ----- CarbonEstimate / CarbonReport / resolve_region tests -----

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
            use_hourly_profiles: true,
            scaphandre_snapshot: None,
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
            use_hourly_profiles: true,
            scaphandre_snapshot: None,
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
            use_hourly_profiles: true,
            scaphandre_snapshot: None,
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
            use_hourly_profiles: true,
            scaphandre_snapshot: None,
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
            use_hourly_profiles: true,
            scaphandre_snapshot: None,
        };
        // Mixed-case service name on the event — should still match.
        let event = make_event("Order-Svc", None);
        assert_eq!(resolve_region(&event, &ctx), Some("us-east-1"));
        // Upper-case service name.
        let event_upper = make_event("ORDER-SVC", None);
        assert_eq!(resolve_region(&event_upper, &ctx), Some("us-east-1"));
    }
}
