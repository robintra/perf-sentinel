//! User-facing configuration for Electricity Maps API integration.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::text_safety::sanitize_for_terminal;

/// Default `Electricity Maps` API endpoint. v4 is the current latest
/// version (<https://app.electricitymaps.com/developer-hub/api/reference>).
/// v3 is still supported by `Electricity Maps` but is in legacy mode,
/// users with custom config pointing to `/v3` get a deprecation warning
/// at daemon startup via `scraper::warn_if_legacy_v3_endpoint`.
pub const DEFAULT_ELECTRICITY_MAPS_ENDPOINT: &str = "https://api.electricitymaps.com/v4";

/// Detected API version of the configured Electricity Maps endpoint.
/// Authoritative source of v3 detection, also used by
/// `scraper::warn_if_legacy_v3_endpoint`. `Custom` covers proxies,
/// mock servers and any URL without a `/vN` suffix.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ApiVersion {
    V3,
    #[default]
    V4,
    Custom,
}

impl ApiVersion {
    /// Derive the API version from a configured endpoint URL. The two
    /// `/vN` and `/vN/` shapes prevent false positives on `/v30` or
    /// `/v300`.
    #[must_use]
    pub fn from_endpoint(endpoint: &str) -> Self {
        if endpoint.ends_with("/v3") || endpoint.contains("/v3/") {
            Self::V3
        } else if endpoint.ends_with("/v4") || endpoint.contains("/v4/") {
            Self::V4
        } else {
            Self::Custom
        }
    }

    /// Stable string label used by the dashboard chips and the terminal
    /// scoring config line. Returns `&'static str`, no allocation.
    #[must_use]
    pub const fn as_chip_label(self) -> &'static str {
        match self {
            Self::V3 => "v3",
            Self::V4 => "v4",
            Self::Custom => "custom",
        }
    }
}

/// Emission factor model used by the API to compute carbon intensity.
/// `Lifecycle` (default) includes upstream emissions like manufacturing
/// and transport of generation infrastructure. `Direct` includes only
/// the combustion phase, which the GHG Protocol Scope 2 Guidance
/// (2015 amendment) treats as the reportable boundary for purchased
/// electricity under the location-based method. See the parameter
/// documentation at <https://app.electricitymaps.com/developer-hub/api/reference>.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EmissionFactorType {
    #[default]
    Lifecycle,
    Direct,
}

impl EmissionFactorType {
    /// Map a TOML string value to the enum. Unknown values trigger a
    /// `tracing::warn!` and fall back to the default. `None` (field
    /// absent in TOML) silently uses the default.
    ///
    /// Case-insensitive via `eq_ignore_ascii_case`, so the function
    /// allocates only on the unknown-value branch (where the value is
    /// also sanitized through `sanitize_for_terminal` before logging).
    #[must_use]
    pub fn from_config(value: Option<&str>) -> Self {
        match value {
            None => Self::default(),
            Some(s) if s.eq_ignore_ascii_case("lifecycle") => Self::Lifecycle,
            Some(s) if s.eq_ignore_ascii_case("direct") => Self::Direct,
            Some(other) => {
                let safe = sanitize_for_terminal(other);
                tracing::warn!(
                    value = %safe,
                    "unknown [green.electricity_maps] emission_factor_type, \
                     falling back to lifecycle. Accepted values: lifecycle, direct"
                );
                Self::default()
            }
        }
    }

    /// String form used in the API query parameter
    /// (`&emissionFactorType=...`).
    #[must_use]
    pub const fn as_query_value(self) -> &'static str {
        match self {
            Self::Lifecycle => "lifecycle",
            Self::Direct => "direct",
        }
    }
}

/// Temporal aggregation requested from the API. `Hourly` (default)
/// returns the hour-average. `FiveMinutes` and `FifteenMinutes` give
/// sub-hour fidelity, only useful when the operator's plan also offers
/// sub-hour granularity, the API silently coarsens otherwise. See
/// <https://app.electricitymaps.com/developer-hub/api/reference> for the
/// per-endpoint accepted values.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TemporalGranularity {
    #[default]
    #[serde(rename = "hourly")]
    Hourly,
    #[serde(rename = "5_minutes")]
    FiveMinutes,
    #[serde(rename = "15_minutes")]
    FifteenMinutes,
}

impl TemporalGranularity {
    /// Map a TOML string value to the enum. Unknown values trigger a
    /// `tracing::warn!` and fall back to the default. Same allocation
    /// + sanitization profile as `EmissionFactorType::from_config`.
    #[must_use]
    pub fn from_config(value: Option<&str>) -> Self {
        match value {
            None => Self::default(),
            Some(s) if s.eq_ignore_ascii_case("hourly") => Self::Hourly,
            Some(s) if s.eq_ignore_ascii_case("5_minutes") => Self::FiveMinutes,
            Some(s) if s.eq_ignore_ascii_case("15_minutes") => Self::FifteenMinutes,
            Some(other) => {
                let safe = sanitize_for_terminal(other);
                tracing::warn!(
                    value = %safe,
                    "unknown [green.electricity_maps] temporal_granularity, \
                     falling back to hourly. Accepted values: hourly, \
                     5_minutes, 15_minutes"
                );
                Self::default()
            }
        }
    }

    /// String form used in the API query parameter
    /// (`&temporalGranularity=...`).
    #[must_use]
    pub const fn as_query_value(self) -> &'static str {
        match self {
            Self::Hourly => "hourly",
            Self::FiveMinutes => "5_minutes",
            Self::FifteenMinutes => "15_minutes",
        }
    }
}

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
    /// Emission factor model. Default: `Lifecycle`. Set `Direct` to
    /// request only combustion-phase emissions.
    pub emission_factor_type: EmissionFactorType,
    /// Temporal aggregation. Default: `Hourly`. Set `FiveMinutes` or
    /// `FifteenMinutes` for sub-hour fidelity (requires a plan that
    /// supports it, otherwise the API coarsens silently).
    pub temporal_granularity: TemporalGranularity,
}

// Manual Debug impl to redact the auth token (secret).
impl std::fmt::Debug for ElectricityMapsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElectricityMapsConfig")
            .field("api_endpoint", &self.api_endpoint)
            .field("auth_token", &"[REDACTED]")
            .field("poll_interval", &self.poll_interval)
            .field("region_map", &self.region_map)
            .field("emission_factor_type", &self.emission_factor_type)
            .field("temporal_granularity", &self.temporal_granularity)
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
            api_endpoint: DEFAULT_ELECTRICITY_MAPS_ENDPOINT.to_string(),
            auth_token: "super-secret-token-do-not-log".to_string(),
            poll_interval: Duration::from_mins(5),
            region_map,
            emission_factor_type: EmissionFactorType::default(),
            temporal_granularity: TemporalGranularity::default(),
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
        crate::test_helpers::assert_debug_redacts_secret!(&cfg, "super-secret-token-do-not-log");
    }

    #[test]
    fn debug_impl_preserves_non_secret_fields() {
        let cfg = sample_config();
        let debug_output = format!("{cfg:?}");
        assert!(debug_output.contains("api_endpoint"));
        assert!(debug_output.contains(DEFAULT_ELECTRICITY_MAPS_ENDPOINT));
        assert!(debug_output.contains("poll_interval"));
        assert!(debug_output.contains("region_map"));
        // Regions/zones are user-visible (not secrets) and should appear.
        assert!(debug_output.contains("eu-west-3"));
        assert!(debug_output.contains("FR"));
    }

    #[test]
    fn default_electricity_maps_endpoint_constant_targets_v4() {
        assert_eq!(
            DEFAULT_ELECTRICITY_MAPS_ENDPOINT,
            "https://api.electricitymaps.com/v4"
        );
    }

    #[test]
    fn clone_preserves_all_fields() {
        let cfg = sample_config();
        let cloned = cfg.clone();
        assert_eq!(cfg.api_endpoint, cloned.api_endpoint);
        assert_eq!(cfg.auth_token, cloned.auth_token);
        assert_eq!(cfg.poll_interval, cloned.poll_interval);
        assert_eq!(cfg.region_map, cloned.region_map);
        assert_eq!(cfg.emission_factor_type, cloned.emission_factor_type);
        assert_eq!(cfg.temporal_granularity, cloned.temporal_granularity);
    }

    #[test]
    fn emission_factor_type_from_config_accepts_known_values() {
        assert_eq!(
            EmissionFactorType::from_config(None),
            EmissionFactorType::Lifecycle
        );
        assert_eq!(
            EmissionFactorType::from_config(Some("lifecycle")),
            EmissionFactorType::Lifecycle
        );
        assert_eq!(
            EmissionFactorType::from_config(Some("LIFECYCLE")),
            EmissionFactorType::Lifecycle
        );
        assert_eq!(
            EmissionFactorType::from_config(Some("direct")),
            EmissionFactorType::Direct
        );
        assert_eq!(
            EmissionFactorType::from_config(Some("Direct")),
            EmissionFactorType::Direct
        );
    }

    #[test]
    fn emission_factor_type_from_config_unknown_falls_back_to_lifecycle() {
        // Unknown value triggers a tracing::warn! and returns the
        // default. Captured here without asserting on the warn (no
        // tracing-subscriber dev-dep), the behavior is the documented
        // graceful fallback.
        assert_eq!(
            EmissionFactorType::from_config(Some("nonsense")),
            EmissionFactorType::Lifecycle
        );
    }

    #[test]
    fn emission_factor_type_query_values_match_api_spec() {
        assert_eq!(EmissionFactorType::Lifecycle.as_query_value(), "lifecycle");
        assert_eq!(EmissionFactorType::Direct.as_query_value(), "direct");
    }

    #[test]
    fn temporal_granularity_from_config_accepts_known_values() {
        assert_eq!(
            TemporalGranularity::from_config(None),
            TemporalGranularity::Hourly
        );
        assert_eq!(
            TemporalGranularity::from_config(Some("hourly")),
            TemporalGranularity::Hourly
        );
        assert_eq!(
            TemporalGranularity::from_config(Some("HOURLY")),
            TemporalGranularity::Hourly
        );
        assert_eq!(
            TemporalGranularity::from_config(Some("5_minutes")),
            TemporalGranularity::FiveMinutes
        );
        assert_eq!(
            TemporalGranularity::from_config(Some("15_minutes")),
            TemporalGranularity::FifteenMinutes
        );
        // Sub-hour values must accept any ASCII casing variant.
        assert_eq!(
            TemporalGranularity::from_config(Some("5_MINUTES")),
            TemporalGranularity::FiveMinutes
        );
        assert_eq!(
            TemporalGranularity::from_config(Some("15_Minutes")),
            TemporalGranularity::FifteenMinutes
        );
    }

    #[test]
    fn temporal_granularity_from_config_unknown_falls_back_to_hourly() {
        assert_eq!(
            TemporalGranularity::from_config(Some("nonsense")),
            TemporalGranularity::Hourly
        );
        assert_eq!(
            TemporalGranularity::from_config(Some("daily")),
            TemporalGranularity::Hourly
        );
    }

    #[test]
    fn temporal_granularity_query_values_match_api_spec() {
        assert_eq!(TemporalGranularity::Hourly.as_query_value(), "hourly");
        assert_eq!(
            TemporalGranularity::FiveMinutes.as_query_value(),
            "5_minutes"
        );
        assert_eq!(
            TemporalGranularity::FifteenMinutes.as_query_value(),
            "15_minutes"
        );
    }

    #[test]
    fn api_version_default_is_v4() {
        assert_eq!(ApiVersion::default(), ApiVersion::V4);
    }

    #[test]
    fn api_version_from_endpoint_matches_v3_at_path_end() {
        assert_eq!(
            ApiVersion::from_endpoint("https://api.electricitymaps.com/v3"),
            ApiVersion::V3
        );
    }

    #[test]
    fn api_version_from_endpoint_matches_v3_in_path() {
        assert_eq!(
            ApiVersion::from_endpoint("https://corporate-proxy.acme.com/electricitymaps/v3/api"),
            ApiVersion::V3
        );
    }

    #[test]
    fn api_version_from_endpoint_matches_v3_with_trailing_slash() {
        // `endpoint = "https://api.electricitymaps.com/v3/"` is a
        // common copy-paste shape. The config-load layer trims the
        // trailing slash, but the helper itself must still match
        // independently for defense-in-depth.
        assert_eq!(
            ApiVersion::from_endpoint("https://api.electricitymaps.com/v3/"),
            ApiVersion::V3
        );
    }

    #[test]
    fn api_version_from_endpoint_matches_v4() {
        assert_eq!(
            ApiVersion::from_endpoint("https://api.electricitymaps.com/v4"),
            ApiVersion::V4
        );
    }

    #[test]
    fn api_version_from_endpoint_returns_custom_for_versionless_url() {
        assert_eq!(
            ApiVersion::from_endpoint("http://127.0.0.1:9999"),
            ApiVersion::Custom
        );
        assert_eq!(
            ApiVersion::from_endpoint("https://api.electricitymaps.com"),
            ApiVersion::Custom
        );
    }

    #[test]
    fn api_version_from_endpoint_avoids_v30_and_v300_false_positives() {
        assert_eq!(
            ApiVersion::from_endpoint("https://api.electricitymaps.com/v30"),
            ApiVersion::Custom
        );
        assert_eq!(
            ApiVersion::from_endpoint("https://api.electricitymaps.com/v300/foo"),
            ApiVersion::Custom
        );
    }

    #[test]
    fn api_version_chip_labels_are_stable() {
        assert_eq!(ApiVersion::V3.as_chip_label(), "v3");
        assert_eq!(ApiVersion::V4.as_chip_label(), "v4");
        assert_eq!(ApiVersion::Custom.as_chip_label(), "custom");
    }

    #[test]
    fn api_version_serde_round_trip() {
        for variant in [ApiVersion::V3, ApiVersion::V4, ApiVersion::Custom] {
            let s = serde_json::to_string(&variant).unwrap();
            let back: ApiVersion = serde_json::from_str(&s).unwrap();
            assert_eq!(variant, back);
        }
        assert_eq!(serde_json::to_string(&ApiVersion::V4).unwrap(), "\"v4\"");
        assert_eq!(serde_json::to_string(&ApiVersion::V3).unwrap(), "\"v3\"");
        assert_eq!(
            serde_json::to_string(&ApiVersion::Custom).unwrap(),
            "\"custom\""
        );
    }

    #[test]
    fn emission_factor_type_serde_lowercase() {
        assert_eq!(
            serde_json::to_string(&EmissionFactorType::Lifecycle).unwrap(),
            "\"lifecycle\""
        );
        assert_eq!(
            serde_json::to_string(&EmissionFactorType::Direct).unwrap(),
            "\"direct\""
        );
        let back: EmissionFactorType = serde_json::from_str("\"direct\"").unwrap();
        assert_eq!(back, EmissionFactorType::Direct);
    }

    #[test]
    fn temporal_granularity_serde_renames_digit_starting_variants() {
        assert_eq!(
            serde_json::to_string(&TemporalGranularity::Hourly).unwrap(),
            "\"hourly\""
        );
        assert_eq!(
            serde_json::to_string(&TemporalGranularity::FiveMinutes).unwrap(),
            "\"5_minutes\""
        );
        assert_eq!(
            serde_json::to_string(&TemporalGranularity::FifteenMinutes).unwrap(),
            "\"15_minutes\""
        );
        let back: TemporalGranularity = serde_json::from_str("\"5_minutes\"").unwrap();
        assert_eq!(back, TemporalGranularity::FiveMinutes);
        let back: TemporalGranularity = serde_json::from_str("\"15_minutes\"").unwrap();
        assert_eq!(back, TemporalGranularity::FifteenMinutes);
    }
}
