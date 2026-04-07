//! Configuration parsing for `.perf-sentinel.toml`.
//!
//! Supports both the new sectioned format (`[thresholds]`, `[detection]`, `[green]`, `[daemon]`)
//! and the legacy flat format for backward compatibility.

use std::collections::HashMap;

use serde::Deserialize;

use crate::score::carbon::DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2;

/// Top-level configuration for perf-sentinel.
#[derive(Debug, Clone)]
pub struct Config {
    // --- Thresholds ---
    /// Maximum allowed critical N+1 SQL findings before quality gate fails.
    pub n_plus_one_sql_critical_max: u32,
    /// Maximum allowed warning+ N+1 HTTP findings before quality gate fails.
    pub n_plus_one_http_warning_max: u32,
    /// Maximum allowed I/O waste ratio before quality gate fails.
    pub io_waste_ratio_max: f64,

    // --- Detection ---
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

    // --- Green ---
    /// Whether `GreenOps` scoring is enabled.
    pub green_enabled: bool,
    /// Default region for gCO₂eq conversion (e.g. "eu-west-3", "FR", "us-east-1").
    ///
    /// Used as the fallback when neither the span's `cloud.region` attribute
    /// nor the per-service mapping resolves a region. Renamed from `green_region`
    /// in Phase 5a (v0.3.0); the previous `[green] region` TOML key is no
    /// longer accepted. Update `.perf-sentinel.toml` when upgrading from v0.2.x.
    pub green_default_region: Option<String>,
    /// Per-service region overrides used when `OTel` `cloud.region` is absent
    /// from spans (e.g. `Jaeger` / `Zipkin` ingestion). Maps service name → region key.
    pub green_service_regions: HashMap<String, String>,
    /// SCI v1.0 embodied carbon term `M`: hardware manufacturing emissions
    /// amortized per request (per trace), in gCO₂eq. Region-independent.
    /// Defaults to [`DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2`].
    pub green_embodied_carbon_per_request_gco2: f64,

    // --- Daemon ---
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Thresholds
            n_plus_one_sql_critical_max: 0,
            n_plus_one_http_warning_max: 3,
            io_waste_ratio_max: 0.30,
            // Detection
            n_plus_one_threshold: 5,
            window_duration_ms: 500,
            slow_query_threshold_ms: 500,
            slow_query_min_occurrences: 3,
            max_fanout: 20,
            // Green
            green_enabled: true,
            green_default_region: None,
            green_service_regions: HashMap::new(),
            green_embodied_carbon_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
            // Daemon
            listen_addr: "127.0.0.1".to_string(),
            listen_port: 4318,
            listen_port_grpc: 4317,
            json_socket: "/tmp/perf-sentinel.sock".to_string(),
            max_active_traces: 10_000,
            trace_ttl_ms: 30_000,
            sampling_rate: 1.0,
            max_events_per_trace: 1_000,
            max_payload_size: 1_048_576, // 1 MB
        }
    }
}

// --- Internal raw deserialization types ---

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    // Sections (new format)
    thresholds: ThresholdsSection,
    detection: DetectionSection,
    green: GreenSection,
    daemon: DaemonSection,

    // Legacy flat fields (backward compatibility)
    max_payload_size: Option<usize>,
    n_plus_one_threshold: Option<u32>,
    listen_addr: Option<String>,
    listen_port: Option<u16>,
    window_duration_ms: Option<u64>,
    trace_ttl_ms: Option<u64>,
    max_active_traces: Option<usize>,
    max_events_per_trace: Option<usize>,
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
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GreenSection {
    enabled: Option<bool>,
    default_region: Option<String>,
    service_regions: HashMap<String, String>,
    embodied_carbon_per_request_gco2: Option<f64>,
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
}

impl From<RawConfig> for Config {
    fn from(raw: RawConfig) -> Self {
        let defaults = Self::default();

        // Sections take priority over flat fields, flat fields over defaults.
        Self {
            // Thresholds
            n_plus_one_sql_critical_max: raw
                .thresholds
                .n_plus_one_sql_critical_max
                .unwrap_or(defaults.n_plus_one_sql_critical_max),
            n_plus_one_http_warning_max: raw
                .thresholds
                .n_plus_one_http_warning_max
                .unwrap_or(defaults.n_plus_one_http_warning_max),
            io_waste_ratio_max: raw
                .thresholds
                .io_waste_ratio_max
                .unwrap_or(defaults.io_waste_ratio_max),

            // Detection: section > flat > default
            n_plus_one_threshold: raw
                .detection
                .n_plus_one_min_occurrences
                .or(raw.n_plus_one_threshold)
                .unwrap_or(defaults.n_plus_one_threshold),
            window_duration_ms: raw
                .detection
                .window_duration_ms
                .or(raw.window_duration_ms)
                .unwrap_or(defaults.window_duration_ms),
            slow_query_threshold_ms: raw
                .detection
                .slow_query_threshold_ms
                .unwrap_or(defaults.slow_query_threshold_ms),
            slow_query_min_occurrences: raw
                .detection
                .slow_query_min_occurrences
                .unwrap_or(defaults.slow_query_min_occurrences),
            max_fanout: raw.detection.max_fanout.unwrap_or(defaults.max_fanout),

            // Green
            green_enabled: raw.green.enabled.unwrap_or(defaults.green_enabled),
            green_default_region: raw.green.default_region,
            // D7: lowercase service_regions keys so resolve_region's
            // lowercase lookup matches regardless of config casing.
            green_service_regions: raw
                .green
                .service_regions
                .into_iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect(),
            green_embodied_carbon_per_request_gco2: raw
                .green
                .embodied_carbon_per_request_gco2
                .unwrap_or(defaults.green_embodied_carbon_per_request_gco2),

            // Daemon: section > flat > default
            listen_addr: raw
                .daemon
                .listen_address
                .or(raw.listen_addr)
                .unwrap_or(defaults.listen_addr),
            listen_port: raw
                .daemon
                .listen_port_http
                .or(raw.listen_port)
                .unwrap_or(defaults.listen_port),
            listen_port_grpc: raw
                .daemon
                .listen_port_grpc
                .unwrap_or(defaults.listen_port_grpc),
            json_socket: raw.daemon.json_socket.unwrap_or(defaults.json_socket),
            max_active_traces: raw
                .daemon
                .max_active_traces
                .or(raw.max_active_traces)
                .unwrap_or(defaults.max_active_traces),
            trace_ttl_ms: raw
                .daemon
                .trace_ttl_ms
                .or(raw.trace_ttl_ms)
                .unwrap_or(defaults.trace_ttl_ms),
            sampling_rate: raw.daemon.sampling_rate.unwrap_or(defaults.sampling_rate),
            max_events_per_trace: raw
                .daemon
                .max_events_per_trace
                .or(raw.max_events_per_trace)
                .unwrap_or(defaults.max_events_per_trace),
            max_payload_size: raw
                .daemon
                .max_payload_size
                .or(raw.max_payload_size)
                .unwrap_or(defaults.max_payload_size),
        }
    }
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
        self.validate_listen_addr()?;
        self.validate_green()?;
        Ok(())
    }

    fn validate_green(&self) -> Result<(), String> {
        /// N6: maximum number of entries in `[green.service_regions]`.
        /// Bounds the config-load memory footprint against fat-finger or
        /// malicious configs. 1024 is 4× `MAX_REGIONS` (256) and comfortably
        /// above any realistic multi-cloud deployment size.
        const MAX_SERVICE_REGIONS: usize = 1024;

        let value = self.green_embodied_carbon_per_request_gco2;
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
        // F9: region ID validation. Config is trusted input — fail loud
        // so typos surface at load time rather than silently producing
        // zeroed CO₂ rows downstream. Same validator used at the OTLP
        // ingestion boundary (there, invalid values are silently dropped).
        if let Some(region) = &self.green_default_region
            && !crate::score::carbon::is_valid_region_id(region)
        {
            return Err(format!(
                "[green] default_region '{region}' contains invalid characters; \
                 expected ASCII alphanumeric + '-' or '_', length 1-64"
            ));
        }
        // N6: cardinality cap on service_regions (defense against fat-finger
        // configs; AWS has ~33 regions, GCP ~40, Azure ~60, so 1024 leaves
        // ample headroom).
        if self.green_service_regions.len() > MAX_SERVICE_REGIONS {
            return Err(format!(
                "[green.service_regions] has {} entries; maximum is {MAX_SERVICE_REGIONS}",
                self.green_service_regions.len()
            ));
        }
        for (service, region) in &self.green_service_regions {
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

    fn validate_daemon_limits(&self) -> Result<(), String> {
        check_range(
            "max_payload_size",
            &self.max_payload_size,
            &1024,
            &(100 * 1024 * 1024),
        )?;
        check_range("max_active_traces", &self.max_active_traces, &1, &1_000_000)?;
        check_range(
            "max_events_per_trace",
            &self.max_events_per_trace,
            &1,
            &100_000,
        )?;
        check_range("trace_ttl_ms", &self.trace_ttl_ms, &100, &3_600_000)?;
        check_range("listen_port_http", &self.listen_port, &1, &65535)?;
        check_range("listen_port_grpc", &self.listen_port_grpc, &1, &65535)?;
        Ok(())
    }

    fn validate_detection_params(&self) -> Result<(), String> {
        check_min("n_plus_one_threshold", &self.n_plus_one_threshold, &1)?;
        check_min("window_duration_ms", &self.window_duration_ms, &1)?;
        check_min("slow_query_threshold_ms", &self.slow_query_threshold_ms, &1)?;
        check_min(
            "slow_query_min_occurrences",
            &self.slow_query_min_occurrences,
            &1,
        )?;
        check_range("max_fanout", &self.max_fanout, &1, &100_000)?;
        Ok(())
    }

    fn validate_rates(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.sampling_rate) {
            return Err(format!(
                "sampling_rate must be in [0.0, 1.0], got {}",
                self.sampling_rate
            ));
        }
        if !(0.0..=1.0).contains(&self.io_waste_ratio_max) {
            return Err(format!(
                "io_waste_ratio_max must be in [0.0, 1.0], got {}",
                self.io_waste_ratio_max
            ));
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)]
    fn validate_listen_addr(&self) -> Result<(), String> {
        if self.listen_addr != "127.0.0.1" && self.listen_addr != "::1" {
            tracing::warn!(
                "Daemon configured to listen on non-loopback address: {}. \
                 Endpoints have no authentication, use a reverse proxy or \
                 network policy for security.",
                self.listen_addr
            );
        }
        Ok(())
    }
}

/// Load configuration from a TOML string.
///
/// Supports both the sectioned format and the legacy flat format.
/// Validates that all values are within acceptable bounds after parsing.
///
/// # Errors
///
/// Returns an error if the TOML content cannot be parsed or contains invalid values.
///
/// # Errors
///
/// Returns `ConfigError::Parse` if the TOML is malformed, or
/// `ConfigError::Validation` if a field value is out of bounds.
pub fn load_from_str(content: &str) -> Result<Config, ConfigError> {
    let raw: RawConfig = toml::from_str(content).map_err(ConfigError::Parse)?;
    let config = Config::from(raw);
    config.validate().map_err(ConfigError::Validation)?;
    Ok(config)
}

/// Errors that can occur during configuration loading.
#[derive(Debug, thiserror::Error)]
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
        assert_eq!(config.max_payload_size, 1_048_576);
        assert_eq!(config.listen_addr, "127.0.0.1");
        assert_eq!(config.n_plus_one_threshold, 5);
        assert_eq!(config.window_duration_ms, 500);
        assert_eq!(config.trace_ttl_ms, 30_000);
        assert_eq!(config.max_active_traces, 10_000);
        assert_eq!(config.max_events_per_trace, 1_000);
    }

    #[test]
    fn parse_empty_toml_gives_defaults() {
        let config = load_from_str("").unwrap();
        assert_eq!(config.max_payload_size, 1_048_576);
    }

    #[test]
    fn parse_partial_toml() {
        let config = load_from_str("n_plus_one_threshold = 10").unwrap();
        assert_eq!(config.n_plus_one_threshold, 10);
        assert_eq!(config.max_payload_size, 1_048_576); // default preserved
    }

    #[test]
    fn parse_window_config() {
        let config = load_from_str(
            "window_duration_ms = 1000\ntrace_ttl_ms = 60000\nmax_active_traces = 5000",
        )
        .unwrap();
        assert_eq!(config.window_duration_ms, 1000);
        assert_eq!(config.trace_ttl_ms, 60_000);
        assert_eq!(config.max_active_traces, 5000);
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
        assert_eq!(config.n_plus_one_sql_critical_max, 2);
        assert_eq!(config.n_plus_one_http_warning_max, 5);
        assert!((config.io_waste_ratio_max - 0.50).abs() < f64::EPSILON);
        assert_eq!(config.n_plus_one_threshold, 10);
        assert_eq!(config.window_duration_ms, 1000);
        assert!(!config.green_enabled);
        assert_eq!(config.listen_addr, "0.0.0.0");
        assert_eq!(config.listen_port, 9418);
        assert_eq!(config.listen_port_grpc, 9417);
        assert_eq!(config.json_socket, "/var/run/perf-sentinel.sock");
        assert_eq!(config.max_active_traces, 20_000);
        assert_eq!(config.trace_ttl_ms, 60_000);
        assert!((config.sampling_rate - 0.5).abs() < f64::EPSILON);
        assert_eq!(config.max_events_per_trace, 500);
        assert_eq!(config.max_payload_size, 2_097_152);
    }

    #[test]
    fn section_overrides_flat_field() {
        let toml = r"
n_plus_one_threshold = 7
window_duration_ms = 800

[detection]
n_plus_one_min_occurrences = 12
";
        let config = load_from_str(toml).unwrap();
        // Section takes priority over flat field
        assert_eq!(config.n_plus_one_threshold, 12);
        // Flat field used when section does not override
        assert_eq!(config.window_duration_ms, 800);
    }

    #[test]
    fn new_fields_have_correct_defaults() {
        let config = Config::default();
        assert_eq!(config.n_plus_one_sql_critical_max, 0);
        assert_eq!(config.n_plus_one_http_warning_max, 3);
        assert!((config.io_waste_ratio_max - 0.30).abs() < f64::EPSILON);
        assert!(config.green_enabled);
        assert_eq!(config.listen_port_grpc, 4317);
        assert_eq!(config.json_socket, "/tmp/perf-sentinel.sock");
        assert!((config.sampling_rate - 1.0).abs() < f64::EPSILON);
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
        assert_eq!(config.slow_query_threshold_ms, 500);
        assert_eq!(config.slow_query_min_occurrences, 3);
        assert!(config.green_default_region.is_none());
        assert!(config.green_service_regions.is_empty());
        assert!(
            (config.green_embodied_carbon_per_request_gco2
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
        assert_eq!(config.slow_query_threshold_ms, 1000);
        assert_eq!(config.slow_query_min_occurrences, 5);
    }

    #[test]
    fn parse_green_default_region() {
        let toml = r#"
[green]
enabled = true
default_region = "eu-west-3"
"#;
        let config = load_from_str(toml).unwrap();
        assert_eq!(config.green_default_region.as_deref(), Some("eu-west-3"));
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
        assert_eq!(config.green_service_regions.len(), 2);
        assert_eq!(
            config
                .green_service_regions
                .get("order-svc")
                .map(String::as_str),
            Some("us-east-1")
        );
        assert_eq!(
            config
                .green_service_regions
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
        assert!((config.green_embodied_carbon_per_request_gco2 - 0.005).abs() < f64::EPSILON);
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
        assert!((config.green_embodied_carbon_per_request_gco2 - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_service_regions_default() {
        let toml = r#"
[green]
default_region = "eu-west-3"
"#;
        let config = load_from_str(toml).unwrap();
        assert!(config.green_service_regions.is_empty());
    }

    // ----- Phase 5a review fixes: region validation + lowercase + both-set -----

    #[test]
    fn rejects_invalid_default_region_characters() {
        // Space in region name — log-injection protection at config load.
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
        // 65 chars — just over the 64-char cap.
        let long_region = "a".repeat(65);
        let toml = format!("[green]\ndefault_region = \"{long_region}\"");
        let result = load_from_str(&toml);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_default_region_with_newline_escape() {
        // W2: in a TOML basic string, `\n` is an escape sequence for a real
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
        // W2: multi-line basic string with an actual newline byte in the
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
            assert_eq!(config.green_default_region.as_deref(), Some(region));
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
        // N6: fat-finger or malicious config with too many entries gets
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
        assert_eq!(config.green_service_regions.len(), 1024);
    }

    #[test]
    fn service_regions_keys_are_lowercased_on_load() {
        // D7: config loader lowercases keys so resolve_region's
        // case-insensitive lookup works transparently.
        let toml = r#"
[green.service_regions]
"Order-Svc" = "us-east-1"
"CHAT-SVC" = "ap-southeast-1"
"#;
        let config = load_from_str(toml).unwrap();
        assert_eq!(config.green_service_regions.len(), 2);
        // Keys are lowercased regardless of TOML casing.
        assert_eq!(
            config
                .green_service_regions
                .get("order-svc")
                .map(String::as_str),
            Some("us-east-1")
        );
        assert_eq!(
            config
                .green_service_regions
                .get("chat-svc")
                .map(String::as_str),
            Some("ap-southeast-1")
        );
        // The original casings should NOT be present.
        assert!(!config.green_service_regions.contains_key("Order-Svc"));
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
        assert!(!config.green_enabled);
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
        assert_eq!(config.listen_port, 1);
    }

    #[test]
    fn accepts_port_65535() {
        let config = load_from_str("[daemon]\nlisten_port_http = 65535").unwrap();
        assert_eq!(config.listen_port, 65535);
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
        assert_eq!(config.trace_ttl_ms, 3_600_000);
    }

    #[test]
    fn accepts_trace_ttl_at_100ms() {
        let config = load_from_str("[daemon]\ntrace_ttl_ms = 100").unwrap();
        assert_eq!(config.trace_ttl_ms, 100);
    }

    // -- Sampling rate edge cases --

    #[test]
    fn accepts_sampling_rate_zero() {
        let config = load_from_str("[daemon]\nsampling_rate = 0.0").unwrap();
        assert!((config.sampling_rate - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accepts_sampling_rate_one() {
        let config = load_from_str("[daemon]\nsampling_rate = 1.0").unwrap();
        assert!((config.sampling_rate - 1.0).abs() < f64::EPSILON);
    }
}
