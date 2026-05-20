//! User-facing Redfish scraper configuration.

use std::collections::HashMap;
use std::time::Duration;

/// Default JSON pointer to the chassis wattage reading. Resolves to a
/// finite positive `f64` on Dell iDRAC, HPE iLO, Lenovo XCC, and the
/// `OpenBMC` reference. Vendor-specific deviations can override the path
/// per chassis via [`RedfishConfig::power_path`].
pub const DEFAULT_POWER_PATH: &str = "/PowerControl/0/PowerConsumedWatts";

/// Lower bound on `scrape_interval_secs`. Several BMCs (notably HPE
/// iLO 4/5) rate-limit Redfish polling below 30 seconds, and many
/// vendors update their internal sensor cache every 30s anyway, so a
/// faster interval gains no information while risking 429 responses.
pub const MIN_SCRAPE_INTERVAL_SECS: u64 = 15;

/// Upper bound on `scrape_interval_secs`. Same shape as Scaphandre /
/// Kepler for consistency.
pub const MAX_SCRAPE_INTERVAL_SECS: u64 = 3600;

/// User-facing configuration for the Redfish scraper.
///
/// Parsed from `[green.redfish]` in `.perf-sentinel.toml`:
///
/// ```toml
/// [green.redfish]
/// scrape_interval_secs = 60
///
/// [green.redfish.endpoints]
/// "chassis-1" = "https://bmc-rack-01.dc.example/redfish/v1/Chassis/1/Power"
/// "chassis-2" = "https://bmc-rack-02.dc.example/redfish/v1/Chassis/1/Power"
///
/// [green.redfish.service_mappings]
/// "order-svc" = "chassis-1"
/// "chat-svc"  = "chassis-1"
/// "ledger-svc" = "chassis-2"
/// ```
///
/// Absent config means no scraper spawned. Setting at least one entry
/// in `endpoints` activates the scraper.
#[derive(Clone)]
pub struct RedfishConfig {
    /// Chassis ID to Redfish `/Power` URL. Each entry produces one
    /// scrape per `scrape_interval`. URLs must start with `http://` or
    /// `https://`.
    pub endpoints: HashMap<String, String>,
    /// How often to scrape each chassis. Default `60s`. Clamped to
    /// `[15, 3600]` at config load time to avoid BMC rate-limit
    /// retaliation.
    pub scrape_interval: Duration,
    /// Maps perf-sentinel service names to the chassis hosting them.
    /// Every service mapped to the same chassis receives the same
    /// chassis-level coefficient.
    pub service_mappings: HashMap<String, String>,
    /// JSON pointer to the wattage reading inside the Redfish `/Power`
    /// response. Defaults to [`DEFAULT_POWER_PATH`]. Override per
    /// vendor if `PowerConsumedWatts` lives at a different path.
    pub power_path: String,
    /// Optional path to a PEM-encoded CA bundle used to validate the
    /// BMC's TLS certificate.
    ///
    /// **Not yet implemented.** Setting this field causes the scraper
    /// to fail loud at startup with a clear error. Operators with
    /// self-signed BMC certs must currently front the BMC with a
    /// reverse proxy that presents a publicly-signed cert. Tracked as
    /// a follow-up.
    pub ca_bundle_path: Option<String>,
    /// Optional auth header in curl format (`"Name: Value"`) attached
    /// to every Redfish request. Most BMCs require Basic auth, e.g.
    /// `"Authorization: Basic base64..."`. Session-token auth (POST
    /// `/SessionService/Sessions`) is not yet supported.
    /// Resolved via the `PERF_SENTINEL_REDFISH_AUTH_HEADER`
    /// environment variable with fallback to this field, env wins
    /// when both are set.
    pub auth_header: Option<String>,
}

// Manual Debug impl to redact the auth header (potentially a secret).
impl std::fmt::Debug for RedfishConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedfishConfig")
            .field("endpoints", &self.endpoints)
            .field("scrape_interval", &self.scrape_interval)
            .field("service_mappings", &self.service_mappings)
            .field("power_path", &self.power_path)
            .field("ca_bundle_path", &self.ca_bundle_path)
            .field(
                "auth_header",
                &self.auth_header.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> RedfishConfig {
        let mut endpoints = HashMap::new();
        endpoints.insert(
            "chassis-1".to_string(),
            "https://bmc/redfish/v1/Chassis/1/Power".to_string(),
        );
        let mut mappings = HashMap::new();
        mappings.insert("order-svc".to_string(), "chassis-1".to_string());
        RedfishConfig {
            endpoints,
            scrape_interval: Duration::from_mins(1),
            service_mappings: mappings,
            power_path: DEFAULT_POWER_PATH.to_string(),
            ca_bundle_path: None,
            auth_header: Some("Authorization: Basic super-secret-do-not-log".to_string()),
        }
    }

    #[test]
    fn debug_impl_redacts_auth_header() {
        let cfg = sample_config();
        crate::test_helpers::assert_debug_redacts_secret!(&cfg, "super-secret-do-not-log");
    }

    #[test]
    fn debug_impl_preserves_non_secret_fields() {
        let cfg = sample_config();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("chassis-1"));
        assert!(dbg.contains("https://bmc/redfish/v1/Chassis/1/Power"));
        assert!(dbg.contains("order-svc"));
        assert!(dbg.contains("PowerConsumedWatts"));
    }
}
