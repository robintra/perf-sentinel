//! Shared helpers and fixtures for the e2e test modules.

use serde_json::Value;
use std::fs;
use std::process::Command;

/// Extract the embedded JSON payload from a rendered HTML dashboard.
/// Mirrors the test helper in `report::html::tests::extract_payload_json`
/// but lives here so the e2e tier does not reach into the core crate.
pub(crate) fn extract_payload_json_from_html(html: &str) -> Value {
    let tag = "<script id=\"report-data\"";
    let start = html.find(tag).expect("report-data script tag present");
    let open = html[start..].find('>').expect("script open") + 1;
    let rest = &html[start + open..];
    let end = rest.find("</script>").expect("script close");
    let blob = rest[..end].trim().replace("<\\/", "</");
    serde_json::from_str(&blob).expect("payload parses as JSON")
}

pub(crate) const ACK_FIXTURE: &str = "../../tests/fixtures/n_plus_one_sql.json";

/// Fixture path expanded against `CARGO_MANIFEST_DIR` (the CLI crate dir).
pub(crate) fn fixture_path(rel: &str) -> String {
    format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel)
}

/// Run `analyze --format json` and parse the output, exiting the test
/// (via panic) on failure with the captured stderr in the message.
pub(crate) fn analyze_json(args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(args)
        .output()
        .expect("spawn perf-sentinel");
    assert!(
        output.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "analyze stdout is not valid JSON: {e}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Run analyze on the fixture, JSON output, no acknowledgments file in
/// play. Read the first finding's signature so each test can wire up
/// matching ack entries without hard-coding the SHA-256 prefix.
pub(crate) fn first_finding_signature() -> String {
    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--no-acknowledgments",
        "--format",
        "json",
    ]);
    v["findings"][0]["signature"]
        .as_str()
        .expect("signature field present in JSON output")
        .to_string()
}

pub(crate) fn write_ack_file(dir: &std::path::Path, signature: &str) -> std::path::PathBuf {
    let path = dir.join(".perf-sentinel-acknowledgments.toml");
    fs::write(
        &path,
        format!(
            "[[acknowledged]]\n\
             signature = \"{signature}\"\n\
             acknowledged_by = \"test@example.com\"\n\
             acknowledged_at = \"2026-05-02\"\n\
             reason = \"smoke test\"\n",
        ),
    )
    .expect("write ack file");
    path
}

pub(crate) const ORG_CONFIG_EXAMPLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/examples/perf-sentinel-org.toml"
);
