//! Configuration parsing for `.perf-sentinel.toml`.
//!
//! Supports both the new sectioned format (`[thresholds]`, `[detection]`, `[green]`, `[daemon]`)
//! and the legacy flat format for backward compatibility.

use std::borrow::Cow;
use std::collections::HashMap;

use serde::Deserialize;

use std::time::Duration;

use crate::detect::Confidence;
use crate::score::carbon::DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2;
use crate::score::cloud_energy::config::{CloudEnergyConfig, ServiceCloudConfig};
use crate::score::scaphandre::ScaphandreConfig;

/// Top-level configuration for perf-sentinel.
///
/// Mirrors the four `.perf-sentinel.toml` sections (`[thresholds]`,
/// `[detection]`, `[green]`, `[daemon]`) into typed sub-structs so a
/// consumer that touches only thresholds does not pull a daemon-shaped
/// import surface. The 0.5.x flat layout was unfolded in 0.6.0; see
/// `docs/CONFIGURATION.md` for the rename matrix.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Quality-gate thresholds enforced by `analyze --ci`.
    pub thresholds: ThresholdsConfig,
    /// Per-detector knobs that drive `detect::detect`.
    pub detection: DetectionConfig,
    /// `GreenOps` / SCI-v1.0 scoring config.
    pub green: GreenConfig,
    /// Daemon (`perf-sentinel watch`) runtime config: listeners, ack
    /// store, TLS, CORS, cross-trace correlation.
    pub daemon: DaemonConfig,
    /// Periodic disclosure report config (intent, org-config path, output
    /// destination). Drives daemon startup validation when
    /// `intent = "official"` and is consumed by `perf-sentinel disclose`.
    pub reporting: ReportingConfig,
}

/// Maps 1:1 to `[reporting]` in TOML. All fields optional: an absent
/// section means the operator never asked for a periodic disclosure.
#[derive(Debug, Clone, Default)]
pub struct ReportingConfig {
    /// `"internal"`, `"official"`, or `"audited"`. `None` means no
    /// reporting intent declared.
    pub intent: Option<String>,
    /// `"internal"` or `"public"`. Drives G1 vs G2 granularity.
    pub confidentiality_level: Option<String>,
    /// Path to the operator's organisation/scope/methodology TOML.
    /// Required by daemon startup when `intent = "official"`.
    pub org_config_path: Option<String>,
    /// Path where `perf-sentinel disclose` writes the produced JSON.
    /// Hint only, the CLI accepts an explicit `--output`.
    pub disclose_output_path: Option<String>,
    /// Period selector hint: `"calendar-quarter"`, `"calendar-month"`,
    /// `"calendar-year"`, or `"custom"`. Pure hint for scheduled runs.
    pub disclose_period: Option<String>,
    /// Sigstore signing target. Empty defaults to the public Sigstore
    /// instance. perf-sentinel does not sign itself; this value lives
    /// in the report so `verify-hash` knows which Rekor to query.
    pub sigstore: SigstoreConfig,
}

/// Sigstore Rekor + Fulcio endpoints used by `verify-hash` and reported
/// in `integrity.signature.rekor_url`. Maps to `[reporting.sigstore]`.
#[derive(Debug, Clone)]
pub struct SigstoreConfig {
    pub rekor_url: String,
    pub fulcio_url: String,
}

impl Default for SigstoreConfig {
    fn default() -> Self {
        Self {
            rekor_url: DEFAULT_REKOR_URL.to_string(),
            fulcio_url: DEFAULT_FULCIO_URL.to_string(),
        }
    }
}

/// Public Sigstore Rekor transparency log.
pub const DEFAULT_REKOR_URL: &str = "https://rekor.sigstore.dev";
/// Public Sigstore Fulcio certificate authority.
pub const DEFAULT_FULCIO_URL: &str = "https://fulcio.sigstore.dev";

/// Workspace version that turns `[reporting] disclose_output_path`
/// into a functional field (daemon-triggered periodic disclosures).
/// Bump here when the timeline slips. The same value appears as a
/// TOML comment in `docs/REPORTING.md` and `docs/FR/REPORTING-FR.md`,
/// kept in sync by grep at release time.
const RESERVED_DISCLOSE_OUTPUT_PATH_VERSION: &str = "0.8.0";

/// Maps to `[daemon.archive]` in TOML. When `Some`, the daemon writes
/// each per-window `Report` as one NDJSON line to `path`, with
/// size-triggered rotation and `max_files` count-based pruning.
#[derive(Debug, Clone)]
pub struct DaemonArchiveConfig {
    pub path: String,
    pub max_size_mb: u64,
    pub max_files: u32,
}

impl Default for DaemonArchiveConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            max_size_mb: 100,
            max_files: 12,
        }
    }
}

/// Quality-gate thresholds. Maps 1:1 to `[thresholds]` in TOML.
#[derive(Debug, Clone)]
pub struct ThresholdsConfig {
    /// Maximum allowed critical N+1 SQL findings before quality gate fails.
    pub n_plus_one_sql_critical_max: u32,
    /// Maximum allowed warning+ N+1 HTTP findings before quality gate fails.
    pub n_plus_one_http_warning_max: u32,
    /// Maximum allowed I/O waste ratio before quality gate fails.
    pub io_waste_ratio_max: f64,
}

/// Per-detector knobs. Maps 1:1 to `[detection]` in TOML.
#[derive(Debug, Clone)]
pub struct DetectionConfig {
    /// N+1 detection threshold: minimum repeated similar queries to flag.
    pub n_plus_one_threshold: u32,
    /// Sliding window duration in milliseconds for N+1 detection.
    pub window_duration_ms: u64,
    /// Threshold in milliseconds above which an operation is considered slow.
    pub slow_query_threshold_ms: u64,
    /// Minimum occurrences of a slow template to flag as a finding.
    pub slow_query_min_occurrences: u32,
    /// Maximum child spans per parent before flagging excessive fanout.
    pub max_fanout: u32,
    /// Minimum HTTP outbound calls per trace to flag as chatty service.
    pub chatty_service_min_calls: u32,
    /// Peak concurrent SQL spans per service to flag pool saturation.
    pub pool_saturation_concurrent_threshold: u32,
    /// Minimum sequential independent sibling calls to flag as serialized.
    pub serialized_min_sequential: u32,
    /// Sanitizer-aware classification mode for SQL N+1 vs redundant.
    /// See [`crate::detect::sanitizer_aware::SanitizerAwareMode`].
    pub sanitizer_aware_classification: crate::detect::sanitizer_aware::SanitizerAwareMode,
}

/// `GreenOps` / carbon scoring config. Maps to `[green]` in TOML.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Config aggregates the [green] toggles from .perf-sentinel.toml
pub struct GreenConfig {
    /// Whether `GreenOps` scoring is enabled.
    pub enabled: bool,
    /// Fallback region for CO₂ scoring (e.g. `"eu-west-3"`).
    pub default_region: Option<String>,
    /// Per-service region overrides. Keys lowercased at load time.
    pub service_regions: HashMap<String, String>,
    /// SCI `M` term: embodied carbon per request (gCO₂eq).
    pub embodied_carbon_per_request_gco2: f64,
    /// Use 24-hour carbon intensity profiles when available.
    pub use_hourly_profiles: bool,
    /// Scaphandre RAPL scraper config (daemon only).
    pub scaphandre: Option<ScaphandreConfig>,
    /// Cloud CPU% + `SPECpower` config (daemon only).
    pub cloud_energy: Option<CloudEnergyConfig>,
    /// Whether to use per-operation energy coefficients (SQL verb weighting,
    /// HTTP payload size tiers) in the proxy model. Default: `true`.
    pub per_operation_coefficients: bool,
    /// Whether to compute a network transport energy term for cross-region
    /// HTTP calls. Default: `false` (opt-in).
    pub include_network_transport: bool,
    /// Energy per byte for network transport (kWh/byte).
    /// Default: 0.04 kWh/GB (Mytton et al. 2024).
    pub network_energy_per_byte_kwh: f64,
    /// Path to user-supplied hourly profiles JSON file. `None` when not
    /// configured (uses only embedded profiles).
    pub hourly_profiles_file: Option<String>,
    /// Pre-parsed custom hourly profiles, loaded at config parse time.
    /// `None` when `hourly_profiles_file` is not set or failed to load.
    pub custom_hourly_profiles:
        Option<std::sync::Arc<HashMap<String, crate::score::carbon::HourlyProfile>>>,
    /// Path to a calibration TOML file generated by `perf-sentinel calibrate`.
    pub calibration_file: Option<String>,
    /// Pre-loaded calibration data, parsed at config load time.
    /// `None` when `calibration_file` is not set or failed to load.
    pub calibration: Option<crate::calibrate::CalibrationData>,
    /// Electricity Maps real-time carbon intensity config (daemon only).
    pub electricity_maps: Option<crate::score::electricity_maps::ElectricityMapsConfig>,
}

/// Daemon runtime config. Maps to `[daemon]` plus its `[daemon.tls]`,
/// `[daemon.ack]`, `[daemon.cors]` and `[daemon.correlation]` sub-tables.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Address for the daemon to listen on.
    pub listen_addr: String,
    /// Port for OTLP HTTP receiver.
    pub listen_port: u16,
    /// Port for OTLP gRPC receiver.
    pub listen_port_grpc: u16,
    /// Unix socket path for JSON ingestion.
    pub json_socket: String,
    /// Maximum number of active traces in streaming mode.
    pub max_active_traces: usize,
    /// Trace TTL in milliseconds for streaming mode eviction.
    pub trace_ttl_ms: u64,
    /// Sampling rate for incoming traces (0.0 - 1.0).
    pub sampling_rate: f64,
    /// Maximum events kept per trace (ring buffer size).
    pub max_events_per_trace: usize,
    /// Maximum payload size in bytes for JSON deserialization.
    pub max_payload_size: usize,
    /// Deployment environment label used by the daemon to stamp findings
    /// with a [`Confidence`] value. Defaults to
    /// [`DaemonEnvironment::Staging`]; set to
    /// [`DaemonEnvironment::Production`] when running on production traffic
    /// so downstream consumers (perf-lint) can boost severity. Ignored in
    /// `analyze` batch mode, which always emits [`Confidence::CiBatch`].
    pub environment: DaemonEnvironment,
    /// Maximum number of findings retained by the daemon query API.
    pub max_retained_findings: usize,
    /// Whether the daemon query API is enabled.
    pub api_enabled: bool,
    /// TLS material for the OTLP listeners. When `cert_path` and
    /// `key_path` are both `Some`, both gRPC and HTTP listen TLS; when
    /// both are `None`, plain TCP (default).
    pub tls: DaemonTlsConfig,
    /// Daemon-side ack store (JSONL persistence + HTTP API).
    pub ack: DaemonAckConfig,
    /// CORS layer for the daemon HTTP API.
    pub cors: DaemonCorsConfig,
    /// Cross-trace correlation. `enabled = false` by default; the
    /// daemon never wires the correlator when off, so the other fields
    /// only apply when `enabled = true`.
    pub correlation: crate::detect::correlate_cross::CorrelationConfig,
    /// Optional per-window `Report` archive writer. `None` (default)
    /// means no archive is written. Consumed by `perf-sentinel disclose`.
    pub archive: Option<DaemonArchiveConfig>,
}

/// TLS material. Both fields must be set together (or both `None`).
#[derive(Debug, Clone, Default)]
pub struct DaemonTlsConfig {
    /// Path to PEM-encoded TLS certificate chain for the OTLP receivers.
    pub cert_path: Option<String>,
    /// Path to PEM-encoded TLS private key for the OTLP receivers.
    pub key_path: Option<String>,
}

/// Daemon-side ack store config.
#[derive(Debug, Clone)]
pub struct DaemonAckConfig {
    /// Whether the daemon-side ack store (JSONL persistence + HTTP API)
    /// is enabled. Default `true`. Disabling skips both the TOML acks
    /// load and the JSONL store init at startup, and the three ack
    /// routes return 503 Service Unavailable.
    pub enabled: bool,
    /// Optional override for the JSONL storage path. Default resolves
    /// at runtime via `dirs::data_local_dir()` to
    /// `<data_local>/perf-sentinel/acks.jsonl`.
    pub storage_path: Option<String>,
    /// Optional opt-in API key. When set, `POST` and `DELETE` on
    /// `/api/findings/<sig>/ack` require an `X-API-Key` header
    /// matching this value (constant-time compared). Default `None`
    /// means no auth, suitable for the loopback-only deployment.
    pub api_key: Option<String>,
    /// Optional override for the CI ack TOML file path read at daemon
    /// startup. Default `.perf-sentinel-acknowledgments.toml` in CWD.
    pub toml_path: Option<String>,
}

/// Daemon HTTP API CORS layer config.
#[derive(Debug, Clone, Default)]
pub struct DaemonCorsConfig {
    /// Allowed origins for the daemon HTTP API CORS layer. Empty (default)
    /// means no CORS headers are emitted, which preserves the pre-CORS
    /// behavior. `["*"]` is wildcard mode, intended for development. A
    /// non-wildcard list is the production posture: each entry must be a
    /// full origin (scheme + host + optional port), e.g.
    /// `"https://reports.example.com"`. Configured via
    /// `[daemon.cors] allowed_origins` in TOML.
    pub allowed_origins: Vec<String>,
}

/// Deployment environment for the daemon's `watch` mode.
///
/// Maps 1:1 to [`Confidence`] via [`Config::confidence`]:
/// - [`Self::Staging`] → [`Confidence::DaemonStaging`]
/// - [`Self::Production`] → [`Confidence::DaemonProduction`]
///
/// Parsed from the `[daemon] environment` TOML field as case-insensitive
/// `"staging"` or `"production"`. Any other value is rejected at load time
/// with a clear validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DaemonEnvironment {
    /// Staging traffic, medium confidence. Default.
    #[default]
    Staging,
    /// Production traffic, high confidence.
    Production,
}

impl DaemonEnvironment {
    /// Returns the lowercase string label used in the TOML config.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }
}

impl Default for ThresholdsConfig {
    fn default() -> Self {
        Self {
            n_plus_one_sql_critical_max: 0,
            n_plus_one_http_warning_max: 3,
            io_waste_ratio_max: 0.30,
        }
    }
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            n_plus_one_threshold: 5,
            window_duration_ms: 500,
            slow_query_threshold_ms: 500,
            slow_query_min_occurrences: 3,
            max_fanout: 20,
            chatty_service_min_calls: 15,
            pool_saturation_concurrent_threshold: 10,
            serialized_min_sequential: 3,
            sanitizer_aware_classification:
                crate::detect::sanitizer_aware::SanitizerAwareMode::default(),
        }
    }
}

impl Default for GreenConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_region: None,
            service_regions: HashMap::new(),
            embodied_carbon_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
            use_hourly_profiles: true,
            scaphandre: None,
            cloud_energy: None,
            per_operation_coefficients: true,
            include_network_transport: false,
            network_energy_per_byte_kwh: crate::score::carbon::DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH,
            hourly_profiles_file: None,
            custom_hourly_profiles: None,
            calibration_file: None,
            calibration: None,
            electricity_maps: None,
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1".to_string(),
            listen_port: 4318,
            listen_port_grpc: 4317,
            json_socket: "/tmp/perf-sentinel.sock".to_string(),
            max_active_traces: 10_000,
            trace_ttl_ms: 30_000,
            sampling_rate: 1.0,
            max_events_per_trace: 1_000,
            // 16 MiB, comfort-zone ceiling (warn_unusual_daemon_limits)
            max_payload_size: 16 * 1024 * 1024,
            environment: DaemonEnvironment::Staging,
            max_retained_findings: 10_000,
            api_enabled: true,
            tls: DaemonTlsConfig::default(),
            ack: DaemonAckConfig::default(),
            cors: DaemonCorsConfig::default(),
            correlation: crate::detect::correlate_cross::CorrelationConfig::default(),
            archive: None,
        }
    }
}

impl Default for DaemonAckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            storage_path: None,
            api_key: None,
            toml_path: None,
        }
    }
}

impl Config {
    /// Map the daemon environment to a [`Confidence`] value.
    ///
    /// Used by `daemon::run` to stamp findings after detection. `analyze`
    /// batch mode does not call this; it hardcodes [`Confidence::CiBatch`]
    /// in `pipeline::analyze_with_traces` instead.
    #[must_use]
    pub const fn confidence(&self) -> Confidence {
        match self.daemon.environment {
            DaemonEnvironment::Staging => Confidence::DaemonStaging,
            DaemonEnvironment::Production => Confidence::DaemonProduction,
        }
    }

    /// Build a [`CarbonContext`] from the green config fields.
    ///
    /// Returns a context with `energy_snapshot: None`. The daemon clones
    /// this and patches in the measured energy snapshot per tick; the
    /// batch pipeline uses it as-is (no scrapers in batch mode).
    #[must_use]
    pub fn carbon_context(&self) -> crate::score::carbon::CarbonContext {
        let scoring_config = self
            .green
            .electricity_maps
            .as_ref()
            .map(crate::score::carbon::ScoringConfig::from_electricity_maps);
        crate::score::carbon::CarbonContext {
            default_region: self.green.default_region.clone(),
            service_regions: self.green.service_regions.clone(),
            embodied_per_request_gco2: self.green.embodied_carbon_per_request_gco2,
            use_hourly_profiles: self.green.use_hourly_profiles,
            energy_snapshot: None,
            per_operation_coefficients: self.green.per_operation_coefficients,
            include_network_transport: self.green.include_network_transport,
            network_energy_per_byte_kwh: self.green.network_energy_per_byte_kwh,
            custom_hourly_profiles: self.green.custom_hourly_profiles.clone(),
            calibration: self.green.calibration.clone(),
            real_time_intensity: None, // set per-tick in daemon via build_tick_ctx
            scoring_config,
        }
    }
}

// --- Internal raw deserialization types ---

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    thresholds: ThresholdsSection,
    detection: DetectionSection,
    green: GreenSection,
    daemon: DaemonSection,
    reporting: ReportingSection,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReportingSection {
    intent: Option<String>,
    confidentiality_level: Option<String>,
    org_config_path: Option<String>,
    disclose_output_path: Option<String>,
    disclose_period: Option<String>,
    sigstore: SigstoreSection,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SigstoreSection {
    rekor_url: Option<String>,
    fulcio_url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ArchiveSection {
    path: Option<String>,
    max_size_mb: Option<u64>,
    max_files: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(clippy::struct_field_names)] // fields like `n_plus_one_sql_critical_max` repeat the struct context but match the TOML keys
struct ThresholdsSection {
    n_plus_one_sql_critical_max: Option<u32>,
    n_plus_one_http_warning_max: Option<u32>,
    io_waste_ratio_max: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DetectionSection {
    window_duration_ms: Option<u64>,
    n_plus_one_min_occurrences: Option<u32>,
    slow_query_threshold_ms: Option<u64>,
    slow_query_min_occurrences: Option<u32>,
    max_fanout: Option<u32>,
    chatty_service_min_calls: Option<u32>,
    pool_saturation_concurrent_threshold: Option<u32>,
    serialized_min_sequential: Option<u32>,
    sanitizer_aware_classification: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GreenSection {
    enabled: Option<bool>,
    default_region: Option<String>,
    service_regions: HashMap<String, String>,
    embodied_carbon_per_request_gco2: Option<f64>,
    /// toggle for the hourly carbon intensity profile path.
    /// Default `true`. Maps to `Config::green.use_hourly_profiles`.
    use_hourly_profiles: Option<bool>,
    /// Scaphandre scraper section. Absent when Scaphandre
    /// is not configured.
    scaphandre: ScaphandreSection,
    /// Cloud energy section. Absent when cloud energy is not configured.
    cloud: CloudSection,
    per_operation_coefficients: Option<bool>,
    include_network_transport: Option<bool>,
    network_energy_per_byte_kwh: Option<f64>,
    /// Path to a JSON file with user-supplied hourly carbon profiles.
    hourly_profiles_file: Option<String>,
    /// Path to a calibration TOML file from `perf-sentinel calibrate`.
    calibration_file: Option<String>,
    /// Electricity Maps API section.
    electricity_maps: ElectricityMapsSection,
}

/// Raw deserialization target for `[green.scaphandre]`.
///
/// Converted to a `ScaphandreConfig` during `RawConfig → Config` only
/// when `endpoint` is set: an empty table (no fields) leaves
/// `Config::green.scaphandre = None`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct ScaphandreSection {
    endpoint: Option<String>,
    scrape_interval_secs: Option<u64>,
    process_map: HashMap<String, String>,
    auth_header: Option<String>,
}

/// Raw deserialization target for `[green.cloud]`.
///
/// Converted to a `CloudEnergyConfig` during `RawConfig -> Config` only
/// when `prometheus_endpoint` is set.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CloudSection {
    prometheus_endpoint: Option<String>,
    scrape_interval_secs: Option<u64>,
    default_provider: Option<String>,
    default_instance_type: Option<String>,
    cpu_metric: Option<String>,
    services: HashMap<String, CloudServiceRaw>,
    auth_header: Option<String>,
}

/// Raw deserialization for a single entry in `[green.cloud.services]`.
///
/// Supports two forms:
/// - Instance type: `{ provider = "aws", instance_type = "m5.large" }`
/// - Manual watts: `{ idle_watts = 45, max_watts = 120 }`
#[derive(Deserialize, Default)]
#[serde(default)]
struct CloudServiceRaw {
    provider: Option<String>,
    instance_type: Option<String>,
    idle_watts: Option<f64>,
    max_watts: Option<f64>,
    cpu_query: Option<String>,
}

/// Raw deserialization target for `[green.electricity_maps]`.
///
/// Converted to an `ElectricityMapsConfig` during `RawConfig -> Config`
/// only when `api_key` is set (directly or via env var).
#[derive(Deserialize, Default)]
#[serde(default)]
struct ElectricityMapsSection {
    api_key: Option<String>,
    endpoint: Option<String>,
    poll_interval_secs: Option<u64>,
    region_map: HashMap<String, String>,
    emission_factor_type: Option<String>,
    temporal_granularity: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DaemonSection {
    listen_address: Option<String>,
    listen_port_http: Option<u16>,
    listen_port_grpc: Option<u16>,
    json_socket: Option<String>,
    max_active_traces: Option<usize>,
    trace_ttl_ms: Option<u64>,
    sampling_rate: Option<f64>,
    max_events_per_trace: Option<usize>,
    max_payload_size: Option<usize>,
    /// `"staging"` (default) or `"production"`. Validated
    /// in `Config::validate`; invalid values fail at load time with a
    /// clear error. Case-insensitive.
    environment: Option<String>,
    /// Path to PEM-encoded TLS certificate chain.
    tls_cert_path: Option<String>,
    /// Path to PEM-encoded TLS private key.
    tls_key_path: Option<String>,
    /// Maximum number of findings kept by the daemon query API.
    max_retained_findings: Option<usize>,
    /// Whether the daemon query API is enabled.
    api_enabled: Option<bool>,
    /// Cross-trace correlation section.
    correlation: CorrelationSection,
    /// Daemon-side ack store section.
    ack: DaemonAckSection,
    /// CORS section for the daemon HTTP API.
    cors: DaemonCorsSection,
    /// Per-window report archive writer section.
    archive: ArchiveSection,
}

/// Raw deserialization target for `[daemon.correlation]`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CorrelationSection {
    enabled: Option<bool>,
    window_minutes: Option<u64>,
    lag_threshold_ms: Option<u64>,
    min_co_occurrences: Option<u32>,
    min_confidence: Option<f64>,
    max_tracked_pairs: Option<usize>,
}

/// Raw deserialization target for `[daemon.ack]`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct DaemonAckSection {
    enabled: Option<bool>,
    storage_path: Option<String>,
    api_key: Option<String>,
    toml_path: Option<String>,
}

/// Raw deserialization target for `[daemon.cors]`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct DaemonCorsSection {
    allowed_origins: Vec<String>,
}

const TOML_PATH_STRING_KEYS: &[&str] = &[
    "hourly_profiles_file",
    "calibration_file",
    "json_socket",
    "tls_cert_path",
    "tls_key_path",
    "storage_path",
    "toml_path",
];

/// Rewrite path-like config fields so Windows-style backslashes are treated
/// as literal separators instead of TOML escapes.
///
/// See `docs/design/07-CLI-CONFIG-RELEASE.md` > "Windows path normalization"
/// for the full algorithm, the UNC rule, and the fallback design.
fn normalize_toml_path_strings(content: &str) -> Cow<'_, str> {
    let mut changed = false;
    let mut normalized = String::with_capacity(content.len());

    for line in content.split_inclusive('\n') {
        let rewritten = normalize_toml_path_line(line);
        changed |= matches!(rewritten, Cow::Owned(_));
        normalized.push_str(rewritten.as_ref());
    }

    if changed {
        Cow::Owned(normalized)
    } else {
        Cow::Borrowed(content)
    }
}

fn normalize_toml_path_line(line: &str) -> Cow<'_, str> {
    let leading_ws = line.len() - line.trim_start_matches([' ', '\t']).len();
    let trimmed = &line[leading_ws..];
    let Some(eq_idx) = trimmed.find('=') else {
        return Cow::Borrowed(line);
    };

    let key = trimmed[..eq_idx].trim();
    if !TOML_PATH_STRING_KEYS.contains(&key) {
        return Cow::Borrowed(line);
    }

    let after_eq = &trimmed[eq_idx + 1..];
    let value_ws = after_eq.len() - after_eq.trim_start_matches([' ', '\t']).len();
    let value_start = leading_ws + eq_idx + 1 + value_ws;
    let value = &line[value_start..];
    if !value.starts_with('"') {
        return Cow::Borrowed(line);
    }

    let Some(closing_quote) = find_basic_string_end(value) else {
        return Cow::Borrowed(line);
    };
    let inner = &value[1..closing_quote];
    let Cow::Owned(normalized_inner) = escape_toml_path_backslashes(inner) else {
        return Cow::Borrowed(line);
    };

    // Push the opening `"` explicitly so `value_start` is never used as
    // the end of an inclusive byte range. See design doc 07 > "Windows
    // path normalization" for the UTF-8 invariant.
    let mut out =
        String::with_capacity(line.len() + normalized_inner.len().saturating_sub(inner.len()));
    out.push_str(&line[..value_start]);
    out.push('"');
    out.push_str(&normalized_inner);
    out.push_str(&value[closing_quote..]);
    Cow::Owned(out)
}

/// Return the byte offset of the closing `"` that terminates a TOML basic
/// string starting at `value[0]` or `None` if the string is unterminated.
///
/// Linear: the `run` counter avoids an O(n²) lookbehind on inputs full of
/// `\`. See design doc 07 > "Windows path normalization" for context.
fn find_basic_string_end(value: &str) -> Option<usize> {
    debug_assert!(value.starts_with('"'));

    let bytes = value.as_bytes();
    let mut run: usize = 0;
    let mut idx = 1;
    while idx < bytes.len() {
        match bytes[idx] {
            b'"' if run.is_multiple_of(2) => return Some(idx),
            b'\\' => run += 1,
            _ => run = 0,
        }
        idx += 1;
    }
    None
}

/// Escape single backslashes inside a TOML basic-string path so its value
/// round-trips as a literal separator.
///
/// See design doc 07 > "Windows path normalization" for the per-run rules
/// (single `\`, escape pairs, raw UNC prefix). Returns `Cow::Borrowed(inner)`
/// when no rewrite is needed.
fn escape_toml_path_backslashes(inner: &str) -> Cow<'_, str> {
    if !inner.contains('\\') {
        return Cow::Borrowed(inner);
    }

    let bytes = inner.as_bytes();
    let mut out = String::with_capacity(inner.len() + 4);
    let mut changed = false;
    let mut idx = 0;

    while idx < bytes.len() {
        if bytes[idx] != b'\\' {
            idx = copy_until_backslash(inner, bytes, idx, &mut out);
            continue;
        }

        let run_start = idx;
        idx = skip_backslash_run(bytes, idx);
        let run_len = idx - run_start;
        let emit_len = backslash_emit_len(run_start, run_len, bytes.get(idx).copied());
        changed |= emit_len != run_len;
        for _ in 0..emit_len {
            out.push('\\');
        }
    }

    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(inner)
    }
}

/// Copy bytes from `start` up to (but not including) the next `\` into
/// `out`, and return the index where the run of `\` begins (or
/// `bytes.len()` if no more `\` is found).
fn copy_until_backslash(inner: &str, bytes: &[u8], start: usize, out: &mut String) -> usize {
    let mut idx = start;
    while idx < bytes.len() && bytes[idx] != b'\\' {
        idx += 1;
    }
    out.push_str(&inner[start..idx]);
    idx
}

/// Skip a run of consecutive `\` starting at `start` and return the index
/// of the first non-`\` byte (or `bytes.len()`).
fn skip_backslash_run(bytes: &[u8], start: usize) -> usize {
    let mut idx = start;
    while idx < bytes.len() && bytes[idx] == b'\\' {
        idx += 1;
    }
    idx
}

/// Decide how many `\` to emit for a run of `run_len` backslashes
/// starting at byte offset `run_start`. `next_byte` is the byte
/// immediately after the run (used to disambiguate UNC prefixes).
fn backslash_emit_len(run_start: usize, run_len: usize, next_byte: Option<u8>) -> usize {
    let raw_unc_prefix = run_start == 0 && run_len == 2 && next_byte != Some(b'\\');
    if raw_unc_prefix {
        4
    } else if run_len == 1 {
        2
    } else {
        run_len
    }
}

impl From<RawConfig> for Config {
    #[allow(clippy::too_many_lines)] // Sectioned config-to-typed mapping: splitting would scatter field assignments across helpers
    fn from(raw: RawConfig) -> Self {
        let thresholds_defaults = ThresholdsConfig::default();
        let detection_defaults = DetectionConfig::default();
        let green_defaults = GreenConfig::default();
        let daemon_defaults = DaemonConfig::default();
        let correlation_defaults = crate::detect::correlate_cross::CorrelationConfig::default();
        let ack_defaults = DaemonAckConfig::default();

        Self {
            thresholds: ThresholdsConfig {
                n_plus_one_sql_critical_max: raw
                    .thresholds
                    .n_plus_one_sql_critical_max
                    .unwrap_or(thresholds_defaults.n_plus_one_sql_critical_max),
                n_plus_one_http_warning_max: raw
                    .thresholds
                    .n_plus_one_http_warning_max
                    .unwrap_or(thresholds_defaults.n_plus_one_http_warning_max),
                io_waste_ratio_max: raw
                    .thresholds
                    .io_waste_ratio_max
                    .unwrap_or(thresholds_defaults.io_waste_ratio_max),
            },
            detection: DetectionConfig {
                n_plus_one_threshold: raw
                    .detection
                    .n_plus_one_min_occurrences
                    .unwrap_or(detection_defaults.n_plus_one_threshold),
                window_duration_ms: raw
                    .detection
                    .window_duration_ms
                    .unwrap_or(detection_defaults.window_duration_ms),
                slow_query_threshold_ms: raw
                    .detection
                    .slow_query_threshold_ms
                    .unwrap_or(detection_defaults.slow_query_threshold_ms),
                slow_query_min_occurrences: raw
                    .detection
                    .slow_query_min_occurrences
                    .unwrap_or(detection_defaults.slow_query_min_occurrences),
                max_fanout: raw
                    .detection
                    .max_fanout
                    .unwrap_or(detection_defaults.max_fanout),
                chatty_service_min_calls: raw
                    .detection
                    .chatty_service_min_calls
                    .unwrap_or(detection_defaults.chatty_service_min_calls),
                pool_saturation_concurrent_threshold: raw
                    .detection
                    .pool_saturation_concurrent_threshold
                    .unwrap_or(detection_defaults.pool_saturation_concurrent_threshold),
                serialized_min_sequential: raw
                    .detection
                    .serialized_min_sequential
                    .unwrap_or(detection_defaults.serialized_min_sequential),
                sanitizer_aware_classification:
                    crate::detect::sanitizer_aware::SanitizerAwareMode::from_config(
                        raw.detection.sanitizer_aware_classification.as_deref(),
                    ),
            },
            green: GreenConfig {
                enabled: raw.green.enabled.unwrap_or(green_defaults.enabled),
                // Lowercase default_region and service_regions keys so
                // resolve_region's lowercase lookup matches regardless of
                // config casing, without paying the lowercase cost on every
                // downstream call site.
                default_region: raw.green.default_region.map(|s| s.to_ascii_lowercase()),
                service_regions: raw
                    .green
                    .service_regions
                    .into_iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v))
                    .collect(),
                embodied_carbon_per_request_gco2: raw
                    .green
                    .embodied_carbon_per_request_gco2
                    .unwrap_or(green_defaults.embodied_carbon_per_request_gco2),
                use_hourly_profiles: raw
                    .green
                    .use_hourly_profiles
                    .unwrap_or(green_defaults.use_hourly_profiles),
                scaphandre: convert_scaphandre_section(&raw.green.scaphandre),
                cloud_energy: convert_cloud_section(&raw.green.cloud),
                per_operation_coefficients: raw
                    .green
                    .per_operation_coefficients
                    .unwrap_or(green_defaults.per_operation_coefficients),
                include_network_transport: raw
                    .green
                    .include_network_transport
                    .unwrap_or(green_defaults.include_network_transport),
                network_energy_per_byte_kwh: raw
                    .green
                    .network_energy_per_byte_kwh
                    .unwrap_or(green_defaults.network_energy_per_byte_kwh),
                hourly_profiles_file: raw.green.hourly_profiles_file.clone(),
                custom_hourly_profiles: raw.green.hourly_profiles_file.as_ref().and_then(|path| {
                    if has_control_char(path) {
                        tracing::warn!(
                            "hourly_profiles_file path contains control characters, skipping"
                        );
                        return None;
                    }
                    let p = std::path::Path::new(path);
                    match crate::score::carbon::load_custom_profiles(p) {
                        Ok(profiles) => Some(std::sync::Arc::new(profiles)),
                        Err(e) => {
                            // Not logged at warn: validate_green() will
                            // surface a hard error for this case.
                            tracing::debug!(
                                error = %e,
                                "Custom hourly profiles failed to load"
                            );
                            None
                        }
                    }
                }),
                calibration_file: raw.green.calibration_file.clone(),
                calibration: raw.green.calibration_file.as_ref().and_then(|path| {
                    if has_control_char(path) {
                        tracing::warn!(
                            "calibration_file path contains control characters, skipping"
                        );
                        return None;
                    }
                    match crate::calibrate::load_calibration_file(path) {
                        Ok(data) => Some(data),
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                "Calibration file failed to load"
                            );
                            None
                        }
                    }
                }),
                electricity_maps: convert_electricity_maps_section(&raw.green.electricity_maps),
            },
            daemon: DaemonConfig {
                listen_addr: raw
                    .daemon
                    .listen_address
                    .unwrap_or(daemon_defaults.listen_addr),
                listen_port: raw
                    .daemon
                    .listen_port_http
                    .unwrap_or(daemon_defaults.listen_port),
                listen_port_grpc: raw
                    .daemon
                    .listen_port_grpc
                    .unwrap_or(daemon_defaults.listen_port_grpc),
                json_socket: raw
                    .daemon
                    .json_socket
                    .unwrap_or(daemon_defaults.json_socket),
                max_active_traces: raw
                    .daemon
                    .max_active_traces
                    .unwrap_or(daemon_defaults.max_active_traces),
                trace_ttl_ms: raw
                    .daemon
                    .trace_ttl_ms
                    .unwrap_or(daemon_defaults.trace_ttl_ms),
                sampling_rate: raw
                    .daemon
                    .sampling_rate
                    .unwrap_or(daemon_defaults.sampling_rate),
                max_events_per_trace: raw
                    .daemon
                    .max_events_per_trace
                    .unwrap_or(daemon_defaults.max_events_per_trace),
                max_payload_size: raw
                    .daemon
                    .max_payload_size
                    .unwrap_or(daemon_defaults.max_payload_size),
                // Parse environment into the typed enum. Invalid strings are
                // rejected by load_from_str() before reaching this conversion;
                // direct callers (tests only) get Staging as a safe default.
                environment: match raw.daemon.environment.as_deref() {
                    None => daemon_defaults.environment,
                    Some(s) => parse_daemon_environment(s).unwrap_or(DaemonEnvironment::Staging),
                },
                max_retained_findings: raw
                    .daemon
                    .max_retained_findings
                    .unwrap_or(daemon_defaults.max_retained_findings),
                api_enabled: raw
                    .daemon
                    .api_enabled
                    .unwrap_or(daemon_defaults.api_enabled),
                tls: DaemonTlsConfig {
                    cert_path: raw.daemon.tls_cert_path,
                    key_path: raw.daemon.tls_key_path,
                },
                ack: DaemonAckConfig {
                    enabled: raw.daemon.ack.enabled.unwrap_or(ack_defaults.enabled),
                    storage_path: raw.daemon.ack.storage_path,
                    api_key: raw.daemon.ack.api_key,
                    toml_path: raw.daemon.ack.toml_path,
                },
                cors: DaemonCorsConfig {
                    allowed_origins: raw.daemon.cors.allowed_origins,
                },
                correlation: {
                    let c = &raw.daemon.correlation;
                    crate::detect::correlate_cross::CorrelationConfig {
                        enabled: c.enabled.unwrap_or(correlation_defaults.enabled),
                        window_ms: c
                            .window_minutes
                            .map_or(correlation_defaults.window_ms, |m| m.saturating_mul(60_000)),
                        lag_threshold_ms: c
                            .lag_threshold_ms
                            .unwrap_or(correlation_defaults.lag_threshold_ms),
                        min_co_occurrences: c
                            .min_co_occurrences
                            .unwrap_or(correlation_defaults.min_co_occurrences),
                        min_confidence: c
                            .min_confidence
                            .unwrap_or(correlation_defaults.min_confidence),
                        max_tracked_pairs: c
                            .max_tracked_pairs
                            .unwrap_or(correlation_defaults.max_tracked_pairs),
                    }
                },
                archive: convert_archive_section(&raw.daemon.archive),
            },
            reporting: ReportingConfig {
                intent: raw.reporting.intent,
                confidentiality_level: raw.reporting.confidentiality_level,
                org_config_path: raw.reporting.org_config_path,
                disclose_output_path: raw.reporting.disclose_output_path,
                disclose_period: raw.reporting.disclose_period,
                sigstore: SigstoreConfig {
                    rekor_url: raw
                        .reporting
                        .sigstore
                        .rekor_url
                        .unwrap_or_else(|| DEFAULT_REKOR_URL.to_string()),
                    fulcio_url: raw
                        .reporting
                        .sigstore
                        .fulcio_url
                        .unwrap_or_else(|| DEFAULT_FULCIO_URL.to_string()),
                },
            },
        }
    }
}

/// Convert the raw `[daemon.archive]` TOML section into a typed config.
/// Returns `None` when `path` is absent (the operator did not opt in).
fn convert_archive_section(raw: &ArchiveSection) -> Option<DaemonArchiveConfig> {
    let path = raw.path.clone()?;
    let defaults = DaemonArchiveConfig::default();
    Some(DaemonArchiveConfig {
        path,
        max_size_mb: raw.max_size_mb.unwrap_or(defaults.max_size_mb),
        max_files: raw.max_files.unwrap_or(defaults.max_files),
    })
}

/// Parse a case-insensitive environment string into [`DaemonEnvironment`].
///
/// Returns `None` for any value that is not `"staging"` or `"production"`.
/// Called from [`Config::from`] (which falls back to default on error,
/// deferring the real rejection to [`Config::validate`]).
fn parse_daemon_environment(value: &str) -> Option<DaemonEnvironment> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("staging") {
        Some(DaemonEnvironment::Staging)
    } else if trimmed.eq_ignore_ascii_case("production") {
        Some(DaemonEnvironment::Production)
    } else {
        None
    }
}

/// Convert the raw `[green.cloud]` TOML section into a typed config.
///
/// Returns `None` when `prometheus_endpoint` is absent (section empty
/// or not present). Per-service entries are classified as either
/// `InstanceType` or `ManualWatts` based on which fields are set.
fn convert_cloud_section(raw: &CloudSection) -> Option<CloudEnergyConfig> {
    convert_cloud_section_with_env(raw, || {
        std::env::var("PERF_SENTINEL_CLOUD_AUTH_HEADER").ok()
    })
}

/// Test-friendly inner form: takes the env-var lookup as a closure so
/// tests can exercise the precedence branch without mutating the
/// global process env. Same pattern as
/// [`convert_electricity_maps_section_with_env`].
fn convert_cloud_section_with_env(
    raw: &CloudSection,
    env_lookup: impl FnOnce() -> Option<String>,
) -> Option<CloudEnergyConfig> {
    let endpoint = raw.prometheus_endpoint.as_ref()?;
    let mut services = HashMap::with_capacity(raw.services.len());
    for (name, svc) in &raw.services {
        let config = if svc.idle_watts.is_some() || svc.max_watts.is_some() {
            // Manual watts mode: both must be present (validated later).
            ServiceCloudConfig::ManualWatts {
                idle_watts: svc.idle_watts.unwrap_or(0.0),
                max_watts: svc.max_watts.unwrap_or(0.0),
                cpu_query: svc.cpu_query.clone(),
            }
        } else {
            ServiceCloudConfig::InstanceType {
                provider: svc.provider.clone(),
                instance_type: svc.instance_type.clone().unwrap_or_default(),
                cpu_query: svc.cpu_query.clone(),
            }
        };
        services.insert(name.clone(), config);
    }

    // Auth header: env var takes precedence over config file.
    let from_env = env_lookup();
    let auth_header = from_env.clone().or_else(|| raw.auth_header.clone());
    if from_env.is_none() && raw.auth_header.is_some() {
        tracing::warn!(
            "[green.cloud] auth_header is set in the config file. \
             Prefer the PERF_SENTINEL_CLOUD_AUTH_HEADER environment variable \
             to avoid committing secrets to version control."
        );
    }

    Some(CloudEnergyConfig {
        prometheus_endpoint: endpoint.clone(),
        scrape_interval: Duration::from_secs(raw.scrape_interval_secs.unwrap_or(15)),
        default_provider: raw.default_provider.clone(),
        default_instance_type: raw.default_instance_type.clone(),
        cpu_metric: raw.cpu_metric.clone(),
        services,
        auth_header,
    })
}

/// Convert the raw `[green.scaphandre]` TOML section into a typed config.
///
/// Returns `None` when `endpoint` is absent (section empty or not present).
fn convert_scaphandre_section(raw: &ScaphandreSection) -> Option<ScaphandreConfig> {
    convert_scaphandre_section_with_env(raw, || {
        std::env::var("PERF_SENTINEL_SCAPHANDRE_AUTH_HEADER").ok()
    })
}

/// Test-friendly inner form: takes the env-var lookup as a closure so
/// tests can exercise the precedence branch without mutating the
/// global process env. Same pattern as
/// [`convert_electricity_maps_section_with_env`].
fn convert_scaphandre_section_with_env(
    raw: &ScaphandreSection,
    env_lookup: impl FnOnce() -> Option<String>,
) -> Option<ScaphandreConfig> {
    let endpoint = raw.endpoint.as_ref()?;

    // Auth header: env var takes precedence over config file.
    let from_env = env_lookup();
    let auth_header = from_env.clone().or_else(|| raw.auth_header.clone());
    if from_env.is_none() && raw.auth_header.is_some() {
        tracing::warn!(
            "[green.scaphandre] auth_header is set in the config file. \
             Prefer the PERF_SENTINEL_SCAPHANDRE_AUTH_HEADER environment variable \
             to avoid committing secrets to version control."
        );
    }

    Some(ScaphandreConfig {
        endpoint: endpoint.clone(),
        // Default scrape interval 5s; clamped in validate_green
        // to the [1, 3600] range.
        scrape_interval: Duration::from_secs(raw.scrape_interval_secs.unwrap_or(5)),
        process_map: raw.process_map.clone(),
        auth_header,
    })
}

/// Convert the raw `[green.electricity_maps]` TOML section into a typed config.
///
/// Returns `None` when no `api_key` is set (neither in config nor env var).
fn convert_electricity_maps_section(
    raw: &ElectricityMapsSection,
) -> Option<crate::score::electricity_maps::ElectricityMapsConfig> {
    convert_electricity_maps_section_with_env(raw, || {
        std::env::var("PERF_SENTINEL_EMAPS_TOKEN").ok()
    })
}

/// Test-friendly inner form: takes the env-var lookup as a closure so tests
/// can pass `|| None` instead of mutating the global process env. Avoids the
/// `unsafe` that Rust 2024 requires on `std::env::remove_var` (`set_var` and
/// `remove_var` are data races with other threads inside the same process,
/// including the `cargo test` harness).
fn convert_electricity_maps_section_with_env(
    raw: &ElectricityMapsSection,
    env_lookup: impl FnOnce() -> Option<String>,
) -> Option<crate::score::electricity_maps::ElectricityMapsConfig> {
    // Auth token: env var takes precedence over config file.
    let from_env = env_lookup();
    let token = from_env.clone().or_else(|| raw.api_key.clone())?;

    if token.is_empty() {
        return None;
    }

    // Nudge users toward the env var when the token is in the config file.
    if from_env.is_none() && raw.api_key.is_some() {
        tracing::warn!(
            "[green.electricity_maps] api_key is set in the config file. \
             Prefer the PERF_SENTINEL_EMAPS_TOKEN environment variable \
             to avoid committing secrets to version control."
        );
    }

    let poll_secs = raw.poll_interval_secs.unwrap_or(300);
    // Trim trailing slashes so the URL we build downstream
    // (`format!("{api_endpoint}/carbon-intensity/latest?zone={zone}")`)
    // never produces a double-slash like `.../v4//carbon-intensity/...`,
    // and so `is_legacy_v3_endpoint` matches `.../v3/` (trailing slash).
    let api_endpoint = raw
        .endpoint
        .clone()
        .unwrap_or_else(|| {
            crate::score::electricity_maps::config::DEFAULT_ELECTRICITY_MAPS_ENDPOINT.to_string()
        })
        .trim_end_matches('/')
        .to_string();
    let emission_factor_type =
        crate::score::electricity_maps::config::EmissionFactorType::from_config(
            raw.emission_factor_type.as_deref(),
        );
    let temporal_granularity =
        crate::score::electricity_maps::config::TemporalGranularity::from_config(
            raw.temporal_granularity.as_deref(),
        );
    Some(crate::score::electricity_maps::ElectricityMapsConfig {
        api_endpoint,
        auth_token: token,
        poll_interval: Duration::from_secs(poll_secs),
        // Lowercase region keys so scoring loop lookups match regardless
        // of config casing (same pattern as service_regions).
        region_map: raw
            .region_map
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect(),
        emission_factor_type,
        temporal_granularity,
    })
}

fn check_range<T: PartialOrd + std::fmt::Display>(
    name: &str,
    val: &T,
    min: &T,
    max: &T,
) -> Result<(), String> {
    if val < min {
        return Err(format!("{name} must be >= {min}, got {val}"));
    }
    if val > max {
        return Err(format!("{name} must be <= {max}, got {val}"));
    }
    Ok(())
}

fn check_min<T: PartialOrd + std::fmt::Display>(
    name: &str,
    val: &T,
    min: &T,
) -> Result<(), String> {
    if val < min {
        return Err(format!("{name} must be >= {min}, got {val}"));
    }
    Ok(())
}

/// Emit a single startup warning when `val` is inside the hard bounds but
/// outside the recommended "comfort zone" `[comfort_lo, comfort_hi]`.
///
/// See design doc 07 > "Comfort-zone warnings" for the rationale and the
/// list of bands per field.
fn warn_outside_comfort_zone<T>(
    name: &str,
    val: &T,
    comfort_lo: &T,
    comfort_hi: &T,
    note_low: &str,
    note_high: &str,
) where
    T: PartialOrd + std::fmt::Display,
{
    if val < comfort_lo {
        tracing::warn!(
            field = %name,
            value = %val,
            recommended_min = %comfort_lo,
            "{name} = {val} is below the recommended floor {comfort_lo}; {note_low}"
        );
    } else if val > comfort_hi {
        tracing::warn!(
            field = %name,
            value = %val,
            recommended_max = %comfort_hi,
            "{name} = {val} is above the recommended ceiling {comfort_hi}; {note_high}"
        );
    }
}

/// `true` if `s` contains any ASCII control character (< 0x20 or DEL).
pub(crate) fn has_control_char(s: &str) -> bool {
    s.bytes().any(|b| b < 0x20 || b == 0x7F)
}

/// Validate the wildcard-mode interactions of `[daemon.cors] allowed_origins`.
///
/// - `["*"]` mixed with explicit origins is ambiguous and silently degrades to
///   wildcard mode in `build_cors_layer`. Reject the mix at config load.
/// - `["*"]` combined with `[daemon.ack] api_key` lets any browser origin
///   replay a captured `X-API-Key` header (header-based auth, not blocked by
///   `allow_credentials = false`). Reject the combination.
fn validate_cors_wildcard_mode(
    has_wildcard: bool,
    origin_count: usize,
    has_api_key: bool,
) -> Result<(), String> {
    if has_wildcard && origin_count > 1 {
        return Err(
            "[daemon.cors] allowed_origins cannot mix \"*\" with explicit origins, \
             either use [\"*\"] for wildcard mode or list every origin explicitly"
                .to_string(),
        );
    }
    if has_wildcard && has_api_key {
        return Err(
            "[daemon.cors] allowed_origins = [\"*\"] is incompatible with \
             [daemon.ack] api_key, since X-API-Key is sent on every cross-origin \
             request and would be replayable from any browser tab. \
             Use an explicit origin list or unset api_key for development"
                .to_string(),
        );
    }
    Ok(())
}

/// Validate a single `[daemon.cors] allowed_origins` entry: rejects empty
/// strings, control characters, missing scheme and trailing slashes. The
/// literal `"*"` is accepted (wildcard-mode interactions live in
/// [`validate_cors_wildcard_mode`]).
fn validate_cors_origin(origin: &str) -> Result<(), String> {
    if origin.is_empty() {
        return Err(
            "[daemon.cors] allowed_origins entry is empty, drop it or set a value".to_string(),
        );
    }
    if has_control_char(origin) {
        return Err(format!(
            "[daemon.cors] allowed_origins entry '{origin}' contains control characters"
        ));
    }
    if origin == "*" {
        return Ok(());
    }
    if !(origin.starts_with("http://") || origin.starts_with("https://")) {
        return Err(format!(
            "[daemon.cors] allowed_origins entry '{origin}' must start with http:// or https:// (or be \"*\" for wildcard mode)"
        ));
    }
    if origin.ends_with('/') {
        return Err(format!(
            "[daemon.cors] allowed_origins entry '{origin}' must not end with a trailing slash, an origin is scheme + host + optional port"
        ));
    }
    Ok(())
}

/// Validate the authority portion of an HTTP(S) URI.
/// Rejects credentials, empty host, control characters, and invalid port.
/// Handles IPv6 bracket notation (`[::1]`, `[::1]:8080`).
fn validate_http_authority(url: &str, label: &str) -> Result<(), String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
    if authority.is_empty() {
        return Err(format!("{label} '{url}' has no host"));
    }
    if authority.contains('@') {
        return Err(format!(
            "{label} must not contain credentials (userinfo): '{url}'"
        ));
    }
    if has_control_char(authority) {
        return Err(format!("{label} '{url}' contains control characters"));
    }
    // Port validation: skip for bare IPv6 without port (`[::1]`), handle
    // bracketed IPv6 with port (`[::1]:8080`) via the `]:` delimiter.
    if authority.starts_with('[') {
        // IPv6 bracket notation: port follows `]:` if present.
        if let Some(bracket_end) = authority.find(']') {
            let after_bracket = &authority[bracket_end + 1..];
            if let Some(port_str) = after_bracket.strip_prefix(':')
                && !port_str.is_empty()
                && port_str.parse::<u16>().is_err()
            {
                return Err(format!("{label} '{url}' has an invalid port"));
            }
        }
    } else if let Some(port_str) = authority.rsplit(':').next()
        && authority.contains(':')
        && port_str.parse::<u16>().is_err()
    {
        return Err(format!("{label} '{url}' has an invalid port"));
    }
    Ok(())
}

impl Config {
    /// Validate that config values are within acceptable bounds.
    ///
    /// # Errors
    ///
    /// Returns a `String` description of the first invalid value found.
    /// The caller (`load_from_str`) wraps this in `ConfigError::Validation`.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_daemon_limits()?;
        self.validate_detection_params()?;
        self.validate_rates()?;
        self.validate_tls()?;
        self.validate_green()?;
        self.validate_daemon_ack()?;
        self.validate_daemon_cors()?;
        self.validate_daemon_archive()?;
        self.validate_reporting()?;
        self.validate_cross_section_consistency()?;
        Ok(())
    }

    /// Emit the non-loopback security advisory if applicable.
    ///
    /// The default is `127.0.0.1` (loopback). Advanced users may override
    /// to `0.0.0.0` for container deployments behind a reverse proxy. We
    /// warn loudly rather than rejecting, because the user's intent is
    /// explicit (they changed the config) and a hard reject would force
    /// workarounds (e.g., iptables) that are harder to audit.
    ///
    /// Kept separate from `validate()` because it is the only check
    /// that depends on CLI overrides (`--listen-address`), so the daemon
    /// entrypoint calls it a second time after applying the overrides.
    /// The other advisory warnings inside `validate()` are config-only
    /// and must be emitted exactly once, at load time, to avoid making
    /// an operator believe the daemon validates the same config twice.
    pub fn warn_listen_addr_if_non_loopback(&self) {
        if self.daemon.listen_addr != "127.0.0.1" && self.daemon.listen_addr != "::1" {
            tracing::warn!(
                "Daemon configured to listen on non-loopback address: {}. \
                 Endpoints have no authentication, use a reverse proxy or \
                 network policy for security.",
                self.daemon.listen_addr
            );
        }
    }

    /// Validate `[reporting]` settings. Rejects unknown intent /
    /// confidentiality values and requires `org_config_path` when
    /// `intent = "official"`.
    fn validate_reporting(&self) -> Result<(), String> {
        if let Some(intent) = &self.reporting.intent {
            match intent.as_str() {
                "internal" | "official" | "audited" => {}
                other => {
                    return Err(format!(
                        "[reporting] intent must be one of \"internal\", \"official\", \"audited\", got {other:?}"
                    ));
                }
            }
        }
        if let Some(level) = &self.reporting.confidentiality_level {
            match level.as_str() {
                "internal" | "public" => {}
                other => {
                    return Err(format!(
                        "[reporting] confidentiality_level must be \"internal\" or \"public\", got {other:?}"
                    ));
                }
            }
        }
        if self.reporting.intent.as_deref() == Some("official")
            && self
                .reporting
                .org_config_path
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(
                "[reporting] org_config_path is required when intent = \"official\"".to_string(),
            );
        }
        Ok(())
    }

    /// Reporting-section advisory warnings emitted at load time only.
    /// Kept separate from `validate_reporting` because the daemon
    /// entrypoint re-runs `validate()` after applying CLI overrides
    /// (`--listen-address`, ports), and an advisory not affected by
    /// those overrides must not be re-emitted, otherwise an operator
    /// upgrading 0.6.2 -> 0.7.0 sees the same warning twice and
    /// suspects two daemon instances or a duplicated config layer.
    fn warn_reporting_advisory(&self) {
        if self
            .reporting
            .disclose_output_path
            .as_deref()
            .is_some_and(|p| !p.is_empty())
        {
            tracing::warn!(
                "[reporting] disclose_output_path is set but currently unused. \
                 Reserved for daemon-triggered periodic disclosures (planned for {}). \
                 Reports today are produced exclusively via `perf-sentinel disclose --output`.",
                RESERVED_DISCLOSE_OUTPUT_PATH_VERSION
            );
        }
    }

    /// Validate `[daemon.archive]` settings when present.
    fn validate_daemon_archive(&self) -> Result<(), String> {
        let Some(archive) = &self.daemon.archive else {
            return Ok(());
        };
        if archive.path.trim().is_empty() {
            return Err("[daemon.archive] path must not be empty".to_string());
        }
        if has_control_char(&archive.path) {
            return Err("[daemon.archive] path contains control characters".to_string());
        }
        if archive.max_size_mb < 1 {
            return Err("[daemon.archive] max_size_mb must be >= 1".to_string());
        }
        if archive.max_files < 1 {
            return Err("[daemon.archive] max_files must be >= 1".to_string());
        }
        Ok(())
    }

    /// Cross-section consistency checks that no individual section
    /// can validate alone. Today this is small (CORS-vs-API), but
    /// `validate` is intentionally extensible: any future "you set X
    /// but Y is off" trap belongs here.
    fn validate_cross_section_consistency(&self) -> Result<(), String> {
        if !self.daemon.api_enabled && !self.daemon.cors.allowed_origins.is_empty() {
            return Err(
                "[daemon.cors] allowed_origins is set but [daemon] api_enabled = false. \
                 The CORS layer would attach to a non-mounted /api/* sub-router and \
                 silently do nothing, which is almost always a misconfiguration. \
                 Either remove [daemon.cors] allowed_origins for this environment, or \
                 enable the API with [daemon] api_enabled = true."
                    .to_string(),
            );
        }
        if self.daemon.archive.is_some() && !self.green.enabled {
            return Err(
                "[daemon.archive] is configured but [green] enabled = false. The archive \
                 would write windows with zero carbon/energy, making `perf-sentinel disclose` \
                 produce a meaningless output. Either enable green scoring or remove the \
                 archive section."
                    .to_string(),
            );
        }
        Ok(())
    }

    fn validate_daemon_cors(&self) -> Result<(), String> {
        let has_wildcard = self.daemon.cors.allowed_origins.iter().any(|o| o == "*");
        validate_cors_wildcard_mode(
            has_wildcard,
            self.daemon.cors.allowed_origins.len(),
            self.daemon.ack.api_key.is_some(),
        )?;
        for origin in &self.daemon.cors.allowed_origins {
            validate_cors_origin(origin)?;
        }
        Ok(())
    }

    /// Validate `[daemon.ack]` settings.
    fn validate_daemon_ack(&self) -> Result<(), String> {
        if let Some(key) = &self.daemon.ack.api_key {
            if key.is_empty() {
                return Err("[daemon.ack] api_key must not be empty".to_string());
            }
            if has_control_char(key) {
                return Err("[daemon.ack] api_key contains control characters".to_string());
            }
            // Hard reject obviously-broken keys. The threat model is a
            // co-resident local attacker hitting the loopback API at
            // line rate, with no rate limiting on the daemon side.
            // 36^12 ~= 4.7e18 is well past the brute-force horizon for
            // any realistic deployment, 16+ remains the recommended
            // floor for production.
            if key.len() < 12 {
                return Err(format!(
                    "[daemon.ack] api_key is too short ({} chars), \
                     use at least 12 characters (16 recommended)",
                    key.len()
                ));
            }
            if key.len() < 16 {
                tracing::warn!(
                    len = key.len(),
                    "[daemon.ack] api_key is shorter than 16 characters, \
                     consider a longer secret to resist brute-force attempts"
                );
            }
        }
        if let Some(path) = &self.daemon.ack.storage_path
            && has_control_char(path)
        {
            return Err("[daemon.ack] storage_path contains control characters".to_string());
        }
        if let Some(path) = &self.daemon.ack.toml_path
            && has_control_char(path)
        {
            return Err("[daemon.ack] toml_path contains control characters".to_string());
        }
        Ok(())
    }

    /// Validate TLS configuration: both paths must be set or both absent.
    /// When set, verify the files exist and warn if the key is
    /// world-readable on Unix.
    fn validate_tls(&self) -> Result<(), String> {
        match (&self.daemon.tls.cert_path, &self.daemon.tls.key_path) {
            (Some(cert), Some(key)) => {
                if has_control_char(cert) {
                    return Err("[daemon] tls.cert_path contains control characters".to_string());
                }
                if has_control_char(key) {
                    return Err("[daemon] tls.key_path contains control characters".to_string());
                }
                if !std::path::Path::new(cert).exists() {
                    return Err(format!("[daemon] tls.cert_path '{cert}' does not exist"));
                }
                if !std::path::Path::new(key).exists() {
                    return Err(format!("[daemon] tls.key_path '{key}' does not exist"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(key) {
                        let mode = meta.permissions().mode();
                        if mode & 0o077 != 0 {
                            tracing::warn!(
                                "TLS key file '{key}' is readable by group/others \
                                 (mode {mode:o}). Consider restricting to owner-only \
                                 (chmod 600)."
                            );
                        }
                    }
                }
                tracing::info!("TLS enabled for daemon OTLP receivers (cert: {cert})");
                Ok(())
            }
            (None, None) => Ok(()),
            (Some(_), None) => {
                Err("[daemon] tls.cert_path is set but tls.key_path is missing".to_string())
            }
            (None, Some(_)) => {
                Err("[daemon] tls.key_path is set but tls.cert_path is missing".to_string())
            }
        }
    }

    fn validate_green(&self) -> Result<(), String> {
        Self::validate_embodied_carbon(self.green.embodied_carbon_per_request_gco2)?;
        Self::validate_default_region(self.green.default_region.as_deref())?;
        Self::validate_service_regions(&self.green.service_regions)?;
        if let Some(cfg) = &self.green.scaphandre {
            Self::validate_scaphandre(cfg)?;
        }
        if let Some(cfg) = &self.green.cloud_energy {
            Self::validate_cloud_energy(cfg)?;
        }
        Self::validate_network_energy(self.green.network_energy_per_byte_kwh)?;
        self.validate_hourly_profiles_file()?;
        if let Some(cfg) = &self.green.electricity_maps {
            Self::validate_electricity_maps(cfg)?;
        }
        Ok(())
    }

    fn validate_embodied_carbon(value: f64) -> Result<(), String> {
        if !value.is_finite() {
            return Err(format!(
                "embodied_carbon_per_request_gco2 must be finite, got {value}"
            ));
        }
        if value < 0.0 {
            return Err(format!(
                "embodied_carbon_per_request_gco2 must be >= 0.0, got {value}"
            ));
        }
        Ok(())
    }

    /// Validate the optional `[green] default_region`. Config is trusted
    /// input, so typos surface loudly here rather than silently producing
    /// zeroed CO₂ rows downstream. Same validator used at the OTLP
    /// ingestion boundary (there, invalid values are silently dropped).
    fn validate_default_region(region: Option<&str>) -> Result<(), String> {
        let Some(region) = region else {
            return Ok(());
        };
        if crate::score::carbon::is_valid_region_id(region) {
            return Ok(());
        }
        Err(format!(
            "[green] default_region '{region}' contains invalid characters; \
             expected ASCII alphanumeric + '-' or '_', length 1-64"
        ))
    }

    /// Validate the `[green.service_regions]` map: cardinality cap, plus
    /// region-id syntax on every key/value pair.
    fn validate_service_regions(map: &HashMap<String, String>) -> Result<(), String> {
        /// Maximum number of entries in `[green.service_regions]`.
        /// Bounds the config-load memory footprint against fat-finger or
        /// malicious configs. 1024 is 4× `MAX_REGIONS` (256) and comfortably
        /// above any realistic multi-cloud deployment size.
        const MAX_SERVICE_REGIONS: usize = 1024;
        if map.len() > MAX_SERVICE_REGIONS {
            return Err(format!(
                "[green.service_regions] has {} entries; maximum is {MAX_SERVICE_REGIONS}",
                map.len()
            ));
        }
        for (service, region) in map {
            if !crate::score::carbon::is_valid_region_id(service) {
                return Err(format!(
                    "[green.service_regions] invalid service name '{service}'; \
                     expected ASCII alphanumeric + '-' or '_', length 1-64"
                ));
            }
            if !crate::score::carbon::is_valid_region_id(region) {
                return Err(format!(
                    "[green.service_regions] invalid region '{region}' for service '{service}'; \
                     expected ASCII alphanumeric + '-' or '_', length 1-64"
                ));
            }
        }
        Ok(())
    }

    fn validate_network_energy(value: f64) -> Result<(), String> {
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "network_energy_per_byte_kwh must be finite and >= 0.0, got {value}"
            ));
        }
        Ok(())
    }

    /// Validate `[green] hourly_profiles_file`: reject control characters
    /// in the path (log injection) and require that the file actually
    /// loaded when the field is configured.
    fn validate_hourly_profiles_file(&self) -> Result<(), String> {
        let Some(path) = &self.green.hourly_profiles_file else {
            return Ok(());
        };
        if has_control_char(path) {
            return Err("[green] hourly_profiles_file contains control characters".to_string());
        }
        if self.green.custom_hourly_profiles.is_none() {
            return Err(format!(
                "[green] hourly_profiles_file '{path}' was configured but \
                 failed to load. Remove the field to use embedded profiles only."
            ));
        }
        Ok(())
    }

    /// Validate a parsed `[green.electricity_maps]` config section.
    fn validate_electricity_maps(
        cfg: &crate::score::electricity_maps::ElectricityMapsConfig,
    ) -> Result<(), String> {
        if cfg.auth_token.is_empty() {
            return Err(
                "[green.electricity_maps] api_key or PERF_SENTINEL_EMAPS_TOKEN is required"
                    .to_string(),
            );
        }
        if has_control_char(&cfg.auth_token) {
            return Err(
                "[green.electricity_maps] auth token contains control characters".to_string(),
            );
        }
        validate_http_authority(&cfg.api_endpoint, "[green.electricity_maps] endpoint")?;
        // Warn (but do not fail) when a non-empty auth token travels to an
        // http:// endpoint. The Electricity Maps production API is served
        // over https in practice; an http:// endpoint usually means a local
        // test server or a misconfiguration. Flag it so users do not
        // silently ship credentials in cleartext.
        if cfg.api_endpoint.starts_with("http://") && !cfg.auth_token.is_empty() {
            tracing::warn!(
                "[green.electricity_maps] auth token will be sent over http:// \
                 (no TLS). Use https:// for production or set the endpoint to \
                 a loopback/private address if this is intentional."
            );
        }
        let secs = cfg.poll_interval.as_secs();
        check_range(
            "[green.electricity_maps] poll_interval_secs",
            &secs,
            &60,
            &86400,
        )?;
        if cfg.region_map.is_empty() {
            return Err(
                "[green.electricity_maps] region_map must contain at least one entry".to_string(),
            );
        }
        for (region, zone) in &cfg.region_map {
            if zone.is_empty() {
                return Err(format!(
                    "[green.electricity_maps.region_map] zone for '{region}' is empty"
                ));
            }
            if has_control_char(zone)
                || zone.contains('&')
                || zone.contains('#')
                || zone.contains('=')
                || zone.contains('?')
                || zone.contains('%')
                || zone.contains(' ')
                || zone.contains('+')
            {
                return Err(format!(
                    "[green.electricity_maps.region_map] zone '{zone}' for '{region}' \
                     contains invalid characters"
                ));
            }
            if has_control_char(region) {
                return Err(format!(
                    "[green.electricity_maps.region_map] region key '{region}' \
                     contains control characters"
                ));
            }
        }
        Ok(())
    }

    /// Validate a parsed `[green.scaphandre]` config section.
    ///
    /// Rejects: empty endpoint, non-`http://` scheme, credentials in
    /// authority, control characters, invalid port, `scrape_interval_secs`
    /// outside [1, 3600], and `process_map` keys/values that are empty,
    /// >256 chars, or contain control characters.
    fn validate_scaphandre(cfg: &ScaphandreConfig) -> Result<(), String> {
        if cfg.endpoint.is_empty() {
            return Err(
                "[green.scaphandre] endpoint is required when the section is present".to_string(),
            );
        }
        if !cfg.endpoint.starts_with("http://") && !cfg.endpoint.starts_with("https://") {
            return Err(format!(
                "[green.scaphandre] endpoint '{}' must start with 'http://' or 'https://'",
                cfg.endpoint
            ));
        }
        validate_http_authority(&cfg.endpoint, "[green.scaphandre] endpoint")?;
        let secs = cfg.scrape_interval.as_secs();
        if !(1..=3600).contains(&secs) {
            return Err(format!(
                "[green.scaphandre] scrape_interval_secs must be in [1, 3600], got {secs}"
            ));
        }
        Self::validate_scaphandre_process_map(cfg)?;
        // The `AuthHeader` type lives in the `ingest` module, which is
        // only compiled when hyper is pulled in via one of the daemon /
        // tempo / jaeger-query features. Bare `cargo publish` builds
        // `sentinel-core` with no features and must skip the parse.
        #[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
        if let Some(auth) = cfg.auth_header.as_deref() {
            crate::ingest::auth_header::AuthHeader::parse(auth)
                .map_err(|msg| format!("[green.scaphandre] auth_header: {msg}"))?;
        }
        Ok(())
    }

    /// Validate `[green.scaphandre].process_map` keys and values.
    ///
    /// Service names (keys) and Scaphandre `exe` labels (values) must be
    /// 1 to 256 chars and free of control characters. Service names are
    /// intentionally NOT run through `is_valid_region_id` because they
    /// may legitimately contain dots, slashes and similar.
    fn validate_scaphandre_process_map(cfg: &ScaphandreConfig) -> Result<(), String> {
        for (service, exe) in &cfg.process_map {
            if service.is_empty() || service.len() > 256 {
                return Err(format!(
                    "[green.scaphandre] process_map service name '{service}' must be 1-256 chars"
                ));
            }
            if has_control_char(service) {
                return Err(format!(
                    "[green.scaphandre] process_map service name '{service}' \
                     contains control characters"
                ));
            }
            if exe.is_empty() || exe.len() > 256 {
                return Err(format!(
                    "[green.scaphandre] process_map exe for service '{service}' \
                     must be 1-256 chars, got '{exe}'"
                ));
            }
            if has_control_char(exe) {
                return Err(format!(
                    "[green.scaphandre] process_map exe for service '{service}' \
                     contains control characters"
                ));
            }
        }
        Ok(())
    }

    /// Validate a parsed `[green.cloud]` config section.
    fn validate_cloud_energy(cfg: &CloudEnergyConfig) -> Result<(), String> {
        Self::validate_cloud_endpoint(cfg)?;
        Self::validate_cloud_services(cfg)?;
        // See the twin note in `validate_scaphandre`: the `AuthHeader`
        // type is feature-gated, so bare no-features builds skip it.
        #[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
        if let Some(auth) = cfg.auth_header.as_deref() {
            crate::ingest::auth_header::AuthHeader::parse(auth)
                .map_err(|msg| format!("[green.cloud] auth_header: {msg}"))?;
        }
        Ok(())
    }

    /// Validate `[green.cloud]` endpoint, scrape interval, provider, and instance type.
    fn validate_cloud_endpoint(cfg: &CloudEnergyConfig) -> Result<(), String> {
        if cfg.prometheus_endpoint.is_empty() {
            return Err(
                "[green.cloud] prometheus_endpoint is required when the section is present"
                    .to_string(),
            );
        }
        if !cfg.prometheus_endpoint.starts_with("http://")
            && !cfg.prometheus_endpoint.starts_with("https://")
        {
            return Err(format!(
                "[green.cloud] prometheus_endpoint '{}' must start with 'http://' or 'https://'",
                cfg.prometheus_endpoint
            ));
        }
        validate_http_authority(
            &cfg.prometheus_endpoint,
            "[green.cloud] prometheus_endpoint",
        )?;
        let secs = cfg.scrape_interval.as_secs();
        if !(1..=3600).contains(&secs) {
            return Err(format!(
                "[green.cloud] scrape_interval_secs must be in [1, 3600], got {secs}"
            ));
        }
        if let Some(ref p) = cfg.default_provider
            && !matches!(p.as_str(), "aws" | "gcp" | "azure")
        {
            return Err(format!(
                "[green.cloud] default_provider must be 'aws', 'gcp', or 'azure', got '{p}'"
            ));
        }
        if let Some(ref it) = cfg.default_instance_type
            && !crate::score::cloud_energy::table::is_known_instance_type(it)
        {
            tracing::warn!(
                instance_type = %it,
                "[green.cloud] default_instance_type is not in the embedded \
                 SPECpower table; the provider default watts will be used"
            );
        }
        if let Some(ref m) = cfg.cpu_metric
            && has_control_char(m)
        {
            return Err("[green.cloud] cpu_metric contains control characters".to_string());
        }
        Ok(())
    }

    /// Validate per-service entries in `[green.cloud.services]`: cardinality
    /// cap, name/control-char checks, watts ranges, instance type lookup.
    fn validate_cloud_services(cfg: &CloudEnergyConfig) -> Result<(), String> {
        const MAX_CLOUD_SERVICES: usize = 256;
        if cfg.services.len() > MAX_CLOUD_SERVICES {
            return Err(format!(
                "[green.cloud.services] has {} entries; maximum is {MAX_CLOUD_SERVICES}",
                cfg.services.len()
            ));
        }
        for (service, svc_cfg) in &cfg.services {
            Self::validate_cloud_service_name(service)?;
            Self::validate_cloud_service_cpu_query(service, svc_cfg)?;
            match svc_cfg {
                ServiceCloudConfig::ManualWatts {
                    idle_watts,
                    max_watts,
                    ..
                } => Self::validate_manual_watts(service, *idle_watts, *max_watts)?,
                ServiceCloudConfig::InstanceType {
                    provider,
                    instance_type,
                    ..
                } => Self::validate_instance_type_variant(
                    service,
                    provider.as_deref(),
                    instance_type,
                )?,
            }
        }
        Ok(())
    }

    /// Shape + control-char check on a cloud service name.
    fn validate_cloud_service_name(service: &str) -> Result<(), String> {
        if service.is_empty() || service.len() > 256 {
            return Err(format!(
                "[green.cloud.services] service name '{service}' must be 1-256 chars"
            ));
        }
        if has_control_char(service) {
            return Err(format!(
                "[green.cloud.services] service name '{service}' contains control characters"
            ));
        }
        Ok(())
    }

    /// Reject control characters in a service's optional per-service
    /// `cpu_query` override (log-injection / Prometheus-label-injection
    /// guard).
    fn validate_cloud_service_cpu_query(
        service: &str,
        svc_cfg: &ServiceCloudConfig,
    ) -> Result<(), String> {
        let Some(q) = svc_cfg.cpu_query() else {
            return Ok(());
        };
        if has_control_char(q) {
            return Err(format!(
                "[green.cloud.services.{service}] cpu_query contains control characters"
            ));
        }
        Ok(())
    }

    /// Validate a [`ServiceCloudConfig::ManualWatts`] arm: both values
    /// finite and non-negative, and `max_watts >= idle_watts`.
    fn validate_manual_watts(service: &str, idle_watts: f64, max_watts: f64) -> Result<(), String> {
        if !idle_watts.is_finite() || idle_watts < 0.0 {
            return Err(format!(
                "[green.cloud.services.{service}] idle_watts must be finite and >= 0, \
                 got {idle_watts}"
            ));
        }
        if !max_watts.is_finite() || max_watts < 0.0 {
            return Err(format!(
                "[green.cloud.services.{service}] max_watts must be finite and >= 0, \
                 got {max_watts}"
            ));
        }
        if max_watts < idle_watts {
            return Err(format!(
                "[green.cloud.services.{service}] max_watts ({max_watts}) must be \
                 >= idle_watts ({idle_watts})"
            ));
        }
        Ok(())
    }

    /// Validate a [`ServiceCloudConfig::InstanceType`] arm: provider
    /// allow-list, control-char rejection on `instance_type`, and a
    /// soft warning when the type is not in the embedded `SPECpower`
    /// table (not an error, the provider default is used instead).
    fn validate_instance_type_variant(
        service: &str,
        provider: Option<&str>,
        instance_type: &str,
    ) -> Result<(), String> {
        if let Some(p) = provider
            && !matches!(p, "aws" | "gcp" | "azure")
        {
            return Err(format!(
                "[green.cloud.services.{service}] provider must be 'aws', 'gcp', \
                 or 'azure', got '{p}'"
            ));
        }
        if has_control_char(instance_type) {
            return Err(format!(
                "[green.cloud.services.{service}] instance_type contains control characters"
            ));
        }
        if !instance_type.is_empty()
            && !crate::score::cloud_energy::table::is_known_instance_type(instance_type)
        {
            tracing::warn!(
                service = %service,
                instance_type = %instance_type,
                "[green.cloud.services] instance_type is not in the embedded \
                 SPECpower table; provider default watts will be used"
            );
        }
        Ok(())
    }

    fn validate_daemon_limits(&self) -> Result<(), String> {
        check_range(
            "max_payload_size",
            &self.daemon.max_payload_size,
            &1024,
            &(100 * 1024 * 1024),
        )?;
        check_range(
            "max_active_traces",
            &self.daemon.max_active_traces,
            &1,
            &1_000_000,
        )?;
        check_range(
            "max_events_per_trace",
            &self.daemon.max_events_per_trace,
            &1,
            &100_000,
        )?;
        // 0 is documented as "disable the findings store entirely". Cap
        // the upper end at 10M so a typo can't OOM the daemon.
        check_range(
            "max_retained_findings",
            &self.daemon.max_retained_findings,
            &0,
            &10_000_000,
        )?;
        check_range("trace_ttl_ms", &self.daemon.trace_ttl_ms, &100, &3_600_000)?;
        check_range("listen_port_http", &self.daemon.listen_port, &1, &65535)?;
        check_range(
            "listen_port_grpc",
            &self.daemon.listen_port_grpc,
            &1,
            &65535,
        )?;
        self.warn_unusual_daemon_limits();
        Ok(())
    }

    /// Soft startup warnings for daemon-limit values inside the hard
    /// bounds but outside their recommended comfort zone.
    ///
    /// See design doc 07 > "Comfort-zone warnings" for the band table
    /// and the rationale.
    fn warn_unusual_daemon_limits(&self) {
        // The 16 MiB ceiling intentionally matches the `max_payload_size`
        // default value (see line 205). Default-at-ceiling is inclusive
        // (`..=`), so the canonical config emits no warning. A future
        // bump of the default must also raise this ceiling, otherwise
        // every fresh daemon would log a startup warning.
        warn_outside_comfort_zone(
            "max_payload_size",
            &self.daemon.max_payload_size,
            &(256 * 1024),
            &(16 * 1024 * 1024),
            "tiny payloads may reject legitimate OTLP batches",
            "large payloads increase ingest latency and memory pressure",
        );
        warn_outside_comfort_zone(
            "max_active_traces",
            &self.daemon.max_active_traces,
            &1_000,
            &100_000,
            "aggressive LRU eviction is likely under load",
            "memory footprint grows roughly linearly with this cap",
        );
        warn_outside_comfort_zone(
            "max_events_per_trace",
            &self.daemon.max_events_per_trace,
            &100,
            &10_000,
            "complex traces will be truncated by the per-trace ring buffer",
            "very wide ring buffers rarely improve detection quality",
        );
        // Skip the comfort-zone check when the store is intentionally
        // disabled (max_retained_findings == 0); warning on that would
        // be noise.
        if self.daemon.max_retained_findings > 0 {
            warn_outside_comfort_zone(
                "max_retained_findings",
                &self.daemon.max_retained_findings,
                &100,
                &100_000,
                "old findings will be evicted before /api/findings can serve them",
                "the findings store will hold a large in-memory backlog",
            );
        }
        warn_outside_comfort_zone(
            "trace_ttl_ms",
            &self.daemon.trace_ttl_ms,
            &1_000,
            &600_000,
            "TTL below 1s flushes traces before slow spans land",
            "TTL above 10min keeps near-dead traces in the active set",
        );
    }

    fn validate_detection_params(&self) -> Result<(), String> {
        check_min(
            "n_plus_one_threshold",
            &self.detection.n_plus_one_threshold,
            &1,
        )?;
        check_min("window_duration_ms", &self.detection.window_duration_ms, &1)?;
        check_min(
            "slow_query_threshold_ms",
            &self.detection.slow_query_threshold_ms,
            &1,
        )?;
        check_min(
            "slow_query_min_occurrences",
            &self.detection.slow_query_min_occurrences,
            &1,
        )?;
        check_range("max_fanout", &self.detection.max_fanout, &1, &100_000)?;
        warn_outside_comfort_zone(
            "max_fanout",
            &self.detection.max_fanout,
            &5,
            &1_000,
            "very low fanout floods the findings store with noise",
            "very high fanout suppresses most fan-out detections",
        );
        check_min(
            "chatty_service_min_calls",
            &self.detection.chatty_service_min_calls,
            &1,
        )?;
        check_min(
            "pool_saturation_concurrent_threshold",
            &self.detection.pool_saturation_concurrent_threshold,
            &2,
        )?;
        check_min(
            "serialized_min_sequential",
            &self.detection.serialized_min_sequential,
            &2,
        )?;
        Ok(())
    }

    fn validate_rates(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.daemon.sampling_rate) {
            return Err(format!(
                "sampling_rate must be in [0.0, 1.0], got {}",
                self.daemon.sampling_rate
            ));
        }
        if !(0.0..=1.0).contains(&self.thresholds.io_waste_ratio_max) {
            return Err(format!(
                "io_waste_ratio_max must be in [0.0, 1.0], got {}",
                self.thresholds.io_waste_ratio_max
            ));
        }
        Ok(())
    }
}

/// Top-level TOML keys that perf-sentinel accepted in 0.5.x as legacy
/// flat aliases for sectioned fields. Removed in 0.6.0; loading a config
/// that still uses any of them returns
/// [`ConfigError::Validation`] with the new section path so the operator
/// can migrate without grep-around. Tuple is `(legacy_top_level_key,
/// new_section_path)`. The list is intentionally exhaustive: a 0.5.x
/// config that loads on 0.6.x without a clear error is the worst-case
/// outcome we want to avoid.
const REMOVED_LEGACY_TOP_LEVEL_KEYS: &[(&str, &str)] = &[
    (
        "n_plus_one_threshold",
        "[detection] n_plus_one_min_occurrences",
    ),
    ("window_duration_ms", "[detection] window_duration_ms"),
    ("listen_addr", "[daemon] listen_address"),
    ("listen_port", "[daemon] listen_port_http"),
    ("max_active_traces", "[daemon] max_active_traces"),
    ("trace_ttl_ms", "[daemon] trace_ttl_ms"),
    ("max_events_per_trace", "[daemon] max_events_per_trace"),
    ("max_payload_size", "[daemon] max_payload_size"),
];

/// Reject 0.5.x legacy top-level keys with a migration hint.
///
/// Runs before the typed `RawConfig` parse: a typed parse with no
/// `deny_unknown_fields` would silently drop these keys (operator never
/// sees a warning, defaults silently apply). A typed parse WITH
/// `deny_unknown_fields` would surface a serde error like "unknown field
/// `listen_port`" without the migration path. The bespoke check below
/// prints both pieces of information in one error.
fn reject_legacy_top_level_keys(content: &str) -> Result<(), ConfigError> {
    let value: toml::Value = toml::from_str(content).map_err(ConfigError::Parse)?;
    let toml::Value::Table(table) = value else {
        return Ok(());
    };
    for (legacy, replacement) in REMOVED_LEGACY_TOP_LEVEL_KEYS {
        if table.contains_key(*legacy) {
            return Err(ConfigError::Validation(format!(
                "config: top-level '{legacy}' was removed in 0.6.0; \
                 use '{replacement}' instead. \
                 See the 0.6.0 migration notes for the full list of renamed keys."
            )));
        }
    }
    Ok(())
}

/// Load configuration from a TOML string.
///
/// Validates that all values are within acceptable bounds after parsing.
///
/// # Errors
///
/// Returns `ConfigError::Parse` if the TOML is malformed, or
/// `ConfigError::Validation` if a field value is out of bounds, or if a
/// 0.5.x legacy top-level key is present (see
/// [`REMOVED_LEGACY_TOP_LEVEL_KEYS`]).
pub fn load_from_str(content: &str) -> Result<Config, ConfigError> {
    let normalized = normalize_toml_path_strings(content);
    reject_legacy_top_level_keys(normalized.as_ref())?;
    let raw: RawConfig = match toml::from_str(normalized.as_ref()) {
        Ok(raw) => raw,
        Err(norm_err) => {
            if matches!(normalized, Cow::Owned(_)) {
                // Path normalization fallback. See design doc 07 >
                // "Windows path normalization" for the rationale.
                tracing::debug!(
                    normalized_error = %norm_err,
                    "path normalization produced invalid TOML; retrying with original input"
                );
                toml::from_str(content).map_err(ConfigError::Parse)?
            } else {
                return Err(ConfigError::Parse(norm_err));
            }
        }
    };
    // Validate before the lossy `Config::from` conversion: a typo like
    // `envrionment = "prod"` would otherwise silently downgrade to
    // Staging instead of erroring.
    if let Some(env_str) = raw.daemon.environment.as_deref()
        && parse_daemon_environment(env_str).is_none()
    {
        return Err(ConfigError::Validation(format!(
            "[daemon] environment '{env_str}' is invalid; \
             expected 'staging' or 'production' (case-insensitive)"
        )));
    }
    let config = Config::from(raw);
    config.validate().map_err(ConfigError::Validation)?;
    config.warn_listen_addr_if_non_loopback();
    config.warn_reporting_advisory();
    Ok(config)
}

/// Errors that can occur during configuration loading.
///
/// `#[non_exhaustive]` so that adding future variants (e.g. a new
/// validation failure when a new config section lands) stays a
/// SemVer-minor change.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// TOML parsing error.
    #[error("config parse error: {0}")]
    Parse(#[from] toml::de::Error),
    /// Validation error (out-of-range values).
    #[error("config validation error: {0}")]
    Validation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_safe_defaults() {
        let config = Config::default();
        assert_eq!(config.daemon.max_payload_size, 16 * 1024 * 1024);
        assert_eq!(config.daemon.listen_addr, "127.0.0.1");
        assert_eq!(config.detection.n_plus_one_threshold, 5);
        assert_eq!(config.detection.window_duration_ms, 500);
        assert_eq!(config.daemon.trace_ttl_ms, 30_000);
        assert_eq!(config.daemon.max_active_traces, 10_000);
        assert_eq!(config.daemon.max_events_per_trace, 1_000);
    }

    #[test]
    fn parse_empty_toml_gives_defaults() {
        let config = load_from_str("").unwrap();
        assert_eq!(config.daemon.max_payload_size, 16 * 1024 * 1024);
    }

    #[test]
    fn parse_partial_toml() {
        let config = load_from_str("[detection]\nn_plus_one_min_occurrences = 10").unwrap();
        assert_eq!(config.detection.n_plus_one_threshold, 10);
        assert_eq!(config.daemon.max_payload_size, 16 * 1024 * 1024); // default preserved
    }

    #[test]
    fn parse_window_config() {
        let config = load_from_str(
            "[detection]\nwindow_duration_ms = 1000\n\
             [daemon]\ntrace_ttl_ms = 60000\nmax_active_traces = 5000",
        )
        .unwrap();
        assert_eq!(config.detection.window_duration_ms, 1000);
        assert_eq!(config.daemon.trace_ttl_ms, 60_000);
        assert_eq!(config.daemon.max_active_traces, 5000);
    }

    #[test]
    fn parse_sectioned_format() {
        let toml = r#"
[thresholds]
n_plus_one_sql_critical_max = 2
n_plus_one_http_warning_max = 5
io_waste_ratio_max = 0.50

[detection]
window_duration_ms = 1000
n_plus_one_min_occurrences = 10

[green]
enabled = false

[daemon]
listen_address = "0.0.0.0"
listen_port_http = 9418
listen_port_grpc = 9417
json_socket = "/var/run/perf-sentinel.sock"
max_active_traces = 20000
trace_ttl_ms = 60000
sampling_rate = 0.5
max_events_per_trace = 500
max_payload_size = 2097152
"#;
        let config = load_from_str(toml).unwrap();
        assert_eq!(config.thresholds.n_plus_one_sql_critical_max, 2);
        assert_eq!(config.thresholds.n_plus_one_http_warning_max, 5);
        assert!((config.thresholds.io_waste_ratio_max - 0.50).abs() < f64::EPSILON);
        assert_eq!(config.detection.n_plus_one_threshold, 10);
        assert_eq!(config.detection.window_duration_ms, 1000);
        assert!(!config.green.enabled);
        assert_eq!(config.daemon.listen_addr, "0.0.0.0");
        assert_eq!(config.daemon.listen_port, 9418);
        assert_eq!(config.daemon.listen_port_grpc, 9417);
        assert_eq!(config.daemon.json_socket, "/var/run/perf-sentinel.sock");
        assert_eq!(config.daemon.max_active_traces, 20_000);
        assert_eq!(config.daemon.trace_ttl_ms, 60_000);
        assert!((config.daemon.sampling_rate - 0.5).abs() < f64::EPSILON);
        assert_eq!(config.daemon.max_events_per_trace, 500);
        assert_eq!(config.daemon.max_payload_size, 2_097_152);
    }

    // 0.6.0 breaking change: the 8 legacy top-level flats deprecated in
    // 0.5.26 are now removed. Loading a config that still uses them must
    // fail loudly with a migration message rather than silently accept
    // the file with the legacy key ignored. Each test below covers one
    // (key, replacement) pair so a regression is easy to attribute.

    fn assert_legacy_top_level_rejected(toml: &str, key: &str, replacement: &str) {
        let err = load_from_str(toml).expect_err("legacy top-level key must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains(key),
            "error must name the legacy key '{key}': {msg}"
        );
        assert!(
            msg.contains(replacement),
            "error must point at '{replacement}': {msg}"
        );
        assert!(
            msg.contains("0.6.0"),
            "error must tag the breaking-change version: {msg}"
        );
    }

    #[test]
    fn legacy_flat_n_plus_one_threshold_rejected_with_migration_hint() {
        assert_legacy_top_level_rejected(
            "n_plus_one_threshold = 7\n",
            "n_plus_one_threshold",
            "[detection] n_plus_one_min_occurrences",
        );
    }

    #[test]
    fn legacy_flat_window_duration_ms_rejected_with_migration_hint() {
        assert_legacy_top_level_rejected(
            "window_duration_ms = 1500\n",
            "window_duration_ms",
            "[detection] window_duration_ms",
        );
    }

    #[test]
    fn legacy_flat_listen_addr_rejected_with_migration_hint() {
        assert_legacy_top_level_rejected(
            "listen_addr = \"0.0.0.0\"\n",
            "listen_addr",
            "[daemon] listen_address",
        );
    }

    #[test]
    fn legacy_flat_listen_port_rejected_with_migration_hint() {
        assert_legacy_top_level_rejected(
            "listen_port = 9418\n",
            "listen_port",
            "[daemon] listen_port_http",
        );
    }

    #[test]
    fn legacy_flat_max_active_traces_rejected_with_migration_hint() {
        assert_legacy_top_level_rejected(
            "max_active_traces = 5000\n",
            "max_active_traces",
            "[daemon] max_active_traces",
        );
    }

    #[test]
    fn legacy_flat_trace_ttl_ms_rejected_with_migration_hint() {
        assert_legacy_top_level_rejected(
            "trace_ttl_ms = 60000\n",
            "trace_ttl_ms",
            "[daemon] trace_ttl_ms",
        );
    }

    #[test]
    fn legacy_flat_max_events_per_trace_rejected_with_migration_hint() {
        assert_legacy_top_level_rejected(
            "max_events_per_trace = 250\n",
            "max_events_per_trace",
            "[daemon] max_events_per_trace",
        );
    }

    #[test]
    fn legacy_flat_max_payload_size_rejected_with_migration_hint() {
        assert_legacy_top_level_rejected(
            "max_payload_size = 1048576\n",
            "max_payload_size",
            "[daemon] max_payload_size",
        );
    }

    #[test]
    fn empty_config_yields_defaults_after_legacy_removal() {
        let config = load_from_str("").unwrap();
        let d = Config::default();
        assert_eq!(
            config.detection.n_plus_one_threshold,
            d.detection.n_plus_one_threshold
        );
        assert_eq!(
            config.detection.window_duration_ms,
            d.detection.window_duration_ms
        );
        assert_eq!(config.daemon.listen_addr, d.daemon.listen_addr);
        assert_eq!(config.daemon.listen_port, d.daemon.listen_port);
        assert_eq!(config.daemon.max_active_traces, d.daemon.max_active_traces);
        assert_eq!(config.daemon.trace_ttl_ms, d.daemon.trace_ttl_ms);
        assert_eq!(
            config.daemon.max_events_per_trace,
            d.daemon.max_events_per_trace
        );
        assert_eq!(config.daemon.max_payload_size, d.daemon.max_payload_size);
    }

    #[test]
    fn parse_sanitizer_aware_classification_modes() {
        use crate::detect::sanitizer_aware::SanitizerAwareMode;

        let default_config = load_from_str("").unwrap();
        assert_eq!(
            default_config.detection.sanitizer_aware_classification,
            SanitizerAwareMode::Auto
        );

        for (value, expected) in [
            ("auto", SanitizerAwareMode::Auto),
            ("always", SanitizerAwareMode::Always),
            ("never", SanitizerAwareMode::Never),
            ("ALWAYS", SanitizerAwareMode::Always),
            ("strict", SanitizerAwareMode::Strict),
            ("STRICT", SanitizerAwareMode::Strict),
        ] {
            let toml = format!("[detection]\nsanitizer_aware_classification = \"{value}\"\n");
            let config = load_from_str(&toml).unwrap();
            assert_eq!(
                config.detection.sanitizer_aware_classification, expected,
                "value: {value}"
            );
        }

        let unknown =
            load_from_str("[detection]\nsanitizer_aware_classification = \"unknown\"\n").unwrap();
        assert_eq!(
            unknown.detection.sanitizer_aware_classification,
            SanitizerAwareMode::Auto,
            "unknown value should fall back to Auto"
        );
    }

    #[test]
    fn parse_windows_style_json_socket_path_in_basic_string() {
        let config = load_from_str(
            r#"
[daemon]
json_socket = "C:\temp\perf-sentinel.sock"
"#,
        )
        .unwrap();
        assert_eq!(config.daemon.json_socket, r"C:\temp\perf-sentinel.sock");
    }

    #[test]
    fn parse_escaped_windows_style_json_socket_path_stays_stable() {
        let config = load_from_str(
            r#"
[daemon]
json_socket = "C:\\temp\\perf-sentinel.sock"
"#,
        )
        .unwrap();
        assert_eq!(config.daemon.json_socket, r"C:\temp\perf-sentinel.sock");
    }

    #[test]
    fn parse_windows_style_json_socket_path_with_trailing_comment() {
        // Covers `find_basic_string_end` stopping before `#`, a common
        // hand-edited config shape the initial test matrix missed.
        let config = load_from_str(
            "[daemon]\n\
             json_socket = \"C:\\temp\\sock\" # inline note\n",
        )
        .unwrap();
        assert_eq!(config.daemon.json_socket, r"C:\temp\sock");
    }

    #[test]
    fn parse_unc_json_socket_path_preserves_double_leading_backslash() {
        // Raw UNC `\\server\share\sock` must round-trip verbatim. The
        // `raw_unc_prefix` branch in `escape_toml_path_backslashes`
        // emits 4 leading `\` so TOML decode yields 2.
        let config = load_from_str(
            r#"
[daemon]
json_socket = "\\server\share\sock"
"#,
        )
        .unwrap();
        assert_eq!(config.daemon.json_socket, r"\\server\share\sock");
    }

    #[test]
    fn parse_pre_escaped_unc_json_socket_path_is_stable() {
        let config = load_from_str(
            r#"
[daemon]
json_socket = "\\\\server\\share\\sock"
"#,
        )
        .unwrap();
        assert_eq!(config.daemon.json_socket, r"\\server\share\sock");
    }

    #[test]
    fn literal_string_windows_path_bypasses_normalization() {
        // TOML literal strings (`'...'`) already treat `\` literally.
        // Our normalizer must not touch them; checked indirectly by
        // confirming the parser accepts a path with lone `\` inside `'`.
        let config = load_from_str(
            r"
[daemon]
json_socket = 'C:\temp\sock'
",
        )
        .unwrap();
        assert_eq!(config.daemon.json_socket, r"C:\temp\sock");
    }

    #[test]
    fn normalization_applies_to_tls_cert_and_key_paths() {
        // TLS paths are validated as filesystem entries, so a non-existent
        // literal yields ConfigError::Validation. The test passes iff the
        // error message surfaces the expected *normalized* path, i.e. our
        // rewriter reached both keys before validation ran.
        let err = load_from_str(
            r#"
[daemon]
tls_cert_path = "C:\certs\server.crt"
tls_key_path = "C:\certs\server.key"
"#,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains(r"C:\certs\server.crt") || msg.contains(r"C:\certs\server.key"),
            "expected normalized TLS path in error, got: {msg}"
        );
    }

    #[test]
    fn normalization_applies_to_all_registered_path_keys() {
        // Guard against a copy-paste bug in `TOML_PATH_STRING_KEYS`:
        // exercise each key via the unit-level normalizer rather than
        // the full loader (hourly/calibration files would otherwise
        // trigger disk I/O or validation noise).
        for key in TOML_PATH_STRING_KEYS {
            let line = format!("{key} = \"C:\\temp\\x\"\n");
            let rewritten = normalize_toml_path_strings(&line);
            assert!(
                matches!(rewritten, Cow::Owned(_)),
                "{key}: expected normalization to rewrite bare Windows path"
            );
            assert!(
                rewritten.as_ref().contains(r#""C:\\temp\\x""#),
                "{key}: normalized output missing escaped path, got {rewritten}"
            );
        }
    }

    #[test]
    fn normalization_leaves_toml_escape_sequences_literal_in_path_keys() {
        // `\t` and `\n` inside a path key are treated as literal
        // backslash-sequences, not TOML escapes. This is by design for
        // `TOML_PATH_STRING_KEYS` and documented in the helper's rustdoc.
        let config = load_from_str(
            r#"
[daemon]
json_socket = "C:\new\tmp\sock"
"#,
        )
        .unwrap();
        assert_eq!(config.daemon.json_socket, r"C:\new\tmp\sock");
    }

    #[test]
    fn load_from_str_falls_back_when_original_error_is_unrelated_to_path() {
        // Force the normalization branch (Cow::Owned) via a Windows path,
        // then introduce a type mismatch on a strictly-typed key
        // (`listen_port` is `u16`). Both the normalized and the original
        // parse fail; we just assert we surface a ConfigError::Parse
        // rather than silently masking the issue.
        let err = load_from_str(
            r#"
[daemon]
json_socket = "C:\temp\sock"
sampling_rate = "not a number"
"#,
        )
        .unwrap_err();
        assert!(
            matches!(err, ConfigError::Parse(_)),
            "expected ConfigError::Parse, got {err:?}"
        );
    }

    #[test]
    fn find_basic_string_end_handles_escaped_inner_quote() {
        // `"a\"b"`: the first `"` at byte 3 is escaped, real end at byte
        // 5. Guards the linear `run`-counter rewrite against regressions
        // that would terminate too early.
        let value = r#""a\"b""#;
        assert_eq!(find_basic_string_end(value), Some(5));
    }

    #[test]
    fn find_basic_string_end_survives_very_long_backslash_run() {
        // Previously the lookbehind was O(n²); this is a smoke test
        // that a pathological input completes in well under the test
        // timeout. If this regresses to quadratic, it still passes,
        // but the timing would blow up.
        let mut input = String::from("\"");
        input.extend(std::iter::repeat_n('\\', 10_000));
        input.push('"');
        // 10_000 backslashes → 5_000 `\\` pairs → closing `"` valid.
        assert_eq!(find_basic_string_end(&input), Some(10_001));
    }

    #[test]
    fn detection_section_drives_thresholds() {
        let toml = r"
[detection]
n_plus_one_min_occurrences = 12
window_duration_ms = 800
";
        let config = load_from_str(toml).unwrap();
        assert_eq!(config.detection.n_plus_one_threshold, 12);
        assert_eq!(config.detection.window_duration_ms, 800);
    }

    #[test]
    fn new_fields_have_correct_defaults() {
        let config = Config::default();
        assert_eq!(config.thresholds.n_plus_one_sql_critical_max, 0);
        assert_eq!(config.thresholds.n_plus_one_http_warning_max, 3);
        assert!((config.thresholds.io_waste_ratio_max - 0.30).abs() < f64::EPSILON);
        assert!(config.green.enabled);
        assert_eq!(config.daemon.listen_port_grpc, 4317);
        assert_eq!(config.daemon.json_socket, "/tmp/perf-sentinel.sock");
        assert!((config.daemon.sampling_rate - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn default_config_validates() {
        let config = Config::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_sampling_rate_above_one() {
        let result = load_from_str("[daemon]\nsampling_rate = 5.0");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("sampling_rate"), "got: {err}");
    }

    #[test]
    fn rejects_negative_sampling_rate() {
        let result = load_from_str("[daemon]\nsampling_rate = -0.1");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_io_waste_ratio_max_above_one() {
        let result = load_from_str("[thresholds]\nio_waste_ratio_max = 1.5");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("io_waste_ratio_max"), "got: {err}");
    }

    #[test]
    fn rejects_zero_max_payload_size() {
        let result = load_from_str("[daemon]\nmax_payload_size = 0");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_zero_n_plus_one_threshold() {
        let result = load_from_str("n_plus_one_threshold = 0");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_zero_max_active_traces() {
        let result = load_from_str("max_active_traces = 0");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_zero_max_events_per_trace() {
        let result = load_from_str("max_events_per_trace = 0");
        assert!(result.is_err());
    }

    #[test]
    fn slow_query_defaults() {
        let config = Config::default();
        assert_eq!(config.detection.slow_query_threshold_ms, 500);
        assert_eq!(config.detection.slow_query_min_occurrences, 3);
        assert!(config.green.default_region.is_none());
        assert!(config.green.service_regions.is_empty());
        assert!(
            (config.green.embodied_carbon_per_request_gco2
                - DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2)
                .abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn parse_slow_query_config() {
        let toml = r"
[detection]
slow_query_threshold_ms = 1000
slow_query_min_occurrences = 5
";
        let config = load_from_str(toml).unwrap();
        assert_eq!(config.detection.slow_query_threshold_ms, 1000);
        assert_eq!(config.detection.slow_query_min_occurrences, 5);
    }

    #[test]
    fn parse_green_default_region() {
        let toml = r#"
[green]
enabled = true
default_region = "eu-west-3"
"#;
        let config = load_from_str(toml).unwrap();
        assert_eq!(config.green.default_region.as_deref(), Some("eu-west-3"));
    }

    #[test]
    fn parse_green_service_regions() {
        let toml = r#"
[green]
enabled = true
default_region = "eu-west-3"

[green.service_regions]
"order-svc" = "us-east-1"
"chat-svc" = "ap-southeast-1"
"#;
        let config = load_from_str(toml).unwrap();
        assert_eq!(config.green.service_regions.len(), 2);
        assert_eq!(
            config
                .green
                .service_regions
                .get("order-svc")
                .map(String::as_str),
            Some("us-east-1")
        );
        assert_eq!(
            config
                .green
                .service_regions
                .get("chat-svc")
                .map(String::as_str),
            Some("ap-southeast-1")
        );
    }

    #[test]
    fn parse_green_embodied_carbon_override() {
        let toml = r"
[green]
enabled = true
embodied_carbon_per_request_gco2 = 0.005
";
        let config = load_from_str(toml).unwrap();
        assert!((config.green.embodied_carbon_per_request_gco2 - 0.005).abs() < f64::EPSILON);
    }

    #[test]
    fn rejects_negative_embodied_carbon() {
        let result = load_from_str("[green]\nembodied_carbon_per_request_gco2 = -0.001");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("embodied_carbon_per_request_gco2"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_zero_embodied_carbon() {
        let toml = r"
[green]
embodied_carbon_per_request_gco2 = 0.0
";
        let config = load_from_str(toml).unwrap();
        assert!((config.green.embodied_carbon_per_request_gco2 - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_service_regions_default() {
        let toml = r#"
[green]
default_region = "eu-west-3"
"#;
        let config = load_from_str(toml).unwrap();
        assert!(config.green.service_regions.is_empty());
    }

    // ----- Region validation + lowercase + both-set -----

    #[test]
    fn rejects_invalid_default_region_characters() {
        // Space in region name: log-injection protection at config load.
        let result = load_from_str("[green]\ndefault_region = \"eu west 3\"");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("default_region"),
            "error should mention default_region, got: {err}"
        );
    }

    #[test]
    fn rejects_oversized_default_region() {
        // 65 chars, just over the 64-char cap.
        let long_region = "a".repeat(65);
        let toml = format!("[green]\ndefault_region = \"{long_region}\"");
        let result = load_from_str(&toml);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_default_region_with_newline_escape() {
        // In a TOML basic string, `\n` is an escape sequence for a real
        // newline byte. The validator must reject the resulting control
        // char to block log-forging via default_region.
        let result = load_from_str("[green]\ndefault_region = \"eu-west-3\\n\"");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("default_region"),
            "error should mention default_region"
        );
    }

    #[test]
    fn rejects_default_region_with_literal_newline() {
        // Multi-line basic string with an actual newline byte in the
        // value. Also rejected at load time.
        let result = load_from_str("[green]\ndefault_region = \"\"\"eu-west-3\n\"\"\"");
        assert!(result.is_err());
    }

    #[test]
    fn accepts_known_regions() {
        // Sanity: all known region names pass the validator.
        for region in ["eu-west-3", "us-east-1", "fr", "mars-1", "unknown"] {
            let toml = format!("[green]\ndefault_region = \"{region}\"");
            let config = load_from_str(&toml)
                .unwrap_or_else(|e| panic!("region '{region}' should be accepted, got error: {e}"));
            assert_eq!(config.green.default_region.as_deref(), Some(region));
        }
    }

    #[test]
    fn rejects_invalid_service_regions_service_name() {
        let toml = r#"
[green.service_regions]
"bad service" = "us-east-1"
"#;
        let result = load_from_str(toml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("service_regions"),
            "error should mention service_regions, got: {err}"
        );
    }

    #[test]
    fn rejects_invalid_service_regions_region_value() {
        let toml = r#"
[green.service_regions]
"order-svc" = "us east 1"
"#;
        let result = load_from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_oversized_service_regions_map() {
        // Fat-finger or malicious config with too many entries gets
        // rejected at load time with a clear error mentioning the cap.
        use std::fmt::Write as _;
        let mut toml = String::from("[green.service_regions]\n");
        for i in 0..1025 {
            let _ = writeln!(toml, "\"svc-{i:04}\" = \"eu-west-3\"");
        }
        let result = load_from_str(&toml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("service_regions") && err.contains("1025"),
            "error should mention service_regions and the count, got: {err}"
        );
    }

    #[test]
    fn accepts_service_regions_at_exactly_the_cap() {
        // Boundary check: exactly 1024 entries should pass.
        use std::fmt::Write as _;
        let mut toml = String::from("[green.service_regions]\n");
        for i in 0..1024 {
            let _ = writeln!(toml, "\"svc-{i:04}\" = \"eu-west-3\"");
        }
        let config = load_from_str(&toml).expect("1024 entries should be accepted");
        assert_eq!(config.green.service_regions.len(), 1024);
    }

    #[test]
    fn service_regions_keys_are_lowercased_on_load() {
        // Config loader lowercases keys so resolve_region's
        // case-insensitive lookup works transparently.
        let toml = r#"
[green.service_regions]
"Order-Svc" = "us-east-1"
"CHAT-SVC" = "ap-southeast-1"
"#;
        let config = load_from_str(toml).unwrap();
        assert_eq!(config.green.service_regions.len(), 2);
        // Keys are lowercased regardless of TOML casing.
        assert_eq!(
            config
                .green
                .service_regions
                .get("order-svc")
                .map(String::as_str),
            Some("us-east-1")
        );
        assert_eq!(
            config
                .green
                .service_regions
                .get("chat-svc")
                .map(String::as_str),
            Some("ap-southeast-1")
        );
        // The original casings should NOT be present.
        assert!(!config.green.service_regions.contains_key("Order-Svc"));
    }

    #[test]
    fn rejects_zero_slow_query_threshold() {
        let result = load_from_str("[detection]\nslow_query_threshold_ms = 0");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_zero_slow_query_min_occurrences() {
        let result = load_from_str("[detection]\nslow_query_min_occurrences = 0");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_zero_max_fanout() {
        let result = load_from_str("[detection]\nmax_fanout = 0");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_max_fanout_over_100k() {
        let result = load_from_str("[detection]\nmax_fanout = 100001");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max_fanout"), "got: {err}");
    }

    #[test]
    fn accepts_max_fanout_at_100k() {
        let result = load_from_str("[detection]\nmax_fanout = 100000");
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_max_payload_size_over_100mb() {
        let result = load_from_str("[daemon]\nmax_payload_size = 104857601");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max_payload_size"), "got: {err}");
    }

    #[test]
    fn accepts_max_payload_size_at_100mb() {
        let result = load_from_str("[daemon]\nmax_payload_size = 104857600");
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_max_active_traces_over_1m() {
        let result = load_from_str("[daemon]\nmax_active_traces = 1000001");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max_active_traces"), "got: {err}");
    }

    #[test]
    fn accepts_max_active_traces_at_1m() {
        let result = load_from_str("[daemon]\nmax_active_traces = 1000000");
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_max_events_per_trace_over_100k() {
        let result = load_from_str("[daemon]\nmax_events_per_trace = 100001");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max_events_per_trace"), "got: {err}");
    }

    #[test]
    fn accepts_max_events_per_trace_at_100k() {
        let result = load_from_str("[daemon]\nmax_events_per_trace = 100000");
        assert!(result.is_ok());
    }

    // --- comfort-zone warnings (parse OK, hard caps unchanged) ---

    #[test]
    fn config_defaults_sit_inside_every_comfort_zone() {
        // Locks the invariant that the canonical defaults never trigger
        // a startup warning. If a default is moved, this test forces an
        // explicit re-check of the matching comfort band.
        let cfg = Config::default();
        assert!(
            (256 * 1024..=16 * 1024 * 1024).contains(&cfg.daemon.max_payload_size),
            "default max_payload_size {} is outside its comfort zone",
            cfg.daemon.max_payload_size
        );
        assert!(
            (1_000..=100_000).contains(&cfg.daemon.max_active_traces),
            "default max_active_traces {} is outside its comfort zone",
            cfg.daemon.max_active_traces
        );
        assert!(
            (100..=10_000).contains(&cfg.daemon.max_events_per_trace),
            "default max_events_per_trace {} is outside its comfort zone",
            cfg.daemon.max_events_per_trace
        );
        assert!(
            (100..=100_000).contains(&cfg.daemon.max_retained_findings),
            "default max_retained_findings {} is outside its comfort zone",
            cfg.daemon.max_retained_findings
        );
        assert!(
            (1_000..=600_000).contains(&cfg.daemon.trace_ttl_ms),
            "default trace_ttl_ms {} is outside its comfort zone",
            cfg.daemon.trace_ttl_ms
        );
        assert!(
            (5..=1_000).contains(&cfg.detection.max_fanout),
            "default max_fanout {} is outside its comfort zone",
            cfg.detection.max_fanout
        );
    }

    #[test]
    fn accepts_max_active_traces_below_comfort_floor_with_warning() {
        // 500 < comfort floor (1_000) but well within hard floor (1).
        let result = load_from_str("[daemon]\nmax_active_traces = 500");
        assert!(result.is_ok(), "expected parse OK, got {result:?}");
    }

    #[test]
    fn accepts_max_active_traces_above_comfort_ceiling_with_warning() {
        // 500_000 > comfort ceiling (100_000) but within hard ceiling (1_000_000).
        let result = load_from_str("[daemon]\nmax_active_traces = 500000");
        assert!(result.is_ok(), "expected parse OK, got {result:?}");
    }

    #[test]
    fn accepts_max_events_per_trace_outside_comfort_zone() {
        // 10 < comfort floor (100); 50_000 > comfort ceiling (10_000).
        // Both inside hard bounds [1, 100_000].
        for value in [10, 50_000] {
            let result = load_from_str(&format!("[daemon]\nmax_events_per_trace = {value}\n"));
            assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
        }
    }

    #[test]
    fn accepts_trace_ttl_outside_comfort_but_inside_hard_bounds() {
        // 200ms < comfort floor (1s); 1_800_000 (30min) > comfort ceiling (10min).
        for value in [200_u64, 1_800_000_u64] {
            let result = load_from_str(&format!("[daemon]\ntrace_ttl_ms = {value}\n"));
            assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
        }
    }

    #[test]
    fn accepts_max_fanout_outside_comfort_but_inside_hard_bounds() {
        // 2 < comfort floor (5); 5_000 > comfort ceiling (1_000).
        for value in [2, 5_000] {
            let result = load_from_str(&format!("[detection]\nmax_fanout = {value}\n"));
            assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
        }
    }

    #[test]
    fn accepts_max_payload_size_outside_comfort_but_inside_hard_bounds() {
        // 64 KiB < comfort floor (256 KiB); 32 MiB > comfort ceiling (16 MiB).
        for value in [64 * 1024_u64, 32 * 1024 * 1024_u64] {
            let result = load_from_str(&format!("[daemon]\nmax_payload_size = {value}\n"));
            assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
        }
    }

    // --- max_retained_findings hard cap (was unbounded before) ---

    #[test]
    fn accepts_zero_max_retained_findings_disables_store() {
        // `0` is a documented way to disable the findings store and
        // reclaim its memory. It must keep parsing.
        let result = load_from_str("[daemon]\nmax_retained_findings = 0");
        assert!(result.is_ok(), "expected 0 to parse, got {result:?}");
    }

    #[test]
    fn rejects_max_retained_findings_above_10m() {
        let result = load_from_str("[daemon]\nmax_retained_findings = 10000001");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max_retained_findings"), "got: {err}");
    }

    #[test]
    fn accepts_max_retained_findings_at_10m_hard_ceiling() {
        let result = load_from_str("[daemon]\nmax_retained_findings = 10000000");
        assert!(result.is_ok());
    }

    #[test]
    fn accepts_max_retained_findings_outside_comfort_but_inside_hard_bounds() {
        // 50 < comfort floor (100); 500_000 > comfort ceiling (100_000).
        for value in [50, 500_000] {
            let result = load_from_str(&format!("[daemon]\nmax_retained_findings = {value}\n"));
            assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
        }
    }

    #[test]
    fn rejects_trace_ttl_below_100() {
        let result = load_from_str("[daemon]\ntrace_ttl_ms = 50");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_zero_window_duration() {
        let result = load_from_str("[detection]\nwindow_duration_ms = 0");
        assert!(result.is_err());
    }

    #[test]
    fn green_disabled_parses() {
        let config = load_from_str("[green]\nenabled = false").unwrap();
        assert!(!config.green.enabled);
    }

    // -- Port validation --

    #[test]
    fn rejects_port_zero() {
        let result = load_from_str("[daemon]\nlisten_port_http = 0");
        assert!(result.is_err());
    }

    #[test]
    fn accepts_port_one() {
        let config = load_from_str("[daemon]\nlisten_port_http = 1").unwrap();
        assert_eq!(config.daemon.listen_port, 1);
    }

    #[test]
    fn accepts_port_65535() {
        let config = load_from_str("[daemon]\nlisten_port_http = 65535").unwrap();
        assert_eq!(config.daemon.listen_port, 65535);
    }

    #[test]
    fn rejects_grpc_port_zero() {
        let result = load_from_str("[daemon]\nlisten_port_grpc = 0");
        assert!(result.is_err());
    }

    // -- trace_ttl_ms upper bound --

    #[test]
    fn rejects_trace_ttl_above_1h() {
        let result = load_from_str("[daemon]\ntrace_ttl_ms = 3600001");
        assert!(result.is_err());
    }

    #[test]
    fn accepts_trace_ttl_at_1h() {
        let config = load_from_str("[daemon]\ntrace_ttl_ms = 3600000").unwrap();
        assert_eq!(config.daemon.trace_ttl_ms, 3_600_000);
    }

    #[test]
    fn accepts_trace_ttl_at_100ms() {
        let config = load_from_str("[daemon]\ntrace_ttl_ms = 100").unwrap();
        assert_eq!(config.daemon.trace_ttl_ms, 100);
    }

    // -- Sampling rate edge cases --

    #[test]
    fn accepts_sampling_rate_zero() {
        let config = load_from_str("[daemon]\nsampling_rate = 0.0").unwrap();
        assert!((config.daemon.sampling_rate - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accepts_sampling_rate_one() {
        let config = load_from_str("[daemon]\nsampling_rate = 1.0").unwrap();
        assert!((config.daemon.sampling_rate - 1.0).abs() < f64::EPSILON);
    }

    // --- [daemon] environment parsing ---

    #[test]
    fn daemon_environment_defaults_to_staging() {
        let config = Config::default();
        assert_eq!(config.daemon.environment, DaemonEnvironment::Staging);
        assert_eq!(config.confidence(), Confidence::DaemonStaging);
    }

    #[test]
    fn daemon_environment_omitted_uses_default() {
        let config = load_from_str("[daemon]\nmax_active_traces = 100").unwrap();
        assert_eq!(config.daemon.environment, DaemonEnvironment::Staging);
    }

    #[test]
    fn daemon_environment_staging() {
        let config = load_from_str("[daemon]\nenvironment = \"staging\"").unwrap();
        assert_eq!(config.daemon.environment, DaemonEnvironment::Staging);
        assert_eq!(config.confidence(), Confidence::DaemonStaging);
    }

    #[test]
    fn daemon_environment_production() {
        let config = load_from_str("[daemon]\nenvironment = \"production\"").unwrap();
        assert_eq!(config.daemon.environment, DaemonEnvironment::Production);
        assert_eq!(config.confidence(), Confidence::DaemonProduction);
    }

    #[test]
    fn daemon_environment_case_insensitive() {
        let config = load_from_str("[daemon]\nenvironment = \"PRODUCTION\"").unwrap();
        assert_eq!(config.daemon.environment, DaemonEnvironment::Production);
        let config = load_from_str("[daemon]\nenvironment = \"Staging\"").unwrap();
        assert_eq!(config.daemon.environment, DaemonEnvironment::Staging);
    }

    #[test]
    fn daemon_environment_rejects_unknown() {
        let result = load_from_str("[daemon]\nenvironment = \"prod\"");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("environment"), "got: {err}");
        assert!(err.contains("staging"), "error should mention valid values");
        assert!(
            err.contains("production"),
            "error should mention valid values"
        );
    }

    #[test]
    fn daemon_environment_rejects_empty() {
        let result = load_from_str("[daemon]\nenvironment = \"\"");
        assert!(result.is_err());
    }

    #[test]
    fn daemon_environment_rejects_dev() {
        let result = load_from_str("[daemon]\nenvironment = \"dev\"");
        assert!(result.is_err());
    }

    #[test]
    fn daemon_environment_as_str() {
        assert_eq!(DaemonEnvironment::Staging.as_str(), "staging");
        assert_eq!(DaemonEnvironment::Production.as_str(), "production");
    }

    // --- [green] use_hourly_profiles ---

    #[test]
    fn green_use_hourly_profiles_defaults_to_true() {
        let config = Config::default();
        assert!(config.green.use_hourly_profiles);
    }

    #[test]
    fn green_use_hourly_profiles_omitted_uses_default() {
        let config = load_from_str("[green]\nenabled = true\n").unwrap();
        assert!(config.green.use_hourly_profiles);
    }

    #[test]
    fn green_use_hourly_profiles_explicit_false() {
        let config = load_from_str("[green]\nuse_hourly_profiles = false\n").unwrap();
        assert!(!config.green.use_hourly_profiles);
    }

    #[test]
    fn green_use_hourly_profiles_explicit_true() {
        let config = load_from_str("[green]\nuse_hourly_profiles = true\n").unwrap();
        assert!(config.green.use_hourly_profiles);
    }

    // --- [green] hourly_profiles_file ---

    #[test]
    fn hourly_profiles_file_absent_by_default() {
        let config = Config::default();
        assert!(config.green.hourly_profiles_file.is_none());
        assert!(config.green.custom_hourly_profiles.is_none());
    }

    #[test]
    fn hourly_profiles_file_control_chars_rejected() {
        let config = load_from_str("[green]\nhourly_profiles_file = \"/tmp/profiles\\n.json\"\n");
        // The control character check happens during loading (sets None)
        // and then validate_green rejects the config.
        if let Ok(c) = config {
            let err = c.validate().unwrap_err();
            assert!(
                err.contains("control characters") || err.contains("failed to load"),
                "expected control char or load failure error, got: {err}"
            );
        } else {
            // TOML parse error is also acceptable
        }
    }

    #[test]
    fn hourly_profiles_file_nonexistent_path_rejected() {
        let result =
            load_from_str("[green]\nhourly_profiles_file = \"/nonexistent/profiles.json\"\n");
        let err = result.unwrap_err();
        assert!(
            format!("{err}").contains("failed to load"),
            "expected load failure error, got: {err}"
        );
    }

    #[test]
    fn hourly_profiles_windows_path_reports_load_failure_not_parse_error() {
        let err = load_from_str(
            r#"
[green]
hourly_profiles_file = "C:\temp\profiles.json"
"#,
        )
        .unwrap_err();
        assert!(
            format!("{err}").contains("failed to load"),
            "expected load failure error, got: {err}"
        );
    }

    // --- [green.scaphandre] parsing ---

    #[test]
    fn scaphandre_absent_by_default() {
        let config = Config::default();
        assert!(config.green.scaphandre.is_none());
    }

    #[test]
    fn scaphandre_empty_section_parses_to_none() {
        // An empty [green.scaphandre] table (no endpoint) is treated
        // as "Scaphandre not configured": the scraper is not spawned.
        let config = load_from_str("[green.scaphandre]\n").unwrap();
        assert!(config.green.scaphandre.is_none());
    }

    #[test]
    fn scaphandre_endpoint_only() {
        let config =
            load_from_str("[green.scaphandre]\nendpoint = \"http://localhost:8080/metrics\"\n")
                .unwrap();
        let cfg = config.green.scaphandre.unwrap();
        assert_eq!(cfg.endpoint, "http://localhost:8080/metrics");
        // Default interval is 5 s.
        assert_eq!(cfg.scrape_interval.as_secs(), 5);
        assert!(cfg.process_map.is_empty());
    }

    #[test]
    fn scaphandre_full_config() {
        let toml = r#"
[green.scaphandre]
endpoint = "http://localhost:9090/metrics"
scrape_interval_secs = 10

[green.scaphandre.process_map]
"order-svc" = "java"
"chat-svc" = "dotnet"
"#;
        let config = load_from_str(toml).unwrap();
        let cfg = config.green.scaphandre.unwrap();
        assert_eq!(cfg.endpoint, "http://localhost:9090/metrics");
        assert_eq!(cfg.scrape_interval.as_secs(), 10);
        assert_eq!(
            cfg.process_map.get("order-svc").map(String::as_str),
            Some("java")
        );
        assert_eq!(
            cfg.process_map.get("chat-svc").map(String::as_str),
            Some("dotnet")
        );
    }

    #[test]
    fn scaphandre_accepts_https_endpoint() {
        let result =
            load_from_str("[green.scaphandre]\nendpoint = \"https://secure:8080/metrics\"\n");
        assert!(result.is_ok(), "HTTPS endpoints should be accepted");
    }

    #[test]
    fn scaphandre_rejects_zero_interval() {
        let result = load_from_str(
            "[green.scaphandre]\nendpoint = \"http://localhost/metrics\"\nscrape_interval_secs = 0\n",
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("scrape_interval_secs"), "got: {err}");
    }

    #[test]
    fn scaphandre_rejects_huge_interval() {
        let result = load_from_str(
            "[green.scaphandre]\nendpoint = \"http://localhost/metrics\"\nscrape_interval_secs = 99999\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn scaphandre_rejects_empty_exe_in_process_map() {
        let toml = r#"
[green.scaphandre]
endpoint = "http://localhost/metrics"

[green.scaphandre.process_map]
"order-svc" = ""
"#;
        let result = load_from_str(toml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("process_map"), "got: {err}");
    }

    #[test]
    fn scaphandre_accepts_interval_at_boundary_1s() {
        let config = load_from_str(
            "[green.scaphandre]\nendpoint = \"http://localhost/metrics\"\nscrape_interval_secs = 1\n",
        )
        .unwrap();
        assert_eq!(
            config
                .green
                .scaphandre
                .as_ref()
                .unwrap()
                .scrape_interval
                .as_secs(),
            1
        );
    }

    #[test]
    fn scaphandre_accepts_interval_at_boundary_3600s() {
        let config = load_from_str(
            "[green.scaphandre]\nendpoint = \"http://localhost/metrics\"\nscrape_interval_secs = 3600\n",
        )
        .unwrap();
        assert_eq!(
            config
                .green
                .scaphandre
                .as_ref()
                .unwrap()
                .scrape_interval
                .as_secs(),
            3600
        );
    }

    // ------------------------------------------------------------------
    // [green.cloud] config tests
    // ------------------------------------------------------------------

    #[test]
    fn cloud_section_absent_yields_none() {
        let toml = "[green]\nenabled = true\n";
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        assert!(cfg.green.cloud_energy.is_none());
    }

    #[test]
    fn cloud_section_endpoint_only_parses_with_defaults() {
        let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"
"#;
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        let cloud = cfg.green.cloud_energy.unwrap();
        assert_eq!(cloud.prometheus_endpoint, "http://prom:9090");
        assert_eq!(cloud.scrape_interval.as_secs(), 15);
        assert!(cloud.default_provider.is_none());
        assert!(cloud.services.is_empty());
    }

    #[test]
    fn cloud_section_full_config_with_both_service_types() {
        let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"
scrape_interval_secs = 30
default_provider = "aws"

[green.cloud.services.svc-a]
provider = "gcp"
instance_type = "n2-standard-8"

[green.cloud.services.svc-b]
idle_watts = 45
max_watts = 120
"#;
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        assert!(cfg.validate().is_ok());
        let cloud = cfg.green.cloud_energy.as_ref().unwrap();
        assert_eq!(cloud.scrape_interval.as_secs(), 30);
        assert_eq!(cloud.default_provider.as_deref(), Some("aws"));
        assert_eq!(cloud.services.len(), 2);
    }

    #[test]
    fn cloud_accepts_https_endpoint() {
        let toml = r#"
[green.cloud]
prometheus_endpoint = "https://prom:9090"
"#;
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        assert!(cfg.validate().is_ok(), "HTTPS endpoints should be accepted");
    }

    #[test]
    fn cloud_rejects_credentials_in_endpoint() {
        let toml = r#"
[green.cloud]
prometheus_endpoint = "http://user:pass@prom:9090"
"#;
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("credentials"), "error: {err}");
    }

    #[test]
    fn cloud_rejects_invalid_scrape_interval() {
        let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"
scrape_interval_secs = 0
"#;
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("scrape_interval"), "error: {err}");
    }

    #[test]
    fn cloud_rejects_invalid_provider() {
        let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"
default_provider = "alibaba"
"#;
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("default_provider"), "error: {err}");
    }

    #[test]
    fn cloud_rejects_max_watts_less_than_idle() {
        let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"

[green.cloud.services.bad-svc]
idle_watts = 100
max_watts = 50
"#;
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("max_watts"), "error: {err}");
    }

    #[test]
    fn cloud_rejects_service_name_with_control_chars() {
        let toml = "
[green.cloud]
prometheus_endpoint = \"http://prom:9090\"

[green.cloud.services.\"bad\\nsvc\"]
idle_watts = 10
max_watts = 50
";
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("control characters"), "error: {err}");
    }

    #[test]
    fn config_per_operation_coefficients_default_true() {
        let cfg = Config::default();
        assert!(cfg.green.per_operation_coefficients);
    }

    #[test]
    fn config_include_network_transport_default_false() {
        let cfg = Config::default();
        assert!(!cfg.green.include_network_transport);
    }

    #[test]
    fn config_network_energy_per_byte_kwh_default() {
        let cfg = Config::default();
        assert!(
            (cfg.green.network_energy_per_byte_kwh
                - crate::score::carbon::DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH)
                .abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn config_network_energy_per_byte_kwh_rejects_negative() {
        let toml = r"
[green]
network_energy_per_byte_kwh = -0.001
";
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("network_energy_per_byte_kwh"), "error: {err}");
    }

    #[test]
    fn config_network_energy_per_byte_kwh_rejects_nan() {
        let cfg = Config {
            green: GreenConfig {
                network_energy_per_byte_kwh: f64::NAN,
                ..GreenConfig::default()
            },
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("network_energy_per_byte_kwh"), "error: {err}");
    }

    #[test]
    fn config_per_operation_coefficients_from_toml() {
        let toml = r"
[green]
per_operation_coefficients = false
";
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        assert!(!cfg.green.per_operation_coefficients);
    }

    #[test]
    fn config_include_network_transport_from_toml() {
        let toml = r"
[green]
include_network_transport = true
network_energy_per_byte_kwh = 0.00000000008
";
        let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
        assert!(cfg.green.include_network_transport);
        assert!((cfg.green.network_energy_per_byte_kwh - 0.000_000_000_08).abs() < f64::EPSILON);
    }

    // --- validate_http_authority error paths ---

    #[test]
    fn validate_http_authority_rejects_empty_host() {
        assert!(validate_http_authority("http://", "test").is_err());
    }

    #[test]
    fn validate_http_authority_rejects_credentials() {
        let err = validate_http_authority("http://user:pass@host/", "test").unwrap_err();
        assert!(err.contains("credentials"));
    }

    #[test]
    fn validate_http_authority_rejects_control_char() {
        // Embed a tab (0x09) in the host.
        let err = validate_http_authority("http://bad\thost/", "test").unwrap_err();
        assert!(err.contains("control"));
    }

    #[test]
    fn validate_http_authority_rejects_invalid_ipv4_port() {
        let err = validate_http_authority("http://host:abc/", "test").unwrap_err();
        assert!(err.contains("port"));
    }

    #[test]
    fn validate_http_authority_accepts_bare_ipv6() {
        // `[::1]` without port: should not error on the port-parse branch.
        assert!(validate_http_authority("http://[::1]/metrics", "test").is_ok());
    }

    #[test]
    fn validate_http_authority_accepts_ipv6_with_port() {
        assert!(validate_http_authority("http://[::1]:8080/metrics", "test").is_ok());
    }

    #[test]
    fn validate_http_authority_rejects_ipv6_with_invalid_port() {
        let err = validate_http_authority("http://[::1]:abc/metrics", "test").unwrap_err();
        assert!(err.contains("port"));
    }

    #[test]
    fn validate_http_authority_accepts_https_scheme() {
        assert!(validate_http_authority("https://host:443/", "test").is_ok());
    }

    // --- validate_green error paths ---

    #[test]
    fn validate_green_rejects_nonfinite_embodied_carbon() {
        let toml = "[green]\nembodied_carbon_per_request_gco2 = nan\n";
        let err = load_from_str(toml).unwrap_err();
        assert!(format!("{err:?}").contains("finite"));
    }

    #[test]
    fn validate_green_rejects_hourly_profiles_file_that_fails_to_load() {
        let toml = r#"
[green]
hourly_profiles_file = "/tmp/does-not-exist-perfsentinel-test.json"
"#;
        let err = load_from_str(toml).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("hourly_profiles_file") || msg.contains("failed to load"));
    }

    // --- convert_electricity_maps_section branches ---

    // Local imports used by all the electricity_maps tests below.
    // `HashMap` and `Duration` are already in scope via `use super::*;`
    // at the top of this module, but Qodana flags the fully-qualified
    // forms as unnecessary; using the short names reads cleaner anyway.
    use crate::score::electricity_maps::ElectricityMapsConfig;
    use crate::score::electricity_maps::config::{EmissionFactorType, TemporalGranularity};

    #[test]
    fn electricity_maps_empty_api_key_returns_none() {
        // When the api_key is explicitly an empty string and no env var is
        // set, the conversion returns None (subsystem stays inactive).
        let raw = ElectricityMapsSection {
            api_key: Some(String::new()),
            endpoint: None,
            poll_interval_secs: None,
            region_map: HashMap::new(),
            emission_factor_type: None,
            temporal_granularity: None,
        };
        // Pass a stubbed env-lookup that returns None so the test is
        // independent of the ambient process environment (no `unsafe`
        // env mutation, no races with other tests in the same binary).
        assert!(convert_electricity_maps_section_with_env(&raw, || None).is_none());
    }

    #[test]
    fn electricity_maps_warn_when_api_key_in_config_file() {
        // `api_key` set, env var unset → returns Some(...) but emits a
        // warning about preferring the env var. The warning path is
        // exercised; we just verify the conversion succeeds.
        let mut region_map = HashMap::new();
        region_map.insert("eu-west-3".to_string(), "FR".to_string());
        let raw = ElectricityMapsSection {
            api_key: Some("file-token".to_string()),
            endpoint: None,
            poll_interval_secs: Some(600),
            region_map,
            emission_factor_type: None,
            temporal_granularity: None,
        };
        let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
        assert_eq!(cfg.auth_token, "file-token");
        assert_eq!(cfg.poll_interval, Duration::from_mins(10));
        // default endpoint fallback
        assert_eq!(cfg.api_endpoint, "https://api.electricitymaps.com/v4");
        // region key was lowercased (it was already lowercase, so idempotent)
        assert!(cfg.region_map.contains_key("eu-west-3"));
    }

    #[test]
    fn electricity_maps_legacy_v3_endpoint_loads_cleanly() {
        // Backward-compat guard: an explicit v3 endpoint in the TOML
        // must still produce a valid ElectricityMapsConfig. The
        // deprecation warning is emitted at scraper startup (covered
        // by the `is_legacy_v3_endpoint` unit tests), the conversion
        // path here just keeps the field as-is.
        let mut region_map = HashMap::new();
        region_map.insert("eu-west-3".to_string(), "FR".to_string());
        let raw = ElectricityMapsSection {
            api_key: Some("tok".to_string()),
            endpoint: Some("https://api.electricitymaps.com/v3".to_string()),
            poll_interval_secs: Some(300),
            region_map,
            emission_factor_type: None,
            temporal_granularity: None,
        };
        let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
        assert_eq!(cfg.api_endpoint, "https://api.electricitymaps.com/v3");
    }

    #[test]
    fn electricity_maps_endpoint_trailing_slash_is_normalized() {
        // A copy-paste with trailing slash must not produce a
        // double-slash URL when we later format
        // `{api_endpoint}/carbon-intensity/latest`. The trim happens at
        // config-load so the canonical form propagates everywhere
        // (state, logs, error messages).
        let mut region_map = HashMap::new();
        region_map.insert("eu-west-3".to_string(), "FR".to_string());
        let raw = ElectricityMapsSection {
            api_key: Some("tok".to_string()),
            endpoint: Some("https://api.electricitymaps.com/v4/".to_string()),
            poll_interval_secs: Some(300),
            region_map,
            emission_factor_type: None,
            temporal_granularity: None,
        };
        let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
        assert_eq!(cfg.api_endpoint, "https://api.electricitymaps.com/v4");
    }

    #[test]
    fn electricity_maps_endpoint_strips_multiple_trailing_slashes() {
        let mut region_map = HashMap::new();
        region_map.insert("eu-west-3".to_string(), "FR".to_string());
        let raw = ElectricityMapsSection {
            api_key: Some("tok".to_string()),
            endpoint: Some("https://api.electricitymaps.com/v4///".to_string()),
            poll_interval_secs: Some(300),
            region_map,
            emission_factor_type: None,
            temporal_granularity: None,
        };
        let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
        assert_eq!(cfg.api_endpoint, "https://api.electricitymaps.com/v4");
    }

    #[test]
    fn electricity_maps_region_map_keys_lowercased() {
        let mut region_map = HashMap::new();
        region_map.insert("EU-WEST-3".to_string(), "FR".to_string());
        region_map.insert("Us-East-1".to_string(), "US-MIDA-PJM".to_string());
        let raw = ElectricityMapsSection {
            api_key: Some("tok".to_string()),
            endpoint: Some("https://custom.api/v3".to_string()),
            poll_interval_secs: Some(120),
            region_map,
            emission_factor_type: None,
            temporal_granularity: None,
        };
        let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
        assert!(cfg.region_map.contains_key("eu-west-3"));
        assert!(cfg.region_map.contains_key("us-east-1"));
        assert_eq!(cfg.api_endpoint, "https://custom.api/v3");
    }

    #[test]
    fn electricity_maps_env_var_takes_precedence_over_config_file() {
        // Env-lookup returns a token → it wins over `api_key` in the file.
        // Covers the from_env branch of convert_electricity_maps_section_with_env
        // without touching the real process environment.
        let mut region_map = HashMap::new();
        region_map.insert("eu-west-3".to_string(), "FR".to_string());
        let raw = ElectricityMapsSection {
            api_key: Some("from-file".to_string()),
            endpoint: None,
            poll_interval_secs: None,
            region_map,
            emission_factor_type: None,
            temporal_granularity: None,
        };
        let cfg = convert_electricity_maps_section_with_env(&raw, || Some("from-env".to_string()))
            .expect("env-supplied token should produce a valid config");
        assert_eq!(cfg.auth_token, "from-env");
    }

    #[test]
    fn cloud_auth_header_env_var_takes_precedence_over_config_file() {
        // Env-lookup returns a header → it wins over `auth_header` in the file.
        // Mirrors electricity_maps_env_var_takes_precedence_over_config_file.
        let raw = CloudSection {
            prometheus_endpoint: Some("http://prometheus:9090".to_string()),
            scrape_interval_secs: None,
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services: HashMap::new(),
            auth_header: Some("Authorization: Bearer from-file".to_string()),
        };
        let cfg = convert_cloud_section_with_env(&raw, || {
            Some("Authorization: Bearer from-env".to_string())
        })
        .expect("cloud section should convert");
        assert_eq!(
            cfg.auth_header.as_deref(),
            Some("Authorization: Bearer from-env"),
            "env var must take precedence over the config file value"
        );
    }

    #[test]
    fn cloud_auth_header_falls_back_to_config_when_env_unset() {
        let raw = CloudSection {
            prometheus_endpoint: Some("http://prometheus:9090".to_string()),
            scrape_interval_secs: None,
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services: HashMap::new(),
            auth_header: Some("Authorization: Bearer from-file".to_string()),
        };
        let cfg =
            convert_cloud_section_with_env(&raw, || None).expect("cloud section should convert");
        assert_eq!(
            cfg.auth_header.as_deref(),
            Some("Authorization: Bearer from-file"),
            "config value is used when the env var is unset"
        );
    }

    #[test]
    fn scaphandre_auth_header_env_var_takes_precedence_over_config_file() {
        let raw = ScaphandreSection {
            endpoint: Some("http://localhost:8080/metrics".to_string()),
            scrape_interval_secs: None,
            process_map: HashMap::new(),
            auth_header: Some("Authorization: Bearer from-file".to_string()),
        };
        let cfg = convert_scaphandre_section_with_env(&raw, || {
            Some("Authorization: Bearer from-env".to_string())
        })
        .expect("scaphandre section should convert");
        assert_eq!(
            cfg.auth_header.as_deref(),
            Some("Authorization: Bearer from-env"),
            "env var must take precedence over the config file value"
        );
    }

    #[test]
    fn scaphandre_auth_header_falls_back_to_config_when_env_unset() {
        let raw = ScaphandreSection {
            endpoint: Some("http://localhost:8080/metrics".to_string()),
            scrape_interval_secs: None,
            process_map: HashMap::new(),
            auth_header: Some("Authorization: Bearer from-file".to_string()),
        };
        let cfg = convert_scaphandre_section_with_env(&raw, || None)
            .expect("scaphandre section should convert");
        assert_eq!(
            cfg.auth_header.as_deref(),
            Some("Authorization: Bearer from-file"),
            "config value is used when the env var is unset"
        );
    }

    // --- validate_electricity_maps error paths ---

    #[test]
    fn validate_electricity_maps_rejects_control_char_in_token() {
        let cfg = ElectricityMapsConfig {
            api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
            auth_token: "tok\x07en".to_string(), // contains a control char
            poll_interval: Duration::from_mins(5),
            region_map: {
                let mut m = HashMap::new();
                m.insert("eu-west-3".to_string(), "FR".to_string());
                m
            },
            emission_factor_type: EmissionFactorType::default(),
            temporal_granularity: TemporalGranularity::default(),
        };
        let err = Config::validate_electricity_maps(&cfg).unwrap_err();
        assert!(err.contains("control"));
    }

    #[test]
    fn validate_electricity_maps_rejects_empty_region_map() {
        let cfg = ElectricityMapsConfig {
            api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
            auth_token: "tok".to_string(),
            poll_interval: Duration::from_mins(5),
            region_map: HashMap::new(),
            emission_factor_type: EmissionFactorType::default(),
            temporal_granularity: TemporalGranularity::default(),
        };
        let err = Config::validate_electricity_maps(&cfg).unwrap_err();
        assert!(err.contains("region_map"));
    }

    #[test]
    fn validate_electricity_maps_rejects_empty_zone() {
        let mut region_map = HashMap::new();
        region_map.insert("eu-west-3".to_string(), String::new());
        let cfg = ElectricityMapsConfig {
            api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
            auth_token: "tok".to_string(),
            poll_interval: Duration::from_mins(5),
            region_map,
            emission_factor_type: EmissionFactorType::default(),
            temporal_granularity: TemporalGranularity::default(),
        };
        let err = Config::validate_electricity_maps(&cfg).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_electricity_maps_rejects_invalid_poll_interval() {
        let mut region_map = HashMap::new();
        region_map.insert("eu-west-3".to_string(), "FR".to_string());
        let cfg = ElectricityMapsConfig {
            api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
            auth_token: "tok".to_string(),
            poll_interval: Duration::from_secs(10), // below 60
            region_map,
            emission_factor_type: EmissionFactorType::default(),
            temporal_granularity: TemporalGranularity::default(),
        };
        let err = Config::validate_electricity_maps(&cfg).unwrap_err();
        assert!(err.contains("poll_interval"));
    }

    // ---------------------------------------------------------------
    // TLS validation
    // ---------------------------------------------------------------

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_tls_accepts_both_absent() {
        let cfg = Config::default();
        assert!(cfg.validate_tls().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_tls_rejects_cert_without_key() {
        let mut cfg = Config::default();
        cfg.daemon.tls.cert_path = Some("/tmp/cert.pem".to_string());
        let err = cfg.validate_tls().unwrap_err();
        assert!(err.contains("tls.key_path is missing"), "{err}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_tls_rejects_key_without_cert() {
        let mut cfg = Config::default();
        cfg.daemon.tls.key_path = Some("/tmp/key.pem".to_string());
        let err = cfg.validate_tls().unwrap_err();
        assert!(err.contains("tls.cert_path is missing"), "{err}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_tls_rejects_nonexistent_cert() {
        let mut cfg = Config::default();
        cfg.daemon.tls.cert_path = Some("/nonexistent/cert.pem".to_string());
        cfg.daemon.tls.key_path = Some("/nonexistent/key.pem".to_string());
        let err = cfg.validate_tls().unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_tls_accepts_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        std::fs::write(&cert, b"fake cert").unwrap();
        std::fs::write(&key, b"fake key").unwrap();

        let mut cfg = Config::default();
        cfg.daemon.tls.cert_path = Some(cert.to_str().unwrap().to_string());
        cfg.daemon.tls.key_path = Some(key.to_str().unwrap().to_string());
        assert!(cfg.validate_tls().is_ok());
    }

    #[test]
    fn tls_config_fields_round_trip_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        std::fs::write(&cert, b"fake cert").unwrap();
        std::fs::write(&key, b"fake key").unwrap();
        let toml = format!(
            "[daemon]\ntls_cert_path = \"{}\"\ntls_key_path = \"{}\"",
            cert.display(),
            key.display()
        );
        let cfg = load_from_str(&toml).unwrap();
        assert_eq!(
            cfg.daemon.tls.cert_path.as_deref(),
            Some(cert.to_str().unwrap())
        );
        assert_eq!(
            cfg.daemon.tls.key_path.as_deref(),
            Some(key.to_str().unwrap())
        );
    }

    #[test]
    fn tls_config_defaults_to_none() {
        let cfg = load_from_str("").unwrap();
        assert!(cfg.daemon.tls.cert_path.is_none());
        assert!(cfg.daemon.tls.key_path.is_none());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_tls_rejects_control_chars_in_cert_path() {
        let mut cfg = Config::default();
        cfg.daemon.tls.cert_path = Some("/tmp/cert\x00.pem".to_string());
        cfg.daemon.tls.key_path = Some("/tmp/key.pem".to_string());
        let err = cfg.validate_tls().unwrap_err();
        assert!(err.contains("control characters"), "{err}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_tls_rejects_control_chars_in_key_path() {
        let mut cfg = Config::default();
        cfg.daemon.tls.cert_path = Some("/tmp/cert.pem".to_string());
        cfg.daemon.tls.key_path = Some("/tmp/key\n.pem".to_string());
        let err = cfg.validate_tls().unwrap_err();
        assert!(err.contains("control characters"), "{err}");
    }

    #[test]
    fn default_daemon_ack_is_enabled_with_no_secrets() {
        let cfg = Config::default();
        assert!(cfg.daemon.ack.enabled);
        assert!(cfg.daemon.ack.storage_path.is_none());
        assert!(cfg.daemon.ack.api_key.is_none());
        assert!(cfg.daemon.ack.toml_path.is_none());
    }

    #[test]
    fn parse_daemon_ack_section_overrides() {
        let toml = "
[daemon.ack]
enabled = false
storage_path = \"/var/lib/perf-sentinel/acks.jsonl\"
api_key = \"a-long-enough-secret-key\"
toml_path = \"/etc/perf-sentinel/acknowledgments.toml\"
";
        let cfg = load_from_str(toml).unwrap();
        assert!(!cfg.daemon.ack.enabled);
        assert_eq!(
            cfg.daemon.ack.storage_path.as_deref(),
            Some("/var/lib/perf-sentinel/acks.jsonl")
        );
        assert_eq!(
            cfg.daemon.ack.api_key.as_deref(),
            Some("a-long-enough-secret-key")
        );
        assert_eq!(
            cfg.daemon.ack.toml_path.as_deref(),
            Some("/etc/perf-sentinel/acknowledgments.toml")
        );
    }

    #[test]
    fn validate_daemon_ack_rejects_empty_api_key() {
        let toml = "
[daemon.ack]
api_key = \"\"
";
        let err = load_from_str(toml).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("must not be empty"), "{msg}");
    }

    #[test]
    fn validate_daemon_ack_rejects_short_api_key() {
        let toml = "
[daemon.ack]
api_key = \"shortish\"
";
        let err = load_from_str(toml).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("too short"), "{msg}");
    }

    #[test]
    fn validate_daemon_ack_accepts_twelve_char_api_key() {
        let toml = "
[daemon.ack]
api_key = \"short-enough\"
";
        let cfg = load_from_str(toml).unwrap();
        assert_eq!(cfg.daemon.ack.api_key.as_deref(), Some("short-enough"));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_daemon_ack_rejects_control_chars_in_storage_path() {
        let mut cfg = Config::default();
        cfg.daemon.ack.storage_path = Some("/var/lib/acks\x00.jsonl".to_string());
        let err = cfg.validate_daemon_ack().unwrap_err();
        assert!(err.contains("control characters"), "{err}");
    }

    #[test]
    fn validate_daemon_cors_accepts_empty_default() {
        let cfg = Config::default();
        assert!(cfg.validate_daemon_cors().is_ok());
        assert!(cfg.daemon.cors.allowed_origins.is_empty());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_daemon_cors_accepts_wildcard() {
        let mut cfg = Config::default();
        cfg.daemon.cors.allowed_origins = vec!["*".to_string()];
        assert!(cfg.validate_daemon_cors().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_daemon_cors_accepts_https_origin() {
        let mut cfg = Config::default();
        cfg.daemon.cors.allowed_origins = vec!["https://reports.example.com".to_string()];
        assert!(cfg.validate_daemon_cors().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_daemon_cors_rejects_origin_without_scheme() {
        let mut cfg = Config::default();
        cfg.daemon.cors.allowed_origins = vec!["reports.example.com".to_string()];
        let err = cfg.validate_daemon_cors().unwrap_err();
        assert!(err.contains("must start with http://"), "{err}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_daemon_cors_rejects_trailing_slash() {
        let mut cfg = Config::default();
        cfg.daemon.cors.allowed_origins = vec!["https://reports.example.com/".to_string()];
        let err = cfg.validate_daemon_cors().unwrap_err();
        assert!(err.contains("trailing slash"), "{err}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_daemon_cors_rejects_empty_entry() {
        let mut cfg = Config::default();
        cfg.daemon.cors.allowed_origins = vec![String::new()];
        let err = cfg.validate_daemon_cors().unwrap_err();
        assert!(err.contains("is empty"), "{err}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_daemon_cors_rejects_control_chars() {
        let mut cfg = Config::default();
        cfg.daemon.cors.allowed_origins = vec!["https://example.com\nattacker".to_string()];
        let err = cfg.validate_daemon_cors().unwrap_err();
        assert!(err.contains("control characters"), "{err}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_daemon_cors_rejects_wildcard_mixed_with_explicit_origins() {
        let mut cfg = Config::default();
        cfg.daemon.cors.allowed_origins = vec!["*".to_string(), "https://example.com".to_string()];
        let err = cfg.validate_daemon_cors().unwrap_err();
        assert!(err.contains("cannot mix"), "{err}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn validate_daemon_cors_rejects_wildcard_with_api_key() {
        // X-API-Key is a header (not a cookie), so allow_credentials = false
        // does not block it, any browser origin can replay it under wildcard
        // CORS. Reject the combination at config load.
        let mut cfg = Config::default();
        cfg.daemon.cors.allowed_origins = vec!["*".to_string()];
        cfg.daemon.ack.api_key = Some("test-token-12chars".to_string());
        let err = cfg.validate_daemon_cors().unwrap_err();
        assert!(
            err.contains("incompatible with") && err.contains("api_key"),
            "{err}"
        );
    }

    #[test]
    fn cors_section_round_trips_via_toml() {
        let toml = r#"
[daemon.cors]
allowed_origins = ["https://reports.example.com", "https://gitlab.example.com"]
"#;
        let cfg = load_from_str(toml).expect("valid TOML");
        assert_eq!(
            cfg.daemon.cors.allowed_origins,
            vec![
                "https://reports.example.com".to_string(),
                "https://gitlab.example.com".to_string(),
            ]
        );
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn cors_with_api_disabled_is_rejected() {
        // The CORS layer attaches to the `/api/*` sub-router only.
        // When `[daemon] api_enabled = false`, the sub-router is not
        // mounted, so a non-empty `allowed_origins` would silently do
        // nothing post-deploy. Reject at config load.
        let mut cfg = Config::default();
        cfg.daemon.api_enabled = false;
        cfg.daemon.cors.allowed_origins = vec!["https://reports.example.com".to_string()];
        let err = cfg.validate().unwrap_err();
        assert!(
            err.contains("api_enabled = false"),
            "expected mismatch error, got: {err}"
        );
    }

    #[test]
    fn cors_disabled_with_api_disabled_is_accepted() {
        let cfg = Config {
            daemon: DaemonConfig {
                api_enabled: false,
                ..DaemonConfig::default()
            },
            ..Config::default()
        };
        // Empty CORS list = layer not wired = no inconsistency.
        assert!(cfg.daemon.cors.allowed_origins.is_empty());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn cors_section_rejects_mixed_wildcard_via_toml_load() {
        // The mixed-wildcard rule is enforced at validation time, not
        // at deserialization. Verify the full `load_from_str` path
        // surfaces the validation error rather than silently dropping
        // explicit origins on the way in.
        let toml = r#"
[daemon.cors]
allowed_origins = ["*", "https://reports.example.com"]
"#;
        let err = load_from_str(toml).expect_err("mixed wildcard must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("cannot mix"),
            "expected validation error to mention mixing: {msg}"
        );
    }

    #[test]
    fn reporting_section_parses_and_validates() {
        let toml = r#"
[reporting]
intent = "official"
confidentiality_level = "public"
org_config_path = "/etc/perf-sentinel/org.toml"
disclose_output_path = "/var/lib/perf-sentinel/last.json"
disclose_period = "calendar-quarter"
"#;
        let cfg = load_from_str(toml).expect("valid reporting section");
        assert_eq!(cfg.reporting.intent.as_deref(), Some("official"));
        assert_eq!(
            cfg.reporting.confidentiality_level.as_deref(),
            Some("public")
        );
        assert_eq!(
            cfg.reporting.org_config_path.as_deref(),
            Some("/etc/perf-sentinel/org.toml")
        );
    }

    #[test]
    fn reporting_unknown_intent_rejected() {
        let toml = r#"
[reporting]
intent = "draft"
"#;
        let err = load_from_str(toml).expect_err("unknown intent must be rejected");
        assert!(err.to_string().contains("intent must be one of"));
    }

    #[test]
    fn reporting_unknown_confidentiality_rejected() {
        let toml = r#"
[reporting]
confidentiality_level = "restricted"
"#;
        let err = load_from_str(toml).expect_err("unknown confidentiality must be rejected");
        assert!(err.to_string().contains("confidentiality_level"));
    }

    #[test]
    fn reporting_official_requires_org_config_path() {
        let toml = r#"
[reporting]
intent = "official"
"#;
        let err = load_from_str(toml).expect_err("missing org_config_path must be rejected");
        assert!(err.to_string().contains("org_config_path"));
    }

    #[test]
    fn reporting_disclose_output_path_accepted_but_unused() {
        // disclose_output_path is reserved for daemon-triggered
        // periodic disclosures (planned for 0.8.0). Today it is
        // accepted by the parser but unused at runtime, and the
        // validator logs a tracing::warn so operators do not silently
        // depend on a no-op. The test confirms the field survives
        // round-trip parsing without error; the warning itself is
        // surfaced as an effect at validate time.
        let toml = r#"
[reporting]
disclose_output_path = "/var/lib/perf-sentinel/last.json"
"#;
        let cfg = load_from_str(toml).expect("disclose_output_path must parse without error");
        assert_eq!(
            cfg.reporting.disclose_output_path.as_deref(),
            Some("/var/lib/perf-sentinel/last.json")
        );
    }

    #[test]
    fn reporting_sigstore_absent_defaults_to_public() {
        let cfg = load_from_str("[reporting]\n").expect("valid empty reporting");
        assert_eq!(cfg.reporting.sigstore.rekor_url, DEFAULT_REKOR_URL);
        assert_eq!(cfg.reporting.sigstore.fulcio_url, DEFAULT_FULCIO_URL);
    }

    #[test]
    fn reporting_sigstore_section_overrides_defaults() {
        let toml = r#"
[reporting.sigstore]
rekor_url = "https://rekor.internal.example.fr"
fulcio_url = "https://fulcio.internal.example.fr"
"#;
        let cfg = load_from_str(toml).expect("valid sigstore section");
        assert_eq!(
            cfg.reporting.sigstore.rekor_url,
            "https://rekor.internal.example.fr"
        );
        assert_eq!(
            cfg.reporting.sigstore.fulcio_url,
            "https://fulcio.internal.example.fr"
        );
    }

    #[test]
    fn daemon_archive_section_parses_with_defaults() {
        let toml = r#"
[daemon.archive]
path = "/var/lib/perf-sentinel/reports.ndjson"
"#;
        let cfg = load_from_str(toml).expect("valid archive section");
        let archive = cfg.daemon.archive.expect("archive must be Some");
        assert_eq!(archive.path, "/var/lib/perf-sentinel/reports.ndjson");
        assert_eq!(archive.max_size_mb, 100);
        assert_eq!(archive.max_files, 12);
    }

    #[test]
    fn daemon_archive_zero_size_rejected() {
        let toml = r#"
[daemon.archive]
path = "/tmp/a.ndjson"
max_size_mb = 0
"#;
        let err = load_from_str(toml).expect_err("zero size must be rejected");
        assert!(err.to_string().contains("max_size_mb"));
    }

    #[test]
    fn daemon_archive_zero_files_rejected() {
        let toml = r#"
[daemon.archive]
path = "/tmp/a.ndjson"
max_files = 0
"#;
        let err = load_from_str(toml).expect_err("zero files must be rejected");
        assert!(err.to_string().contains("max_files"));
    }

    #[test]
    fn daemon_archive_absent_section_yields_none() {
        let cfg = load_from_str("").expect("empty config parses");
        assert!(cfg.daemon.archive.is_none());
    }
}
