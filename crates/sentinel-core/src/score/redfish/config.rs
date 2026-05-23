//! User-facing Redfish scraper configuration.

use std::collections::HashMap;
use std::time::Duration;

/// JSON pointer for the legacy `/Power` resource. Resolves to a finite
/// positive `f64` on Dell iDRAC, HPE iLO, Lenovo XCC and the `OpenBMC`
/// reference, all of which still surface this path as of 2026.
const LEGACY_POWER_JSON_POINTER: &str = "/PowerControl/0/PowerConsumedWatts";

/// JSON pointer for the modern `EnvironmentMetrics` resource (Redfish
/// schema v1.0+, DMTF Release 2020.4). The `PowerWatts.Reading` property
/// is a `SensorPowerExcerpt` carrying the chassis wattage gauge that
/// replaces the deprecated `/Power` array form.
const ENVIRONMENT_METRICS_JSON_POINTER: &str = "/PowerWatts/Reading";

/// Wire shape served by a Redfish endpoint. The schema is declared
/// per-endpoint so a fleet can host both legacy and modern BMCs without
/// duplicating top-level config sections. The JSON pointer used by the
/// parser is derived from this enum (see [`RedfishSchema::json_pointer`]),
/// the operator never spells out a pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedfishSchema {
    /// `/Chassis/{id}/Power` with `PowerControl[0].PowerConsumedWatts`.
    /// Deprecated by DMTF Release 2020.4 but still mandatory in BMC
    /// firmware as of 2026, the default for existing deployments.
    LegacyPower,
    /// `/Chassis/{id}/EnvironmentMetrics` with `PowerWatts.Reading`.
    /// Modern replacement, present alongside `/Power` during the
    /// transition period.
    EnvironmentMetrics,
}

impl RedfishSchema {
    /// JSON pointer (RFC 6901) used by the parser to extract the
    /// wattage reading from this schema's response shape.
    #[must_use]
    pub const fn json_pointer(self) -> &'static str {
        match self {
            Self::LegacyPower => LEGACY_POWER_JSON_POINTER,
            Self::EnvironmentMetrics => ENVIRONMENT_METRICS_JSON_POINTER,
        }
    }
}

/// Per-chassis endpoint definition: the full URL (path included) plus
/// the wire schema served by that URL. The schema drives the JSON
/// pointer used by the parser, no operator-typed pointer needed.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedfishEndpoint {
    pub url: String,
    pub schema: RedfishSchema,
}

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
/// [green.redfish.endpoints."chassis-legacy-1"]
/// url = "https://bmc-rack-01.dc.example/redfish/v1/Chassis/1/Power"
/// schema = "legacy_power"
///
/// [green.redfish.endpoints."chassis-modern-1"]
/// url = "https://bmc-rack-02.dc.example/redfish/v1/Chassis/1/EnvironmentMetrics"
/// schema = "environment_metrics"
///
/// [green.redfish.service_mappings]
/// "order-svc" = "chassis-legacy-1"
/// "chat-svc"  = "chassis-legacy-1"
/// "ledger-svc" = "chassis-modern-1"
/// ```
///
/// Absent config means no scraper spawned. Setting at least one entry
/// in `endpoints` activates the scraper.
#[derive(Clone)]
pub struct RedfishConfig {
    /// Chassis ID to typed endpoint (URL + wire schema). Each entry
    /// produces one scrape per `scrape_interval`. URLs must start with
    /// `http://` or `https://`. The schema drives the JSON pointer the
    /// parser uses, no operator-typed pointer.
    pub endpoints: HashMap<String, RedfishEndpoint>,
    /// How often to scrape each chassis. Default `60s`. Clamped to
    /// `[15, 3600]` at config load time to avoid BMC rate-limit
    /// retaliation.
    pub scrape_interval: Duration,
    /// Maps perf-sentinel service names to the chassis hosting them.
    /// Every service mapped to the same chassis receives the same
    /// chassis-level coefficient.
    pub service_mappings: HashMap<String, String>,
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
            RedfishEndpoint {
                url: "https://bmc/redfish/v1/Chassis/1/Power".to_string(),
                schema: RedfishSchema::LegacyPower,
            },
        );
        let mut mappings = HashMap::new();
        mappings.insert("order-svc".to_string(), "chassis-1".to_string());
        RedfishConfig {
            endpoints,
            scrape_interval: Duration::from_mins(1),
            service_mappings: mappings,
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
        assert!(dbg.contains("LegacyPower"));
        assert!(dbg.contains("order-svc"));
    }

    #[test]
    fn schema_dispatches_to_canonical_json_pointer() {
        assert_eq!(
            RedfishSchema::LegacyPower.json_pointer(),
            "/PowerControl/0/PowerConsumedWatts"
        );
        assert_eq!(
            RedfishSchema::EnvironmentMetrics.json_pointer(),
            "/PowerWatts/Reading"
        );
    }
}
