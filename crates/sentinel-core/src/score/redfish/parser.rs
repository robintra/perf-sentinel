//! JSON parser for Redfish power responses (legacy `/Power`, modern
//! `EnvironmentMetrics` and a chassis power `Sensor`).
//!
//! Resolves the chassis reading for the configured schema (see
//! [`RedfishSchema`]) and validates that the value is a
//! finite, strictly positive number. Vendor responses with `null`, `0`,
//! negative or `NaN` wattage are rejected as transitional states. The
//! caller keeps the previous coefficient in that case.

use serde_json::Value;

use super::config::RedfishSchema;

/// Result of parsing one Redfish power response.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParseOutcome {
    /// Wattage successfully resolved and validated.
    Ok(f64),
    /// JSON parse failed (malformed body).
    InvalidJson,
    /// No reading where the schema expects one (vendor variance or
    /// wrong schema declared for the endpoint), or a `sensor` body that
    /// is not a chassis power sensor (`ReadingUnits` other than `W`, or
    /// a `PhysicalContext` other than `Chassis`).
    PathMissing,
    /// Pointer resolved but the value was not a finite positive number.
    InvalidValue,
}

/// Parse a Redfish power JSON body and resolve the chassis wattage for
/// `schema`. See [`RedfishSchema`] for what each schema reads.
#[must_use]
pub fn parse_redfish_power(body: &str, schema: RedfishSchema) -> ParseOutcome {
    let value: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return ParseOutcome::InvalidJson,
    };
    let Some(node) = locate_reading(&value, schema) else {
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

/// Resolve the wattage node for `schema`, or `None` when the body
/// carries no chassis reading.
fn locate_reading(value: &Value, schema: RedfishSchema) -> Option<&Value> {
    match schema {
        RedfishSchema::LegacyPower => chassis_power_control(value).map_or_else(
            || value.pointer(schema.json_pointer()),
            |entry| entry.get("PowerConsumedWatts"),
        ),
        RedfishSchema::Sensor if !is_chassis_power_sensor(value) => None,
        RedfishSchema::EnvironmentMetrics | RedfishSchema::Sensor => {
            value.pointer(schema.json_pointer())
        }
    }
}

/// The `PowerControl` entry whose `PhysicalContext` is `Chassis`. DMTF
/// does not require entry 0 to cover the whole chassis, so the
/// canonical pointer to entry 0 is only the fallback.
fn chassis_power_control(value: &Value) -> Option<&Value> {
    value
        .get("PowerControl")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("PhysicalContext").and_then(Value::as_str) == Some("Chassis"))
}

/// A `Sensor` resource is read as the chassis wattage only when it
/// reads in watts (DMTF requires `W` for `ReadingType` `Power`) and,
/// when it states a `PhysicalContext`, names the whole chassis. Power
/// supply and CPU power sensors read in watts too.
fn is_chassis_power_sensor(value: &Value) -> bool {
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    text("ReadingUnits") == Some("W") && text("PhysicalContext").is_none_or(|c| c == "Chassis")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_power_control_shape() {
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
    fn legacy_power_prefers_chassis_power_control() {
        // DMTF does not require PowerControl[0] to cover the whole
        // chassis: here it is a CPU subsystem and [1] the chassis.
        let body = r#"{
            "PowerControl": [
                {"MemberId": "0", "PhysicalContext": "CPU", "PowerConsumedWatts": 120},
                {"MemberId": "1", "PhysicalContext": "Chassis", "PowerConsumedWatts": 410}
            ]
        }"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::Ok(410.0)
        );
    }

    #[test]
    fn legacy_power_without_chassis_context_keeps_first_entry() {
        let body = r#"{
            "PowerControl": [
                {"MemberId": "0", "PowerConsumedWatts": 287.5},
                {"MemberId": "1", "PhysicalContext": "CPU", "PowerConsumedWatts": 120}
            ]
        }"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::Ok(287.5)
        );
    }

    #[test]
    fn legacy_power_chassis_entry_with_null_reading_is_invalid() {
        // A transitional null on the chassis entry must not fall back
        // to a subsystem reading.
        let body = r#"{
            "PowerControl": [
                {"PhysicalContext": "CPU", "PowerConsumedWatts": 120},
                {"PhysicalContext": "Chassis", "PowerConsumedWatts": null}
            ]
        }"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::LegacyPower),
            ParseOutcome::InvalidValue
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
    fn parses_sensor_shape() {
        // Trimmed from dmtf/redfish-mockup-server v1.2.9 public-rackmount1
        // mockup at GET /redfish/v1/Chassis/1U/Sensors/TotalPower.
        let body = r##"{
            "@odata.type": "#Sensor.v1_8_1.Sensor",
            "Id": "TotalPower",
            "ReadingType": "Power",
            "ElectricalContext": "Total",
            "Reading": 374,
            "ReadingUnits": "W",
            "PhysicalContext": "Chassis"
        }"##;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::Sensor),
            ParseOutcome::Ok(374.0)
        );
    }

    #[test]
    fn sensor_without_watt_units_returns_path_missing() {
        // The same mockup's Sensors/CPU1Temp minus its PhysicalContext,
        // as bmcweb serves temperatures: only ReadingUnits rejects it.
        let body = r#"{"Id": "CPU1Temp", "Reading": 37, "ReadingUnits": "Cel"}"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::Sensor),
            ParseOutcome::PathMissing
        );
    }

    #[test]
    fn sensor_outside_chassis_context_returns_path_missing() {
        // Shape of the same mockup's Sensors/PS1InputPower: watts, but
        // for one power supply, not the whole chassis.
        let body = r#"{
            "Id": "PS1InputPower",
            "ReadingType": "Power",
            "Reading": 374,
            "ReadingUnits": "W",
            "PhysicalContext": "PowerSupply",
            "PhysicalSubContext": "Input"
        }"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::Sensor),
            ParseOutcome::PathMissing
        );
    }

    #[test]
    fn sensor_without_physical_context_resolves() {
        // bmcweb only sets PhysicalContext on accelerator sensors, so
        // its total-power sensor carries none (sensor_utils.hpp).
        let body = r#"{
            "Id": "power_total_power",
            "ReadingType": "Power",
            "Reading": 412.5,
            "ReadingUnits": "W"
        }"#;
        assert_eq!(
            parse_redfish_power(body, RedfishSchema::Sensor),
            ParseOutcome::Ok(412.5)
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
