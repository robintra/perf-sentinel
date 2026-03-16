//! End-to-end tests for perf-sentinel pipeline stages.

use sentinel_core::config::Config;
use sentinel_core::correlate;
use sentinel_core::event::SpanEvent;
use sentinel_core::ingest::IngestSource;
use sentinel_core::ingest::json::JsonIngest;
use sentinel_core::normalize;
use sentinel_core::pipeline;

fn load_fixture(name: &str) -> Vec<SpanEvent> {
    let path = format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    let ingest = JsonIngest::new(10_000_000);
    ingest.ingest(&data).unwrap()
}

#[test]
fn empty_input_produces_empty_report() {
    let config = Config::default();
    let report = pipeline::analyze(vec![], &config);
    assert!(report.detections.is_empty());
    assert!(report.scores.is_empty());
}

#[test]
fn n_plus_one_sql_fixture_normalizes_to_same_template() {
    let events = load_fixture("n_plus_one_sql.json");
    assert_eq!(events.len(), 6);

    let normalized = normalize::normalize_all(events);
    // All 6 events should have the same normalized template
    let templates: Vec<&str> = normalized.iter().map(|n| n.template.as_str()).collect();
    assert!(
        templates.iter().all(|t| *t == templates[0]),
        "expected all templates to be the same, got: {templates:?}"
    );
    assert_eq!(templates[0], "SELECT * FROM player WHERE game_id = ?");

    // Each should have a different param
    let params: Vec<&str> = normalized.iter().map(|n| n.params[0].as_str()).collect();
    assert_eq!(params, vec!["1", "2", "3", "4", "5", "6"]);
}

#[test]
fn n_plus_one_http_fixture_normalizes_to_same_template() {
    let events = load_fixture("n_plus_one_http.json");
    assert_eq!(events.len(), 6);

    let normalized = normalize::normalize_all(events);
    let templates: Vec<&str> = normalized.iter().map(|n| n.template.as_str()).collect();
    assert!(
        templates.iter().all(|t| *t == templates[0]),
        "expected all templates to be the same, got: {templates:?}"
    );
    assert_eq!(templates[0], "GET /api/account/{id}");
}

#[test]
fn clean_traces_fixture_has_diverse_templates() {
    let events = load_fixture("clean_traces.json");
    assert_eq!(events.len(), 4);

    let normalized = normalize::normalize_all(events);
    let templates: Vec<&str> = normalized.iter().map(|n| n.template.as_str()).collect();
    // All 4 should be different templates
    let unique: std::collections::HashSet<&&str> = templates.iter().collect();
    assert_eq!(
        unique.len(),
        4,
        "expected 4 unique templates, got: {templates:?}"
    );
}

#[test]
fn n_plus_one_sql_fixture_correlates_to_single_trace() {
    let events = load_fixture("n_plus_one_sql.json");
    let normalized = normalize::normalize_all(events);
    let traces = correlate::correlate(normalized);
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].trace_id, "trace-n1-sql");
    assert_eq!(traces[0].spans.len(), 6);
}

#[test]
fn clean_traces_fixture_correlates_to_two_traces() {
    let events = load_fixture("clean_traces.json");
    let normalized = normalize::normalize_all(events);
    let traces = correlate::correlate(normalized);
    assert_eq!(traces.len(), 2);
}

#[test]
fn full_pipeline_runs_on_fixtures() {
    let config = Config::default();
    for fixture in [
        "n_plus_one_sql.json",
        "n_plus_one_http.json",
        "clean_traces.json",
    ] {
        let events = load_fixture(fixture);
        let report = pipeline::analyze(events, &config);
        // Detection is still a stub, so no findings yet
        assert!(report.detections.is_empty(), "fixture: {fixture}");
    }
}
