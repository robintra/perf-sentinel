//! Frame rendering for the inspect TUI: tab bar, views, panels and their style helpers.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

#[cfg(feature = "daemon")]
use sentinel_core::daemon::query_api::AckSource;
use sentinel_core::detect::Finding;
use sentinel_core::report::periodic::schema::{Confidentiality, ReportIntent};
use sentinel_core::text_safety::{safe_url, sanitize_for_terminal, strip_code_ticks};

use crate::disclose::{CustomField, DiscloseState, Granularity, Tone};
use crate::tui_resize::Axis;

#[cfg(feature = "daemon")]
use super::ack_modal::draw_ack_modal;
use super::{App, Panel, View, dim_style, finding_type_label, severity_color, severity_label};

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

/// `@key=value` for a correlation side, empty when it carries no grouping.
fn grouping_suffix(key: Option<&str>, value: Option<&str>) -> String {
    value.map_or_else(String::new, |value| {
        key.map_or_else(
            || format!("@{}", sanitize_for_terminal(value)),
            |key| {
                format!(
                    "@{}={}",
                    sanitize_for_terminal(key),
                    sanitize_for_terminal(value)
                )
            },
        )
    })
}

pub(super) fn draw(f: &mut Frame, app: &App) {
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
/// repo link are brand green and the link is underlined. "Powered by" and the
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
/// view-level navigation hint. Only a visual orientation aid: the keys
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
    if app.view == View::Inspect {
        spans.push(Span::styled(
            format!(
                "    s sort: {}    f severity: {}",
                app.trace_sort.label(),
                app.severity_filter_label()
            ),
            dim,
        ));
    }
    spans.push(Span::styled(
        "    Enter \u{2193} \u{00b7} Esc \u{2191} \u{00b7} q quit".to_string(),
        dim,
    ));
    // The MOUSE badge shows in every drill-down view so capture can never
    // be silently trapped on. The drag/reset hint is Inspect-only.
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
/// detail).
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
                "A batch report carries no spans, a daemon snapshot only the traces it retained. Launch `inspect --input <events>.json` or `query inspect`.",
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
/// `\u{256a}` handle at its midpoint (vertical stubs hint the up-down drag).
/// See [`highlight_vline`].
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

    // A narrowed list must say so, or an empty panel reads as a trace
    // with no findings rather than as a filter hiding them.
    let title = match app.severity_filter {
        None => " Findings ".to_string(),
        Some(_) => format!(" Findings [{}] ", app.severity_filter_label()),
    };
    let block = Block::default()
        .title(title)
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
                        "{}{}:{} ",
                        sanitize_for_terminal(&c.source.service),
                        grouping_suffix(
                            c.source.grouping_key.as_deref(),
                            c.source.grouping_value.as_deref(),
                        ),
                        c.source.finding_type.as_str()
                    ),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw("-> "),
                Span::styled(
                    format!(
                        "{}{}:{}  ",
                        sanitize_for_terminal(&c.target.service),
                        grouping_suffix(
                            c.target.grouping_key.as_deref(),
                            c.target.grouping_value.as_deref(),
                        ),
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

pub(super) fn draw_detail_panel(f: &mut Frame, app: &App, area: Rect) {
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
            // Labelled by the attribute that decided the identity, since
            // which one that is comes from operator config.
            Span::styled(
                format!(
                    "{}: ",
                    finding
                        .effective_grouping()
                        .map_or_else(|| "Grouping".into(), |g| sanitize_for_terminal(&g.key))
                ),
                dim_style(),
            ),
            Span::raw(finding.grouping_value().map_or_else(
                || "-".to_string(),
                |v| sanitize_for_terminal(v).into_owned(),
            )),
        ]),
        Line::from(vec![
            Span::styled("Endpoint: ", dim_style()),
            Span::raw(&finding.source_endpoint),
        ]),
        Line::from(vec![
            Span::styled("Suggestion: ", Style::default().fg(Color::Cyan)),
            Span::raw(strip_code_ticks(&finding.suggestion).into_owned()),
        ]),
    ];

    push_finding_context_lines(&mut lines, finding);

    if let Some(ref impact) = finding.green_impact {
        lines.push(Line::from(vec![
            Span::styled("Extra I/O: ", dim_style()),
            Span::raw(format!("{} avoidable ops", impact.estimated_extra_io_ops)),
        ]));
    }
    let key = crate::render::recurrence_key(finding);
    if let Some(stats) = app.recurrence.get(&key).filter(|s| s.count > 1) {
        let ops = if stats.total_ops > 0 {
            format!(" \u{b7} ~{} avoidable ops in total", stats.total_ops)
        } else {
            String::new()
        };
        lines.push(Line::from(vec![
            Span::styled("Recurrence: ", dim_style()),
            Span::raw(format!("detected in {} traces{ops}", stats.count)),
        ]));
    }

    push_span_tree_lines(&mut lines, app);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));

    f.render_widget(paragraph, area);
}

/// The fix, source location and per-finding statistics rows of the Detail
/// panel. Split out of `draw_detail_panel` so neither half carries the
/// whole panel's branching.
fn push_finding_context_lines<'a>(lines: &mut Vec<Line<'a>>, finding: &'a Finding) {
    // Same sanitization as the CLI path (render.rs): the fix can come
    // from a --input JSON or a daemon, not only the embedded table.
    if let Some(ref fix) = finding.suggested_fix {
        let plain = strip_code_ticks(&fix.recommendation);
        let mut spans = vec![
            Span::styled("Suggested fix: ", Style::default().fg(Color::Cyan)),
            Span::raw(sanitize_for_terminal(&plain).into_owned()),
        ];
        if let Some(url) = fix.reference_url.as_deref().and_then(safe_url) {
            spans.push(Span::styled(format!(" (see: {url})"), dim_style()));
        }
        lines.push(Line::from(spans));
    }

    if let Some(ref loc) = finding.code_location {
        let src = loc.display_string();
        if !src.is_empty() {
            // After Endpoint, at index 6 behind the grouping row.
            lines.insert(
                7,
                Line::from(vec![
                    Span::styled("Source:   ", dim_style()),
                    Span::raw(src),
                ]),
            );
        }
    }

    // The three rows below use the same figures and formatting as the
    // dashboard.
    if let Some(timing) = crate::render::format_span_timing(&finding.pattern) {
        lines.push(Line::from(vec![
            Span::styled("Timing:   ", dim_style()),
            Span::raw(timing),
        ]));
    }
    if let Some(label) = crate::render::classification_label(finding) {
        lines.push(Line::from(vec![
            Span::styled("Class:    ", dim_style()),
            Span::raw(label),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("Window:   ", dim_style()),
        Span::raw(format!(
            "{} -> {}",
            crate::render::fmt_local_iso(
                &finding.first_timestamp,
                crate::render::LOCAL_TIME_FORMAT
            ),
            crate::render::fmt_local_iso(&finding.last_timestamp, crate::render::LOCAL_TIME_FORMAT)
        )),
    ]));
    if !finding.confidence.is_batch() {
        lines.push(Line::from(vec![
            Span::styled("Confidence: ", dim_style()),
            Span::raw(finding.confidence.as_str()),
        ]));
    }
}

/// The Span tree section closing the Detail panel, either the cached tree
/// or the hint naming the two commands that produce one.
fn push_span_tree_lines(lines: &mut Vec<Line<'_>>, app: &App) {
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Span tree:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));

    // Span tree is pre-computed before draw, cached per trace.
    if let Some((ct, ref tree_text)) = app.cached_detail
        && ct == app.selected_trace
    {
        for tree_line in tree_text.lines() {
            lines.push(Line::from(tree_line.to_string()));
        }
        return;
    }

    // No span tree available: the input was a Report (no embedded spans)
    // or a daemon trace that the explain endpoint did not return. Surface
    // the two paths that produce a real tree so the user knows what to
    // try next.
    lines.push(Line::from(Span::styled(
        "Not available for this trace. A batch report carries no spans, and a daemon",
        dim_style(),
    )));
    lines.push(Line::from(Span::styled(
        "snapshot only keeps the traces its retention held.",
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
