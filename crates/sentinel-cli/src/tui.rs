//! Terminal UI for interactive trace and finding inspection.
//!
//! Provides a 3-panel layout: traces list, findings for selected trace,
//! and finding detail with span tree.

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use sentinel_core::correlate::Trace;
#[cfg(feature = "daemon")]
use sentinel_core::daemon::query_api::AckSource;
use sentinel_core::detect::correlate_cross::CrossTraceCorrelation;
use sentinel_core::detect::{DetectConfig, Finding, FindingType, Severity};
use sentinel_core::explain;
use sentinel_core::text_safety::sanitize_for_terminal;

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
            detail_origin: Panel::Findings,
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

    /// Handle Enter key: drill into the next panel.
    ///
    /// - Traces -> Findings (when there are findings)
    /// - Findings -> Detail
    /// - Correlations -> Detail (jumps to `sample_trace_id` if known locally)
    /// - Detail -> no-op
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
            Panel::Detail => {}
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
            Panel::Traces | Panel::Correlations => {}
            Panel::Findings => self.active_panel = Panel::Traces,
            Panel::Detail => self.active_panel = self.detail_origin,
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

/// Outcome of a single keystroke at the run-loop level. The loop
/// returns `Quit` to break out, `Continue` to repaint and wait for the
/// next event.
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
        // Pre-compute detail tree text (requires &mut self) before immutable draw
        app.detail_tree_text();
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
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            #[cfg(feature = "daemon")]
            let outcome = handle_keystroke(app, key.code, &tx_outcome);
            #[cfg(not(feature = "daemon"))]
            let outcome = handle_keystroke(app, key.code);
            if matches!(outcome, KeyOutcome::Quit) {
                return Ok(());
            }
        }
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
    dispatch_panel_key(app, code)
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
        KeyCode::Right | KeyCode::Tab => app.next_panel(),
        KeyCode::Left | KeyCode::BackTab => app.prev_panel(),
        KeyCode::Enter => app.enter(),
        KeyCode::Esc => app.escape(),
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

fn draw_detail_panel(f: &mut Frame, app: &App, area: Rect) {
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
            Span::styled("Finding: ", Style::default().fg(Color::DarkGray)),
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
        Style::default().fg(Color::DarkGray)
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
        Style::default().fg(Color::White).bg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
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
        Span::styled(
            "Tab/Shift-Tab to switch, Esc to cancel",
            Style::default().fg(Color::DarkGray),
        ),
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
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rendered = render_buffer_to_string(&buffer);
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
}
