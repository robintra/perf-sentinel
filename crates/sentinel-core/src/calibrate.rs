//! Calibration mode: tune I/O-to-energy coefficients from real measurements.
//!
//! The `calibrate` subcommand correlates trace I/O ops with measured power or
//! energy readings (e.g. from Scaphandre RAPL exports or cloud monitoring) and
//! produces adjusted energy-per-op coefficients. These are written to a TOML
//! file that can be loaded via `[green] calibration_file`.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::event::SpanEvent;
use crate::score::carbon::ENERGY_PER_IO_OP_KWH;

// ---------------------------------------------------------------
// Error type
// ---------------------------------------------------------------

/// Errors that can occur during calibration.
#[derive(Debug, thiserror::Error)]
pub enum CalibrationError {
    /// A CSV row had the wrong number of columns, unparseable numeric
    /// values, or an unknown header layout. `line` is 1-indexed.
    #[error("CSV parse error at line {line}: {reason}")]
    CsvParse { line: usize, reason: String },

    /// An ISO 8601 timestamp column could not be parsed. Covers missing
    /// `Z` suffix, non-UTC offsets, and out-of-range date/time fields.
    #[error("failed to parse timestamp '{value}' at line {line}: {reason}")]
    TimestampParse {
        line: usize,
        value: String,
        reason: String,
    },

    /// The CSV parsed successfully but contained zero data rows. At
    /// least one measurement is required to compute a calibration.
    #[error("empty energy CSV: no data rows found")]
    EmptyData,

    /// Underlying filesystem I/O error when reading the CSV or writing
    /// the calibration TOML output.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The calibration TOML file loaded via `[green] calibration_file`
    /// failed TOML deserialization (malformed syntax, wrong types).
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// A semantic validation check failed on an otherwise well-formed
    /// calibration TOML: negative or non-finite factor, missing base
    /// energy, or a service factor outside the accepted range.
    #[error("validation error: {0}")]
    Validation(String),
}

// ---------------------------------------------------------------
// Energy CSV parsing
// ---------------------------------------------------------------

/// Whether the CSV uses power (watts) or direct energy (kWh).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CsvFormat {
    PowerWatts,
    EnergyKwh,
}

/// A single energy measurement reading.
#[derive(Debug, Clone)]
pub struct EnergyReading {
    pub timestamp_ms: u64,
    pub service: String,
    pub energy_kwh: f64,
}

/// Parse an ISO 8601 UTC timestamp into milliseconds since epoch.
///
/// Thin wrapper around [`crate::time::parse_iso8601_utc_to_ms`]. Kept
/// as a module-local function for a clearer stack trace on CSV errors
/// ("failed to parse timestamp at row 42") and because earlier versions
/// of this module had a hand-rolled implementation that has since been
/// centralized in `time.rs`.
fn parse_timestamp_ms(s: &str) -> Result<u64, String> {
    crate::time::parse_iso8601_utc_to_ms(s)
}

/// Parse an energy measurement CSV file.
///
/// Two column formats are supported, auto-detected from the header:
/// - `timestamp,service,power_watts`: power measurements converted to energy
///   using intervals between consecutive readings per service.
/// - `timestamp,service,energy_kwh`: direct energy readings.
///
/// Lines starting with `#` are treated as comments and skipped.
///
/// # Errors
///
/// Returns `CalibrationError::EmptyData` if no data rows are found, or
/// `CalibrationError::CsvParse` / `CalibrationError::TimestampParse` for
/// malformed rows.
pub fn parse_energy_csv(content: &str) -> Result<Vec<EnergyReading>, CalibrationError> {
    let mut lines = content.lines().enumerate();

    // Find header line (skip comments and blank lines)
    let format = loop {
        let (line_num, line) = lines.next().ok_or(CalibrationError::EmptyData)?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("power_watts") {
            break CsvFormat::PowerWatts;
        } else if lower.contains("energy_kwh") {
            break CsvFormat::EnergyKwh;
        }
        return Err(CalibrationError::CsvParse {
            line: line_num + 1,
            reason: "header must contain 'power_watts' or 'energy_kwh'".to_string(),
        });
    };

    // Parse data rows
    let mut raw_rows: Vec<(u64, String, f64)> = Vec::new();

    for (line_num, line) in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, ',').collect();
        if parts.len() != 3 {
            return Err(CalibrationError::CsvParse {
                line: line_num + 1,
                reason: "expected 3 comma-separated columns".to_string(),
            });
        }
        let ts = parse_timestamp_ms(parts[0].trim()).map_err(|reason| {
            CalibrationError::TimestampParse {
                line: line_num + 1,
                value: parts[0].trim().to_string(),
                reason,
            }
        })?;
        let service = parts[1].trim().to_string();
        let value: f64 = parts[2]
            .trim()
            .parse()
            .map_err(|_| CalibrationError::CsvParse {
                line: line_num + 1,
                reason: format!("invalid numeric value '{}'", parts[2].trim()),
            })?;
        if !value.is_finite() || value < 0.0 {
            return Err(CalibrationError::CsvParse {
                line: line_num + 1,
                reason: format!("invalid value: {value} (must be finite and non-negative)"),
            });
        }
        raw_rows.push((ts, service, value));
    }

    if raw_rows.is_empty() {
        return Err(CalibrationError::EmptyData);
    }

    match format {
        CsvFormat::EnergyKwh => Ok(raw_rows
            .into_iter()
            .map(|(ts, service, energy_kwh)| EnergyReading {
                timestamp_ms: ts,
                service,
                energy_kwh,
            })
            .collect()),
        CsvFormat::PowerWatts => convert_power_to_energy(raw_rows),
    }
}

/// Convert power (watts) readings to energy (kWh) by computing intervals
/// between consecutive readings per service.
fn convert_power_to_energy(
    mut rows: Vec<(u64, String, f64)>,
) -> Result<Vec<EnergyReading>, CalibrationError> {
    // Sort by service then timestamp for sequential processing
    rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));

    let mut readings = Vec::new();
    let mut prev: Option<(u64, &str, f64)> = None;

    for (ts, service, watts) in &rows {
        if let Some((prev_ts, prev_svc, prev_watts)) = prev
            && prev_svc == service.as_str()
        {
            let interval_secs = ts.saturating_sub(prev_ts) as f64 / 1000.0;
            if interval_secs > 0.0 {
                // Average power over the interval, converted to kWh
                let avg_watts = (prev_watts + watts) / 2.0;
                let energy_kwh = avg_watts * interval_secs / 3_600_000.0;
                readings.push(EnergyReading {
                    timestamp_ms: *ts,
                    service: service.clone(),
                    energy_kwh,
                });
            }
        }
        prev = Some((*ts, service, *watts));
    }

    if readings.is_empty() {
        return Err(CalibrationError::EmptyData);
    }

    Ok(readings)
}

// ---------------------------------------------------------------
// Calibration computation
// ---------------------------------------------------------------

/// Result of calibrating a single service.
#[derive(Debug, Clone)]
pub struct CalibrationResult {
    pub service: String,
    pub total_ops: u64,
    pub total_energy_kwh: f64,
    pub energy_per_op_kwh: f64,
    pub default_energy_per_op_kwh: f64,
    pub factor: f64,
}

/// Run calibration: correlate trace I/O ops with energy measurements.
///
/// For each service that appears in both traces and energy readings, compute:
/// - total I/O ops from the traces
/// - total energy from the readings
/// - energy per op = total energy / total ops
/// - calibration factor = energy per op / default proxy energy
///
/// Services with zero ops in the observation window are skipped.
///
/// # Errors
///
/// Returns `CalibrationError::EmptyData` if the readings slice is empty.
pub fn calibrate(
    events: &[SpanEvent],
    readings: &[EnergyReading],
) -> Result<Vec<CalibrationResult>, CalibrationError> {
    if readings.is_empty() {
        return Err(CalibrationError::EmptyData);
    }

    // Determine the observation window from the energy readings
    let window_start = readings.iter().map(|r| r.timestamp_ms).min().unwrap_or(0);
    let window_end = readings.iter().map(|r| r.timestamp_ms).max().unwrap_or(0);

    // Count I/O ops per service within the observation window.
    // Events with unparsable timestamps are skipped with a debug log.
    let mut ops_per_service: HashMap<String, u64> = HashMap::new();
    let mut skipped = 0usize;
    for event in events {
        let Ok(ts) = parse_timestamp_ms(&event.timestamp) else {
            skipped += 1;
            continue;
        };
        if ts >= window_start && ts <= window_end {
            *ops_per_service.entry(event.service.clone()).or_default() += 1;
        }
    }
    if skipped > 0 {
        tracing::debug!(
            skipped,
            "skipped events with unparsable timestamps during calibration"
        );
    }

    // Sum energy per service
    let mut energy_per_service: HashMap<String, f64> = HashMap::new();
    for reading in readings {
        *energy_per_service
            .entry(reading.service.clone())
            .or_default() += reading.energy_kwh;
    }

    // Compute calibration results for services with both measurements
    let mut results: Vec<CalibrationResult> = Vec::new();
    for (service, total_energy_kwh) in &energy_per_service {
        let total_ops = ops_per_service.get(service).copied().unwrap_or(0);
        if total_ops == 0 {
            tracing::warn!(
                service = %service,
                "no I/O ops found for service in the observation window, skipping"
            );
            continue;
        }
        let energy_per_op_kwh = total_energy_kwh / total_ops as f64;
        let factor = energy_per_op_kwh / ENERGY_PER_IO_OP_KWH;

        results.push(CalibrationResult {
            service: service.clone(),
            total_ops,
            total_energy_kwh: *total_energy_kwh,
            energy_per_op_kwh,
            default_energy_per_op_kwh: ENERGY_PER_IO_OP_KWH,
            factor,
        });
    }

    // Sort by service name for deterministic output
    results.sort_by(|a, b| a.service.cmp(&b.service));

    Ok(results)
}

// ---------------------------------------------------------------
// Calibration output (TOML file)
// ---------------------------------------------------------------

/// Generate a calibration TOML file from calibration results.
#[must_use]
pub fn write_calibration_toml(
    results: &[CalibrationResult],
    traces_path: &str,
    energy_path: &str,
) -> String {
    let now = chrono_like_now();
    let mut out = String::new();
    out.push_str("# Auto-generated by perf-sentinel calibrate\n");
    let _ = writeln!(out, "# Based on: {traces_path} + {energy_path}");
    let _ = writeln!(out, "# Date: {now}");
    out.push('\n');
    out.push_str("[calibration]\n");
    let _ = writeln!(out, "base_energy_per_io_op_kwh = {ENERGY_PER_IO_OP_KWH}");
    out.push('\n');
    out.push_str("[calibration.services]\n");
    for r in results {
        // Escape service name for TOML double-quoted string to prevent
        // injection via service names containing quotes or newlines.
        let escaped = r
            .service
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        let _ = writeln!(
            out,
            "\"{escaped}\" = {{ factor = {:.2}, measured_energy_per_op_kwh = {:.10} }}",
            r.factor, r.energy_per_op_kwh
        );
    }
    out
}

/// Simple UTC timestamp for the calibration file header.
fn chrono_like_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    crate::time::nanos_to_iso8601(now.as_nanos() as u64)
}

// ---------------------------------------------------------------
// Calibration data loading (for config integration)
// ---------------------------------------------------------------

/// Per-service calibration factor loaded from a calibration TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceCalibration {
    pub factor: f64,
    pub measured_energy_per_op_kwh: f64,
}

/// Calibration data loaded from a `.perf-sentinel-calibration.toml` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationData {
    pub calibration: CalibrationSection,
}

/// The `[calibration]` section in the calibration TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSection {
    pub base_energy_per_io_op_kwh: f64,
    #[serde(default)]
    pub services: HashMap<String, ServiceCalibration>,
}

impl CalibrationData {
    /// Look up the calibration factor for a service.
    ///
    /// Returns `None` if the service has no calibration entry.
    #[must_use]
    pub fn factor_for(&self, service: &str) -> Option<f64> {
        self.calibration.services.get(service).map(|s| s.factor)
    }
}

/// Load and validate a calibration TOML file.
///
/// # Errors
///
/// Returns `CalibrationError::Io` if the file cannot be read,
/// `CalibrationError::TomlParse` if parsing fails, or
/// `CalibrationError::Validation` for invalid factor values.
pub fn load_calibration_file(path: &str) -> Result<CalibrationData, CalibrationError> {
    let content = std::fs::read_to_string(path)?;
    let data: CalibrationData = toml::from_str(&content)?;

    // Validate factors
    for (service, cal) in &data.calibration.services {
        if !cal.factor.is_finite() || cal.factor < 0.0 {
            return Err(CalibrationError::Validation(format!(
                "service '{service}' has invalid calibration factor: {}",
                cal.factor
            )));
        }
        if !cal.measured_energy_per_op_kwh.is_finite() || cal.measured_energy_per_op_kwh < 0.0 {
            return Err(CalibrationError::Validation(format!(
                "service '{service}' has invalid measured_energy_per_op_kwh: {}",
                cal.measured_energy_per_op_kwh
            )));
        }
        if cal.factor == 0.0 {
            tracing::warn!(
                service = %service,
                "calibration factor is 0.0, service will have zero carbon impact"
            );
        }
        if cal.factor > 10.0 {
            tracing::warn!(
                service = %service,
                factor = cal.factor,
                "calibration factor > 10x default, possible measurement error"
            );
        }
        if cal.factor > 0.0 && cal.factor < 0.1 {
            tracing::warn!(
                service = %service,
                factor = cal.factor,
                "calibration factor < 0.1x default, possible measurement error"
            );
        }
    }

    Ok(data)
}

/// Validate calibration results for sanity.
#[must_use]
pub fn validate_results(results: &[CalibrationResult]) -> Vec<String> {
    let mut warnings = Vec::new();
    for r in results {
        if r.factor > 10.0 {
            warnings.push(format!(
                "{}: factor {:.1}x is > 10x default, possible measurement error",
                r.service, r.factor
            ));
        }
        if r.factor > 0.0 && r.factor < 0.1 {
            warnings.push(format!(
                "{}: factor {:.1}x is < 0.1x default, possible measurement error",
                r.service, r.factor
            ));
        }
    }
    warnings
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventSource, EventType};

    fn make_event(service: &str, timestamp: &str) -> SpanEvent {
        SpanEvent {
            timestamp: timestamp.to_string(),
            trace_id: "trace-1".to_string(),
            span_id: "span-1".to_string(),
            parent_span_id: None,
            service: service.to_string(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: "SELECT * FROM t".to_string(),
            duration_us: 100,
            status_code: None,
            response_size_bytes: None,
            source: EventSource {
                endpoint: "GET /api/test".to_string(),
                method: "test".to_string(),
            },
        }
    }

    // --- Timestamp parsing ---

    #[test]
    fn parse_timestamp_basic() {
        let ms = parse_timestamp_ms("2025-07-10T14:32:01.123Z").unwrap();
        assert!(ms > 0);
    }

    #[test]
    fn parse_timestamp_no_fraction() {
        let ms = parse_timestamp_ms("2025-07-10T14:32:01Z").unwrap();
        assert!(ms > 0);
    }

    #[test]
    fn parse_timestamp_rejects_non_utc() {
        assert!(parse_timestamp_ms("2025-07-10T14:32:01+02:00").is_err());
    }

    #[test]
    fn parse_timestamp_rejects_invalid() {
        assert!(parse_timestamp_ms("not-a-timestamp").is_err());
        assert!(parse_timestamp_ms("").is_err());
    }

    // --- CSV parsing: energy_kwh format ---

    #[test]
    fn parse_csv_energy_format() {
        let csv = "timestamp,service,energy_kwh\n\
                   2025-07-10T14:00:00Z,order-svc,0.0001\n\
                   2025-07-10T14:05:00Z,order-svc,0.0002\n";
        let readings = parse_energy_csv(csv).unwrap();
        assert_eq!(readings.len(), 2);
        assert_eq!(readings[0].service, "order-svc");
        assert!((readings[0].energy_kwh - 0.0001).abs() < 1e-10);
    }

    #[test]
    fn parse_csv_power_format() {
        let csv = "timestamp,service,power_watts\n\
                   2025-07-10T14:00:00Z,svc-a,10.0\n\
                   2025-07-10T14:00:05Z,svc-a,12.0\n";
        let readings = parse_energy_csv(csv).unwrap();
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].service, "svc-a");
        // Average power = (10+12)/2 = 11W, interval = 5s
        // Energy = 11 * 5 / 3_600_000 kWh
        let expected = 11.0 * 5.0 / 3_600_000.0;
        assert!((readings[0].energy_kwh - expected).abs() < 1e-15);
    }

    #[test]
    fn parse_csv_with_comments() {
        let csv = "# Energy measurements\n\
                   timestamp,service,energy_kwh\n\
                   # First reading\n\
                   2025-07-10T14:00:00Z,svc-a,0.001\n";
        let readings = parse_energy_csv(csv).unwrap();
        assert_eq!(readings.len(), 1);
    }

    #[test]
    fn parse_csv_empty_data() {
        let csv = "timestamp,service,energy_kwh\n";
        assert!(matches!(
            parse_energy_csv(csv),
            Err(CalibrationError::EmptyData)
        ));
    }

    #[test]
    fn parse_csv_rejects_negative() {
        let csv = "timestamp,service,energy_kwh\n\
                   2025-07-10T14:00:00Z,svc-a,-0.001\n";
        assert!(matches!(
            parse_energy_csv(csv),
            Err(CalibrationError::CsvParse { .. })
        ));
    }

    #[test]
    fn parse_csv_rejects_bad_header() {
        let csv = "timestamp,service,something\n\
                   2025-07-10T14:00:00Z,svc-a,0.001\n";
        assert!(matches!(
            parse_energy_csv(csv),
            Err(CalibrationError::CsvParse { .. })
        ));
    }

    #[test]
    fn parse_csv_rejects_malformed_row() {
        let csv = "timestamp,service,energy_kwh\n\
                   2025-07-10T14:00:00Z,svc-a\n";
        assert!(matches!(
            parse_energy_csv(csv),
            Err(CalibrationError::CsvParse { .. })
        ));
    }

    // --- Calibration computation ---

    #[test]
    fn calibrate_basic() {
        let events = vec![
            make_event("svc-a", "2025-07-10T14:00:01Z"),
            make_event("svc-a", "2025-07-10T14:00:02Z"),
            make_event("svc-a", "2025-07-10T14:00:03Z"),
            make_event("svc-a", "2025-07-10T14:00:04Z"),
        ];
        let readings = vec![
            EnergyReading {
                timestamp_ms: parse_timestamp_ms("2025-07-10T14:00:00Z").unwrap(),
                service: "svc-a".to_string(),
                energy_kwh: 0.000_000_2, // 2e-7
            },
            EnergyReading {
                timestamp_ms: parse_timestamp_ms("2025-07-10T14:00:05Z").unwrap(),
                service: "svc-a".to_string(),
                energy_kwh: 0.000_000_2,
            },
        ];

        let results = calibrate(&events, &readings).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].service, "svc-a");
        assert_eq!(results[0].total_ops, 4);
        let expected_energy = 0.000_000_4; // sum of readings
        assert!((results[0].total_energy_kwh - expected_energy).abs() < 1e-15);
        let expected_per_op = expected_energy / 4.0;
        assert!((results[0].energy_per_op_kwh - expected_per_op).abs() < 1e-15);
        assert!((results[0].factor - (expected_per_op / ENERGY_PER_IO_OP_KWH)).abs() < 1e-10);
    }

    #[test]
    fn calibrate_multiple_services() {
        let events = vec![
            make_event("svc-a", "2025-07-10T14:00:01Z"),
            make_event("svc-a", "2025-07-10T14:00:02Z"),
            make_event("svc-b", "2025-07-10T14:00:01Z"),
        ];
        let readings = vec![
            EnergyReading {
                timestamp_ms: parse_timestamp_ms("2025-07-10T14:00:00Z").unwrap(),
                service: "svc-a".to_string(),
                energy_kwh: 0.000_001,
            },
            EnergyReading {
                timestamp_ms: parse_timestamp_ms("2025-07-10T14:00:05Z").unwrap(),
                service: "svc-b".to_string(),
                energy_kwh: 0.000_000_5,
            },
        ];

        let results = calibrate(&events, &readings).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].service, "svc-a");
        assert_eq!(results[0].total_ops, 2);
        assert_eq!(results[1].service, "svc-b");
        assert_eq!(results[1].total_ops, 1);
    }

    #[test]
    fn calibrate_skips_zero_ops_service() {
        let events = vec![make_event("svc-a", "2025-07-10T14:00:01Z")];
        let readings = vec![
            EnergyReading {
                timestamp_ms: parse_timestamp_ms("2025-07-10T14:00:00Z").unwrap(),
                service: "svc-a".to_string(),
                energy_kwh: 0.0001,
            },
            EnergyReading {
                timestamp_ms: parse_timestamp_ms("2025-07-10T14:00:05Z").unwrap(),
                service: "svc-b".to_string(), // no ops for svc-b
                energy_kwh: 0.0001,
            },
        ];

        let results = calibrate(&events, &readings).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].service, "svc-a");
    }

    // --- TOML output ---

    #[test]
    fn write_toml_round_trip() {
        let results = vec![CalibrationResult {
            service: "svc-a".to_string(),
            total_ops: 100,
            total_energy_kwh: 0.00001,
            energy_per_op_kwh: 0.000_000_1,
            default_energy_per_op_kwh: ENERGY_PER_IO_OP_KWH,
            factor: 1.0,
        }];

        let toml_str = write_calibration_toml(&results, "traces.json", "energy.csv");
        assert!(toml_str.contains("[calibration]"));
        assert!(toml_str.contains("[calibration.services]"));
        assert!(toml_str.contains("svc-a"));

        // Round-trip: parse the generated TOML back
        let data: CalibrationData = toml::from_str(&toml_str).unwrap();
        assert!(data.calibration.services.contains_key("svc-a"));
        let cal = &data.calibration.services["svc-a"];
        assert!((cal.factor - 1.0).abs() < 0.01);
    }

    // --- Calibration file loading ---

    #[test]
    fn load_calibration_validates_negative_factor() {
        let toml_str = r#"
[calibration]
base_energy_per_io_op_kwh = 0.000_000_1

[calibration.services]
"svc-a" = { factor = -1.0, measured_energy_per_op_kwh = 0.000_000_1 }
"#;
        let tmp = std::env::temp_dir().join("test-cal-negative.toml");
        std::fs::write(&tmp, toml_str).unwrap();
        let result = load_calibration_file(tmp.to_str().unwrap());
        assert!(matches!(result, Err(CalibrationError::Validation(_))));
        let _ = std::fs::remove_file(tmp);
    }

    // --- Validation ---

    #[test]
    fn validate_warns_extreme_factors() {
        let results = vec![
            CalibrationResult {
                service: "normal".to_string(),
                total_ops: 100,
                total_energy_kwh: 0.00001,
                energy_per_op_kwh: ENERGY_PER_IO_OP_KWH,
                default_energy_per_op_kwh: ENERGY_PER_IO_OP_KWH,
                factor: 1.0,
            },
            CalibrationResult {
                service: "too-high".to_string(),
                total_ops: 100,
                total_energy_kwh: 0.001,
                energy_per_op_kwh: ENERGY_PER_IO_OP_KWH * 15.0,
                default_energy_per_op_kwh: ENERGY_PER_IO_OP_KWH,
                factor: 15.0,
            },
            CalibrationResult {
                service: "too-low".to_string(),
                total_ops: 100,
                total_energy_kwh: 0.000_000_1,
                energy_per_op_kwh: ENERGY_PER_IO_OP_KWH * 0.05,
                default_energy_per_op_kwh: ENERGY_PER_IO_OP_KWH,
                factor: 0.05,
            },
        ];
        let warnings = validate_results(&results);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("too-high"));
        assert!(warnings[1].contains("too-low"));
    }

    // --- parse_timestamp_ms error branches ---

    #[test]
    fn parse_timestamp_accepts_space_between_date_and_time() {
        // Exercises the space-separator branch of parse_timestamp_ms.
        let ms = parse_timestamp_ms("2025-07-10 14:32:01Z").unwrap();
        let t_form = parse_timestamp_ms("2025-07-10T14:32:01Z").unwrap();
        assert_eq!(ms, t_form);
    }

    #[test]
    fn parse_timestamp_rejects_missing_t_and_space() {
        let err = parse_timestamp_ms("2025-07-1014:32:01Z").unwrap_err();
        assert!(err.contains("'T' or space"));
    }

    #[test]
    fn parse_timestamp_rejects_wrong_date_format() {
        let err = parse_timestamp_ms("2025-07T14:32:01Z").unwrap_err();
        assert!(err.contains("YYYY-MM-DD"));
    }

    #[test]
    fn parse_timestamp_rejects_non_numeric_year_month_day() {
        assert!(
            parse_timestamp_ms("abcd-07-10T14:32:01Z")
                .unwrap_err()
                .contains("year")
        );
        assert!(
            parse_timestamp_ms("2025-ab-10T14:32:01Z")
                .unwrap_err()
                .contains("month")
        );
        assert!(
            parse_timestamp_ms("2025-07-abT14:32:01Z")
                .unwrap_err()
                .contains("day")
        );
    }

    #[test]
    fn parse_timestamp_rejects_pre_1970_year() {
        let err = parse_timestamp_ms("1969-12-31T23:59:59Z").unwrap_err();
        assert!(err.contains("1970"));
    }

    #[test]
    fn parse_timestamp_rejects_month_day_out_of_range() {
        assert!(
            parse_timestamp_ms("2025-13-01T00:00:00Z")
                .unwrap_err()
                .contains("out of range")
        );
        assert!(
            parse_timestamp_ms("2025-07-32T00:00:00Z")
                .unwrap_err()
                .contains("out of range")
        );
    }

    #[test]
    fn parse_timestamp_rejects_wrong_time_format() {
        let err = parse_timestamp_ms("2025-07-10T14:32Z").unwrap_err();
        assert!(err.contains("HH:MM:SS"));
    }

    #[test]
    fn parse_timestamp_rejects_non_numeric_hours_minutes_seconds() {
        assert!(
            parse_timestamp_ms("2025-07-10Tab:32:01Z")
                .unwrap_err()
                .contains("hours")
        );
        assert!(
            parse_timestamp_ms("2025-07-10T14:ab:01Z")
                .unwrap_err()
                .contains("minutes")
        );
        assert!(
            parse_timestamp_ms("2025-07-10T14:32:abZ")
                .unwrap_err()
                .contains("seconds")
        );
    }

    #[test]
    fn parse_timestamp_rejects_time_out_of_range() {
        let err = parse_timestamp_ms("2025-07-10T25:00:00Z").unwrap_err();
        assert!(err.contains("out of range"));
    }

    #[test]
    fn parse_timestamp_accepts_1_digit_fractional_seconds() {
        // Exercises the `1 => ... * 100` branch of the fractional parser.
        let ms_1 = parse_timestamp_ms("2025-07-10T14:32:01.5Z").unwrap();
        let ms_base = parse_timestamp_ms("2025-07-10T14:32:01Z").unwrap();
        assert_eq!(ms_1 - ms_base, 500);
    }

    #[test]
    fn parse_timestamp_accepts_2_digit_fractional_seconds() {
        // Exercises the `2 => ... * 10` branch.
        let ms_2 = parse_timestamp_ms("2025-07-10T14:32:01.25Z").unwrap();
        let ms_base = parse_timestamp_ms("2025-07-10T14:32:01Z").unwrap();
        assert_eq!(ms_2 - ms_base, 250);
    }

    #[test]
    fn parse_timestamp_truncates_sub_millisecond_fractional_seconds() {
        // Exercises the `_ => frac[..3].parse()` branch for 4+ digits.
        let ms = parse_timestamp_ms("2025-07-10T14:32:01.123456Z").unwrap();
        let ms_base = parse_timestamp_ms("2025-07-10T14:32:01Z").unwrap();
        assert_eq!(ms - ms_base, 123);
    }

    #[test]
    fn parse_timestamp_rejects_non_numeric_fractional_seconds() {
        let err = parse_timestamp_ms("2025-07-10T14:32:01.abZ").unwrap_err();
        assert!(err.contains("fractional"));
    }

    // --- parse_energy_csv error branches ---

    #[test]
    fn parse_energy_csv_reports_invalid_timestamp_with_line_number() {
        let csv = "timestamp,service,energy_kwh\n\
                   not-a-timestamp,svc-a,0.001\n";
        let err = parse_energy_csv(csv).unwrap_err();
        match err {
            CalibrationError::TimestampParse { line, value, .. } => {
                assert_eq!(line, 2);
                assert_eq!(value, "not-a-timestamp");
            }
            other => panic!("expected TimestampParse, got {other:?}"),
        }
    }

    #[test]
    fn parse_energy_csv_reports_invalid_numeric_value_with_line_number() {
        let csv = "timestamp,service,energy_kwh\n\
                   2025-07-10T14:00:00Z,svc-a,not-a-number\n";
        let err = parse_energy_csv(csv).unwrap_err();
        match err {
            CalibrationError::CsvParse { line, reason } => {
                assert_eq!(line, 2);
                assert!(reason.contains("not-a-number"));
            }
            other => panic!("expected CsvParse, got {other:?}"),
        }
    }

    #[test]
    fn parse_energy_csv_power_watts_empty_after_conversion_returns_empty_data() {
        // A single power reading cannot be converted (needs a pair).
        // After conversion, the result is empty → EmptyData error.
        let csv = "timestamp,service,power_watts\n\
                   2025-07-10T14:00:00Z,svc-a,12.5\n";
        let err = parse_energy_csv(csv).unwrap_err();
        assert!(matches!(err, CalibrationError::EmptyData));
    }

    // --- calibrate() error branches ---

    #[test]
    fn calibrate_rejects_empty_readings() {
        let events = vec![make_event("svc-a", "2025-07-10T14:00:00Z")];
        let err = calibrate(&events, &[]).unwrap_err();
        assert!(matches!(err, CalibrationError::EmptyData));
    }

    #[test]
    fn calibrate_skips_events_with_unparsable_timestamp() {
        // The trace event has a garbage timestamp — calibrate() must skip
        // it silently via the `let Ok(ts) = ... else continue` path and
        // still produce a valid result for the other events.
        let mut bad_event = make_event("svc-a", "2025-07-10T14:00:05Z");
        bad_event.timestamp = "not-a-timestamp".to_string();
        let events = vec![bad_event, make_event("svc-a", "2025-07-10T14:00:05Z")];
        let readings = vec![
            EnergyReading {
                timestamp_ms: parse_timestamp_ms("2025-07-10T14:00:00Z").unwrap(),
                service: "svc-a".to_string(),
                energy_kwh: 0.000_000_2,
            },
            EnergyReading {
                timestamp_ms: parse_timestamp_ms("2025-07-10T14:00:10Z").unwrap(),
                service: "svc-a".to_string(),
                energy_kwh: 0.000_000_2,
            },
        ];
        let results = calibrate(&events, &readings).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].total_ops, 1, "bad-timestamp event was skipped");
    }

    // --- load_calibration_file error branches ---

    #[test]
    fn load_calibration_file_rejects_missing_file() {
        let err = load_calibration_file("/tmp/does-not-exist-abc123.toml").unwrap_err();
        assert!(matches!(err, CalibrationError::Io(_)));
    }

    /// Write `contents` to a fresh file inside a `tempfile::TempDir` and
    /// return the owned dir + path. The dir is auto-cleaned on drop, and
    /// the path is unique per test invocation, avoiding symlink TOCTOU
    /// and parallel-run collisions from predictable `/tmp/...` names.
    fn write_temp_toml(filename: &str, contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir creation");
        let path = dir.path().join(filename);
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn load_calibration_file_rejects_malformed_toml() {
        let (_dir, path) = write_temp_toml("malformed.toml", "not = valid [toml");
        let err = load_calibration_file(path.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, CalibrationError::TomlParse(_)));
    }

    #[test]
    fn load_calibration_file_rejects_negative_factor() {
        let (_dir, path) = write_temp_toml(
            "neg.toml",
            r#"
[calibration]
base_energy_per_io_op_kwh = 0.0000001

[calibration.services]
"svc-a" = { factor = -1.0, measured_energy_per_op_kwh = 0.0000001 }
"#,
        );
        let err = load_calibration_file(path.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, CalibrationError::Validation(_)));
    }

    #[test]
    fn load_calibration_file_rejects_nonfinite_measured_energy() {
        let (_dir, path) = write_temp_toml(
            "inf.toml",
            r#"
[calibration]
base_energy_per_io_op_kwh = 0.0000001

[calibration.services]
"svc-a" = { factor = 1.0, measured_energy_per_op_kwh = nan }
"#,
        );
        let err = load_calibration_file(path.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, CalibrationError::Validation(_)));
    }

    #[test]
    fn load_calibration_file_accepts_extreme_factors_with_warning() {
        // Factors 0.0, > 10, < 0.1 all emit warnings but load successfully.
        // This exercises the three `tracing::warn!` branches in load_calibration_file.
        let (_dir, path) = write_temp_toml(
            "warn.toml",
            r#"
[calibration]
base_energy_per_io_op_kwh = 0.0000001

[calibration.services]
"zero" = { factor = 0.0, measured_energy_per_op_kwh = 0.0 }
"too-high" = { factor = 15.0, measured_energy_per_op_kwh = 0.0000015 }
"too-low" = { factor = 0.05, measured_energy_per_op_kwh = 0.000000005 }
"normal" = { factor = 1.0, measured_energy_per_op_kwh = 0.0000001 }
"#,
        );
        let data = load_calibration_file(path.to_str().unwrap()).unwrap();
        assert_eq!(data.calibration.services.len(), 4);
    }
}
