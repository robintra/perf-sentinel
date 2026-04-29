//! `GreenOps` gCO₂eq conversion: static region-based carbon intensity table.
//!
//! Embeds carbon intensity values per region (gCO₂eq/kWh), cloud provider PUE,
//! per-operation energy coefficients, and network transport energy constants.
//! No network calls: all data is embedded at compile time.
//!
//! Sources:
//! - Cloud Carbon Footprint (CCF): annual grid intensities, PUE values, I/O
//!   energy methodology (<https://ccf.climatiq.io>)
//! - Electricity Maps: annual average gCO₂eq/kWh per region (2023-2024)
//! - ENTSO-E Transparency Platform: hourly carbon profiles for EU regions
//! - Mytton, Lunden & Malmodin (J. Industrial Ecology, 2024): network
//!   transport energy model (0.03-0.06 kWh/GB, power model critique)
//! - Xu et al. (VLDB 2010), Tsirogiannis et al. (SIGMOD 2010): foundational
//!   DBMS energy benchmarks for per-operation SQL verb weighting
//! - Siddik et al., `DBJoules` (2023): per-operation energy measurement
//!   confirming 7-38% inter-operation variance across DBMS
//! - Guo et al. (ACM Computing Surveys 2022): systematic survey of
//!   energy-efficient database systems
//! - IDEAS 2025: real-time energy estimation framework for DBMS queries
//! - Boavizta API / `HotCarbon` 2024: server lifecycle embodied carbon
//!   bottom-up model

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
/// This is a rough order-of-magnitude approximation (~0.1 µWh per I/O op).
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

/// Network transport energy per byte (kWh/byte). 0.04 kWh/GB, midpoint of
/// 0.03-0.06 kWh/GB range from Mytton, Lunden & Malmodin (2024) and
/// Sustainable Web Design (2024). Previous Shift Project 2019 value (0.07)
/// was on the high end; see Mytton 2024 "power model" critique.
/// Only for cross-region HTTP calls when `include_network_transport` is enabled.
pub const DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH: f64 = 0.000_000_000_04;

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
/// Precedence: `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`.
pub const CO2_MODEL_V3: &str = "io_proxy_v3";

/// Carbon estimation model: Scaphandre per-process RAPL measurement.
/// Highest precedence.
pub const CO2_MODEL_SCAPHANDRE: &str = "scaphandre_rapl";

/// Carbon estimation model: cloud CPU% + `SPECpower` interpolation.
/// Precedence: `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`.
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
/// `(E x I) + M + T` where `T` is network transport CO2. Used when
/// `[green] include_network_transport = true` and transport CO2 > 0.
pub const METHODOLOGY_SCI_NUMERATOR_TRANSPORT: &str = "sci_v1_numerator+transport";

/// Methodology tag: avoidable CO2 via `operational * (avoidable_ops / accounted_ops)`.
/// Region-blind, excludes embodied.
pub const METHODOLOGY_OPERATIONAL_RATIO: &str = "sci_v1_operational_ratio";

/// SCI `M` term: embodied carbon per request in gCO₂eq. Conservative
/// upper bound for lightly-loaded servers. Override via
/// `[green] embodied_carbon_per_request_gco2`. Derivation in design doc.
pub const DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2: f64 = 0.001;

/// Generic PUE (Power Usage Effectiveness) for regions not associated
/// with a specific cloud provider. Used as fallback for out-of-table
/// regions that have a custom hourly profile.
pub const GENERIC_PUE: f64 = 1.2;

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
    /// Model tag identifying the measurement source.
    /// One of [`CO2_MODEL_SCAPHANDRE`] or [`CO2_MODEL_CLOUD_SPECPOWER`].
    pub model_tag: &'static str,
}

impl EnergyEntry {
    /// Build an entry from a Scaphandre RAPL measurement.
    #[must_use]
    pub const fn scaphandre(energy_per_op_kwh: f64) -> Self {
        Self {
            energy_per_op_kwh,
            model_tag: CO2_MODEL_SCAPHANDRE,
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
/// still uses static string constants; the one-time `.to_string()` at
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
    /// Only present when `[green] include_network_transport = true`
    /// and at least one cross-region HTTP call had response size data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_gco2: Option<f64>,
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
    pub include_network_transport: bool,
    pub network_energy_per_byte_kwh: f64,
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
    /// the operator's TOML. `None` when Electricity Maps is not
    /// configured.
    pub scoring_config: Option<ScoringConfig>,
}

/// Active Electricity Maps scoring configuration. Three dimensions
/// surfaced together because they all influence the carbon numbers:
/// API version (v3 deprecated, v4 default), emission factor model
/// (lifecycle default, direct opt-in), temporal granularity (hourly
/// default, sub-hour opt-in). Built via
/// [`ScoringConfig::from_electricity_maps`] at config load time.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScoringConfig {
    pub api_version: ApiVersion,
    pub emission_factor_type: EmissionFactorType,
    pub temporal_granularity: TemporalGranularity,
}

impl ScoringConfig {
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
        }
    }
}

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
    fn default() -> Self {
        Self {
            default_region: None,
            service_regions: HashMap::new(),
            embodied_per_request_gco2: 0.0,
            use_hourly_profiles: true,
            energy_snapshot: None,
            per_operation_coefficients: true,
            include_network_transport: false,
            network_energy_per_byte_kwh: DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH,
            custom_hourly_profiles: None,
            calibration: None,
            real_time_intensity: None,
            scoring_config: None,
        }
    }
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
            ctx.service_regions.get(event.service.as_str())
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
/// Data from Cloud Carbon Footprint (CCF) and Electricity Maps
/// (2023-2024 annual averages). PUE values from CCF per provider.
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
    ("eu-west-4", 328.0, Provider::Aws),      // Netherlands (canonical hourly key)
    ("eu-south-1", 370.0, Provider::Aws),     // Milan (Italy)
    ("ap-southeast-2", 550.0, Provider::Aws), // Sydney
    ("ap-south-1", 708.0, Provider::Aws),     // Mumbai
    ("ca-central-1", 13.0, Provider::Aws),    // Canada
    ("sa-east-1", 62.0, Provider::Aws),       // São Paulo
    // GCP regions
    ("us-central1", 426.0, Provider::Gcp),
    ("us-east1", 379.0, Provider::Gcp),
    ("us-west1", 89.0, Provider::Gcp),
    ("europe-west1", 187.0, Provider::Gcp),      // Belgium
    ("europe-west4", 328.0, Provider::Gcp),      // Netherlands
    ("europe-west9", 56.0, Provider::Gcp),       // Paris
    ("europe-north1", 8.0, Provider::Gcp),       // Finland
    ("europe-west8", 370.0, Provider::Gcp),      // Milan (Italy)
    ("europe-southwest1", 200.0, Provider::Gcp), // Madrid (Spain)
    ("europe-central2", 700.0, Provider::Gcp),   // Warsaw (Poland)
    ("europe-north2", 7.0, Provider::Gcp),       // Oslo-ish (Norway)
    ("asia-northeast1", 462.0, Provider::Gcp),   // Tokyo
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
    ("it", 370.0, Provider::Generic),
    ("es", 200.0, Provider::Generic),
    ("pl", 700.0, Provider::Generic),
];

/// Pre-built map for O(1) region lookup (keys are lowercase).
static REGION_MAP: std::sync::LazyLock<HashMap<&'static str, (f64, Provider)>> =
    std::sync::LazyLock::new(|| {
        CARBON_TABLE
            .iter()
            .map(|&(key, intensity, provider)| (key, (intensity, provider)))
            .collect()
    });

/// Pre-built map for O(1) hourly profile lookup (keys are lowercase).
/// Merges flat-year profiles, monthly profiles, and aliases.
static HOURLY_REGION_MAP: std::sync::LazyLock<HashMap<&'static str, HourlyProfileRef<'static>>> =
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
/// file with formatting is well under 100 KB; 2 MiB is generous.
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
pub(crate) fn io_ops_to_co2_grams(io_ops: usize, region: &str) -> Option<f64> {
    let (intensity, pue) = lookup_region_lower(region)?;
    Some(compute_operational_gco2(io_ops, intensity, pue))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- hourly profile tests ---

    #[test]
    fn hourly_profile_present_for_key_regions() {
        // The original 4 regions (now Monthly) plus new FlatYear regions.
        assert!(hourly_profile_for_region_lower("eu-west-3").is_some());
        assert!(hourly_profile_for_region_lower("eu-central-1").is_some());
        assert!(hourly_profile_for_region_lower("eu-west-2").is_some());
        assert!(hourly_profile_for_region_lower("us-east-1").is_some());
        // New FlatYear regions.
        assert!(hourly_profile_for_region_lower("eu-west-1").is_some());
        assert!(hourly_profile_for_region_lower("eu-west-4").is_some());
        assert!(hourly_profile_for_region_lower("eu-north-1").is_some());
        assert!(hourly_profile_for_region_lower("europe-west1").is_some());
        assert!(hourly_profile_for_region_lower("europe-north1").is_some());
        assert!(hourly_profile_for_region_lower("us-east-2").is_some());
        assert!(hourly_profile_for_region_lower("us-west-1").is_some());
        assert!(hourly_profile_for_region_lower("us-west-2").is_some());
        assert!(hourly_profile_for_region_lower("ca-central-1").is_some());
        assert!(hourly_profile_for_region_lower("ap-southeast-2").is_some());
        assert!(hourly_profile_for_region_lower("ap-northeast-1").is_some());
        assert!(hourly_profile_for_region_lower("ap-southeast-1").is_some());
        assert!(hourly_profile_for_region_lower("ap-south-1").is_some());
        assert!(hourly_profile_for_region_lower("sa-east-1").is_some());
    }

    #[test]
    fn hourly_profile_absent_for_unknown_region() {
        assert!(hourly_profile_for_region_lower("mars-1").is_none());
        assert!(hourly_profile_for_region_lower("unknown-region").is_none());
    }

    #[test]
    fn hourly_profile_aliases_resolve() {
        // Country-code aliases should point to the same profile.
        assert!(hourly_profile_for_region_lower("fr").is_some());
        assert!(hourly_profile_for_region_lower("de").is_some());
        assert!(hourly_profile_for_region_lower("gb").is_some());
        assert!(hourly_profile_for_region_lower("ie").is_some());
        assert!(hourly_profile_for_region_lower("nl").is_some());
        assert!(hourly_profile_for_region_lower("se").is_some());
        assert!(hourly_profile_for_region_lower("no").is_some());
        assert!(hourly_profile_for_region_lower("jp").is_some());
        assert!(hourly_profile_for_region_lower("br").is_some());
        // Cloud-provider aliases.
        assert!(hourly_profile_for_region_lower("westeurope").is_some());
        assert!(hourly_profile_for_region_lower("northeurope").is_some());
        assert!(hourly_profile_for_region_lower("uksouth").is_some());
        assert!(hourly_profile_for_region_lower("francecentral").is_some());
    }

    #[test]
    fn hourly_profile_original_4_are_monthly() {
        // The original 4 regions upgraded to Monthly profiles.
        assert!(
            hourly_profile_for_region_lower("eu-west-3")
                .unwrap()
                .is_monthly()
        );
        assert!(
            hourly_profile_for_region_lower("eu-central-1")
                .unwrap()
                .is_monthly()
        );
        assert!(
            hourly_profile_for_region_lower("eu-west-2")
                .unwrap()
                .is_monthly()
        );
        assert!(
            hourly_profile_for_region_lower("us-east-1")
                .unwrap()
                .is_monthly()
        );
    }

    #[test]
    fn hourly_profile_new_regions_are_flat_year() {
        assert!(
            !hourly_profile_for_region_lower("eu-west-1")
                .unwrap()
                .is_monthly()
        );
        assert!(
            !hourly_profile_for_region_lower("us-east-2")
                .unwrap()
                .is_monthly()
        );
        assert!(
            !hourly_profile_for_region_lower("ca-central-1")
                .unwrap()
                .is_monthly()
        );
    }

    #[test]
    fn hourly_intensity_lookup_returns_hour_value() {
        // France at July (month 6): night should be less than evening peak.
        let night_fr = lookup_hourly_intensity_lower("eu-west-3", 3, Some(6)).unwrap();
        let evening_fr = lookup_hourly_intensity_lower("eu-west-3", 18, Some(6)).unwrap();
        assert!(
            night_fr < evening_fr,
            "expected night ({night_fr}) < evening peak ({evening_fr}) in eu-west-3 (July)"
        );
    }

    #[test]
    fn hourly_intensity_unknown_region_returns_none() {
        assert!(lookup_hourly_intensity_lower("mars-1", 10, None).is_none());
    }

    #[test]
    fn hourly_intensity_invalid_hour_returns_none() {
        assert!(lookup_hourly_intensity_lower("eu-west-3", 24, None).is_none());
        assert!(lookup_hourly_intensity_lower("eu-west-3", 99, None).is_none());
    }

    #[test]
    fn hourly_intensity_invalid_month_returns_none() {
        assert!(lookup_hourly_intensity_lower("eu-west-3", 12, Some(12)).is_none());
        assert!(lookup_hourly_intensity_lower("eu-west-3", 12, Some(99)).is_none());
    }

    /// Helper: compute the grand mean of a profile (monthly or flat year).
    fn profile_grand_mean(pr: HourlyProfileRef<'_>) -> f64 {
        match pr {
            HourlyProfileRef::FlatYear(profile) => profile.iter().sum::<f64>() / 24.0,
            HourlyProfileRef::Monthly(profiles) => {
                let total: f64 = profiles.iter().flat_map(|m| m.iter()).sum();
                total / (12.0 * 24.0)
            }
        }
    }

    #[test]
    fn hourly_profile_mean_close_to_annual_for_fr() {
        let pr = hourly_profile_for_region_lower("eu-west-3").unwrap();
        let mean = profile_grand_mean(pr);
        let annual = lookup_region_lower("eu-west-3").unwrap().0;
        let deviation = (mean - annual).abs() / annual;
        assert!(
            deviation < 0.05,
            "fr grand mean {mean:.1} deviates {deviation:.3} from annual {annual}"
        );
    }

    #[test]
    fn hourly_profile_mean_close_to_annual_for_us_east() {
        let pr = hourly_profile_for_region_lower("us-east-1").unwrap();
        let mean = profile_grand_mean(pr);
        let annual = lookup_region_lower("us-east-1").unwrap().0;
        let deviation = (mean - annual).abs() / annual;
        assert!(
            deviation < 0.05,
            "us-east-1 grand mean {mean:.1} deviates {deviation:.3} from annual {annual}"
        );
    }

    #[test]
    fn hourly_profile_mean_close_to_annual_for_gb() {
        let pr = hourly_profile_for_region_lower("eu-west-2").unwrap();
        let mean = profile_grand_mean(pr);
        let annual = lookup_region_lower("eu-west-2").unwrap().0;
        let deviation = (mean - annual).abs() / annual;
        assert!(
            deviation < 0.05,
            "gb grand mean {mean:.1} deviates {deviation:.3} from annual {annual}"
        );
    }

    #[test]
    fn hourly_profile_de_known_divergence_from_annual() {
        let pr = hourly_profile_for_region_lower("eu-central-1").unwrap();
        let mean = profile_grand_mean(pr);
        assert!(
            (420.0..=460.0).contains(&mean),
            "eu-central-1 grand mean {mean:.1} should be in [420, 460] (known divergence from annual 338)"
        );
    }

    // Mean invariant for all new FlatYear regions.
    #[test]
    fn hourly_profile_mean_close_to_annual_for_all_flat_year_regions() {
        for &(key, ref profile) in crate::score::carbon_profiles::FLAT_YEAR_PROFILES {
            let vals: &[f64; 24] = profile;
            let mean: f64 = vals.iter().sum::<f64>() / 24.0;
            let (annual, _) = lookup_region_lower(key).unwrap_or_else(|| {
                panic!("{key} is a canonical profile key but is missing from CARBON_TABLE")
            });
            let deviation = (mean - annual).abs() / annual;
            assert!(
                deviation < 0.05,
                "{key} hourly mean {mean:.1} deviates {deviation:.3} from annual {annual}"
            );
        }
    }

    #[test]
    fn hourly_profile_mean_close_to_annual_for_all_monthly_regions() {
        for &(key, ref months) in crate::score::carbon_profiles::MONTHLY_PROFILES {
            if key == "eu-central-1" {
                continue; // Known ~31% divergence, covered by separate test
            }
            let total: f64 = months.iter().flat_map(|m| m.iter()).sum();
            let mean = total / (12.0 * 24.0);
            let (annual, _) = lookup_region_lower(key).unwrap_or_else(|| {
                panic!("{key} is a canonical monthly profile key but is missing from CARBON_TABLE")
            });
            let deviation = (mean - annual).abs() / annual;
            assert!(
                deviation < 0.05,
                "{key} monthly grand mean {mean:.1} deviates {deviation:.3} from annual {annual}"
            );
        }
    }

    #[test]
    fn monthly_profile_seasonal_variation_fr() {
        // France: winter months should have higher mean than summer months.
        let pr = hourly_profile_for_region_lower("eu-west-3").unwrap();
        let jan_mean = (0..24).map(|h| pr.intensity_at(h, Some(0))).sum::<f64>() / 24.0;
        let jul_mean = (0..24).map(|h| pr.intensity_at(h, Some(6))).sum::<f64>() / 24.0;
        assert!(
            jan_mean > jul_mean,
            "FR January mean ({jan_mean:.1}) should be higher than July ({jul_mean:.1})"
        );
    }

    #[test]
    fn monthly_profile_seasonal_variation_de() {
        let pr = hourly_profile_for_region_lower("eu-central-1").unwrap();
        let jan_mean = (0..24).map(|h| pr.intensity_at(h, Some(0))).sum::<f64>() / 24.0;
        let jun_mean = (0..24).map(|h| pr.intensity_at(h, Some(5))).sum::<f64>() / 24.0;
        assert!(
            jan_mean > jun_mean,
            "DE January mean ({jan_mean:.1}) should be higher than June ({jun_mean:.1})"
        );
    }

    // --- profile shape tests for solar-heavy grids ---

    #[test]
    fn caiso_profile_has_midday_solar_dip() {
        // CAISO duck curve: intensity at peak solar (UTC 18-21, local 10am-1pm)
        // must be well below the evening gas ramp (UTC 2-4, local 6-8pm).
        let pr = hourly_profile_for_region_lower("us-west-1").unwrap();
        let solar_min = (18..=21)
            .map(|h| pr.intensity_at(h, None))
            .fold(f64::INFINITY, f64::min);
        let evening_max = (2..=4)
            .map(|h| pr.intensity_at(h, None))
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            solar_min < evening_max * 0.80,
            "CAISO solar dip ({solar_min:.0}) should be well below evening peak ({evening_max:.0})"
        );
    }

    #[test]
    fn spain_profile_has_midday_solar_dip() {
        // Spain: solar peak at local noon-2pm (UTC 10-12, CET=UTC+1).
        let pr = hourly_profile_for_region_lower("europe-southwest1").unwrap();
        let solar_min = (10..=13)
            .map(|h| pr.intensity_at(h, None))
            .fold(f64::INFINITY, f64::min);
        let evening_max = (17..=19)
            .map(|h| pr.intensity_at(h, None))
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            solar_min < evening_max * 0.85,
            "Spain solar dip ({solar_min:.0}) should be below evening peak ({evening_max:.0})"
        );
    }

    #[test]
    fn hydro_profiles_are_nearly_flat() {
        // Hydro-dominated grids (SE, NO, CA) should have very low variation.
        for region in ["eu-north-1", "europe-north2", "ca-central-1"] {
            let pr = hourly_profile_for_region_lower(region).unwrap();
            let min = (0..24)
                .map(|h| pr.intensity_at(h, None))
                .fold(f64::INFINITY, f64::min);
            let max = (0..24)
                .map(|h| pr.intensity_at(h, None))
                .fold(f64::NEG_INFINITY, f64::max);
            assert!(
                max <= min * 2.5,
                "{region} hydro profile should be nearly flat (min={min:.0}, max={max:.0})"
            );
        }
    }

    // --- resolve_hourly_intensity tests ---

    #[test]
    fn resolve_hourly_intensity_custom_takes_precedence() {
        let mut custom = HashMap::new();
        custom.insert(
            "eu-west-3".to_string(),
            HourlyProfile::FlatYear([999.0; 24]),
        );
        let (val, src) = resolve_hourly_intensity("eu-west-3", 12, None, Some(&custom)).unwrap();
        assert!((val - 999.0).abs() < f64::EPSILON);
        assert_eq!(src, IntensitySource::Hourly);
    }

    #[test]
    fn resolve_hourly_intensity_falls_through_to_embedded() {
        let (val, src) = resolve_hourly_intensity("eu-west-1", 12, None, None).unwrap();
        assert!(val > 0.0);
        assert_eq!(src, IntensitySource::Hourly); // eu-west-1 is FlatYear
    }

    #[test]
    fn resolve_hourly_intensity_monthly_embedded() {
        let (val, src) = resolve_hourly_intensity("eu-west-3", 12, Some(6), None).unwrap();
        assert!(val > 0.0);
        assert_eq!(src, IntensitySource::MonthlyHourly);
    }

    #[test]
    fn resolve_hourly_intensity_unknown_region_returns_none() {
        assert!(resolve_hourly_intensity("mars-1", 12, None, None).is_none());
    }

    #[test]
    fn resolve_hourly_intensity_rejects_invalid_month() {
        assert!(resolve_hourly_intensity("eu-west-3", 12, Some(12), None).is_none());
        assert!(resolve_hourly_intensity("eu-west-3", 12, Some(99), None).is_none());
    }

    #[test]
    fn resolve_hourly_intensity_rejects_invalid_hour() {
        assert!(resolve_hourly_intensity("eu-west-3", 24, None, None).is_none());
        assert!(resolve_hourly_intensity("eu-west-3", 99, None, None).is_none());
    }

    // --- load_custom_profiles tests ---

    #[test]
    fn load_custom_profiles_flat_year() {
        let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_flat.json");
        let hours: Vec<f64> = (0..24).map(|h| 50.0 + f64::from(h)).collect();
        let json =
            format!(r#"{{"profiles": {{"my-dc": {{"type": "flat_year", "hours": {hours:?}}}}}}}"#);
        std::fs::write(&path, &json).unwrap();
        let result = load_custom_profiles(&path).unwrap();
        assert!(result.contains_key("my-dc"));
        assert!(!result["my-dc"].is_monthly());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_custom_profiles_monthly() {
        let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_monthly.json");
        let month: Vec<f64> = vec![100.0; 24];
        let months: Vec<Vec<f64>> = vec![month; 12];
        let json =
            format!(r#"{{"profiles": {{"my-dc": {{"type": "monthly", "months": {months:?}}}}}}}"#);
        std::fs::write(&path, &json).unwrap();
        let result = load_custom_profiles(&path).unwrap();
        assert!(result["my-dc"].is_monthly());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_custom_profiles_rejects_wrong_dimensions() {
        let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_bad_dim.json");
        let json = r#"{"profiles": {"my-dc": {"type": "flat_year", "hours": [1.0, 2.0]}}}"#;
        std::fs::write(&path, json).unwrap();
        assert!(load_custom_profiles(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_custom_profiles_rejects_negative() {
        let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_neg.json");
        let mut hours = vec![50.0; 24];
        hours[5] = -1.0;
        let json =
            format!(r#"{{"profiles": {{"my-dc": {{"type": "flat_year", "hours": {hours:?}}}}}}}"#);
        std::fs::write(&path, &json).unwrap();
        assert!(load_custom_profiles(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_custom_profiles_rejects_nan() {
        let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_nan.json");
        // NaN is not valid JSON, so we use null which will fail to parse as f64.
        let json = r#"{"profiles": {"my-dc": {"type": "flat_year", "hours": [null, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0, 20.0, 21.0, 22.0, 23.0]}}}"#;
        std::fs::write(&path, json).unwrap();
        assert!(load_custom_profiles(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn per_op_gco2_single_source() {
        // verify the per_op helper matches the compute_operational
        // formula so the two paths stay in sync.
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
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
            instrumentation_scopes: Vec::new(),
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
    fn intensity_source_ordering_by_fidelity() {
        // Pin the derived Ord so reordering variants is caught.
        assert!(IntensitySource::Annual < IntensitySource::Hourly);
        assert!(IntensitySource::Hourly < IntensitySource::MonthlyHourly);
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
            energy_snapshot: None,
            ..CarbonContext::default()
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
            energy_snapshot: None,
            ..CarbonContext::default()
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
            energy_snapshot: None,
            ..CarbonContext::default()
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
            energy_snapshot: None,
            ..CarbonContext::default()
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
            energy_snapshot: None,
            ..CarbonContext::default()
        };
        // Mixed-case service name on the event, should still match.
        let event = make_event("Order-Svc", None);
        assert_eq!(resolve_region(&event, &ctx), Some("us-east-1"));
        // Upper-case service name.
        let event_upper = make_event("ORDER-SVC", None);
        assert_eq!(resolve_region(&event_upper, &ctx), Some("us-east-1"));
    }

    // ── energy_coefficient tests ───────────────────────────────────

    fn make_sql_target_event(target: &str) -> SpanEvent {
        SpanEvent {
            timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            trace_id: "trace-1".to_string(),
            span_id: "span-1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "postgresql".to_string(),
            target: target.to_string(),
            duration_us: 1000,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::method".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
            instrumentation_scopes: Vec::new(),
        }
    }

    fn make_http_size_event(response_size_bytes: Option<u64>) -> SpanEvent {
        SpanEvent {
            timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            trace_id: "trace-1".to_string(),
            span_id: "span-1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            cloud_region: None,
            event_type: EventType::HttpOut,
            operation: "GET".to_string(),
            target: "http://user-svc:5000/api/users/123".to_string(),
            duration_us: 1000,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::method".to_string(),
            },
            status_code: Some(200),
            response_size_bytes,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
            instrumentation_scopes: Vec::new(),
        }
    }

    #[test]
    fn energy_coefficient_sql_select() {
        let event = make_sql_target_event("SELECT * FROM users WHERE id = 1");
        assert!((energy_coefficient(&event) - SQL_SELECT_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_sql_insert() {
        let event = make_sql_target_event("INSERT INTO users (name) VALUES ('Alice')");
        assert!((energy_coefficient(&event) - SQL_INSERT_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_sql_update() {
        let event = make_sql_target_event("UPDATE users SET name = 'Bob' WHERE id = 1");
        assert!((energy_coefficient(&event) - SQL_UPDATE_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_sql_delete() {
        let event = make_sql_target_event("DELETE FROM users WHERE id = 1");
        assert!((energy_coefficient(&event) - SQL_DELETE_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_sql_other() {
        let event = make_sql_target_event("CREATE TABLE users (id INT)");
        assert!((energy_coefficient(&event) - SQL_OTHER_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_sql_case_insensitive() {
        let event = make_sql_target_event("select * from users");
        assert!((energy_coefficient(&event) - SQL_SELECT_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_http_small() {
        let event = make_http_size_event(Some(1024)); // 1 KB
        assert!((energy_coefficient(&event) - HTTP_SMALL_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_http_medium() {
        let event = make_http_size_event(Some(100 * 1024)); // 100 KB
        assert!((energy_coefficient(&event) - HTTP_MEDIUM_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_http_large() {
        let event = make_http_size_event(Some(2 * 1024 * 1024)); // 2 MB
        assert!((energy_coefficient(&event) - HTTP_LARGE_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_http_no_size() {
        let event = make_http_size_event(None);
        assert!((energy_coefficient(&event) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_http_boundary_small_threshold() {
        // Exactly at the small/medium boundary (10 KB) should be medium.
        let event = make_http_size_event(Some(HTTP_SMALL_THRESHOLD));
        assert!((energy_coefficient(&event) - HTTP_MEDIUM_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_http_boundary_large_threshold() {
        // Exactly at the large boundary (1 MB) is still medium; >1 MB is large.
        let event = make_http_size_event(Some(HTTP_LARGE_THRESHOLD));
        assert!((energy_coefficient(&event) - HTTP_MEDIUM_COEFF).abs() < f64::EPSILON);
        let event_over = make_http_size_event(Some(HTTP_LARGE_THRESHOLD + 1));
        assert!((energy_coefficient(&event_over) - HTTP_LARGE_COEFF).abs() < f64::EPSILON);
    }

    // ── extract_hostname tests ─────────────────────────────────────

    #[test]
    fn extract_hostname_http_with_port() {
        assert_eq!(
            extract_hostname("http://user-svc:5000/api/users"),
            Some("user-svc")
        );
    }

    #[test]
    fn extract_hostname_http_no_port() {
        assert_eq!(
            extract_hostname("http://user-svc/api/users"),
            Some("user-svc")
        );
    }

    #[test]
    fn extract_hostname_https() {
        assert_eq!(
            extract_hostname("https://api.example.com/path"),
            Some("api.example.com")
        );
    }

    #[test]
    fn extract_hostname_empty() {
        assert_eq!(extract_hostname(""), None);
    }

    #[test]
    fn extract_hostname_no_scheme() {
        assert_eq!(extract_hostname("/api/users"), None);
    }

    #[test]
    fn extract_hostname_empty_host() {
        assert_eq!(extract_hostname("http:///path"), None);
    }

    #[test]
    fn extract_hostname_with_userinfo() {
        // RFC 3986 userinfo: "user:pass@host:port" should extract "host"
        assert_eq!(
            extract_hostname("http://user:pass@order-api:8080/api/orders"),
            Some("order-api")
        );
    }

    #[test]
    fn extract_hostname_with_user_only() {
        assert_eq!(
            extract_hostname("http://admin@order-api/api"),
            Some("order-api")
        );
    }

    #[test]
    fn energy_coefficient_http_zero_bytes() {
        let event = make_http_size_event(Some(0));
        assert!((energy_coefficient(&event) - HTTP_SMALL_COEFF).abs() < f64::EPSILON);
    }

    #[test]
    fn energy_coefficient_sql_empty_target() {
        let event = make_sql_target_event("");
        assert!((energy_coefficient(&event) - SQL_OTHER_COEFF).abs() < f64::EPSILON);
    }

    // --- ScoringConfig (0.5.12 audit-trail surface) ---

    #[test]
    fn scoring_config_default_is_v4_lifecycle_hourly() {
        let cfg = ScoringConfig::default();
        assert_eq!(cfg.api_version, ApiVersion::V4);
        assert_eq!(cfg.emission_factor_type, EmissionFactorType::Lifecycle);
        assert_eq!(cfg.temporal_granularity, TemporalGranularity::Hourly);
    }

    #[test]
    fn scoring_config_round_trip_json_all_defaults() {
        let cfg = ScoringConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ScoringConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(json.contains("\"v4\""));
        assert!(json.contains("\"lifecycle\""));
        assert!(json.contains("\"hourly\""));
    }

    #[test]
    fn scoring_config_round_trip_json_all_optins() {
        let cfg = ScoringConfig {
            api_version: ApiVersion::V3,
            emission_factor_type: EmissionFactorType::Direct,
            temporal_granularity: TemporalGranularity::FiveMinutes,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ScoringConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(json.contains("\"v3\""));
        assert!(json.contains("\"direct\""));
        assert!(json.contains("\"5_minutes\""));
    }

    #[test]
    fn scoring_config_from_electricity_maps_derives_api_version_from_endpoint() {
        // ElectricityMapsConfig has no Default impl (auth_token is
        // mandatory), build manually. The test asserts that the
        // api_version field is derived from the endpoint URL and the
        // two knobs are copied through verbatim.
        let cfg = ElectricityMapsConfig {
            api_endpoint: "https://api.electricitymaps.com/v3".to_string(),
            auth_token: "test-token".to_string(),
            poll_interval: std::time::Duration::from_mins(5),
            region_map: HashMap::new(),
            emission_factor_type: EmissionFactorType::Direct,
            temporal_granularity: TemporalGranularity::FifteenMinutes,
        };
        let scoring = ScoringConfig::from_electricity_maps(&cfg);
        assert_eq!(scoring.api_version, ApiVersion::V3);
        assert_eq!(scoring.emission_factor_type, EmissionFactorType::Direct);
        assert_eq!(
            scoring.temporal_granularity,
            TemporalGranularity::FifteenMinutes
        );
    }

    #[test]
    fn scoring_config_from_electricity_maps_v4_default_endpoint() {
        // Lock the v4 path so a future short-circuit on V3 in
        // `from_electricity_maps` cannot regress the default detection.
        let cfg = ElectricityMapsConfig {
            api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
            auth_token: "test-token".to_string(),
            poll_interval: std::time::Duration::from_mins(5),
            region_map: HashMap::new(),
            emission_factor_type: EmissionFactorType::Lifecycle,
            temporal_granularity: TemporalGranularity::Hourly,
        };
        let scoring = ScoringConfig::from_electricity_maps(&cfg);
        assert_eq!(scoring.api_version, ApiVersion::V4);
        assert_eq!(scoring.emission_factor_type, EmissionFactorType::Lifecycle);
        assert_eq!(scoring.temporal_granularity, TemporalGranularity::Hourly);
    }

    #[test]
    fn scoring_config_from_electricity_maps_custom_endpoint() {
        // Lock the Custom path so an enterprise proxy or mock URL
        // without a `/vN` suffix surfaces correctly on the
        // `green_summary.scoring_config.api_version` chip.
        let cfg = ElectricityMapsConfig {
            api_endpoint: "https://corp-proxy.acme.internal/electricity-maps".to_string(),
            auth_token: "test-token".to_string(),
            poll_interval: std::time::Duration::from_mins(5),
            region_map: HashMap::new(),
            emission_factor_type: EmissionFactorType::Lifecycle,
            temporal_granularity: TemporalGranularity::Hourly,
        };
        let scoring = ScoringConfig::from_electricity_maps(&cfg);
        assert_eq!(scoring.api_version, ApiVersion::Custom);
    }
}
