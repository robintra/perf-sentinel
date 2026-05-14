//! Structural and validator-level checks on the published example
//! reports in `docs/schemas/examples/`. JSON Schema validation itself
//! is exercised by external tooling (CI matrix, future
//! `--features schema-test` job); the Rust tests here guarantee that
//! the canonical examples remain parseable and obey the official-intent
//! validator.

use std::path::PathBuf;

use sentinel_core::report::periodic::schema::{
    Application, Confidentiality, PeriodicReport, ReportIntent,
};
use sentinel_core::report::periodic::{compute_content_hash, validate_official};

fn workspace_doc(rel: &str) -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("..").join("..").join(rel)
}

fn load_example(rel: &str) -> PeriodicReport {
    let path = workspace_doc(rel);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn example_internal_g1_parses_and_validates() {
    let r = load_example("docs/schemas/examples/example-internal-G1.json");
    assert_eq!(r.schema_version, "perf-sentinel-report/v1.0");
    assert!(matches!(r.report_metadata.intent, ReportIntent::Internal));
    assert!(matches!(
        r.report_metadata.confidentiality_level,
        Confidentiality::Internal
    ));
    assert!(
        r.applications
            .iter()
            .all(|a| matches!(a, Application::G1(_)))
    );
    // Internal intent: validator is permissive.
    validate_official(&r).expect("internal intent must pass");
    // Hash recomputation should yield a well-formed value.
    let hash = compute_content_hash(&r).expect("hash succeeds");
    assert!(hash.starts_with("sha256:"));
    assert_eq!(hash.len(), 7 + 64);
}

#[test]
fn example_official_public_g2_parses_and_validates() {
    let r = load_example("docs/schemas/examples/example-official-public-G2.json");
    assert_eq!(r.schema_version, "perf-sentinel-report/v1.0");
    assert!(matches!(r.report_metadata.intent, ReportIntent::Official));
    assert!(matches!(
        r.report_metadata.confidentiality_level,
        Confidentiality::Public
    ));
    assert!(
        r.applications
            .iter()
            .all(|a| matches!(a, Application::G2(_)))
    );
    validate_official(&r).expect("official public example must pass validator");
    let hash = compute_content_hash(&r).expect("hash succeeds");
    assert!(hash.starts_with("sha256:"));
}

#[test]
fn example_hashes_are_deterministic() {
    let g1 = load_example("docs/schemas/examples/example-internal-G1.json");
    let g2 = load_example("docs/schemas/examples/example-official-public-G2.json");
    let h1a = compute_content_hash(&g1).unwrap();
    let h1b = compute_content_hash(&g1).unwrap();
    let h2a = compute_content_hash(&g2).unwrap();
    let h2b = compute_content_hash(&g2).unwrap();
    assert_eq!(h1a, h1b);
    assert_eq!(h2a, h2b);
    assert_ne!(h1a, h2a, "different examples should hash differently");
}

#[test]
fn example_internal_g1_has_all_quality_signal_fields() {
    let r = load_example("docs/schemas/examples/example-internal-G1.json");
    assert!(r.aggregate.period_coverage > 0.0);
    assert!(r.aggregate.runtime_windows_count > 0);
    assert!(!r.aggregate.binary_versions.is_empty());
    assert!(!r.aggregate.per_service_energy_models.is_empty());
    assert!(!r.aggregate.per_service_measured_ratio.is_empty());
    assert!(!r.report_metadata.binary_version.is_empty());
    assert!(r.methodology.calibration_inputs.calibration_applied);
}

#[test]
fn example_official_public_g2_has_all_quality_signal_fields() {
    let r = load_example("docs/schemas/examples/example-official-public-G2.json");
    assert!(r.aggregate.period_coverage >= 0.75);
    assert!(r.aggregate.runtime_windows_count > 0);
    assert!(!r.aggregate.binary_versions.is_empty());
    assert!(!r.aggregate.per_service_energy_models.is_empty());
    assert!(!r.aggregate.per_service_measured_ratio.is_empty());
}

#[test]
fn example_disclaimers_include_quality_signals() {
    let r = load_example("docs/schemas/examples/example-internal-G1.json");
    let combined = r.notes.disclaimers.join(" ");
    assert!(combined.contains("Calibration applied"));
    assert!(combined.contains("multiple perf-sentinel binary versions"));
}
