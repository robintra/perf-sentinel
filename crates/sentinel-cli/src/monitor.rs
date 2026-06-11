//! `perf-sentinel query monitor`: live operator TUI over the daemon.
//!
//! Three tabs cycled with Tab: `Advisor` (the daemon's `warning_details`
//! settings hints), `Energy` (the effective energy/carbon mix per
//! service and per region) and `Scrapers` (live health of the energy
//! backends from `/api/energy`). A background task polls the daemon on
//! a fixed interval; when it becomes unreachable the last good snapshot
//! stays on screen with a stale indicator instead of going blank.
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
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use sentinel_core::daemon::query_api::EnergyStatusResponse;
use sentinel_core::report::{GreenSummary, Warning};
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
    Scrapers,
}

const TABS: [(Tab, &str); 3] = [
    (Tab::Advisor, "Advisor"),
    (Tab::Energy, "Energy"),
    (Tab::Scrapers, "Scrapers"),
];

/// Partial deserialization target for `/api/export/report`: only the
/// fields the monitor renders. Serde skips the heavy remainder
/// (findings, correlations, per-endpoint ops) without materializing it,
/// which matters on a polling path.
#[derive(serde::Deserialize)]
struct ReportSlim {
    green_summary: GreenSummary,
    #[serde(default)]
    warning_details: Vec<Warning>,
    /// Legacy free-text warnings (pre-0.5.19 daemons). Rendered by the
    /// Advisor tab when `warning_details` is empty, matching the
    /// renderer convention in `report/mod.rs`.
    #[serde(default)]
    warnings: Vec<String>,
}

/// One successful poll tick, reduced to the fields the monitor renders.
/// `scrapers` is `None` when `/api/energy` is unavailable (daemon
/// predating the endpoint), independently of the report fetch.
struct Snapshot {
    green_summary: GreenSummary,
    warning_details: Vec<Warning>,
    warnings: Vec<String>,
    scrapers: Option<EnergyStatusResponse>,
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
    /// Body line count per tab (TABS order), recomputed once per
    /// snapshot instead of rebuilding the line vectors on every
    /// keypress for the scroll clamp.
    line_counts: [u16; TABS.len()],
    /// Something visible changed (snapshot, tab, scroll): repaint on
    /// the next loop turn. The header age repaints on its own clock.
    dirty: bool,
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
            line_counts: [0; TABS.len()],
            dirty: true,
        }
    }

    /// Recompute the cached body line counts for all tabs. Called once
    /// per applied snapshot.
    fn refresh_line_counts(&mut self) {
        let latest = self.latest.as_ref();
        let count = |lines: Vec<Line<'static>>| u16::try_from(lines.len()).unwrap_or(u16::MAX);
        self.line_counts = [
            count(build_advisor_lines(latest)),
            count(build_energy_lines(latest)),
            count(build_scrapers_lines(latest)),
        ];
    }

    fn apply(&mut self, outcome: FetchOutcome) {
        match outcome {
            FetchOutcome::Snapshot(s) => {
                let mut s = *s;
                // A transient /api/energy failure on an otherwise good
                // tick must not wipe the last scraper table (nor render
                // the misleading old-daemon hint): carry the previous
                // value forward. A genuinely old daemon never produced
                // Some, so its hint is unaffected.
                if s.scrapers.is_none()
                    && let Some(prev) = self.latest.as_mut().and_then(|p| p.scrapers.take())
                {
                    s.scrapers = Some(prev);
                }
                self.latest = Some(s);
                self.stale = false;
                self.last_update = Some(Instant::now());
                self.refresh_line_counts();
                // The new snapshot may be shorter than the scroll position.
                self.scroll = self.scroll.min(self.line_count().saturating_sub(1));
                self.dirty = true;
            }
            FetchOutcome::Unreachable => {
                self.dirty = self.dirty || !self.stale;
                self.stale = true;
            }
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
        self.dirty = true;
    }

    /// Logical line count of the active tab body, for the scroll clamp.
    /// Reads the per-snapshot cache, never rebuilds the lines.
    fn line_count(&self) -> u16 {
        let i = TABS
            .iter()
            .position(|(t, _)| *t == self.tab)
            .unwrap_or_default();
        self.line_counts[i]
    }
}

/// Entry point for `query monitor`. `base_url` is already validated and
/// trimmed by `cmd_query`. Spawns the poller, runs the sync event loop,
/// aborts the poller on exit. Synchronous but must be called inside the
/// multi-thread tokio runtime: `tokio::spawn` needs a runtime and
/// `block_in_place` panics on the `current_thread` flavor.
pub(crate) fn cmd_monitor(base_url: &str, refresh_secs: u64) {
    // Bounded: only the newest snapshot matters, and the UI thread can
    // stall on terminal writes (hung SSH peer). An unbounded queue
    // would then grow by one boxed snapshot per tick without limit.
    let (tx, mut rx) = mpsc::channel::<FetchOutcome>(4);
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
async fn poll_loop(base_url: String, refresh: Duration, tx: mpsc::Sender<FetchOutcome>) {
    let client = sentinel_core::http_client::build_client();
    loop {
        let outcome = fetch_snapshot(&client, &base_url).await;
        match tx.try_send(outcome) {
            // Full: the UI stalled with a full queue, drop this
            // snapshot, the next tick brings a fresher one anyway.
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => return,
        }
        tokio::time::sleep(refresh).await;
    }
}

async fn fetch_snapshot(
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
) -> FetchOutcome {
    // Concurrent: the tick latency is the slower of the two requests,
    // not their sum. The energy fetch is best-effort: `None` covers both
    // a daemon predating /api/energy and a transient failure; `apply`
    // carries the previous value forward so only the former shows the
    // old-daemon hint persistently.
    let (report, scrapers) = tokio::join!(
        crate::query::fetch_json::<ReportSlim>(
            client,
            base_url,
            "/api/export/report",
            FETCH_TIMEOUT
        ),
        crate::query::fetch_json::<EnergyStatusResponse>(
            client,
            base_url,
            "/api/energy",
            FETCH_TIMEOUT
        ),
    );
    match report {
        Some(report) => FetchOutcome::Snapshot(Box::new(Snapshot {
            green_summary: report.green_summary,
            warning_details: report.warning_details,
            warnings: report.warnings,
            scrapers,
        })),
        None => FetchOutcome::Unreachable,
    }
}

/// Terminal scaffold around [`run_loop`], mirroring `tui::run`.
fn run(state: &mut MonitorState, rx: &mut mpsc::Receiver<FetchOutcome>) -> io::Result<()> {
    crate::tui::install_terminal_restore_panic_hook();
    enable_raw_mode()?;
    // Restores raw mode + alternate screen on every exit path,
    // including an Err from the setup lines below.
    let _restore = crate::tui::RawModeGuard;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, state, rx);

    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut MonitorState,
    rx: &mut mpsc::Receiver<FetchOutcome>,
) -> io::Result<()> {
    // Repaint only when something visible changed: a new snapshot, a
    // key action, or the header's whole-second age counter ticking.
    // Otherwise the 250ms poll would rebuild the body 4x/second idle.
    let mut last_age: Option<Option<u64>> = None;
    loop {
        while let Ok(outcome) = rx.try_recv() {
            state.apply(outcome);
        }
        let age = state.last_update.map(|t| t.elapsed().as_secs());
        if state.dirty || last_age != Some(age) {
            terminal.draw(|f| draw(f, state))?;
            state.dirty = false;
            last_age = Some(age);
        }

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
                    let prev = state.scroll;
                    state.scroll = state.scroll.saturating_sub(1);
                    state.dirty = state.dirty || state.scroll != prev;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let max = state.line_count().saturating_sub(1);
                    if state.scroll < max {
                        state.scroll = state.scroll.saturating_add(1);
                        state.dirty = true;
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

    // Key hints live in the block title rather than the header line,
    // which must stay short enough for 80-column terminals.
    let (title, lines, wrap) = match state.tab {
        Tab::Advisor => (
            " Advisor \u{00b7} Tab \u{21c4} \u{00b7} j/k \u{2195} \u{00b7} q ",
            build_advisor_lines(state.latest.as_ref()),
            true,
        ),
        // No wrap on the table tabs: fixed-width columns must not reflow.
        Tab::Energy => (
            " Energy \u{00b7} Tab \u{21c4} \u{00b7} j/k \u{2195} \u{00b7} q ",
            build_energy_lines(state.latest.as_ref()),
            false,
        ),
        Tab::Scrapers => (
            " Scrapers \u{00b7} Tab \u{21c4} \u{00b7} j/k \u{2195} \u{00b7} q ",
            build_scrapers_lines(state.latest.as_ref()),
            false,
        ),
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

/// One-line header: tabs, daemon URL, refresh cadence, snapshot age,
/// and a short stale marker when the daemon stopped answering (or sent
/// an incompatible response). Kept compact so it fits 80-column
/// terminals; the key hints live in the body block title.
fn draw_header(f: &mut Frame, state: &MonitorState, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = vec![Span::raw(" ")];
    for (i, (tab, label)) in TABS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" \u{00b7} ", dim));
        }
        spans.push(Span::styled(
            format!(" {label} "),
            crate::tui::tab_label_style(state.tab == *tab),
        ));
    }
    let age = state.last_update.map_or_else(
        || "waiting".to_string(),
        |t| format!("{}s ago", t.elapsed().as_secs()),
    );
    spans.push(Span::styled(
        format!(
            "  {} \u{00b7} {}s \u{00b7} {age}",
            state.daemon_url, state.refresh_secs
        ),
        dim,
    ));
    if state.stale {
        spans.push(Span::styled(
            " [STALE]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Shared tab preamble guard: when no snapshot has arrived yet, push
/// the waiting hint and let the caller return its header-only lines.
fn snapshot_or_waiting<'a>(
    latest: Option<&'a Snapshot>,
    lines: &mut Vec<Line<'static>>,
) -> Option<&'a Snapshot> {
    if latest.is_none() {
        lines.push(Line::from(Span::styled(
            "Waiting for the first snapshot from /api/export/report...",
            Style::default().fg(Color::DarkGray),
        )));
    }
    latest
}

/// Body of the Advisor tab: the daemon's settings-advisor hints
/// (`warning_details`). Each entry is `[kind] message`, color-coded by
/// kind. Both fields are sanitized for the terminal, matching the other
/// daemon-sourced strings the TUIs render.
fn build_advisor_lines(latest: Option<&Snapshot>) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Settings advisor",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Config tuning hints the daemon emits when a setting looks undersized for the load.",
            dim,
        )),
        Line::from(""),
    ];
    let Some(snapshot) = snapshot_or_waiting(latest, &mut lines) else {
        return lines;
    };
    if snapshot.warning_details.is_empty() && snapshot.warnings.is_empty() {
        lines.push(Line::from(Span::styled(
            "No hints: the daemon reports no undersized setting.",
            dim,
        )));
        return lines;
    }
    for w in &snapshot.warning_details {
        lines.push(Line::from(vec![
            Span::raw("  ["),
            Span::styled(
                sanitize_for_terminal(&w.kind).into_owned(),
                Style::default().fg(warning_kind_color(&w.kind)),
            ),
            Span::raw("] "),
            Span::raw(sanitize_for_terminal(&w.message).into_owned()),
        ]));
    }
    if snapshot.warning_details.is_empty() {
        // Pre-0.5.19 daemons only carry the legacy free-text field;
        // renderers fall back to it, matching report/mod.rs.
        for w in &snapshot.warnings {
            lines.push(Line::from(Span::raw(format!(
                "  {}",
                sanitize_for_terminal(w)
            ))));
        }
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
        Line::from(Span::styled("Energy / carbon mix", bold)),
        Line::from(Span::styled(
            "Effective source per service, grid intensity per region (cold vs hot).",
            dim,
        )),
        Line::from(""),
    ];
    let Some(snapshot) = snapshot_or_waiting(latest, &mut lines) else {
        return lines;
    };
    let gs = &snapshot.green_summary;
    if gs.per_service_energy_kwh.is_empty() && gs.regions.is_empty() {
        lines.push(Line::from(Span::styled(
            "No energy/carbon data (green scoring disabled, or no events analyzed yet).",
            dim,
        )));
        return lines;
    }

    lines.push(Line::from(vec![
        Span::styled("Window energy: ", dim),
        Span::raw(format!("{} kWh", fmt_kwh(gs.energy_kwh))),
        Span::styled(
            format!("   model: {}", truncate_cell(&gs.energy_model, 32)),
            dim,
        ),
    ]));
    lines.push(Line::from(""));

    if !gs.per_service_energy_kwh.is_empty() {
        lines.push(Line::from(Span::styled("By service", bold)));
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
            // "-" when the daemon did not report a ratio for the service:
            // a fabricated 0% would read as "measured, nothing matched".
            let meas = gs
                .per_service_measured_ratio
                .get(svc)
                .map_or_else(|| "-".to_string(), |r| format!("{:.0}%", r * 100.0));
            let co2 = gs
                .per_service_carbon_kgco2eq
                .get(svc)
                .copied()
                .unwrap_or(0.0);
            lines.push(Line::from(Span::raw(format!(
                "  {:<22} {:<14} {:<16} {:>6}  {:>12}  {:.9}",
                truncate_cell(svc, 22),
                truncate_cell(region, 14),
                truncate_cell(model, 16),
                meas,
                fmt_kwh(*kwh),
                co2,
            ))));
        }
        lines.push(Line::from(""));
    }

    if !gs.regions.is_empty() {
        lines.push(Line::from(Span::styled("By region", bold)));
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

/// Body of the Scrapers tab: live health of the energy/intensity
/// backends from `/api/energy`. One row per backend: configured, last
/// scrape age, scrape counters. Degrades to a hint when the endpoint is
/// unavailable (daemon predating it).
fn build_scrapers_lines(latest: Option<&Snapshot>) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled("Energy scrapers", bold)),
        Line::from(Span::styled(
            "Live health of the measured-energy and grid-intensity backends.",
            dim,
        )),
        Line::from(""),
    ];
    let Some(snapshot) = snapshot_or_waiting(latest, &mut lines) else {
        return lines;
    };
    let Some(scrapers) = snapshot.scrapers.as_ref() else {
        lines.push(Line::from(Span::styled(
            "/api/energy unavailable (daemon predates the endpoint?). Scraper freshness is also on /metrics.",
            dim,
        )));
        return lines;
    };

    lines.push(Line::from(Span::styled(
        format!(
            "  {:<18} {:<12} {:>10} {:>8} {:>8}",
            "backend", "configured", "age (s)", "ok", "failed"
        ),
        dim,
    )));
    let fmt_u64 = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |n| n.to_string());
    for b in &scrapers.backends {
        let configured = if b.configured { "yes" } else { "no" };
        let age = b
            .last_scrape_age_seconds
            .map_or_else(|| "-".to_string(), |a| format!("{a:.0}"));
        let style = if b.configured { Style::default() } else { dim };
        lines.push(Line::from(Span::styled(
            format!(
                "  {:<18} {:<12} {:>10} {:>8} {:>8}",
                truncate_cell(&b.backend, 18),
                configured,
                age,
                fmt_u64(b.scrapes_ok),
                fmt_u64(b.scrapes_failed),
            ),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "electricity_maps has no freshness gauge: its liveness shows as",
        dim,
    )));
    lines.push(Line::from(Span::styled(
        "RealTime intensity sources on the Energy tab.",
        dim,
    )));
    lines
}

/// Color for an advisor hint by its stable `kind`. `tuning` is the
/// actionable yellow, `ingestion_drops` the louder red (data was lost),
/// `cold_start` dim (transient), anything else neutral.
fn warning_kind_color(kind: &str) -> Color {
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

/// Format an energy value: six fixed decimals down to `1e-5` kWh,
/// scientific notation below so a tiny-but-real window does not
/// collapse to a misleading `0.000000`.
fn fmt_kwh(kwh: f64) -> String {
    if kwh == 0.0 || kwh >= 1e-5 {
        format!("{kwh:.6}")
    } else {
        format!("{kwh:.3e}")
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
    use crate::tui::line_text;

    fn snapshot_with_warnings(warning_details: Vec<Warning>) -> Snapshot {
        Snapshot {
            green_summary: GreenSummary::disabled(0),
            warning_details,
            warnings: Vec::new(),
            scrapers: None,
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
            warnings: Vec::new(),
            scrapers: None,
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
    fn warning_kind_color_maps_kinds() {
        assert_eq!(warning_kind_color("tuning"), Color::Yellow);
        assert_eq!(warning_kind_color("ingestion_drops"), Color::Red);
        assert_eq!(warning_kind_color("cold_start"), Color::DarkGray);
        assert_eq!(warning_kind_color("something_else"), Color::Gray);
    }

    #[test]
    fn fmt_kwh_switches_to_scientific_below_floor() {
        assert_eq!(fmt_kwh(1.6), "1.600000");
        assert_eq!(fmt_kwh(0.0), "0.000000");
        assert_eq!(fmt_kwh(1e-5), "0.000010");
        let tiny = fmt_kwh(3.2e-7);
        assert!(tiny.contains('e'), "got: {tiny}");
        assert!(!tiny.starts_with("0.000000"), "got: {tiny}");
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
        assert_eq!(state.tab, Tab::Scrapers);
        state.cycle_tab(true);
        assert_eq!(state.tab, Tab::Advisor, "Tab wraps back");
        state.cycle_tab(false);
        assert_eq!(state.tab, Tab::Scrapers, "Shift-Tab wraps the other way");
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
}
