//! Ack/revoke modal overlay: state, key handling, rendering and the async submit roundtrip.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use sentinel_core::daemon::query_api::AckSource;
use sentinel_core::text_safety::sanitize_for_terminal;
use tokio::sync::mpsc;

use super::{App, dim_style};

/// State for the ack/revoke modal overlay. Lives on `App.ack_modal`.
/// `Default` is the hidden state. The modal is opened by `open_ack` /
/// `open_unack` from the `a` and `u` key handlers in `run_loop`.
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum AckModalMode {
    #[default]
    Hidden,
    /// Creating an ack for the given signature.
    Ack { signature: String },
    /// Revoking an existing ack for the given signature.
    Unack { signature: String },
}

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
// reason / by anyway. These caps only keep the modal layout stable.
pub(super) const REASON_MAX: usize = 256;
pub(super) const EXPIRES_MAX: usize = 64;
pub(super) const BY_MAX: usize = 128;

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

pub(super) const ACK_FOCUS_CYCLE: [AckFormField; 5] = [
    AckFormField::Reason,
    AckFormField::Expires,
    AckFormField::By,
    AckFormField::Submit,
    AckFormField::Cancel,
];

pub(super) const UNACK_FOCUS_CYCLE: [AckFormField; 2] =
    [AckFormField::Submit, AckFormField::Cancel];

/// Move along a focus cycle by `step` positions (positive forward,
/// negative backward), wrapping at both ends. Falls back to the first
/// entry when `current` is not in the cycle (e.g. opening a Unack
/// modal while the previous focus was on Reason).
pub(super) fn step_focus(
    cycle: &[AckFormField],
    current: AckFormField,
    step: isize,
) -> AckFormField {
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
#[derive(Debug, PartialEq, Eq)]
pub enum ModalAction {
    None,
    Cancel,
    Submit,
}

/// Result of an ack/revoke roundtrip executed off the run loop.
/// The async task sends one of these through the outcome channel, and
/// `apply_ack_outcome` applies it the next time the loop tick drains.
/// `Success.refreshed_acks` is `None` when the post-write refetch failed
/// (keep the previous snapshot), `Some(map)` otherwise even if empty
/// (legitimate "all acks expired" state).
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
pub(crate) struct AckSubmitPayload {
    pub(super) daemon_url: String,
    pub(super) signature: String,
    pub(super) api_key: Option<String>,
    pub(super) op: AckSubmitOp,
}

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

#[derive(Debug)]
pub(crate) enum AckSubmitOp {
    Create {
        by: String,
        reason: String,
        expires_at: Option<DateTime<Utc>>,
    },
    Revoke,
}

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

fn is_modal_input_char_acceptable(c: char) -> bool {
    // C0 / C1 / DEL controls would corrupt the rendered modal.
    if c.is_control() {
        return false;
    }
    // Bidi overrides and isolates can flip the visible order of the
    // surrounding text, including the modal labels and buttons.
    !matches!(c as u32, 0x202A..=0x202E | 0x2066..=0x2069)
}

pub(super) fn dispatch_modal_key(
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

pub(super) fn open_ack_modal_for_current(app: &mut App, revoke: bool) {
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

pub(super) fn draw_ack_modal(f: &mut Frame, app: &App) {
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

fn render_finding_signature_line(f: &mut Frame, area: Rect, signature: &str) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Finding: ", dim_style()),
            Span::raw(sanitize_for_terminal(signature)),
        ])),
        area,
    );
}

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

fn render_field_input(f: &mut Frame, area: Rect, value: &str, focused: bool) {
    // Borrow when possible: only the focused branch allocates, to append
    // the cursor char.
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
/// (black foreground on the action color background) and bold. The
/// unfocused state uses the action color as foreground only.
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

/// Validate the modal state and spawn the async ack/revoke roundtrip on
/// the tokio runtime. Returns immediately so the run loop keeps redrawing
/// while the request is in flight. The result lands later through
/// `tx_outcome`, which `apply_ack_outcome` consumes the next time the
/// loop tick drains.
pub(super) fn submit_ack_modal(app: &mut App, tx_outcome: &mpsc::UnboundedSender<AckOutcome>) {
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
/// successful write keeps the previous `acks_by_signature` snapshot. The
/// indicator may briefly look stale, but the write itself succeeded.
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
/// already-closed modal (Esc-while-submitting). Success still refreshes
/// the global ack map when present, so the Findings indicator updates.
/// A Failure on a closed modal logs at WARN before being dropped, so a
/// misconfigured `[daemon.ack] api_key` still shows up in the operator's
/// logs.
pub(super) fn apply_ack_outcome(app: &mut App, outcome: AckOutcome) {
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
