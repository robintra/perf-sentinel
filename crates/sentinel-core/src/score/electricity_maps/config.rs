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
