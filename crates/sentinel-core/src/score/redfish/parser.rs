//! JSON parser for Redfish `/Power` responses.
//!
//! Resolves a configurable JSON pointer (default
//! `/PowerControl/0/PowerConsumedWatts`) and validates that the value
//! is a finite, strictly positive number. Vendor responses with `null`,
//! `0`, negative, or `NaN` wattage are rejected as transitional states,
//! the caller keeps the previous coefficient in that case.

/// Result of parsing one Redfish `/Power` response.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParseOutcome {
    /// Wattage successfully resolved and validated.
    Ok(f64),
    /// JSON parse failed (malformed body).
    InvalidJson,
    /// Pointer resolved to nothing (vendor variance).
    PathMissing,
    /// Pointer resolved but the value was not a finite positive number.
    InvalidValue,
}

/// Parse a Redfish `/Power` JSON body and resolve `power_path` to a
/// wattage reading. `power_path` is a JSON pointer per RFC 6901, e.g.
/// `/PowerControl/0/PowerConsumedWatts`.
#[must_use]
pub fn parse_redfish_power(body: &str, power_path: &str) -> ParseOutcome {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return ParseOutcome::InvalidJson,
    };
    let Some(node) = value.pointer(power_path) else {
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
            parse_redfish_power(body, "/PowerControl/0/PowerConsumedWatts"),
            ParseOutcome::Ok(287.5)
        );
    }

    #[test]
    fn malformed_json_returns_invalid_json() {
        assert_eq!(
            parse_redfish_power("not json", "/PowerControl/0/PowerConsumedWatts"),
            ParseOutcome::InvalidJson
        );
    }

    #[test]
    fn missing_path_returns_path_missing() {
        let body = r#"{"PowerControl": []}"#;
        assert_eq!(
            parse_redfish_power(body, "/PowerControl/0/PowerConsumedWatts"),
            ParseOutcome::PathMissing
        );
    }

    #[test]
    fn null_value_returns_invalid_value() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": null}]}"#;
        assert_eq!(
            parse_redfish_power(body, "/PowerControl/0/PowerConsumedWatts"),
            ParseOutcome::InvalidValue
        );
    }

    #[test]
    fn zero_wattage_returns_invalid_value() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": 0}]}"#;
        assert_eq!(
            parse_redfish_power(body, "/PowerControl/0/PowerConsumedWatts"),
            ParseOutcome::InvalidValue
        );
    }

    #[test]
    fn negative_wattage_returns_invalid_value() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": -42}]}"#;
        assert_eq!(
            parse_redfish_power(body, "/PowerControl/0/PowerConsumedWatts"),
            ParseOutcome::InvalidValue
        );
    }

    #[test]
    fn custom_path_resolves() {
        let body = r#"{"Oem": {"Hp": {"PowerSummary": {"Watts": 412.0}}}}"#;
        assert_eq!(
            parse_redfish_power(body, "/Oem/Hp/PowerSummary/Watts"),
            ParseOutcome::Ok(412.0)
        );
    }

    #[test]
    fn float_value_with_decimals_resolves() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": 287.5}]}"#;
        assert_eq!(
            parse_redfish_power(body, "/PowerControl/0/PowerConsumedWatts"),
            ParseOutcome::Ok(287.5)
        );
    }

    #[test]
    fn integer_value_resolves_as_float() {
        let body = r#"{"PowerControl": [{"PowerConsumedWatts": 300}]}"#;
        assert_eq!(
            parse_redfish_power(body, "/PowerControl/0/PowerConsumedWatts"),
            ParseOutcome::Ok(300.0)
        );
    }
}
