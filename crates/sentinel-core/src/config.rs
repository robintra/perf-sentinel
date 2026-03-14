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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_payload_size: 1_048_576, // 1 MB
            n_plus_one_threshold: 5,
            listen_addr: "127.0.0.1".to_string(),
            listen_port: 4318,
        }
    }
}

/// Load configuration from a TOML string.
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
}
