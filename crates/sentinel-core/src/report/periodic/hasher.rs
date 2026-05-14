//! Deterministic SHA-256 content hash for a [`PeriodicReport`].
//! Canonical form (sorted keys, compact JSON, blanked `content_hash`)
//! and design rationale: `docs/design/08-PERIODIC-DISCLOSURE.md`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::errors::HashError;
use super::schema::PeriodicReport;

/// Soft cap on the binary read in [`binary_hash`]. perf-sentinel release
/// binaries are tens of MiB; this guards against `current_exe` resolving
/// to an unexpectedly large path (e.g. a procfs link).
const BINARY_HASH_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Compute the canonical SHA-256 content hash of a report.
///
/// The returned string is prefixed with `"sha256:"` and contains 64
/// lowercase hex characters.
///
/// # Errors
///
/// Returns [`HashError::Serialize`] if the report cannot be serialised to
/// JSON, which in practice only happens if a float is non-finite.
pub fn compute_content_hash(report: &PeriodicReport) -> Result<String, HashError> {
    let mut value = serde_json::to_value(report)?;
    blank_content_hash(&mut value);
    let canonical = canonicalize(value);
    let bytes = serde_json::to_vec(&canonical)?;
    Ok(format_sha256(&bytes))
}

fn blank_content_hash(v: &mut Value) {
    if let Some(integrity) = v.get_mut("integrity").and_then(Value::as_object_mut) {
        integrity.insert("content_hash".to_string(), Value::String(String::new()));
    }
}

/// Recursively re-build every JSON object via `BTreeMap` so the output
/// has sorted keys regardless of how `serde_json::Map` happens to be
/// configured upstream. Removing this collect would silently break the
/// hash determinism the moment a transitive crate flips the
/// `serde_json/preserve_order` feature.
fn canonicalize(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .into_iter()
                .map(|(k, val)| (k, canonicalize(val)))
                .collect();
            let mut out = serde_json::Map::new();
            for (k, val) in sorted {
                out.insert(k, val);
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

fn format_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Hash the running binary at `std::env::current_exe()` and return the
/// `"sha256:<64-hex>"` string used by
/// [`crate::report::periodic::schema::Integrity::binary_hash`].
///
/// Streams the file via a `BufReader` (no whole-binary allocation) and
/// caps the read at `BINARY_HASH_MAX_BYTES` so an unexpectedly large
/// `current_exe` resolution cannot OOM the process.
///
/// # Errors
///
/// Returns the I/O error from `current_exe` or the file read when the
/// running executable cannot be resolved or read.
pub fn binary_hash() -> std::io::Result<String> {
    let path = std::env::current_exe()?;
    let file = std::fs::File::open(&path)?;
    let total_len = file.metadata().map_or(0, |m| m.len());
    if total_len > BINARY_HASH_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "binary at {} exceeds {} byte cap ({} bytes), refusing to hash a truncated view",
                path.display(),
                BINARY_HASH_MAX_BYTES,
                total_len
            ),
        ));
    }
    let mut reader = std::io::BufReader::new(file).take(BINARY_HASH_MAX_BYTES);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::periodic::schema::{
        Aggregate, AntiPatternDetail, Application, ApplicationG1, CalibrationInputs,
        Confidentiality, Conformance, Integrity, IntegrityLevel, Methodology, Notes,
        OrgIdentifiers, Organisation, Period, PeriodType, PeriodicReport, ReportIntent,
        ReportMetadata, SCHEMA_VERSION, ScopeManifest, core_patterns_required,
    };
    use chrono::{DateTime, NaiveDate, Utc};
    use uuid::Uuid;

    fn sample_report() -> PeriodicReport {
        PeriodicReport {
            schema_version: SCHEMA_VERSION.to_string(),
            report_metadata: ReportMetadata {
                intent: ReportIntent::Official,
                confidentiality_level: Confidentiality::Public,
                integrity_level: IntegrityLevel::HashOnly,
                generated_at: DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                generated_by: "cli-batch".to_string(),
                perf_sentinel_version: "0.6.2".to_string(),
                report_uuid: Uuid::nil(),
            },
            organisation: Organisation {
                name: "Acme".to_string(),
                country: "FR".to_string(),
                identifiers: OrgIdentifiers::default(),
                sector: None,
            },
            period: Period {
                from_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                to_date: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
                period_type: PeriodType::CalendarQuarter,
                days_covered: 90,
            },
            scope_manifest: ScopeManifest {
                total_applications_declared: 1,
                applications_measured: 1,
                applications_excluded: vec![],
                environments_measured: vec!["prod".to_string()],
                environments_excluded: vec![],
                total_requests_in_period: None,
                requests_measured: 1000,
                coverage_percentage: None,
            },
            methodology: Methodology {
                sci_specification: "ISO/IEC 21031:2024".to_string(),
                perf_sentinel_version: "0.6.2".to_string(),
                enabled_patterns: vec!["n_plus_one_sql".to_string()],
                disabled_patterns: vec![],
                core_patterns_required: core_patterns_required(),
                conformance: Conformance::CoreRequired,
                calibration_inputs: CalibrationInputs {
                    cloud_regions: vec!["eu-west-3".to_string()],
                    carbon_intensity_source: "electricity_maps".to_string(),
                    specpower_table_version: "2024-2026".to_string(),
                    scaphandre_used: false,
                },
            },
            aggregate: Aggregate {
                total_requests: 1000,
                total_energy_kwh: 0.5,
                total_carbon_kgco2eq: 0.05,
                aggregate_efficiency_score: 90.0,
                aggregate_waste_ratio: 0.1,
                anti_patterns_detected_count: 3,
                estimated_optimization_potential_kgco2eq: 0.01,
            },
            applications: vec![Application::G1(ApplicationG1 {
                service_name: "svc".to_string(),
                display_name: None,
                service_version: None,
                endpoints_observed: 1,
                total_requests: 1000,
                energy_kwh: 0.5,
                carbon_kgco2eq: 0.05,
                efficiency_score: 90.0,
                anti_patterns: vec![AntiPatternDetail {
                    kind: "n_plus_one_sql".to_string(),
                    occurrences: 3,
                    estimated_waste_kwh: 0.02,
                    estimated_waste_kgco2eq: 0.003,
                    first_seen: DateTime::parse_from_rfc3339("2026-01-05T00:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                    last_seen: DateTime::parse_from_rfc3339("2026-03-20T00:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                }],
            })],
            integrity: Integrity {
                content_hash: String::new(),
                binary_hash: None,
                binary_verification_url: None,
                trace_integrity_chain: Value::Null,
                signature: Value::Null,
            },
            notes: Notes::default(),
        }
    }

    #[test]
    fn hash_is_deterministic() {
        let r = sample_report();
        let first = compute_content_hash(&r).unwrap();
        for _ in 0..100 {
            assert_eq!(compute_content_hash(&r).unwrap(), first);
        }
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), 7 + 64);
        assert!(first[7..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_changes_on_aggregate_mutation() {
        let r = sample_report();
        let baseline = compute_content_hash(&r).unwrap();

        let mut mutated = r.clone();
        mutated.aggregate.total_energy_kwh += 0.000_001;
        let after = compute_content_hash(&mutated).unwrap();

        assert_ne!(baseline, after);
    }

    #[test]
    fn hash_ignores_existing_content_hash() {
        let mut r = sample_report();
        r.integrity.content_hash = "sha256:aaaa".to_string();
        let first = compute_content_hash(&r).unwrap();

        r.integrity.content_hash = "sha256:bbbb".to_string();
        let second = compute_content_hash(&r).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn canonicalize_is_key_order_invariant() {
        let a = serde_json::json!({ "alpha": 1, "beta": 2, "gamma": 3 });
        let b = serde_json::json!({ "gamma": 3, "alpha": 1, "beta": 2 });
        let ca = canonicalize(a);
        let cb = canonicalize(b);
        assert_eq!(
            serde_json::to_vec(&ca).unwrap(),
            serde_json::to_vec(&cb).unwrap()
        );
    }

    #[test]
    fn canonicalize_recurses_into_nested_objects() {
        let a = serde_json::json!({
            "outer": { "z": 1, "a": 2 },
            "list": [{ "b": 1, "a": 2 }]
        });
        let b = serde_json::json!({
            "list": [{ "a": 2, "b": 1 }],
            "outer": { "a": 2, "z": 1 }
        });
        assert_eq!(
            serde_json::to_vec(&canonicalize(a)).unwrap(),
            serde_json::to_vec(&canonicalize(b)).unwrap(),
        );
    }

    #[test]
    fn hash_blanks_content_hash_without_removing_key() {
        let r = sample_report();
        let mut v = serde_json::to_value(&r).unwrap();
        blank_content_hash(&mut v);
        let integrity = v.get("integrity").and_then(Value::as_object).unwrap();
        assert!(integrity.contains_key("content_hash"));
        assert_eq!(
            integrity.get("content_hash"),
            Some(&Value::String(String::new()))
        );
    }

    #[test]
    fn format_sha256_known_vector() {
        // SHA-256 of the empty string, well-known constant.
        let empty = format_sha256(&[]);
        assert_eq!(
            empty,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
