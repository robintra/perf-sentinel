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

use serde::Serialize;

use crate::event::SpanEvent;

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

/// Carbon estimation model: Scaphandre per-process RAPL measurement.
/// Highest precedence.
pub const CO2_MODEL_SCAPHANDRE: &str = "scaphandre_rapl";

/// Carbon estimation model: cloud CPU% + `SPECpower` interpolation.
/// Precedence: `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v2` > `io_proxy_v1`.
pub const CO2_MODEL_CLOUD_SPECPOWER: &str = "cloud_specpower";

/// Methodology tag: SCI v1.0 numerator `(E x I) + M` summed over traces.
/// Not the per-R intensity. See design doc for SCI semantics.
pub const METHODOLOGY_SCI_NUMERATOR: &str = "sci_v1_numerator";

/// Methodology tag: avoidable CO2 via `operational * (avoidable_ops / accounted_ops)`.
/// Region-blind, excludes embodied.
pub const METHODOLOGY_OPERATIONAL_RATIO: &str = "sci_v1_operational_ratio";

/// SCI `M` term: embodied carbon per request in gCO₂eq. Conservative
/// upper bound for lightly-loaded servers. Override via
/// `[green] embodied_carbon_per_request_gco2`. Derivation in design doc.
pub const DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2: f64 = 0.001;

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
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CarbonEstimate {
    pub low: f64,
    pub mid: f64,
    pub high: f64,
    pub model: &'static str,
    pub methodology: &'static str,
}

impl CarbonEstimate {
    /// Derive `low`/`high` from midpoint using multiplicative factors.
    const fn new_with_model(mid: f64, model: &'static str, methodology: &'static str) -> Self {
        Self {
            low: mid * CO2_LOW_FACTOR,
            mid,
            high: mid * CO2_HIGH_FACTOR,
            model,
            methodology,
        }
    }

    /// SCI v1.0 numerator estimate with default proxy v1 model.
    #[must_use]
    pub const fn sci_numerator(mid: f64) -> Self {
        Self::new_with_model(mid, CO2_MODEL, METHODOLOGY_SCI_NUMERATOR)
    }

    /// Avoidable CO₂ estimate with default proxy v1 model.
    #[must_use]
    pub const fn operational_ratio(mid: f64) -> Self {
        Self::new_with_model(mid, CO2_MODEL, METHODOLOGY_OPERATIONAL_RATIO)
    }

    /// SCI v1.0 numerator estimate with explicit model tag.
    #[must_use]
    pub const fn sci_numerator_with_model(mid: f64, model: &'static str) -> Self {
        Self::new_with_model(mid, model, METHODOLOGY_SCI_NUMERATOR)
    }

    /// Avoidable CO₂ estimate with explicit model tag.
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
    /// Network transport CO₂ for cross-region HTTP calls (gCO₂eq).
    /// Only present when `[green] include_network_transport = true`
    /// and at least one cross-region HTTP call had response size data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_gco2: Option<f64>,
}

/// Whether a region row used the flat annual or 24-hour profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IntensitySource {
    #[default]
    Annual,
    Hourly,
}

/// Per-region operational CO₂ breakdown row in `green_summary.regions[]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegionBreakdown {
    /// `"known"` / `"out_of_table"` / `"unresolved"`.
    pub status: &'static str,
    pub region: String,
    /// Ops-weighted mean grid intensity (gCO₂eq/kWh). `0.0` if out-of-table.
    pub grid_intensity_gco2_kwh: f64,
    pub pue: f64,
    pub io_ops: usize,
    pub co2_gco2: f64,
    #[serde(default)]
    pub intensity_source: IntensitySource,
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

/// Hourly carbon intensity profiles (UTC, gCO₂eq/kWh). 24 values per region.
/// Sources and per-region rationale in `docs/design/05-GREENOPS-AND-CARBON.md`.
static HOURLY_CARBON_TABLE: &[(&str, [f64; 24])] = &[
    // France (eu-west-3) — nuclear baseload, mean ≈ 55.
    (
        "eu-west-3",
        [
            48.0, 46.0, 45.0, 44.0, 45.0, 47.0, // 00-05 UTC
            52.0, 58.0, 62.0, 60.0, 58.0, 56.0, // 06-11 UTC
            54.0, 52.0, 50.0, 52.0, 58.0, 68.0, // 12-17 UTC
            72.0, 68.0, 62.0, 56.0, 52.0, 50.0, // 18-23 UTC
        ],
    ),
    // Germany (eu-central-1) — coal + wind, mean ≈ 442. See LIMITATIONS.md.
    (
        "eu-central-1",
        [
            380.0, 370.0, 365.0, 360.0, 370.0, 395.0, // 00-05 UTC
            450.0, 480.0, 490.0, 475.0, 460.0, 445.0, // 06-11 UTC
            430.0, 415.0, 420.0, 435.0, 470.0, 510.0, // 12-17 UTC
            525.0, 500.0, 470.0, 440.0, 410.0, 395.0, // 18-23 UTC
        ],
    ),
    // UK (eu-west-2) — wind + gas, mean ≈ 232.
    (
        "eu-west-2",
        [
            195.0, 185.0, 180.0, 175.0, 185.0, 210.0, // 00-05 UTC
            245.0, 275.0, 270.0, 255.0, 240.0, 230.0, // 06-11 UTC
            220.0, 210.0, 215.0, 230.0, 260.0, 290.0, // 12-17 UTC
            300.0, 280.0, 255.0, 235.0, 215.0, 205.0, // 18-23 UTC
        ],
    ),
    // US-East (us-east-1) — gas + coal, mean ≈ 379.
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

/// Hourly intensity for a pre-lowercased region at UTC hour, or `None`.
#[must_use]
pub(crate) fn lookup_hourly_intensity_lower(region: &str, hour: u8) -> Option<f64> {
    if hour >= 24 {
        return None;
    }
    HOURLY_REGION_MAP
        .get(region)
        .map(|profile| profile[hour as usize])
}

/// Full 24-hour profile for a region, or `None` if not profiled.
#[must_use]
pub(crate) fn hourly_profile_for_region_lower(region: &str) -> Option<&'static [f64; 24]> {
    HOURLY_REGION_MAP.get(region).copied()
}

/// Look up `(intensity, pue)` for a region (case-insensitive).
#[must_use]
pub fn lookup_region(region: &str) -> Option<(f64, f64)> {
    let lower = region.to_ascii_lowercase();
    lookup_region_lower(&lower)
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
    fn hourly_profile_de_known_divergence_from_annual() {
        // eu-central-1 intentionally diverges ~30% from the annual value (338).
        // The hourly profile reflects recent 2023-2024 data. This test guards
        // against accidental edits to the profile values.
        let profile = hourly_profile_for_region_lower("eu-central-1").unwrap();
        let mean: f64 = profile.iter().sum::<f64>() / 24.0;
        assert!(
            (420.0..=460.0).contains(&mean),
            "eu-central-1 hourly mean {mean} should be in [420, 460] (known divergence from annual 338)"
        );
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
        // Mixed-case service name on the event — should still match.
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
}
