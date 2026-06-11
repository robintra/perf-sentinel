//! `perf-sentinel query monitor`: live operator TUI over the daemon.
//!
//! Two tabs cycled with Tab: `Advisor` (the daemon's `warning_details`
//! settings hints) and `Energy` (the effective energy/carbon mix per
//! service and per region). A background task polls
//! `/api/export/report` on a fixed interval; when the daemon becomes
//! unreachable the last good snapshot stays on screen with a stale
//! indicator instead of going blank.
//!
//! Deliberately separate from the `inspect` drill-down TUI: `inspect`
//! is the developer's trace/finding browser, this is the operator's
//! deployment monitor. The data here (config hints, source provenance,
//! per-region intensities) is categorical and high-cardinality, which
//! is exactly what the bounded-label rule keeps off `/metrics`.

#![cfg(all(feature = "daemon", feature = "tui"))]

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use sentinel_core::report::{GreenSummary, Report, Warning};
use sentinel_core::score::carbon::IntensitySource;
use sentinel_core::text_safety::sanitize_for_terminal;
use tokio::sync::mpsc;

/// How often the sync event loop wakes up to drain fresh snapshots and
/// repaint (the header age counter included). Keystrokes interrupt the
/// wait immediately, so this only bounds the repaint latency of data
/// that arrived between keys.
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Per-request timeout of the background poller.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The monitor's tabs, cycled with Tab/Shift-Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Advisor,
    Energy,
}

const TABS: &[(Tab, &str)] = &[(Tab::Advisor, "Advisor"), (Tab::Energy, "Energy")];

/// One successful `/api/export/report` fetch, reduced to the fields the
/// monitor renders.
struct Snapshot {
    green_summary: GreenSummary,
    warning_details: Vec<Warning>,
}

/// Outcome of one poll tick. `Unreachable` keeps the previous snapshot
/// on screen and flips the stale indicator.
enum FetchOutcome {
    Snapshot(Box<Snapshot>),
    Unreachable,
}

struct MonitorState {
    daemon_url: String,
    refresh_secs: u64,
    tab: Tab,
    scroll: u16,
    latest: Option<Snapshot>,
    /// True when the most recent poll failed; `latest` then shows the
    /// last good data.
    stale: bool,
    last_update: Option<Instant>,
}

impl MonitorState {
    fn new(daemon_url: String, refresh_secs: u64) -> Self {
        Self {
            daemon_url,
            refresh_secs,
            tab: Tab::Advisor,
            scroll: 0,
            latest: None,
            stale: false,
            last_update: None,
        }
    }

    fn apply(&mut self, outcome: FetchOutcome) {
        match outcome {
            FetchOutcome::Snapshot(s) => {
                self.latest = Some(*s);
                self.stale = false;
                self.last_update = Some(Instant::now());
                // The new snapshot may be shorter than the scroll position.
                self.scroll = self.scroll.min(self.line_count().saturating_sub(1));
            }
            FetchOutcome::Unreachable => self.stale = true,
        }
    }

    fn cycle_tab(&mut self, forward: bool) {
        let n = TABS.len();
        let i = TABS
            .iter()
            .position(|(t, _)| *t == self.tab)
            .unwrap_or_default();
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.tab = TABS[next].0;
        self.scroll = 0;
    }

    /// Logical line count of the active tab body, for the scroll clamp.
    fn line_count(&self) -> u16 {
        let lines = match self.tab {
            Tab::Advisor => build_advisor_lines(self.latest.as_ref()),
            Tab::Energy => build_energy_lines(self.latest.as_ref()),
        };
        u16::try_from(lines.len()).unwrap_or(u16::MAX)
    }
}

/// Entry point for `query monitor`. `base_url` is already validated and
/// trimmed by `cmd_query`. Spawns the poller, runs the sync event loop,
/// aborts the poller on exit. Synchronous but must be called inside the
/// tokio runtime (`tokio::spawn` and `block_in_place` both require it).
pub(crate) fn cmd_monitor(base_url: &str, refresh_secs: u64) {
    let (tx, mut rx) = mpsc::unbounded_channel::<FetchOutcome>();
    let poller = tokio::spawn(poll_loop(
        base_url.to_string(),
        Duration::from_secs(refresh_secs),
        tx,
    ));
    let mut state = MonitorState::new(base_url.to_string(), refresh_secs);
    // Same idiom as `query inspect`: the loop blocks on crossterm events,
    // so keep it off the async worker threads.
    let result = tokio::task::block_in_place(|| run(&mut state, &mut rx));
    poller.abort();
    if let Err(e) = result {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}

/// Background poller: fetch, push, sleep, repeat. Exits when the UI side
/// of the channel is gone.
async fn poll_loop(base_url: String, refresh: Duration, tx: mpsc::UnboundedSender<FetchOutcome>) {
    let client = sentinel_core::http_client::build_client();
    loop {
        let outcome = fetch_snapshot(&client, &base_url).await;
        if tx.send(outcome).is_err() {
            return;
        }
        tokio::time::sleep(refresh).await;
    }
}

async fn fetch_snapshot(
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
) -> FetchOutcome {
    let Ok(uri) =
        format!("{base_url}/api/export/report").parse::<sentinel_core::http_client::Uri>()
    else {
        return FetchOutcome::Unreachable;
    };
    let Ok(body) = sentinel_core::http_client::fetch_get(
        client,
        &uri,
        "perf-sentinel-query",
        FETCH_TIMEOUT,
        None,
    )
    .await
    else {
        return FetchOutcome::Unreachable;
    };
    match serde_json::from_slice::<Report>(&body) {
        Ok(report) => FetchOutcome::Snapshot(Box::new(Snapshot {
            green_summary: report.green_summary,
            warning_details: report.warning_details,
        })),
        Err(_) => FetchOutcome::Unreachable,
    }
}

/// Terminal scaffold around [`run_loop`], mirroring `tui::run`.
fn run(state: &mut MonitorState, rx: &mut mpsc::UnboundedReceiver<FetchOutcome>) -> io::Result<()> {
    crate::tui::install_terminal_restore_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, state, rx);

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut MonitorState,
    rx: &mut mpsc::UnboundedReceiver<FetchOutcome>,
) -> io::Result<()> {
    loop {
        while let Ok(outcome) = rx.try_recv() {
            state.apply(outcome);
        }
        terminal.draw(|f| draw(f, state))?;

        // Short-timeout poll instead of a blocking read: fresh snapshots
        // and the header age counter repaint without a keypress.
        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Tab => state.cycle_tab(true),
                KeyCode::BackTab => state.cycle_tab(false),
                KeyCode::Up | KeyCode::Char('k') => {
                    state.scroll = state.scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let max = state.line_count().saturating_sub(1);
                    if state.scroll < max {
                        state.scroll = state.scroll.saturating_add(1);
                    }
                }
                _ => {}
            }
        }
    }
}

fn draw(f: &mut Frame, state: &MonitorState) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(f.area());
    draw_header(f, state, outer[0]);

    let (title, lines, wrap) = match state.tab {
        Tab::Advisor => (
            " Advisor ",
            build_advisor_lines(state.latest.as_ref()),
            true,
        ),
        // No wrap on Energy: the fixed-width columns must not reflow.
        Tab::Energy => (" Energy ", build_energy_lines(state.latest.as_ref()), false),
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let mut paragraph = Paragraph::new(lines).block(block).scroll((state.scroll, 0));
    if wrap {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }
    f.render_widget(paragraph, outer[1]);
}

/// One-line header: tabs, daemon URL, refresh cadence, snapshot age and
/// the stale indicator when the daemon stopped answering.
fn draw_header(f: &mut Frame, state: &MonitorState, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = vec![Span::raw(" ")];
    for (i, (tab, label)) in TABS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" \u{00b7} ", dim));
        }
        let style = if state.tab == *tab {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            dim
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    let age = state.last_update.map_or_else(
        || "waiting".to_string(),
        |t| format!("updated {}s ago", t.elapsed().as_secs()),
    );
    spans.push(Span::styled(
        format!(
            "    {} \u{00b7} refresh {}s \u{00b7} {age}",
            state.daemon_url, state.refresh_secs
        ),
        dim,
    ));
    if state.stale {
        spans.push(Span::styled(
            "  [daemon unreachable, showing last data]".to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(
        "    Tab \u{21c4} \u{00b7} j/k scroll \u{00b7} q quit".to_string(),
        dim,
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Body of the Advisor tab: the daemon's settings-advisor hints
/// (`warning_details`). Each entry is `[kind] message`, color-coded by
/// kind. Both fields are sanitized for the terminal, matching the other
/// daemon-sourced strings the TUIs render.
fn build_advisor_lines(latest: Option<&Snapshot>) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Settings advisor".to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Config tuning hints the daemon emits when a setting looks undersized for the load."
                .to_string(),
            dim,
        )),
        Line::from(""),
    ];
    let Some(snapshot) = latest else {
        lines.push(Line::from(Span::styled(
            "Waiting for the first snapshot from /api/export/report...".to_string(),
            dim,
        )));
        return lines;
    };
    if snapshot.warning_details.is_empty() {
        lines.push(Line::from(Span::styled(
            "No hints: the daemon reports no undersized setting.".to_string(),
            dim,
        )));
        return lines;
    }
    for w in &snapshot.warning_details {
        lines.push(Line::from(vec![
            Span::raw("  [".to_string()),
            Span::styled(
                sanitize_for_terminal(&w.kind).into_owned(),
                Style::default().fg(interpret_warning_color(&w.kind)),
            ),
            Span::raw("] ".to_string()),
            Span::raw(sanitize_for_terminal(&w.message).into_owned()),
        ]));
    }
    lines
}

/// Body of the Energy tab: the effective energy/carbon mix, all from the
/// live `green_summary` (no extra aggregation). Two tables: per service
/// (effective source, measured share, energy, region) and per region
/// (grid intensity, cold embedded vs hot scraped source).
fn build_energy_lines(latest: Option<&Snapshot>) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled("Energy / carbon mix".to_string(), bold)),
        Line::from(Span::styled(
            "Effective source per service and grid intensity per region (cold embedded vs hot scraped)."
                .to_string(),
            dim,
        )),
        Line::from(""),
    ];
    let Some(snapshot) = latest else {
        lines.push(Line::from(Span::styled(
            "Waiting for the first snapshot from /api/export/report...".to_string(),
            dim,
        )));
        return lines;
    };
    let gs = &snapshot.green_summary;
    if gs.per_service_energy_kwh.is_empty() && gs.regions.is_empty() {
        lines.push(Line::from(Span::styled(
            "No energy/carbon data (green scoring disabled, or no events analyzed yet)."
                .to_string(),
            dim,
        )));
        return lines;
    }

    lines.push(Line::from(vec![
        Span::styled("Window energy: ".to_string(), dim),
        Span::raw(format!("{:.6} kWh", gs.energy_kwh)),
        Span::styled(
            format!("   model: {}", sanitize_for_terminal(&gs.energy_model)),
            dim,
        ),
    ]));
    lines.push(Line::from(""));

    if !gs.per_service_energy_kwh.is_empty() {
        lines.push(Line::from(Span::styled("By service".to_string(), bold)));
        lines.push(Line::from(Span::styled(
            format!(
                "  {:<22} {:<14} {:<16} {:>6}  {:>12}  {}",
                "service", "region", "source", "meas%", "kWh", "kgCO2eq"
            ),
            dim,
        )));
        // BTreeMap iterates sorted by service, deterministic output.
        for (svc, kwh) in &gs.per_service_energy_kwh {
            let region = gs.per_service_region.get(svc).map_or("-", String::as_str);
            let model = gs
                .per_service_energy_model
                .get(svc)
                .map_or("-", String::as_str);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let meas = (gs
                .per_service_measured_ratio
                .get(svc)
                .copied()
                .unwrap_or(0.0)
                * 100.0)
                .round() as u32;
            let co2 = gs
                .per_service_carbon_kgco2eq
                .get(svc)
                .copied()
                .unwrap_or(0.0);
            lines.push(Line::from(Span::raw(format!(
                "  {:<22} {:<14} {:<16} {:>5}% {:>12.6}  {:.9}",
                truncate_cell(svc, 22),
                truncate_cell(region, 14),
                truncate_cell(model, 16),
                meas,
                kwh,
                co2,
            ))));
        }
        lines.push(Line::from(""));
    }

    if !gs.regions.is_empty() {
        lines.push(Line::from(Span::styled("By region".to_string(), bold)));
        lines.push(Line::from(Span::styled(
            format!(
                "  {:<14} {:>10} {:<22} {:<10} {:>8}  {}",
                "region", "gCO2/kWh", "source", "estimated", "ops", "gCO2"
            ),
            dim,
        )));
        for r in &gs.regions {
            let estimated = match r.intensity_estimated {
                Some(true) => "yes",
                Some(false) => "no",
                None => "-",
            };
            lines.push(Line::from(Span::raw(format!(
                "  {:<14} {:>10.1} {:<22} {:<10} {:>8}  {:.6}",
                truncate_cell(&r.region, 14),
                r.grid_intensity_gco2_kwh,
                intensity_source_label(r.intensity_source),
                estimated,
                r.io_ops,
                r.co2_gco2,
            ))));
        }
    }
    lines
}

/// Color for an advisor hint by its stable `kind`. `tuning` is the
/// actionable yellow, `ingestion_drops` the louder red (data was lost),
/// `cold_start` dim (transient), anything else neutral.
fn interpret_warning_color(kind: &str) -> Color {
    use sentinel_core::report::warnings::{COLD_START, INGESTION_DROPS, TUNING};
    match kind {
        TUNING => Color::Yellow,
        INGESTION_DROPS => Color::Red,
        COLD_START => Color::DarkGray,
        _ => Color::Gray,
    }
}

/// Label for a grid-intensity source, tagging cold (embedded reference
/// data) vs hot (live Electricity Maps real-time).
fn intensity_source_label(src: IntensitySource) -> &'static str {
    match src {
        IntensitySource::RealTime => "RealTime (hot)",
        IntensitySource::MonthlyHourly => "MonthlyHourly (cold)",
        IntensitySource::Hourly => "Hourly (cold)",
        IntensitySource::Annual => "Annual (cold)",
    }
}

/// Sanitize a daemon-sourced cell value and hard-cap its length (with an
/// ellipsis) so the fixed-width energy tables stay aligned.
fn truncate_cell(s: &str, max: usize) -> String {
    let safe = sanitize_for_terminal(s);
    if safe.chars().count() <= max {
        return safe.into_owned();
    }
    let mut out: String = safe.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn snapshot_with_warnings(warning_details: Vec<Warning>) -> Snapshot {
        Snapshot {
            green_summary: GreenSummary::disabled(0),
            warning_details,
        }
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
              ]
            }"#,
        )
        .unwrap();
        Snapshot {
            green_summary,
            warning_details: Vec::new(),
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
    fn interpret_warning_color_maps_kinds() {
        assert_eq!(interpret_warning_color("tuning"), Color::Yellow);
        assert_eq!(interpret_warning_color("ingestion_drops"), Color::Red);
        assert_eq!(interpret_warning_color("cold_start"), Color::DarkGray);
        assert_eq!(interpret_warning_color("something_else"), Color::Gray);
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
        assert_eq!(state.tab, Tab::Advisor, "Tab wraps back");
        state.cycle_tab(false);
        assert_eq!(state.tab, Tab::Energy, "Shift-Tab wraps the other way");
    }

    #[test]
    fn unreachable_keeps_last_snapshot_and_flags_stale() {
        let mut state = MonitorState::new("http://localhost:4318".into(), 5);
        state.apply(FetchOutcome::Snapshot(Box::new(snapshot_with_warnings(
            vec![Warning::new("tuning", "hint")],
        ))));
        assert!(!state.stale);
        assert!(state.latest.is_some());
        state.apply(FetchOutcome::Unreachable);
        assert!(state.stale, "stale flag set on failed poll");
        assert!(
            state.latest.is_some(),
            "last good snapshot must stay on screen"
        );
    }
}
