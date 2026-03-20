//! Configuration parsing for `.perf-sentinel.toml`.

use serde::Deserialize;

/// Top-level configuration for perf-sentinel.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Maximum payload size in bytes for JSON deserialization.
    pub max_payload_size: usize,
    /// N+1 detection threshold: minimum repeated similar queries to flag.
    pub n_plus_one_threshold: u32,
    /// Address for the daemon to listen on.
    pub listen_addr: String,
    /// Port for the daemon to listen on.
    pub listen_port: u16,
    /// Sliding window duration in milliseconds for N+1 detection.
    pub window_duration_ms: u64,
    /// Trace TTL in milliseconds for streaming mode eviction.
    pub trace_ttl_ms: u64,
    /// Maximum number of active traces in streaming mode.
    pub max_active_traces: usize,
    /// Maximum events kept per trace (ring buffer size).
    pub max_events_per_trace: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_payload_size: 1_048_576, // 1 MB
            n_plus_one_threshold: 5,
            listen_addr: "127.0.0.1".to_string(),
            listen_port: 4318,
            window_duration_ms: 500,
            trace_ttl_ms: 30_000,
            max_active_traces: 10_000,
            max_events_per_trace: 1_000,
        }
    }
}

/// Load configuration from a TOML string.
///
/// # Errors
///
/// Returns an error if the TOML content cannot be parsed into a `Config`.
pub fn load_from_str(content: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(content)
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
}
