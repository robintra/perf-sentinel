use super::*;
use crate::tui::line_text;

/// A monitor state with a 100x20 Trends chart area at the origin:
/// vertical border at row 10 (`rows = [50,50]`), top row spans rows
/// 0..10, column border at x=50 (`cols = [50,50]`).
fn state_with_trends_area() -> MonitorState {
    let state = MonitorState::new("http://localhost:4318".into(), 5);
    state.trends_area.set(Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 20,
    });
    state
}

#[test]
fn trends_drag_vertical_changes_rows() {
    let mut state = state_with_trends_area();
    state.begin_drag(50, 10);
    assert_eq!(
        state.drag,
        Some(DragTarget {
            axis: DragAxis::Vertical,
            boundary: 0,
        })
    );
    state.apply_drag(50, 15);
    assert_eq!(state.trends_rows, [75, 25]);
    assert_eq!(state.trends_cols, TRENDS_SPLIT_DEFAULT);
}

#[test]
fn trends_drag_horizontal_changes_cols() {
    let mut state = state_with_trends_area();
    state.begin_drag(50, 5);
    assert_eq!(
        state.drag,
        Some(DragTarget {
            axis: DragAxis::Horizontal,
            boundary: 0,
        })
    );
    state.apply_drag(30, 5);
    assert_eq!(state.trends_cols, [30, 70]);
    assert_eq!(state.trends_rows, TRENDS_SPLIT_DEFAULT);
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
fn trends_hover_repaints_only_on_border_change() {
    let mut state = state_with_trends_area();
    // Move onto the Energy|Carbon border (x=50): hover set, needs repaint.
    assert!(handle_mouse(&mut state, moved(50, 5)));
    assert_eq!(
        state.hover,
        Some(DragTarget {
            axis: DragAxis::Horizontal,
            boundary: 0,
        })
    );
    // Still on the border: no change, no repaint (motion throttle).
    assert!(!handle_mouse(&mut state, moved(50, 6)));
    // Off the border: hover cleared, repaint.
    assert!(handle_mouse(&mut state, moved(100, 5)));
    assert_eq!(state.hover, None);
}

#[test]
fn cycle_tab_clears_hover() {
    let mut state = state_with_trends_area();
    handle_mouse(&mut state, moved(50, 5));
    assert!(state.hover.is_some());
    // Leaving the tab must drop the phantom highlight.
    state.cycle_tab(true);
    assert_eq!(state.hover, None);
    assert_eq!(state.drag, None);
}

#[test]
fn toggle_and_reset_mark_dirty() {
    let mut state = state_with_trends_area();
    state.dirty = false;
    state.toggle_mouse_mode();
    assert!(state.mouse_mode);
    assert!(state.dirty, "the [MOUSE] marker must repaint at once");

    state.begin_drag(50, 5);
    state.apply_drag(20, 5);
    assert_ne!(state.trends_cols, TRENDS_SPLIT_DEFAULT);
    state.dirty = false;
    state.reset_layout();
    assert_eq!(state.trends_cols, TRENDS_SPLIT_DEFAULT);
    assert!(state.dirty, "a reset must repaint at once");
}

fn snapshot_with_warnings(warning_details: Vec<Warning>) -> Snapshot {
    Snapshot {
        green_summary: GreenSummary::disabled(0),
        warning_details,
        warnings: Vec::new(),
        scrapers: None,
        status: None,
        config: None,
        incidents: Ok(Vec::new()),
    }
}

/// One incident in the daemon's wire shape (`GET /api/incidents`
/// entry): still firing, complete capture, one finding that fired
/// before the restart and one only after it.
fn incident_payload() -> &'static str {
    r#"{
          "id": "0123456789abcdef0123456789abcdef",
          "service": "cart-svc",
          "namespace": "shop",
          "kind": "oom_kill",
          "at_ms": 1700000400000,
          "detail": "container exceeded its memory limit",
          "window_from_ms": 1700000100000,
          "window_to_ms": 1700000460000,
          "oldest_finding_ms": 1700000050000,
          "findings": [
            {
              "finding": {
                "type": "n_plus_one_sql",
                "severity": "critical",
                "trace_id": "t1",
                "service": "cart-svc",
                "source_endpoint": "GET /cart",
                "pattern": { "template": "select * from items where id = ?", "occurrences": 40, "window_ms": 100, "distinct_params": 40 },
                "suggestion": "batch the lookups",
                "first_timestamp": "2026-09-01T14:00:00Z",
                "last_timestamp": "2026-09-01T14:02:00Z",
                "green_impact": { "estimated_extra_io_ops": 39, "io_intensity_score": 2.0, "io_intensity_band": "high" },
                "confidence": "daemon_production",
                "signature": "n_plus_one_sql:cart-svc:_cart:0123456789abcdef0123456789abcdef"
              },
              "stored_at_ms": 1700000300000,
              "first_seen_ms": 1700000100000,
              "seen_count": 12
            },
            {
              "finding": {
                "type": "slow_sql",
                "severity": "warning",
                "trace_id": "t2",
                "service": "cart-svc",
                "source_endpoint": "POST /checkout",
                "pattern": { "template": "update carts set total = ?", "occurrences": 1, "window_ms": 900, "distinct_params": 1 },
                "suggestion": "add an index",
                "first_timestamp": "2026-09-01T14:03:01Z",
                "last_timestamp": "2026-09-01T14:03:01Z",
                "confidence": "daemon_production"
              },
              "stored_at_ms": 1700000400500,
              "first_seen_ms": 1700000400500,
              "seen_count": 1
            }
          ]
        }"#
}

fn incident() -> IncidentSlim {
    serde_json::from_str(incident_payload()).expect("IncidentSlim deserializes")
}

fn snapshot_with_incidents(incidents: Result<Vec<IncidentSlim>, IncidentsError>) -> Snapshot {
    let mut snapshot = snapshot_with_warnings(Vec::new());
    snapshot.incidents = incidents;
    snapshot
}

/// A populated energy/carbon mix: two services in two regions, one
/// hot (real-time) intensity source and one cold.
fn snapshot_with_energy_mix() -> Snapshot {
    let green_summary: GreenSummary = serde_json::from_str(
        r#"{
              "total_io_ops":150,"avoidable_io_ops":30,"io_waste_ratio":0.2,
              "io_waste_ratio_band":"moderate","top_offenders":[],
              "energy_kwh":1.6,"energy_model":"scaphandre_rapl",
              "per_service_energy_kwh":{"order-svc":1.2,"cart-svc":0.4},
              "per_service_region":{"order-svc":"eu-west-3","cart-svc":"us-east-1"},
              "per_service_energy_model":{"order-svc":"scaphandre_rapl","cart-svc":"io_proxy_v3"},
              "per_service_measured_ratio":{"order-svc":0.92,"cart-svc":0.0},
              "per_service_carbon_kgco2eq":{"order-svc":0.00005,"cart-svc":0.00012},
              "regions":[
                {"status":"known","region":"eu-west-3","grid_intensity_gco2_kwh":41.0,"pue":1.2,"io_ops":100,"co2_gco2":0.5,"intensity_source":"real_time","intensity_estimated":false},
                {"status":"known","region":"us-east-1","grid_intensity_gco2_kwh":368.0,"pue":1.2,"io_ops":50,"co2_gco2":2.0,"intensity_source":"annual"}
              ],
              "database_waste":{"energy_kwh":0.01,"waste_kwh":0.002,"waste_gco2":0.09,"region":"eu-west-3","sql_waste_ratio":0.2,"model":"alumet_rapl"},"messaging_waste":{"energy_kwh":0.02,"waste_kwh":0.008,"waste_gco2":0.36,"region":"eu-west-3","messaging_waste_ratio":0.4,"model":"broker_specpower"}
            }"#,
    )
    .unwrap();
    Snapshot {
        green_summary,
        warning_details: Vec::new(),
        warnings: Vec::new(),
        scrapers: None,
        status: None,
        config: None,
        incidents: Ok(Vec::new()),
    }
}

fn full_status() -> StatusSlim {
    StatusSlim {
        active_traces: 62,
        max_active_traces: 100,
        analysis_queue_depth: 8,
        analysis_queue_capacity: 256,
        stored_findings: 410,
        max_retained_findings: 1000,
    }
}

#[test]
fn advisor_renders_warning_details() {
    let snapshot = snapshot_with_warnings(vec![
        Warning::new(
            "tuning",
            "raise [daemon] analysis_queue_capacity (currently 1024)",
        ),
        Warning::new("ingestion_drops", "412 OTLP requests rejected"),
    ]);
    let text = line_text(&build_advisor_lines(Some(&snapshot)));
    assert!(text.contains("Settings advisor"), "got: {text}");
    assert!(text.contains("[tuning]"), "got: {text}");
    assert!(
        text.contains("analysis_queue_capacity (currently 1024)"),
        "got: {text}"
    );
    assert!(text.contains("[ingestion_drops]"), "got: {text}");
}

#[test]
fn advisor_empty_shows_no_hints() {
    let snapshot = snapshot_with_warnings(Vec::new());
    let text = line_text(&build_advisor_lines(Some(&snapshot)));
    assert!(text.contains("No hints"), "got: {text}");
}

#[test]
fn advisor_waits_before_first_snapshot() {
    let text = line_text(&build_advisor_lines(None));
    assert!(
        text.contains("Waiting for the first snapshot"),
        "got: {text}"
    );
}

#[test]
fn energy_renders_service_and_region_tables() {
    let snapshot = snapshot_with_energy_mix();
    let text = line_text(&build_energy_lines(Some(&snapshot)));
    assert!(text.contains("Energy / carbon mix"), "got: {text}");
    // Per-service: effective source + region + measured share.
    assert!(text.contains("By service"), "got: {text}");
    assert!(text.contains("order-svc"), "got: {text}");
    assert!(text.contains("scaphandre_rapl"), "got: {text}");
    assert!(text.contains("eu-west-3"), "got: {text}");
    assert!(text.contains("io_proxy_v3"), "got: {text}");
    // Per-region: cold vs hot intensity source.
    assert!(text.contains("By region"), "got: {text}");
    assert!(text.contains("RealTime (hot)"), "got: {text}");
    assert!(text.contains("Annual (cold)"), "got: {text}");
}

#[test]
fn energy_empty_when_green_disabled() {
    let snapshot = snapshot_with_warnings(Vec::new());
    let text = line_text(&build_energy_lines(Some(&snapshot)));
    assert!(text.contains("No energy/carbon data"), "got: {text}");
}

#[test]
fn energy_renders_database_waste_line() {
    let snapshot = snapshot_with_energy_mix();
    let text = line_text(&build_energy_lines(Some(&snapshot)));
    assert!(text.contains("Database waste:"), "got: {text}");
    assert!(text.contains("20% SQL ratio"), "got: {text}");
    assert!(text.contains("0.090000 gCO2"), "got: {text}");
    assert!(text.contains("model alumet_rapl"), "got: {text}");
    assert!(text.contains("excluded from totals"), "got: {text}");
}

#[test]
fn energy_renders_broker_waste_line() {
    let snapshot = snapshot_with_energy_mix();
    let text = line_text(&build_energy_lines(Some(&snapshot)));
    assert!(text.contains("Broker waste:"), "got: {text}");
    assert!(text.contains("40% messaging ratio"), "got: {text}");
    assert!(
        text.contains("model broker_specpower"),
        "a declared cluster must not read as a measurement: {text}"
    );
    assert!(text.contains("excluded from totals"), "got: {text}");
}

#[test]
fn energy_database_waste_estimated_reads_within_totals() {
    let mut snapshot = snapshot_with_energy_mix();
    let db = snapshot.green_summary.database_waste.as_mut().unwrap();
    db.model = sentinel_core::report::DB_WASTE_MODEL_ESTIMATED.to_string();
    let text = line_text(&build_energy_lines(Some(&snapshot)));
    assert!(text.contains("model estimated"), "got: {text}");
    assert!(text.contains("within the report totals"), "got: {text}");
}

#[test]
fn database_waste_region_is_sanitized_for_terminal() {
    let mut snapshot = snapshot_with_energy_mix();
    let db = snapshot.green_summary.database_waste.as_mut().unwrap();
    db.region = Some("eu\u{1b}[2Jwest".to_string());
    let text = line_text(&build_energy_lines(Some(&snapshot)));
    assert!(
        !text.contains('\u{1b}'),
        "escape must not reach the terminal"
    );
}

#[test]
fn warning_kind_style_maps_kinds() {
    assert_eq!(
        warning_kind_style("tuning"),
        Style::default().fg(Color::Yellow)
    );
    assert_eq!(
        warning_kind_style("ingestion_drops"),
        Style::default().fg(Color::Red)
    );
    assert_eq!(warning_kind_style("cold_start"), crate::tui::dim_style());
    assert_eq!(
        warning_kind_style("something_else"),
        Style::default().fg(Color::Gray)
    );
}

#[test]
fn fmt_tiny_switches_to_scientific_below_floor() {
    assert_eq!(fmt_tiny(1.6), "1.600000");
    assert_eq!(fmt_tiny(0.0), "0.000000");
    assert_eq!(fmt_tiny(1e-5), "0.000010");
    let tiny = fmt_tiny(3.2e-7);
    assert!(tiny.contains('e'), "got: {tiny}");
    assert!(!tiny.starts_with("0.000000"), "got: {tiny}");
}

#[test]
fn fmt_tiny_normalizes_negative_zero() {
    // An empty `regions` carbon sum yields -0.0, which must not render
    // as a stray "-0.000000" in the chart legend.
    assert_eq!(fmt_tiny(-0.0), "0.000000");
    let empty: Vec<f64> = Vec::new();
    let carbon: f64 = empty.iter().sum();
    assert_eq!(fmt_tiny(carbon), "0.000000");
}

#[test]
fn energy_meas_dash_when_ratio_missing() {
    let mut snapshot = snapshot_with_energy_mix();
    snapshot
        .green_summary
        .per_service_measured_ratio
        .remove("cart-svc");
    let text = line_text(&build_energy_lines(Some(&snapshot)));
    assert!(text.contains("92%"), "order-svc keeps its ratio: {text}");
    let cart_row = text
        .lines()
        .find(|l| l.contains("cart-svc"))
        .expect("cart-svc row");
    assert!(!cart_row.contains('%'), "no fabricated 0%: {cart_row}");
    assert!(cart_row.contains(" - "), "got: {cart_row}");
}

#[test]
fn intensity_source_label_tags_cold_and_hot() {
    assert!(intensity_source_label(IntensitySource::RealTime).contains("hot"));
    assert!(intensity_source_label(IntensitySource::Annual).contains("cold"));
    assert!(intensity_source_label(IntensitySource::Hourly).contains("cold"));
    assert!(intensity_source_label(IntensitySource::MonthlyHourly).contains("cold"));
}

#[test]
fn truncate_cell_caps_with_ellipsis() {
    assert_eq!(truncate_cell("short", 10), "short");
    let long = truncate_cell("a-very-long-service-name", 8);
    assert_eq!(long.chars().count(), 8);
    assert!(long.ends_with('\u{2026}'));
}

#[test]
fn tab_cycles_and_wraps() {
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    assert_eq!(state.tab, Tab::Advisor);
    state.cycle_tab(true);
    assert_eq!(state.tab, Tab::Energy);
    state.cycle_tab(true);
    assert_eq!(state.tab, Tab::Trends);
    state.cycle_tab(true);
    assert_eq!(state.tab, Tab::Scrapers);
    state.cycle_tab(true);
    assert_eq!(state.tab, Tab::Config);
    state.cycle_tab(true);
    assert_eq!(state.tab, Tab::Incidents);
    state.cycle_tab(true);
    assert_eq!(state.tab, Tab::Advisor, "Tab wraps back");
    state.cycle_tab(false);
    assert_eq!(state.tab, Tab::Incidents, "Shift-Tab wraps the other way");
    state.cycle_tab(false);
    assert_eq!(state.tab, Tab::Config);
}

#[test]
fn header_tail_sheds_the_url_then_the_cadence_when_narrow() {
    let full = header_tail("http://localhost:4318", 5, "12s ago", 120);
    assert!(full.contains("http://localhost:4318"), "{full}");
    assert!(full.contains("5s"), "{full}");
    assert!(full.contains("12s ago"), "{full}");
    // Six labels take 70 columns of an 80-column terminal: no URL.
    let narrow = header_tail("http://localhost:4318", 5, "12s ago", 10);
    assert_eq!(narrow, "  12s ago");
    let cadence = header_tail("http://localhost:4318", 5, "12s ago", 20);
    assert_eq!(cadence, "  5s \u{00b7} 12s ago");
    // The stale marker reserved the rest: nothing, never a cut word.
    assert_eq!(header_tail("http://localhost:4318", 5, "12s ago", 2), "");
}

#[test]
fn auth_header_for_wraps_the_key_and_refuses_an_unsendable_one() {
    assert!(auth_header_for(None).unwrap().is_none());
    assert!(auth_header_for(Some("read-key-123456")).unwrap().is_some());
    assert!(auth_header_for(Some("bad\nkey")).is_err());
}

fn full_config() -> ConfigSlim {
    // All-default config: nothing should read as "modified".
    let d = DaemonConfig::default();
    ConfigSlim {
        max_active_traces: d.max_active_traces,
        trace_ttl_ms: d.trace_ttl_ms,
        sampling_rate: d.sampling_rate,
        environment: d.environment.as_str().to_string(),
        listen_addr: d.listen_addr.clone(),
        per_service_labels: d.per_service_labels,
        per_grouping_labels: d.per_grouping_labels,
        // A 0.20.0 daemon reports both, at their defaults here.
        read_api_key_set: Some(d.read_api_key.is_some()),
        incidents_enabled: Some(d.incidents.enabled),
        ..Default::default()
    }
}

#[test]
fn config_renders_params_with_defaults() {
    let mut snapshot = snapshot_with_warnings(Vec::new());
    snapshot.config = Some(full_config());
    let text = line_text(&build_config_lines(Some(&snapshot)));
    assert!(text.contains("Daemon configuration"), "got: {text}");
    assert!(text.contains("max_active_traces ="), "got: {text}");
    assert!(text.contains("environment ="), "got: {text}");
    assert!(
        text.contains("correlation.max_tracked_pairs ="),
        "got: {text}"
    );
    let gline = text
        .lines()
        .find(|l| l.contains("per_grouping_labels ="))
        .expect("per_grouping_labels row");
    assert!(
        gline.contains("= yes") && !gline.contains("modified"),
        "default knob rendered as its value, unflagged: {gline}"
    );
    // A param left at its default must not be flagged modified.
    let dline = text
        .lines()
        .find(|l| l.contains("max_active_traces ="))
        .expect("max_active_traces row");
    assert!(
        !dline.contains("modified"),
        "default value not modified: {dline}"
    );
}

#[test]
fn config_flags_modified_params() {
    let mut snapshot = snapshot_with_warnings(Vec::new());
    let mut cfg = full_config();
    cfg.trace_ttl_ms = 400; // differs from the 30000 default
    snapshot.config = Some(cfg);
    let text = line_text(&build_config_lines(Some(&snapshot)));
    let ttl = text
        .lines()
        .find(|l| l.contains("trace_ttl_ms ="))
        .expect("trace_ttl_ms row");
    assert!(ttl.contains("400"), "got: {ttl}");
    assert!(ttl.contains("modified"), "non-default value flagged: {ttl}");
}

#[test]
fn config_degrades_when_endpoint_missing() {
    let snapshot = snapshot_with_warnings(Vec::new());
    let text = line_text(&build_config_lines(Some(&snapshot)));
    assert!(text.contains("/api/config unavailable"), "got: {text}");
}

#[test]
fn config_never_shows_secret_values() {
    // The slim type has no api_key/cert/key field at all, so the tab
    // can only ever render the boolean summaries.
    let mut snapshot = snapshot_with_warnings(Vec::new());
    let mut cfg = full_config();
    cfg.ack_api_key_set = true;
    cfg.tls_configured = true;
    snapshot.config = Some(cfg);
    let text = line_text(&build_config_lines(Some(&snapshot)));
    assert!(text.contains("ack_api_key = set"), "got: {text}");
    assert!(text.contains("tls = configured"), "got: {text}");
    assert!(text.contains("read_api_key = unset"), "got: {text}");
}

#[test]
fn config_shows_the_read_key_and_the_incident_store_switches() {
    let mut snapshot = snapshot_with_warnings(Vec::new());
    let mut cfg = full_config();
    cfg.read_api_key_set = Some(true);
    cfg.incidents_enabled = Some(true);
    snapshot.config = Some(cfg);
    let text = line_text(&build_config_lines(Some(&snapshot)));
    let read = text
        .lines()
        .find(|l| l.contains("read_api_key ="))
        .expect("read_api_key row");
    assert!(
        read.contains("= set") && read.contains("modified"),
        "{read}"
    );
    let incidents = text
        .lines()
        .find(|l| l.contains("incidents ="))
        .expect("incidents row");
    assert!(
        incidents.contains("= enabled") && incidents.contains("modified"),
        "{incidents}"
    );
    // A pre-0.20.0 daemon omits both fields, and the tab omits both
    // rows rather than printing settings that daemon does not have.
    let old: ConfigSlim = serde_json::from_str(r#"{"listen_port":4318}"#).unwrap();
    assert!(old.read_api_key_set.is_none());
    assert!(old.incidents_enabled.is_none());
    let mut snapshot = snapshot_with_warnings(Vec::new());
    snapshot.config = Some(old);
    let text = line_text(&build_config_lines(Some(&snapshot)));
    assert!(!text.contains("read_api_key ="), "got: {text}");
    assert!(!text.contains("incidents ="), "got: {text}");
}

#[test]
fn config_sanitizes_daemon_controlled_strings() {
    // A hostile daemon (--daemon-url can point anywhere) could embed
    // ANSI/BiDi sequences in string-valued config fields. The Config
    // tab must strip them like every other tab.
    let mut snapshot = snapshot_with_warnings(Vec::new());
    let mut cfg = full_config();
    cfg.listen_addr = "0.0.0.0\u{1b}[31m\u{202e}evil".to_string();
    cfg.environment = "prod\u{1b}[0m".to_string();
    snapshot.config = Some(cfg);
    let text = line_text(&build_config_lines(Some(&snapshot)));
    assert!(!text.contains('\u{1b}'), "ANSI escape leaked: {text:?}");
    assert!(!text.contains('\u{202e}'), "BiDi override leaked: {text:?}");
}

#[test]
fn unreachable_keeps_last_snapshot_and_flags_stale() {
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_warnings(
        vec![Warning::new("tuning", "hint")],
    ))));
    assert!(!state.stale);
    assert!(state.latest.is_some());
    state.apply(FetchOutcome::Unreachable(None));
    assert!(state.stale, "stale flag set on failed poll");
    assert!(
        state.latest.is_some(),
        "last good snapshot must stay on screen"
    );
}

#[test]
fn oversized_snapshot_names_its_cause_beside_the_stale_marker() {
    // A daemon whose export knobs outgrew this client answers fine, but
    // the body cannot be read. A bare STALE would send the operator to
    // the network instead of the configuration.
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    // Verbatim from `query::fetch_json_reporting`: the header budget is
    // the constraint that message is written against, so asserting a
    // shorter stand-in would pass while the real one lost its tail.
    state.apply(FetchOutcome::Unreachable(Some(
        "/api/export/report over the 8 MiB read limit: lower max_export_findings \
             or max_retained_traces"
            .to_string(),
    )));
    assert!(state.stale);
    let shown = stale_reason(state.last_error.as_deref()).expect("reason must reach the header");
    assert!(shown.contains("8 MiB"), "{shown}");
    // The action must show too: a reason cut before naming the knob
    // leaves the operator where the bare marker did.
    assert!(shown.contains("lower max_export_findings"), "{shown}");
    assert!(shown.chars().count() <= HEADER_REASON_MAX_CHARS);

    // A tick that fails without a nameable cause keeps the header bare.
    state.apply(FetchOutcome::Unreachable(None));
    assert_eq!(stale_reason(state.last_error.as_deref()), None);
}

#[test]
fn handle_key_quits_cycles_and_scrolls() {
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_warnings(
        vec![
            Warning::new("tuning", "hint one"),
            Warning::new("cold_start", "hint two"),
        ],
    ))));
    state.tab = Tab::Advisor;

    // q and Esc request quit. Every other key returns false.
    assert!(handle_key(&mut state, KeyCode::Char('q')));
    assert!(handle_key(&mut state, KeyCode::Esc));

    // Tab advances the active tab without quitting.
    assert!(!handle_key(&mut state, KeyCode::Tab));
    assert_eq!(state.tab, Tab::Energy);
    // Shift-Tab from the first tab lands on the last one, Incidents.
    state.tab = Tab::Advisor;
    assert!(!handle_key(&mut state, KeyCode::BackTab));
    assert_eq!(state.tab, Tab::Incidents);

    // Down scrolls one line, Up scrolls back and clamps at the top.
    state.tab = Tab::Advisor;
    state.scroll = 0;
    assert!(!handle_key(&mut state, KeyCode::Down));
    assert_eq!(state.scroll, 1);
    assert!(!handle_key(&mut state, KeyCode::Up));
    assert_eq!(state.scroll, 0);
    assert!(!handle_key(&mut state, KeyCode::Up));
    assert_eq!(state.scroll, 0, "Up clamps at the top");
}

#[test]
fn transient_energy_failure_keeps_last_scraper_table() {
    use sentinel_core::daemon::query_api::EnergyBackendStatus;
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    let mut first = snapshot_with_warnings(Vec::new());
    first.scrapers = Some(EnergyStatusResponse {
        backends: vec![EnergyBackendStatus {
            backend: "scaphandre".to_string(),
            configured: true,
            last_scrape_age_seconds: Some(1.0),
            scrapes_ok: Some(10),
            scrapes_failed: Some(0),
        }],
    });
    state.apply(FetchOutcome::Snapshot(Box::new(first)));
    // Next tick: report fine, /api/energy transiently failed.
    state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_warnings(
        Vec::new(),
    ))));
    let scrapers = state
        .latest
        .as_ref()
        .and_then(|s| s.scrapers.as_ref())
        .expect("previous scraper table carried forward");
    assert_eq!(scrapers.backends.len(), 1);
    assert!(!state.stale, "a good report tick is not stale");
}

#[test]
fn advisor_falls_back_to_legacy_warnings() {
    // Pre-0.5.19 daemons only carry the free-text warnings field.
    let mut snapshot = snapshot_with_warnings(Vec::new());
    snapshot.warnings = vec!["legacy warning text".to_string()];
    let text = line_text(&build_advisor_lines(Some(&snapshot)));
    assert!(text.contains("legacy warning text"), "got: {text}");
    assert!(!text.contains("No hints"), "got: {text}");
}

#[test]
fn energy_service_rows_align_with_header() {
    // The Energy tab renders without wrap: every By-service row must
    // be column-aligned with its header.
    let snapshot = snapshot_with_energy_mix();
    let text = line_text(&build_energy_lines(Some(&snapshot)));
    let lines: Vec<&str> = text.lines().collect();
    let header_idx = lines
        .iter()
        .position(|l| l.contains("kWh") && l.contains("meas%"))
        .expect("service table header");
    let header = lines[header_idx];
    let row = lines[header_idx + 1];
    let h_kwh_end = header.find("kWh").expect("kWh in header") + 3;
    // The row's kWh value is right-aligned: it must END at the same
    // column the header's kWh label ends. `get` instead of byte
    // slicing: a multi-byte char at the boundary must fail the
    // assertion, not panic the test.
    let row_prefix = row.get(..h_kwh_end).unwrap_or(row);
    assert!(
        !row_prefix.ends_with(' '),
        "kWh value must right-align under its header label:\nH: {header}\nR: {row}"
    );
}

#[test]
fn scrapers_renders_backend_rows() {
    use sentinel_core::daemon::query_api::EnergyBackendStatus;
    let mut snapshot = snapshot_with_warnings(Vec::new());
    snapshot.scrapers = Some(EnergyStatusResponse {
        backends: vec![
            EnergyBackendStatus {
                backend: "scaphandre".to_string(),
                configured: true,
                last_scrape_age_seconds: Some(3.0),
                scrapes_ok: Some(120),
                scrapes_failed: Some(2),
            },
            EnergyBackendStatus {
                backend: "kepler".to_string(),
                configured: false,
                last_scrape_age_seconds: None,
                scrapes_ok: None,
                scrapes_failed: None,
            },
        ],
    });
    let text = line_text(&build_scrapers_lines(Some(&snapshot)));
    assert!(text.contains("Energy scrapers"), "got: {text}");
    assert!(text.contains("scaphandre"), "got: {text}");
    assert!(text.contains("120"), "got: {text}");
    assert!(text.contains("yes"), "got: {text}");
    // Unconfigured backend: no, and dash placeholders.
    assert!(text.contains("kepler"), "got: {text}");
    assert!(text.contains("no"), "got: {text}");
    assert!(text.contains('-'), "got: {text}");
}

#[test]
fn scrapers_degrades_when_endpoint_missing() {
    // Report fetched but /api/energy absent (older daemon).
    let snapshot = snapshot_with_warnings(Vec::new());
    let text = line_text(&build_scrapers_lines(Some(&snapshot)));
    assert!(text.contains("/api/energy unavailable"), "got: {text}");
}

#[test]
fn scrapers_waits_before_first_snapshot() {
    let text = line_text(&build_scrapers_lines(None));
    assert!(
        text.contains("Waiting for the first snapshot"),
        "got: {text}"
    );
}

#[test]
fn incident_slim_parses_a_daemon_payload() {
    let inc = incident();
    assert_eq!(inc.id, "0123456789abcdef0123456789abcdef");
    assert_eq!(inc.service, "cart-svc");
    assert_eq!(inc.namespace.as_deref(), Some("shop"));
    assert_eq!(inc.kind, "oom_kill");
    assert_eq!(inc.at_ms, 1_700_000_400_000);
    assert_eq!(inc.ended_at_ms, None);
    assert_eq!(
        inc.detail.as_deref(),
        Some("container exceeded its memory limit")
    );
    assert_eq!(inc.oldest_finding_ms, Some(1_700_000_050_000));
    assert_eq!(inc.findings.len(), 2);
    assert_eq!(inc.findings[0].seen_count, 12);
    assert_eq!(inc.findings[1].first_seen_ms, 1_700_000_400_500);
    // A kind this build does not know still parses (string, not enum).
    let mut unknown: serde_json::Value = serde_json::from_str(incident_payload()).unwrap();
    unknown["kind"] = serde_json::Value::String("brownout".into());
    let parsed: IncidentSlim = serde_json::from_value(unknown).unwrap();
    assert_eq!(parsed.kind, "brownout");
}

#[test]
fn incidents_renders_rows_and_findings() {
    let snapshot = snapshot_with_incidents(Ok(vec![incident()]));
    let text = line_text(&build_incidents_lines(Some(&snapshot)));
    assert!(text.contains("Incidents"), "got: {text}");
    let row = text
        .lines()
        .find(|l| l.contains("cart-svc \u{00b7} oom_kill"))
        .expect("incident summary row");
    assert!(row.contains("shop/cart-svc"), "{row}");
    // A long namespace never costs the service its name: the two halves
    // are capped apart, so both stay visible inside the same 24 cells.
    let mut wide = incident();
    wide.namespace = Some("platform-observability".to_string());
    wide.service = "order-service".to_string();
    let wide_row = incident_summary_row(&wide);
    assert!(
        wide_row.contains("platform-o\u{2026}/order-servi\u{2026}"),
        "{wide_row}"
    );
    assert!(row.contains("firing"), "{row}");
    // Started as a local calendar stamp, not epoch milliseconds.
    assert!(!row.contains("1700000400000"), "{row}");
    let capture = text
        .lines()
        .find(|l| l.contains("capture complete"))
        .expect("capture row");
    assert!(capture.contains("2 findings"), "{capture}");
    assert!(capture.len() <= 78, "fits an 80-column terminal: {capture}");
    assert!(
        text.contains("container exceeded its memory limit"),
        "got: {text}"
    );
    let first = text
        .lines()
        .find(|l| l.contains("n_plus_one_sql"))
        .expect("first finding row");
    assert!(first.contains("critical"), "{first}");
    assert!(first.contains("GET /cart"), "{first}");
    assert!(first.trim_end().ends_with("12"), "{first}");
    assert!(first.trim_start().starts_with("before "), "{first}");
    assert!(first.len() <= 78, "fits an 80-column terminal: {first}");
    let second = text
        .lines()
        .find(|l| l.contains("slow_sql"))
        .expect("second finding row");
    assert!(second.trim_start().starts_with("after "), "{second}");
}

#[test]
fn incidents_separates_entries_and_marks_an_ended_one() {
    let mut ended = incident();
    ended.ended_at_ms = Some(1_700_000_420_000);
    ended.findings.clear();
    let snapshot = snapshot_with_incidents(Ok(vec![incident(), ended]));
    let text = line_text(&build_incidents_lines(Some(&snapshot)));
    assert!(text.contains("ended "), "got: {text}");
    assert!(text.contains("0 findings"), "got: {text}");
    assert!(text.contains("no findings in the window"), "got: {text}");
    // A blank line between the two incidents.
    let lines: Vec<&str> = text.lines().collect();
    let second_row = lines
        .iter()
        .rposition(|l| l.contains("cart-svc \u{00b7} oom_kill"))
        .expect("second summary row");
    assert!(lines[second_row - 1].trim().is_empty(), "got: {text}");
}

#[test]
fn incidents_flags_a_partial_capture() {
    let mut partial = incident();
    partial.oldest_finding_ms = Some(partial.window_from_ms + 1);
    let mut empty = incident();
    empty.oldest_finding_ms = None;
    let snapshot = snapshot_with_incidents(Ok(vec![partial, empty]));
    let text = line_text(&build_incidents_lines(Some(&snapshot)));
    assert!(text.contains("capture partial"), "got: {text}");
    assert!(text.contains("capture empty ring"), "got: {text}");
    assert!(!text.contains("capture complete"), "got: {text}");
}

#[test]
fn incidents_sanitizes_daemon_controlled_strings() {
    let mut hostile = incident();
    hostile.service = "cart\u{1b}[31m-svc".to_string();
    hostile.detail = Some("oom\u{202e}evil".to_string());
    hostile.findings[0].finding.source_endpoint = "GET /\u{1b}[0m".to_string();
    let snapshot = snapshot_with_incidents(Ok(vec![hostile]));
    let text = line_text(&build_incidents_lines(Some(&snapshot)));
    assert!(!text.contains('\u{1b}'), "ANSI escape leaked: {text:?}");
    assert!(!text.contains('\u{202e}'), "BiDi override leaked: {text:?}");
}

#[test]
fn incidents_names_401_503_and_404() {
    let unauthorized = incidents_fetch_reason(&FetchError::HttpStatus(401));
    assert!(unauthorized.contains("--api-key-file"), "{unauthorized}");
    assert!(
        unauthorized.contains("PERF_SENTINEL_DAEMON_API_KEY"),
        "{unauthorized}"
    );
    assert!(unauthorized.contains("read_api_key"), "{unauthorized}");
    let disabled = incidents_fetch_reason(&FetchError::HttpStatus(503));
    assert!(
        disabled.contains("[daemon.incidents] enabled = false"),
        "{disabled}"
    );
    let old = incidents_fetch_reason(&FetchError::HttpStatus(404));
    assert!(old.contains("predates 0.20.0"), "{old}");
    let other = incidents_fetch_reason(&FetchError::HttpStatus(500));
    assert!(other.contains("HTTP 500"), "{other}");
    let big = incidents_fetch_reason(&FetchError::BodyTooLarge(8 * 1024 * 1024));
    assert!(big.contains("8 MiB"), "{big}");
    assert!(
        big.contains("lower --limit and page with --offset"),
        "{big}"
    );
    // The tab shows the reason where the rows would be.
    let snapshot = snapshot_with_incidents(Err(incidents_error(&FetchError::HttpStatus(401))));
    let text = line_text(&build_incidents_lines(Some(&snapshot)));
    assert!(text.contains("--api-key-file"), "got: {text}");
    assert!(!text.contains("No incidents recorded"), "got: {text}");
}

#[test]
fn incidents_empty_list_says_so() {
    let snapshot = snapshot_with_incidents(Ok(Vec::new()));
    let text = line_text(&build_incidents_lines(Some(&snapshot)));
    assert!(text.contains("No incidents recorded."), "got: {text}");
}

#[test]
fn incidents_waits_before_first_snapshot() {
    let text = line_text(&build_incidents_lines(None));
    assert!(
        text.contains("Waiting for the first snapshot"),
        "got: {text}"
    );
}

#[test]
fn incidents_failure_keeps_last_list_and_never_flips_stale() {
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_incidents(
        Ok(vec![incident()]),
    ))));
    // Next tick: report fine, the incidents poll timed out.
    state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_incidents(
        Err(transient("request timed out".to_string())),
    ))));
    assert!(!state.stale, "an incidents failure is not a stale daemon");
    let incidents = state
        .latest
        .as_ref()
        .and_then(|s| s.incidents.as_ref().ok())
        .expect("previous list carried forward");
    assert_eq!(incidents.len(), 1);
    // The Incidents entry of the scroll cache follows TABS order.
    state.tab = Tab::Incidents;
    assert!(state.line_count() > 3, "cached line count for the new tab");
}

#[test]
fn incidents_refusal_shows_when_no_poll_ever_succeeded() {
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_incidents(
        Err(incidents_error(&FetchError::HttpStatus(503))),
    ))));
    assert!(!state.stale);
    let e = state
        .latest
        .as_ref()
        .and_then(|s| s.incidents.as_ref().err())
        .expect("nothing to carry forward, the reason stays");
    assert!(e.reason.contains("enabled = false"), "{}", e.reason);
}

#[test]
fn incidents_refusal_after_a_success_replaces_the_list() {
    // A restarted daemon with a rotated key answers 401: showing the
    // pre-restart list would be wrong in a way the operator cannot detect.
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_incidents(
        Ok(vec![incident()]),
    ))));
    state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_incidents(
        Err(incidents_error(&FetchError::HttpStatus(401))),
    ))));
    assert!(!state.stale);
    let e = state
        .latest
        .as_ref()
        .and_then(|s| s.incidents.as_ref().err())
        .expect("a refusal replaces the list");
    assert!(
        e.refused && e.reason.contains("--api-key-file"),
        "{}",
        e.reason
    );
    assert!(!incidents_error(&FetchError::HttpStatus(500)).refused);
    assert!(!transient("timed out".into()).refused);
}

#[test]
fn trend_history_caps_at_capacity() {
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    for _ in 0..(TREND_CAPACITY + 10) {
        state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_warnings(
            Vec::new(),
        ))));
    }
    assert_eq!(state.history.len(), TREND_CAPACITY);
}

#[test]
fn trend_point_computes_percentages() {
    let mut snapshot = snapshot_with_energy_mix();
    snapshot.status = Some(full_status());
    let p = trend_point(&snapshot);
    assert_eq!(p.traces_pct, Some(62.0));
    assert!((p.queue_pct.unwrap() - 3.125).abs() < 1e-9);
    assert_eq!(p.findings_pct, Some(41.0));
    // Carbon: sum of the per-region co2_gco2 (0.5 + 2.0).
    assert!((p.carbon_gco2 - 2.5).abs() < 1e-9, "got {}", p.carbon_gco2);
    assert!((p.energy_kwh - 1.6).abs() < 1e-9);
}

#[test]
fn trend_point_clamps_gauge_over_cap_to_100() {
    // A gauge above its cap (e.g. active_traces briefly exceeding
    // max_active_traces) must read as a full 100%, not overshoot.
    let mut snapshot = snapshot_with_warnings(Vec::new());
    let mut status = full_status();
    status.active_traces = 150;
    status.max_active_traces = 100;
    snapshot.status = Some(status);
    let p = trend_point(&snapshot);
    assert_eq!(p.traces_pct, Some(100.0));
}

#[test]
fn trend_point_suppresses_ratio_on_zero_cap() {
    // Old daemon: serde defaults leave the caps at 0.
    let mut snapshot = snapshot_with_warnings(Vec::new());
    let mut status = full_status();
    status.max_active_traces = 0;
    status.analysis_queue_capacity = 0;
    status.max_retained_findings = 0;
    snapshot.status = Some(status);
    let p = trend_point(&snapshot);
    assert_eq!(p.traces_pct, None);
    assert_eq!(p.queue_pct, None);
    assert_eq!(p.findings_pct, None);
}

#[test]
fn trend_point_clamps_negative_queue_depth() {
    let mut snapshot = snapshot_with_warnings(Vec::new());
    let mut status = full_status();
    status.analysis_queue_depth = -3;
    snapshot.status = Some(status);
    let p = trend_point(&snapshot);
    assert_eq!(p.queue_pct, Some(0.0));
}

#[test]
fn trend_series_keeps_x_aligned_across_missing_status() {
    // Tick 0 carries status, tick 1 does not: the percentage series
    // must keep the global tick index as x, not re-densify.
    let mut state = MonitorState::new("http://localhost:4318".into(), 5);
    let mut first = snapshot_with_warnings(Vec::new());
    first.status = Some(full_status());
    state.apply(FetchOutcome::Snapshot(Box::new(first)));
    state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_warnings(
        Vec::new(),
    ))));
    let mut third = snapshot_with_warnings(Vec::new());
    third.status = Some(full_status());
    state.apply(FetchOutcome::Snapshot(Box::new(third)));

    let series = build_trend_series(&state.history);
    assert_eq!(series.energy.len(), 3);
    assert_eq!(series.traces_pct.len(), 2, "middle tick lacks status");
    // x coordinates are exact small integers, compare as such.
    #[allow(clippy::cast_possible_truncation)]
    let xs: Vec<i64> = series.traces_pct.iter().map(|p| p.0 as i64).collect();
    assert_eq!(xs, vec![0, 2], "x is the global tick index");
}

#[test]
fn status_slim_parses_old_daemon_payload() {
    // Pre-0.8.8 /api/status: no capacity fields. serde defaults
    // must fill the caps with 0 ("unknown").
    let old = r#"{"version":"0.8.7","uptime_seconds":12,"active_traces":4,"stored_findings":7}"#;
    let parsed: StatusSlim = serde_json::from_str(old).unwrap();
    assert_eq!(parsed.active_traces, 4);
    assert_eq!(parsed.max_active_traces, 0);
    assert_eq!(parsed.analysis_queue_capacity, 0);
    assert_eq!(parsed.max_retained_findings, 0);
}

#[test]
fn fmt_span_secs_picks_compact_unit() {
    assert_eq!(fmt_span_secs(90), "90s");
    assert_eq!(fmt_span_secs(600), "10m");
    assert_eq!(fmt_span_secs(7200), "2.0h");
}
