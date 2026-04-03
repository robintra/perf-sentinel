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
use sentinel_core::detect::{DetectConfig, Finding, FindingType, Severity};
use sentinel_core::explain;
/// Panel that currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Traces,
    Findings,
    Detail,
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
}

impl App {
    /// Create a new app from analysis findings and traces.
    #[must_use]
    pub fn new(findings: Vec<Finding>, traces: Vec<Trace>, detect_config: DetectConfig) -> Self {
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
        }
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

    /// Get the cached detail tree text, computing it if needed.
    ///
    /// Cached per trace (not per finding) since the tree is the same for all
    /// findings in a trace — `build_tree` annotates all findings inline.
    fn detail_tree_text(&mut self) -> Option<String> {
        let trace_idx = self.selected_trace;

        if let Some((ct, ref text)) = self.cached_detail
            && ct == trace_idx
        {
            return Some(text.clone());
        }

        let finding = self.current_finding()?;
        let trace_vec_idx = self.trace_index.get(&finding.trace_id).copied()?;
        let trace = &self.traces[trace_vec_idx];
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
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
        }
    }

    /// Move focus to the next panel.
    pub fn next_panel(&mut self) {
        self.active_panel = match self.active_panel {
            Panel::Traces => Panel::Findings,
            Panel::Findings => Panel::Detail,
            Panel::Detail => Panel::Traces,
        };
    }

    /// Move focus to the previous panel.
    pub fn prev_panel(&mut self) {
        self.active_panel = match self.active_panel {
            Panel::Traces => Panel::Detail,
            Panel::Findings => Panel::Traces,
            Panel::Detail => Panel::Findings,
        };
    }

    /// Handle Enter key: drill into next panel.
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
            Panel::Detail => {}
        }
    }

    /// Handle Escape: go back to previous panel.
    pub fn escape(&mut self) {
        match self.active_panel {
            Panel::Traces => {}
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
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(chunks[0]);

    draw_traces_panel(f, app, top[0]);
    draw_findings_panel(f, app, top[1]);
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
    match ft {
        FindingType::NPlusOneSql => "N+1 SQL",
        FindingType::NPlusOneHttp => "N+1 HTTP",
        FindingType::RedundantSql => "Redundant SQL",
        FindingType::RedundantHttp => "Redundant HTTP",
        FindingType::SlowSql => "Slow SQL",
        FindingType::SlowHttp => "Slow HTTP",
        FindingType::ExcessiveFanout => "Excessive Fanout",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_core::detect::{GreenImpact, Pattern};

    fn make_test_app() -> App {
        let findings = vec![
            Finding {
                finding_type: FindingType::NPlusOneSql,
                severity: Severity::Critical,
                trace_id: "trace-1".to_string(),
                service: "game".to_string(),
                source_endpoint: "POST /api/game/42/start".to_string(),
                pattern: Pattern {
                    template: "SELECT * FROM player WHERE game_id = ?".to_string(),
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
                }),
            },
            Finding {
                finding_type: FindingType::RedundantSql,
                severity: Severity::Warning,
                trace_id: "trace-2".to_string(),
                service: "account".to_string(),
                source_endpoint: "GET /api/account/123".to_string(),
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
            },
        ];

        let detect_config = DetectConfig {
            n_plus_one_threshold: 5,
            window_ms: 500,
            slow_threshold_ms: 500,
            slow_min_occurrences: 3,
            max_fanout: 20,
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
        assert_eq!(app.active_panel, Panel::Traces);
    }

    #[test]
    fn prev_panel_cycles() {
        let mut app = make_test_app();
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
}
