//! End-to-end tests for perf-sentinel pipeline stages.

use sentinel_core::config::Config;
use sentinel_core::correlate;
use sentinel_core::detect::FindingType;
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
    assert!(report.findings.is_empty());
    assert_eq!(report.analysis.events_processed, 0);
    assert_eq!(report.analysis.traces_analyzed, 0);
}

#[test]
fn n_plus_one_sql_fixture_normalizes_to_same_template() {
    let events = load_fixture("n_plus_one_sql.json");
    assert_eq!(events.len(), 6);

    let normalized = normalize::normalize_all(events);
    let templates: Vec<&str> = normalized.iter().map(|n| n.template.as_str()).collect();
    assert!(
        templates.iter().all(|t| *t == templates[0]),
        "expected all templates to be the same, got: {templates:?}"
    );
    assert_eq!(templates[0], "SELECT * FROM player WHERE game_id = ?");

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

// --- Detection-level integration tests ---

#[test]
fn n_plus_one_sql_detected() {
    let config = Config::default();
    let events = load_fixture("n_plus_one_sql.json");
    let report = pipeline::analyze(events, &config);

    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].finding_type, FindingType::NPlusOneSql);
    assert_eq!(report.findings[0].pattern.occurrences, 6);
    assert_eq!(report.findings[0].pattern.distinct_params, 6);
    assert_eq!(report.findings[0].trace_id, "trace-n1-sql");
    assert_eq!(report.findings[0].service, "game");
}

#[test]
fn n_plus_one_http_detected() {
    let config = Config::default();
    let events = load_fixture("n_plus_one_http.json");
    let report = pipeline::analyze(events, &config);

    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].finding_type, FindingType::NPlusOneHttp);
    assert_eq!(report.findings[0].pattern.occurrences, 6);
    assert_eq!(report.findings[0].trace_id, "trace-n1-http");
}

#[test]
fn clean_traces_no_findings() {
    let config = Config::default();
    let events = load_fixture("clean_traces.json");
    let report = pipeline::analyze(events, &config);

    assert!(
        report.findings.is_empty(),
        "expected no findings for clean traces, got: {:?}",
        report.findings
    );
}

#[test]
fn mixed_fixture_detects_all_patterns() {
    let config = Config::default();
    let events = load_fixture("mixed.json");
    let report = pipeline::analyze(events, &config);

    // Should detect: N+1 SQL, N+1 HTTP, redundant SQL
    let n1_sql = report
        .findings
        .iter()
        .filter(|f| f.finding_type == FindingType::NPlusOneSql)
        .count();
    let n1_http = report
        .findings
        .iter()
        .filter(|f| f.finding_type == FindingType::NPlusOneHttp)
        .count();
    let redundant_sql = report
        .findings
        .iter()
        .filter(|f| f.finding_type == FindingType::RedundantSql)
        .count();

    assert_eq!(n1_sql, 1, "expected 1 N+1 SQL finding");
    assert_eq!(n1_http, 1, "expected 1 N+1 HTTP finding");
    assert_eq!(redundant_sql, 1, "expected 1 redundant SQL finding");

    // Green summary should reflect avoidable ops
    assert!(report.green_summary.avoidable_io_ops > 0);
    assert!(report.green_summary.io_waste_ratio > 0.0);
}

#[test]
fn full_pipeline_runs_on_all_fixtures() {
    let config = Config::default();
    for fixture in [
        "n_plus_one_sql.json",
        "n_plus_one_http.json",
        "clean_traces.json",
        "mixed.json",
    ] {
        let events = load_fixture(fixture);
        let report = pipeline::analyze(events, &config);
        // Verify report structure is valid
        assert!(report.analysis.events_processed > 0, "fixture: {fixture}");
        // Quality gate rules are always populated
        assert_eq!(report.quality_gate.rules.len(), 3, "fixture: {fixture}");
    }
}

#[test]
fn clean_fixture_passes_quality_gate() {
    let config = Config::default();
    let events = load_fixture("clean_traces.json");
    let report = pipeline::analyze(events, &config);
    assert!(report.quality_gate.passed);
}

#[test]
fn n_plus_one_fixture_fails_quality_gate() {
    let config = Config::default();
    let events = load_fixture("n_plus_one_sql.json");
    let report = pipeline::analyze(events, &config);
    // Waste ratio 5/6 > 0.30 default threshold
    assert!(!report.quality_gate.passed);
}
