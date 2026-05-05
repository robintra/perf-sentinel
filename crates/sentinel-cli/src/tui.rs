//! Terminal UI for interactive trace and finding inspection.
//!
//! Provides a 3-panel layout: traces list, findings for selected trace,
//! and finding detail with span tree.

use std::collections::HashMap;
use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use sentinel_core::correlate::Trace;
#[cfg(feature = "daemon")]
use sentinel_core::daemon::query_api::AckSource;
use sentinel_core::detect::correlate_cross::CrossTraceCorrelation;
use sentinel_core::detect::{DetectConfig, Finding, FindingType, Severity};
use sentinel_core::explain;
use sentinel_core::text_safety::sanitize_for_terminal;
/// Panel that currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Traces,
    Findings,
    Detail,
    Correlations,
}

/// Application state for the TUI.
pub struct App {
    pub traces: Vec<Trace>,
    pub detect_config: DetectConfig,
    /// All findings from the report (owned, flat list).
    all_findings: Vec<Finding>,
    /// Per-trace finding indices into `all_findings`.
    findings_by_trace: Vec<Vec<usize>>,
    trace_ids: Vec<String>,
    trace_index: HashMap<String, usize>,

    pub selected_trace: usize,
    pub selected_finding: usize,
    pub active_panel: Panel,
    pub scroll_offset: u16,
    /// Cached detail tree text per trace: (`trace_idx`, rendered tree).
    cached_detail: Option<(usize, String)>,
    /// Pre-rendered span trees keyed by `trace_id`, populated by callers
    /// that don't have raw spans in memory (e.g. `query inspect` which
    /// fetches trees from the daemon's `/api/explain/{trace_id}` endpoint).
    /// When `Some(text)`, takes precedence over the `detect + build_tree`
    /// path that requires `traces[i].spans` to be populated.
    pre_rendered_trees: HashMap<String, String>,
    /// Cross-trace correlations to display in the Correlations panel.
    /// Empty in batch mode (correlator is daemon-only). Populated by
    /// `query inspect` from `/api/correlations`.
    correlations: Vec<CrossTraceCorrelation>,
    pub selected_correlation: usize,

    /// Daemon URL when running under `query inspect`. `None` in batch
    /// mode (`inspect --input`), which disables `a`/`u` keys.
    #[cfg(feature = "daemon")]
    pub daemon_url: Option<String>,
    /// Resolved API key (env var or `--api-key-file`). `None` is a
    /// legitimate value when the daemon has no `[daemon.ack] api_key`.
    /// Used as the `X-API-Key` header on POST/DELETE ack writes.
    #[cfg(feature = "daemon")]
    pub api_key: Option<String>,
    /// Per-finding ack annotations keyed by signature. Populated at
    /// boot from `FindingResponse.acknowledged_by` and refreshed after
    /// every successful submit by `refetch_acks`.
    #[cfg(feature = "daemon")]
    pub acks_by_signature: HashMap<String, AckSource>,
    /// Modal overlay state for the ack/revoke flow. Hidden when not
    /// active. Drives `draw_ack_modal` and `handle_modal_key`.
    #[cfg(feature = "daemon")]
    pub ack_modal: AckModalState,
}

impl App {
    /// Create a new app from analysis findings and traces.
    #[must_use]
    pub fn new(
        findings: Vec<Finding>,
        mut traces: Vec<Trace>,
        detect_config: DetectConfig,
    ) -> Self {
        // Sort traces by trace_id so the trace list panel has a stable,
        // predictable display order across runs. The upstream `correlate`
        // stage iterates a `HashMap<String, Vec<_>>` which yields traces
        // in randomized hash order, which is fine for batch analysis but
        // makes the interactive TUI non-reproducible (the same input file
        // shows traces in a different order on every launch, breaking
        // muscle memory for users who come back to investigate a trace
        // they just saw).
        //
        // `sort_unstable_by` is preferred over `sort_by`: the correlate
        // stage guarantees unique `trace_id` per `Trace` (all spans with
        // the same trace_id are folded into one entry), so sort stability
        // has no semantic value here and the unstable variant avoids the
        // merge-sort allocation.
        traces.sort_unstable_by(|a, b| a.trace_id.cmp(&b.trace_id));

        let trace_ids: Vec<String> = traces.iter().map(|t| t.trace_id.clone()).collect();
        let trace_index: HashMap<String, usize> = traces
            .iter()
            .enumerate()
            .map(|(i, t)| (t.trace_id.clone(), i))
            .collect();

        // Build per-trace index lists (indices into all_findings)
        let mut findings_by_trace: Vec<Vec<usize>> = vec![Vec::new(); traces.len()];
        for (idx, finding) in findings.iter().enumerate() {
            if let Some(&trace_vec_idx) = trace_index.get(&finding.trace_id) {
                findings_by_trace[trace_vec_idx].push(idx);
            }
        }

        Self {
            traces,
            detect_config,
            all_findings: findings,
            findings_by_trace,
            trace_ids,
            trace_index,
            selected_trace: 0,
            selected_finding: 0,
            active_panel: Panel::Traces,
            scroll_offset: 0,
            cached_detail: None,
            pre_rendered_trees: HashMap::new(),
            correlations: Vec::new(),
            selected_correlation: 0,
            #[cfg(feature = "daemon")]
            daemon_url: None,
            #[cfg(feature = "daemon")]
            api_key: None,
            #[cfg(feature = "daemon")]
            acks_by_signature: HashMap::new(),
            #[cfg(feature = "daemon")]
            ack_modal: AckModalState::default(),
        }
    }

    /// Attach a daemon handle so `a`/`u` keys are active. Used by
    /// `query inspect` to wire the TUI into the live daemon ack flow.
    /// Without this, the TUI is read-only and the keys are no-op.
    #[cfg(feature = "daemon")]
    pub(crate) fn with_daemon_handle(
        mut self,
        daemon_url: String,
        api_key: Option<String>,
        acks_by_signature: HashMap<String, AckSource>,
    ) -> Self {
        self.daemon_url = Some(daemon_url);
        self.api_key = api_key;
        self.acks_by_signature = acks_by_signature;
        self
    }

    /// Attach pre-rendered span trees keyed by `trace_id`. Used by
    /// `query inspect` to populate the detail panel from daemon
    /// responses when the CLI has no raw spans.
    ///
    /// `pub(crate)` because the TUI module is internal to the
    /// `perf-sentinel` binary crate, not a published library API.
    pub(crate) fn with_pre_rendered_trees(
        mut self,
        trees: std::collections::HashMap<String, String>,
    ) -> Self {
        self.pre_rendered_trees = trees;
        self
    }

    /// Attach cross-trace correlations fetched from a daemon. The
    /// Correlations panel renders them as a navigable list.
    pub(crate) fn with_correlations(mut self, correlations: Vec<CrossTraceCorrelation>) -> Self {
        self.correlations = correlations;
        self
    }

    /// Number of correlations available in the Correlations panel.
    #[must_use]
    pub fn correlation_count(&self) -> usize {
        self.correlations.len()
    }

    /// Number of traces available.
    #[must_use]
    pub fn trace_count(&self) -> usize {
        self.trace_ids.len()
    }

    /// Number of findings for the currently selected trace.
    #[must_use]
    pub fn finding_count(&self) -> usize {
        self.current_finding_indices().len()
    }

    /// Finding indices for the currently selected trace.
    fn current_finding_indices(&self) -> &[usize] {
        self.findings_by_trace
            .get(self.selected_trace)
            .map_or(&[], Vec::as_slice)
    }

    /// Currently selected finding, if any.
    pub(crate) fn current_finding(&self) -> Option<&Finding> {
        let indices = self.current_finding_indices();
        indices
            .get(self.selected_finding)
            .map(|&idx| &self.all_findings[idx])
    }

    /// Count the logical lines the Detail panel will render for the
    /// currently selected finding.
    ///
    /// Mirrors the line construction in [`draw_detail_panel`]: 7 always-
    /// present metadata rows (type header, template, occurrences, service,
    /// endpoint, suggestion, plus the blank between the header and the
    /// body), +1 when the finding carries a `green_impact`, then either
    /// the cached span tree (+2 for the header, +N for the tree lines)
    /// or the unavailability hint (+5 for the blank, header, and 3 hint
    /// lines pointing at `inspect --input <events>.json` and `query
    /// inspect`).
    ///
    /// Used by [`App::move_down`] to clamp the Detail-panel scroll offset
    /// so `Down`/`j` cannot scroll past the content. Long wrapped lines
    /// count as one logical line, so the clamp is slightly conservative
    /// on wrapped output, the tradeoff vs. reading the panel width at
    /// event-handling time (which ratatui does not expose) is accepted.
    fn detail_panel_line_count(&self) -> u16 {
        let Some(finding) = self.current_finding() else {
            return 0;
        };
        // 6 always-present metadata rows + 1 blank after the type header.
        let mut count: u16 = 7;
        if finding.green_impact.is_some() {
            count = count.saturating_add(1);
        }
        if let Some((ct, ref text)) = self.cached_detail
            && ct == self.selected_trace
        {
            // +2 for blank + "Span tree:" header, +N for the tree lines.
            let tree_count = u16::try_from(text.lines().count()).unwrap_or(u16::MAX);
            count = count.saturating_add(2).saturating_add(tree_count);
        } else {
            // +2 for blank + "Span tree:" header, +3 for the hint lines.
            count = count.saturating_add(5);
        }
        count
    }

    /// Get the cached detail tree text, computing it if needed.
    ///
    /// Cached per trace (not per finding) since the tree is the same for all
    /// findings in a trace, `build_tree` annotates all findings inline.
    fn detail_tree_text(&mut self) -> Option<String> {
        let trace_idx = self.selected_trace;

        if let Some((ct, ref text)) = self.cached_detail
            && ct == trace_idx
        {
            return Some(text.clone());
        }

        let trace_id = self.trace_ids.get(trace_idx)?.clone();

        // Prefer the pre-rendered tree (populated by `query inspect` from
        // daemon API responses) over the local detect + build_tree path.
        // This lets the TUI display real span trees even when the caller
        // has no raw spans in memory.
        if let Some(text) = self.pre_rendered_trees.get(&trace_id) {
            let text = text.clone();
            self.cached_detail = Some((trace_idx, text.clone()));
            return Some(text);
        }

        let trace_vec_idx = self.trace_index.get(&trace_id).copied()?;
        let trace = &self.traces[trace_vec_idx];
        // When spans are empty (e.g. stub traces from `query inspect` without
        // pre-rendered trees), skip the build_tree path that would produce
        // an empty, confusing panel.
        if trace.spans.is_empty() {
            return None;
        }
        let per_trace_findings =
            sentinel_core::detect::detect(std::slice::from_ref(trace), &self.detect_config);
        let tree = explain::build_tree(trace, &per_trace_findings);
        let text = explain::format_tree_text(&tree, false);
        self.cached_detail = Some((trace_idx, text.clone()));
        Some(text)
    }

    /// Move selection up in the active panel.
    pub fn move_up(&mut self) {
        match self.active_panel {
            Panel::Traces => {
                if self.selected_trace > 0 {
                    self.selected_trace -= 1;
                    self.selected_finding = 0;
                    self.scroll_offset = 0;
                    self.cached_detail = None;
                }
            }
            Panel::Findings => {
                if self.selected_finding > 0 {
                    self.selected_finding -= 1;
                    self.scroll_offset = 0;
                }
            }
            Panel::Detail => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            Panel::Correlations => {
                if self.selected_correlation > 0 {
                    self.selected_correlation -= 1;
                }
            }
        }
    }

    /// Move selection down in the active panel.
    pub fn move_down(&mut self) {
        match self.active_panel {
            Panel::Traces => {
                if self.selected_trace + 1 < self.trace_count() {
                    self.selected_trace += 1;
                    self.selected_finding = 0;
                    self.scroll_offset = 0;
                    self.cached_detail = None;
                }
            }
            Panel::Findings => {
                if self.selected_finding + 1 < self.finding_count() {
                    self.selected_finding += 1;
                    self.scroll_offset = 0;
                }
            }
            Panel::Detail => {
                // Clamp to `line_count - 1` so the last logical line stays
                // at least partially visible at the top of the viewport
                // when fully scrolled down. Prevents infinite scroll past
                // the content, which used to leave the panel blank.
                let max_offset = self.detail_panel_line_count().saturating_sub(1);
                if self.scroll_offset < max_offset {
                    self.scroll_offset = self.scroll_offset.saturating_add(1);
                }
            }
            Panel::Correlations => {
                if self.selected_correlation + 1 < self.correlation_count() {
                    self.selected_correlation += 1;
                }
            }
        }
    }

    /// Move focus to the next panel.
    pub fn next_panel(&mut self) {
        self.active_panel = match self.active_panel {
            Panel::Traces => Panel::Findings,
            Panel::Findings => Panel::Detail,
            Panel::Detail => Panel::Correlations,
            Panel::Correlations => Panel::Traces,
        };
    }

    /// Move focus to the previous panel.
    pub fn prev_panel(&mut self) {
        self.active_panel = match self.active_panel {
            Panel::Traces => Panel::Correlations,
            Panel::Findings => Panel::Traces,
            Panel::Detail => Panel::Findings,
            Panel::Correlations => Panel::Detail,
        };
    }

    /// Handle Enter key: drill into next panel.
    // TODO: in a follow-up, jump to `correlation.sample_trace_id` from
    // the Correlations panel into the Detail panel for that trace.
    pub fn enter(&mut self) {
        match self.active_panel {
            Panel::Traces => {
                if self.finding_count() > 0 {
                    self.active_panel = Panel::Findings;
                    self.selected_finding = 0;
                }
            }
            Panel::Findings => {
                self.active_panel = Panel::Detail;
                self.scroll_offset = 0;
            }
            Panel::Detail | Panel::Correlations => {}
        }
    }

    /// Handle Escape: go back to previous panel.
    pub fn escape(&mut self) {
        match self.active_panel {
            Panel::Traces | Panel::Correlations => {}
            Panel::Findings => self.active_panel = Panel::Traces,
            Panel::Detail => self.active_panel = Panel::Findings,
        }
    }
}

/// State for the ack/revoke modal overlay. Lives on `App.ack_modal`.
/// `Default` is the hidden state, the modal is opened by `open_ack` /
/// `open_unack` from the `a` and `u` key handlers in `run_loop`.
#[cfg(feature = "daemon")]
#[derive(Debug, Default)]
pub struct AckModalState {
    pub mode: AckModalMode,
    /// Reason input buffer (max 256 chars, single-line).
    pub reason_buf: String,
    /// Expires input buffer (free text, parsed at submit time).
    pub expires_buf: String,
    /// Acknowledger identity buffer (max 128 chars). Pre-filled from $USER.
    pub by_buf: String,
    pub focus: AckFormField,
    /// Error message displayed at the bottom of the modal.
    pub error_message: Option<String>,
    /// Whether a request is currently in flight.
    pub submitting: bool,
}

#[cfg(feature = "daemon")]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum AckModalMode {
    #[default]
    Hidden,
    /// Creating an ack for the given signature.
    Ack { signature: String },
    /// Revoking an existing ack for the given signature.
    Unack { signature: String },
}

#[cfg(feature = "daemon")]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AckFormField {
    #[default]
    Reason,
    Expires,
    By,
    Submit,
    Cancel,
}

// Modal text-buffer character caps. Capping in chars (not bytes) so
// multi-byte UTF-8 input fills the buffer at the same rate the user
// sees typed characters. The daemon enforces server-side limits on
// reason / by anyway, these caps just keep the modal layout stable.
#[cfg(feature = "daemon")]
const REASON_MAX: usize = 256;
#[cfg(feature = "daemon")]
const EXPIRES_MAX: usize = 64;
#[cfg(feature = "daemon")]
const BY_MAX: usize = 128;

#[cfg(feature = "daemon")]
impl AckModalState {
    #[must_use]
    pub fn is_visible(&self) -> bool {
        !matches!(self.mode, AckModalMode::Hidden)
    }

    /// Open the modal in Ack mode with empty buffers and focus on
    /// Reason. `by_buf` is pre-filled from `$USER` (empty if unset).
    pub fn open_ack(&mut self, signature: String) {
        self.mode = AckModalMode::Ack { signature };
        self.reason_buf.clear();
        self.expires_buf.clear();
        self.by_buf = std::env::var("USER").unwrap_or_default();
        self.focus = AckFormField::Reason;
        self.error_message = None;
        self.submitting = false;
    }

    /// Open the modal in Unack mode (confirmation only, no form).
    /// Focus starts on Submit so a single Enter confirms the revoke.
    pub fn open_unack(&mut self, signature: String) {
        self.mode = AckModalMode::Unack { signature };
        self.reason_buf.clear();
        self.expires_buf.clear();
        self.by_buf.clear();
        self.focus = AckFormField::Submit;
        self.error_message = None;
        self.submitting = false;
    }

    pub fn close(&mut self) {
        self.mode = AckModalMode::Hidden;
        self.error_message = None;
        self.submitting = false;
    }

    pub fn next_field(&mut self) {
        // Unack mode only exposes Submit/Cancel, Ack mode cycles
        // Reason -> Expires -> By -> Submit -> Cancel.
        self.focus = if matches!(self.mode, AckModalMode::Unack { .. }) {
            match self.focus {
                AckFormField::Submit => AckFormField::Cancel,
                _ => AckFormField::Submit,
            }
        } else {
            match self.focus {
                AckFormField::Reason => AckFormField::Expires,
                AckFormField::Expires => AckFormField::By,
                AckFormField::By => AckFormField::Submit,
                AckFormField::Submit => AckFormField::Cancel,
                AckFormField::Cancel => AckFormField::Reason,
            }
        };
    }

    pub fn prev_field(&mut self) {
        self.focus = if matches!(self.mode, AckModalMode::Unack { .. }) {
            match self.focus {
                AckFormField::Cancel => AckFormField::Submit,
                _ => AckFormField::Cancel,
            }
        } else {
            match self.focus {
                AckFormField::Reason => AckFormField::Cancel,
                AckFormField::Expires => AckFormField::Reason,
                AckFormField::By => AckFormField::Expires,
                AckFormField::Submit => AckFormField::By,
                AckFormField::Cancel => AckFormField::Submit,
            }
        };
    }
}

/// Outcome of a single key press inside the modal. The run loop reacts
/// by closing, submitting, or doing nothing.
#[cfg(feature = "daemon")]
#[derive(Debug, PartialEq, Eq)]
pub enum ModalAction {
    None,
    Cancel,
    Submit,
}

/// Pure function that maps a `KeyCode` to a `ModalAction` while mutating
/// the form buffers. Tested without spinning up a real terminal.
#[cfg(feature = "daemon")]
pub fn handle_modal_key(modal: &mut AckModalState, code: KeyCode) -> ModalAction {
    match code {
        KeyCode::Esc => ModalAction::Cancel,
        KeyCode::Tab => {
            modal.next_field();
            ModalAction::None
        }
        KeyCode::BackTab => {
            modal.prev_field();
            ModalAction::None
        }
        KeyCode::Enter => match modal.focus {
            AckFormField::Submit => ModalAction::Submit,
            AckFormField::Cancel => ModalAction::Cancel,
            _ => {
                modal.next_field();
                ModalAction::None
            }
        },
        KeyCode::Char(c) => {
            push_char_into_focused_buffer(modal, c);
            ModalAction::None
        }
        KeyCode::Backspace => {
            match modal.focus {
                AckFormField::Reason => {
                    modal.reason_buf.pop();
                }
                AckFormField::Expires => {
                    modal.expires_buf.pop();
                }
                AckFormField::By => {
                    modal.by_buf.pop();
                }
                AckFormField::Submit | AckFormField::Cancel => {}
            }
            ModalAction::None
        }
        _ => ModalAction::None,
    }
}

#[cfg(feature = "daemon")]
fn push_char_into_focused_buffer(modal: &mut AckModalState, c: char) {
    // Defense-in-depth: refuse C0/C1 controls and bidi overrides on
    // typed input. The daemon strips them server-side too, but a
    // bracketed paste of an attacker-crafted signature could otherwise
    // skew the modal layout for the operator who is approving it.
    if !is_modal_input_char_acceptable(c) {
        return;
    }
    match modal.focus {
        AckFormField::Reason if modal.reason_buf.chars().count() < REASON_MAX => {
            modal.reason_buf.push(c);
        }
        AckFormField::Expires if modal.expires_buf.chars().count() < EXPIRES_MAX => {
            modal.expires_buf.push(c);
        }
        AckFormField::By if modal.by_buf.chars().count() < BY_MAX => {
            modal.by_buf.push(c);
        }
        _ => {}
    }
}

#[cfg(feature = "daemon")]
fn is_modal_input_char_acceptable(c: char) -> bool {
    // C0 / C1 / DEL controls would corrupt the rendered modal.
    if c.is_control() {
        return false;
    }
    // Bidi overrides and isolates can flip the visible order of the
    // surrounding text, including the modal labels and buttons.
    !matches!(c as u32, 0x202A..=0x202E | 0x2066..=0x2069)
}

/// Install a panic hook that restores the terminal before the
/// standard hook prints the panic message. Without this, a panic
/// inside `run_loop` (e.g. from a future ratatui upgrade or from a
/// `block_on` in `submit_ack_modal`) leaves the operator with raw
/// mode + alternate screen still active, forcing a `reset` in their
/// shell.
///
/// Wrapped in `Once` so that calling `run` twice in the same process
/// (test re-entry, future embedding) does not stack hooks. The chain
/// to the previous hook is captured at first install and persists for
/// the process lifetime.
fn install_terminal_restore_panic_hook() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
            prev_hook(info);
        }));
    });
}

/// Run the TUI event loop.
///
/// # Errors
///
/// Returns an error if terminal setup or event reading fails.
pub fn run(app: &mut App) -> io::Result<()> {
    install_terminal_restore_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, app);

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    loop {
        // Pre-compute detail tree text (requires &mut self) before immutable draw
        app.detail_tree_text();
        terminal.draw(|f| draw(f, app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Modal takes precedence on input. While visible, all keys
            // route to the form handler, none of the panel navigation
            // keys (q, j/k, Tab, Enter, Esc) reach the main dispatch.
            #[cfg(feature = "daemon")]
            if app.ack_modal.is_visible() {
                match handle_modal_key(&mut app.ack_modal, key.code) {
                    ModalAction::None => {}
                    ModalAction::Cancel => app.ack_modal.close(),
                    ModalAction::Submit => submit_ack_modal(app),
                }
                continue;
            }

            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::Right | KeyCode::Tab => app.next_panel(),
                KeyCode::Left | KeyCode::BackTab => app.prev_panel(),
                KeyCode::Enter => app.enter(),
                KeyCode::Esc => app.escape(),
                #[cfg(feature = "daemon")]
                KeyCode::Char('a') if app.daemon_url.is_some() => {
                    if let Some(finding) = app.current_finding() {
                        let sig = finding.signature.clone();
                        app.ack_modal.open_ack(sig);
                    }
                }
                #[cfg(feature = "daemon")]
                KeyCode::Char('u') if app.daemon_url.is_some() => {
                    if let Some(finding) = app.current_finding() {
                        let sig = finding.signature.clone();
                        app.ack_modal.open_unack(sig);
                    }
                }
                _ => {}
            }
        }
    }
}

fn draw(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(f.area());

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Percentage(45),
            Constraint::Percentage(35),
        ])
        .split(chunks[0]);

    draw_traces_panel(f, app, top[0]);
    draw_findings_panel(f, app, top[1]);
    draw_correlations_panel(f, app, top[2]);
    draw_detail_panel(f, app, chunks[1]);

    #[cfg(feature = "daemon")]
    if app.ack_modal.is_visible() {
        draw_ack_modal(f, app);
    }
}

fn panel_style(app: &App, panel: Panel) -> Style {
    if app.active_panel == panel {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_traces_panel(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = app
        .trace_ids
        .iter()
        .enumerate()
        .map(|(i, tid)| {
            let finding_count = app.findings_by_trace.get(i).map_or(0, Vec::len);
            let label = if finding_count > 0 {
                format!("{tid} ({finding_count})")
            } else {
                tid.clone()
            };
            ListItem::new(Line::from(label))
        })
        .collect();

    let block = Block::default()
        .title(" Traces ")
        .borders(Borders::ALL)
        .border_style(panel_style(app, Panel::Traces));

    let mut state = ListState::default();
    state.select(Some(app.selected_trace));

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED),
    );

    f.render_stateful_widget(list, area, &mut state);
}

fn draw_findings_panel(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let indices = app.current_finding_indices();
    let items: Vec<ListItem> = indices
        .iter()
        .enumerate()
        .map(|(i, &idx)| {
            let finding = &app.all_findings[idx];
            let severity_color = severity_color(&finding.severity);
            let type_label = finding_type_label(&finding.finding_type);
            let mut spans = vec![
                Span::styled(
                    format!("[{}] ", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{type_label} "),
                    Style::default()
                        .fg(severity_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    severity_label(&finding.severity),
                    Style::default().fg(severity_color),
                ),
            ];
            #[cfg(feature = "daemon")]
            if let Some(ack) = app.acks_by_signature.get(&finding.signature) {
                let by = match ack {
                    AckSource::Toml {
                        acknowledged_by, ..
                    } => acknowledged_by.as_str(),
                    AckSource::Daemon { by, .. } => by.as_str(),
                };
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("[acked by {}]", sanitize_for_terminal(by)),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let block = Block::default()
        .title(" Findings ")
        .borders(Borders::ALL)
        .border_style(panel_style(app, Panel::Findings));

    let mut state = ListState::default();
    if !indices.is_empty() {
        state.select(Some(app.selected_finding));
    }

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED),
    );

    f.render_stateful_widget(list, area, &mut state);
}

fn draw_correlations_panel(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let block = Block::default()
        .title(" Correlations ")
        .borders(Borders::ALL)
        .border_style(panel_style(app, Panel::Correlations));

    if app.correlations.is_empty() {
        let hint = Paragraph::new(
            "No correlations available.\n\nLaunch via 'query inspect' against a daemon to see cross-trace pairs.",
        )
        .block(block)
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, area);
        return;
    }

    let items: Vec<ListItem> = app
        .correlations
        .iter()
        .map(|c| {
            let line = Line::from(vec![
                Span::styled(
                    format!(
                        "{}:{} ",
                        sanitize_for_terminal(&c.source.service),
                        c.source.finding_type.as_str()
                    ),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw("-> "),
                Span::styled(
                    format!(
                        "{}:{}  ",
                        sanitize_for_terminal(&c.target.service),
                        c.target.finding_type.as_str()
                    ),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("{:.0}% ", c.confidence * 100.0),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:.0}ms ", c.median_lag_ms),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(format!("({}x)", c.co_occurrence_count)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.selected_correlation));

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED),
    );

    f.render_stateful_widget(list, area, &mut state);
}

fn draw_detail_panel(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let block = Block::default()
        .title(" Detail ")
        .borders(Borders::ALL)
        .border_style(panel_style(app, Panel::Detail));

    let Some(finding) = app.current_finding() else {
        let help = Paragraph::new("Select a finding to see details.\n\nKeys: ↑↓ navigate, ←→/Tab switch panels, Enter drill in, Esc back, q quit")
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(help, area);
        return;
    };

    let severity_color = severity_color(&finding.severity);
    let type_label = finding_type_label(&finding.finding_type);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{type_label} "),
                Style::default()
                    .fg(severity_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                severity_label(&finding.severity),
                Style::default().fg(severity_color),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Template: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&finding.pattern.template),
        ]),
        Line::from(vec![
            Span::styled("Occurrences: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                "{}, {} distinct params, {}ms window",
                finding.pattern.occurrences,
                finding.pattern.distinct_params,
                finding.pattern.window_ms
            )),
        ]),
        Line::from(vec![
            Span::styled("Service: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&finding.service),
        ]),
        Line::from(vec![
            Span::styled("Endpoint: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&finding.source_endpoint),
        ]),
        Line::from(vec![
            Span::styled("Suggestion: ", Style::default().fg(Color::Cyan)),
            Span::raw(&finding.suggestion),
        ]),
    ];

    if let Some(ref loc) = finding.code_location {
        let src = loc.display_string();
        if !src.is_empty() {
            lines.insert(
                6,
                Line::from(vec![
                    Span::styled("Source:   ", Style::default().fg(Color::DarkGray)),
                    Span::raw(src),
                ]),
            );
        }
    }

    if let Some(ref impact) = finding.green_impact {
        lines.push(Line::from(vec![
            Span::styled("Extra I/O: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{} avoidable ops", impact.estimated_extra_io_ops)),
        ]));
    }

    // Add span tree from cache (pre-computed before draw, cached per trace)
    if let Some((ct, ref tree_text)) = app.cached_detail
        && ct == app.selected_trace
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Span tree:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for tree_line in tree_text.lines() {
            lines.push(Line::from(tree_line.to_string()));
        }
    } else {
        // No span tree available: the input was a Report (no embedded
        // spans) or a daemon trace that the explain endpoint did not
        // return. Surface the two paths that produce a real tree so
        // the user knows what to try next.
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Span tree:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "Not available for this trace. Reports do not carry raw spans.",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  - perf-sentinel inspect --input <events>.json  (raw events)",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  - perf-sentinel query inspect                  (live daemon)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));

    f.render_widget(paragraph, area);
}

#[cfg(feature = "daemon")]
fn draw_ack_modal(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();
    // 70 cols accommodate the footer hint and the unack confirmation
    // message at full width on a typical terminal. Clamped down on
    // narrow terminals to keep the modal inside the screen.
    let modal_w = 70.min(area.width.saturating_sub(4));
    let modal_h: u16 = match app.ack_modal.mode {
        AckModalMode::Ack { .. } => 16,
        AckModalMode::Unack { .. } => 8,
        AckModalMode::Hidden => return,
    };
    let modal_area = centered_rect(modal_w, modal_h, area);
    f.render_widget(Clear, modal_area);

    let title = match app.ack_modal.mode {
        AckModalMode::Ack { .. } => " Acknowledge finding ",
        AckModalMode::Unack { .. } => " Revoke acknowledgment ",
        AckModalMode::Hidden => return,
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(modal_area);
    f.render_widget(block, modal_area);

    match app.ack_modal.mode {
        AckModalMode::Ack { ref signature } => draw_ack_form(f, app, inner, signature),
        AckModalMode::Unack { ref signature } => draw_unack_form(f, app, inner, signature),
        AckModalMode::Hidden => {}
    }
}

#[cfg(feature = "daemon")]
fn draw_ack_form(f: &mut ratatui::Frame, app: &App, area: Rect, signature: &str) {
    let constraints = [
        Constraint::Length(1), // signature
        Constraint::Length(1), // blank
        Constraint::Length(1), // reason label
        Constraint::Length(1), // reason input
        Constraint::Length(1), // expires label
        Constraint::Length(1), // expires input
        Constraint::Length(1), // by label
        Constraint::Length(1), // by input
        Constraint::Length(1), // blank
        Constraint::Length(1), // buttons
        Constraint::Min(1),    // error / hint
    ];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Finding: ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize_for_terminal(signature)),
        ])),
        rows[0],
    );
    render_field_label(
        f,
        rows[2],
        "Reason (required)",
        app.ack_modal.focus,
        AckFormField::Reason,
    );
    render_field_input(
        f,
        rows[3],
        &app.ack_modal.reason_buf,
        app.ack_modal.focus == AckFormField::Reason,
    );
    render_field_label(
        f,
        rows[4],
        "Expires (e.g. 24h, 7d, ISO8601)",
        app.ack_modal.focus,
        AckFormField::Expires,
    );
    render_field_input(
        f,
        rows[5],
        &app.ack_modal.expires_buf,
        app.ack_modal.focus == AckFormField::Expires,
    );
    render_field_label(f, rows[6], "By", app.ack_modal.focus, AckFormField::By);
    render_field_input(
        f,
        rows[7],
        &app.ack_modal.by_buf,
        app.ack_modal.focus == AckFormField::By,
    );
    render_modal_buttons(f, rows[9], &app.ack_modal);
    render_modal_footer(f, rows[10], app.ack_modal.error_message.as_deref());
}

#[cfg(feature = "daemon")]
fn draw_unack_form(f: &mut ratatui::Frame, app: &App, area: Rect, signature: &str) {
    let constraints = [
        Constraint::Length(1), // signature
        Constraint::Length(1), // blank
        Constraint::Length(1), // confirm message
        Constraint::Length(1), // blank
        Constraint::Length(1), // buttons
        Constraint::Min(1),    // error
    ];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Finding: ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize_for_terminal(signature)),
        ])),
        rows[0],
    );
    f.render_widget(
        Paragraph::new("Revoke this acknowledgment? Press Enter to confirm, Esc to cancel.")
            .style(Style::default().fg(Color::Yellow)),
        rows[2],
    );
    render_modal_buttons(f, rows[4], &app.ack_modal);
    render_modal_footer(f, rows[5], app.ack_modal.error_message.as_deref());
}

#[cfg(feature = "daemon")]
fn render_field_label(
    f: &mut ratatui::Frame,
    area: Rect,
    label: &str,
    focus: AckFormField,
    field: AckFormField,
) {
    let style = if focus == field {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    f.render_widget(Paragraph::new(label).style(style), area);
}

#[cfg(feature = "daemon")]
fn render_field_input(f: &mut ratatui::Frame, area: Rect, value: &str, focused: bool) {
    // Borrow when possible: the focused branch needs to append a
    // cursor char so it allocates, the empty placeholder is `'static`
    // and the unfocused branch just borrows `value`.
    let display: std::borrow::Cow<'_, str> = if value.is_empty() && !focused {
        std::borrow::Cow::Borrowed("(empty)")
    } else if focused {
        std::borrow::Cow::Owned(format!("{value}_"))
    } else {
        std::borrow::Cow::Borrowed(value)
    };
    let style = if focused {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    f.render_widget(Paragraph::new(display).style(style), area);
}

#[cfg(feature = "daemon")]
fn render_modal_buttons(f: &mut ratatui::Frame, area: Rect, modal: &AckModalState) {
    let submit_label = if modal.submitting {
        "[Submitting...]"
    } else {
        "[Submit]"
    };
    let submit_style = if modal.focus == AckFormField::Submit {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };
    let cancel_style = if modal.focus == AckFormField::Cancel {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red)
    };
    let line = Line::from(vec![
        Span::styled(submit_label, submit_style),
        Span::raw("   "),
        Span::styled("[Cancel]", cancel_style),
        Span::raw("   "),
        Span::styled(
            "Tab/Shift-Tab to switch, Esc to cancel",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

#[cfg(feature = "daemon")]
fn render_modal_footer(f: &mut ratatui::Frame, area: Rect, error: Option<&str>) {
    if let Some(msg) = error {
        f.render_widget(
            Paragraph::new(sanitize_for_terminal(msg))
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: true }),
            area,
        );
    }
}

#[cfg(feature = "daemon")]
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

fn severity_color(severity: &Severity) -> Color {
    match severity {
        Severity::Critical => Color::Red,
        Severity::Warning => Color::Yellow,
        Severity::Info => Color::Cyan,
    }
}

fn severity_label(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical => "CRITICAL",
        Severity::Warning => "WARNING",
        Severity::Info => "INFO",
    }
}

fn finding_type_label(ft: &FindingType) -> &'static str {
    ft.display_label()
}

/// Bridge from the synchronous `run_loop` into the async daemon HTTP
/// helpers in `crate::ack`. Relies on `tokio::task::block_in_place`
/// being active around `run`, which `query.rs::run_inspect_action`
/// arranges before calling into the TUI.
///
/// Submits the ack/unack request, refreshes `acks_by_signature` on
/// success, and maps errors into `error_message` so the user can see
/// what went wrong without leaving the TUI.
#[cfg(feature = "daemon")]
fn submit_ack_modal(app: &mut App) {
    let Some(daemon_url) = app.daemon_url.clone() else {
        app.ack_modal.error_message = Some("daemon not configured".to_string());
        return;
    };

    let Some(signature) = signature_for_modal_mode(&app.ack_modal.mode).map(str::to_string) else {
        // Logic-bug guard: submit reached run_loop's modal-visible
        // dispatch, which `is_visible` already filtered Hidden out of,
        // so this arm is unreachable in current control flow. Log via
        // tracing so a future refactor that bypasses the guard does
        // not silently drop the click.
        tracing::error!("submit_ack_modal called with Hidden mode, dropped");
        return;
    };

    app.ack_modal.submitting = true;

    // The mode is non-Hidden here (signature_for_modal_mode would have
    // returned None otherwise). Match on the discriminant so a future
    // enum variant gets a compile error rather than a runtime panic.
    let is_ack = matches!(app.ack_modal.mode, AckModalMode::Ack { .. });
    let result = if is_ack {
        submit_ack_create(app, &daemon_url, &signature)
    } else {
        submit_ack_revoke(app, &daemon_url, &signature)
    };

    match result {
        Ok(()) => {
            let api_key = app.api_key.clone();
            match tokio::runtime::Handle::current()
                .block_on(refetch_acks_from_daemon(&daemon_url, api_key.as_deref()))
            {
                Ok(refreshed) => app.acks_by_signature = refreshed,
                Err(e) => tracing::warn!(
                    "ack submit succeeded but refetch failed, indicator may be stale: {e}"
                ),
            }
            app.ack_modal.close();
        }
        Err(crate::ack::AckSubmitError::Unauthorized) => {
            app.ack_modal.error_message = Some(
                "API key required: set PERF_SENTINEL_DAEMON_API_KEY or pass \
                 --api-key-file when launching `query inspect`."
                    .to_string(),
            );
            app.ack_modal.submitting = false;
        }
        Err(e) => {
            app.ack_modal.error_message = Some(e.to_string());
            app.ack_modal.submitting = false;
        }
    }
}

#[cfg(feature = "daemon")]
fn signature_for_modal_mode(mode: &AckModalMode) -> Option<&str> {
    match mode {
        AckModalMode::Ack { signature } | AckModalMode::Unack { signature } => {
            Some(signature.as_str())
        }
        AckModalMode::Hidden => None,
    }
}

#[cfg(feature = "daemon")]
fn submit_ack_create(
    app: &mut App,
    daemon_url: &str,
    signature: &str,
) -> Result<(), crate::ack::AckSubmitError> {
    let expires = if app.ack_modal.expires_buf.trim().is_empty() {
        None
    } else {
        match crate::ack::parse_expires(&app.ack_modal.expires_buf) {
            Ok(dt) => Some(dt),
            // Surface the parser's `expected ISO8601 datetime ...`
            // message via `Validation` so the outer match in
            // `submit_ack_modal` does not clobber it with a generic
            // network-error wrapper.
            Err(e) => {
                return Err(crate::ack::AckSubmitError::Validation(format!(
                    "expires: {e}"
                )));
            }
        }
    };
    let by = app.ack_modal.by_buf.clone();
    let reason = app.ack_modal.reason_buf.clone();
    let api_key = app.api_key.clone();
    tokio::runtime::Handle::current().block_on(crate::ack::post_ack_via_daemon(
        daemon_url,
        signature,
        &by,
        &reason,
        expires,
        api_key.as_deref(),
    ))
}

#[cfg(feature = "daemon")]
fn submit_ack_revoke(
    app: &mut App,
    daemon_url: &str,
    signature: &str,
) -> Result<(), crate::ack::AckSubmitError> {
    let api_key = app.api_key.clone();
    tokio::runtime::Handle::current().block_on(crate::ack::delete_ack_via_daemon(
        daemon_url,
        signature,
        api_key.as_deref(),
    ))
}

/// Fetch `/api/findings?include_acked=true&limit={FINDINGS_FETCH_LIMIT}`
/// and rebuild the `acks_by_signature` map. Called after every
/// successful submit so the Findings panel indicator and modal
/// gating stay in sync.
#[cfg(feature = "daemon")]
async fn refetch_acks_from_daemon(
    daemon_url: &str,
    api_key: Option<&str>,
) -> Result<HashMap<String, AckSource>, String> {
    let client = sentinel_core::http_client::build_client_with_body();
    let limit = crate::ack::FINDINGS_FETCH_LIMIT;
    let url = format!("{daemon_url}/api/findings?include_acked=true&limit={limit}");
    let (status, body) = crate::ack::http_call(
        &client,
        hyper::Method::GET,
        &url,
        api_key,
        bytes::Bytes::new(),
    )
    .await
    .map_err(|e| e.to_string())?;
    if status.as_u16() != 200 {
        return Err(format!("HTTP {} on findings refetch", status.as_u16()));
    }
    let responses: Vec<sentinel_core::daemon::query_api::FindingResponse> =
        serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    Ok(responses
        .into_iter()
        .filter_map(|r| {
            r.acknowledged_by
                .map(|src| (r.stored.finding.signature, src))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
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
                },
                suggestion: "Use WHERE ... IN (?)".to_string(),
                first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
                last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
                green_impact: Some(GreenImpact {
                    estimated_extra_io_ops: 5,
                    io_intensity_score: 6.0,
                    io_intensity_band:
                        sentinel_core::report::interpret::InterpretationLevel::for_iis(6.0),
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
        // At traces, escape does nothing
        app.escape();
        assert_eq!(app.active_panel, Panel::Traces);
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
        let mut trees = std::collections::HashMap::new();
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
        let mut trees = std::collections::HashMap::new();
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
        let mut trees = std::collections::HashMap::new();
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
            // Cell (0, 0) is the top-left corner of the Traces panel border.
            terminal.backend().buffer()[(0, 0)].style()
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
                at: chrono::Utc::now(),
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
                at: chrono::Utc::now(),
                reason: Some("test".to_string()),
                expires_at: None,
            },
        );
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rendered: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("acked by alice"),
            "expected ack indicator in rendered TUI buffer, got:\n{rendered}"
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
        let rendered: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("Acknowledge finding"),
            "expected modal title, got:\n{rendered}"
        );
        assert!(rendered.contains("Reason"), "expected reason field label");
        assert!(rendered.contains("[Submit]"), "expected submit button");
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn submit_ack_create_validation_error_uses_validation_variant() {
        // Drive submit_ack_create directly with an unparseable expires
        // input. The function must return AckSubmitError::Validation
        // (not Transport) so submit_ack_modal does not clobber the
        // message with a "network error:" prefix when it Display's it.
        let mut app = make_test_app();
        app.daemon_url = Some("http://localhost:14318".to_string());
        app.ack_modal.open_ack("sig".to_string());
        app.ack_modal.expires_buf = "not a date".to_string();
        let result = submit_ack_create(&mut app, "http://localhost:14318", "sig");
        let err = result.expect_err("invalid expires must surface an error");
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
    fn opening_ack_modal_with_no_finding_is_silent() {
        // Build an app with no findings: pressing `a` would call
        // `current_finding()` which returns None, the modal stays
        // hidden. Mirror that path here by reading current_finding and
        // confirming we cannot dispatch an open with an empty signature.
        let app = App::new(
            Vec::new(),
            Vec::new(),
            sentinel_core::detect::DetectConfig {
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
        let rendered: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("daemon ack store disabled"),
            "expected error message in modal footer, got:\n{rendered}"
        );
    }
}
