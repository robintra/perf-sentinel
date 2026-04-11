//! User-facing configuration for Electricity Maps API integration.

use std::collections::HashMap;
use std::time::Duration;

/// Configuration for the Electricity Maps real-time carbon intensity API.
///
/// Parsed from `[green.electricity_maps]` in `.perf-sentinel.toml`.
/// The subsystem is only active when `auth_token` is set (either in
/// config or via the `PERF_SENTINEL_EMAPS_TOKEN` environment variable).
#[derive(Clone)]
pub struct ElectricityMapsConfig {
    /// API base URL. User-configurable via `endpoint` in the TOML section.
    pub api_endpoint: String,
    /// API authentication token. Required. Stored as plain `String` (not
    /// `secrecy::SecretString`) to avoid adding a dependency. The manual
    /// `Debug` impl below redacts this field.
    pub auth_token: String,
    /// How often to poll the API. Default: 300s (5 min).
    /// Range: `[60, 86400]` seconds.
    pub poll_interval: Duration,
    /// Mapping from perf-sentinel cloud region to Electricity Maps zone code.
    /// Keys are lowercased cloud regions (e.g. `eu-west-3`), values are EM
    /// zones (e.g. `FR`, `US-NY`, `US-CAL-CISO`).
    pub region_map: HashMap<String, String>,
}

// Manual Debug impl to redact the auth token (secret).
impl std::fmt::Debug for ElectricityMapsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElectricityMapsConfig")
            .field("api_endpoint", &self.api_endpoint)
            .field("auth_token", &"[REDACTED]")
            .field("poll_interval", &self.poll_interval)
            .field("region_map", &self.region_map)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> ElectricityMapsConfig {
        let mut region_map = HashMap::new();
        region_map.insert("eu-west-3".to_string(), "FR".to_string());
        region_map.insert("us-east-1".to_string(), "US-MIDA-PJM".to_string());
        ElectricityMapsConfig {
            api_endpoint: "https://api.electricitymap.org/v3".to_string(),
            auth_token: "super-secret-token-do-not-log".to_string(),
            poll_interval: Duration::from_secs(300),
            region_map,
        }
    }

    #[test]
    fn debug_impl_redacts_auth_token() {
        // Secret hygiene regression guard: the manual `Debug` impl must
        // print `[REDACTED]` in place of the actual token. If someone
        // removes the manual impl (e.g. derives `Debug` automatically),
        // this test fails and the CI catches the leak before any log
        // line can expose a real token.
        let cfg = sample_config();
        let debug_output = format!("{cfg:?}");
        assert!(
            !debug_output.contains("super-secret-token-do-not-log"),
            "auth token must not appear in Debug output: {debug_output}"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output should mention [REDACTED]: {debug_output}"
        );
    }

    #[test]
    fn debug_impl_preserves_non_secret_fields() {
        let cfg = sample_config();
        let debug_output = format!("{cfg:?}");
        assert!(debug_output.contains("api_endpoint"));
        assert!(debug_output.contains("https://api.electricitymap.org/v3"));
        assert!(debug_output.contains("poll_interval"));
        assert!(debug_output.contains("region_map"));
        // Regions/zones are user-visible (not secrets) and should appear.
        assert!(debug_output.contains("eu-west-3"));
        assert!(debug_output.contains("FR"));
    }

    #[test]
    fn clone_preserves_all_fields() {
        let cfg = sample_config();
        let cloned = cfg.clone();
        assert_eq!(cfg.api_endpoint, cloned.api_endpoint);
        assert_eq!(cfg.auth_token, cloned.auth_token);
        assert_eq!(cfg.poll_interval, cloned.poll_interval);
        assert_eq!(cfg.region_map, cloned.region_map);
    }
}
