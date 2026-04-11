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
    #[error("CSV parse error at line {line}: {reason}")]
    CsvParse { line: usize, reason: String },

    #[error("failed to parse timestamp '{value}' at line {line}: {reason}")]
    TimestampParse {
        line: usize,
        value: String,
        reason: String,
    },

    #[error("empty energy CSV: no data rows found")]
    EmptyData,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

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

/// Parse an ISO 8601 timestamp string into milliseconds since epoch.
///
/// Handles formats like `2025-07-10T14:32:01.123Z` and `2025-07-10T14:32:01Z`.
/// Rejects non-UTC timestamps.
fn parse_timestamp_ms(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if !s.ends_with('Z') {
        return Err("only UTC timestamps (ending with 'Z') are supported".to_string());
    }
    let without_z = &s[..s.len() - 1];

    // Split on 'T' or space
    let (date_part, time_part) = if let Some(pos) = without_z.find('T') {
        (&without_z[..pos], &without_z[pos + 1..])
    } else if let Some(pos) = without_z.find(' ') {
        (&without_z[..pos], &without_z[pos + 1..])
    } else {
        return Err("expected 'T' or space between date and time".to_string());
    };

    // Parse date: YYYY-MM-DD
    let date_parts: Vec<&str> = date_part.split('-').collect();
    if date_parts.len() != 3 {
        return Err("expected date format YYYY-MM-DD".to_string());
    }
    let year: u64 = date_parts[0]
        .parse()
        .map_err(|_| "invalid year".to_string())?;
    let month: u64 = date_parts[1]
        .parse()
        .map_err(|_| "invalid month".to_string())?;
    let day: u64 = date_parts[2]
        .parse()
        .map_err(|_| "invalid day".to_string())?;

    if year < 1970 {
        return Err("year must be >= 1970".to_string());
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err("month or day out of range".to_string());
    }

    // Parse time: HH:MM:SS[.mmm]
    let (time_no_frac, millis) = if let Some(dot_pos) = time_part.find('.') {
        let frac = &time_part[dot_pos + 1..];
        let ms: u64 = match frac.len() {
            1 => {
                frac.parse::<u64>()
                    .map_err(|_| "invalid fractional seconds")?
                    * 100
            }
            2 => {
                frac.parse::<u64>()
                    .map_err(|_| "invalid fractional seconds")?
                    * 10
            }
            3 => frac
                .parse::<u64>()
                .map_err(|_| "invalid fractional seconds")?,
            _ => frac[..3]
                .parse::<u64>()
                .map_err(|_| "invalid fractional seconds")?,
        };
        (&time_part[..dot_pos], ms)
    } else {
        (time_part, 0u64)
    };

    let time_parts: Vec<&str> = time_no_frac.split(':').collect();
    if time_parts.len() != 3 {
        return Err("expected time format HH:MM:SS".to_string());
    }
    let hours: u64 = time_parts[0]
        .parse()
        .map_err(|_| "invalid hours".to_string())?;
    let minutes: u64 = time_parts[1]
        .parse()
        .map_err(|_| "invalid minutes".to_string())?;
    let seconds: u64 = time_parts[2]
        .parse()
        .map_err(|_| "invalid seconds".to_string())?;

    if hours >= 24 || minutes >= 60 || seconds >= 60 {
        return Err("time values out of range".to_string());
    }

    // Convert date to days since epoch using the same algorithm as time.rs
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    let total_ms =
        days * 86_400_000 + hours * 3_600_000 + minutes * 60_000 + seconds * 1_000 + millis;
    Ok(total_ms)
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
}
