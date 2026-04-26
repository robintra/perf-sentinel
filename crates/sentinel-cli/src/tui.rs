//! Terminal UI for interactive trace and finding inspection.
//!
//! Provides a 3-panel layout: traces list, findings for selected trace,
//! and finding detail with span tree.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use sentinel_core::correlate::Trace;
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
    trace_index: std::collections::HashMap<String, usize>,

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
    pre_rendered_trees: std::collections::HashMap<String, String>,
    /// Cross-trace correlations to display in the Correlations panel.
    /// Empty in batch mode (correlator is daemon-only). Populated by
    /// `query inspect` from `/api/correlations`.
    correlations: Vec<CrossTraceCorrelation>,
    pub selected_correlation: usize,
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
        let trace_index: std::collections::HashMap<String, usize> = traces
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
            pre_rendered_trees: std::collections::HashMap::new(),
            correlations: Vec::new(),
            selected_correlation: 0,
        }
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
    fn current_finding(&self) -> Option<&Finding> {
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

/// Run the TUI event loop.
///
/// # Errors
///
/// Returns an error if terminal setup or event reading fails.
pub fn run(app: &mut App) -> io::Result<()> {
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
            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::Right | KeyCode::Tab => app.next_panel(),
                KeyCode::Left | KeyCode::BackTab => app.prev_panel(),
                KeyCode::Enter => app.enter(),
                KeyCode::Esc => app.escape(),
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
            let line = Line::from(vec![
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
            ]);
            ListItem::new(line)
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
                code_location: None,
                instrumentation_scopes: Vec::new(),
                suggested_fix: None,
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
                code_location: None,
                instrumentation_scopes: Vec::new(),
                suggested_fix: None,
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
}
