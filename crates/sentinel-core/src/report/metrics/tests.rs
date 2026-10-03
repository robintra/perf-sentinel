use super::*;
use crate::detect::{Confidence, Finding, FindingType, GreenImpact, Pattern, Severity};
use crate::report::{Analysis, GreenSummary, QualityGate, Report};

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn make_test_report(findings: Vec<Finding>, waste_ratio: f64) -> Report {
    let total = 10;
    let avoidable = (total as f64 * waste_ratio) as usize;
    Report {
        analysis: Analysis {
            duration_ms: 1,
            events_processed: 100,
            traces_analyzed: 2,
            ingest: None,
        },
        findings,
        green_summary: GreenSummary {
            total_io_ops: total,
            avoidable_io_ops: avoidable,
            io_waste_ratio: waste_ratio,
            io_waste_ratio_band: crate::report::interpret::InterpretationLevel::for_waste_ratio(
                waste_ratio,
            ),
            ..GreenSummary::disabled(0)
        },
        quality_gate: QualityGate {
            passed: true,
            rules: vec![],
        },
        per_endpoint_io_ops: vec![],
        correlations: vec![],
        embedded_traces: vec![],
        warnings: vec![],
        warning_details: vec![],
        acknowledged_findings: vec![],
        binary_version: String::new(),
        detection_config: None,
        disclosure_waste: None,
    }
}

fn make_finding(
    finding_type: FindingType,
    severity: Severity,
    trace_id: &str,
    occurrences: usize,
) -> Finding {
    Finding {
        finding_type,
        severity,
        trace_id: trace_id.to_string(),
        service: "order-svc".to_string(),
        grouping: Vec::new(),
        source_endpoint: "POST /api/orders/42/submit".to_string(),
        pattern: Pattern {
            template: "SELECT * FROM t WHERE id = ?".to_string(),
            occurrences,
            window_ms: 200,
            distinct_params: occurrences,
            ..Default::default()
        },
        suggestion: "batch".to_string(),
        first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
        last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
        green_impact: Some(GreenImpact {
            estimated_extra_io_ops: occurrences.saturating_sub(1),
            io_intensity_score: 6.0,
            io_intensity_band: crate::report::interpret::InterpretationLevel::for_iis(6.0),
        }),
        confidence: Confidence::default(),
        classification_method: None,
        code_location: None,
        instrumentation_scopes: Vec::new(),
        suggested_fix: None,
        signature: String::new(),
    }
}

#[test]
fn default_creates_same_as_new() {
    let state = MetricsState::default();
    // Should work identically to new()
    state.events_processed_total.inc();
    let output = state.render();
    assert!(output.contains("perf_sentinel_events_processed_total"));
}

#[tokio::test]
async fn metrics_route_returns_prometheus_output() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let state = Arc::new(MetricsState::new());
    state.traces_analyzed_total.inc_by(42.0);
    state.io_waste_ratio.set(0.25);
    state
        .findings_total
        .with_label_values(&["n_plus_one_sql", "warning", "order-svc", ""])
        .inc();

    let router = metrics_route(state);

    let request = Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify Prometheus-compliant Content-Type
    let content_type = response
        .headers()
        .get("content-type")
        .expect("should have content-type header")
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("text/plain"),
        "Content-Type should be text/plain, got: {content_type}"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        body_str.contains("perf_sentinel_traces_analyzed_total 42"),
        "should contain traces count, got: {body_str}"
    );
    assert!(
        body_str.contains("perf_sentinel_io_waste_ratio 0.25"),
        "should contain waste ratio"
    );
    assert!(
        body_str.contains("n_plus_one_sql"),
        "should contain finding type label"
    );
}

#[test]
fn metrics_state_creates_successfully() {
    let state = MetricsState::new();
    // Initialize the CounterVec with a label pair so it appears in output
    state
        .findings_total
        .with_label_values(&["test", "test", "test", ""])
        .inc_by(0.0);
    let output = state.render();
    assert!(
        output.contains("perf_sentinel_findings_total"),
        "output: {output}"
    );
    assert!(output.contains("perf_sentinel_io_waste_ratio"));
    assert!(output.contains("perf_sentinel_traces_analyzed_total"));
    assert!(output.contains("perf_sentinel_events_processed_total"));
    assert!(output.contains("perf_sentinel_active_traces"));
    // Trends gauges backing the Grafana energy/carbon and headroom panels.
    assert!(output.contains("perf_sentinel_energy_kwh"));
    assert!(output.contains("perf_sentinel_carbon_gco2"));
    assert!(output.contains("perf_sentinel_max_active_traces"));
    assert!(output.contains("perf_sentinel_analysis_queue_capacity"));
    assert!(output.contains("perf_sentinel_max_retained_findings"));
    assert!(output.contains("perf_sentinel_stored_findings"));
}

#[test]
fn increment_findings_counter() {
    let state = MetricsState::new();
    state
        .findings_total
        .with_label_values(&["n_plus_one_sql", "critical", "order-svc", ""])
        .inc();
    state
        .findings_total
        .with_label_values(&["n_plus_one_sql", "critical", "order-svc", ""])
        .inc();

    let output = state.render();
    assert!(output.contains(r#"type="n_plus_one_sql""#));
    assert!(output.contains(r#"severity="critical""#));
}

#[test]
fn set_gauge_values() {
    let state = MetricsState::new();
    state.io_waste_ratio.set(0.42);
    state.active_traces.set(5.0);

    let output = state.render();
    assert!(output.contains("0.42"));
}

#[test]
fn increment_counters() {
    let state = MetricsState::new();
    state.traces_analyzed_total.inc_by(10.0);
    state.events_processed_total.inc_by(100.0);

    let output = state.render();
    assert!(output.contains("100"));
}

// -- Exemplar tests --

#[test]
fn record_batch_tracks_worst_finding_trace() {
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Critical,
            "trace-abc",
            10,
        )],
        0.5,
    );
    state.record_batch(&report);

    let map = state.worst_finding_trace.read().unwrap();
    assert_eq!(
        map.get(&(
            "n_plus_one_sql",
            "critical",
            "order-svc".to_string(),
            String::new()
        ))
        .unwrap()
        .trace_id,
        "trace-abc"
    );
}

#[test]
fn exemplars_stay_per_service_when_type_and_severity_collide() {
    let state = MetricsState::new();
    let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning, "trace-a", 5);
    f1.service = "svc-a".to_string();
    let mut f2 = make_finding(FindingType::NPlusOneSql, Severity::Warning, "trace-b", 5);
    f2.service = "svc-b".to_string();
    state.record_batch(&make_test_report(vec![f1, f2], 0.5));

    let map = state.worst_finding_trace.read().unwrap();
    let trace_of = |svc: &str| {
        map.get(&("n_plus_one_sql", "warning", svc.to_string(), String::new()))
            .unwrap()
            .trace_id
            .clone()
    };
    assert_eq!(trace_of("svc-a"), "trace-a");
    assert_eq!(trace_of("svc-b"), "trace-b");
}

#[test]
fn exemplars_survive_metacharacters_in_service_names() {
    // service comes from OTLP `service.name`: commas, quotes,
    // braces and backslashes are all legal and must round-trip
    // through the rendered-text parse.
    for service in ["shop,eu", "a\"b", "svc{prod}", "back\\slash"] {
        let state = MetricsState::new();
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning, "trace-x", 5);
        finding.service = service.to_string();
        state.record_batch(&make_test_report(vec![finding], 0.0));

        let output = openmetrics_body(&state);
        let line = output
            .lines()
            .find(|l| l.starts_with("perf_sentinel_findings_total{"))
            .unwrap_or_else(|| panic!("no findings_total line for {service:?}"));
        assert!(
            line.contains("trace_id=\"trace-x\""),
            "exemplar missing for service {service:?}: {line}"
        );
    }
}

#[test]
fn record_batch_tracks_worst_waste_trace() {
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-waste",
            8,
        )],
        0.4,
    );
    state.record_batch(&report);

    let waste = state.worst_waste_trace.read().unwrap();
    assert_eq!(waste.as_ref().unwrap().trace_id, "trace-waste");
}

#[test]
fn stale_exemplars_drop_out_of_the_scrape() {
    let state = MetricsState::new();
    let mut fresh = make_finding(
        FindingType::NPlusOneSql,
        Severity::Critical,
        "trace-fresh",
        10,
    );
    fresh.service = "svc-fresh".to_string();
    let mut stale = make_finding(
        FindingType::NPlusOneSql,
        Severity::Critical,
        "trace-stale",
        5,
    );
    stale.service = "svc-stale".to_string();
    state.record_batch(&make_test_report(vec![fresh, stale], 0.5));

    // Age one of the two. A service that stopped emitting must not
    // keep annotating its series with a trace the tracing backend
    // has already dropped, while its live neighbour still does.
    let age_out = |key: &str| {
        state
            .worst_finding_trace
            .write()
            .unwrap()
            .get_mut(&("n_plus_one_sql", "critical", key.to_string(), String::new()))
            .unwrap()
            .expires_at -= EXEMPLAR_TTL;
    };
    age_out("svc-stale");

    let body = openmetrics_body(&state);
    assert!(
        body.contains("trace-fresh"),
        "live exemplar dropped: {body}"
    );
    assert!(
        !body.contains("trace-stale"),
        "stale exemplar still injected: {body}"
    );

    // Everything aged out: nothing left to annotate at all.
    age_out("svc-fresh");
    if let Some(exemplar) = state.worst_waste_trace.write().unwrap().as_mut() {
        exemplar.expires_at -= EXEMPLAR_TTL;
    }
    assert!(!state.has_exemplars());

    // A live batch prunes the aged entries under the write lock it
    // already holds.
    let mut revived = make_finding(
        FindingType::NPlusOneSql,
        Severity::Critical,
        "trace-new",
        10,
    );
    revived.service = "svc-fresh".to_string();
    state.record_batch(&make_test_report(vec![revived], 0.5));
    assert_eq!(state.worst_finding_trace.read().unwrap().len(), 1);
    assert!(openmetrics_body(&state).contains("trace-new"));
}

/// Body an exemplar-aware scraper receives. Exemplars are opt-in, so
/// every exemplar assertion goes through an explicit `OpenMetrics` Accept.
fn openmetrics_body(state: &MetricsState) -> String {
    state
        .negotiate(Some("application/openmetrics-text;version=1.0.0"))
        .0
}

#[test]
fn render_includes_exemplar_annotation() {
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-exemplar",
            6,
        )],
        0.3,
    );
    state.record_batch(&report);

    let output = openmetrics_body(&state);
    assert!(
        output.contains(r#"# {trace_id="trace-exemplar"}"#),
        "should contain exemplar annotation, got: {output}"
    );
}

#[test]
fn render_no_exemplar_when_no_data() {
    let state = MetricsState::new();
    // Manually set some metrics without using record_batch
    state.traces_analyzed_total.inc();
    state
        .findings_total
        .with_label_values(&["n_plus_one_sql", "warning", "order-svc", ""])
        .inc();

    let output = state.render();
    assert!(
        !output.contains("# {trace_id="),
        "should not contain exemplar when no record_batch called"
    );
}

#[test]
fn exemplar_on_io_waste_ratio() {
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::RedundantSql,
            Severity::Warning,
            "trace-waste-ratio",
            4,
        )],
        0.5,
    );
    state.record_batch(&report);

    let output = openmetrics_body(&state);
    // The io_waste_ratio line should have an exemplar
    for line in output.lines() {
        if line.starts_with("perf_sentinel_io_waste_ratio ") {
            assert!(
                line.contains(r#"# {trace_id="trace-waste-ratio"}"#),
                "waste ratio line should have exemplar: {line}"
            );
        }
    }
}

#[test]
fn content_type_is_openmetrics_with_exemplars() {
    let state = MetricsState::new();
    assert_eq!(
        state.content_type(),
        "text/plain; version=0.0.4; charset=utf-8"
    );

    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-1",
            5,
        )],
        0.0,
    );
    state.record_batch(&report);
    assert_eq!(
        state.content_type(),
        "text/plain; version=0.0.4; charset=utf-8",
        "recorded exemplars must not flip the default content type"
    );
}

#[test]
fn multiple_batches_update_exemplars() {
    let state = MetricsState::new();

    let report1 = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-old",
            5,
        )],
        0.3,
    );
    state.record_batch(&report1);

    let report2 = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-new",
            10,
        )],
        0.5,
    );
    state.record_batch(&report2);

    let map = state.worst_finding_trace.read().unwrap();
    assert_eq!(
        map.get(&(
            "n_plus_one_sql",
            "warning",
            "order-svc".to_string(),
            String::new()
        ))
        .unwrap()
        .trace_id,
        "trace-new",
        "should update to latest batch's worst finding"
    );
}

#[tokio::test]
async fn metrics_route_returns_openmetrics_with_exemplars() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let state = Arc::new(MetricsState::new());
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-route-test",
            5,
        )],
        0.0,
    );
    state.record_batch(&report);

    let router = metrics_route(state);
    let request = Request::builder()
        .uri("/metrics")
        .header("accept", "application/openmetrics-text;version=1.0.0")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("openmetrics"),
        "should use OpenMetrics content type: {content_type}"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body_str.contains(r#"trace_id="trace-route-test""#),
        "should contain exemplar trace_id"
    );
}

// Two-mode Accept negotiation tests. An explicit accepted OpenMetrics
// media type selects OM. Every absent, wildcard or refused preference
// stays on plain Prometheus text.

#[test]
fn negotiate_returns_openmetrics_when_accept_header_explicitly_requests_it() {
    // No exemplars, but an explicit OM request still gets OM 1.0 with `# EOF`.
    let state = MetricsState::new();
    state.traces_analyzed_total.inc();

    let (body, content_type) = state.negotiate(Some("application/openmetrics-text;version=1.0.0"));
    assert_eq!(
        content_type,
        "application/openmetrics-text; version=1.0.0; charset=utf-8"
    );
    assert!(
        body.ends_with("# EOF\n"),
        "OM-forced body must terminate with `# EOF\\n`"
    );
}

#[test]
fn negotiate_returns_openmetrics_with_exemplars_when_accept_explicit_and_exemplars_present() {
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-om-explicit",
            5,
        )],
        0.4,
    );
    state.record_batch(&report);

    let (body, content_type) = state.negotiate(Some("application/openmetrics-text"));
    assert_eq!(
        content_type,
        "application/openmetrics-text; version=1.0.0; charset=utf-8"
    );
    assert!(body.ends_with("# EOF\n"));
    assert!(
        body.contains(r#"# {trace_id="trace-om-explicit"} 1.0"#),
        "OM-forced body must include exemplar annotation: {body}"
    );
}

#[test]
fn negotiate_stays_plain_when_accept_absent() {
    let state = MetricsState::new();
    state.traces_analyzed_total.inc();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-legacy",
            5,
        )],
        0.0,
    );
    state.record_batch(&report);

    let (body, content_type) = state.negotiate(None);
    assert_eq!(content_type, "text/plain; version=0.0.4; charset=utf-8");
    assert!(!body.contains("# EOF"));
    assert!(!body.contains("trace_id="));
}

/// vmagent sends `text/plain;version=0.0.4;*/*;q=0.1` and does not
/// parse exemplars. Serving them anyway makes it read the whole line
/// `perf_sentinel_io_waste_ratio 0.60 # {trace_id="..."} 1.0` as a
/// metric NAME, minting one series per scrape (6k+ dead series in a
/// few hours on a real cluster). A wildcard is not an opt-in.
#[test]
fn wildcard_accept_never_receives_exemplars() {
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-vmagent",
            5,
        )],
        0.5,
    );
    state.record_batch(&report);

    for header in [
        "text/plain;version=0.0.4;*/*;q=0.1",
        "*/*",
        "text/plain;version=0.0.4,*/*;q=0.1",
    ] {
        let (body, content_type) = state.negotiate(Some(header));
        assert_eq!(
            content_type, "text/plain; version=0.0.4; charset=utf-8",
            "{header} must not be served OpenMetrics"
        );
        assert!(!body.contains("# EOF"), "{header}");
        assert!(
            !body.contains("trace_id="),
            "{header} received an exemplar it cannot parse"
        );
        // The unlabeled gauge is the line that corrupts: it must carry
        // its value and nothing else.
        let waste = body
            .lines()
            .find(|l| l.starts_with("perf_sentinel_io_waste_ratio "))
            .expect("gauge must be exposed");
        assert_eq!(
            waste.split_whitespace().count(),
            2,
            "unlabeled gauge line must be `name value`, got: {waste}"
        );
    }
}

#[test]
fn negotiate_returns_plain_strict_when_accept_text_plain_only() {
    // Strict refusal of OM and `*/*` gets plain Prometheus: no exemplars
    // even when present, no `# EOF`. Defends pre-OpenMetrics scrapers.
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-strict",
            5,
        )],
        0.4,
    );
    state.record_batch(&report);

    let (body, content_type) = state.negotiate(Some("text/plain;version=0.0.4"));
    assert_eq!(content_type, "text/plain; version=0.0.4; charset=utf-8");
    assert!(!body.contains("# EOF"));
    assert!(
        !body.contains("# {trace_id="),
        "plain-strict body must not contain exemplar annotations: {body}"
    );
}

#[test]
fn select_format_dispatches_correctly() {
    // No Accept header stays plain.
    assert_eq!(select_format(None), NegotiatedFormat::Plain);
    // Plain strict.
    assert_eq!(select_format(Some("text/plain")), NegotiatedFormat::Plain);
    assert_eq!(
        select_format(Some("text/plain;version=0.0.4")),
        NegotiatedFormat::Plain
    );
    // A wildcard is not an opt-in: vmagent sends the second form and
    // cannot parse the exemplars it would receive.
    assert_eq!(select_format(Some("*/*")), NegotiatedFormat::Plain);
    assert_eq!(
        select_format(Some("text/plain;*/*;q=0.1")),
        NegotiatedFormat::Plain
    );
    assert_eq!(
        select_format(Some("text/plain;version=0.0.4,*/*;q=0.1")),
        NegotiatedFormat::Plain
    );
    // Forced via explicit OM.
    assert_eq!(
        select_format(Some("application/openmetrics-text;version=1.0.0")),
        NegotiatedFormat::OpenMetricsForced
    );
    assert_eq!(
        select_format(Some("application/openmetrics-text;q=0.9,text/plain;q=0.5")),
        NegotiatedFormat::OpenMetricsForced
    );
    // Case-insensitive media-type match.
    assert_eq!(
        select_format(Some("Application/OpenMetrics-Text")),
        NegotiatedFormat::OpenMetricsForced
    );
    // Substring attack: a hostile media type that contains the OM
    // literal as a substring must NOT route to OM.
    assert_eq!(
        select_format(Some("application/openmetrics-text-foo")),
        NegotiatedFormat::Plain
    );
    assert_eq!(
        select_format(Some("xapplication/openmetrics-text")),
        NegotiatedFormat::Plain
    );
    // Explicit refusal via `q=0` per RFC 7231 §5.3.1: the client
    // rejects OM even though the literal is present.
    assert_eq!(
        select_format(Some("application/openmetrics-text;q=0,*/*;q=1")),
        NegotiatedFormat::Plain
    );
    assert_eq!(
        select_format(Some("application/openmetrics-text;q=0.0,text/plain")),
        NegotiatedFormat::Plain
    );
    assert_eq!(
        select_format(Some("application/openmetrics-text;q=0.00,text/plain")),
        NegotiatedFormat::Plain
    );
    assert_eq!(
        select_format(Some("application/openmetrics-text;q=0.000,text/plain")),
        NegotiatedFormat::Plain
    );
}

#[tokio::test]
async fn metrics_route_honors_explicit_openmetrics_accept_header() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let state = Arc::new(MetricsState::new());
    state.traces_analyzed_total.inc();

    let router = metrics_route(state);
    let request = Request::builder()
        .uri("/metrics")
        .header("accept", "application/openmetrics-text;version=1.0.0")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let ct = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.starts_with("application/openmetrics-text"),
        "expected OM CT, got: {ct}"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(
        String::from_utf8(body.to_vec())
            .unwrap()
            .ends_with("# EOF\n")
    );
}

#[tokio::test]
async fn metrics_route_serves_plain_text_to_vmagent_style_accept() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let state = Arc::new(MetricsState::new());
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-vmagent-route",
            5,
        )],
        0.0,
    );
    state.record_batch(&report);

    let router = metrics_route(state);
    let request = Request::builder()
        .uri("/metrics")
        .header("accept", "text/plain;version=0.0.4;*/*;q=0.1")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let ct = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.starts_with("text/plain"),
        "vmagent-style Accept (`*/*`) is not an exemplar opt-in: {ct}"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(!body_str.contains("# EOF"));
    assert!(!body_str.contains("trace_id="));
}

#[test]
fn sanitize_exemplar_value_strips_dangerous_chars() {
    assert_eq!(sanitize_exemplar_value("abc-123_def"), "abc-123_def");
    assert_eq!(
        sanitize_exemplar_value("evil\"} 999\nfake_metric"),
        "evil999fake_metric"
    );
    assert_eq!(sanitize_exemplar_value(""), "");
    // Truncation to 64 chars
    let long = "a".repeat(100);
    assert_eq!(sanitize_exemplar_value(&long).len(), 64);
}

#[test]
fn exemplar_with_malicious_trace_id_is_sanitized() {
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "evil\"} 999\nmy_fake_metric",
            5,
        )],
        0.0,
    );
    state.record_batch(&report);

    let output = openmetrics_body(&state);
    // Should NOT contain the raw malicious string
    assert!(
        !output.contains("evil\""),
        "malicious trace_id should be sanitized"
    );
    // Should contain the sanitized version
    assert!(output.contains("evil999my_fake_metric"));
}

#[test]
fn render_appends_eof_marker_with_exemplars() {
    // OpenMetrics 1.0.0 mandates `# EOF` as the end-of-exposition marker.
    // Strict scrapers (Prometheus in openmetrics-text negotiation)
    // refuse an OpenMetrics payload without it.
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-eof",
            5,
        )],
        0.3,
    );
    state.record_batch(&report);
    let output = openmetrics_body(&state);

    assert!(
        output.ends_with("# EOF\n"),
        "OpenMetrics output must terminate with `# EOF\\n`, got tail: {:?}",
        &output[output.len().saturating_sub(64)..]
    );
    // Catch a regression where someone appends content after the EOF
    // marker. The spec requires EOF to be the last logical record.
    assert!(
        !output.contains("# EOF\n#") && !output.contains("# EOF\nperf_sentinel"),
        "no content may follow the `# EOF` marker"
    );
}

#[test]
fn render_omits_eof_marker_without_exemplars() {
    // Plain Prometheus text format (text/plain; version=0.0.4) must NOT
    // contain `# EOF`, which is illegal in pre-OpenMetrics scrapers.
    let state = MetricsState::new();
    state.traces_analyzed_total.inc();

    let output = state.render();
    assert!(
        !output.contains("# EOF"),
        "Prometheus text/plain output must not contain `# EOF`, got: {output}"
    );
}

#[test]
fn exemplar_annotation_includes_numeric_value() {
    // OpenMetrics 1.0.0 section 5.1.10 requires a numeric value after the
    // exemplar labels block.
    let state = MetricsState::new();
    let report = make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-numeric",
            5,
        )],
        0.5,
    );
    state.record_batch(&report);
    let output = openmetrics_body(&state);

    let exemplar_line = output
        .lines()
        .find(|l| l.starts_with("perf_sentinel_findings_total{") && l.contains("trace_id="))
        .expect("expected at least one findings exemplar line");
    assert!(
        exemplar_line.ends_with(r#" # {trace_id="trace-numeric"} 1.0"#),
        "findings exemplar must follow OpenMetrics 1.0 format: {exemplar_line}"
    );

    let waste_line = output
        .lines()
        .find(|l| l.starts_with("perf_sentinel_io_waste_ratio ") && l.contains("trace_id="))
        .expect("expected the io_waste_ratio exemplar line");
    assert!(
        waste_line.ends_with(r#" # {trace_id="trace-numeric"} 1.0"#),
        "io_waste_ratio exemplar must follow OpenMetrics 1.0 format: {waste_line}"
    );
}

#[test]
fn prometheus_output_format_matches_expected_prefixes() {
    // Regression test: validates that the prometheus crate (0.14.0) output
    // format matches the line prefixes used by inject_exemplars().
    // If this test fails after a prometheus crate upgrade, update inject_exemplars.
    let state = MetricsState::new();
    state
        .findings_total
        .with_label_values(&["n_plus_one_sql", "warning", "order-svc", ""])
        .inc();
    state.io_waste_ratio.set(0.5);

    let output = state.render();

    // Verify the line prefix format that inject_exemplars relies on
    let has_findings_prefix = output
        .lines()
        .any(|l| l.starts_with("perf_sentinel_findings_total{"));
    assert!(
        has_findings_prefix,
        "prometheus output must contain lines starting with 'perf_sentinel_findings_total{{': {output}"
    );

    let has_waste_prefix = output
        .lines()
        .any(|l| l.starts_with("perf_sentinel_io_waste_ratio "));
    assert!(
        has_waste_prefix,
        "prometheus output must contain lines starting with 'perf_sentinel_io_waste_ratio ': {output}"
    );
}

#[test]
fn registry_contains_otlp_rejected() {
    let state = MetricsState::new();
    let output = state.render();
    assert!(
        output.contains("perf_sentinel_otlp_rejected_total"),
        "registry should expose perf_sentinel_otlp_rejected_total, got: {output}"
    );
}

#[test]
fn otlp_rejected_starts_at_zero_for_all_reasons() {
    let state = MetricsState::new();
    // Iterate the exhaustive `ALL` so a new variant is covered
    // without editing this test.
    for reason in OtlpRejectReason::ALL {
        let count = state
            .otlp_rejected_total
            .with_label_values(&[reason.as_str()])
            .get();
        assert_eq!(count, 0, "reason {} should start at 0", reason.as_str());
    }
    let output = state.render();
    for reason in OtlpRejectReason::ALL {
        assert!(
            output.contains(&format!(
                "perf_sentinel_otlp_rejected_total{{reason=\"{}\"}} 0",
                reason.as_str()
            )),
            "pre-warmed line for reason {} should appear in /metrics, got: {output}",
            reason.as_str()
        );
    }
}

#[test]
fn record_otlp_reject_increments_correct_label() {
    let state = MetricsState::new();
    state.record_otlp_reject(OtlpRejectReason::ChannelFull);
    state.record_otlp_reject(OtlpRejectReason::ChannelFull);
    state.record_otlp_reject(OtlpRejectReason::ChannelFull);
    assert_eq!(
        state
            .otlp_rejected_total
            .with_label_values(&["channel_full"])
            .get(),
        3
    );
    assert_eq!(
        state
            .otlp_rejected_total
            .with_label_values(&["parse_error"])
            .get(),
        0
    );
    assert_eq!(
        state
            .otlp_rejected_total
            .with_label_values(&["unsupported_media_type"])
            .get(),
        0
    );
}

#[test]
fn otlp_span_counters_start_at_zero_and_prewarm_all_reasons() {
    let state = MetricsState::new();
    assert_eq!(state.otlp_spans_received_total.get(), 0);
    let output = state.render();
    assert!(
        output.contains("perf_sentinel_otlp_spans_received_total 0"),
        "received counter should render at 0, got: {output}"
    );
    for reason in [
        "not_io",
        "missing_db_statement",
        "missing_http_url",
        "non_sql_datastore",
        "merged_db_span",
    ] {
        assert!(
            output.contains(&format!(
                "perf_sentinel_otlp_spans_filtered_total{{reason=\"{reason}\"}} 0"
            )),
            "pre-warmed line for reason {reason} should appear in /metrics, got: {output}"
        );
    }
}

#[test]
fn otlp_span_filter_reason_as_str_round_trips_all_variants() {
    for (variant, label) in [
        (OtlpSpanFilterReason::NotIo, "not_io"),
        (
            OtlpSpanFilterReason::MissingDbStatement,
            "missing_db_statement",
        ),
        (OtlpSpanFilterReason::MissingHttpUrl, "missing_http_url"),
        (OtlpSpanFilterReason::NonSqlDatastore, "non_sql_datastore"),
        (OtlpSpanFilterReason::MergedDbSpan, "merged_db_span"),
    ] {
        assert_eq!(variant.as_str(), label);
    }
}

#[cfg(feature = "daemon")]
#[test]
fn ack_failure_reason_as_str_round_trips_all_variants() {
    for (variant, label) in [
        (AckFailureReason::AlreadyAcked, "already_acked"),
        (AckFailureReason::NotAcked, "not_acked"),
        (AckFailureReason::Unauthorized, "unauthorized"),
        (AckFailureReason::NoStore, "no_store"),
        (AckFailureReason::InvalidSignature, "invalid_signature"),
        (AckFailureReason::LimitReached, "limit_reached"),
        (AckFailureReason::FileTooLarge, "file_too_large"),
        (AckFailureReason::EntryTooLarge, "entry_too_large"),
        (AckFailureReason::InternalError, "internal_error"),
    ] {
        assert_eq!(variant.as_str(), label);
    }
}

#[test]
fn archive_drops_start_at_zero_for_all_reasons() {
    let state = MetricsState::new();
    // Iterate the exhaustive `ALL` so a new variant is covered
    // without editing this test.
    for reason in ArchiveDropReason::ALL {
        let count = state
            .archive_windows_dropped_total
            .with_label_values(&[reason.as_str()])
            .get();
        assert_eq!(count, 0, "reason {} should start at 0", reason.as_str());
    }
    let output = state.render();
    for reason in ArchiveDropReason::ALL {
        assert!(
            output.contains(&format!(
                "perf_sentinel_archive_windows_dropped_total{{reason=\"{}\"}} 0",
                reason.as_str()
            )),
            "pre-warmed line for reason {} should appear in /metrics, got: {output}",
            reason.as_str()
        );
    }
}

#[cfg(feature = "daemon")]
#[test]
fn record_archive_drop_increments_correct_label() {
    let state = MetricsState::new();
    state.record_archive_drop(ArchiveDropReason::ChannelFull);
    state.record_archive_drop(ArchiveDropReason::ChannelFull);
    state.record_archive_drop(ArchiveDropReason::WriteError);
    for (reason, expected) in [
        (ArchiveDropReason::ChannelFull, 2),
        (ArchiveDropReason::WriterExited, 0),
        (ArchiveDropReason::SerializeError, 0),
        (ArchiveDropReason::WriteError, 1),
    ] {
        assert_eq!(
            state
                .archive_windows_dropped_total
                .with_label_values(&[reason.as_str()])
                .get(),
            expected,
            "unexpected count for {reason:?}"
        );
    }
}

/// The stamped archive value must be the sum over every reason, not
/// one label's count.
#[cfg(feature = "daemon")]
#[test]
fn archive_drops_total_sums_across_reasons() {
    let state = MetricsState::new();
    assert_eq!(state.archive_drops_total(), 0);
    state.record_archive_drop(ArchiveDropReason::ChannelFull);
    state.record_archive_drop(ArchiveDropReason::WriteError);
    state.record_archive_drop(ArchiveDropReason::SerializeError);
    assert_eq!(state.archive_drops_total(), 3);
}

#[cfg(feature = "daemon")]
#[test]
fn record_ack_success_increments_correct_label() {
    let state = MetricsState::new();
    state.record_ack_success(AckAction::Ack);
    state.record_ack_success(AckAction::Ack);
    state.record_ack_success(AckAction::Unack);
    assert_eq!(state.ack_operations_ack_success.get(), 2);
    assert_eq!(state.ack_operations_unack_success.get(), 1);
}

#[cfg(feature = "daemon")]
#[test]
fn incident_rejections_prewarm_all_reasons_and_skip_zero() {
    let state = MetricsState::new();
    let output = state.render();
    for reason in IncidentRejection::ALL {
        let line = format!(
            "perf_sentinel_incidents_rejected_total{{reason=\"{}\"}} 0",
            reason.as_str()
        );
        assert!(output.contains(&line), "missing {line} in: {output}");
    }
    state.record_incident_rejections(IncidentRejection::NoService, 0);
    state.record_incident_rejections(IncidentRejection::NoService, 3);
    assert_eq!(
        state
            .incidents_rejected_total
            .with_label_values(&["no_service"])
            .get(),
        3
    );
}

#[cfg(feature = "daemon")]
#[test]
fn record_ack_failure_increments_correct_combination() {
    let state = MetricsState::new();
    state.record_ack_failure(AckAction::Ack, AckFailureReason::Unauthorized);
    state.record_ack_failure(AckAction::Unack, AckFailureReason::NotAcked);
    assert_eq!(
        state
            .ack_operations_failed_total
            .with_label_values(&["ack", "unauthorized"])
            .get(),
        1
    );
    assert_eq!(
        state
            .ack_operations_failed_total
            .with_label_values(&["unack", "not_acked"])
            .get(),
        1
    );
    assert_eq!(
        state
            .ack_operations_failed_total
            .with_label_values(&["ack", "not_acked"])
            .get(),
        0
    );
}

#[cfg(feature = "daemon")]
#[test]
fn ack_operations_total_starts_at_zero_for_both_actions() {
    let state = MetricsState::new();
    assert_eq!(state.ack_operations_ack_success.get(), 0);
    assert_eq!(state.ack_operations_unack_success.get(), 0);
    let output = state.render();
    for action in ["ack", "unack"] {
        assert!(
            output.contains(&format!(
                "perf_sentinel_ack_operations_total{{action=\"{action}\"}} 0"
            )),
            "pre-warmed line for action={action} should appear, got: {output}"
        );
    }
}

#[cfg(feature = "daemon")]
#[test]
fn ack_operations_failed_total_starts_at_zero_for_documented_combinations() {
    let state = MetricsState::new();
    let output = state.render();
    let documented: &[(&str, &[&str])] = &[
        (
            "ack",
            &[
                "already_acked",
                "unauthorized",
                "no_store",
                "invalid_signature",
                "limit_reached",
                "file_too_large",
                "entry_too_large",
                "internal_error",
            ],
        ),
        (
            "unack",
            &[
                "not_acked",
                "unauthorized",
                "no_store",
                "invalid_signature",
                "internal_error",
            ],
        ),
    ];
    for (action, reasons) in documented {
        for reason in *reasons {
            let line = format!(
                "perf_sentinel_ack_operations_failed_total{{action=\"{action}\",reason=\"{reason}\"}} 0"
            );
            assert!(
                output.contains(&line),
                "pre-warmed line {line} should appear in /metrics"
            );
        }
    }
    // Impossible combinations must not be pre-warmed: scraping for
    // them would mislead operators into building queries on series
    // that can never grow.
    for forbidden in [
        "perf_sentinel_ack_operations_failed_total{action=\"ack\",reason=\"not_acked\"}",
        "perf_sentinel_ack_operations_failed_total{action=\"unack\",reason=\"already_acked\"}",
        "perf_sentinel_ack_operations_failed_total{action=\"unack\",reason=\"limit_reached\"}",
        "perf_sentinel_ack_operations_failed_total{action=\"unack\",reason=\"file_too_large\"}",
        "perf_sentinel_ack_operations_failed_total{action=\"unack\",reason=\"entry_too_large\"}",
    ] {
        assert!(
            !output.contains(forbidden),
            "forbidden combination {forbidden} should not be pre-warmed"
        );
    }
}

#[cfg(feature = "daemon")]
#[test]
fn ack_operations_appear_in_render() {
    let state = MetricsState::new();
    let output = state.render();
    assert!(
        output.contains("perf_sentinel_ack_operations_total"),
        "registry should expose perf_sentinel_ack_operations_total"
    );
    assert!(
        output.contains("perf_sentinel_ack_operations_failed_total"),
        "registry should expose perf_sentinel_ack_operations_failed_total"
    );
}

#[test]
fn energy_backend_configured_distinguishes_absent_from_healthy() {
    // Without this gauge, "not configured" and "configured and
    // healthy" are the same flat zero: every energy gauge is
    // pre-registered at zero, and a successful scrape sets the
    // staleness gauge back to zero too.
    let state = MetricsState::new();
    state
        .energy_backend_configured
        .with_label_values(&["alumet"])
        .set(1.0);
    state
        .energy_backend_configured
        .with_label_values(&["kepler"])
        .set(0.0);
    let output = state.render();
    assert!(
        output.contains("perf_sentinel_energy_backend_configured{backend=\"alumet\"} 1"),
        "a configured backend reads 1, got:\n{output}"
    );
    assert!(
        output.contains("perf_sentinel_energy_backend_configured{backend=\"kepler\"} 0"),
        "an absent backend reads 0, got:\n{output}"
    );
    // The staleness gauge cannot tell the two apart on its own.
    assert!(
        output.contains("perf_sentinel_alumet_last_scrape_age_seconds 0"),
        "the staleness gauge is zero for both states, which is the point"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn process_collector_registered_on_linux() {
    let state = MetricsState::new();
    let output = state.render();
    assert!(
        output
            .lines()
            .any(|l| l.starts_with("process_resident_memory_bytes")),
        "process_resident_memory_bytes should be exposed on Linux, got: {output}"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn process_collector_not_registered_on_non_linux() {
    let state = MetricsState::new();
    let output = state.render();
    assert!(
        !output.contains("process_resident_memory_bytes"),
        "process_resident_memory_bytes must not be exposed off Linux, got: {output}"
    );
}

// --- grouping label (0.19.0) ---

#[test]
fn exemplars_stay_per_grouping_when_type_severity_and_service_collide() {
    let state = MetricsState::new();
    let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning, "trace-a", 5);
    f1.grouping = crate::test_helpers::k8s_grouping("prod");
    let mut f2 = make_finding(FindingType::NPlusOneSql, Severity::Warning, "trace-b", 5);
    f2.grouping = crate::test_helpers::k8s_grouping("staging");
    state.record_batch(&make_test_report(vec![f1, f2], 0.5));

    let map = state.worst_finding_trace.read().unwrap();
    let trace_of = |grouping: &str| {
        map.get(&(
            "n_plus_one_sql",
            "warning",
            "order-svc".to_string(),
            grouping.to_string(),
        ))
        .unwrap()
        .trace_id
        .clone()
    };
    assert_eq!(trace_of("prod"), "trace-a");
    assert_eq!(trace_of("staging"), "trace-b");
}

#[test]
fn exemplars_survive_metacharacters_in_grouping_values() {
    // grouping comes from a resource attribute such as
    // k8s.namespace.name: the same metacharacters are legal as in
    // service.name, and take the same rendered-text round-trip.
    for grouping in ["shop,eu", "a\"b", "ns{prod}", "back\\slash"] {
        let state = MetricsState::new();
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning, "trace-x", 5);
        finding.grouping = crate::test_helpers::k8s_grouping(grouping);
        state.record_batch(&make_test_report(vec![finding], 0.0));

        let output = openmetrics_body(&state);
        let line = output
            .lines()
            .find(|l| l.starts_with("perf_sentinel_findings_total{"))
            .unwrap_or_else(|| panic!("no findings_total line for {grouping:?}"));
        assert!(
            line.contains("trace_id=\"trace-x\""),
            "exemplar missing for grouping {grouping:?}: {line}"
        );
    }
}

#[test]
fn record_batch_renders_the_empty_grouping_when_absent() {
    let state = MetricsState::new();
    state.record_batch(&make_test_report(
        vec![make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "trace-x",
            5,
        )],
        0.0,
    ));

    let output = openmetrics_body(&state);
    let line = output
        .lines()
        .find(|l| l.starts_with("perf_sentinel_findings_total{"))
        .expect("findings_total line");
    // Declared but empty: PromQL reads it as no label, and the
    // exemplar still matches through the parser.
    assert!(line.contains("grouping=\"\""), "{line}");
    assert!(line.contains("trace_id=\"trace-x\""), "{line}");
}

#[test]
fn snapshot_service_io_ops_folds_grouping_into_the_service_total() {
    let state = MetricsState::new();
    let inc = |service: &str, grouping: &str, by: f64| {
        state
            .service_io_ops_total
            .with_label_values(&[service, grouping])
            .inc_by(by);
    };
    inc("svc-a", "prod", 2.0);
    inc("svc-a", "staging", 3.0);
    inc("svc-a", "_other", 1.0);
    inc("svc-b", "", 4.0);

    // The one contract the energy scrapers depend on: a total per
    // service, whatever the grouping split.
    let snapshot = state.snapshot_service_io_ops();
    assert_eq!(snapshot["svc-a"], 6);
    assert_eq!(snapshot["svc-b"], 4);
    assert_eq!(snapshot.len(), 2);
}

#[test]
fn counter_value_as_u64_saturates_at_both_ends() {
    assert_eq!(counter_value_as_u64(-1.0), 0);
    assert_eq!(counter_value_as_u64(0.0), 0);
    assert_eq!(counter_value_as_u64(7.9), 7);
    assert_eq!(counter_value_as_u64(f64::MAX), u64::MAX);
}

#[test]
fn service_label_reads_the_service_label_only() {
    use prometheus::core::Collector;
    let labeled =
        CounterVec::new(Opts::new("t_service", "help"), &["grouping", "service"]).unwrap();
    labeled.with_label_values(&["prod", "svc-a"]).inc();
    let families = Collector::collect(&labeled);
    assert_eq!(service_label(&families[0].get_metric()[0]), Some("svc-a"));

    let unlabeled = CounterVec::new(Opts::new("t_grouping", "help"), &["grouping"]).unwrap();
    unlabeled.with_label_values(&["prod"]).inc();
    let families = Collector::collect(&unlabeled);
    assert_eq!(service_label(&families[0].get_metric()[0]), None);
}
