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
    assert_eq!(templates[0], "SELECT * FROM order_item WHERE order_id = ?");

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
    assert_eq!(templates[0], "GET /api/users/{id}");
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
    assert_eq!(report.findings[0].service, "order-svc");
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

#[test]
fn slow_sql_detected_in_fixture() {
    let config = Config::default();
    let events = load_fixture("slow_queries.json");
    let report = pipeline::analyze(events, &config);

    let slow_sql: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.finding_type == FindingType::SlowSql)
        .collect();
    assert_eq!(slow_sql.len(), 1, "expected 1 slow SQL finding");
    assert_eq!(slow_sql[0].pattern.occurrences, 3);
    // Max duration is 2600ms > 5x threshold (2500ms) -> Critical
    assert_eq!(
        slow_sql[0].severity,
        sentinel_core::detect::Severity::Critical
    );
    assert!(slow_sql[0].suggestion.contains("index"));
}

#[test]
fn slow_http_detected_in_fixture() {
    let config = Config::default();
    let events = load_fixture("slow_queries.json");
    let report = pipeline::analyze(events, &config);

    let slow_http: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.finding_type == FindingType::SlowHttp)
        .collect();
    assert_eq!(slow_http.len(), 1, "expected 1 slow HTTP finding");
    assert_eq!(slow_http[0].pattern.occurrences, 3);
    assert!(slow_http[0].suggestion.contains("caching"));
}

#[test]
fn slow_finding_has_timestamps_and_green_impact() {
    let config = Config::default();
    let events = load_fixture("slow_queries.json");
    let report = pipeline::analyze(events, &config);

    for finding in &report.findings {
        assert!(
            !finding.first_timestamp.is_empty(),
            "finding should have first_timestamp"
        );
        assert!(
            !finding.last_timestamp.is_empty(),
            "finding should have last_timestamp"
        );
        assert!(
            finding.green_impact.is_some(),
            "finding should have green_impact after scoring"
        );
    }
}

#[test]
fn pipeline_with_region_includes_co2_in_report() {
    let config = Config {
        green_default_region: Some("eu-west-3".to_string()),
        ..Config::default()
    };
    let events = load_fixture("n_plus_one_sql.json");
    let report = pipeline::analyze(events, &config);

    let co2 = report
        .green_summary
        .co2
        .as_ref()
        .expect("co2 should be Some when default_region is configured");
    assert!(co2.total.mid > 0.0, "total co2 should be positive");
    assert!(
        co2.avoidable.mid > 0.0,
        "avoidable co2 should be positive when there are findings"
    );

    for offender in &report.green_summary.top_offenders {
        assert!(
            offender.co2_grams.is_some(),
            "top offender should have co2_grams in mono-region mode"
        );
    }
}

#[test]
fn pipeline_without_region_emits_only_embodied_floor() {
    // with green enabled (default) and no region configured,
    // operational CO₂ is 0 (events fall into the "unknown" bucket) but
    // embodied CO₂ is still emitted as a floor estimate.
    let config = Config::default();
    let events = load_fixture("n_plus_one_sql.json");
    let report = pipeline::analyze(events, &config);

    let co2 = report
        .green_summary
        .co2
        .as_ref()
        .expect("co2 should be Some when green is enabled");
    assert!((co2.operational_gco2 - 0.0).abs() < f64::EPSILON);
    assert!(co2.embodied_gco2 > 0.0, "embodied is region-independent");
    assert!(co2.total.mid > 0.0);
    // Per-offender scalar uses default_region; without one set it stays None.
    for offender in &report.green_summary.top_offenders {
        assert!(offender.co2_grams.is_none());
    }
    // Unknown region bucket present in the breakdown.
    assert!(
        report
            .green_summary
            .regions
            .iter()
            .any(|r| r.region == "unknown")
    );
}

#[test]
fn pipeline_unknown_region_emits_zero_operational() {
    // a region not in the embedded carbon table (e.g. "mars-1")
    // contributes 0 operational CO₂. Embodied is still emitted.
    let config = Config {
        green_default_region: Some("mars-1".to_string()),
        ..Config::default()
    };
    let events = load_fixture("n_plus_one_sql.json");
    let report = pipeline::analyze(events, &config);

    let co2 = report
        .green_summary
        .co2
        .as_ref()
        .expect("co2 should be Some when green is enabled");
    assert!((co2.operational_gco2 - 0.0).abs() < f64::EPSILON);
    assert!(co2.embodied_gco2 > 0.0);
    // mars-1 row exists in the breakdown with the user's name.
    assert!(
        report
            .green_summary
            .regions
            .iter()
            .any(|r| r.region == "mars-1")
    );
}

#[test]
fn slow_and_n_plus_one_coexist_in_mixed_fixture() {
    let config = Config::default();
    let events = load_fixture("mixed.json");
    // mixed.json has N+1 SQL, N+1 HTTP, redundant SQL, no slow queries (durations are low)
    let report = pipeline::analyze(events, &config);

    let slow_count = report
        .findings
        .iter()
        .filter(|f| {
            f.finding_type == FindingType::SlowSql || f.finding_type == FindingType::SlowHttp
        })
        .count();
    // mixed.json events have duration_us 800-14000, below 500ms threshold
    assert_eq!(slow_count, 0, "mixed.json should have no slow findings");

    // But N+1 and redundant should still be detected
    assert!(
        report.findings.len() >= 3,
        "mixed.json should have N+1 SQL, N+1 HTTP, and redundant SQL"
    );
}

#[test]
fn full_pipeline_runs_on_slow_fixture() {
    let config = Config::default();
    let events = load_fixture("slow_queries.json");
    let report = pipeline::analyze(events, &config);

    assert_eq!(report.analysis.events_processed, 7);
    assert_eq!(report.analysis.traces_analyzed, 1);
    assert_eq!(report.quality_gate.rules.len(), 3);
    assert!(report.green_summary.total_io_ops > 0);
}

#[test]
fn co2_serializes_correctly_in_json() {
    let config = Config {
        green_default_region: Some("eu-west-3".to_string()),
        ..Config::default()
    };
    let events = load_fixture("n_plus_one_sql.json");
    let report = pipeline::analyze(events, &config);

    let json = serde_json::to_string(&report).unwrap();
    // Structured co2 object with methodology tags must appear in the report.
    assert!(json.contains("\"co2\""));
    assert!(json.contains("\"sci_v1_numerator\""));
    assert!(json.contains("\"sci_v1_operational_ratio\""));
    assert!(json.contains("\"methodology\""));
}

#[test]
fn co2_absent_from_json_when_green_disabled() {
    // With green disabled, no co2/regions are serialized.
    let config = Config {
        green_enabled: false,
        ..Config::default()
    };
    let events = load_fixture("clean_traces.json");
    let report = pipeline::analyze(events, &config);

    let json = serde_json::to_string(&report).unwrap();
    assert!(
        !json.contains("\"co2\""),
        "JSON should omit co2 object when green disabled"
    );
    assert!(
        !json.contains("\"regions\""),
        "JSON should omit regions array when green disabled"
    );
}

// --- Jaeger/Zipkin auto-detection tests ---

#[test]
fn jaeger_fixture_auto_detected_and_analyzed() {
    let config = Config::default();
    let events = load_fixture("jaeger_export.json");
    assert!(!events.is_empty(), "Jaeger fixture should produce events");
    let report = pipeline::analyze(events, &config);
    let n1 = report
        .findings
        .iter()
        .filter(|f| f.finding_type == FindingType::NPlusOneSql)
        .count();
    assert_eq!(n1, 1, "Jaeger fixture should detect N+1 SQL");
}

#[test]
fn zipkin_fixture_auto_detected_and_analyzed() {
    let config = Config::default();
    let events = load_fixture("zipkin_export.json");
    assert!(!events.is_empty(), "Zipkin fixture should produce events");
    let report = pipeline::analyze(events, &config);
    let n1 = report
        .findings
        .iter()
        .filter(|f| f.finding_type == FindingType::NPlusOneSql)
        .count();
    assert_eq!(n1, 1, "Zipkin fixture should detect N+1 SQL");
}

#[test]
fn fanout_fixture_detects_excessive_fanout() {
    let config = Config::default();
    let events = load_fixture("fanout.json");
    let report = pipeline::analyze(events, &config);
    let fanout = report
        .findings
        .iter()
        .filter(|f| f.finding_type == FindingType::ExcessiveFanout)
        .count();
    assert_eq!(fanout, 1, "fanout fixture should detect excessive fanout");
}

#[test]
fn explain_tree_from_n_plus_one_fixture() {
    let events = load_fixture("n_plus_one_sql.json");
    let normalized = normalize::normalize_all(events);
    let traces = correlate::correlate(normalized);
    let trace = &traces[0];

    let detect_config = sentinel_core::detect::DetectConfig {
        n_plus_one_threshold: 5,
        window_ms: 500,
        slow_threshold_ms: 500,
        slow_min_occurrences: 3,
        max_fanout: 20,
    };
    let findings = sentinel_core::detect::detect(std::slice::from_ref(trace), &detect_config);
    let tree = sentinel_core::explain::build_tree(trace, &findings);

    assert_eq!(tree.trace_id, "trace-n1-sql");
    assert!(!tree.roots.is_empty());

    let text = sentinel_core::explain::format_tree_text(&tree, false);
    assert!(text.contains("trace-n1-sql"));

    let json = sentinel_core::explain::format_tree_json(&tree).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["trace_id"], "trace-n1-sql");
}

#[test]
fn full_pipeline_runs_on_new_fixtures() {
    let config = Config::default();
    for fixture in ["jaeger_export.json", "zipkin_export.json", "fanout.json"] {
        let events = load_fixture(fixture);
        let report = pipeline::analyze(events, &config);
        assert!(report.analysis.events_processed > 0, "fixture: {fixture}");
    }
}

#[test]
fn pg_stat_csv_fixture_parses_successfully() {
    let path = format!(
        "{}/../../tests/fixtures/pg_stat_statements.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    let entries =
        sentinel_core::ingest::pg_stat::parse_pg_stat(&raw, 1_048_576).expect("CSV parse failed");
    assert_eq!(entries.len(), 15, "CSV fixture should have 15 entries");
    // Verify normalization was applied
    assert!(
        entries[0].normalized_template.contains('?'),
        "first entry should have normalized template"
    );
}

#[test]
fn pg_stat_json_fixture_parses_successfully() {
    let path = format!(
        "{}/../../tests/fixtures/pg_stat_statements.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    let entries =
        sentinel_core::ingest::pg_stat::parse_pg_stat(&raw, 1_048_576).expect("JSON parse failed");
    assert_eq!(entries.len(), 15, "JSON fixture should have 15 entries");
}

#[test]
fn pg_stat_csv_and_json_fixtures_produce_same_entries() {
    let csv_path = format!(
        "{}/../../tests/fixtures/pg_stat_statements.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let json_path = format!(
        "{}/../../tests/fixtures/pg_stat_statements.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let csv_raw = std::fs::read(&csv_path).unwrap();
    let json_raw = std::fs::read(&json_path).unwrap();
    let csv_entries = sentinel_core::ingest::pg_stat::parse_pg_stat(&csv_raw, 1_048_576).unwrap();
    let json_entries = sentinel_core::ingest::pg_stat::parse_pg_stat(&json_raw, 1_048_576).unwrap();
    assert_eq!(csv_entries.len(), json_entries.len());
    // Verify same normalized templates in same order
    for (csv, json) in csv_entries.iter().zip(json_entries.iter()) {
        assert_eq!(csv.normalized_template, json.normalized_template);
        assert_eq!(csv.calls, json.calls);
    }
}
