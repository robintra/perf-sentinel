//! Configuration parsing for `.perf-sentinel.toml`.
//!
//! Supports both the new sectioned format (`[thresholds]`, `[detection]`, `[green]`, `[daemon]`)
//! and the legacy flat format for backward compatibility.

use std::borrow::Cow;
use std::collections::HashMap;
#[cfg(test)]
use std::time::Duration;

use crate::detect::Confidence;
use crate::score::carbon::DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2;
use crate::score::cloud_energy::config::CloudEnergyConfig;
use crate::score::kepler::KeplerConfig;
use crate::score::redfish::RedfishConfig;
#[cfg(test)]
use crate::score::redfish::RedfishEndpoint;
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
    /// Kepler eBPF energy scraper config (daemon only).
    pub kepler: Option<KeplerConfig>,
    /// Redfish BMC wall-plug-power scraper config (daemon only).
    pub redfish: Option<RedfishConfig>,
    /// Cloud CPU% + `SPECpower` config (daemon only).
    pub cloud_energy: Option<CloudEnergyConfig>,
    /// Whether to use per-operation energy coefficients (SQL verb weighting,
    /// HTTP payload size tiers) in the proxy model. Default: `true`.
    pub per_operation_coefficients: bool,
    /// Whether to compute a network transport energy term for cross-region
    /// HTTP calls. Default: `false` (opt-in).
    pub include_network_transport: bool,
    /// Energy per byte for network transport (kWh/byte).
    /// Default: 0.04 kWh/GB, a conservative upper bound below recent
    /// whole-network averages (see `DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH`).
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
    pub listen_addr: String,
    /// Port for OTLP HTTP receiver.
    pub listen_port: u16,
    /// Port for OTLP gRPC receiver.
    pub listen_port_grpc: u16,
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
    /// Deployment environment label used to stamp findings with a
    /// [`Confidence`] value, so downstream consumers (perf-lint) can boost
    /// severity on production traffic. Ignored in `analyze` batch mode,
    /// which always emits [`Confidence::CiBatch`].
    pub environment: DaemonEnvironment,
    /// Maximum number of findings retained by the daemon query API.
    pub max_retained_findings: usize,
    /// Capacity of the ingestion channel: span-event batches buffered
    /// between the listeners and the event loop. Provides ingestion
    /// backpressure once full.
    pub ingest_queue_capacity: usize,
    /// Capacity of the analysis worker queue: evicted/expired batches
    /// awaiting detect+score. When full, whole batches are shed (counted
    /// on `perf_sentinel_analysis_shed_*`).
    pub analysis_queue_capacity: usize,
    /// Memory-pressure admission control, as a percentage of the cgroup v2
    /// memory limit (1-100). When the pod's `memory.current / memory.max`
    /// crosses this high-water mark, OTLP ingest is rejected with a
    /// retryable status (counted on `perf_sentinel_otlp_rejected_total`
    /// `{reason="memory_pressure"}`) until usage falls back below the mark,
    /// so RSS is bounded independently of queue depth. `0` disables the
    /// guard (default). Linux/cgroup-v2 only, inert elsewhere.
    pub memory_high_water_pct: u8,
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
            kepler: None,
            redfish: None,
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
            ingest_queue_capacity: 1024,
            analysis_queue_capacity: 1024,
            memory_high_water_pct: 0,
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
    /// batch mode does not call this; it picks `CiBatch` or `LocalBatch`
    /// from the host CI environment in `pipeline::analyze_with_traces`
    /// instead (see `pipeline::ci_environment_detected`).
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

mod raw;
mod toml_paths;
mod validate;

use raw::{RawConfig, parse_daemon_environment, parse_kepler_metric_kind};
use toml_paths::normalize_toml_path_strings;
pub(crate) use validate::has_control_char;

// Re-imports so `use super::*;` in the tests module keeps resolving the
// names that moved into submodules.
#[cfg(test)]
use raw::{
    CloudSection, ElectricityMapsSection, KeplerSection, RedfishSection, ScaphandreSection,
    convert_cloud_section_with_env, convert_electricity_maps_section_with_env,
    convert_kepler_section_with_env, convert_redfish_section_with_env,
    convert_scaphandre_section_with_env,
};
#[cfg(test)]
use toml_paths::{TOML_PATH_STRING_KEYS, find_basic_string_end};
#[cfg(test)]
use validate::validate_http_authority;

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
    // Same pattern for `[green.kepler] metric_kind`: the From conversion
    // would otherwise downgrade an invalid value to a tracing::error log
    // and silently drop the whole section, which on a v0.7.4 → v0.7.5
    // upgrade would translate an operator's `metric_kind = "process_package"`
    // into a silent Kepler disable instead of the documented loud error.
    parse_kepler_metric_kind(raw.green.kepler.metric_kind.as_deref())
        .map_err(ConfigError::Validation)?;
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
mod tests;
