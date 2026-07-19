//! `perf-sentinel query monitor`: live operator TUI over the daemon.
//!
//! Four tabs cycled with Tab: `Advisor` (the daemon's `warning_details`
//! settings hints), `Energy` (the effective energy/carbon mix per
//! service and per region), `Trends` (braille charts of the energy and
//! carbon per window plus runtime gauges as a share of their configured
//! caps) and `Scrapers` (live health of the energy backends from
//! `/api/energy`). A background task polls the daemon on a fixed
//! interval; when it becomes unreachable the last good snapshot stays
//! on screen with a stale indicator instead of going blank.
//!
//! Deliberately separate from the `inspect` drill-down TUI: `inspect`
//! is the developer's trace/finding browser, this is the operator's
//! deployment monitor. The data here (config hints, source provenance,
//! per-region intensities) is categorical and high-cardinality, which
//! is exactly what the bounded-label rule keeps off `/metrics`.

#![cfg(all(feature = "daemon", feature = "tui"))]

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Borders, Chart, Clear, Dataset, GraphType, Paragraph, Wrap};
use sentinel_core::config::DaemonConfig;
use sentinel_core::daemon::query_api::EnergyStatusResponse;
use sentinel_core::report::{GreenSummary, Warning};
use sentinel_core::score::carbon::IntensitySource;
use sentinel_core::text_safety::sanitize_for_terminal;
use tokio::sync::mpsc;

// `Axis` aliased: ratatui's chart `Axis` is already in scope here.
use crate::tui_resize::{
    Axis as DragAxis, DragTarget, MIN_PCT, boundary_cell, in_range, near, pos_to_pct, set_cut,
};

/// How often the sync event loop wakes up to drain fresh snapshots and
/// repaint (the header age counter included). Keystrokes interrupt the
/// wait immediately, so this only bounds the repaint latency of data
/// that arrived between keys.
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Per-request timeout of the background poller.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Depth of the Trends history ring: one point per successful poll
/// tick, so the plotted span is `TREND_CAPACITY x --refresh` (20
/// minutes at the default 5 s). Bounded so an always-on monitor cannot
/// grow without limit.
const TREND_CAPACITY: usize = 240;

/// The settings advisor flags a gauge at 90% of its cap
/// (`TUNING_ACTIVE_TRACES_RATIO` daemon-side); the headroom chart draws
/// the same threshold so the curve shows what the hint says.
const ADVISOR_THRESHOLD_PCT: f64 = 90.0;

/// Carbon legend bullet, the "true" vivid green the carbon curve should
/// read as.
const CARBON_BULLET: Color = Color::Rgb(0x27, 0xBE, 0x6E);

/// Carbon CURVE color, deliberately brighter and more saturated than
/// [`CARBON_BULLET`]. VHS renders braille as sub-cell dots blended into
/// the dark background, which drains a pure green toward gray (yellow,
/// having two bright channels, survives; green does not). Feeding the
/// curve an oversaturated green makes the braille dots land near the
/// bullet's vivid green instead of a dull olive.
const CARBON_CURVE: Color = Color::Rgb(0x00, 0xF5, 0x66);

/// The monitor's tabs, cycled with Tab/Shift-Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Advisor,
    Energy,
    Trends,
    Scrapers,
    Config,
}

const TABS: [(Tab, &str); 5] = [
    (Tab::Advisor, "Advisor"),
    (Tab::Energy, "Energy"),
    (Tab::Trends, "Trends"),
    (Tab::Scrapers, "Scrapers"),
    (Tab::Config, "Config"),
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

/// Partial deserialization target for `/api/status`: only the gauge and
/// capacity fields the Trends headroom chart plots. The capacity fields
/// are 0.8.8 additions; `default` keeps older daemons parseable, and a
/// zero cap reads as "unknown" and suppresses the ratio.
#[derive(serde::Deserialize)]
struct StatusSlim {
    active_traces: u64,
    #[serde(default)]
    max_active_traces: u64,
    #[serde(default)]
    analysis_queue_depth: i64,
    #[serde(default)]
    analysis_queue_capacity: u64,
    stored_findings: u64,
    #[serde(default)]
    max_retained_findings: u64,
}

/// Deserialization target for `/api/config`: the daemon's effective
/// `[daemon]` config, read-only. Mirrors the daemon's `ConfigResponse`
/// (secrets are already summarized to booleans server-side). All fields
/// `#[serde(default)]` so a daemon predating the endpoint (404) or a
/// future field never breaks parsing.
// Independent config flags mirrored from the daemon, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(serde::Deserialize, Default)]
struct ConfigSlim {
    #[serde(default)]
    listen_addr: String,
    #[serde(default)]
    listen_port: u16,
    #[serde(default)]
    listen_port_grpc: u16,
    #[serde(default)]
    json_socket: String,
    #[serde(default)]
    max_active_traces: usize,
    #[serde(default)]
    trace_ttl_ms: u64,
    #[serde(default)]
    sampling_rate: f64,
    #[serde(default)]
    max_events_per_trace: usize,
    #[serde(default)]
    max_payload_size: usize,
    #[serde(default)]
    environment: String,
    #[serde(default)]
    max_retained_findings: usize,
    #[serde(default)]
    ingest_queue_capacity: usize,
    #[serde(default)]
    analysis_queue_capacity: usize,
    #[serde(default)]
    api_enabled: bool,
    #[serde(default)]
    tls_configured: bool,
    #[serde(default)]
    ack_enabled: bool,
    #[serde(default)]
    ack_api_key_set: bool,
    #[serde(default)]
    cors_allowed_origins: Vec<String>,
    #[serde(default)]
    archive_configured: bool,
    #[serde(default)]
    correlation_enabled: bool,
    #[serde(default)]
    correlation_window_ms: u64,
    #[serde(default)]
    correlation_lag_threshold_ms: u64,
    #[serde(default)]
    correlation_min_co_occurrences: u32,
    #[serde(default)]
    correlation_min_confidence: f64,
    #[serde(default)]
    correlation_max_tracked_pairs: usize,
}

/// One successful poll tick, reduced to the fields the monitor renders.
/// `scrapers` and `status` are `None` when their endpoint is
/// unavailable, independently of the report fetch.
struct Snapshot {
    green_summary: GreenSummary,
    warning_details: Vec<Warning>,
    warnings: Vec<String>,
    scrapers: Option<EnergyStatusResponse>,
    status: Option<StatusSlim>,
    config: Option<ConfigSlim>,
}

/// One sample of the Trends time series, recorded per successful poll
/// tick. The percentages are `None` when `/api/status` was unavailable
/// on that tick or the daemon predates the capacity fields.
struct TrendPoint {
    energy_kwh: f64,
    carbon_gco2: f64,
    traces_pct: Option<f64>,
    queue_pct: Option<f64>,
    findings_pct: Option<f64>,
}

/// Reduce a snapshot to its trend sample. Carbon is the sum of the
/// per-region operational CO2 (grams), the same window scope as
/// `energy_kwh`.
#[allow(clippy::cast_precision_loss)] // gauge values are far below 2^52
fn trend_point(s: &Snapshot) -> TrendPoint {
    let gs = &s.green_summary;
    let pct = |value: f64, cap: u64| {
        if cap == 0 {
            None
        } else {
            Some((value / cap as f64 * 100.0).clamp(0.0, 100.0))
        }
    };
    let st = s.status.as_ref();
    TrendPoint {
        energy_kwh: gs.energy_kwh,
        carbon_gco2: gs.regions.iter().map(|r| r.co2_gco2).sum(),
        traces_pct: st.and_then(|st| pct(st.active_traces as f64, st.max_active_traces)),
        queue_pct: st.and_then(|st| {
            // The depth gauge cannot legitimately go negative; clamp
            // defensively since it travels as a signed Prometheus value.
            pct(
                st.analysis_queue_depth.max(0) as f64,
                st.analysis_queue_capacity,
            )
        }),
        findings_pct: st.and_then(|st| pct(st.stored_findings as f64, st.max_retained_findings)),
    }
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
    /// Trends history ring: one [`TrendPoint`] per successful poll
    /// tick, capped at [`TREND_CAPACITY`]. Failed polls add nothing,
    /// the curve freezes alongside the `[STALE]` banner.
    history: VecDeque<TrendPoint>,
    /// Something visible changed (snapshot, tab, scroll): repaint on
    /// the next loop turn. The header age repaints on its own clock.
    dirty: bool,
    /// Trends-tab split ratios (percentages summing to 100): `rows` is the
    /// charts/headroom vertical split, `cols` the Energy/Carbon horizontal
    /// split. Drag-adjustable in mouse mode, reset by `r`, not persisted.
    trends_rows: [u16; 2],
    trends_cols: [u16; 2],
    /// Mouse capture toggle (`m`); off preserves native copy-paste.
    mouse_mode: bool,
    /// Border being dragged, set on mouse-down over a Trends border.
    drag: Option<DragTarget>,
    /// Border under the cursor, set on motion. Drives the resize highlight.
    hover: Option<DragTarget>,
    /// Trends chart area from the last frame, for drag hit-testing.
    trends_area: std::cell::Cell<Rect>,
}

/// Default Trends split ratios, also the `r`-reset target.
const TRENDS_SPLIT_DEFAULT: [u16; 2] = [50, 50];

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
            history: VecDeque::new(),
            dirty: true,
            trends_rows: TRENDS_SPLIT_DEFAULT,
            trends_cols: TRENDS_SPLIT_DEFAULT,
            mouse_mode: false,
            drag: None,
            hover: None,
            trends_area: std::cell::Cell::new(Rect::default()),
        }
    }

    /// Flip mouse mode and, when turning it off, cancel any in-progress
    /// drag. The terminal capture side-effect is applied by the caller via
    /// [`crate::tui::set_mouse_capture`], keeping this pure and testable.
    fn toggle_mouse_mode(&mut self) {
        self.mouse_mode = !self.mouse_mode;
        if !self.mouse_mode {
            self.drag = None;
            self.hover = None;
        }
        // Repaint so the [MOUSE] marker / Trends hint shows at once; the
        // repaint is otherwise gated on `dirty` or the per-second age tick.
        self.dirty = true;
    }

    /// Reset the Trends split ratios to their defaults (`r`).
    fn reset_layout(&mut self) {
        self.trends_rows = TRENDS_SPLIT_DEFAULT;
        self.trends_cols = TRENDS_SPLIT_DEFAULT;
        self.dirty = true;
    }

    /// Border (if any) under the cursor, using the last drawn Trends area.
    fn hit_test(&self, col: u16, row: u16) -> Option<DragTarget> {
        let area = self.trends_area.get();
        if area.width == 0 || area.height == 0 {
            return None;
        }
        // The horizontal border lives in the top (charts) row; checked
        // before the vertical border so the vertical ±1 tolerance can't
        // shadow the top row's bottom cell.
        let top_h = u16::try_from(u32::from(area.height) * u32::from(self.trends_rows[0]) / 100)
            .unwrap_or(area.height);
        if in_range(row, area.y, top_h)
            && near(col, boundary_cell(&self.trends_cols, 0, area.x, area.width))
        {
            return Some(DragTarget {
                axis: DragAxis::Horizontal,
                boundary: 0,
            });
        }
        let vy = boundary_cell(&self.trends_rows, 0, area.y, area.height);
        if near(row, vy) && in_range(col, area.x, area.width) {
            return Some(DragTarget {
                axis: DragAxis::Vertical,
                boundary: 0,
            });
        }
        None
    }

    /// Mouse-down: start dragging a Trends border under the cursor.
    fn begin_drag(&mut self, col: u16, row: u16) {
        self.drag = self.hit_test(col, row);
    }

    /// Mouse-drag: move the active Trends border to the cursor.
    fn apply_drag(&mut self, col: u16, row: u16) {
        let Some(target) = self.drag else {
            return;
        };
        let area = self.trends_area.get();
        match target.axis {
            DragAxis::Vertical => set_cut(
                &mut self.trends_rows,
                target.boundary,
                pos_to_pct(row, area.y, area.height),
                MIN_PCT,
            ),
            DragAxis::Horizontal => set_cut(
                &mut self.trends_cols,
                target.boundary,
                pos_to_pct(col, area.x, area.width),
                MIN_PCT,
            ),
        }
    }

    /// Recompute the cached body line counts for all tabs. Called once
    /// per applied snapshot. Entries follow TABS order; Trends renders
    /// charts (no scroll), so its count stays 0.
    fn refresh_line_counts(&mut self) {
        let latest = self.latest.as_ref();
        let count = |lines: Vec<Line<'static>>| u16::try_from(lines.len()).unwrap_or(u16::MAX);
        self.line_counts = [
            count(build_advisor_lines(latest)),
            count(build_energy_lines(latest)),
            0,
            count(build_scrapers_lines(latest)),
            count(build_config_lines(latest)),
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
                // Config is static for the daemon's lifetime, so a transient
                // /api/config failure must not flip the Config tab to the
                // misleading old-daemon hint: carry the previous value
                // forward, same rationale as scrapers above.
                if s.config.is_none()
                    && let Some(prev) = self.latest.as_mut().and_then(|p| p.config.take())
                {
                    s.config = Some(prev);
                }
                // No carry-forward for the trend sample: a tick without
                // /api/status yields a point with absent percentages
                // rather than a fabricated repeat of stale gauges.
                self.history.push_back(trend_point(&s));
                if self.history.len() > TREND_CAPACITY {
                    self.history.pop_front();
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
        // Leaving Trends invalidates the hovered/dragged border, so a later
        // return doesn't paint a phantom highlight with no cursor on it.
        self.drag = None;
        self.hover = None;
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

    /// Scroll one line up, repainting only if the position actually moved.
    fn scroll_up(&mut self) {
        let prev = self.scroll;
        self.scroll = self.scroll.saturating_sub(1);
        self.dirty = self.dirty || self.scroll != prev;
    }

    /// Scroll one line down, clamped to the active tab's last line.
    fn scroll_down(&mut self) {
        let max = self.line_count().saturating_sub(1);
        if self.scroll < max {
            self.scroll = self.scroll.saturating_add(1);
            self.dirty = true;
        }
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
    // Concurrent: the tick latency is the slowest of the three
    // requests, not their sum. The energy and status fetches are
    // best-effort: `None` covers both a daemon predating the endpoint
    // fields and a transient failure; `apply` carries the previous
    // scraper table forward so only the former shows the old-daemon
    // hint persistently.
    let (report, scrapers, status, config) = tokio::join!(
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
        crate::query::fetch_json::<StatusSlim>(client, base_url, "/api/status", FETCH_TIMEOUT),
        crate::query::fetch_json::<ConfigSlim>(client, base_url, "/api/config", FETCH_TIMEOUT),
    );
    match report {
        Some(report) => FetchOutcome::Snapshot(Box::new(Snapshot {
            green_summary: report.green_summary,
            warning_details: report.warning_details,
            warnings: report.warnings,
            scrapers,
            status,
            config,
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
        if handle_event(state, &event::read()?) {
            return Ok(());
        }
    }
}

/// Apply one input event to the monitor state. Returns `true` when the
/// user asked to quit.
fn handle_event(state: &mut MonitorState, event: &Event) -> bool {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => return handle_key(state, key.code),
        // Only on the Trends tab (the only resizable layout): elsewhere the
        // stored area is stale. Repaint only when the drag state actually
        // changed, so bare motion events don't defeat the dirty throttle.
        Event::Mouse(me) if state.mouse_mode && state.tab == Tab::Trends => {
            if handle_mouse(state, *me) {
                state.dirty = true;
            }
        }
        Event::Resize(_, _) => state.dirty = true,
        _ => {}
    }
    false
}

/// Route a mouse event to the Trends border-drag state machine. Returns
/// true when something visible changed (and thus needs a repaint), so bare
/// motion that doesn't cross a border doesn't defeat the dirty throttle.
fn handle_mouse(state: &mut MonitorState, me: MouseEvent) -> bool {
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            state.begin_drag(me.column, me.row);
            true
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            state.apply_drag(me.column, me.row);
            true
        }
        MouseEventKind::Up(MouseButton::Left) => {
            state.drag = None;
            state.hover = state.hit_test(me.column, me.row);
            true
        }
        MouseEventKind::Moved => {
            let next = state.hit_test(me.column, me.row);
            if next == state.hover {
                return false;
            }
            state.hover = next;
            true
        }
        _ => false,
    }
}

/// Apply one key press to the monitor state. Returns `true` when the
/// user asked to quit (`q` or `Esc`).
fn handle_key(state: &mut MonitorState, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Tab => state.cycle_tab(true),
        KeyCode::BackTab => state.cycle_tab(false),
        KeyCode::Up | KeyCode::Char('k') => state.scroll_up(),
        KeyCode::Down | KeyCode::Char('j') => state.scroll_down(),
        KeyCode::Char('m') => {
            state.toggle_mouse_mode();
            crate::tui::set_mouse_capture(state.mouse_mode);
        }
        // Reset only the Trends layout, and only from that tab, so `r`
        // can't silently discard it from an unrelated tab.
        KeyCode::Char('r') if state.tab == Tab::Trends => state.reset_layout(),
        _ => {}
    }
    false
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
        // Charts, not text: no scroll, dedicated renderer.
        Tab::Trends => {
            draw_trends(f, state, outer[1]);
            return;
        }
        Tab::Scrapers => (
            " Scrapers \u{00b7} Tab \u{21c4} \u{00b7} j/k \u{2195} \u{00b7} q ",
            build_scrapers_lines(state.latest.as_ref()),
            false,
        ),
        // Wrap on: the per-parameter descriptions are prose.
        Tab::Config => (
            " Config \u{00b7} Tab \u{21c4} \u{00b7} j/k \u{2195} \u{00b7} q ",
            build_config_lines(state.latest.as_ref()),
            true,
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
    let dim = crate::tui::dim_style();
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
    // Global reminder that mouse capture is on (copy-paste grabbed),
    // since it can be toggled from any tab even though only Trends resizes.
    if state.mouse_mode {
        spans.push(Span::styled(
            " [MOUSE]",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
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
            crate::tui::dim_style(),
        )));
    }
    latest
}

/// Body of the Advisor tab: the daemon's settings-advisor hints
/// (`warning_details`). Each entry is `[kind] message`, color-coded by
/// kind. Both fields are sanitized for the terminal, matching the other
/// daemon-sourced strings the TUIs render.
fn build_advisor_lines(latest: Option<&Snapshot>) -> Vec<Line<'static>> {
    let dim = crate::tui::dim_style();
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
                warning_kind_style(&w.kind),
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
    let dim = crate::tui::dim_style();
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
    if gs.per_service_energy_kwh.is_empty() && gs.regions.is_empty() && gs.database_waste.is_none()
    {
        lines.push(Line::from(Span::styled(
            "No energy/carbon data (green scoring disabled, or no events analyzed yet).",
            dim,
        )));
        return lines;
    }

    lines.push(Line::from(vec![
        Span::styled("Window energy: ", dim),
        Span::raw(format!("{} kWh", fmt_tiny(gs.energy_kwh))),
        Span::styled(
            format!("   model: {}", truncate_cell(&gs.energy_model, 32)),
            dim,
        ),
    ]));
    if let Some(db) = &gs.database_waste {
        let gco2 = db
            .waste_gco2
            .map_or_else(|| "-".to_string(), |g| format!("{} gCO2", fmt_tiny(g)));
        // Daemon-sourced string: sanitize + cap like every other cell.
        let region = truncate_cell(db.region.as_deref().unwrap_or("-"), 24);
        let model = truncate_cell(if db.model.is_empty() { "-" } else { &db.model }, 24);
        lines.push(Line::from(vec![
            Span::styled("Database waste: ", dim),
            Span::raw(format!(
                "{} kWh of {} kWh ({:.0}% SQL ratio)   {gco2}",
                fmt_tiny(db.waste_kwh),
                fmt_tiny(db.energy_kwh),
                db.sql_waste_ratio * 100.0,
            )),
            Span::styled(
                format!(
                    "   model {model}   region {region}   {}",
                    if db.model == "alumet_rapl" {
                        "excluded from totals"
                    } else {
                        "within the report totals"
                    }
                ),
                dim,
            ),
        ]));
    }
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
                fmt_tiny(*kwh),
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
    let dim = crate::tui::dim_style();
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

/// Append one parameter row to the Config tab: a value line
/// (`name = current  (default <d>[, modified])`, the suffix colored when
/// the running value differs from the default) and a dim description
/// line below it.
fn config_row(
    lines: &mut Vec<Line<'static>>,
    name: &str,
    current: &str,
    default: &str,
    desc: &str,
) {
    let dim = crate::tui::dim_style();
    // `current` comes straight from the daemon's /api/config JSON for the
    // string-valued rows (listen_addr, environment, ...). A hostile or
    // compromised daemon could embed ANSI/BiDi sequences, so sanitize
    // before it reaches the terminal, same as every other tab.
    let current = sanitize_for_terminal(current);
    let modified = current.as_ref() != default;
    let suffix = if modified {
        Span::styled(
            format!("  (default {default}, modified)"),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::styled(format!("  (default {default})"), dim)
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("{name} = "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(current.into_owned()),
        suffix,
    ]));
    lines.push(Line::from(Span::styled(format!("    {desc}"), dim)));
}

/// Render a byte count as a compact MiB string for the config view.
#[allow(clippy::cast_precision_loss)]
fn fmt_mib(bytes: usize) -> String {
    format!("{:.0} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Body of the Config tab: the daemon's effective `[daemon]` settings
/// (from `/api/config`), each with its current value, the compiled-in
/// default (computed locally from `DaemonConfig::default`), and a
/// one-line explanation. Read-only. Secrets are already summarized to
/// booleans server-side, never shown in clear.
#[allow(clippy::too_many_lines)] // one straight-line row per parameter
fn build_config_lines(latest: Option<&Snapshot>) -> Vec<Line<'static>> {
    let dim = crate::tui::dim_style();
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled("Daemon configuration", bold)),
        Line::from(Span::styled(
            "Effective [daemon] settings (read-only), with the compiled-in default and what each does."
                .to_string(),
            dim,
        )),
        Line::from(""),
    ];
    let Some(snapshot) = snapshot_or_waiting(latest, &mut lines) else {
        return lines;
    };
    let Some(c) = snapshot.config.as_ref() else {
        lines.push(Line::from(Span::styled(
            "/api/config unavailable (daemon predates the endpoint?). Upgrade the daemon to 0.8.8+."
                .to_string(),
            dim,
        )));
        return lines;
    };
    let d = DaemonConfig::default();

    let bool_str = |b: bool| if b { "yes" } else { "no" }.to_string();

    config_row(
        &mut lines,
        "max_active_traces",
        &c.max_active_traces.to_string(),
        &d.max_active_traces.to_string(),
        "Cap of the in-memory correlation window; the oldest trace is evicted (LRU) past it. The advisor hints at 90%.",
    );
    config_row(
        &mut lines,
        "trace_ttl_ms",
        &c.trace_ttl_ms.to_string(),
        &d.trace_ttl_ms.to_string(),
        "How long a trace waits for more spans before it is evicted and analyzed (ms).",
    );
    config_row(
        &mut lines,
        "sampling_rate",
        &format!("{:.2}", c.sampling_rate),
        &format!("{:.2}", d.sampling_rate),
        "Fraction of incoming traces analyzed (0.0-1.0); lower it to shed load under heavy traffic.",
    );
    config_row(
        &mut lines,
        "max_events_per_trace",
        &c.max_events_per_trace.to_string(),
        &d.max_events_per_trace.to_string(),
        "Ring-buffer size per trace; oldest spans drop once exceeded.",
    );
    config_row(
        &mut lines,
        "max_payload_size",
        &fmt_mib(c.max_payload_size),
        &fmt_mib(d.max_payload_size),
        "Largest JSON payload the daemon will deserialize from one request.",
    );
    config_row(
        &mut lines,
        "ingest_queue_capacity",
        &c.ingest_queue_capacity.to_string(),
        &d.ingest_queue_capacity.to_string(),
        "Span-event batches buffered between listeners and the event loop; full applies backpressure (OTLP 503).",
    );
    config_row(
        &mut lines,
        "analysis_queue_capacity",
        &c.analysis_queue_capacity.to_string(),
        &d.analysis_queue_capacity.to_string(),
        "Batches awaiting detect+score; full sheds whole batches (perf_sentinel_analysis_shed_*).",
    );
    config_row(
        &mut lines,
        "max_retained_findings",
        &c.max_retained_findings.to_string(),
        &d.max_retained_findings.to_string(),
        "Findings kept in the query ring buffer; oldest evicted past it.",
    );
    config_row(
        &mut lines,
        "environment",
        &c.environment,
        d.environment.as_str(),
        "Deployment label stamped on findings as a Confidence (staging = medium, production = high).",
    );
    config_row(
        &mut lines,
        "api_enabled",
        &bool_str(c.api_enabled),
        &bool_str(d.api_enabled),
        "Whether the daemon query API (/api/*) is served at all.",
    );
    config_row(
        &mut lines,
        "listen_addr",
        &c.listen_addr,
        &d.listen_addr,
        "Bind address for OTLP and /metrics. A non-loopback value exposes unauthenticated endpoints.",
    );
    config_row(
        &mut lines,
        "listen_port",
        &c.listen_port.to_string(),
        &d.listen_port.to_string(),
        "OTLP HTTP receiver and /metrics port.",
    );
    config_row(
        &mut lines,
        "listen_port_grpc",
        &c.listen_port_grpc.to_string(),
        &d.listen_port_grpc.to_string(),
        "OTLP gRPC receiver port.",
    );
    config_row(
        &mut lines,
        "json_socket",
        &c.json_socket,
        &d.json_socket,
        "Unix domain socket path for native NDJSON event ingestion.",
    );

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Sub-systems", bold)));
    lines.push(Line::from(""));
    config_row(
        &mut lines,
        "tls",
        if c.tls_configured {
            "configured"
        } else {
            "not configured"
        },
        "not configured",
        "TLS for the OTLP listeners (cert/key paths summarized; never shown).",
    );
    config_row(
        &mut lines,
        "ack_enabled",
        &bool_str(c.ack_enabled),
        &bool_str(d.ack.enabled),
        "Daemon-side acknowledgment store (JSONL persistence + ack HTTP routes).",
    );
    config_row(
        &mut lines,
        "ack_api_key",
        if c.ack_api_key_set { "set" } else { "unset" },
        "unset",
        "Whether the ack mutation routes require an X-API-Key (the key itself is never exposed).",
    );
    config_row(
        &mut lines,
        "cors_allowed_origins",
        &if c.cors_allowed_origins.is_empty() {
            "(none)".to_string()
        } else {
            c.cors_allowed_origins.join(", ")
        },
        "(none)",
        "Origins allowed by the HTTP API CORS layer; empty emits no CORS headers.",
    );
    config_row(
        &mut lines,
        "archive",
        if c.archive_configured {
            "configured"
        } else {
            "not configured"
        },
        "not configured",
        "Per-window Report NDJSON archive writer consumed by `perf-sentinel disclose`.",
    );

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Correlation", bold)));
    lines.push(Line::from(""));
    let cd = d.correlation;
    config_row(
        &mut lines,
        "correlation.enabled",
        &bool_str(c.correlation_enabled),
        &bool_str(cd.enabled),
        "Whether the cross-trace correlator runs; off by default, the fields below apply only when on.",
    );
    config_row(
        &mut lines,
        "correlation.window_ms",
        &c.correlation_window_ms.to_string(),
        &cd.window_ms.to_string(),
        "Rolling window (ms) over which finding co-occurrences are tracked.",
    );
    config_row(
        &mut lines,
        "correlation.lag_threshold_ms",
        &c.correlation_lag_threshold_ms.to_string(),
        &cd.lag_threshold_ms.to_string(),
        "Max delay (ms) between two findings to count them as co-occurring.",
    );
    config_row(
        &mut lines,
        "correlation.min_co_occurrences",
        &c.correlation_min_co_occurrences.to_string(),
        &cd.min_co_occurrences.to_string(),
        "Minimum co-occurrence count before a correlation is reported.",
    );
    config_row(
        &mut lines,
        "correlation.min_confidence",
        &format!("{:.2}", c.correlation_min_confidence),
        &format!("{:.2}", cd.min_confidence),
        "Minimum confidence (co-occurrences / occurrences of A) to report a correlation.",
    );
    config_row(
        &mut lines,
        "correlation.max_tracked_pairs",
        &c.correlation_max_tracked_pairs.to_string(),
        &cd.max_tracked_pairs.to_string(),
        "Cap on tracked finding pairs; lowest-co-occurrence pairs are evicted past it.",
    );

    lines
}

/// Chart-ready `(x, y)` series extracted from the trend history. The x
/// coordinate is the tick index in the full history, so the percentage
/// series stay time-aligned with the energy series even when some
/// ticks lack `/api/status` data.
#[derive(Default)]
struct TrendSeries {
    energy: Vec<(f64, f64)>,
    carbon: Vec<(f64, f64)>,
    traces_pct: Vec<(f64, f64)>,
    queue_pct: Vec<(f64, f64)>,
    findings_pct: Vec<(f64, f64)>,
}

#[allow(clippy::cast_precision_loss)] // tick indices are < TREND_CAPACITY
fn build_trend_series(history: &VecDeque<TrendPoint>) -> TrendSeries {
    let mut s = TrendSeries::default();
    for (i, p) in history.iter().enumerate() {
        let x = i as f64;
        s.energy.push((x, p.energy_kwh));
        s.carbon.push((x, p.carbon_gco2));
        if let Some(v) = p.traces_pct {
            s.traces_pct.push((x, v));
        }
        if let Some(v) = p.queue_pct {
            s.queue_pct.push((x, v));
        }
        if let Some(v) = p.findings_pct {
            s.findings_pct.push((x, v));
        }
    }
    s
}

/// Body of the Trends tab: three braille charts over the poll history.
/// Top row: energy and carbon per window side by side. Bottom: runtime
/// gauges as a percentage of their configured caps, with the settings
/// advisor's threshold drawn in.
fn draw_trends(f: &mut Frame, state: &MonitorState, area: Rect) {
    let resize_hint = if state.mouse_mode {
        " MOUSE drag \u{00b7} r reset \u{00b7} m off \u{00b7} "
    } else {
        " m resize \u{00b7} "
    };
    let outer_block = Block::default()
        .title(format!(
            " Trends \u{00b7} Tab \u{21c4} \u{00b7}{resize_hint}q "
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = outer_block.inner(area);
    f.render_widget(outer_block, area);
    // Default: no draggable area until the charts are actually laid out.
    state.trends_area.set(Rect::default());

    if state.history.len() < 2 {
        // A one-point curve renders as nothing; say so instead.
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(
                    "Collecting trend points ({}/2): one lands per refresh tick ({}s)...",
                    state.history.len(),
                    state.refresh_secs
                ),
                crate::tui::dim_style(),
            ))),
            inner,
        );
        return;
    }

    let series = build_trend_series(&state.history);
    // Stored for the next frame's mouse hit-testing (see `begin_drag`).
    state.trends_area.set(inner);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(state.trends_rows[0]),
            Constraint::Percentage(state.trends_rows[1]),
        ])
        .split(inner);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(state.trends_cols[0]),
            Constraint::Percentage(state.trends_cols[1]),
        ])
        .split(rows[0]);

    // Fixed-width window so the curves scroll at a constant rate instead of
    // compressing as the ring fills: the left edge is always one full ring
    // (TREND_CAPACITY) behind "now". Before the ring fills, that left part
    // of the window is simply empty rather than zoomed-in.
    #[allow(clippy::cast_precision_loss)]
    let x_bounds: [f64; 2] = {
        let hi = (state.history.len() - 1).max(1) as f64;
        [hi - (TREND_CAPACITY - 1) as f64, hi]
    };
    let span_label = format!(
        "-{}",
        fmt_span_secs((TREND_CAPACITY as u64 - 1) * state.refresh_secs)
    );
    draw_metric_chart(
        f,
        top[0],
        " Energy \u{00b7} kWh/window ",
        &series.energy,
        (Color::Yellow, Color::Yellow),
        x_bounds,
        &span_label,
    );
    draw_metric_chart(
        f,
        top[1],
        " Carbon \u{00b7} gCO2e/window ",
        &series.carbon,
        (CARBON_CURVE, CARBON_BULLET),
        x_bounds,
        &span_label,
    );
    draw_headroom_chart(f, rows[1], &series, x_bounds, &span_label);

    // Light up the border under the cursor (or being dragged): a terminal
    // can't change the OS mouse pointer, so this is the grab affordance.
    if state.mouse_mode
        && let Some(t) = state.drag.or(state.hover)
    {
        let hl = crate::tui::resize_highlight_style();
        match t.axis {
            // Divider between the charts row and the Headroom gauge.
            DragAxis::Vertical => {
                crate::tui::highlight_hline(f, inner.x, rows[1].y, inner.width, hl);
            }
            // Shared edge between the Energy and Carbon charts.
            DragAxis::Horizontal => {
                crate::tui::highlight_vline(f, top[1].x, rows[0].y, rows[0].height, hl);
            }
        }
    }
}

/// Style for a trend curve. Bold promotes a named ANSI color to its
/// bright variant on a dark terminal (the normals are muddy there) while
/// staying legible on a light one. Truecolor (`Rgb`) and green are the
/// exceptions: green bolds toward a dull olive on the VHS palette, and
/// `Rgb` values are already picked for their exact rendered shade, so
/// both keep their plain color.
fn curve_style(color: Color) -> Style {
    let style = Style::default().fg(color);
    if matches!(color, Color::Green | Color::LightGreen | Color::Rgb(..)) {
        style
    } else {
        style.add_modifier(Modifier::BOLD)
    }
}

/// Vertical offset, in data units, of one braille sub-row for `area`.
/// Each curve is drawn together with a twin shifted up by this much, so
/// the line reads as a ~2-dot-thick band instead of a faint single dot.
fn thicken_dy(area: Rect, y_span: f64) -> f64 {
    // Chart plot area = height minus the two borders and the x-axis
    // label row; braille packs 4 dots per cell row.
    let plot_rows = f64::from(area.height.saturating_sub(3)).max(1.0);
    y_span / (plot_rows * 4.0)
}

/// Copy a series shifted up by `dy` (its thickening twin).
fn offset_series(data: &[(f64, f64)], dy: f64) -> Vec<(f64, f64)> {
    data.iter().map(|(x, y)| (*x, y + dy)).collect()
}

/// Draw a chart legend by hand in the top-right of `area`, overlaying
/// the chart's plot. ratatui's native legend forces the dataset (curve)
/// color onto the label text, which renders darker than the thick curve
/// itself; here the label uses the terminal's default foreground (light
/// on a dark background, dark on a light one) and a leading colored
/// bullet carries the curve color for identification. `area` is the
/// chart's full rect, borders included.
fn draw_chart_legend(f: &mut Frame, area: Rect, entries: &[(Color, String)]) {
    if entries.is_empty() || area.width < 6 || area.height < 4 {
        return;
    }
    let lines: Vec<Line<'static>> = entries
        .iter()
        .map(|(color, label)| {
            Line::from(vec![
                Span::styled("\u{25cf} ", Style::default().fg(*color)),
                // Explicit Reset, not a bare Span: the legend overlays the
                // plot, and a curve crossing the cell (e.g. the red 90%
                // threshold line) would otherwise tint the label with its
                // leftover foreground. Reset forces the terminal default.
                Span::styled(label.clone(), Style::default().fg(Color::Reset)),
            ])
        })
        .collect();
    #[allow(clippy::cast_possible_truncation)]
    let want_w = (entries
        .iter()
        .map(|(_, l)| l.chars().count() + 2)
        .max()
        .unwrap_or(0) as u16)
        .min(area.width.saturating_sub(2));
    #[allow(clippy::cast_possible_truncation)]
    let want_h = (entries.len() as u16).min(area.height.saturating_sub(2));
    // Inside the border, flush to the top-right corner.
    let rect = Rect {
        x: area.right().saturating_sub(want_w + 1),
        y: area.y + 1,
        width: want_w,
        height: want_h,
    };
    // Clear the cells first so the curves underneath (the threshold line
    // in particular) do not bleed through between glyphs.
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines), rect);
}

/// One single-series braille chart with adaptive Y bounds. The dataset
/// legend carries the latest value so the curve needs no Y cursor. The
/// curve is drawn as two parallel sub-pixel-apart lines for thickness.
fn draw_metric_chart(
    f: &mut Frame,
    area: Rect,
    title: &'static str,
    data: &[(f64, f64)],
    // (curve color, legend bullet color). Usually the same; they differ
    // for carbon, whose braille curve is oversaturated to render as the
    // bullet's vivid green (see CARBON_CURVE).
    colors: (Color, Color),
    x_bounds: [f64; 2],
    span_label: &str,
) {
    let (curve_color, bullet_color) = colors;
    let dim = crate::tui::dim_style();
    let y_max = data.iter().map(|p| p.1).fold(0.0_f64, f64::max);
    let y_top = if y_max > 0.0 { y_max * 1.15 } else { 1.0 };
    let last = data.last().map_or(0.0, |p| p.1);
    let twin = offset_series(data, thicken_dy(area, y_top));
    // Two parallel lines one sub-row apart so the curve reads thick, not
    // a faint single dot. No legend name on the datasets: the legend is
    // drawn by hand below so its text can stay light.
    let datasets = vec![
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(curve_style(curve_color))
            .data(data),
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(curve_style(curve_color))
            .data(&twin),
    ];
    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(dim),
        )
        .x_axis(
            Axis::default()
                .bounds(x_bounds)
                .labels([span_label.to_string(), "now".to_string()])
                .style(dim),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, y_top])
                .labels(["0".to_string(), fmt_tiny(y_top)])
                .style(dim),
        );
    f.render_widget(chart, area);
    draw_chart_legend(
        f,
        area,
        &[(bullet_color, format!("now {}", fmt_tiny(last)))],
    );
}

/// The gauge-vs-cap chart: each runtime gauge as a percentage of its
/// configured cap, plus the advisor threshold as a flat reference line.
/// Degrades to a hint when no tick carried the capacity fields (daemon
/// predating the 0.8.8 `/api/status` additions).
fn draw_headroom_chart(
    f: &mut Frame,
    area: Rect,
    series: &TrendSeries,
    x_bounds: [f64; 2],
    span_label: &str,
) {
    let dim = crate::tui::dim_style();
    if series.traces_pct.is_empty() && series.queue_pct.is_empty() && series.findings_pct.is_empty()
    {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Headroom unavailable: /api/status predates the capacity fields (0.8.8).",
                dim,
            )))
            .block(
                Block::default()
                    .title(" Headroom ")
                    .borders(Borders::ALL)
                    .border_style(dim),
            ),
            area,
        );
        return;
    }

    let threshold = [
        (x_bounds[0], ADVISOR_THRESHOLD_PCT),
        (x_bounds[1], ADVISOR_THRESHOLD_PCT),
    ];
    let last_pct = |s: &[(f64, f64)]| s.last().map_or(0.0, |p| p.1);
    // Thickening twins (one braille sub-row up) for the three gauges; the
    // flat threshold line needs no thickening. The Y span is the fixed
    // 0..100 axis.
    let dy = thicken_dy(area, 100.0);
    let twin_traces = offset_series(&series.traces_pct, dy);
    let twin_queue = offset_series(&series.queue_pct, dy);
    let twin_findings = offset_series(&series.findings_pct, dy);
    // No legend names on the datasets: each gauge is a thick pair (curve
    // + twin one sub-row up), and the legend is drawn by hand below so
    // its text stays light instead of taking the curve color.
    let braille_line = || {
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
    };
    let datasets = vec![
        braille_line()
            .style(curve_style(Color::Yellow))
            .data(&series.traces_pct),
        braille_line()
            .style(curve_style(Color::Yellow))
            .data(&twin_traces),
        braille_line()
            .style(curve_style(Color::LightBlue))
            .data(&series.queue_pct),
        braille_line()
            .style(curve_style(Color::LightBlue))
            .data(&twin_queue),
        braille_line()
            .style(curve_style(Color::Cyan))
            .data(&series.findings_pct),
        braille_line()
            .style(curve_style(Color::Cyan))
            .data(&twin_findings),
        braille_line()
            .style(curve_style(Color::Red))
            .data(&threshold),
    ];
    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .title(" Headroom \u{00b7} % of configured cap ")
                .borders(Borders::ALL)
                .border_style(dim),
        )
        .x_axis(
            Axis::default()
                .bounds(x_bounds)
                .labels([span_label.to_string(), "now".to_string()])
                .style(dim),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .labels(["0", "50", "100%"])
                .style(dim),
        );
    f.render_widget(chart, area);
    draw_chart_legend(
        f,
        area,
        &[
            (
                Color::Yellow,
                format!("active_traces {:.0}%", last_pct(&series.traces_pct)),
            ),
            (
                Color::LightBlue,
                format!("analysis_queue {:.0}%", last_pct(&series.queue_pct)),
            ),
            (
                Color::Cyan,
                format!("findings_store {:.0}%", last_pct(&series.findings_pct)),
            ),
            (Color::Red, "advisor threshold 90%".to_string()),
        ],
    );
}

/// Compact duration label for the chart X axis: seconds under two
/// minutes, whole minutes under two hours, fractional hours above.
fn fmt_span_secs(secs: u64) -> String {
    if secs < 120 {
        format!("{secs}s")
    } else if secs < 7200 {
        format!("{}m", secs / 60)
    } else {
        #[allow(clippy::cast_precision_loss)]
        let hours = secs as f64 / 3600.0;
        format!("{hours:.1}h")
    }
}

/// Style for an advisor hint by its stable `kind`. `tuning` is the
/// actionable yellow, `ingestion_drops` the louder red (data was lost),
/// `cold_start` theme-adaptive dim (transient), anything else neutral.
fn warning_kind_style(kind: &str) -> Style {
    use sentinel_core::report::warnings::{COLD_START, INGESTION_DROPS, TUNING};
    match kind {
        TUNING => Style::default().fg(Color::Yellow),
        INGESTION_DROPS => Style::default().fg(Color::Red),
        COLD_START => crate::tui::dim_style(),
        _ => Style::default().fg(Color::Gray),
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

use crate::render::fmt_tiny;

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
              ],
              "database_waste":{"energy_kwh":0.01,"waste_kwh":0.002,"waste_gco2":0.09,"region":"eu-west-3","sql_waste_ratio":0.2,"model":"alumet_rapl"}
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
        // An empty `regions` carbon sum yields -0.0; it must not render
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
        assert_eq!(state.tab, Tab::Advisor, "Tab wraps back");
        state.cycle_tab(false);
        assert_eq!(state.tab, Tab::Config, "Shift-Tab wraps the other way");
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
        // The slim type has no api_key/cert/key field at all; the tab can
        // only ever render the boolean summaries.
        let mut snapshot = snapshot_with_warnings(Vec::new());
        let mut cfg = full_config();
        cfg.ack_api_key_set = true;
        cfg.tls_configured = true;
        snapshot.config = Some(cfg);
        let text = line_text(&build_config_lines(Some(&snapshot)));
        assert!(text.contains("ack_api_key = set"), "got: {text}");
        assert!(text.contains("tls = configured"), "got: {text}");
    }

    #[test]
    fn config_sanitizes_daemon_controlled_strings() {
        // A hostile daemon (--daemon-url can point anywhere) could embed
        // ANSI/BiDi sequences in string-valued config fields; the Config
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
        state.apply(FetchOutcome::Unreachable);
        assert!(state.stale, "stale flag set on failed poll");
        assert!(
            state.latest.is_some(),
            "last good snapshot must stay on screen"
        );
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

        // q and Esc request quit; every other key returns false.
        assert!(handle_key(&mut state, KeyCode::Char('q')));
        assert!(handle_key(&mut state, KeyCode::Esc));

        // Tab advances the active tab without quitting.
        assert!(!handle_key(&mut state, KeyCode::Tab));
        assert_eq!(state.tab, Tab::Energy);

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
        let old =
            r#"{"version":"0.8.7","uptime_seconds":12,"active_traces":4,"stored_findings":7}"#;
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
}
