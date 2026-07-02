//! Shared TUI entry points: the terminal preflight check, the unified
//! multi-view launcher, and the `inspect` / `--tui` subcommand runners.

#![cfg(feature = "tui")]

use std::path::PathBuf;

use crate::{
    apply_acknowledgments_or_exit, disclose, limits, load_config, load_report_from_input,
    read_events, trace_not_found_exit, tui,
};

/// Exit early with a clear message when stdout is not an interactive
/// terminal. Called at the top of each TUI entry point, before any input is
/// read or parsed, so a piped `--tui` invocation is rejected without first
/// ingesting (potentially large) input.
pub(crate) fn require_terminal_or_exit() {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        eprintln!("Error: the interactive TUI requires a terminal (stdout is not a TTY)");
        std::process::exit(1);
    }
}

/// Shared launcher for the unified multi-view TUI. `analyze --tui`,
/// `explain --tui` and `inspect` all funnel here, differing only by the
/// initial view (and, for explain, the focused trace). Stubs per-trace
/// placeholders from findings when the input carried no raw spans (a
/// pre-computed Report), exactly like the Detail panel's fallback. Callers
/// must invoke `require_terminal_or_exit` before reading input.
pub(crate) fn launch_unified_tui(
    report: sentinel_core::report::Report,
    mut traces: Vec<sentinel_core::correlate::Trace>,
    detect_config: sentinel_core::detect::DetectConfig,
    initial_view: tui::View,
    focus_trace_id: Option<&str>,
) {
    if traces.is_empty() {
        let mut trace_ids: std::collections::BTreeSet<String> =
            report.findings.iter().map(|f| f.trace_id.clone()).collect();
        // Keep the explain --tui focus trace reachable even if its only
        // finding was filtered out by acknowledgments: without a stub it
        // would be absent from trace_index and `with_focus_trace` would
        // silently land on trace 0.
        if let Some(tid) = focus_trace_id {
            trace_ids.insert(tid.to_string());
        }
        traces = trace_ids
            .into_iter()
            .map(|tid| sentinel_core::correlate::Trace {
                trace_id: tid,
                spans: vec![],
            })
            .collect();
    }

    // `report` is fully consumed below, so move the summary fields out
    // rather than clone them (the findings and correlations are moved into
    // the App separately, disjoint-field moves the borrow checker allows).
    let summary = tui::AnalyzeSummary {
        green_summary: report.green_summary,
        quality_gate: report.quality_gate,
        analysis: report.analysis,
    };

    let mut app = tui::App::new(report.findings, traces, detect_config)
        .with_correlations(report.correlations)
        .with_summary(summary)
        .with_initial_view(initial_view);
    if let Some(tid) = focus_trace_id {
        app = app.with_focus_trace(tid);
    }
    if let Err(e) = tui::run(&mut app) {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}

pub(crate) fn cmd_inspect(
    input: &std::path::Path,
    config_path: Option<&std::path::Path>,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
) {
    require_terminal_or_exit();
    let config = load_config(config_path);
    let raw = read_events(Some(input), limits::MAX_BATCH_INPUT_BYTES);
    let detect_config = sentinel_core::detect::DetectConfig::from(&config);

    // Auto-detect events array vs pre-computed Report object, same shape
    // contract as `report --input`. A Report payload (e.g. a daemon
    // snapshot dumped via /api/export/report) lights up the Findings and
    // Correlations panels. The Detail panel falls back to a per-trace
    // stub with no spans because Reports don't carry raw spans.
    let (mut report, traces) = load_report_from_input(&raw, &config);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );
    launch_unified_tui(report, traces, detect_config, tui::View::Inspect, None);
}

/// `analyze --tui`: run the full pipeline (as `analyze` does) but open the
/// unified TUI on the Analyze view instead of printing the report.
pub(crate) fn cmd_analyze_tui(
    input: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
) {
    require_terminal_or_exit();
    let config = load_config(config_path);
    let raw = read_events(input, limits::MAX_BATCH_INPUT_BYTES);
    let detect_config = sentinel_core::detect::DetectConfig::from(&config);
    let (mut report, traces) = load_report_from_input(&raw, &config);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );
    launch_unified_tui(report, traces, detect_config, tui::View::Analyze, None);
}

/// `explain --tui`: load the full report (all traces, unlike the
/// single-trace non-interactive `explain`) and open the unified TUI on the
/// Explain view focused on `trace_id`.
pub(crate) fn cmd_explain_tui(
    input: &std::path::Path,
    trace_id: &str,
    config_path: Option<&std::path::Path>,
) {
    require_terminal_or_exit();
    let config = load_config(config_path);
    let raw = read_events(Some(input), limits::MAX_BATCH_INPUT_BYTES);
    let detect_config = sentinel_core::detect::DetectConfig::from(&config);
    let (mut report, traces) = load_report_from_input(&raw, &config);
    // Validate the trace exists before entering the TUI, mirroring the
    // non-interactive `explain`'s clear error path including the
    // available-IDs hint. Checked before ack filtering so a trace whose
    // only finding is acknowledged still opens.
    let known = traces.iter().any(|t| t.trace_id == trace_id)
        || report.findings.iter().any(|f| f.trace_id == trace_id);
    if !known {
        // With raw events `traces` holds every id; with a pre-computed
        // Report `traces` is empty and the ids live on the findings.
        let available: Vec<&str> = if traces.is_empty() {
            report
                .findings
                .iter()
                .map(|f| f.trace_id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        } else {
            traces.iter().map(|t| t.trace_id.as_str()).collect()
        };
        trace_not_found_exit(trace_id, available.into_iter());
    }
    // Apply acknowledgments (default file in cwd) so the shared Inspect and
    // Analyze views show the same finding population as `inspect` and
    // `analyze --tui`.
    apply_acknowledgments_or_exit(&mut report, &config, None, false);
    launch_unified_tui(
        report,
        traces,
        detect_config,
        tui::View::Explain,
        Some(trace_id),
    );
}

/// `disclose --tui`: read-only preview. Loads the org-config, scans the cold
/// archive once for its time range (to anchor the default period), then opens
/// the standalone Disclose tab. The preview re-reads the same cold NDJSON via
/// `aggregate_from_paths` on each settings change. Never writes or hashes.
pub(crate) fn cmd_disclose_tui(
    input: Vec<PathBuf>,
    org_config: &std::path::Path,
    strict_attribution: bool,
) {
    use sentinel_core::report::periodic::aggregator::archive_time_range;
    use sentinel_core::report::periodic::org_config as org_config_loader;

    require_terminal_or_exit();

    let org = match org_config_loader::load_from_path(org_config) {
        Ok(c) => c,
        Err(err) => {
            eprintln!(
                "Error: {}",
                sentinel_core::text_safety::sanitize_for_terminal(&err.to_string())
            );
            std::process::exit(1);
        }
    };

    let archive_range = match archive_time_range(&input) {
        Ok(range) => range,
        Err(err) => {
            eprintln!(
                "Error: {}",
                sentinel_core::text_safety::sanitize_for_terminal(&err.to_string())
            );
            std::process::exit(1);
        }
    };

    let state = disclose::DiscloseState::new(
        input,
        org,
        org_config.to_path_buf(),
        strict_attribution,
        archive_range,
        chrono::Utc::now().date_naive(),
    );

    // The Disclose tab reads only `app.disclose`; findings/traces/detect are
    // unused, so a default detect config and empty inputs suffice.
    let config = load_config(None);
    let detect_config = sentinel_core::detect::DetectConfig::from(&config);
    let mut app = tui::App::new(Vec::new(), Vec::new(), detect_config)
        .with_disclose(state)
        .with_initial_view(tui::View::Disclose);
    if let Err(e) = tui::run(&mut app) {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}
