//! Configuration parsing for `.perf-sentinel.toml`.
//!
//! Supports both the new sectioned format (`[thresholds]`, `[detection]`, `[green]`, `[daemon]`)
//! and the legacy flat format for backward compatibility.

use serde::Deserialize;

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

    // --- Green ---
    /// Whether `GreenOps` scoring is enabled.
    pub green_enabled: bool,

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
            // Green
            green_enabled: true,
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
#[allow(clippy::struct_field_names)]
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
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GreenSection {
    enabled: Option<bool>,
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
        let defaults = Config::default();

        // Sections take priority over flat fields, flat fields over defaults.
        Config {
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

            // Green
            green_enabled: raw.green.enabled.unwrap_or(defaults.green_enabled),

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

impl Config {
    /// Validate that config values are within acceptable bounds.
    ///
    /// # Errors
    ///
    /// Returns a description of the first invalid value found.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_payload_size < 1024 {
            return Err(format!(
                "max_payload_size must be >= 1024, got {}",
                self.max_payload_size
            ));
        }
        if self.max_active_traces == 0 {
            return Err("max_active_traces must be >= 1".to_string());
        }
        if self.max_events_per_trace == 0 {
            return Err("max_events_per_trace must be >= 1".to_string());
        }
        if self.n_plus_one_threshold == 0 {
            return Err("n_plus_one_threshold must be >= 1".to_string());
        }
        if self.window_duration_ms == 0 {
            return Err("window_duration_ms must be >= 1".to_string());
        }
        if self.trace_ttl_ms < 100 {
            return Err(format!(
                "trace_ttl_ms must be >= 100, got {}",
                self.trace_ttl_ms
            ));
        }
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
        if self.listen_addr != "127.0.0.1" && self.listen_addr != "::1" {
            tracing::warn!(
                "Daemon configured to listen on non-loopback address: {}",
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
}
