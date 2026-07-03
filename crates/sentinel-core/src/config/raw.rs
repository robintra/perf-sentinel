//! Raw serde deserialization layer for `.perf-sentinel.toml`: the private
//! `*Section` structs, the `RawConfig -> Config` conversion, and the
//! per-section conversion helpers.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::score::cloud_energy::config::{CloudEnergyConfig, ServiceCloudConfig};
use crate::score::kepler::{KeplerConfig, KeplerMetricKind};
use crate::score::redfish::{RedfishConfig, RedfishEndpoint};
use crate::score::scaphandre::{ProcessMatcher, ScaphandreConfig};

use super::validate::has_control_char;
use super::{
    Config, DEFAULT_FULCIO_URL, DEFAULT_REKOR_URL, DaemonAckConfig, DaemonArchiveConfig,
    DaemonConfig, DaemonCorsConfig, DaemonEnvironment, DaemonTlsConfig, DetectionConfig,
    GreenConfig, ReportingConfig, SigstoreConfig, ThresholdsConfig,
};

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct RawConfig {
    thresholds: ThresholdsSection,
    detection: DetectionSection,
    pub(super) green: GreenSection,
    pub(super) daemon: DaemonSection,
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
pub(super) struct GreenSection {
    enabled: Option<bool>,
    default_region: Option<String>,
    service_regions: HashMap<String, String>,
    embodied_carbon_per_request_gco2: Option<f64>,
    use_hourly_profiles: Option<bool>,
    scaphandre: ScaphandreSection,
    pub(super) kepler: KeplerSection,
    redfish: RedfishSection,
    cloud: CloudSection,
    per_operation_coefficients: Option<bool>,
    include_network_transport: Option<bool>,
    network_energy_per_byte_kwh: Option<f64>,
    hourly_profiles_file: Option<String>,
    calibration_file: Option<String>,
    electricity_maps: ElectricityMapsSection,
}

/// Raw deserialization target for `[green.scaphandre]`.
///
/// Converted to a `ScaphandreConfig` during `RawConfig → Config` only
/// when `endpoint` is set: an empty table (no fields) leaves
/// `Config::green.scaphandre = None`.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct ScaphandreSection {
    pub(super) endpoint: Option<String>,
    pub(super) scrape_interval_secs: Option<u64>,
    pub(super) process_map: HashMap<String, ProcessMatcher>,
    pub(super) auth_header: Option<String>,
}

/// Raw deserialization target for `[green.kepler]`.
///
/// Converted to a `KeplerConfig` during `RawConfig → Config` only when
/// `endpoint` is set. The optional `metric_kind` string accepts
/// `"container"` (default) or `"process"`.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct KeplerSection {
    pub(super) endpoint: Option<String>,
    pub(super) scrape_interval_secs: Option<u64>,
    pub(super) metric_kind: Option<String>,
    pub(super) service_mappings: HashMap<String, String>,
    pub(super) auth_header: Option<String>,
}

/// Raw deserialization target for `[green.redfish]`.
///
/// Converted to a `RedfishConfig` during `RawConfig → Config` only
/// when at least one `endpoints` entry is set.
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RedfishSection {
    pub(super) endpoints: HashMap<String, RedfishEndpoint>,
    pub(super) scrape_interval_secs: Option<u64>,
    pub(super) service_mappings: HashMap<String, String>,
    pub(super) ca_bundle_path: Option<String>,
    pub(super) auth_header: Option<String>,
}

/// Raw deserialization target for `[green.cloud]`.
///
/// Converted to a `CloudEnergyConfig` during `RawConfig -> Config` only
/// when `prometheus_endpoint` is set.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct CloudSection {
    pub(super) prometheus_endpoint: Option<String>,
    pub(super) scrape_interval_secs: Option<u64>,
    pub(super) default_provider: Option<String>,
    pub(super) default_instance_type: Option<String>,
    pub(super) cpu_metric: Option<String>,
    pub(super) services: HashMap<String, CloudServiceRaw>,
    pub(super) auth_header: Option<String>,
}

/// Raw deserialization for a single entry in `[green.cloud.services]`.
///
/// Supports two forms:
/// - Instance type: `{ provider = "aws", instance_type = "m5.large" }`
/// - Manual watts: `{ idle_watts = 45, max_watts = 120 }`
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct CloudServiceRaw {
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
pub(super) struct ElectricityMapsSection {
    pub(super) api_key: Option<String>,
    pub(super) endpoint: Option<String>,
    pub(super) poll_interval_secs: Option<u64>,
    pub(super) region_map: HashMap<String, String>,
    pub(super) emission_factor_type: Option<String>,
    pub(super) temporal_granularity: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct DaemonSection {
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
    pub(super) environment: Option<String>,
    tls_cert_path: Option<String>,
    tls_key_path: Option<String>,
    max_retained_findings: Option<usize>,
    ingest_queue_capacity: Option<usize>,
    analysis_queue_capacity: Option<usize>,
    memory_high_water_pct: Option<u8>,
    api_enabled: Option<bool>,
    correlation: CorrelationSection,
    ack: DaemonAckSection,
    cors: DaemonCorsSection,
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
                kepler: convert_kepler_section(&raw.green.kepler),
                redfish: convert_redfish_section(&raw.green.redfish),
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
                ingest_queue_capacity: raw
                    .daemon
                    .ingest_queue_capacity
                    .unwrap_or(daemon_defaults.ingest_queue_capacity),
                analysis_queue_capacity: raw
                    .daemon
                    .analysis_queue_capacity
                    .unwrap_or(daemon_defaults.analysis_queue_capacity),
                memory_high_water_pct: raw
                    .daemon
                    .memory_high_water_pct
                    .unwrap_or(daemon_defaults.memory_high_water_pct),
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
pub(super) fn parse_daemon_environment(value: &str) -> Option<DaemonEnvironment> {
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
pub(super) fn convert_cloud_section_with_env(
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
pub(super) fn convert_scaphandre_section_with_env(
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

/// Parse a `[green.kepler] metric_kind = "..."` string into the typed
/// enum. Returns `Container` when the field is absent. Matching is
/// case-insensitive after trimming. Returns an error string when the
/// value is set-but-empty, set-but-unrecognized, or one of the legacy
/// `process_package` / `process_dram` aliases that targeted metrics
/// Kepler never published. The raw operator-typed string is preserved
/// verbatim in error messages so `grep -F` against the source TOML
/// stays reliable.
pub(super) fn parse_kepler_metric_kind(raw: Option<&str>) -> Result<KeplerMetricKind, String> {
    let Some(literal) = raw else {
        return Ok(KeplerMetricKind::Container);
    };
    // Reject control chars before any branch interpolates `literal`
    // into an error: ANSI escapes in TOML would otherwise reach stderr.
    if has_control_char(literal) {
        return Err("[green.kepler] metric_kind contains control characters".to_string());
    }
    let trimmed = literal.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "[green.kepler] metric_kind '{literal}' is empty; \
             remove the field for the default or set it to 'container' or 'process'"
        ));
    }
    // `eq_ignore_ascii_case` skips the `to_ascii_lowercase` alloc,
    // matches `parse_daemon_environment` on the same TOML surface.
    if trimmed.eq_ignore_ascii_case("container") {
        return Ok(KeplerMetricKind::Container);
    }
    if trimmed.eq_ignore_ascii_case("process") {
        return Ok(KeplerMetricKind::Process);
    }
    if trimmed.eq_ignore_ascii_case("process_package")
        || trimmed.eq_ignore_ascii_case("process_dram")
    {
        return Err(format!(
            "[green.kepler] metric_kind '{literal}' was removed in v0.7.5. \
             Kepler v2 only exposes per-process CPU joules, use 'process' instead."
        ));
    }
    Err(format!(
        "[green.kepler] metric_kind '{literal}' is not recognized \
         (expected 'container' or 'process')"
    ))
}

/// Convert the raw `[green.kepler]` TOML section into a typed config.
///
/// Returns `None` when `endpoint` is absent. An invalid `metric_kind`
/// also yields `None` here as a defense-in-depth fallback. The
/// authoritative rejection happens upstream in [`load_from_str`]
/// before [`Config::from`] runs, so reaching this branch with a
/// malformed `metric_kind` means the operator bypassed `load_from_str`
/// (e.g. constructed a `RawConfig` directly in a test).
fn convert_kepler_section(raw: &KeplerSection) -> Option<KeplerConfig> {
    convert_kepler_section_with_env(raw, || {
        std::env::var("PERF_SENTINEL_KEPLER_AUTH_HEADER").ok()
    })
}

pub(super) fn convert_kepler_section_with_env(
    raw: &KeplerSection,
    env_lookup: impl FnOnce() -> Option<String>,
) -> Option<KeplerConfig> {
    let endpoint = raw.endpoint.as_ref()?;
    let metric_kind = match parse_kepler_metric_kind(raw.metric_kind.as_deref()) {
        Ok(k) => k,
        Err(msg) => {
            tracing::error!("{msg}");
            return None;
        }
    };
    let from_env = env_lookup();
    let auth_header = from_env.clone().or_else(|| raw.auth_header.clone());
    if from_env.is_none() && raw.auth_header.is_some() {
        tracing::warn!(
            "[green.kepler] auth_header is set in the config file. \
             Prefer the PERF_SENTINEL_KEPLER_AUTH_HEADER environment variable \
             to avoid committing secrets to version control."
        );
    }
    Some(KeplerConfig {
        endpoint: endpoint.clone(),
        scrape_interval: Duration::from_secs(raw.scrape_interval_secs.unwrap_or(5)),
        metric_kind,
        service_mappings: raw.service_mappings.clone(),
        auth_header,
    })
}

/// Convert the raw `[green.redfish]` TOML section into a typed config.
/// Returns `None` when `endpoints` is empty.
fn convert_redfish_section(raw: &RedfishSection) -> Option<RedfishConfig> {
    convert_redfish_section_with_env(raw, || {
        std::env::var("PERF_SENTINEL_REDFISH_AUTH_HEADER").ok()
    })
}

pub(super) fn convert_redfish_section_with_env(
    raw: &RedfishSection,
    env_lookup: impl FnOnce() -> Option<String>,
) -> Option<RedfishConfig> {
    if raw.endpoints.is_empty() {
        return None;
    }
    let from_env = env_lookup();
    let auth_header = from_env.clone().or_else(|| raw.auth_header.clone());
    if from_env.is_none() && raw.auth_header.is_some() {
        tracing::warn!(
            "[green.redfish] auth_header is set in the config file. \
             Prefer the PERF_SENTINEL_REDFISH_AUTH_HEADER environment variable \
             to avoid committing secrets to version control."
        );
    }
    Some(RedfishConfig {
        endpoints: raw.endpoints.clone(),
        scrape_interval: Duration::from_secs(raw.scrape_interval_secs.unwrap_or(60)),
        service_mappings: raw.service_mappings.clone(),
        ca_bundle_path: raw.ca_bundle_path.clone(),
        auth_header,
    })
}

/// Convert the raw `[green.electricity_maps]` TOML section into a typed config.
///
/// Returns `None` when no `api_key` is set (neither in config nor env var).
pub(super) fn convert_electricity_maps_section(
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
pub(super) fn convert_electricity_maps_section_with_env(
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
