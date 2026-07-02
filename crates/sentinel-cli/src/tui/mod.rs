//! Terminal UI for interactive trace and finding inspection.
//!
//! Three top-level views form a single drill-down — `Analyze` (summary),
//! `Inspect` (the multi-panel browser: traces list, findings, correlations
//! and a detail span tree) and `Explain` (the selected trace's span tree
//! full screen). Enter descends `Analyze -> Inspect -> Explain`, Esc
//! ascends back.

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
// `Clear` only backs the ack modal, which is daemon-gated.
#[cfg(feature = "daemon")]
use ratatui::widgets::Clear;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use sentinel_core::correlate::Trace;
#[cfg(feature = "daemon")]
use sentinel_core::daemon::query_api::AckSource;
use sentinel_core::detect::correlate_cross::CrossTraceCorrelation;
use sentinel_core::detect::{DetectConfig, Finding, FindingType, Severity};
use sentinel_core::explain;
use sentinel_core::report::interpret::InterpretationLevel;
use sentinel_core::report::periodic::schema::{Confidentiality, ReportIntent};
use sentinel_core::report::{Analysis, GreenSummary, QualityGate};
use sentinel_core::text_safety::sanitize_for_terminal;

use crate::disclose::{CustomField, DiscloseState, Granularity, Tone};
use crate::tui_resize::{
    Axis, DragTarget, MIN_PCT, boundary_cell, in_range, near, pos_to_pct, set_cut,
};

#[cfg(feature = "daemon")]
use chrono::{DateTime, Utc};
#[cfg(feature = "daemon")]
use tokio::sync::mpsc;
/// Panel that currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Traces,
    Findings,
    Detail,
    Correlations,
}

/// Top-level view in the drill-down. Enter descends
/// `Analyze -> Inspect -> Explain`, Esc ascends back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Summary dashboard (`GreenOps` waste, top offenders, quality gate).
    Analyze,
    /// Multi-panel browser: traces, findings, correlations, detail.
    Inspect,
    /// The selected trace's annotated span tree, full screen.
    Explain,
    /// Read-only `disclose` preview: calendar stepper over the period,
    /// live intent/confidentiality toggles, aggregated summary, equivalent
    /// command. Standalone — no drill-down to the other views.
    Disclose,
}

/// Summary data backing the Analyze view. Supplied by the launcher when
/// available; `None` degrades the view to a hint (e.g. an older daemon
/// without `/api/export/report`).
pub struct AnalyzeSummary {
    pub green_summary: GreenSummary,
    pub quality_gate: QualityGate,
    pub analysis: Analysis,
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

    /// Active top-level view. The panel-level `active_panel` only matters
    /// while `view == View::Inspect`.
    pub view: View,
    /// Summary backing the Analyze view. `None` renders a degraded hint.
    summary: Option<AnalyzeSummary>,
    /// Rendered line count of the Analyze body, precomputed once in
    /// `with_summary` (the summary is immutable for the App's lifetime) so
    /// the per-keypress scroll clamp does not rebuild the line vector.
    analyze_line_count: u16,

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
    /// Panel that brought the user into Detail. Read by `escape` to
    /// return to the source panel (Findings or Correlations).
    detail_origin: Panel,

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

    /// Present only under `disclose --tui`. When `Some`, the tab bar shows
    /// just the standalone Disclose tab and the app opens on it; the
    /// analyze/inspect/explain drill-down is unused.
    disclose: Option<DiscloseState>,

    /// Inspect-view panel split ratios (percentages summing to 100),
    /// adjustable at runtime by dragging borders in mouse mode. `rows` is
    /// the vertical top/Detail split, `cols` is the Traces/Findings/
    /// Correlations top row. Reset to the defaults by `r`, not persisted.
    inspect_rows: [u16; 2],
    inspect_cols: [u16; 3],
    /// Mouse capture is opt-in (toggled by `m`): off preserves native
    /// terminal copy-paste, on lets borders be dragged.
    mouse_mode: bool,
    /// Border currently being dragged, set on mouse-down over a border.
    drag: Option<DragTarget>,
    /// Border currently under the cursor, set on mouse motion. Drives the
    /// resize-affordance highlight so the user sees a border is grabbable.
    hover: Option<DragTarget>,
    /// Inspect content area from the last frame, for hit-testing a drag.
    /// `Cell` lets `draw_inspect_view` store it through `&App`.
    inspect_area: std::cell::Cell<Rect>,
}

/// Default Inspect split ratios, also the `r`-reset target.
const INSPECT_ROWS_DEFAULT: [u16; 2] = [50, 50];
const INSPECT_COLS_DEFAULT: [u16; 3] = [20, 30, 50];

impl App {
    /// Create a new app from analysis findings and traces.
    #[must_use]
    pub fn new(
        findings: Vec<Finding>,
        mut traces: Vec<Trace>,
        detect_config: DetectConfig,
    ) -> Self {
        // Sort by trace_id: the upstream `correlate` stage yields traces in
        // randomized HashMap order, so without this the same input file
        // shows a different trace-list order on every launch. Unstable sort
        // is fine (trace_ids are unique) and avoids the merge-sort
        // allocation.
        traces.sort_unstable_by(|a, b| a.trace_id.cmp(&b.trace_id));

        let trace_ids: Vec<String> = traces.iter().map(|t| t.trace_id.clone()).collect();
        let trace_index: HashMap<String, usize> = traces
            .iter()
            .enumerate()
            .map(|(i, t)| (t.trace_id.clone(), i))
            .collect();

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
            view: View::Inspect,
            summary: None,
            analyze_line_count: 0,
            selected_trace: 0,
            selected_finding: 0,
            active_panel: Panel::Traces,
            scroll_offset: 0,
            cached_detail: None,
            pre_rendered_trees: HashMap::new(),
            correlations: Vec::new(),
            selected_correlation: 0,
            detail_origin: Panel::Findings,
            #[cfg(feature = "daemon")]
            daemon_url: None,
            #[cfg(feature = "daemon")]
            api_key: None,
            #[cfg(feature = "daemon")]
            acks_by_signature: HashMap::new(),
            #[cfg(feature = "daemon")]
            ack_modal: AckModalState::default(),
            disclose: None,
            inspect_rows: INSPECT_ROWS_DEFAULT,
            inspect_cols: INSPECT_COLS_DEFAULT,
            mouse_mode: false,
            drag: None,
            hover: None,
            inspect_area: std::cell::Cell::new(Rect::default()),
        }
    }

    /// Attach the `disclose --tui` preview state. The caller pairs this with
    /// `with_initial_view(View::Disclose)`; the App is otherwise built with
    /// empty findings/traces.
    pub(crate) fn with_disclose(mut self, state: DiscloseState) -> Self {
        self.disclose = Some(state);
        self
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
    // Only the daemon-gated `query inspect` flow calls this in
    // production; tests exercise it in every feature combo.
    #[cfg_attr(not(feature = "daemon"), allow(dead_code))]
    pub(crate) fn with_pre_rendered_trees(mut self, trees: HashMap<String, String>) -> Self {
        self.pre_rendered_trees = trees;
        self
    }

    /// Attach cross-trace correlations fetched from a daemon. The
    /// Correlations panel renders them as a navigable list.
    pub(crate) fn with_correlations(mut self, correlations: Vec<CrossTraceCorrelation>) -> Self {
        self.correlations = correlations;
        self
    }

    /// Attach the summary backing the Analyze view (`GreenOps` waste, top
    /// offenders, quality gate). Without it the view shows a hint.
    pub(crate) fn with_summary(mut self, summary: AnalyzeSummary) -> Self {
        self.summary = Some(summary);
        // Build the body once to cache its line count; the summary and
        // findings are immutable afterwards, so the count never changes.
        self.analyze_line_count =
            u16::try_from(self.build_analyze_lines().len()).unwrap_or(u16::MAX);
        self
    }

    /// Set the view the TUI opens on. `analyze --tui` lands on Analyze,
    /// `explain --tui` on Explain, `inspect` keeps the default Inspect.
    pub(crate) fn with_initial_view(mut self, view: View) -> Self {
        self.view = view;
        self
    }

    /// Pre-select a trace by id so the opening view (e.g. Explain for
    /// `explain --tui`) lands on it. No-op when the id is unknown.
    pub(crate) fn with_focus_trace(mut self, trace_id: &str) -> Self {
        if let Some(&idx) = self.trace_index.get(trace_id) {
            self.selected_trace = idx;
            self.selected_finding = 0;
            self.cached_detail = None;
        }
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
    /// Mirrors the line construction in [`draw_detail_panel`]: keep the
    /// two in sync (the body comments track the per-row arithmetic).
    ///
    /// Used by [`App::move_down`] to clamp the Detail-panel scroll offset
    /// so `Down`/`j` cannot scroll past the content. Long wrapped lines
    /// count as one logical line, so the clamp is slightly conservative
    /// on wrapped output, accepted since ratatui does not expose the
    /// panel width at event-handling time.
    fn detail_panel_line_count(&self) -> u16 {
        let Some(finding) = self.current_finding() else {
            return 0;
        };
        // 6 always-present metadata rows + 1 blank after the type header.
        let mut count: u16 = 7;
        // +1 for the optional "Source:" row that `draw_detail_panel` inserts
        // when the finding carries a non-empty code location, else the clamp
        // under-counts by 1 and the last span-tree line stays unreachable.
        if finding
            .code_location
            .as_ref()
            .is_some_and(|loc| !loc.display_string().is_empty())
        {
            count = count.saturating_add(1);
        }
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
                // the content (which would leave the panel blank).
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

    /// Flip mouse mode and, when turning it off, cancel any in-progress
    /// drag. The terminal capture side-effect is applied by the caller via
    /// [`set_mouse_capture`], keeping this pure and unit-testable.
    fn toggle_mouse_mode(&mut self) {
        self.mouse_mode = !self.mouse_mode;
        if !self.mouse_mode {
            self.drag = None;
            self.hover = None;
        }
    }

    /// Reset the Inspect split ratios to their defaults (`r`).
    fn reset_layout(&mut self) {
        self.inspect_rows = INSPECT_ROWS_DEFAULT;
        self.inspect_cols = INSPECT_COLS_DEFAULT;
    }

    /// Whether a mouse drag should resize panels right now. Only in the
    /// Inspect view (the sole resizable layout) and never behind the ack
    /// modal, which captures keys but not the mouse.
    fn accepts_panel_drag(&self) -> bool {
        if self.view != View::Inspect {
            return false;
        }
        #[cfg(feature = "daemon")]
        if self.ack_modal.is_visible() {
            return false;
        }
        true
    }

    /// Border (if any) under the cursor, using the last drawn Inspect area.
    fn hit_test(&self, col: u16, row: u16) -> Option<DragTarget> {
        let area = self.inspect_area.get();
        if area.width == 0 || area.height == 0 {
            return None;
        }
        // Horizontal borders live in the top row; checked before the
        // vertical border so the vertical ±1 tolerance can't shadow the
        // top row's bottom cell.
        let top_h = u16::try_from(u32::from(area.height) * u32::from(self.inspect_rows[0]) / 100)
            .unwrap_or(area.height);
        if in_range(row, area.y, top_h) {
            for b in 0..self.inspect_cols.len() - 1 {
                if near(
                    col,
                    boundary_cell(&self.inspect_cols, b, area.x, area.width),
                ) {
                    return Some(DragTarget {
                        axis: Axis::Horizontal,
                        boundary: b,
                    });
                }
            }
        }
        // Vertical border between the top row and the Detail panel.
        let vy = boundary_cell(&self.inspect_rows, 0, area.y, area.height);
        if near(row, vy) && in_range(col, area.x, area.width) {
            return Some(DragTarget {
                axis: Axis::Vertical,
                boundary: 0,
            });
        }
        None
    }

    /// Mouse-down: if the cursor is on a panel border, start dragging it.
    fn begin_drag(&mut self, col: u16, row: u16) {
        self.drag = self.hit_test(col, row);
    }

    /// The border highlighted right now: the one being dragged, else the
    /// one hovered. Drives the resize-affordance overlay.
    fn resize_target(&self) -> Option<DragTarget> {
        self.drag.or(self.hover)
    }

    /// Mouse-drag: move the active border to the cursor.
    fn apply_drag(&mut self, col: u16, row: u16) {
        let Some(target) = self.drag else {
            return;
        };
        let area = self.inspect_area.get();
        match target.axis {
            Axis::Vertical => set_cut(
                &mut self.inspect_rows,
                target.boundary,
                pos_to_pct(row, area.y, area.height),
                MIN_PCT,
            ),
            Axis::Horizontal => set_cut(
                &mut self.inspect_cols,
                target.boundary,
                pos_to_pct(col, area.x, area.width),
                MIN_PCT,
            ),
        }
    }

    /// Handle Enter key: drill into the next panel.
    ///
    /// - Traces -> Findings (when there are findings)
    /// - Findings -> Detail
    /// - Correlations -> Detail (jumps to `sample_trace_id` if known locally)
    /// - Detail -> Explain view (zooms the span tree full screen)
    pub fn enter(&mut self) {
        match self.active_panel {
            Panel::Traces => {
                if self.finding_count() > 0 {
                    self.active_panel = Panel::Findings;
                    self.selected_finding = 0;
                }
            }
            Panel::Findings => {
                self.enter_detail(Panel::Findings);
            }
            Panel::Correlations => {
                self.jump_to_correlation_sample_trace();
            }
            Panel::Detail => {
                // Deepest panel: zoom the span tree to the full-screen
                // Explain view.
                self.view = View::Explain;
                self.scroll_offset = 0;
            }
        }
    }

    /// Single entry point for any drill-down to `Panel::Detail`. Pairs
    /// `active_panel` with `detail_origin` so `escape` returns to the
    /// source panel. New drill-downs MUST route through this helper.
    fn enter_detail(&mut self, origin: Panel) {
        self.active_panel = Panel::Detail;
        self.detail_origin = origin;
        self.scroll_offset = 0;
    }

    /// Jump from Correlations to Detail for the selected correlation's
    /// `sample_trace_id`. No-op if the trace is unknown locally.
    fn jump_to_correlation_sample_trace(&mut self) {
        let Some(correlation) = self.correlations.get(self.selected_correlation) else {
            return;
        };
        let Some(sample_trace_id) = correlation.sample_trace_id.as_deref() else {
            return;
        };
        let Some(&position) = self.trace_index.get(sample_trace_id) else {
            return;
        };
        if position != self.selected_trace {
            self.selected_trace = position;
            self.cached_detail = None;
        }
        self.selected_finding = 0;
        self.enter_detail(Panel::Correlations);
    }

    /// Handle Escape: go back to previous panel. Detail returns to
    /// `detail_origin` (Findings or Correlations) so the operator lands
    /// back where the drill-down started.
    pub fn escape(&mut self) {
        match self.active_panel {
            // Top-level panels (reached by Tab cycling, not drilled into):
            // ascend to the Analyze view. The active panel is preserved so
            // descending lands back here, and both honor the tab-bar "Esc up"
            // hint rather than leaving Correlations a dead end.
            Panel::Traces | Panel::Correlations => {
                self.view = View::Analyze;
                self.scroll_offset = 0;
            }
            Panel::Findings => self.active_panel = Panel::Traces,
            Panel::Detail => self.active_panel = self.detail_origin,
        }
    }

    /// Logical line count of the Analyze view body, used to clamp the
    /// scroll offset. Returns the value cached by `with_summary` so the
    /// per-keypress clamp does not rebuild the line vector (the degraded
    /// no-summary hint is short and never needs scrolling, hence 0).
    fn analyze_content_line_count(&self) -> u16 {
        self.analyze_line_count
    }

    /// Logical line count of the Explain view body (the cached span tree
    /// for the selected trace, or the short unavailability hint).
    fn explain_content_line_count(&self) -> u16 {
        match &self.cached_detail {
            Some((ct, text)) if *ct == self.selected_trace => {
                u16::try_from(text.lines().count()).unwrap_or(u16::MAX)
            }
            _ => 2,
        }
    }

    /// Per-severity finding counts `(critical, warning, info)` over all
    /// findings, for the Analyze view header.
    fn severity_counts(&self) -> (usize, usize, usize) {
        let mut counts = (0usize, 0usize, 0usize);
        for finding in &self.all_findings {
            match finding.severity {
                Severity::Critical => counts.0 += 1,
                Severity::Warning => counts.1 += 1,
                Severity::Info => counts.2 += 1,
            }
        }
        counts
    }

    /// Build the Analyze view body as owned ratatui lines. Mirrors the
    /// sections of the CLI report (`render.rs`) — analysis metadata,
    /// findings by severity, I/O waste, top offenders, quality gate — but
    /// as widgets. All externally-sourced strings (endpoint, service) are
    /// sanitized for the terminal. Falls back to a hint when no summary
    /// was supplied (e.g. an older daemon without `/api/export/report`).
    fn build_analyze_lines(&self) -> Vec<Line<'static>> {
        let dim = dim_style();
        let Some(summary) = &self.summary else {
            return vec![
                Line::from(Span::styled("Summary unavailable.".to_string(), dim)),
                Line::from(Span::styled(
                    "No analysis summary was supplied (older daemon without /api/export/report?)."
                        .to_string(),
                    dim,
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Press Enter to inspect traces and findings.".to_string(),
                    dim,
                )),
            ];
        };
        let gs = &summary.green_summary;
        let (crit, warn, info) = self.severity_counts();
        let total = self.all_findings.len();
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(vec![
            Span::styled("Traces analyzed: ".to_string(), dim),
            Span::raw(summary.analysis.traces_analyzed.to_string()),
            Span::styled("   Events: ".to_string(), dim),
            Span::raw(summary.analysis.events_processed.to_string()),
            Span::styled("   Duration: ".to_string(), dim),
            Span::raw(format!("{} ms", summary.analysis.duration_ms)),
        ]));
        lines.push(Line::from(""));

        lines.push(Line::from(vec![
            Span::styled("Findings: ".to_string(), dim),
            Span::styled(
                total.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(format!("{crit} critical"), Style::default().fg(Color::Red)),
            Span::raw(", "),
            Span::styled(
                format!("{warn} warning"),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(", "),
            Span::styled(format!("{info} info"), Style::default().fg(Color::Cyan)),
        ]));
        lines.push(Line::from(""));

        let band = gs.io_waste_ratio_band;
        lines.push(Line::from(vec![
            Span::styled("I/O waste ratio: ".to_string(), dim),
            // Percentage form, matching the CLI report (`print_green_summary`)
            // so the same metric does not read as a bare fraction here.
            Span::styled(
                format!("{:.1}%", gs.io_waste_ratio * 100.0),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                format!("({})", band.short_label()),
                Style::default().fg(interpret_band_color(band)),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Avoidable I/O: ".to_string(), dim),
            Span::raw(format!(
                "{} of {} ops",
                gs.avoidable_io_ops, gs.total_io_ops
            )),
        ]));
        if gs.energy_kwh > 0.0 {
            lines.push(Line::from(vec![
                Span::styled("Energy: ".to_string(), dim),
                Span::raw(format!("{:.6} kWh", gs.energy_kwh)),
                // `energy_model` is free text on the daemon snapshot path
                // (`query inspect`), so sanitize like the other daemon-sourced
                // strings below.
                Span::styled(
                    format!("  ({})", sanitize_for_terminal(&gs.energy_model)),
                    dim,
                ),
            ]));
        }
        // Structured CO2 report, mirroring the CLI `print_green_summary`
        // carbon block so the Analyze view does not silently omit the
        // headline carbon figures in carbon-configured deployments.
        if let Some(carbon) = gs.co2.as_ref() {
            // model/methodology are free `String`s on the daemon snapshot path
            // (`query inspect` reads them from /api/export/report), so sanitize
            // like the other daemon-sourced strings rendered above.
            let model = sanitize_for_terminal(&carbon.total.model);
            let methodology = sanitize_for_terminal(&carbon.total.methodology);
            lines.push(Line::from(Span::raw(format!(
                "Est. CO\u{2082}: {:.6} g (low {:.6}, high {:.6}, model {model})",
                carbon.total.mid, carbon.total.low, carbon.total.high,
            ))));
            lines.push(Line::from(Span::raw(format!(
                "Avoidable CO\u{2082}: {:.6} g (low {:.6}, high {:.6})",
                carbon.avoidable.mid, carbon.avoidable.low, carbon.avoidable.high,
            ))));
            lines.push(Line::from(Span::raw(format!(
                "Operational: {:.6} g   Embodied: {:.6} g   Methodology: {methodology}",
                carbon.operational_gco2, carbon.embodied_gco2,
            ))));
            if let Some(transport) = carbon.transport_gco2 {
                lines.push(Line::from(Span::raw(format!(
                    "Transport: {transport:.6} g (cross-region network bytes)"
                ))));
            }
        }
        lines.push(Line::from(""));

        if !gs.top_offenders.is_empty() {
            lines.push(Line::from(Span::styled(
                "Top offenders:".to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for offender in &gs.top_offenders {
                let oband = offender.io_intensity_band;
                let co2 = offender
                    .co2_grams
                    .map_or(String::new(), |c| format!(", {c:.6} gCO2"));
                lines.push(Line::from(vec![
                    Span::raw("  - ".to_string()),
                    Span::raw(sanitize_for_terminal(&offender.endpoint).into_owned()),
                    Span::styled(format!("  IIS {:.1} ", offender.io_intensity_score), dim),
                    Span::styled(
                        format!("({})", oband.short_label()),
                        Style::default().fg(interpret_band_color(oband)),
                    ),
                    Span::styled(
                        format!(
                            "  service: {}{}",
                            sanitize_for_terminal(&offender.service),
                            co2
                        ),
                        dim,
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }

        let gate = &summary.quality_gate;
        // An empty rule set means the gate was never evaluated, e.g. a daemon
        // `/api/export/report` snapshot (which hardcodes passed=true, rules=[]).
        // Render that honestly rather than a misleading green PASSED sitting
        // next to live critical findings under `query inspect`.
        let (gate_label, gate_color) = if gate.rules.is_empty() {
            ("not evaluated", Color::DarkGray)
        } else if gate.passed {
            ("PASSED", Color::Green)
        } else {
            ("FAILED", Color::Red)
        };
        lines.push(Line::from(vec![
            Span::styled(
                "Quality gate: ".to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                gate_label.to_string(),
                Style::default().fg(gate_color).add_modifier(Modifier::BOLD),
            ),
        ]));
        for rule in &gate.rules {
            let (rule_label, rule_color) = if rule.passed {
                ("PASS", Color::Green)
            } else {
                ("FAIL", Color::Red)
            };
            lines.push(Line::from(vec![
                Span::raw(format!("  - {}: ", sanitize_for_terminal(&rule.rule))),
                Span::styled(
                    format!("{:.2} (actual {:.2}) ", rule.threshold, rule.actual),
                    dim,
                ),
                Span::styled(rule_label.to_string(), Style::default().fg(rule_color)),
            ]));
        }

        // Mandatory uncertainty disclaimer whenever CO2 estimates are shown,
        // matching the CLI report.
        if gs.co2.is_some() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Note: CO\u{2082} estimates have ~2\u{00d7} multiplicative uncertainty (low = mid/2, high = mid\u{00d7}2). See docs/LIMITATIONS.md.".to_string(),
                dim,
            )));
        }
        // The bands use fixed heuristic thresholds, not the operator's config,
        // and the full per-region / scoring-config detail lives in the CLI
        // report. Flag both so the view is not mistaken for the full report.
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Note: (healthy/moderate/high/critical) bands use fixed heuristic thresholds, independent of your config overrides.".to_string(),
            dim,
        )));
        lines.push(Line::from(Span::styled(
            "Per-region carbon breakdown and scoring config: run `analyze` for the full report."
                .to_string(),
            dim,
        )));
        lines
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
        self.focus = step_focus(self.focus_cycle(), self.focus, 1_isize);
    }

    pub fn prev_field(&mut self) {
        self.focus = step_focus(self.focus_cycle(), self.focus, -1_isize);
    }

    /// Tab-cycle for the current modal mode. Unack mode only exposes
    /// Submit/Cancel buttons, Ack mode walks the full form.
    fn focus_cycle(&self) -> &'static [AckFormField] {
        match self.mode {
            AckModalMode::Unack { .. } => &UNACK_FOCUS_CYCLE,
            _ => &ACK_FOCUS_CYCLE,
        }
    }
}

#[cfg(feature = "daemon")]
const ACK_FOCUS_CYCLE: [AckFormField; 5] = [
    AckFormField::Reason,
    AckFormField::Expires,
    AckFormField::By,
    AckFormField::Submit,
    AckFormField::Cancel,
];

#[cfg(feature = "daemon")]
const UNACK_FOCUS_CYCLE: [AckFormField; 2] = [AckFormField::Submit, AckFormField::Cancel];

/// Move along a focus cycle by `step` positions (positive forward,
/// negative backward), wrapping at both ends. Falls back to the first
/// entry when `current` is not in the cycle (e.g. opening a Unack
/// modal while the previous focus was on Reason).
#[cfg(feature = "daemon")]
fn step_focus(cycle: &[AckFormField], current: AckFormField, step: isize) -> AckFormField {
    let len = i32::try_from(cycle.len()).unwrap_or(1).max(1);
    let step = i32::try_from(step).unwrap_or(0);
    let idx = cycle
        .iter()
        .position(|f| *f == current)
        .and_then(|p| i32::try_from(p).ok())
        .unwrap_or(0);
    let next = (idx + step).rem_euclid(len);
    let next_usize = usize::try_from(next).unwrap_or(0);
    cycle[next_usize]
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

/// Result of an ack/revoke roundtrip executed off the run loop.
/// The async task sends one of these through the outcome channel,
/// `apply_ack_outcome` applies it the next time the loop tick drains.
/// `Success.refreshed_acks` is `None` when the post-write refetch failed
/// (keep the previous snapshot), `Some(map)` otherwise even if empty
/// (legitimate "all acks expired" state).
#[cfg(feature = "daemon")]
#[derive(Debug)]
pub(crate) enum AckOutcome {
    Success {
        refreshed_acks: Option<HashMap<String, AckSource>>,
    },
    Failure {
        message: String,
    },
}

/// Snapshot of every modal/app field the spawned task needs.
/// Owned and `'static` so the future can outlive the run loop borrow.
/// Manual `Debug` so a future `tracing!("{payload:?}")` cannot leak the
/// API key, mirroring the discipline in `AuthHeader::Debug` and
/// `redact_endpoint`.
#[cfg(feature = "daemon")]
pub(crate) struct AckSubmitPayload {
    daemon_url: String,
    signature: String,
    api_key: Option<String>,
    op: AckSubmitOp,
}

#[cfg(feature = "daemon")]
impl std::fmt::Debug for AckSubmitPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AckSubmitPayload")
            .field("daemon_url", &self.daemon_url)
            .field("signature", &self.signature)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("op", &self.op)
            .finish()
    }
}

#[cfg(feature = "daemon")]
#[derive(Debug)]
pub(crate) enum AckSubmitOp {
    Create {
        by: String,
        reason: String,
        expires_at: Option<DateTime<Utc>>,
    },
    Revoke,
}

#[cfg(feature = "daemon")]
impl AckSubmitPayload {
    /// Capture the modal state and validate `expires_buf` synchronously.
    /// A parse error short-circuits before any spawn happens, so the
    /// `Validation` variant lands in `error_message` without a network
    /// round-trip.
    pub(crate) fn from_modal(app: &App) -> Result<Self, crate::ack::AckSubmitError> {
        let daemon_url = app.daemon_url.clone().ok_or_else(|| {
            crate::ack::AckSubmitError::Validation("daemon not configured".into())
        })?;
        let signature = signature_for_modal_mode(&app.ack_modal.mode)
            .map(str::to_string)
            .ok_or_else(|| crate::ack::AckSubmitError::Validation("modal not visible".into()))?;
        let api_key = app.api_key.clone();
        let op = match app.ack_modal.mode {
            AckModalMode::Ack { .. } => {
                let expires_at = if app.ack_modal.expires_buf.trim().is_empty() {
                    None
                } else {
                    match crate::ack::parse_expires(&app.ack_modal.expires_buf) {
                        Ok(dt) => Some(dt),
                        Err(e) => {
                            return Err(crate::ack::AckSubmitError::Validation(format!(
                                "expires: {e}"
                            )));
                        }
                    }
                };
                AckSubmitOp::Create {
                    by: app.ack_modal.by_buf.clone(),
                    reason: app.ack_modal.reason_buf.clone(),
                    expires_at,
                }
            }
            AckModalMode::Unack { .. } => AckSubmitOp::Revoke,
            AckModalMode::Hidden => unreachable!("guarded by signature_for_modal_mode above"),
        };
        Ok(Self {
            daemon_url,
            signature,
            api_key,
            op,
        })
    }
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

/// Best-effort terminal restore shared by the panic hook and
/// [`RawModeGuard`]. The raw-mode probe makes it idempotent: on a
/// panic, the hook restores first and the guard's later drop becomes a
/// no-op instead of re-sending `LeaveAlternateScreen` (whose implied
/// cursor restore could reposition over the just-printed panic
/// message).
fn restore_terminal_if_raw() {
    // Fail open: if the probe itself errors, attempt the restore anyway.
    // A spurious restore on a cooked terminal is harmless, a leaked raw
    // mode is not.
    if crossterm::terminal::is_raw_mode_enabled().unwrap_or(true) {
        let _ = disable_raw_mode();
        // DisableMouseCapture is harmless when capture was never enabled.
        let _ = crossterm::execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

/// Enable or disable terminal mouse capture, shared by both TUIs. On lets
/// them receive drag events for border resizing, at the cost of native
/// copy-paste. Best-effort: a write failure leaves the in-memory flag
/// authoritative, and teardown always issues `DisableMouseCapture`.
pub(crate) fn set_mouse_capture(on: bool) {
    let _ = if on {
        crossterm::execute!(io::stdout(), EnableMouseCapture)
    } else {
        crossterm::execute!(io::stdout(), DisableMouseCapture)
    };
}

/// RAII terminal restore: leaves the alternate screen and disables raw
/// mode on drop, so an `Err` between terminal setup and loop exit (a
/// path the panic hook does not cover) cannot leak a raw-mode shell.
pub(crate) struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        restore_terminal_if_raw();
    }
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
pub(crate) fn install_terminal_restore_panic_hook() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal_if_raw();
            prev_hook(info);
        }));
    });
}

/// Style for a tab label in a one-line tab bar: the active tab is
/// highlighted, the others dimmed. Shared by the inspect drill-down
/// bar and the `query monitor` header so the two TUIs stay visually
/// consistent.
pub(crate) fn tab_label_style(active: bool) -> Style {
    if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else {
        dim_style()
    }
}

/// Style for secondary / "dim" text shared by both TUIs. Uses the `DIM`
/// modifier on the terminal's default foreground rather than a fixed
/// gray: `Color::DarkGray` is legible on a light background but too dark
/// on a dark one, and `Color::Gray` is the reverse. Dimming the default
/// foreground adapts to either theme (light gray on dark, dark gray on
/// light).
pub(crate) fn dim_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// Concatenate rendered lines into plain text, for assertions. Shared
/// by the tui and monitor test modules.
#[cfg(test)]
pub(crate) fn line_text(lines: &[Line]) -> String {
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

/// Run the TUI event loop.
///
/// # Errors
///
/// Returns an error if terminal setup or event reading fails.
pub fn run(app: &mut App) -> io::Result<()> {
    install_terminal_restore_panic_hook();
    enable_raw_mode()?;
    // Restores raw mode + alternate screen on every exit path,
    // including an Err from the setup lines below.
    let _restore = RawModeGuard;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, app);

    terminal.show_cursor()?;
    result
}

/// Outcome of a single keystroke at the run-loop level. The loop
/// returns `Quit` to break out, `Continue` to repaint and wait for the
/// next event.
#[derive(Debug)]
enum KeyOutcome {
    Continue,
    Quit,
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    // Channel: spawned ack/revoke tasks send their outcome here, the
    // loop drains it before each redraw so the modal closes (or shows
    // the error) without blocking on the HTTP roundtrip.
    #[cfg(feature = "daemon")]
    let (tx_outcome, mut rx_outcome) = mpsc::unbounded_channel::<AckOutcome>();
    loop {
        #[cfg(feature = "daemon")]
        while let Ok(outcome) = rx_outcome.try_recv() {
            apply_ack_outcome(app, outcome);
        }
        // Pre-compute detail tree text (requires &mut self) before immutable
        // draw. Only the Inspect (Detail panel) and Explain views render it,
        // so skip the detect + build_tree + clone work in the Analyze view.
        if matches!(app.view, View::Inspect | View::Explain) {
            app.detail_tree_text();
        }
        terminal.draw(|f| draw(f, app))?;

        // Block on `event::read` when no ack is in flight (0fps idle,
        // matches the pre-refactor power profile). Poll with a short
        // timeout only when a spawned task may push an outcome we need
        // to apply quickly.
        #[cfg(feature = "daemon")]
        let submitting = app.ack_modal.submitting;
        #[cfg(not(feature = "daemon"))]
        let submitting = false;
        if submitting && !event::poll(Duration::from_millis(50))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                #[cfg(feature = "daemon")]
                let outcome = handle_keystroke(app, key.code, &tx_outcome);
                #[cfg(not(feature = "daemon"))]
                let outcome = handle_keystroke(app, key.code);
                if matches!(outcome, KeyOutcome::Quit) {
                    return Ok(());
                }
            }
            // Border dragging, only when a panel drag is currently
            // accepted (Inspect view, no modal up); the stored area is
            // stale otherwise. Resize repaints on the next loop turn,
            // which redraws unconditionally.
            Event::Mouse(me) if app.mouse_mode && app.accepts_panel_drag() => {
                handle_mouse(app, me);
            }
            _ => {}
        }
    }
}

/// Route a mouse event to the border-drag state machine. Motion updates
/// the hovered border so the resize affordance highlights under the cursor.
fn handle_mouse(app: &mut App, me: MouseEvent) {
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => app.begin_drag(me.column, me.row),
        MouseEventKind::Drag(MouseButton::Left) => app.apply_drag(me.column, me.row),
        MouseEventKind::Up(MouseButton::Left) => {
            app.drag = None;
            app.hover = app.hit_test(me.column, me.row);
        }
        MouseEventKind::Moved => app.hover = app.hit_test(me.column, me.row),
        _ => {}
    }
}

/// Dispatch a single keystroke. Modal-visible keys route to the form
/// handler, otherwise the standard panel-navigation keys + the
/// `a`/`u` ack shortcuts apply.
fn handle_keystroke(
    app: &mut App,
    code: KeyCode,
    #[cfg(feature = "daemon")] tx_outcome: &mpsc::UnboundedSender<AckOutcome>,
) -> KeyOutcome {
    #[cfg(feature = "daemon")]
    if app.ack_modal.is_visible() {
        dispatch_modal_key(app, code, tx_outcome);
        return KeyOutcome::Continue;
    }
    // Mouse mode is app-global state: toggle it from any drill-down view so
    // enabling it in Inspect then stepping into Explain can't trap capture
    // on. The standalone Disclose preview keeps its own key map.
    if app.disclose.is_none() && code == KeyCode::Char('m') {
        app.toggle_mouse_mode();
        set_mouse_capture(app.mouse_mode);
        return KeyOutcome::Continue;
    }
    let prev_view = app.view;
    let outcome = match app.view {
        View::Analyze => dispatch_analyze_key(app, code),
        View::Inspect => dispatch_panel_key(app, code),
        View::Explain => dispatch_explain_key(app, code),
        View::Disclose => dispatch_disclose_key(app, code),
    };
    // A view change invalidates the hovered/dragged border, which is only
    // tracked while in Inspect; clear it so re-entry doesn't paint a
    // phantom highlight with no cursor under it.
    if app.view != prev_view {
        app.hover = None;
        app.drag = None;
    }
    outcome
}

/// Keys for the standalone Disclose preview: `g` cycles granularity,
/// `\u{2190}/\u{2192}` (`h`/`l`) step the period (or the active custom edge by a day),
/// `[`/`]` move the active custom edge by a month, `Tab` switches the
/// custom edge, `i`/`c` toggle intent/confidentiality, `\u{2191}/\u{2193}` (`j`/`k`)
/// scroll the summary. No view switching (the tab is autonomous).
fn dispatch_disclose_key(app: &mut App, code: KeyCode) -> KeyOutcome {
    let Some(state) = app.disclose.as_mut() else {
        return KeyOutcome::Continue;
    };
    match code {
        KeyCode::Char('q') => return KeyOutcome::Quit,
        KeyCode::Char('g') => state.cycle_granularity(),
        KeyCode::Left | KeyCode::Char('h') => state.step(false),
        KeyCode::Right | KeyCode::Char('l') => state.step(true),
        KeyCode::Char('[') => state.step_month(false),
        KeyCode::Char(']') => state.step_month(true),
        KeyCode::Tab | KeyCode::BackTab => state.toggle_custom_field(),
        KeyCode::Char('i') => state.toggle_intent(),
        KeyCode::Char('c') => state.toggle_confidentiality(),
        KeyCode::Up | KeyCode::Char('k') => state.scroll(false),
        KeyCode::Down | KeyCode::Char('j') => state.scroll(true),
        _ => {}
    }
    KeyOutcome::Continue
}

/// Keys for the Analyze view: scroll the summary, Enter descends to the
/// Inspect view (keeping the active panel), Esc is a no-op (top of the
/// drill-down).
fn dispatch_analyze_key(app: &mut App, code: KeyCode) -> KeyOutcome {
    match code {
        KeyCode::Char('q') => return KeyOutcome::Quit,
        KeyCode::Up | KeyCode::Char('k') => {
            app.scroll_offset = app.scroll_offset.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let max = app.analyze_content_line_count().saturating_sub(1);
            if app.scroll_offset < max {
                app.scroll_offset = app.scroll_offset.saturating_add(1);
            }
        }
        KeyCode::Enter => {
            // Descend to Inspect, keeping active_panel as-is so an
            // Esc-then-Enter round-trip lands back on the panel ascended from
            // (Traces or Correlations). A fresh `analyze --tui` launch already
            // has active_panel == Traces by default.
            app.view = View::Inspect;
            app.scroll_offset = 0;
        }
        _ => {}
    }
    KeyOutcome::Continue
}

/// Keys for the Explain view: scroll the span tree, Esc ascends back to
/// the Inspect view's Detail panel.
fn dispatch_explain_key(app: &mut App, code: KeyCode) -> KeyOutcome {
    match code {
        KeyCode::Char('q') => return KeyOutcome::Quit,
        KeyCode::Up | KeyCode::Char('k') => {
            app.scroll_offset = app.scroll_offset.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let max = app.explain_content_line_count().saturating_sub(1);
            if app.scroll_offset < max {
                app.scroll_offset = app.scroll_offset.saturating_add(1);
            }
        }
        KeyCode::Esc => {
            app.view = View::Inspect;
            app.active_panel = Panel::Detail;
            app.scroll_offset = 0;
        }
        _ => {}
    }
    KeyOutcome::Continue
}

#[cfg(feature = "daemon")]
fn dispatch_modal_key(
    app: &mut App,
    code: KeyCode,
    tx_outcome: &mpsc::UnboundedSender<AckOutcome>,
) {
    match handle_modal_key(&mut app.ack_modal, code) {
        ModalAction::None => {}
        ModalAction::Cancel => app.ack_modal.close(),
        ModalAction::Submit => submit_ack_modal(app, tx_outcome),
    }
}

fn dispatch_panel_key(app: &mut App, code: KeyCode) -> KeyOutcome {
    match code {
        KeyCode::Char('q') => return KeyOutcome::Quit,
        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => app.next_panel(),
        KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => app.prev_panel(),
        KeyCode::Enter => app.enter(),
        KeyCode::Esc => app.escape(),
        KeyCode::Char('r') => app.reset_layout(),
        #[cfg(feature = "daemon")]
        KeyCode::Char('a') => open_ack_modal_for_current(app, false),
        #[cfg(feature = "daemon")]
        KeyCode::Char('u') => open_ack_modal_for_current(app, true),
        _ => {}
    }
    KeyOutcome::Continue
}

#[cfg(feature = "daemon")]
fn open_ack_modal_for_current(app: &mut App, revoke: bool) {
    if app.daemon_url.is_none() {
        return;
    }
    let Some(sig) = app.current_finding().map(|f| f.signature.clone()) else {
        return;
    };
    if revoke {
        app.ack_modal.open_unack(sig);
    } else {
        app.ack_modal.open_ack(sig);
    }
}

fn draw(f: &mut Frame, app: &App) {
    // One-line tab bar on top, the active view fills the middle, and a
    // centered brand credit line is pinned to the bottom on every view.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_tab_bar(f, app, outer[0]);
    match app.view {
        View::Analyze => draw_analyze_view(f, app, outer[1]),
        View::Inspect => draw_inspect_view(f, app, outer[1]),
        View::Explain => draw_explain_view(f, app, outer[1]),
        View::Disclose => draw_disclose_view(f, app, outer[1]),
    }
    draw_brand_footer(f, outer[2]);

    #[cfg(feature = "daemon")]
    if app.ack_modal.is_visible() {
        draw_ack_modal(f, app);
    }
}

/// Centered "Powered by perf-sentinel (...)" credit pinned to the bottom of
/// every view, mirroring the HTML dashboard footer. "perf-sentinel" and the
/// repo link are brand green and the link is underlined; "Powered by" and the
/// parentheses use the dimmed default foreground so they stay legible on both
/// light and dark terminals.
fn draw_brand_footer(f: &mut Frame, area: Rect) {
    let green = Style::default().fg(Color::Rgb(11, 166, 113));
    let green_link = green.add_modifier(Modifier::UNDERLINED);
    let line = Line::from(vec![
        Span::styled("Powered by ", dim_style()),
        Span::styled("perf-sentinel", green),
        Span::styled(" (", dim_style()),
        Span::styled("github.com/robintra/perf-sentinel", green_link),
        Span::styled(")", dim_style()),
    ]);
    f.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}

/// Top tab bar: the three views with the active one highlighted, plus the
/// view-level navigation hint. Purely a visual orientation aid — the keys
/// that switch views are Enter (down) and Esc (up), bound per view.
fn draw_tab_bar(f: &mut Frame, app: &App, area: Rect) {
    let dim = dim_style();
    // The standalone Disclose tab replaces the drill-down bar entirely.
    if app.disclose.is_some() {
        let spans = vec![
            Span::raw(" "),
            Span::styled(
                " Disclose ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            ),
            Span::styled(
                "    g granularity \u{00b7} \u{2190}/\u{2192} period \u{00b7} i intent \u{00b7} c confidentiality \u{00b7} q quit"
                    .to_string(),
                dim,
            ),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let mut spans = vec![Span::raw(" ")];
    for (i, (view, label)) in [
        (View::Analyze, "Analyze"),
        (View::Inspect, "Inspect"),
        (View::Explain, "Explain"),
    ]
    .iter()
    .enumerate()
    {
        if i > 0 {
            spans.push(Span::styled(" \u{25b8} ", dim));
        }
        spans.push(Span::styled(
            format!(" {label} "),
            tab_label_style(app.view == *view),
        ));
    }
    spans.push(Span::styled(
        "    Enter \u{2193} \u{00b7} Esc \u{2191} \u{00b7} q quit".to_string(),
        dim,
    ));
    // The MOUSE badge shows in every drill-down view so capture can never
    // be silently trapped on; the drag/reset hint is Inspect-only.
    if app.mouse_mode {
        // Unstyled gap so the reversed badge doesn't butt against "q quit".
        spans.push(Span::raw("  "));
        spans.push(Span::styled(" MOUSE ", tab_label_style(true)));
        spans.push(Span::styled(
            if app.view == View::Inspect {
                " drag \u{00b7} r reset \u{00b7} m off"
            } else {
                " m off"
            },
            dim,
        ));
    } else if app.view == View::Inspect {
        spans.push(Span::styled(" \u{00b7} m resize", dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The Inspect view: the 4-panel browser (traces, findings, correlations,
/// detail). Body of the former top-level `draw`.
fn draw_inspect_view(f: &mut Frame, app: &App, area: Rect) {
    // Stored for the next frame's mouse hit-testing (see `begin_drag`).
    app.inspect_area.set(area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(app.inspect_rows[0]),
            Constraint::Percentage(app.inspect_rows[1]),
        ])
        .split(area);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(app.inspect_cols[0]),
            Constraint::Percentage(app.inspect_cols[1]),
            Constraint::Percentage(app.inspect_cols[2]),
        ])
        .split(chunks[0]);

    draw_traces_panel(f, app, top[0]);
    draw_findings_panel(f, app, top[1]);
    draw_correlations_panel(f, app, top[2]);
    draw_detail_panel(f, app, chunks[1]);

    // Light up the border under the cursor (or being dragged) so the user
    // sees the grab line, since the OS mouse pointer can't be changed.
    if app.mouse_mode
        && let Some(t) = app.resize_target()
    {
        let hl = resize_highlight_style();
        match t.axis {
            // Divider between the top row and the Detail panel.
            Axis::Vertical => highlight_hline(f, area.x, chunks[1].y, area.width, hl),
            // Shared edge between top panel b and b + 1.
            Axis::Horizontal => {
                highlight_vline(f, top[t.boundary + 1].x, chunks[0].y, chunks[0].height, hl);
            }
        }
    }
}

/// The Analyze view: `GreenOps` summary dashboard, scrollable.
fn draw_analyze_view(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Analyze ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let paragraph = Paragraph::new(app.build_analyze_lines())
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));
    f.render_widget(paragraph, area);
}

/// The Explain view: the selected trace's annotated span tree, full
/// screen and scrollable. Reuses the per-trace tree cached for the Detail
/// panel (pre-computed before each draw in `run_loop`).
fn draw_explain_view(f: &mut Frame, app: &App, area: Rect) {
    let trace_id = app
        .trace_ids
        .get(app.selected_trace)
        .map_or("-", String::as_str);
    let block = Block::default()
        .title(format!(
            " Explain \u{00b7} {} ",
            sanitize_for_terminal(trace_id)
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let lines: Vec<Line> = match &app.cached_detail {
        // Borrow the cached tree lines for the frame instead of allocating a
        // fresh String per visible line on every repaint.
        Some((ct, text)) if *ct == app.selected_trace => text.lines().map(Line::from).collect(),
        _ => vec![
            Line::from(Span::styled(
                "Span tree not available for this trace.",
                dim_style(),
            )),
            Line::from(Span::styled(
                "Reports do not carry raw spans. Launch `inspect --input <events>.json` or `query inspect`.",
                dim_style(),
            )),
        ],
    };

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));
    f.render_widget(paragraph, area);
}

/// The standalone Disclose preview view: a fixed settings header, the
/// scrollable aggregated summary, and a footer with the equivalent
/// `disclose` command to copy.
fn draw_disclose_view(f: &mut Frame, app: &App, area: Rect) {
    let Some(state) = app.disclose.as_ref() else {
        return;
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(0),
            Constraint::Length(4),
        ])
        .split(area);

    draw_disclose_settings(f, state, chunks[0]);

    let summary_lines: Vec<Line> = state
        .summary_lines()
        .iter()
        .map(|l| Line::from(Span::styled(l.text.clone(), tone_style(l.tone))))
        .collect();
    let summary = Paragraph::new(summary_lines)
        .block(
            Block::default()
                .title(" Summary ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset(), 0));
    f.render_widget(summary, chunks[1]);

    let command = sanitize_for_terminal(&state.equivalent_command()).into_owned();
    let footer = Paragraph::new(command)
        .block(
            Block::default()
                .title(" Equivalent command (run it to write the hashed report) ")
                .borders(Borders::ALL)
                .border_style(dim_style()),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(footer, chunks[2]);
}

fn draw_disclose_settings(f: &mut Frame, state: &DiscloseState, area: Rect) {
    let dim = dim_style();
    let cyan = Style::default().fg(Color::Cyan);
    let (from, to) = state.resolved_dates();

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Granularity: ", dim),
            Span::styled(
                format!("\u{2039} {} \u{203a}", state.granularity().label()),
                cyan.add_modifier(Modifier::BOLD),
            ),
            Span::styled("    Intent: ", dim),
            Span::styled(intent_label(state.intent()), Style::default()),
            Span::styled("    Confidentiality: ", dim),
            Span::styled(
                confidentiality_label(state.confidentiality()),
                Style::default(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Period: ", dim),
            Span::styled(
                format!("{from} \u{2192} {to}"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  ({} days)", state.days_covered()), dim),
        ]),
    ];

    let archive = match state.archive_range() {
        Some((min, max)) => format!("Archive: {} .. {}", min.date_naive(), max.date_naive()),
        None => "Archive: empty".to_string(),
    };
    lines.push(Line::from(Span::styled(archive, dim)));

    if state.granularity() == Granularity::Custom {
        let (from_focus, to_focus) = match state.custom_field() {
            CustomField::From => (cyan.add_modifier(Modifier::REVERSED), dim),
            CustomField::To => (dim, cyan.add_modifier(Modifier::REVERSED)),
        };
        lines.push(Line::from(vec![
            Span::styled("Editing: ", dim),
            Span::styled(" from ", from_focus),
            Span::styled("  ", dim),
            Span::styled(" to ", to_focus),
            Span::styled(
                "    Tab switch \u{00b7} \u{2190}/\u{2192} \u{00b1}1 day \u{00b7} [ ] \u{00b1}1 month",
                dim,
            ),
        ]));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Settings ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(paragraph, area);
}

fn intent_label(intent: ReportIntent) -> &'static str {
    match intent {
        ReportIntent::Internal => "internal",
        ReportIntent::Official => "official",
        ReportIntent::Audited => "audited",
    }
}

fn confidentiality_label(confidentiality: Confidentiality) -> &'static str {
    match confidentiality {
        Confidentiality::Internal => "internal (G1)",
        Confidentiality::Public => "public (G2)",
    }
}

fn tone_style(tone: Tone) -> Style {
    match tone {
        Tone::Header => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        Tone::Normal => Style::default(),
        Tone::Dim => dim_style(),
        Tone::Good => Style::default().fg(Color::Green),
        Tone::Warn => Style::default().fg(Color::Yellow),
        Tone::Bad => Style::default().fg(Color::Red),
    }
}

/// Map an interpretation band to a terminal color, matching the CLI
/// report's `interpret_color` (`render.rs`) exactly so the same band reads
/// the same in `analyze` stdout and the TUI: Critical red, High yellow,
/// Moderate uncolored (it is a non-empirical rule of thumb, kept neutral so
/// it does not compete with High), Healthy green.
fn interpret_band_color(level: InterpretationLevel) -> Color {
    match level {
        InterpretationLevel::Healthy => Color::Green,
        InterpretationLevel::Moderate => Color::Reset,
        InterpretationLevel::High => Color::Yellow,
        InterpretationLevel::Critical => Color::Red,
    }
}

fn panel_style(app: &App, panel: Panel) -> Style {
    if app.active_panel == panel {
        Style::default().fg(Color::Cyan)
    } else {
        dim_style()
    }
}

/// Brand-accent style for the highlighted (hovered/dragged) resize border.
pub(crate) fn resize_highlight_style() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

/// Highlight a draggable VERTICAL border: a terminal can't change the OS
/// mouse pointer, so the in-app affordance (same idea as ratatui-hypertile)
/// is to redraw the grab line heavy + accent, with a `\u{256b}` handle at
/// its midpoint. The handle is a box-drawing glyph (guaranteed single-cell
/// width, unlike arrow glyphs which some terminals render double-width),
/// its horizontal stubs hinting the left-right drag. Skips the panel
/// corners (first/last cell) so they stay `\u{250c}`/`\u{2514}`.
pub(crate) fn highlight_vline(f: &mut Frame, x: u16, y: u16, height: u16, style: Style) {
    let buf = f.buffer_mut();
    let mid = y.saturating_add(height / 2);
    for row in y.saturating_add(1)..y.saturating_add(height).saturating_sub(1) {
        if let Some(cell) = buf.cell_mut((x, row)) {
            cell.set_style(style)
                .set_symbol(if row == mid { "\u{256b}" } else { "\u{2503}" });
        }
    }
}

/// Highlight a draggable HORIZONTAL border, heavy + accent with a
/// `\u{256a}` handle at its midpoint (vertical stubs hint the up-down drag);
/// see [`highlight_vline`].
pub(crate) fn highlight_hline(f: &mut Frame, x: u16, y: u16, width: u16, style: Style) {
    let buf = f.buffer_mut();
    let mid = x.saturating_add(width / 2);
    for col in x.saturating_add(1)..x.saturating_add(width).saturating_sub(1) {
        if let Some(cell) = buf.cell_mut((col, y)) {
            cell.set_style(style)
                .set_symbol(if col == mid { "\u{256a}" } else { "\u{2501}" });
        }
    }
}

fn draw_traces_panel(f: &mut Frame, app: &App, area: Rect) {
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

fn draw_findings_panel(f: &mut Frame, app: &App, area: Rect) {
    let indices = app.current_finding_indices();
    // Inner width inside the block borders, used to pick the ack suffix form.
    #[cfg_attr(not(feature = "daemon"), allow(unused_variables))]
    let inner_width = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = indices
        .iter()
        .enumerate()
        .map(|(i, &idx)| {
            let finding = &app.all_findings[idx];
            let severity_color = severity_color(&finding.severity);
            let type_label = finding_type_label(&finding.finding_type);
            let sev_label = severity_label(&finding.severity);
            let idx_label = format!("[{}] ", i + 1);
            // Only the daemon-gated acked-by suffix mutates the vec.
            #[cfg_attr(not(feature = "daemon"), allow(unused_mut))]
            let mut spans = vec![
                Span::styled(idx_label.clone(), dim_style()),
                Span::styled(
                    format!("{type_label} "),
                    Style::default()
                        .fg(severity_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(sev_label, Style::default().fg(severity_color)),
            ];
            #[cfg(feature = "daemon")]
            if let Some(ack) = app.acks_by_signature.get(&finding.signature) {
                let by = match ack {
                    AckSource::Toml {
                        acknowledged_by, ..
                    } => acknowledged_by.as_str(),
                    AckSource::Daemon { by, .. } => by.as_str(),
                };
                // Prefer the full "[acked by <who>]" suffix, but fall back to a
                // compact "[acked]" when the panel is too narrow to fit it, so
                // the ack status stays visible even in a slim Findings column.
                let full = format!("[acked by {}]", sanitize_for_terminal(by));
                let base = idx_label.chars().count()
                    + type_label.chars().count()
                    + 1
                    + sev_label.chars().count();
                let suffix = if base + 1 + full.chars().count() <= inner_width {
                    full
                } else {
                    "[acked]".to_string()
                };
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    suffix,
                    dim_style().add_modifier(Modifier::ITALIC),
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

fn draw_correlations_panel(f: &mut Frame, app: &App, area: Rect) {
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
        .style(dim_style());
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
                Span::styled(format!("{:.0}ms ", c.median_lag_ms), dim_style()),
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

fn draw_detail_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Detail ")
        .borders(Borders::ALL)
        .border_style(panel_style(app, Panel::Detail));

    let Some(finding) = app.current_finding() else {
        let help = Paragraph::new("Select a finding to see details.\n\nKeys: ↑↓/jk navigate · ←→/hl/Tab panels · Enter deeper · Esc up · q quit")
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
            Span::styled("Template: ", dim_style()),
            Span::raw(&finding.pattern.template),
        ]),
        Line::from(vec![
            Span::styled("Occurrences: ", dim_style()),
            Span::raw(format!(
                "{}, {} distinct params, {}ms window",
                finding.pattern.occurrences,
                finding.pattern.distinct_params,
                finding.pattern.window_ms
            )),
        ]),
        Line::from(vec![
            Span::styled("Service: ", dim_style()),
            Span::raw(&finding.service),
        ]),
        Line::from(vec![
            Span::styled("Endpoint: ", dim_style()),
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
                    Span::styled("Source:   ", dim_style()),
                    Span::raw(src),
                ]),
            );
        }
    }

    if let Some(ref impact) = finding.green_impact {
        lines.push(Line::from(vec![
            Span::styled("Extra I/O: ", dim_style()),
            Span::raw(format!("{} avoidable ops", impact.estimated_extra_io_ops)),
        ]));
    }

    // Span tree is pre-computed before draw, cached per trace.
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
            dim_style(),
        )));
        lines.push(Line::from(Span::styled(
            "  - perf-sentinel inspect --input <events>.json  (raw events)",
            dim_style(),
        )));
        lines.push(Line::from(Span::styled(
            "  - perf-sentinel query inspect                  (live daemon)",
            dim_style(),
        )));
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));

    f.render_widget(paragraph, area);
}

#[cfg(feature = "daemon")]
fn draw_ack_modal(f: &mut Frame, app: &App) {
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
fn draw_ack_form(f: &mut Frame, app: &App, area: Rect, signature: &str) {
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

    render_finding_signature_line(f, rows[0], signature);
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
fn draw_unack_form(f: &mut Frame, app: &App, area: Rect, signature: &str) {
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
    render_finding_signature_line(f, rows[0], signature);
    f.render_widget(
        Paragraph::new("Revoke this acknowledgment? Press Enter to confirm, Esc to cancel.")
            .style(Style::default().fg(Color::Yellow)),
        rows[2],
    );
    render_modal_buttons(f, rows[4], &app.ack_modal);
    render_modal_footer(f, rows[5], app.ack_modal.error_message.as_deref());
}

#[cfg(feature = "daemon")]
fn render_finding_signature_line(f: &mut Frame, area: Rect, signature: &str) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Finding: ", dim_style()),
            Span::raw(sanitize_for_terminal(signature)),
        ])),
        area,
    );
}

#[cfg(feature = "daemon")]
fn render_field_label(
    f: &mut Frame,
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
        dim_style()
    };
    f.render_widget(Paragraph::new(label).style(style), area);
}

#[cfg(feature = "daemon")]
fn render_field_input(f: &mut Frame, area: Rect, value: &str, focused: bool) {
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
        // Focused field is a highlight block: white on an imposed dark
        // background reads on both light and dark terminals.
        Style::default().fg(Color::White).bg(Color::DarkGray)
    } else {
        // Reset (not White): the unfocused field takes the terminal's
        // default foreground, so it stays legible on a light background.
        Style::default().fg(Color::Reset)
    };
    f.render_widget(Paragraph::new(display).style(style), area);
}

#[cfg(feature = "daemon")]
fn render_modal_buttons(f: &mut Frame, area: Rect, modal: &AckModalState) {
    let submit_label = if modal.submitting {
        "[Submitting...]"
    } else {
        "[Submit]"
    };
    let line = Line::from(vec![
        Span::styled(
            submit_label,
            button_style(Color::Green, modal.focus == AckFormField::Submit),
        ),
        Span::raw("   "),
        Span::styled(
            "[Cancel]",
            button_style(Color::Red, modal.focus == AckFormField::Cancel),
        ),
        Span::raw("   "),
        Span::styled("Tab/Shift-Tab to switch, Esc to cancel", dim_style()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Style a modal action button. Focused buttons reverse the color
/// (black foreground on the action color background) and bold; the
/// unfocused state uses the action color as foreground only.
#[cfg(feature = "daemon")]
fn button_style(action_color: Color, focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Black)
            .bg(action_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(action_color)
    }
}

#[cfg(feature = "daemon")]
fn render_modal_footer(f: &mut Frame, area: Rect, error: Option<&str>) {
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

/// Validate the modal state and spawn the async ack/revoke roundtrip on
/// the tokio runtime. Returns immediately so the run loop keeps redrawing
/// while the request is in flight. The result lands later through
/// `tx_outcome`, which `apply_ack_outcome` consumes the next time the
/// loop tick drains.
#[cfg(feature = "daemon")]
fn submit_ack_modal(app: &mut App, tx_outcome: &mpsc::UnboundedSender<AckOutcome>) {
    // Gate concurrent submits: a held Enter (autorepeat) or a double tap
    // would otherwise spawn two roundtrips and the second hits HTTP 409.
    if app.ack_modal.submitting {
        return;
    }
    if !app.ack_modal.is_visible() {
        tracing::error!(target: "tui::ack", "submit called on hidden modal, dropped");
        return;
    }
    let payload = match AckSubmitPayload::from_modal(app) {
        Ok(p) => p,
        Err(e) => {
            app.ack_modal.error_message = Some(e.to_string());
            return;
        }
    };
    app.ack_modal.submitting = true;
    let tx = tx_outcome.clone();
    tokio::runtime::Handle::current().spawn(execute_ack_submit(payload, tx));
}

/// Execute the POST/DELETE roundtrip and the post-success refetch, then
/// push a single `AckOutcome` through the channel. Refetch failure on a
/// successful write keeps the previous `acks_by_signature` snapshot, the
/// indicator may briefly look stale but the write itself succeeded.
#[cfg(feature = "daemon")]
async fn execute_ack_submit(payload: AckSubmitPayload, tx: mpsc::UnboundedSender<AckOutcome>) {
    let write_result = match &payload.op {
        AckSubmitOp::Create {
            by,
            reason,
            expires_at,
        } => {
            crate::ack::post_ack_via_daemon(
                &payload.daemon_url,
                &payload.signature,
                by,
                reason,
                *expires_at,
                payload.api_key.as_deref(),
            )
            .await
        }
        AckSubmitOp::Revoke => {
            crate::ack::delete_ack_via_daemon(
                &payload.daemon_url,
                &payload.signature,
                payload.api_key.as_deref(),
            )
            .await
        }
    };
    let outcome = match write_result {
        Ok(()) => {
            match refetch_acks_from_daemon(&payload.daemon_url, payload.api_key.as_deref()).await {
                Ok(refreshed_acks) => AckOutcome::Success {
                    refreshed_acks: Some(refreshed_acks),
                },
                Err(e) => {
                    tracing::warn!(
                        error = %sanitize_for_terminal(&e),
                        "ack submit succeeded but refetch failed, indicator may be stale"
                    );
                    AckOutcome::Success {
                        refreshed_acks: None,
                    }
                }
            }
        }
        Err(crate::ack::AckSubmitError::Unauthorized) => AckOutcome::Failure {
            message: "API key required: set PERF_SENTINEL_DAEMON_API_KEY or pass \
                 --api-key-file when launching `query inspect`."
                .to_string(),
        },
        Err(e) => AckOutcome::Failure {
            message: e.to_string(),
        },
    };
    if let Err(e) = tx.send(outcome) {
        // Receiver dropped because the run loop has already exited
        // (operator pressed `q` mid-flight). Trace it so a future
        // regression on shutdown ordering is observable.
        tracing::trace!(error = %e, "ack outcome dropped, run loop has exited");
    }
}

/// Apply an `AckOutcome` to the app state. Idempotent against an
/// already-closed modal (Esc-while-submitting): Success still refreshes
/// the global ack map when present so the Findings indicator updates,
/// Failure logs at WARN before being dropped so a misconfigured
/// `[daemon.ack] api_key` does not stay hidden in the operator's logs.
#[cfg(feature = "daemon")]
fn apply_ack_outcome(app: &mut App, outcome: AckOutcome) {
    match outcome {
        AckOutcome::Success { refreshed_acks } => {
            // None signals refetch failed, keep the previous snapshot.
            // Some(map), even empty, replaces it (legitimate "no acks").
            if let Some(refreshed) = refreshed_acks {
                app.acks_by_signature = refreshed;
            }
            if app.ack_modal.is_visible() {
                app.ack_modal.close();
            }
        }
        AckOutcome::Failure { message } => {
            if app.ack_modal.is_visible() {
                app.ack_modal.error_message = Some(message);
                app.ack_modal.submitting = false;
            } else {
                tracing::warn!(
                    target: "tui::ack",
                    error = %sanitize_for_terminal(&message),
                    "ack outcome dropped after modal cancelled, may mask 401/403"
                );
            }
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
mod tests;
