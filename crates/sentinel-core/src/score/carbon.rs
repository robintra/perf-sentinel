//! `GreenOps` gCO₂eq conversion: static region-based carbon intensity
//! table embedded at compile time, no network egress.
//!
//! See `docs/design/05-GREENOPS-AND-CARBON.md` for the SCI methodology,
//! the per-region intensity sources (Cloud Carbon Footprint, Electricity
//! Maps, ENTSO-E), the per-operation energy coefficients (Xu, Tsirogiannis,
//! `DBJoules`) and the network transport model (Mytton et al. 2024).

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::event::SpanEvent;
use crate::score::electricity_maps::config::{
    ApiVersion, ElectricityMapsConfig, EmissionFactorType, TemporalGranularity,
};

pub use super::carbon_profiles::HourlyProfile;
pub(crate) use super::carbon_profiles::HourlyProfileRef;

/// Estimated energy consumed per I/O operation in kWh.
///
/// This is a rough order-of-magnitude approximation (~0.1 mWh per I/O op).
/// It accounts for a typical database query or HTTP round-trip on cloud
/// infrastructure, including CPU, memory, and network overhead.
///
/// Not a measured value. See `docs/design/05-GREENOPS-AND-CARBON.md`.
pub const ENERGY_PER_IO_OP_KWH: f64 = 0.000_000_1;

// Per-operation energy multipliers (proxy model only).
// See docs/design/05-GREENOPS-AND-CARBON.md for sources and rationale.

const SQL_SELECT_COEFF: f64 = 0.5; // read-only index lookup
const SQL_INSERT_COEFF: f64 = 1.5; // WAL write + data page write
const SQL_UPDATE_COEFF: f64 = 1.5; // read + write
const SQL_DELETE_COEFF: f64 = 1.2; // mark + WAL
const SQL_OTHER_COEFF: f64 = 1.0; // DDL, EXPLAIN, BEGIN, etc.

const HTTP_SMALL_COEFF: f64 = 0.8; // payload < 10 KB
const HTTP_MEDIUM_COEFF: f64 = 1.2; // payload 10 KB to 1 MB
const HTTP_LARGE_COEFF: f64 = 2.0; // payload > 1 MB

const HTTP_SMALL_THRESHOLD: u64 = 10 * 1024; // 10 KB
const HTTP_LARGE_THRESHOLD: u64 = 1024 * 1024; // 1 MB

/// Network transport energy per byte, 0.04 kWh/GB. Fixed since 0.9.25 so
/// every disclosure scales transport identically. Sources in
/// `docs/design/05-GREENOPS-AND-CARBON.md` § "Network transport energy".
pub const DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH: f64 = 0.000_000_000_04;

/// Bracket low bound, 0.001 kWh/GB (Cloud Carbon Footprint networking
/// coefficient, inter-datacenter optical networks).
pub const NETWORK_ENERGY_PER_BYTE_KWH_LOW: f64 = 0.000_000_000_001;

/// Bracket high bound, 0.059 kWh/GB (Sustainable Web Design Model v4
/// operational network segment, includes access networks, a ceiling).
pub const NETWORK_ENERGY_PER_BYTE_KWH_HIGH: f64 = 0.000_000_000_059;

/// Lower bound factor for the CO₂ confidence interval (`low = mid × 0.5`).
/// 2x multiplicative uncertainty, log-symmetric.
pub const CO2_LOW_FACTOR: f64 = 0.5;

/// Upper bound factor for the CO₂ confidence interval (`high = mid × 2.0`).
pub const CO2_HIGH_FACTOR: f64 = 2.0;

/// Carbon estimation model: flat annual proxy.
pub const CO2_MODEL: &str = "io_proxy_v1";

/// Carbon estimation model: hourly carbon intensity profiles.
pub const CO2_MODEL_V2: &str = "io_proxy_v2";

/// Carbon estimation model: monthly x hourly carbon intensity profiles.
/// Precedence: `alumet_rapl` > `scaphandre_rapl` > `kepler_ebpf` > `redfish_bmc` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`.
pub const CO2_MODEL_V3: &str = "io_proxy_v3";

/// Carbon estimation model: Alumet RAPL measurement.
/// Highest measured-energy precedence. Ranks above Scaphandre: both read
/// RAPL, but Alumet's sampling is measurably less error-prone (Raffin
/// and Trystram, "Dissecting the Software-Based Measurement of CPU
/// Energy Consumption: A Comparative Analysis", IEEE Transactions on
/// Parallel and Distributed Systems, 2024, <https://hal.science/hal-04420527v2>),
/// and it attributes per cgroup rather than per process.
pub const CO2_MODEL_ALUMET: &str = "alumet_rapl";

/// Carbon estimation model: Scaphandre per-process RAPL measurement.
/// Sits below Alumet (also RAPL) and above Kepler (`x86_64` only,
/// RAPL-dependent).
pub const CO2_MODEL_SCAPHANDRE: &str = "scaphandre_rapl";

/// Carbon estimation model: Kepler eBPF + perf-counter measurement.
/// Sits between Scaphandre (RAPL) and the cloud `SPECpower` interpolation.
/// Works on ARM with degraded precision vs the Scaphandre x86 path.
pub const CO2_MODEL_KEPLER: &str = "kepler_ebpf";

/// Carbon estimation model: Redfish BMC wall-plug power reading.
/// Bare-metal only, node-level granularity (single shared coefficient
/// across services on the same chassis).
pub const CO2_MODEL_REDFISH: &str = "redfish_bmc";

/// Carbon estimation model: cloud CPU% + `SPECpower` interpolation.
/// Precedence: `alumet_rapl` > `scaphandre_rapl` > `kepler_ebpf` > `redfish_bmc` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`.
pub const CO2_MODEL_CLOUD_SPECPOWER: &str = "cloud_specpower";

/// Carbon intensity source: Electricity Maps real-time API data.
/// Highest precedence for the intensity dimension (independent of the
/// energy model tag which tracks Scaphandre/cloud/proxy).
pub const CO2_MODEL_EMAPS: &str = "electricity_maps_api";

/// Suffix appended to the proxy model tag when calibration factors are active.
pub const CO2_MODEL_CAL_SUFFIX: &str = "+cal";

/// Calibrated proxy model tags (static variants to avoid dynamic allocation).
pub const CO2_MODEL_V1_CAL: &str = "io_proxy_v1+cal";
pub const CO2_MODEL_V2_CAL: &str = "io_proxy_v2+cal";
pub const CO2_MODEL_V3_CAL: &str = "io_proxy_v3+cal";

/// Methodology tag: SCI v1.0 numerator `(E x I) + M` summed over traces.
/// Not the per-R intensity. See design doc for SCI semantics.
pub const METHODOLOGY_SCI_NUMERATOR: &str = "sci_v1_numerator";

/// Methodology tag: SCI v1.0 numerator with network transport energy added.
/// `(E x I) + M + T` where `T` is network transport CO2. The transport term
/// is unconditional, so this tag applies on every run, zero included.
pub const METHODOLOGY_SCI_NUMERATOR_TRANSPORT: &str = "sci_v1_numerator+transport";

/// Methodology tag: avoidable CO2 via `operational * (avoidable_ops / accounted_ops)`.
/// Region-blind, excludes embodied.
pub const METHODOLOGY_OPERATIONAL_RATIO: &str = "sci_v1_operational_ratio";

/// Methodology tag: SCI v1.0 per-R intensity `((E x I) + M) / R`, R = 1 trace.
/// The SCI score proper (an intensity), distinct from the numerator footprint.
pub const METHODOLOGY_SCI_INTENSITY: &str = "sci_v1_intensity";

/// SCI `M` term: embodied carbon per request in gCO₂eq. Conservative
/// upper bound for lightly-loaded servers. Override via
/// `[green] embodied_carbon_per_request_gco2`. Derivation in design doc.
pub const DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2: f64 = 0.001;

/// Generic PUE for regions not associated with a specific cloud
/// provider, also the fallback for out-of-table regions with a custom
/// hourly profile. Tracks the Uptime Institute survey average, rounded
/// to one decimal (the survey plateau spans 1.5-1.6). Sources in
/// `docs/design/05-GREENOPS-AND-CARBON.md` § "PUE values".
pub const GENERIC_PUE: f64 = 1.5;

/// Vintage of the per-provider PUE constants embedded in `Provider::pue`.
/// Release procedure step 2.5 surfaces this string via `grep`. Bump when
/// any provider's published sustainability report supersedes the value
/// in the table.
#[allow(dead_code)]
pub(crate) const PUE_VINTAGE: &str =
    "2026 refresh (AWS 2024 global, GCP 2024 fleet, Azure FY25, OVHcloud FY25, Scaleway 2024)";

/// Synthetic region label for events with no resolved region.
pub const UNKNOWN_REGION: &str = "unknown";

/// Region is in the embedded carbon table.
pub const REGION_STATUS_KNOWN: &str = "known";

/// Region name resolved but not in the carbon table (`co2_gco2 = 0.0`).
pub const REGION_STATUS_OUT_OF_TABLE: &str = "out_of_table";

/// Synthetic "unknown" bucket for unresolved events (`co2_gco2 = 0.0`).
pub const REGION_STATUS_UNRESOLVED: &str = "unresolved";

/// Per-service measured energy-per-op with provenance tag.
#[derive(Debug, Clone, Copy)]
pub struct EnergyEntry {
    /// Energy consumed per I/O operation, in kWh.
    pub energy_per_op_kwh: f64,
    /// Model tag identifying the measurement source. One of
    /// [`CO2_MODEL_ALUMET`], [`CO2_MODEL_SCAPHANDRE`],
    /// [`CO2_MODEL_KEPLER`], [`CO2_MODEL_REDFISH`], or
    /// [`CO2_MODEL_CLOUD_SPECPOWER`].
    pub model_tag: &'static str,
}

impl EnergyEntry {
    /// Build an entry from an Alumet RAPL measurement.
    #[must_use]
    pub const fn alumet(energy_per_op_kwh: f64) -> Self {
        Self {
            energy_per_op_kwh,
            model_tag: CO2_MODEL_ALUMET,
        }
    }

    /// Build an entry from a Scaphandre RAPL measurement.
    #[must_use]
    pub const fn scaphandre(energy_per_op_kwh: f64) -> Self {
        Self {
            energy_per_op_kwh,
            model_tag: CO2_MODEL_SCAPHANDRE,
        }
    }

    /// Build an entry from a Kepler eBPF measurement.
    #[must_use]
    pub const fn kepler(energy_per_op_kwh: f64) -> Self {
        Self {
            energy_per_op_kwh,
            model_tag: CO2_MODEL_KEPLER,
        }
    }

    /// Build an entry from a Redfish BMC wall-plug measurement.
    #[must_use]
    pub const fn redfish(energy_per_op_kwh: f64) -> Self {
        Self {
            energy_per_op_kwh,
            model_tag: CO2_MODEL_REDFISH,
        }
    }

    /// Build an entry from a cloud `SPECpower` interpolation.
    #[must_use]
    pub const fn cloud(energy_per_op_kwh: f64) -> Self {
        Self {
            energy_per_op_kwh,
            model_tag: CO2_MODEL_CLOUD_SPECPOWER,
        }
    }
}

/// CO₂ point estimate with 2x multiplicative uncertainty interval.
///
/// `model` and `methodology` are `String` (not `&'static str`) so the
/// struct can be round-tripped through serde. In-process construction
/// still uses static string constants. The one-time `.to_string()` at
/// build time is negligible next to the numeric work around it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarbonEstimate {
    pub low: f64,
    pub mid: f64,
    pub high: f64,
    pub model: String,
    pub methodology: String,
}

impl CarbonEstimate {
    /// Derive `low`/`high` from midpoint using multiplicative factors.
    pub(crate) fn new_with_model(mid: f64, model: &'static str, methodology: &'static str) -> Self {
        Self {
            low: mid * CO2_LOW_FACTOR,
            mid,
            high: mid * CO2_HIGH_FACTOR,
            model: model.to_string(),
            methodology: methodology.to_string(),
        }
    }

    /// SCI v1.0 numerator estimate with default proxy v1 model.
    #[must_use]
    pub fn sci_numerator(mid: f64) -> Self {
        Self::new_with_model(mid, CO2_MODEL, METHODOLOGY_SCI_NUMERATOR)
    }

    /// Avoidable CO₂ estimate with default proxy v1 model.
    #[must_use]
    pub fn operational_ratio(mid: f64) -> Self {
        Self::new_with_model(mid, CO2_MODEL, METHODOLOGY_OPERATIONAL_RATIO)
    }

    /// SCI v1.0 numerator estimate with explicit model tag.
    #[must_use]
    pub fn sci_numerator_with_model(mid: f64, model: &'static str) -> Self {
        Self::new_with_model(mid, model, METHODOLOGY_SCI_NUMERATOR)
    }

    /// Avoidable CO₂ estimate with explicit model tag.
    #[must_use]
    pub fn operational_ratio_with_model(mid: f64, model: &'static str) -> Self {
        Self::new_with_model(mid, model, METHODOLOGY_OPERATIONAL_RATIO)
    }
}

/// Structured carbon report aligned with the SCI v1.0 model.
///
/// Carries the per-run carbon estimate with two SCI-aligned views:
/// `total` is the SCI numerator `(E × I) + M` summed over analyzed traces,
/// `avoidable` is the region-blind operational ratio approximation.
/// Each estimate carries a 2× multiplicative uncertainty bracket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Network transport CO₂ for cross-region HTTP calls (gCO₂eq).
    /// Present when at least one cross-region HTTP call carried response
    /// size data. Always computed and always displayed since 0.9.25.
    /// `[green] include_network_transport` is deprecated and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_gco2: Option<f64>,
    /// SCI v1.0 per-functional-unit intensity: `total / R`, R = 1 trace.
    /// The SCI score proper (an intensity), distinct from `total` (the
    /// numerator footprint). Methodology tag `sci_v1_intensity`. Optional
    /// only for backward-compatible deserialization of pre-0.8.13 baselines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sci_per_trace: Option<CarbonEstimate>,
    /// SCI functional unit `R`. "trace" maps to the SCI spec's Transaction /
    /// database read-or-write functional unit. Empty only on old baselines.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub functional_unit: String,
}

/// Whether a region row used the flat annual, 24-hour, monthly x hourly profile,
/// or real-time data from the Electricity Maps API.
/// Variants are ordered by fidelity: `Annual` < `Hourly` < `MonthlyHourly` < `RealTime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IntensitySource {
    #[default]
    Annual,
    Hourly,
    MonthlyHourly,
    /// Real-time data from Electricity Maps API (highest fidelity).
    RealTime,
}

/// Per-region operational CO₂ breakdown row in `green_summary.regions[]`.
///
/// `status` is `String` (not `&'static str`) so the struct can be
/// round-tripped through serde. Construction sites use the
/// `REGION_STATUS_*` constants and pay a one-time `.to_string()` cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionBreakdown {
    /// `"known"` / `"out_of_table"` / `"unresolved"`.
    pub status: String,
    pub region: String,
    /// Ops-weighted mean grid intensity (gCO₂eq/kWh). `0.0` if out-of-table.
    pub grid_intensity_gco2_kwh: f64,
    pub pue: f64,
    pub io_ops: usize,
    pub co2_gco2: f64,
    #[serde(default)]
    pub intensity_source: IntensitySource,
    /// Whether the real-time intensity was estimated by `Electricity Maps`
    /// rather than measured directly. Only present when
    /// `intensity_source == RealTime`. `Some(true)` means estimated,
    /// `Some(false)` means measured, `None` means unknown (the API
    /// did not surface the field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity_estimated: Option<bool>,
    /// Estimation algorithm tag returned by `Electricity Maps`
    /// alongside an estimated value, e.g. `"TIME_SLICER_AVERAGE"`.
    /// Only present when `intensity_estimated == Some(true)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity_estimation_method: Option<String>,
}

/// Carbon scoring configuration. Built via [`Config::carbon_context()`].
/// `Default` is for tests only (`embodied = 0.0`, not the config default).
#[derive(Debug, Clone)]
pub struct CarbonContext {
    pub default_region: Option<String>,
    /// Keys lowercased at config load.
    pub service_regions: HashMap<String, String>,
    pub embodied_per_request_gco2: f64,
    pub use_hourly_profiles: bool,
    /// Measured energy from Scaphandre/cloud scrapers (daemon only).
    pub energy_snapshot: Option<HashMap<String, EnergyEntry>>,
    /// SQL verb / HTTP size tier weighting (proxy model only).
    pub per_operation_coefficients: bool,
    /// Deprecated since 0.9.25, retained for API compatibility. Scoring
    /// reads [`DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH`] directly, so no code
    /// path can scale transport differently, and this field always holds
    /// that same value.
    #[deprecated(
        since = "0.9.25",
        note = "the transport coefficient is fixed; this value has no effect"
    )]
    pub network_energy_per_byte_kwh: f64,
    /// Deprecated since 0.9.25, retained for API compatibility. The
    /// transport term is always computed and always displayed, so this
    /// always reads `true` and nothing reads it back.
    #[deprecated(
        since = "0.9.25",
        note = "the transport term is always shown; this value has no effect"
    )]
    pub include_network_transport: bool,
    /// User-supplied hourly profiles from `[green] hourly_profiles_file`.
    /// Keys are pre-lowercased region identifiers.
    /// Takes precedence over embedded profiles. Wrapped in `Arc` so the
    /// daemon can clone the context per tick without deep-copying profiles.
    pub custom_hourly_profiles: Option<Arc<HashMap<String, HourlyProfile>>>,
    /// Per-service calibration factors from `[green] calibration_file`.
    /// Multiplied with the proxy model `ENERGY_PER_IO_OP_KWH` per service.
    pub calibration: Option<crate::calibrate::CalibrationData>,
    /// Real-time grid intensity from Electricity Maps (daemon only).
    /// Keys are lowercased cloud region names, values carry gCO2/kWh
    /// plus the optional `isEstimated` / `estimationMethod` metadata
    /// surfaced by the API.
    pub real_time_intensity: Option<HashMap<String, RealTimeIntensityEntry>>,
    /// Active Electricity Maps scoring configuration (API version,
    /// emission factor type, temporal granularity). Surfaced on
    /// [`crate::report::GreenSummary::scoring_config`] so auditors can
    /// verify which carbon model produced the numbers without reading
    /// the operator's TOML. `None` when green scoring is disabled.
    pub scoring_config: Option<ScoringConfig>,
    /// Declared database measured by Alumet (`[green.alumet.database]`).
    /// Daemon-only: `Config::carbon_context` leaves this `None`, and only
    /// the daemon patches in a `Some` per tick with the energy accumulated
    /// since the previous scored batch. A batch run starts no scraper, so
    /// it emits no database figure and falls back to the estimated one.
    pub db_energy: Option<DbEnergyContext>,
    /// Declared broker measured by Alumet (`[green.alumet.broker]`),
    /// same lifecycle as [`Self::db_energy`].
    pub broker_energy: Option<DbEnergyContext>,
}

/// Window energy of a declared workload cgroup or cluster, feeding
/// [`crate::report::GreenSummary::database_waste`] or its messaging twin.
#[derive(Debug, Clone, PartialEq)]
pub struct DbEnergyContext {
    /// kWh since the previous scored batch, `0.0` = no reading.
    pub window_kwh: f64,
    /// Operator-declared region, `None` skips the carbon conversion.
    pub region: Option<String>,
    /// Provenance of `window_kwh`, carried here because only the caller
    /// that fills it knows whether it was measured or declared.
    pub model: &'static str,
}

impl Default for DbEnergyContext {
    fn default() -> Self {
        Self {
            window_kwh: 0.0,
            region: None,
            // Weakest of the three tags: a caller that forgets to state its
            // provenance must not claim a measurement.
            model: crate::report::DB_WASTE_MODEL_ESTIMATED,
        }
    }
}

/// Audit trail of the settings that shaped the carbon numbers: the
/// Electricity Maps dimensions when that API is configured, and the
/// coefficients applied on every run. Built for each run by
/// [`crate::config::Config::carbon_context`].
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ScoringConfig {
    /// The three fields below describe the Electricity Maps API and are
    /// meaningful only when [`ScoringConfig::electricity_maps`] is true.
    /// They keep their defaults otherwise and are not a claim.
    pub api_version: ApiVersion,
    pub emission_factor_type: EmissionFactorType,
    pub temporal_granularity: TemporalGranularity,
    /// True when `[green.electricity_maps]` is configured. Absent on
    /// windows written before the struct covered anything else, where a
    /// present `scoring_config` did mean the API was configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub electricity_maps: Option<bool>,
    /// Coefficients that scale the published figures. Absent on windows
    /// written before they were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embodied_per_request_gco2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_energy_per_byte_kwh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_operation_coefficients: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_hourly_profiles: Option<bool>,
}

impl ScoringConfig {
    /// Whether this report used Electricity Maps. A present legacy
    /// `scoring_config` predates the explicit flag and therefore means yes.
    #[must_use]
    pub fn uses_electricity_maps(&self) -> bool {
        self.electricity_maps.unwrap_or(true)
    }

    /// Build from the live Electricity Maps config. Used by
    /// [`crate::config::Config::carbon_context`] when the daemon (or
    /// the analyze pipeline) has the `[green.electricity_maps]` block
    /// loaded.
    #[must_use]
    pub fn from_electricity_maps(cfg: &ElectricityMapsConfig) -> Self {
        Self {
            api_version: ApiVersion::from_endpoint(&cfg.api_endpoint),
            emission_factor_type: cfg.emission_factor_type,
            temporal_granularity: cfg.temporal_granularity,
            electricity_maps: Some(true),
            ..Self::default()
        }
    }
}

impl Eq for ScoringConfig {}

/// One real-time intensity value from `Electricity Maps`, carrying the
/// optional `isEstimated` and `estimationMethod` metadata fields the
/// API surfaces alongside `carbonIntensity`. Plumbed through
/// [`CarbonContext::real_time_intensity`] so the per-region breakdown
/// can flag when the value was estimated rather than measured.
#[derive(Debug, Clone)]
#[must_use]
pub struct RealTimeIntensityEntry {
    /// Grid intensity in gCO₂eq/kWh.
    pub gco2_per_kwh: f64,
    /// `Some(true)` if the API marked this value as estimated,
    /// `Some(false)` if explicitly measured, `None` if the field was
    /// absent from the response (forward-compatibility with API
    /// versions that may stop emitting it).
    pub is_estimated: Option<bool>,
    /// Method tag returned alongside an estimated value, e.g.
    /// `"TIME_SLICER_AVERAGE"` or `"GENERAL_PURPOSE_ZONE_DEVELOPMENT"`.
    /// Typically `Some` only when `is_estimated == Some(true)`.
    pub estimation_method: Option<String>,
}

impl RealTimeIntensityEntry {
    /// Build a measured entry with no estimation metadata. Convenience
    /// constructor for tests and callers that only have a raw `f64`.
    pub fn measured(gco2_per_kwh: f64) -> Self {
        Self {
            gco2_per_kwh,
            is_estimated: None,
            estimation_method: None,
        }
    }
}

impl Default for CarbonContext {
    #[allow(deprecated)] // the transport toggle is retained for API compatibility only
    fn default() -> Self {
        Self {
            include_network_transport: true,
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: true,
            network_energy_per_byte_kwh: DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH,
            custom_hourly_profiles: None,
            calibration: None,
            real_time_intensity: None,
            scoring_config: None,
            db_energy: None,
            broker_energy: None,
        }
    }
}

/// Convert workload waste kWh to gCO₂, for the database and broker figures
/// alike: real-time intensity when available, embedded annual otherwise,
/// times provider PUE. A region unknown to the embedded table still
/// converts with [`GENERIC_PUE`] when a real-time entry covers it (custom
/// on-prem region ids), matching the per-span fallback. `None` only when
/// no intensity exists at all.
#[must_use]
pub(crate) fn db_waste_gco2(waste_kwh: f64, region: &str, ctx: &CarbonContext) -> Option<f64> {
    let region_lower = region.to_ascii_lowercase();
    let real_time = ctx
        .real_time_intensity
        .as_ref()
        .and_then(|m| m.get(&region_lower))
        .map(|e| e.gco2_per_kwh);
    let (intensity, pue) = match (lookup_region_lower(&region_lower), real_time) {
        (Some((_, pue)), Some(rt)) => (rt, pue),
        (Some((annual, pue)), None) => (annual, pue),
        (None, Some(rt)) => (rt, GENERIC_PUE),
        (None, None) => return None,
    };
    Some(per_op_gco2(waste_kwh, intensity, pue))
}

/// Resolve region: `cloud_region` > `service_regions` > `default_region` > `None`.
#[must_use]
pub fn resolve_region<'a>(event: &'a SpanEvent, ctx: &'a CarbonContext) -> Option<&'a str> {
    if let Some(region) = event.cloud_region.as_deref() {
        return Some(region);
    }
    // Probe-before-allocate: skip lowercase when service is already lowercase.
    if !ctx.service_regions.is_empty() {
        let lookup = if event.service.bytes().any(|b| b.is_ascii_uppercase()) {
            ctx.service_regions.get(&event.service.to_ascii_lowercase())
        } else {
            ctx.service_regions.get(event.service.as_ref())
        };
        if let Some(region) = lookup {
            return Some(region.as_str());
        }
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
pub(super) enum Provider {
    Aws,
    Gcp,
    Azure,
    Ovh,
    Scaleway,
    Outscale,
    Generic,
}

impl Provider {
    /// Power Usage Effectiveness for this provider.
    const fn pue(self) -> f64 {
        match self {
            Self::Aws => 1.15,
            Self::Gcp => 1.09,
            Self::Azure => 1.17,
            Self::Ovh => 1.24,
            Self::Scaleway => 1.375,
            // 3DS OUTSCALE publishes no PUE: it rents capacity rather than
            // operating its own datacenters, so there is no fleet figure to
            // cite. The variant only names the provider on its rows. Its
            // partner Thesee advertises a design PUE of 1.2, which is
            // neither a measured value nor OUTSCALE's own fleet.
            Self::Outscale | Self::Generic => GENERIC_PUE,
        }
    }
}

/// Hand-refreshed rows: regions on subnational grids. The refresh
/// source (Ember) is national-only and these grids diverge beyond the
/// 2x uncertainty bracket (us-west-2 = 89, hydro Oregon, vs ~370 US
/// national). Values: CCF and Electricity Maps 2023-2024
/// consumption-based averages. The `ca` rows carry a hydro-dominant
/// zone value and the `br` rows the BR-CS (Central-South) zone value
/// containing Sao Paulo. Both hourly profiles are normalized to those
/// levels, not to the national average. Nationally-gridded rows live
/// in `carbon_data.rs`.
///
/// PUE values come from each provider's latest sustainability report
/// (AWS 2024 global, GCP 2024 fleet, Azure FY25, `OVHcloud` FY25 group
/// average, Scaleway 2024 average), 2026 refresh cycle.
static MANUAL_CARBON_ROWS: &[(&str, f64, Provider)] = &[
    // AWS regions
    ("us-east-1", 379.0, Provider::Aws),
    ("us-east-2", 410.0, Provider::Aws),
    ("us-west-1", 200.0, Provider::Aws),
    ("us-west-2", 89.0, Provider::Aws),
    ("ca-central-1", 13.0, Provider::Aws), // Canada (hydro-dominant zone)
    ("sa-east-1", 96.0, Provider::Aws),    // Sao Paulo (BR-CS zone)
    // GCP regions
    ("us-central1", 426.0, Provider::Gcp),
    ("us-east1", 379.0, Provider::Gcp),
    ("us-west1", 89.0, Provider::Gcp),
    // Azure regions
    ("eastus", 379.0, Provider::Azure),
    ("westus2", 89.0, Provider::Azure),
    // OVHcloud regions on subnational grids. The Public Cloud identifier
    // and the S3 location string name the same datacenter, so both keys
    // carry the same zone value.
    ("bhs5", 13.0, Provider::Ovh), // Beauharnois, Quebec (hydro zone)
    ("bhs", 13.0, Provider::Ovh),
    ("us-east-va-1", 379.0, Provider::Ovh), // Vint Hill, Virginia (PJM)
    ("us-west-or-1", 89.0, Provider::Ovh),  // Hillsboro, Oregon
    // OUTSCALE regions on subnational grids. Keys are prefixed because
    // OUTSCALE reuses AWS region identifiers for different places.
    ("outscale-us-east-2", 379.0, Provider::Outscale), // New Jersey, PJM zone as us-east-1
    ("outscale-us-west-1", 200.0, Provider::Outscale), // San Jose, CAISO zone as us-west-1
    // Country / ISO codes (generic PUE)
    ("ca", 13.0, Provider::Generic),
    ("br", 96.0, Provider::Generic), // BR-CS zone, matches sa-east-1
];

/// Pre-built map for O(1) region lookup (keys are lowercase).
/// Chains the generated rows (`carbon_data.rs`) with the manual rows
/// above. Keys are disjoint by construction.
static REGION_MAP: std::sync::LazyLock<HashMap<&str, (f64, Provider)>> =
    std::sync::LazyLock::new(|| {
        super::carbon_data::GENERATED_CARBON_ROWS
            .iter()
            .chain(MANUAL_CARBON_ROWS)
            .map(|&(key, intensity, provider)| (key, (intensity, provider)))
            .collect()
    });

/// Pre-built map for O(1) hourly profile lookup (keys are lowercase).
/// Merges flat-year profiles, monthly profiles, and aliases.
static HOURLY_REGION_MAP: std::sync::LazyLock<HashMap<&str, HourlyProfileRef<'static>>> =
    std::sync::LazyLock::new(|| {
        use super::carbon_profiles::{FLAT_YEAR_PROFILES, MONTHLY_PROFILES, PROFILE_ALIASES};

        let cap = FLAT_YEAR_PROFILES.len() + MONTHLY_PROFILES.len() + PROFILE_ALIASES.len();
        let mut map = HashMap::with_capacity(cap);
        for (key, profile) in FLAT_YEAR_PROFILES {
            map.insert(*key, HourlyProfileRef::FlatYear(profile));
        }
        for (key, profile) in MONTHLY_PROFILES {
            map.insert(*key, HourlyProfileRef::Monthly(profile));
        }
        // Aliases: look up the canonical key and insert a copy of the
        // reference under the alias key (same static data, zero-copy).
        for &(alias, canonical) in PROFILE_ALIASES {
            if let Some(&profile_ref) = map.get(canonical) {
                map.insert(alias, profile_ref);
            }
        }
        map
    });

/// Hourly intensity for a pre-lowercased region at UTC hour and optional
/// month (0-indexed, 0 = January). Returns `None` for unknown regions
/// or invalid hour/month values.
#[cfg(test)]
#[must_use]
pub(crate) fn lookup_hourly_intensity_lower(
    region: &str,
    hour: u8,
    month: Option<u8>,
) -> Option<f64> {
    if hour >= 24 {
        return None;
    }
    if let Some(m) = month
        && m >= 12
    {
        return None;
    }
    HOURLY_REGION_MAP
        .get(region)
        .map(|profile_ref: &HourlyProfileRef<'_>| profile_ref.intensity_at(hour, month))
}

/// Look up the profile reference for a pre-lowercased region.
/// Returns `None` if no hourly profile exists for this region.
#[must_use]
pub(crate) fn hourly_profile_for_region_lower(region: &str) -> Option<HourlyProfileRef<'static>> {
    HOURLY_REGION_MAP.get(region).copied()
}

/// Resolve hourly intensity with custom profile priority.
/// Lookup chain: custom > embedded > None.
///
/// Returns `(intensity, source)` where `source` indicates
/// whether a monthly or flat-year profile was used.
#[cfg(test)]
#[must_use]
pub(crate) fn resolve_hourly_intensity(
    region: &str,
    hour: u8,
    month: Option<u8>,
    custom: Option<&HashMap<String, HourlyProfile>>,
) -> Option<(f64, IntensitySource)> {
    if hour >= 24 {
        return None;
    }
    if let Some(m) = month
        && m >= 12
    {
        return None;
    }
    // 1. Check custom profiles.
    if let Some(custom_map) = custom
        && let Some(profile) = custom_map.get(region)
    {
        let val = profile.intensity_at(hour, month);
        let src = if profile.is_monthly() {
            IntensitySource::MonthlyHourly
        } else {
            IntensitySource::Hourly
        };
        return Some((val, src));
    }
    // 2. Check embedded profiles.
    HOURLY_REGION_MAP
        .get(region)
        .map(|profile_ref: &HourlyProfileRef<'_>| {
            let val = profile_ref.intensity_at(hour, month);
            let src = if profile_ref.is_monthly() {
                IntensitySource::MonthlyHourly
            } else {
                IntensitySource::Hourly
            };
            (val, src)
        })
}

/// Maximum file size for custom profiles (2 MiB). A 30-region monthly
/// file with formatting is well under 100 KB, so 2 MiB is generous.
const MAX_PROFILE_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Maximum plausible grid intensity (gCO2/kWh). No national grid
/// exceeds ~950 (South Africa, Mongolia). Values above 1000 likely
/// indicate a unit confusion (mg vs g or kgCO2 vs gCO2).
const MAX_PLAUSIBLE_INTENSITY: f64 = 1000.0;

/// Maximum number of custom profile entries (same cap as `MAX_REGIONS`).
const MAX_CUSTOM_PROFILES: usize = 256;

/// Load user-supplied hourly profiles from a JSON file.
///
/// Expected format:
/// ```json
/// {
///   "profiles": {
///     "my-region": { "type": "flat_year", "hours": [24 values] },
///     "other":     { "type": "monthly", "months": [[24 values] x 12] }
///   }
/// }
/// ```
///
/// Validation: dimension checks (24 or 12x24), finite, non-negative.
/// Warns (does not reject) when mean diverges >5% from embedded annual.
///
/// # Errors
///
/// Returns `Err` when the file cannot be read, contains invalid JSON,
/// has wrong dimensions, negative or non-finite values or invalid
/// region keys.
pub fn load_custom_profiles(
    path: &std::path::Path,
) -> Result<HashMap<String, HourlyProfile>, String> {
    let content = read_custom_profiles_file(path)?;
    let raw: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("invalid JSON in '{}': {e}", path.display()))?;
    let profiles_obj = raw
        .get("profiles")
        .and_then(|v| v.as_object())
        .ok_or_else(|| format!("'{}' missing 'profiles' object", path.display()))?;
    if profiles_obj.len() > MAX_CUSTOM_PROFILES {
        return Err(format!(
            "'{}' contains {} profiles, exceeding the {} limit",
            path.display(),
            profiles_obj.len(),
            MAX_CUSTOM_PROFILES
        ));
    }

    let mut result = HashMap::with_capacity(profiles_obj.len());
    for (region, value) in profiles_obj {
        let region_lower = region.to_ascii_lowercase();
        if !is_valid_region_id(&region_lower) {
            return Err(
                "invalid region key (expected ASCII alphanumeric + '-'/'_', length 1-64)"
                    .to_string(),
            );
        }
        let profile = parse_single_custom_profile(region, value)?;
        warn_on_profile_anomalies(&region_lower, &profile);
        result.insert(region_lower, profile);
    }
    Ok(result)
}

/// Stat-then-read the file, enforcing [`MAX_PROFILE_FILE_BYTES`] before
/// loading any bytes into memory.
fn read_custom_profiles_file(path: &std::path::Path) -> Result<String, String> {
    let metadata =
        std::fs::metadata(path).map_err(|e| format!("failed to stat '{}': {e}", path.display()))?;
    if metadata.len() > MAX_PROFILE_FILE_BYTES {
        return Err(format!(
            "'{}' is {} bytes, exceeding the {} byte limit",
            path.display(),
            metadata.len(),
            MAX_PROFILE_FILE_BYTES
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("failed to read '{}': {e}", path.display()))
}

/// Dispatch a single `(region, value)` JSON entry to the flat-year or
/// monthly parser based on the `"type"` field.
fn parse_single_custom_profile(
    region: &str,
    value: &serde_json::Value,
) -> Result<HourlyProfile, String> {
    let profile_type = value
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| format!("region '{region}': missing 'type' field"))?;
    match profile_type {
        "flat_year" => parse_flat_year_profile(region, value),
        "monthly" => parse_monthly_profile(region, value),
        _ => Err(format!(
            "region '{region}': unknown profile type (expected 'flat_year' or 'monthly')"
        )),
    }
}

/// Parse the `hours` array of a `flat_year` profile into a `[f64; 24]`.
fn parse_flat_year_profile(
    region: &str,
    value: &serde_json::Value,
) -> Result<HourlyProfile, String> {
    let hours = value
        .get("hours")
        .and_then(|h| h.as_array())
        .ok_or_else(|| format!("region '{region}': missing 'hours' array"))?;
    if hours.len() != 24 {
        return Err(format!(
            "region '{region}': flat_year profile must have exactly 24 values, got {}",
            hours.len()
        ));
    }
    let mut arr = [0.0_f64; 24];
    for (i, v) in hours.iter().enumerate() {
        arr[i] = parse_profile_f64(v, &format!("region '{region}' hour {i}"))?;
    }
    Ok(HourlyProfile::FlatYear(arr))
}

/// Parse the `months` nested array of a `monthly` profile into a
/// `[[f64; 24]; 12]`. Validates both dimensions strictly.
fn parse_monthly_profile(region: &str, value: &serde_json::Value) -> Result<HourlyProfile, String> {
    let months = value
        .get("months")
        .and_then(|m| m.as_array())
        .ok_or_else(|| format!("region '{region}': missing 'months' array"))?;
    if months.len() != 12 {
        return Err(format!(
            "region '{region}': monthly profile must have exactly 12 months, got {}",
            months.len()
        ));
    }
    let mut arr = [[0.0_f64; 24]; 12];
    for (m, month_val) in months.iter().enumerate() {
        let month_arr = month_val
            .as_array()
            .ok_or_else(|| format!("region '{region}' month {m}: expected an array"))?;
        if month_arr.len() != 24 {
            return Err(format!(
                "region '{region}' month {m}: must have exactly 24 values, got {}",
                month_arr.len()
            ));
        }
        for (h, v) in month_arr.iter().enumerate() {
            arr[m][h] = parse_profile_f64(v, &format!("region '{region}' month {m} hour {h}"))?;
        }
    }
    Ok(HourlyProfile::Monthly(Box::new(arr)))
}

/// Convert a [`serde_json::Value`] to a finite non-negative `f64` or
/// return an error prefixed with `context`. Errors are built eagerly
/// because this is a one-shot config load, not a hot path.
fn parse_profile_f64(v: &serde_json::Value, context: &str) -> Result<f64, String> {
    let val = v
        .as_f64()
        .ok_or_else(|| format!("{context}: expected a number"))?;
    if !val.is_finite() || val < 0.0 {
        return Err(format!(
            "{context}: value must be finite and non-negative, got {val}"
        ));
    }
    Ok(val)
}

/// Emit soft warnings on a freshly parsed custom profile:
/// - Mean divergence > 5% vs the embedded annual value for a known region
/// - Mean above [`MAX_PLAUSIBLE_INTENSITY`] (likely unit confusion)
///
/// Never fails: these are hints to the operator, not validation errors.
fn warn_on_profile_anomalies(region_lower: &str, profile: &HourlyProfile) {
    let mean = profile.mean();
    if let Some(&(annual, _)) = REGION_MAP.get(region_lower)
        && annual > 0.0
    {
        let deviation = (mean - annual).abs() / annual;
        if deviation > 0.05 {
            tracing::warn!(
                region = %region_lower,
                profile_mean = mean,
                annual_value = annual,
                deviation_pct = deviation * 100.0,
                "Custom hourly profile mean deviates from embedded annual value. \
                 The profile will be used as-is.",
            );
        }
    }
    if mean > MAX_PLAUSIBLE_INTENSITY {
        tracing::warn!(
            region = %region_lower,
            profile_mean = mean,
            "Custom hourly profile has an unusually high mean intensity. \
             Verify the values are in gCO2/kWh, not mg or another unit.",
        );
    }
}

/// Look up `(intensity, pue)` for a region (case-insensitive).
#[must_use]
pub fn lookup_region(region: &str) -> Option<(f64, f64)> {
    if region.bytes().any(|b| b.is_ascii_uppercase()) {
        lookup_region_lower(&region.to_ascii_lowercase())
    } else {
        lookup_region_lower(region)
    }
}

/// Look up `(intensity, pue)` for a pre-lowercased region.
#[must_use]
pub(crate) fn lookup_region_lower(region: &str) -> Option<(f64, f64)> {
    REGION_MAP
        .get(region)
        .map(|(intensity, provider)| (*intensity, provider.pue()))
}

/// `energy × intensity × pue`. Single source of truth for the CO₂ formula.
#[inline]
#[must_use]
pub(crate) fn per_op_gco2(energy_kwh: f64, intensity: f64, pue: f64) -> f64 {
    energy_kwh * intensity * pue
}

/// Return the energy multiplier for a span based on its operation type.
///
/// For SQL spans: extract the verb from the first word of `target` (the raw
/// SQL statement). OTLP-ingested spans store `db.system` in `operation`,
/// not the SQL verb, so we parse `target` instead.
///
/// For HTTP spans: classify by `response_size_bytes` into small/medium/large
/// tiers. Falls back to `1.0` (base) when size is unknown.
#[inline]
#[must_use]
pub(crate) fn energy_coefficient(event: &SpanEvent) -> f64 {
    match event.event_type {
        crate::event::EventType::Sql => {
            let verb = event.target.split_ascii_whitespace().next().unwrap_or("");
            if verb.eq_ignore_ascii_case("SELECT") {
                SQL_SELECT_COEFF
            } else if verb.eq_ignore_ascii_case("INSERT") {
                SQL_INSERT_COEFF
            } else if verb.eq_ignore_ascii_case("UPDATE") {
                SQL_UPDATE_COEFF
            } else if verb.eq_ignore_ascii_case("DELETE") {
                SQL_DELETE_COEFF
            } else {
                SQL_OTHER_COEFF
            }
        }
        crate::event::EventType::HttpOut => match event.response_size_bytes {
            Some(size) if size > HTTP_LARGE_THRESHOLD => HTTP_LARGE_COEFF,
            Some(size) if size >= HTTP_SMALL_THRESHOLD => HTTP_MEDIUM_COEFF,
            Some(_) => HTTP_SMALL_COEFF,
            None => 1.0,
        },
        // Unweighted: the HTTP size tiers encode a web payload distribution
        // (1 MB is Kafka's default ceiling, not a large message), so reusing
        // them would assert a discount nothing measures.
        crate::event::EventType::Messaging => 1.0,
    }
}

/// Extract the hostname from an HTTP URL.
///
/// Handles `http://host:port/path`, `https://host:port/path`, and
/// `http://user:pass@host:port/path` (RFC 3986 userinfo) patterns.
/// Returns `None` if the URL is malformed or not an HTTP URL.
#[must_use]
pub(crate) fn extract_hostname(url: &str) -> Option<&str> {
    let after_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let host_port = after_scheme.split('/').next()?;
    // Strip userinfo (RFC 3986): "user:pass@host:port" -> "host:port"
    let authority = host_port.rsplit('@').next().unwrap_or(host_port);
    let host = authority.split(':').next()?;
    if host.is_empty() { None } else { Some(host) }
}

/// Compute operational CO₂ in gCO₂eq from raw I/O operation count, grid
/// carbon intensity, and provider PUE.
///
/// Single source of truth for the formula
/// `gCO₂eq = io_ops × ENERGY_PER_IO_OP_KWH × carbon_intensity × PUE`,
/// used by both [`io_ops_to_co2_grams`] (public convenience) and the
/// multi-region scoring stage in `score::compute_carbon_report`.
///
/// Implemented as `io_ops × per_op_gco2(...)` to share the
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
pub(crate) fn io_ops_to_co2_grams(io_ops: usize, region: &str) -> Option<f64> {
    let (intensity, pue) = lookup_region_lower(region)?;
    Some(compute_operational_gco2(io_ops, intensity, pue))
}

#[cfg(test)]
mod tests;
