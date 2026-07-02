//! `analyze` acknowledgments: signature emission and ack-file filtering.

use crate::helpers::{
    ACK_FIXTURE, analyze_json, first_finding_signature, fixture_path, write_ack_file,
};
use serde_json::Value;
use std::process::Command;

// ── Acknowledgments (0.5.17) ─────────────────────────────────────────

#[test]
fn cli_analyze_signature_emitted_in_json() {
    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--no-acknowledgments",
        "--format",
        "json",
    ]);
    let findings = v["findings"].as_array().expect("findings array");
    assert!(!findings.is_empty(), "fixture must produce findings");
    for f in findings {
        let sig = f["signature"].as_str().expect("signature field present");
        assert!(!sig.is_empty(), "signature must be non-empty");
        assert_eq!(
            sig.matches(':').count(),
            3,
            "signature must have 4 colon-separated segments: {sig}"
        );
    }
}

#[test]
fn cli_analyze_with_acks_filters_output() {
    let sig = first_finding_signature();
    let dir = tempfile::tempdir().unwrap();
    let ack_path = write_ack_file(dir.path(), &sig);

    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--acknowledgments",
        ack_path.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let signatures: Vec<&str> = v["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter_map(|f| f["signature"].as_str())
        .collect();
    assert!(
        !signatures.contains(&sig.as_str()),
        "acked finding must be absent: signatures={signatures:?}"
    );
    // Without --show-acknowledged, the wire payload must omit the
    // acknowledged_findings array entirely (skip_serializing_if).
    assert!(
        v.get("acknowledged_findings").is_none(),
        "acknowledged_findings must be hidden by default: payload={v}"
    );
}

#[test]
fn cli_analyze_no_acknowledgments_flag_disables() {
    let sig = first_finding_signature();
    let dir = tempfile::tempdir().unwrap();
    let ack_path = write_ack_file(dir.path(), &sig);

    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--acknowledgments",
        ack_path.to_str().unwrap(),
        "--no-acknowledgments",
        "--format",
        "json",
    ]);
    let signatures: Vec<&str> = v["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter_map(|f| f["signature"].as_str())
        .collect();
    assert!(
        signatures.contains(&sig.as_str()),
        "--no-acknowledgments must surface the finding: signatures={signatures:?}"
    );
}

#[test]
fn cli_analyze_show_acknowledged_includes_in_output() {
    let sig = first_finding_signature();
    let dir = tempfile::tempdir().unwrap();
    let ack_path = write_ack_file(dir.path(), &sig);

    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--acknowledgments",
        ack_path.to_str().unwrap(),
        "--show-acknowledged",
        "--format",
        "json",
    ]);
    let acked = v["acknowledged_findings"]
        .as_array()
        .expect("acknowledged_findings present with --show-acknowledged");
    assert_eq!(acked.len(), 1, "exactly one ack matched");
    assert_eq!(
        acked[0]["finding"]["signature"].as_str(),
        Some(sig.as_str()),
        "ack finding signature roundtrips"
    );
    assert_eq!(
        acked[0]["acknowledgment"]["acknowledged_by"].as_str(),
        Some("test@example.com"),
        "ack metadata is preserved"
    );
}

#[test]
fn cli_analyze_acknowledgments_path_override() {
    // Two distinct dirs: cwd-side has no ack file, override path holds the ack.
    let sig = first_finding_signature();
    let cwd_dir = tempfile::tempdir().unwrap();
    let ack_dir = tempfile::tempdir().unwrap();
    let ack_path = write_ack_file(ack_dir.path(), &sig);

    // Run analyze from the cwd-side directory so the default lookup
    // would find no ack file. The --acknowledgments override must still
    // pick up the ack from `ack_dir`.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .current_dir(cwd_dir.path())
        .args([
            "analyze",
            "--input",
            &fixture_path(ACK_FIXTURE),
            "--acknowledgments",
            ack_path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("spawn perf-sentinel");
    assert!(
        output.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    let signatures: Vec<&str> = v["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter_map(|f| f["signature"].as_str())
        .collect();
    assert!(
        !signatures.contains(&sig.as_str()),
        "override path must apply the ack from outside cwd"
    );
}

#[test]
fn cli_analyze_no_ack_file_is_no_op() {
    // The default behavior must be a no-op when no ack file exists in
    // the cwd: zero error, all findings preserved.
    let cwd_dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .current_dir(cwd_dir.path())
        .args([
            "analyze",
            "--input",
            &fixture_path(ACK_FIXTURE),
            "--format",
            "json",
        ])
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "missing ack file must be a clean no-op, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = v["findings"].as_array().expect("findings array");
    assert!(!findings.is_empty(), "fixture must produce findings");
    assert!(
        v.get("acknowledged_findings").is_none(),
        "no-op path must omit acknowledged_findings entirely"
    );
}

#[test]
fn cli_signature_consistent_across_json_and_sarif() {
    let json_v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--no-acknowledgments",
        "--format",
        "json",
    ]);
    let json_findings = json_v["findings"].as_array().expect("findings array");
    assert!(
        !json_findings.is_empty(),
        "fixture must produce at least one finding"
    );

    let mut json_signatures: std::collections::HashMap<(String, String, String), String> =
        std::collections::HashMap::new();
    for f in json_findings {
        let key = (
            f["type"].as_str().unwrap().to_string(),
            f["service"].as_str().unwrap().to_string(),
            f["source_endpoint"].as_str().unwrap().to_string(),
        );
        let sig = f["signature"]
            .as_str()
            .expect("signature in JSON output")
            .to_string();
        assert!(!sig.is_empty(), "signature must be non-empty");
        json_signatures.insert(key, sig);
    }
    assert_eq!(
        json_signatures.len(),
        json_findings.len(),
        "(type, service, endpoint) triplet collisions in fixture would invalidate the cross-format match key; use a richer key"
    );

    let sarif_output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "analyze",
            "--input",
            &fixture_path(ACK_FIXTURE),
            "--no-acknowledgments",
            "--format",
            "sarif",
        ])
        .output()
        .expect("spawn perf-sentinel");
    assert!(
        sarif_output.status.success(),
        "sarif analyze failed: {}",
        String::from_utf8_lossy(&sarif_output.stderr)
    );
    let sarif: Value =
        serde_json::from_slice(&sarif_output.stdout).expect("sarif stdout must be valid JSON");

    let results = sarif["runs"][0]["results"]
        .as_array()
        .expect("sarif results array");
    assert_eq!(
        results.len(),
        json_findings.len(),
        "JSON and SARIF must emit the same number of results"
    );

    for r in results {
        let key = (
            r["ruleId"].as_str().unwrap().to_string(),
            r["logicalLocations"][0]["name"]
                .as_str()
                .unwrap()
                .to_string(),
            r["logicalLocations"][1]["name"]
                .as_str()
                .unwrap()
                .to_string(),
        );
        let expected_sig = json_signatures
            .get(&key)
            .unwrap_or_else(|| panic!("no JSON match for SARIF key {key:?}"));
        let props_sig = r["properties"]["signature"]
            .as_str()
            .unwrap_or_else(|| panic!("SARIF result missing properties.signature for {key:?}"));
        let fp_sig = r["fingerprints"]["perfsentinel/v1"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("SARIF result missing fingerprints[perfsentinel/v1] for {key:?}")
            });
        assert_eq!(
            props_sig, expected_sig,
            "properties.signature must match JSON signature for {key:?}"
        );
        assert_eq!(
            fp_sig, expected_sig,
            "fingerprints[perfsentinel/v1] must match JSON signature for {key:?}"
        );
    }
}
