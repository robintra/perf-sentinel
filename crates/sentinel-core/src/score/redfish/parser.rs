//! JSON parser for Redfish power responses (legacy `/Power` and modern
//! `EnvironmentMetrics`).
//!
//! Resolves the canonical JSON pointer for the configured schema (see
//! [`RedfishSchema`]) and validates that the value is a
//! finite, strictly positive number. Vendor responses with `null`, `0`,
//! negative or `NaN` wattage are rejected as transitional states, the
//! caller keeps the previous coefficient in that case.

use super::config::RedfishSchema;

/// Result of parsing one Redfish power response.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParseOutcome {
    /// Wattage successfully resolved and validated.
    Ok(f64),
    /// JSON parse failed (malformed body).
    InvalidJson,
    /// Pointer resolved to nothing (vendor variance or wrong schema
    /// declared for the endpoint).
    PathMissing,
    /// Pointer resolved but the value was not a finite positive number.
    InvalidValue,
}

/// Parse a Redfish power JSON body and resolve the wattage reading
/// using the canonical JSON pointer for `schema`. See
/// [`RedfishSchema::json_pointer`] for the pointer dispatch table.
#[must_use]
pub fn parse_redfish_power(body: &str, schema: RedfishSchema) -> ParseOutcome {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return ParseOutcome::InvalidJson,
    };
    let Some(node) = value.pointer(schema.json_pointer()) else {
        return ParseOutcome::PathMissing;
    };
    let Some(watts) = node.as_f64() else {
        return ParseOutcome::InvalidValue;
    };
    if !watts.is_finite() || watts <= 0.0 {
        return ParseOutcome::InvalidValue;
    }
    ParseOutcome::Ok(watts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_dell_idrac_shape() {
        let body = r#"{
            "PowerControl": [
                {"PowerConsumedWatts": 287.5, "Name": "System Power Control"}
            ]
        }"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::Ok(287.5)
        );
    }

    #[test]
    fn malformed_json_returns_invalid_json() {
        assert_eq!(
            parse_redfish_power("not json", RedfishSchema::LegacyPower),
            ParseOutcome::InvalidJson
        );
    }

    #[test]
    fn missing_path_returns_path_missing() {
        let body = r#"{"PowerControl": []}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::PathMissing
        );
    }

    #[test]
    fn null_value_returns_invalid_value() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": null}]}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::InvalidValue
        );
    }

    #[test]
    fn zero_wattage_returns_invalid_value() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": 0}]}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::InvalidValue
        );
    }

    #[test]
    fn negative_wattage_returns_invalid_value() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": -42}]}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::InvalidValue
        );
    }

    #[test]
    fn float_value_with_decimals_resolves() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": 287.5}]}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::Ok(287.5)
        );
    }

    #[test]
    fn integer_value_resolves_as_float() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": 300}]}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::Ok(300.0)
        );
    }

    #[test]
    fn parses_environment_metrics_shape() {
        // Captured from dmtf/redfish-mockup-server v1.2.9 public-rackmount1
        // mockup at GET /redfish/v1/Chassis/1U/EnvironmentMetrics.
        let body = r##"{
            "@odata.type": "#EnvironmentMetrics.v1_3_1.EnvironmentMetrics",
            "PowerWatts": {
                "DataSourceUri": "/redfish/v1/Chassis/1U/Sensors/TotalPower",
                "Reading": 374
            },
            "TemperatureCelsius": {"Reading": 39}
        }"##;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::EnvironmentMetrics),
            ParseOutcome::Ok(374.0)
        );
    }

    #[test]
    fn environment_metrics_missing_power_watts_returns_path_missing() {
        let body = r#"{"TemperatureCelsius": {"Reading": 39}}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::EnvironmentMetrics),
            ParseOutcome::PathMissing
        );
    }

    #[test]
    fn environment_metrics_null_reading_returns_invalid_value() {
        let body = r#"{"PowerWatts": {"Reading": null}}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::EnvironmentMetrics),
            ParseOutcome::InvalidValue
        );
    }

    #[test]
    fn environment_metrics_zero_reading_returns_invalid_value() {
        let body = r#"{"PowerWatts": {"Reading": 0}}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::EnvironmentMetrics),
            ParseOutcome::InvalidValue
        );
    }

    #[test]
    fn legacy_pointer_on_environment_metrics_body_misses() {
        // Defensive check: declaring the wrong schema for an endpoint
        // surfaces as PathMissing, not a silent fall-through.
        let body = r#"{"PowerWatts": {"Reading": 374}}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::PathMissing
        );
    }
}
