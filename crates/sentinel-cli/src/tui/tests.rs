use super::*;
use core::assert_matches;
use sentinel_core::detect::{Confidence, GreenImpact, Pattern};

fn make_test_app() -> App {
    let findings = vec![
        Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Critical,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 200,
                distinct_params: 6,
                ..Default::default()
            },
            suggestion: "Use WHERE ... IN (?)".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
            green_impact: Some(GreenImpact {
                estimated_extra_io_ops: 5,
                io_intensity_score: 6.0,
                io_intensity_band: InterpretationLevel::for_iis(6.0),
            }),
            confidence: Confidence::default(),
            classification_method: None,
            code_location: None,
            instrumentation_scopes: Vec::new(),
            suggested_fix: None,
            signature: String::new(),
        },
        Finding {
            finding_type: FindingType::RedundantSql,
            severity: Severity::Warning,
            trace_id: "trace-2".to_string(),
            service: "user-svc".to_string(),
            source_endpoint: "GET /api/users/123".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM config WHERE key = ?".to_string(),
                occurrences: 3,
                window_ms: 100,
                distinct_params: 1,
                ..Default::default()
            },
            suggestion: "Cache result".to_string(),
            first_timestamp: "2025-07-10T14:32:02.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:02.100Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
            classification_method: None,
            code_location: None,
            instrumentation_scopes: Vec::new(),
            suggested_fix: None,
            signature: String::new(),
        },
    ];

    let detect_config = DetectConfig {
        n_plus_one_threshold: 5,
        window_ms: 500,
        slow_threshold_ms: 500,
        slow_min_occurrences: 3,
        max_fanout: 20,
        chatty_service_min_calls: 15,
        pool_saturation_concurrent_threshold: 10,
        serialized_min_sequential: 3,
        sanitizer_aware_classification:
            sentinel_core::detect::sanitizer_aware::SanitizerAwareMode::default(),
    };

    let traces = vec![
        Trace {
            trace_id: "trace-1".to_string(),
            spans: vec![],
        },
        Trace {
            trace_id: "trace-2".to_string(),
            spans: vec![],
        },
    ];

    App::new(findings, traces, detect_config)
}

/// A 100x20 Inspect area at the origin: vertical border at row 10
/// (`rows = [50,50]`), top row spans rows 0..10, column borders at
/// x=20 and x=50 (`cols = [20,30,50]`).
fn app_with_inspect_area() -> App {
    let app = make_test_app();
    app.inspect_area.set(Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 20,
    });
    app
}

#[test]
fn resize_drag_vertical_changes_rows_from_the_row_coord() {
    let mut app = app_with_inspect_area();
    // Click the vertical border (row 10) below the top row.
    app.begin_drag(50, 10);
    assert_eq!(
        app.drag,
        Some(DragTarget {
            axis: Axis::Vertical,
            boundary: 0,
        })
    );
    // Drag to row 15 -> top grows to 75%. Columns untouched.
    app.apply_drag(50, 15);
    assert_eq!(app.inspect_rows, [75, 25]);
    assert_eq!(app.inspect_cols, INSPECT_COLS_DEFAULT);
}

#[test]
fn resize_drag_horizontal_changes_cols_from_the_col_coord() {
    let mut app = app_with_inspect_area();
    // Click the first column border (x=20) inside the top row.
    app.begin_drag(20, 5);
    assert_eq!(
        app.drag,
        Some(DragTarget {
            axis: Axis::Horizontal,
            boundary: 0,
        })
    );
    // Drag to x=30 -> Traces grows to 30%, Findings shrinks. Rows untouched.
    app.apply_drag(30, 5);
    assert_eq!(app.inspect_cols, [30, 20, 50]);
    assert_eq!(app.inspect_rows, INSPECT_ROWS_DEFAULT);
}

#[test]
fn begin_drag_prefers_horizontal_on_top_rows_bottom_cell() {
    // Regression: the vertical border's +/-1 tolerance must not shadow
    // a column border on the top row's bottom cell (row 9, near vy=10).
    let mut app = app_with_inspect_area();
    app.begin_drag(20, 9);
    assert_eq!(
        app.drag,
        Some(DragTarget {
            axis: Axis::Horizontal,
            boundary: 0,
        })
    );
}

#[test]
fn toggle_mouse_mode_flips_and_clears_drag() {
    let mut app = app_with_inspect_area();
    app.begin_drag(20, 5);
    assert!(app.drag.is_some());
    app.toggle_mouse_mode();
    assert!(app.mouse_mode);
    app.toggle_mouse_mode();
    assert!(!app.mouse_mode);
    assert!(
        app.drag.is_none(),
        "turning mouse mode off cancels the drag"
    );
}

#[test]
fn reset_layout_restores_defaults() {
    let mut app = app_with_inspect_area();
    app.begin_drag(20, 5);
    app.apply_drag(35, 5);
    assert_ne!(app.inspect_cols, INSPECT_COLS_DEFAULT);
    app.reset_layout();
    assert_eq!(app.inspect_rows, INSPECT_ROWS_DEFAULT);
    assert_eq!(app.inspect_cols, INSPECT_COLS_DEFAULT);
}

fn moved(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Moved,
        column,
        row,
        modifiers: event::KeyModifiers::empty(),
    }
}

#[test]
fn hover_tracks_border_under_cursor() {
    let mut app = app_with_inspect_area();
    app.mouse_mode = true;
    // Over the first column border (x=20) in the top row.
    handle_mouse(&mut app, moved(20, 5));
    assert_eq!(
        app.resize_target(),
        Some(DragTarget {
            axis: Axis::Horizontal,
            boundary: 0,
        })
    );
    // Off any border: nothing to highlight.
    handle_mouse(&mut app, moved(100, 5));
    assert_eq!(app.resize_target(), None);
}

#[test]
fn accepts_panel_drag_only_in_inspect_view() {
    let mut app = make_test_app();
    app.view = View::Inspect;
    assert!(app.accepts_panel_drag());
    app.view = View::Explain;
    assert!(!app.accepts_panel_drag());
    app.view = View::Analyze;
    assert!(!app.accepts_panel_drag());
}

#[test]
fn app_initial_state() {
    let app = make_test_app();
    assert_eq!(app.trace_count(), 2);
    assert_eq!(app.selected_trace, 0);
    assert_eq!(app.selected_finding, 0);
    assert_eq!(app.active_panel, Panel::Traces);
}

#[test]
fn move_down_traces() {
    let mut app = make_test_app();
    app.move_down();
    assert_eq!(app.selected_trace, 1);
    // Past the end should not go further
    app.move_down();
    assert_eq!(app.selected_trace, 1);
}

#[test]
fn move_up_traces() {
    let mut app = make_test_app();
    // At 0, should stay at 0
    app.move_up();
    assert_eq!(app.selected_trace, 0);
    app.move_down();
    app.move_up();
    assert_eq!(app.selected_trace, 0);
}

#[test]
fn next_panel_cycles() {
    let mut app = make_test_app();
    assert_eq!(app.active_panel, Panel::Traces);
    app.next_panel();
    assert_eq!(app.active_panel, Panel::Findings);
    app.next_panel();
    assert_eq!(app.active_panel, Panel::Detail);
    app.next_panel();
    assert_eq!(app.active_panel, Panel::Correlations);
    app.next_panel();
    assert_eq!(app.active_panel, Panel::Traces);
}

#[test]
fn prev_panel_cycles() {
    let mut app = make_test_app();
    app.prev_panel();
    assert_eq!(app.active_panel, Panel::Correlations);
    app.prev_panel();
    assert_eq!(app.active_panel, Panel::Detail);
    app.prev_panel();
    assert_eq!(app.active_panel, Panel::Findings);
}

#[test]
fn enter_drills_into_findings() {
    let mut app = make_test_app();
    app.enter();
    assert_eq!(app.active_panel, Panel::Findings);
    app.enter();
    assert_eq!(app.active_panel, Panel::Detail);
}

#[test]
fn escape_goes_back() {
    let mut app = make_test_app();
    app.active_panel = Panel::Detail;
    app.escape();
    assert_eq!(app.active_panel, Panel::Findings);
    app.escape();
    assert_eq!(app.active_panel, Panel::Traces);
    // At Traces (top of the inspect drill-down), escape ascends to the
    // Analyze view; the active panel stays Traces so descending lands
    // back here.
    assert_eq!(app.view, View::Inspect);
    app.escape();
    assert_eq!(app.active_panel, Panel::Traces);
    assert_eq!(app.view, View::Analyze);
}

#[test]
fn escape_from_correlations_ascends_to_analyze() {
    // Correlations is a top-level panel (Tab-reachable); Esc must ascend
    // to Analyze like Traces, honoring the tab-bar "Esc up" hint rather
    // than being a dead end.
    let mut app = make_test_app();
    app.active_panel = Panel::Correlations;
    app.escape();
    assert_eq!(app.view, View::Analyze);
    assert_eq!(app.active_panel, Panel::Correlations);
}

#[test]
fn analyze_enter_descends_to_inspect_traces() {
    let mut app = make_test_app();
    app.view = View::Analyze;
    let out = dispatch_analyze_key(&mut app, KeyCode::Enter);
    assert_matches!(out, KeyOutcome::Continue);
    assert_eq!(app.view, View::Inspect);
    assert_eq!(app.active_panel, Panel::Traces);
}

#[test]
fn detail_enter_zooms_to_explain() {
    let mut app = make_test_app();
    app.active_panel = Panel::Detail;
    app.enter();
    assert_eq!(app.view, View::Explain);
}

#[test]
fn explain_escape_returns_to_inspect_detail() {
    let mut app = make_test_app();
    app.view = View::Explain;
    let out = dispatch_explain_key(&mut app, KeyCode::Esc);
    assert_matches!(out, KeyOutcome::Continue);
    assert_eq!(app.view, View::Inspect);
    assert_eq!(app.active_panel, Panel::Detail);
}

#[test]
fn full_drilldown_round_trip() {
    // Analyze -> Inspect/Traces -> Findings -> Detail -> Explain, then
    // all the way back up the same path.
    let mut app = make_test_app();
    app.view = View::Analyze;

    dispatch_analyze_key(&mut app, KeyCode::Enter);
    assert_eq!((app.view, app.active_panel), (View::Inspect, Panel::Traces));
    app.enter(); // trace-1 has a finding -> Findings
    assert_eq!(app.active_panel, Panel::Findings);
    app.enter();
    assert_eq!(app.active_panel, Panel::Detail);
    app.enter();
    assert_eq!(app.view, View::Explain);

    dispatch_explain_key(&mut app, KeyCode::Esc);
    assert_eq!((app.view, app.active_panel), (View::Inspect, Panel::Detail));
    app.escape(); // Detail -> origin (Findings)
    assert_eq!(app.active_panel, Panel::Findings);
    app.escape(); // Findings -> Traces
    assert_eq!(app.active_panel, Panel::Traces);
    app.escape(); // Traces -> Analyze
    assert_eq!(app.view, View::Analyze);
}

#[test]
fn hjkl_parity_with_arrows_for_panels() {
    let mut app = make_test_app();
    assert_eq!(app.active_panel, Panel::Traces);
    dispatch_panel_key(&mut app, KeyCode::Char('l'));
    assert_eq!(app.active_panel, Panel::Findings);
    dispatch_panel_key(&mut app, KeyCode::Char('h'));
    assert_eq!(app.active_panel, Panel::Traces);
    // Arrows behave identically.
    dispatch_panel_key(&mut app, KeyCode::Right);
    assert_eq!(app.active_panel, Panel::Findings);
    dispatch_panel_key(&mut app, KeyCode::Left);
    assert_eq!(app.active_panel, Panel::Traces);
}

#[test]
fn initial_view_and_focus_trace() {
    let app = make_test_app()
        .with_initial_view(View::Explain)
        .with_focus_trace("trace-2");
    assert_eq!(app.view, View::Explain);
    assert_eq!(app.trace_ids[app.selected_trace], "trace-2");
}

#[test]
fn analyze_view_without_summary_shows_hint() {
    let app = make_test_app();
    let text = line_text(&app.build_analyze_lines());
    assert!(text.contains("unavailable"), "got: {text}");
}

#[test]
fn analyze_view_renders_gate_and_offenders() {
    let green_summary: GreenSummary = serde_json::from_str(
        r#"{"total_io_ops":100,"avoidable_io_ops":42,"io_waste_ratio":0.42,"io_waste_ratio_band":"high","top_offenders":[{"endpoint":"GET /api/x","service":"svc-a","io_intensity_score":7.5,"io_intensity_band":"high"}]}"#,
    )
    .unwrap();
    let quality_gate: QualityGate = serde_json::from_str(
        r#"{"passed":false,"rules":[{"rule":"io_waste_ratio_max","threshold":0.3,"actual":0.42,"passed":false}]}"#,
    )
    .unwrap();
    let analysis: Analysis =
        serde_json::from_str(r#"{"duration_ms":12,"events_processed":50,"traces_analyzed":2}"#)
            .unwrap();
    let app = make_test_app().with_summary(AnalyzeSummary {
        green_summary,
        quality_gate,
        analysis,
    });
    let text = line_text(&app.build_analyze_lines());
    assert!(text.contains("Quality gate"), "got: {text}");
    assert!(text.contains("FAILED"), "got: {text}");
    assert!(text.contains("Top offenders"), "got: {text}");
    assert!(text.contains("GET /api/x"), "got: {text}");
    // Waste ratio is shown as a percentage, matching the CLI report.
    assert!(text.contains("42.0%"), "got: {text}");
    assert!(
        !text.contains("0.42 "),
        "must not show the bare fraction: {text}"
    );
    // Heuristic-band disclaimer is always present.
    assert!(text.contains("fixed heuristic thresholds"), "got: {text}");
}

#[test]
fn analyze_view_renders_carbon_block_and_uncertainty_note() {
    let green_summary: GreenSummary = serde_json::from_str(
        r#"{"total_io_ops":100,"avoidable_io_ops":42,"io_waste_ratio":0.42,"io_waste_ratio_band":"high","top_offenders":[],"co2":{"total":{"low":0.5,"mid":1.0,"high":2.0,"model":"io_proxy_v3","methodology":"sci_v1"},"avoidable":{"low":0.2,"mid":0.4,"high":0.8,"model":"io_proxy_v3","methodology":"sci_v1"},"operational_gco2":0.9,"embodied_gco2":0.1}}"#,
    )
    .unwrap();
    let quality_gate: QualityGate = serde_json::from_str(r#"{"passed":true,"rules":[]}"#).unwrap();
    let analysis: Analysis =
        serde_json::from_str(r#"{"duration_ms":1,"events_processed":1,"traces_analyzed":1}"#)
            .unwrap();
    let app = make_test_app().with_summary(AnalyzeSummary {
        green_summary,
        quality_gate,
        analysis,
    });
    let text = line_text(&app.build_analyze_lines());
    assert!(text.contains("Est. CO"), "carbon block missing: {text}");
    assert!(
        text.contains("multiplicative uncertainty"),
        "mandatory uncertainty note missing: {text}"
    );
}

#[test]
fn interpret_band_color_matches_cli_palette() {
    // Must mirror render.rs `interpret_color`: Critical red, High yellow,
    // Moderate uncolored (Reset), Healthy green. Guards against the two
    // surfaces drifting on the band gradient.
    assert_eq!(
        interpret_band_color(InterpretationLevel::Critical),
        Color::Red
    );
    assert_eq!(
        interpret_band_color(InterpretationLevel::High),
        Color::Yellow
    );
    assert_eq!(
        interpret_band_color(InterpretationLevel::Moderate),
        Color::Reset
    );
    assert_eq!(
        interpret_band_color(InterpretationLevel::Healthy),
        Color::Green
    );
}

#[test]
fn analyze_view_gate_not_evaluated_when_rules_empty() {
    // A daemon `/api/export/report` snapshot carries an empty rule set;
    // the view must not paint a misleading green PASSED.
    let green_summary: GreenSummary = serde_json::from_str(
        r#"{"total_io_ops":10,"avoidable_io_ops":5,"io_waste_ratio":0.5,"io_waste_ratio_band":"critical","top_offenders":[]}"#,
    )
    .unwrap();
    let quality_gate: QualityGate = serde_json::from_str(r#"{"passed":true,"rules":[]}"#).unwrap();
    let analysis: Analysis =
        serde_json::from_str(r#"{"duration_ms":1,"events_processed":1,"traces_analyzed":1}"#)
            .unwrap();
    let app = make_test_app().with_summary(AnalyzeSummary {
        green_summary,
        quality_gate,
        analysis,
    });
    let text = line_text(&app.build_analyze_lines());
    assert!(text.contains("Quality gate: not evaluated"), "got: {text}");
    assert!(
        !text.contains("PASSED"),
        "must not show a misleading PASSED with no rules: {text}"
    );
}

#[test]
fn analyze_enter_preserves_active_panel_for_round_trip() {
    // Esc from Correlations ascends to Analyze keeping active_panel;
    // Enter must descend back to that same panel, not force Traces.
    let mut app = make_test_app();
    app.active_panel = Panel::Correlations;
    app.view = View::Analyze;
    dispatch_analyze_key(&mut app, KeyCode::Enter);
    assert_eq!(app.view, View::Inspect);
    assert_eq!(app.active_panel, Panel::Correlations);
}

#[test]
fn detail_line_count_counts_code_location_row() {
    use sentinel_core::event::CodeLocation;
    let mut app = make_test_app();
    app.active_panel = Panel::Detail;
    let without = app.detail_panel_line_count();
    // current_finding() resolves to all_findings[0] for the default
    // selection; give it a code location so draw_detail_panel inserts
    // the "Source:" row.
    app.all_findings[0].code_location = Some(CodeLocation {
        function: Some("load_orders".to_string()),
        filepath: Some("svc/orders.rs".to_string()),
        lineno: Some(42),
        namespace: None,
    });
    let with = app.detail_panel_line_count();
    assert_eq!(
        with,
        without + 1,
        "the inserted Source: row must be counted in the scroll clamp"
    );
}

#[test]
fn finding_count_for_traces() {
    let app = make_test_app();
    assert_eq!(app.finding_count(), 1); // trace-1 has 1 finding
}

#[test]
fn select_second_trace_shows_its_findings() {
    let mut app = make_test_app();
    app.move_down(); // select trace-2
    assert_eq!(app.finding_count(), 1); // trace-2 has 1 finding
    assert_eq!(
        app.current_finding().unwrap().finding_type,
        FindingType::RedundantSql
    );
}

#[test]
fn scroll_in_detail_panel() {
    let mut app = make_test_app();
    app.active_panel = Panel::Detail;
    assert_eq!(app.scroll_offset, 0);
    app.move_down();
    assert_eq!(app.scroll_offset, 1);
    app.move_down();
    assert_eq!(app.scroll_offset, 2);
    app.move_up();
    assert_eq!(app.scroll_offset, 1);
}

#[test]
fn scroll_in_detail_panel_clamps_at_content_end() {
    let mut app = make_test_app();
    app.active_panel = Panel::Detail;

    // The test app's finding carries `green_impact` but has no cached
    // span tree, so the detail panel renders 8 logical lines (6 meta
    // rows + type header + blank + extra I/O). The scroll offset must
    // clamp at 7 (line_count - 1) no matter how many Down keys fire.
    let expected_max = app.detail_panel_line_count().saturating_sub(1);
    assert!(expected_max > 0, "test app should have detail content");

    // Hammer Down far beyond the content height.
    for _ in 0..100 {
        app.move_down();
    }

    assert_eq!(
        app.scroll_offset, expected_max,
        "scroll_offset should clamp at `line_count - 1`, got {}",
        app.scroll_offset
    );

    // move_up still works from the clamp ceiling.
    app.move_up();
    assert_eq!(app.scroll_offset, expected_max.saturating_sub(1));
}

#[test]
fn scroll_clamps_with_cached_span_tree() {
    // Exercises the `cached_detail.is_some()` branch of
    // `detail_panel_line_count`: when a span tree is cached for the
    // selected trace, the clamp must include its line count.
    //
    // Without this test, a regression that misroutes the +2 "Span tree:"
    // header offset or the tree line count would only be caught on
    // actual trace data, not in CI.
    let mut app = make_test_app();
    app.active_panel = Panel::Detail;

    // Inject a synthetic cached tree: 5 lines for the current trace.
    // 7 base meta lines + 1 green_impact (the test fixture sets it)
    // + 2 (blank + "Span tree:" header) + 5 (tree lines) = 15 logical
    // rows, so the clamp should plateau at 14.
    app.cached_detail = Some((
        app.selected_trace,
        "line1\nline2\nline3\nline4\nline5".to_string(),
    ));

    let expected_max = app.detail_panel_line_count().saturating_sub(1);
    assert_eq!(
        expected_max, 14,
        "base 7 + green_impact 1 + header 2 + tree 5 - 1 = 14"
    );

    for _ in 0..100 {
        app.move_down();
    }

    assert_eq!(
        app.scroll_offset, expected_max,
        "scroll_offset must include cached tree lines in the clamp"
    );
}

#[test]
fn switching_trace_resets_finding_and_scroll() {
    let mut app = make_test_app();
    app.scroll_offset = 5;
    app.selected_finding = 0;
    // Switch to trace-2
    app.move_down();
    assert_eq!(app.selected_trace, 1);
    assert_eq!(app.selected_finding, 0);
    assert_eq!(app.scroll_offset, 0);
}

#[test]
fn pre_rendered_trees_take_precedence_over_detect_path() {
    // `query inspect` fetches explain trees from the daemon and passes
    // them via `with_pre_rendered_trees`. This path must be preferred
    // over the local `detect + build_tree` path so users see real span
    // trees when the CLI has no raw spans.
    let mut app = make_test_app();
    let mut trees = HashMap::new();
    let trace_id = app.trace_ids[0].clone();
    trees.insert(trace_id, "pre-rendered tree from daemon".to_string());
    app.pre_rendered_trees = trees;

    let text = app.detail_tree_text();
    assert_eq!(text.as_deref(), Some("pre-rendered tree from daemon"));
}

#[test]
fn empty_spans_without_pre_rendered_tree_returns_none() {
    // Without pre-rendered trees, a stub trace with no spans should
    // not produce an empty tree panel. `make_test_app` ships with
    // `spans: vec![]` on every trace, matching the `query inspect` flow.
    let mut app = make_test_app();
    let text = app.detail_tree_text();
    assert!(text.is_none(), "empty spans must not produce a tree");
}

#[test]
fn with_pre_rendered_trees_builder_populates_field() {
    let mut trees = HashMap::new();
    trees.insert("trace-a".to_string(), "tree-a".to_string());
    let app = make_test_app().with_pre_rendered_trees(trees);
    assert_eq!(
        app.pre_rendered_trees.get("trace-a").map(String::as_str),
        Some("tree-a")
    );
}

// ── Rendering tests via TestBackend ────────────────────────────
//
// ratatui ships a headless `TestBackend` that lets us exercise the
// `draw` function and its helpers without a real terminal. These
// tests verify that the three panels render without panicking and
// include the expected content, covering the render code paths
// that a coverage tool would otherwise flag as untested.

fn render_once(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal init");
    // Pre-compute the detail tree text as the real run loop does.
    app.detail_tree_text();
    terminal
        .draw(|f| draw(f, app))
        .expect("draw should not fail");
    terminal.backend().buffer().clone()
}

/// Extract all text content from a buffer for substring assertions.
fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn draw_renders_all_three_panels() {
    let mut app = make_test_app();
    let buf = render_once(&mut app, 120, 40);
    let text = buffer_text(&buf);
    // Three panel titles should be visible.
    assert!(text.contains("Traces"), "trace panel missing");
    assert!(text.contains("Findings"), "findings panel missing");
    assert!(text.contains("Detail"), "detail panel missing");
}

#[test]
fn draw_shows_resize_indicator_on_hover() {
    let mut app = make_test_app();
    app.mouse_mode = true;
    // First render establishes `inspect_area` so the hit-test has geometry.
    render_once(&mut app, 120, 40);
    let area = app.inspect_area.get();
    let bx = area.x + area.width * 20 / 100; // first column border (cols[0]=20)
    let top_h = area.height * 50 / 100; // rows[0]=50
    handle_mouse(&mut app, moved(bx, area.y + top_h / 2));
    assert!(
        matches!(
            app.resize_target(),
            Some(DragTarget {
                axis: Axis::Horizontal,
                ..
            })
        ),
        "hover over the column border should arm a horizontal resize"
    );
    let text = buffer_text(&render_once(&mut app, 120, 40));
    assert!(text.contains('\u{256b}'), "resize handle glyph missing");
    assert!(text.contains('\u{2503}'), "heavy grab line missing");
}

#[test]
fn draw_renders_brand_footer() {
    let mut app = make_test_app();
    let buf = render_once(&mut app, 120, 40);
    assert!(
        buffer_text(&buf).contains("Powered by perf-sentinel"),
        "brand footer missing"
    );
}

#[test]
fn draw_renders_selected_trace_findings() {
    let mut app = make_test_app();
    // Fixture has trace-1 selected by default.
    let buf = render_once(&mut app, 120, 40);
    let text = buffer_text(&buf);
    // The N+1 finding's type should appear somewhere in the findings panel.
    assert!(
        text.contains("n_plus_one_sql") || text.contains("N+1"),
        "expected N+1 finding to render; got: {text}"
    );
}

#[test]
fn draw_reflects_selected_trace_change() {
    let mut app = make_test_app();
    let before = buffer_text(&render_once(&mut app, 120, 40));
    app.move_down(); // select next trace (still on Traces panel)
    let after = buffer_text(&render_once(&mut app, 120, 40));
    assert_ne!(
        before, after,
        "buffer should differ after switching selected trace"
    );
}

#[test]
fn draw_renders_with_pre_rendered_tree() {
    let mut app = make_test_app();
    let mut trees = HashMap::new();
    let trace_id = app.trace_ids[0].clone();
    trees.insert(trace_id, "pre-rendered tree from daemon".to_string());
    app.pre_rendered_trees = trees;

    let buf = render_once(&mut app, 120, 40);
    let text = buffer_text(&buf);
    assert!(
        text.contains("pre-rendered tree from daemon") || text.contains("Span tree"),
        "pre-rendered tree should surface in the detail panel"
    );
}

#[test]
fn draw_handles_small_terminal_without_panic() {
    // Minimum viable terminal size should not panic even if panels
    // are cramped.
    let mut app = make_test_app();
    let _buf = render_once(&mut app, 40, 10);
}

#[test]
fn draw_focus_changes_active_panel_border_style() {
    // Active panel change updates border color, not text content.
    // Compare the cell style of the first trace panel cell across
    // states to confirm the render path reads `active_panel`.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut app = make_test_app();
    let render = |app: &mut App| {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        app.detail_tree_text();
        terminal.draw(|f| draw(f, app)).unwrap();
        // Row 0 is the view tab bar; cell (0, 1) is the top-left corner
        // of the Traces panel border below it.
        terminal.backend().buffer()[(0, 1)].style()
    };
    let before = render(&mut app);
    app.next_panel();
    let after = render(&mut app);
    // The border style must differ (color change on focus).
    assert_ne!(
        before, after,
        "border style must differ when active panel changes"
    );
}

fn make_correlation(src_svc: &str, tgt_svc: &str) -> CrossTraceCorrelation {
    use sentinel_core::detect::correlate_cross::CorrelationEndpoint;
    CrossTraceCorrelation {
        source: CorrelationEndpoint {
            finding_type: FindingType::NPlusOneSql,
            service: src_svc.to_string(),
            template: "SELECT * FROM t WHERE id = ?".to_string(),
        },
        target: CorrelationEndpoint {
            finding_type: FindingType::SlowHttp,
            service: tgt_svc.to_string(),
            template: "GET /api/x".to_string(),
        },
        co_occurrence_count: 47,
        source_total_occurrences: 50,
        confidence: 0.92,
        median_lag_ms: 214.0,
        first_seen: "2026-04-25T10:00:00.000Z".to_string(),
        last_seen: "2026-04-25T10:30:00.000Z".to_string(),
        sample_trace_id: Some("trace-sample".to_string()),
    }
}

fn buffer_contains(buf: &ratatui::buffer::Buffer, needle: &str) -> bool {
    let area = buf.area;
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        if line.contains(needle) {
            return true;
        }
    }
    false
}

/// Flatten a `TestBackend` buffer into a newline-separated string so
/// `assert!(rendered.contains(...))` can search the whole frame.
/// Used by the modal/indicator render tests.
#[cfg(feature = "daemon")]
fn render_buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
    let area = buf.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| {
                    buf.cell((x, y))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn with_correlations_populates_field() {
    let app = make_test_app()
        .with_correlations(vec![make_correlation("a", "b"), make_correlation("c", "d")]);
    assert_eq!(app.correlation_count(), 2);
}

#[test]
fn next_panel_cycles_through_four_panels() {
    let mut app = make_test_app();
    assert_eq!(app.active_panel, Panel::Traces);
    app.next_panel();
    assert_eq!(app.active_panel, Panel::Findings);
    app.next_panel();
    assert_eq!(app.active_panel, Panel::Detail);
    app.next_panel();
    assert_eq!(app.active_panel, Panel::Correlations);
    app.next_panel();
    assert_eq!(app.active_panel, Panel::Traces);
}

#[test]
fn correlations_panel_shows_empty_hint_when_zero() {
    let mut app = make_test_app();
    app.active_panel = Panel::Correlations;
    let buf = render_once(&mut app, 120, 40);
    assert!(
        buffer_contains(&buf, "No correlations available"),
        "missing empty-state hint, dump:\n{buf:?}"
    );
}

#[test]
fn correlations_panel_renders_each_pair() {
    let mut app = make_test_app().with_correlations(vec![
        make_correlation("svc-alpha", "svc-beta"),
        make_correlation("svc-gamma", "svc-delta"),
    ]);
    app.active_panel = Panel::Correlations;
    // Test exercises the full layout: the 25% Correlations column at
    // typical terminal widths (80 to 160) truncates the metrics tail.
    // Use a very wide TestBackend so the entire row fits and every
    // field is asserted. Narrow-width rendering is covered by
    // `correlations_panel_renders_at_typical_width`.
    let buf = render_once(&mut app, 320, 40);
    assert!(
        buffer_contains(&buf, "svc-alpha"),
        "first correlation source missing"
    );
    assert!(
        buffer_contains(&buf, "svc-delta"),
        "second correlation target missing"
    );
    assert!(
        buffer_contains(&buf, "92%"),
        "confidence percentage missing"
    );
}

#[test]
fn detail_panel_shows_hint_when_spans_unavailable() {
    // make_test_app() builds traces with `spans: vec![]`, mirroring
    // a Report-mode input or a query-inspect trace whose explain
    // tree did not come back from the daemon. The Detail panel
    // must surface the two paths that produce a real tree.
    let mut app = make_test_app();
    app.active_panel = Panel::Findings;
    app.enter(); // drill into Detail
    let buf = render_once(&mut app, 160, 40);
    assert!(
        buffer_contains(&buf, "Not available"),
        "Detail panel must surface a span-tree-unavailable hint"
    );
    assert!(
        buffer_contains(&buf, "inspect --input"),
        "hint must mention `inspect --input <events>.json`"
    );
    assert!(
        buffer_contains(&buf, "query inspect"),
        "hint must mention `query inspect`"
    );
}

#[test]
fn correlations_panel_renders_at_typical_width() {
    let mut app = make_test_app().with_correlations(vec![
        make_correlation("svc-alpha", "svc-beta"),
        make_correlation("svc-gamma", "svc-delta"),
    ]);
    app.active_panel = Panel::Correlations;
    let buf = render_once(&mut app, 160, 40);
    assert!(
        buffer_contains(&buf, "svc-alpha"),
        "source service prefix must remain visible at typical width"
    );
    assert!(
        buffer_contains(&buf, "svc-gamma"),
        "second source service prefix must remain visible"
    );
}

#[test]
fn correlations_panel_strips_ansi_from_service_name() {
    use sentinel_core::detect::correlate_cross::CorrelationEndpoint;
    let mut hostile = make_correlation("a", "b");
    hostile.source.service = "evil\x1b[2J\x1b[H wipe".to_string();
    hostile.target = CorrelationEndpoint {
        finding_type: FindingType::SlowHttp,
        service: "click\x1b]8;;https://attacker/\x07tag\x1b]8;;\x07".to_string(),
        template: "GET /x".to_string(),
    };
    let mut app = make_test_app().with_correlations(vec![hostile]);
    app.active_panel = Panel::Correlations;
    let buf = render_once(&mut app, 320, 40);
    let mut full = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            full.push_str(buf[(x, y)].symbol());
        }
    }
    assert!(
        !full.as_bytes().contains(&0x1b),
        "ESC byte from service leaked into terminal buffer"
    );
    assert!(
        !full.as_bytes().contains(&0x07),
        "BEL byte from OSC 8 leaked into terminal buffer"
    );
}

#[test]
fn move_down_in_correlations_panel_advances_selection() {
    let mut app = make_test_app().with_correlations(vec![
        make_correlation("a", "b"),
        make_correlation("c", "d"),
        make_correlation("e", "f"),
    ]);
    app.active_panel = Panel::Correlations;
    assert_eq!(app.selected_correlation, 0);
    app.move_down();
    app.move_down();
    assert_eq!(app.selected_correlation, 2);
    app.move_down();
    assert_eq!(
        app.selected_correlation, 2,
        "selection must clamp at last index"
    );
}

// ── Ack modal tests (gated behind the daemon feature) ───────────

#[cfg(feature = "daemon")]
#[test]
fn ack_modal_default_is_hidden() {
    let modal = AckModalState::default();
    assert!(!modal.is_visible());
    assert_eq!(modal.mode, AckModalMode::Hidden);
    assert_eq!(modal.focus, AckFormField::Reason);
}

#[cfg(feature = "daemon")]
#[test]
fn ack_modal_open_ack_focuses_reason_and_clears_buffers() {
    let mut modal = AckModalState {
        reason_buf: "old".to_string(),
        expires_buf: "old".to_string(),
        error_message: Some("stale".to_string()),
        ..AckModalState::default()
    };
    modal.open_ack("sig-123".to_string());
    assert!(modal.is_visible());
    assert_eq!(
        modal.mode,
        AckModalMode::Ack {
            signature: "sig-123".to_string()
        }
    );
    assert_eq!(modal.focus, AckFormField::Reason);
    assert!(modal.reason_buf.is_empty());
    assert!(modal.expires_buf.is_empty());
    assert!(modal.error_message.is_none());
}

#[cfg(feature = "daemon")]
#[test]
fn ack_modal_open_unack_focuses_submit_directly() {
    let mut modal = AckModalState::default();
    modal.open_unack("sig-456".to_string());
    assert_eq!(
        modal.mode,
        AckModalMode::Unack {
            signature: "sig-456".to_string()
        }
    );
    assert_eq!(modal.focus, AckFormField::Submit);
}

#[cfg(feature = "daemon")]
#[test]
fn ack_modal_close_resets_state() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    modal.error_message = Some("err".to_string());
    modal.submitting = true;
    modal.close();
    assert!(!modal.is_visible());
    assert!(modal.error_message.is_none());
    assert!(!modal.submitting);
}

#[cfg(feature = "daemon")]
#[test]
fn ack_modal_next_field_cycles_5_steps_then_loops() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    assert_eq!(modal.focus, AckFormField::Reason);
    modal.next_field();
    assert_eq!(modal.focus, AckFormField::Expires);
    modal.next_field();
    assert_eq!(modal.focus, AckFormField::By);
    modal.next_field();
    assert_eq!(modal.focus, AckFormField::Submit);
    modal.next_field();
    assert_eq!(modal.focus, AckFormField::Cancel);
    modal.next_field();
    assert_eq!(modal.focus, AckFormField::Reason);
}

#[cfg(feature = "daemon")]
#[test]
fn ack_modal_prev_field_cycles_backwards() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    modal.prev_field();
    assert_eq!(modal.focus, AckFormField::Cancel);
    modal.prev_field();
    assert_eq!(modal.focus, AckFormField::Submit);
    modal.prev_field();
    assert_eq!(modal.focus, AckFormField::By);
    modal.prev_field();
    assert_eq!(modal.focus, AckFormField::Expires);
    modal.prev_field();
    assert_eq!(modal.focus, AckFormField::Reason);
}

#[cfg(feature = "daemon")]
#[test]
fn step_focus_wraps_at_both_ends() {
    let cycle = ACK_FOCUS_CYCLE;
    assert_eq!(
        step_focus(&cycle, AckFormField::Cancel, 1),
        AckFormField::Reason,
        "forward from last wraps to first"
    );
    assert_eq!(
        step_focus(&cycle, AckFormField::Reason, -1),
        AckFormField::Cancel,
        "backward from first wraps to last"
    );
    let unack = UNACK_FOCUS_CYCLE;
    assert_eq!(
        step_focus(&unack, AckFormField::Reason, 1),
        AckFormField::Cancel,
        "unknown current is treated as index 0, +1 lands on Cancel"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn ack_modal_unack_field_cycle_skips_text_inputs() {
    let mut modal = AckModalState::default();
    modal.open_unack("sig".to_string());
    assert_eq!(modal.focus, AckFormField::Submit);
    modal.next_field();
    assert_eq!(modal.focus, AckFormField::Cancel);
    modal.next_field();
    assert_eq!(modal.focus, AckFormField::Submit);
    modal.prev_field();
    assert_eq!(modal.focus, AckFormField::Cancel);
}

#[cfg(feature = "daemon")]
#[test]
fn handle_modal_key_typing_appends_to_focused_buffer() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    modal.reason_buf.clear();
    let _ = handle_modal_key(&mut modal, KeyCode::Char('h'));
    let _ = handle_modal_key(&mut modal, KeyCode::Char('i'));
    assert_eq!(modal.reason_buf, "hi");

    modal.focus = AckFormField::Expires;
    let _ = handle_modal_key(&mut modal, KeyCode::Char('2'));
    let _ = handle_modal_key(&mut modal, KeyCode::Char('4'));
    let _ = handle_modal_key(&mut modal, KeyCode::Char('h'));
    assert_eq!(modal.expires_buf, "24h");

    modal.focus = AckFormField::By;
    modal.by_buf.clear(); // open_ack pre-filled it from $USER
    let _ = handle_modal_key(&mut modal, KeyCode::Char('a'));
    let _ = handle_modal_key(&mut modal, KeyCode::Char('b'));
    assert_eq!(modal.by_buf, "ab");
}

#[cfg(feature = "daemon")]
#[test]
fn handle_modal_key_backspace_pops_focused_buffer() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    modal.reason_buf = "hello".to_string();
    let _ = handle_modal_key(&mut modal, KeyCode::Backspace);
    assert_eq!(modal.reason_buf, "hell");
}

#[cfg(feature = "daemon")]
#[test]
fn handle_modal_key_tab_advances_focus() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    let action = handle_modal_key(&mut modal, KeyCode::Tab);
    assert_eq!(action, ModalAction::None);
    assert_eq!(modal.focus, AckFormField::Expires);
}

#[cfg(feature = "daemon")]
#[test]
fn handle_modal_key_esc_returns_cancel() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    let action = handle_modal_key(&mut modal, KeyCode::Esc);
    assert_eq!(action, ModalAction::Cancel);
}

#[cfg(feature = "daemon")]
#[test]
fn handle_modal_key_enter_on_submit_returns_submit() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    modal.focus = AckFormField::Submit;
    let action = handle_modal_key(&mut modal, KeyCode::Enter);
    assert_eq!(action, ModalAction::Submit);
}

#[cfg(feature = "daemon")]
#[test]
fn handle_modal_key_enter_on_cancel_returns_cancel() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    modal.focus = AckFormField::Cancel;
    let action = handle_modal_key(&mut modal, KeyCode::Enter);
    assert_eq!(action, ModalAction::Cancel);
}

#[cfg(feature = "daemon")]
#[test]
fn handle_modal_key_enter_on_text_field_advances_focus() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    let action = handle_modal_key(&mut modal, KeyCode::Enter);
    assert_eq!(action, ModalAction::None);
    assert_eq!(modal.focus, AckFormField::Expires);
}

#[cfg(feature = "daemon")]
#[test]
fn handle_modal_key_enforces_max_lengths() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    modal.focus = AckFormField::Reason;
    for _ in 0..(REASON_MAX + 5) {
        let _ = handle_modal_key(&mut modal, KeyCode::Char('x'));
    }
    assert_eq!(modal.reason_buf.chars().count(), REASON_MAX);

    modal.focus = AckFormField::Expires;
    for _ in 0..(EXPIRES_MAX + 5) {
        let _ = handle_modal_key(&mut modal, KeyCode::Char('y'));
    }
    assert_eq!(modal.expires_buf.chars().count(), EXPIRES_MAX);

    modal.focus = AckFormField::By;
    modal.by_buf.clear();
    for _ in 0..(BY_MAX + 5) {
        let _ = handle_modal_key(&mut modal, KeyCode::Char('z'));
    }
    assert_eq!(modal.by_buf.chars().count(), BY_MAX);
}

#[cfg(feature = "daemon")]
#[test]
fn app_default_has_no_daemon_handle() {
    let app = make_test_app();
    assert!(app.daemon_url.is_none());
    assert!(app.api_key.is_none());
    assert!(app.acks_by_signature.is_empty());
    assert!(!app.ack_modal.is_visible());
}

#[cfg(feature = "daemon")]
#[test]
fn app_with_daemon_handle_populates_acks_by_signature() {
    let mut acks = HashMap::new();
    acks.insert(
        "sig-1".to_string(),
        AckSource::Daemon {
            by: "alice".to_string(),
            at: Utc::now(),
            reason: Some("investigating".to_string()),
            expires_at: None,
        },
    );
    let app = make_test_app().with_daemon_handle(
        "http://localhost:14318".to_string(),
        Some("secret".to_string()),
        acks,
    );
    assert_eq!(app.daemon_url.as_deref(), Some("http://localhost:14318"));
    assert_eq!(app.api_key.as_deref(), Some("secret"));
    assert!(app.acks_by_signature.contains_key("sig-1"));
}

#[cfg(feature = "daemon")]
#[test]
fn findings_panel_renders_acked_indicator_when_signature_in_map() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = make_test_app();
    app.all_findings[0].signature = "sig-acked".to_string();
    app.daemon_url = Some("http://localhost:14318".to_string());
    app.acks_by_signature.insert(
        "sig-acked".to_string(),
        AckSource::Daemon {
            by: "alice".to_string(),
            at: Utc::now(),
            reason: Some("test".to_string()),
            expires_at: None,
        },
    );
    let render_at = |width: u16| {
        let backend = TestBackend::new(width, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        render_buffer_to_string(terminal.backend().buffer())
    };

    // Wide terminal: the full "[acked by <who>]" suffix fits.
    let wide = render_at(200);
    assert!(
        wide.contains("acked by alice"),
        "expected full ack indicator on a wide terminal, got:\n{wide}"
    );

    // Narrow terminal: the Findings panel is too slim for the full suffix,
    // so it degrades to the compact "[acked]" marker (still visible).
    let narrow = render_at(120);
    assert!(
        narrow.contains("[acked]"),
        "expected compact ack marker on a narrow terminal, got:\n{narrow}"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn ack_modal_renders_centered_overlay() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = make_test_app();
    app.daemon_url = Some("http://localhost:14318".to_string());
    app.ack_modal.open_ack("sig-123".to_string());
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, &app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let rendered = render_buffer_to_string(&buffer);
    assert!(
        rendered.contains("Acknowledge finding"),
        "expected modal title, got:\n{rendered}"
    );
    assert!(rendered.contains("Reason"), "expected reason field label");
    assert!(rendered.contains("[Submit]"), "expected submit button");
}

#[cfg(feature = "daemon")]
#[test]
fn ack_submit_payload_validation_error_uses_validation_variant() {
    // Drive AckSubmitPayload::from_modal with an unparseable expires
    // input. It must return AckSubmitError::Validation (not Transport)
    // so apply_ack_outcome does not clobber the message with a
    // "network error:" prefix when it Displays it.
    let mut app = make_test_app();
    app.daemon_url = Some("http://localhost:14318".to_string());
    app.ack_modal.open_ack("sig".to_string());
    app.ack_modal.expires_buf = "not a date".to_string();
    let err =
        AckSubmitPayload::from_modal(&app).expect_err("invalid expires must surface an error");
    match err {
        crate::ack::AckSubmitError::Validation(msg) => {
            assert!(
                msg.starts_with("expires:"),
                "expected `expires:` prefix, got: {msg}"
            );
            assert!(
                !msg.contains("network error"),
                "validation must not be wrapped as network error: {msg}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[cfg(feature = "daemon")]
#[test]
fn apply_ack_outcome_success_closes_modal_and_updates_map() {
    let mut app = make_test_app();
    app.daemon_url = Some("http://localhost:14318".to_string());
    app.ack_modal.open_ack("sig".to_string());
    app.ack_modal.submitting = true;
    let mut refreshed = HashMap::new();
    refreshed.insert(
        "sig".to_string(),
        AckSource::Daemon {
            by: "alice".to_string(),
            at: Utc::now(),
            reason: Some("test".to_string()),
            expires_at: None,
        },
    );
    refreshed.insert(
        "sig2".to_string(),
        AckSource::Daemon {
            by: "bob".to_string(),
            at: Utc::now(),
            reason: None,
            expires_at: None,
        },
    );
    apply_ack_outcome(
        &mut app,
        AckOutcome::Success {
            refreshed_acks: Some(refreshed),
        },
    );
    assert!(!app.ack_modal.is_visible(), "modal must close on success");
    assert_eq!(app.acks_by_signature.len(), 2);
}

#[cfg(feature = "daemon")]
#[test]
fn apply_ack_outcome_success_with_none_keeps_existing_map() {
    // Refetch failed but write succeeded: the previous snapshot must
    // stay intact so the indicator reflects the most recent known
    // truth instead of dropping to empty.
    let mut app = make_test_app();
    app.daemon_url = Some("http://localhost:14318".to_string());
    app.acks_by_signature.insert(
        "sig-prior".to_string(),
        AckSource::Daemon {
            by: "alice".to_string(),
            at: Utc::now(),
            reason: None,
            expires_at: None,
        },
    );
    app.ack_modal.open_ack("sig-prior".to_string());
    app.ack_modal.submitting = true;
    apply_ack_outcome(
        &mut app,
        AckOutcome::Success {
            refreshed_acks: None,
        },
    );
    assert!(!app.ack_modal.is_visible(), "modal must close on success");
    assert_eq!(
        app.acks_by_signature.len(),
        1,
        "previous snapshot preserved"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn apply_ack_outcome_success_with_some_empty_clears_map() {
    // Legitimate "all acks expired" refetch: an empty Some(map)
    // overrides a prior non-empty snapshot.
    let mut app = make_test_app();
    app.daemon_url = Some("http://localhost:14318".to_string());
    app.acks_by_signature.insert(
        "sig-prior".to_string(),
        AckSource::Daemon {
            by: "alice".to_string(),
            at: Utc::now(),
            reason: None,
            expires_at: None,
        },
    );
    apply_ack_outcome(
        &mut app,
        AckOutcome::Success {
            refreshed_acks: Some(HashMap::new()),
        },
    );
    assert!(app.acks_by_signature.is_empty());
}

#[cfg(feature = "daemon")]
#[test]
fn apply_ack_outcome_failure_keeps_modal_with_error_message() {
    let mut app = make_test_app();
    app.daemon_url = Some("http://localhost:14318".to_string());
    app.ack_modal.open_ack("sig".to_string());
    app.ack_modal.submitting = true;
    apply_ack_outcome(
        &mut app,
        AckOutcome::Failure {
            message: "HTTP 503 daemon ack store disabled".to_string(),
        },
    );
    assert!(app.ack_modal.is_visible(), "modal stays open on failure");
    assert_eq!(
        app.ack_modal.error_message.as_deref(),
        Some("HTTP 503 daemon ack store disabled"),
    );
    assert!(
        !app.ack_modal.submitting,
        "submitting flag clears on failure"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn apply_ack_outcome_after_user_cancel_drops_failure_silently() {
    let mut app = make_test_app();
    app.daemon_url = Some("http://localhost:14318".to_string());
    // Open then close to simulate Esc-while-submitting.
    app.ack_modal.open_ack("sig".to_string());
    app.ack_modal.close();
    apply_ack_outcome(
        &mut app,
        AckOutcome::Failure {
            message: "transport error".to_string(),
        },
    );
    assert!(!app.ack_modal.is_visible());
    assert!(app.ack_modal.error_message.is_none());
}

#[cfg(feature = "daemon")]
#[test]
fn submit_ack_modal_is_no_op_when_already_submitting() {
    // Held Enter or double tap: the second submit must not spawn a
    // duplicate roundtrip. The submitting flag stays true and no
    // outcome is sent through the channel.
    let mut app = make_test_app();
    app.daemon_url = Some("http://localhost:14318".to_string());
    app.ack_modal.open_ack("sig".to_string());
    app.ack_modal.submitting = true;
    let (tx, mut rx) = mpsc::unbounded_channel::<AckOutcome>();
    submit_ack_modal(&mut app, &tx);
    assert!(app.ack_modal.submitting, "submitting flag stays true");
    assert!(
        matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "no spawn happened, channel must be empty"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn ack_submit_payload_debug_redacts_api_key() {
    let payload = AckSubmitPayload {
        daemon_url: "http://localhost:14318".to_string(),
        signature: "sig".to_string(),
        api_key: Some("topsecret".to_string()),
        op: AckSubmitOp::Revoke,
    };
    let dbg = format!("{payload:?}");
    assert!(dbg.contains("<redacted>"), "expected redaction marker");
    assert!(
        !dbg.contains("topsecret"),
        "api key must not appear in Debug"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn opening_ack_modal_with_no_finding_is_silent() {
    // Build an app with no findings: pressing `a` would call
    // `current_finding()` which returns None, the modal stays
    // hidden. Mirror that path here by reading current_finding and
    // confirming we cannot dispatch an open with an empty signature.
    let app = App::new(
        Vec::new(),
        Vec::new(),
        DetectConfig {
            n_plus_one_threshold: 5,
            window_ms: 500,
            slow_threshold_ms: 500,
            slow_min_occurrences: 3,
            max_fanout: 20,
            chatty_service_min_calls: 15,
            pool_saturation_concurrent_threshold: 10,
            serialized_min_sequential: 3,
            sanitizer_aware_classification:
                sentinel_core::detect::sanitizer_aware::SanitizerAwareMode::default(),
        },
    );
    assert!(app.current_finding().is_none());
    // The dispatch in run_loop is `if let Some(finding) = ...`, so
    // no current_finding means no `open_ack` call.
    assert!(!app.ack_modal.is_visible());
}

#[cfg(feature = "daemon")]
#[test]
fn modal_input_rejects_control_and_bidi_chars() {
    let mut modal = AckModalState::default();
    modal.open_ack("sig".to_string());
    modal.reason_buf.clear();
    // C0 controls (Tab/Esc/etc are KeyCode variants in real input,
    // but a paste stream could land them via Char). Bidi overrides
    // U+202A..U+202E and isolates U+2066..U+2069.
    for c in ['\u{0007}', '\u{001B}', '\u{202E}', '\u{2068}', '\u{007F}'] {
        let _ = handle_modal_key(&mut modal, KeyCode::Char(c));
    }
    assert!(
        modal.reason_buf.is_empty(),
        "control/bidi chars should not be appended, got: {:?}",
        modal.reason_buf
    );
    // Plain ASCII still works.
    let _ = handle_modal_key(&mut modal, KeyCode::Char('a'));
    assert_eq!(modal.reason_buf, "a");
}

#[cfg(feature = "daemon")]
#[test]
fn ack_modal_error_message_is_rendered() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = make_test_app();
    app.daemon_url = Some("http://localhost:14318".to_string());
    app.ack_modal.open_ack("sig".to_string());
    app.ack_modal.error_message = Some("HTTP 503 daemon ack store disabled".to_string());
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, &app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let rendered = render_buffer_to_string(&buffer);
    assert!(
        rendered.contains("daemon ack store disabled"),
        "expected error message in modal footer, got:\n{rendered}"
    );
}

#[test]
fn enter_in_correlations_jumps_to_sample_trace_detail() {
    let mut app = make_test_app().with_correlations(vec![{
        let mut c = make_correlation("a", "b");
        c.sample_trace_id = Some("trace-2".to_string());
        c
    }]);
    app.active_panel = Panel::Correlations;
    app.selected_correlation = 0;

    app.enter();

    assert_eq!(app.active_panel, Panel::Detail);
    assert_eq!(
        app.traces[app.selected_trace].trace_id, "trace-2",
        "selected_trace must point to trace-2"
    );
    assert_eq!(app.selected_finding, 0);
    assert_eq!(app.scroll_offset, 0);
}

#[test]
fn enter_in_correlations_with_no_sample_trace_id_is_silent() {
    let mut app = make_test_app().with_correlations(vec![{
        let mut c = make_correlation("a", "b");
        c.sample_trace_id = None;
        c
    }]);
    app.active_panel = Panel::Correlations;
    let panel_before = app.active_panel;
    let trace_before = app.selected_trace;

    app.enter();

    assert_eq!(
        app.active_panel, panel_before,
        "no jump must happen when sample_trace_id is None"
    );
    assert_eq!(app.selected_trace, trace_before);
}

#[test]
fn enter_in_correlations_with_unknown_trace_id_is_silent() {
    let mut app = make_test_app().with_correlations(vec![{
        let mut c = make_correlation("a", "b");
        c.sample_trace_id = Some("trace-from-yesterday".to_string());
        c
    }]);
    app.active_panel = Panel::Correlations;
    let panel_before = app.active_panel;
    let trace_before = app.selected_trace;

    app.enter();

    assert_eq!(
        app.active_panel, panel_before,
        "no jump when sample_trace_id is not in trace_index"
    );
    assert_eq!(app.selected_trace, trace_before);
}

#[test]
fn enter_in_correlations_resets_finding_and_scroll() {
    let mut app = make_test_app().with_correlations(vec![{
        let mut c = make_correlation("a", "b");
        c.sample_trace_id = Some("trace-2".to_string());
        c
    }]);
    app.active_panel = Panel::Correlations;
    app.selected_correlation = 0;
    app.selected_finding = 3;
    app.scroll_offset = 5;
    app.cached_detail = Some((0, "stale tree from trace-1".to_string()));

    app.enter();

    assert_eq!(app.selected_finding, 0, "selected_finding must reset to 0");
    assert_eq!(app.scroll_offset, 0, "scroll_offset must reset to 0");
    assert!(
        app.cached_detail.is_none(),
        "cached_detail must invalidate so the new trace's tree is recomputed"
    );
}

#[test]
fn enter_in_correlations_with_empty_correlations_is_silent() {
    let mut app = make_test_app();
    app.active_panel = Panel::Correlations;

    app.enter();

    assert_eq!(app.active_panel, Panel::Correlations);
}

#[test]
fn enter_in_correlations_with_out_of_bounds_cursor_is_silent() {
    let mut app = make_test_app().with_correlations(vec![{
        let mut c = make_correlation("a", "b");
        c.sample_trace_id = Some("trace-2".to_string());
        c
    }]);
    app.active_panel = Panel::Correlations;
    app.selected_correlation = 99;

    app.enter();

    assert_eq!(app.active_panel, Panel::Correlations);
    assert_eq!(app.selected_trace, 0);
}

#[test]
fn escape_from_correlations_drilled_detail_returns_to_correlations() {
    let mut app = make_test_app().with_correlations(vec![{
        let mut c = make_correlation("a", "b");
        c.sample_trace_id = Some("trace-2".to_string());
        c
    }]);
    app.active_panel = Panel::Correlations;
    app.selected_correlation = 0;
    app.enter();
    assert_eq!(app.active_panel, Panel::Detail);

    app.escape();

    assert_eq!(
        app.active_panel,
        Panel::Correlations,
        "Detail entered from Correlations must escape back to Correlations"
    );
}

#[test]
fn escape_from_findings_drilled_detail_still_returns_to_findings() {
    let mut app = make_test_app();
    app.active_panel = Panel::Findings;
    app.enter();
    assert_eq!(app.active_panel, Panel::Detail);

    app.escape();

    assert_eq!(
        app.active_panel,
        Panel::Findings,
        "Detail entered from Findings must keep escaping back to Findings"
    );
}

#[test]
fn jump_to_same_trace_preserves_cached_detail() {
    let mut app = make_test_app().with_correlations(vec![{
        let mut c = make_correlation("a", "b");
        c.sample_trace_id = Some("trace-1".to_string());
        c
    }]);
    app.active_panel = Panel::Correlations;
    app.selected_correlation = 0;
    app.cached_detail = Some((0, "rendered tree for trace-1".to_string()));

    app.enter();

    assert_eq!(app.active_panel, Panel::Detail);
    assert!(
        app.cached_detail.is_some(),
        "cached_detail must be preserved when jumping to the already-selected trace"
    );
}
