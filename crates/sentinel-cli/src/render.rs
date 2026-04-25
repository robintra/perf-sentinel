//! Colored CLI rendering helpers: ANSI palette, findings/offender/gate
//! pretty-printers and the top-level `emit_report_and_gate` used by every
//! command that produces a `Report`.

use std::collections::HashMap;

use sentinel_core::detect::{Confidence, Severity};
use sentinel_core::report::json::JsonReportSink;
use sentinel_core::report::{Report, ReportSink};
use sentinel_core::score::carbon::IntensitySource;
use sentinel_core::text_safety::{safe_url, sanitize_for_terminal};

use crate::OutputFormat;

/// Emit the final report in the requested format and enforce the quality
/// gate in CI mode. Exits with status 1 on any write failure or a failed
/// gate when `ci` is true.
pub(crate) fn emit_report_and_gate(
    report: &Report,
    format: Option<OutputFormat>,
    ci: bool,
    label: &str,
) {
    let effective_format = format.unwrap_or(if ci {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    });

    match effective_format {
        OutputFormat::Text => {
            print_colored_report(report, label);
        }
        OutputFormat::Json => {
            let sink = JsonReportSink;
            if let Err(e) = sink.emit(report) {
                eprintln!("Error writing report: {e}");
                std::process::exit(1);
            }
        }
        OutputFormat::Sarif => {
            if let Err(e) = sentinel_core::report::sarif::emit_sarif(report) {
                eprintln!("Error writing SARIF report: {e}");
                std::process::exit(1);
            }
        }
    }

    if ci && !report.quality_gate.passed {
        eprintln!("Quality gate FAILED");
        std::process::exit(1);
    }
}

pub(crate) fn print_colored_report(report: &Report, title: &str) {
    format_colored_report(report, title, false);
}

/// ANSI color codes bundled for CLI rendering.
///
/// Named fields avoid the "count underscores in a 7-tuple" pattern that
/// made it easy to misuse the old `AnsiColors` tuple alias. Every field
/// is either a SGR escape sequence or an empty string (when the output
/// is not a terminal).
#[derive(Clone, Copy)]
pub(crate) struct AnsiColors {
    pub(crate) bold: &'static str,
    pub(crate) cyan: &'static str,
    pub(crate) red: &'static str,
    pub(crate) yellow: &'static str,
    pub(crate) green: &'static str,
    pub(crate) dim: &'static str,
    pub(crate) reset: &'static str,
}

pub(crate) fn ansi_colors(force_color: bool) -> AnsiColors {
    use std::io::IsTerminal;
    if force_color || std::io::stdout().is_terminal() {
        AnsiColors {
            bold: "\x1b[1m",
            cyan: "\x1b[36m",
            red: "\x1b[31m",
            yellow: "\x1b[33m",
            green: "\x1b[32m",
            dim: "\x1b[2m",
            reset: "\x1b[0m",
        }
    } else {
        no_colors()
    }
}

/// Plain palette with every field empty. Used when the sink is known
/// not to be a terminal (e.g. writing to `--output file.txt`), where
/// `ansi_colors`'s `stdout().is_terminal()` probe would otherwise emit
/// escape sequences into the file.
pub(crate) const fn no_colors() -> AnsiColors {
    AnsiColors {
        bold: "",
        cyan: "",
        red: "",
        yellow: "",
        green: "",
        dim: "",
        reset: "",
    }
}

/// Map an [`InterpretationLevel`] to the ANSI color used for CLI
/// rendering. Mirrors the palette used for finding severities:
/// Critical=red, High=yellow, Healthy=green. Moderate returns an empty
/// string (uncolored) to keep it informational without visually competing
/// with High.
///
/// [`InterpretationLevel`]: sentinel_core::InterpretationLevel
pub(crate) fn interpret_color(
    level: sentinel_core::InterpretationLevel,
    colors: AnsiColors,
) -> &'static str {
    use sentinel_core::InterpretationLevel::{Critical, Healthy, High, Moderate};
    match level {
        Critical => colors.red,
        High => colors.yellow,
        Moderate => "",
        Healthy => colors.green,
    }
}

/// Snake-case label for an `IntensitySource`, matching the JSON
/// representation so the terminal vocabulary aligns with the dashboard.
const fn intensity_source_label(source: IntensitySource) -> &'static str {
    match source {
        IntensitySource::Annual => "annual",
        IntensitySource::Hourly => "hourly",
        IntensitySource::MonthlyHourly => "monthly_hourly",
        IntensitySource::RealTime => "real_time",
    }
}

pub(crate) fn format_colored_report(report: &Report, title: &str, force_color: bool) {
    let colors = ansi_colors(force_color);
    let AnsiColors {
        bold,
        cyan,
        green,
        dim,
        reset,
        ..
    } = colors;

    println!();
    println!("{bold}{cyan}=== perf-sentinel {title} ==={reset}");
    println!(
        "{dim}Analyzed {} events across {} traces in {}ms{reset}",
        report.analysis.events_processed,
        report.analysis.traces_analyzed,
        report.analysis.duration_ms
    );
    println!();

    if report.findings.is_empty() {
        println!("{green}No performance anti-patterns detected.{reset}");
    } else {
        print_findings(&report.findings, force_color);
    }

    print_green_summary(&report.green_summary, force_color);
    print_quality_gate(&report.quality_gate, force_color);
}

pub(crate) fn print_findings(findings: &[sentinel_core::detect::Finding], force_color: bool) {
    let colors = ansi_colors(force_color);
    println!(
        "{}Found {} issue(s):{}",
        colors.bold,
        findings.len(),
        colors.reset
    );
    if let Some(line) = format_severity_breakdown(findings, colors) {
        println!("{line}");
    }
    if let Some(line) = format_top_finding_types(findings) {
        println!("{line}");
    }
    println!();
    for (i, finding) in findings.iter().enumerate() {
        print_finding_entry(i, finding, colors);
        println!();
    }
}

/// One-line severity breakdown printed under the `Found N issue(s):`
/// header. Returns `None` for empty inputs so the caller can skip the
/// line entirely.
fn format_severity_breakdown(
    findings: &[sentinel_core::detect::Finding],
    colors: AnsiColors,
) -> Option<String> {
    if findings.is_empty() {
        return None;
    }
    let mut critical = 0usize;
    let mut warning = 0usize;
    let mut info = 0usize;
    for f in findings {
        match f.severity {
            Severity::Critical => critical += 1,
            Severity::Warning => warning += 1,
            Severity::Info => info += 1,
        }
    }
    let AnsiColors {
        red,
        yellow,
        dim,
        reset,
        ..
    } = colors;
    Some(format!(
        "  {red}{critical} critical{reset}, {yellow}{warning} warning{reset}, {dim}{info} info{reset}"
    ))
}

/// Top-N (2 normally, 3 above 10) finding types by frequency. None on
/// runs of 5 findings or fewer to avoid noise.
fn format_top_finding_types(findings: &[sentinel_core::detect::Finding]) -> Option<String> {
    if findings.len() <= 5 {
        return None;
    }
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for f in findings {
        *counts.entry(f.finding_type.as_str()).or_insert(0) += 1;
    }
    let mut pairs: Vec<(&'static str, usize)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let take = if findings.len() > 10 { 3 } else { 2 };
    let formatted: Vec<String> = pairs
        .into_iter()
        .take(take)
        .map(|(k, v)| format!("{k} ({v})"))
        .collect();
    if formatted.is_empty() {
        None
    } else {
        Some(format!("  Most common: {}", formatted.join(", ")))
    }
}

fn print_finding_entry(index: usize, finding: &sentinel_core::detect::Finding, colors: AnsiColors) {
    let AnsiColors {
        bold,
        cyan,
        dim,
        reset,
        ..
    } = colors;
    let severity_color = severity_color(&finding.severity, colors);
    let severity_label = severity_label(&finding.severity);
    let type_label = finding.finding_type.display_label();

    println!(
        "  {bold}{severity_color}[{severity_label}] #{} {type_label}{reset}",
        index + 1,
    );
    println!(
        "    {dim}Trace:{reset}    {}",
        sanitize_for_terminal(&finding.trace_id)
    );
    println!(
        "    {dim}Service:{reset}  {}",
        sanitize_for_terminal(&finding.service)
    );
    println!(
        "    {dim}Endpoint:{reset} {}",
        sanitize_for_terminal(&finding.source_endpoint)
    );
    if let Some(ref loc) = finding.code_location {
        let src = loc.display_string();
        if !src.is_empty() {
            println!("    {dim}Source:{reset}   {}", sanitize_for_terminal(&src));
        }
    }
    println!(
        "    {dim}Template:{reset} {}",
        sanitize_for_terminal(&finding.pattern.template)
    );
    println!(
        "    {dim}Hits:{reset}     {} occurrences, {} distinct params, {}ms window",
        finding.pattern.occurrences, finding.pattern.distinct_params, finding.pattern.window_ms
    );
    println!(
        "    {dim}Window:{reset}   {} -> {}",
        finding.first_timestamp, finding.last_timestamp
    );
    println!(
        "    {cyan}Suggestion:{reset} {}",
        sanitize_for_terminal(&finding.suggestion)
    );
    if finding.confidence != Confidence::CiBatch {
        println!(
            "    {dim}Confidence:{reset} {}",
            finding.confidence.as_str()
        );
    }
    if let Some(ref fix) = finding.suggested_fix {
        let recommendation = sanitize_for_terminal(&fix.recommendation);
        match fix.reference_url.as_deref().and_then(safe_url) {
            Some(url) => println!("    {cyan}Suggested fix:{reset} {recommendation} (see: {url})"),
            None => println!("    {cyan}Suggested fix:{reset} {recommendation}"),
        }
    }
    if let Some(ref impact) = finding.green_impact {
        print_finding_impact(impact, colors);
    }
}

fn print_finding_impact(impact: &sentinel_core::detect::GreenImpact, colors: AnsiColors) {
    let AnsiColors { dim, reset, .. } = colors;
    println!(
        "    {dim}Extra I/O:{reset} {} avoidable ops",
        impact.estimated_extra_io_ops
    );
    // Read the pre-computed band from the struct field rather than
    // calling for_iis() again: keeps the CLI rendering in lockstep with
    // the JSON output and prevents silent drift if thresholds change.
    let level = impact.io_intensity_band;
    let level_color = interpret_color(level, colors);
    println!(
        "    {dim}IIS:{reset}      {:.1} {level_color}({}){reset}",
        impact.io_intensity_score,
        level.short_label(),
    );
}

fn severity_color(severity: &Severity, colors: AnsiColors) -> &'static str {
    match severity {
        Severity::Critical => colors.red,
        Severity::Warning => colors.yellow,
        Severity::Info => colors.dim,
    }
}

fn severity_label(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical => "CRITICAL",
        Severity::Warning => "WARNING",
        Severity::Info => "INFO",
    }
}

fn print_green_summary(summary: &sentinel_core::report::GreenSummary, force_color: bool) {
    let colors = ansi_colors(force_color);
    let AnsiColors {
        bold,
        cyan,
        dim,
        reset,
        ..
    } = colors;

    println!("{bold}{cyan}--- GreenOps Summary ---{reset}");
    println!("  Total I/O ops:     {}", summary.total_io_ops);
    println!("  Avoidable I/O ops: {}", summary.avoidable_io_ops);
    // Read the pre-computed band from the struct field (see print_findings).
    let waste_level = summary.io_waste_ratio_band;
    let waste_color = interpret_color(waste_level, colors);
    println!(
        "  I/O waste ratio:   {:.1}% {waste_color}({}){reset}",
        summary.io_waste_ratio * 100.0,
        waste_level.short_label(),
    );

    // Render the structured CO₂ report when present.
    if let Some(carbon) = summary.co2.as_ref() {
        println!(
            "  Est. CO\u{2082}:          {:.6} g (low {:.6}, high {:.6}, model {})",
            carbon.total.mid, carbon.total.low, carbon.total.high, carbon.total.model,
        );
        println!(
            "  Avoidable CO\u{2082}:     {:.6} g (low {:.6}, high {:.6})",
            carbon.avoidable.mid, carbon.avoidable.low, carbon.avoidable.high,
        );
        println!(
            "  Operational:       {:.6} g    Embodied: {:.6} g    Methodology: {}",
            carbon.operational_gco2, carbon.embodied_gco2, carbon.total.methodology,
        );
        if let Some(transport) = carbon.transport_gco2 {
            println!("  Transport:         {transport:.6} g    (cross-region network bytes)");
        }
    }

    // Per-region breakdown whenever at least one region was resolved.
    // Always emitted (even mono-region) so the intensity source is
    // visible to the user, matching the dashboard's per-region table.
    if !summary.regions.is_empty() {
        println!();
        println!("  {bold}Per-region breakdown:{reset}");
        for region in &summary.regions {
            // Unresolved regions carry placeholder zero intensity. Render
            // an explicit marker rather than `0 gCO2/kWh, source: annual`
            // so a reader does not mistake an unknown region for a clean
            // grid.
            if region.status == sentinel_core::score::carbon::REGION_STATUS_UNRESOLVED {
                println!(
                    "    - {}: {} I/O ops, {:.6} gCO\u{2082} (intensity: unresolved, source: -)",
                    region.region, region.io_ops, region.co2_gco2,
                );
            } else {
                let source_str = intensity_source_label(region.intensity_source);
                println!(
                    "    - {}: {} I/O ops, {:.6} gCO\u{2082} ({:.0} gCO\u{2082}/kWh, source: {source_str})",
                    region.region, region.io_ops, region.co2_gco2, region.grid_intensity_gco2_kwh,
                );
            }
        }
    }

    if !summary.top_offenders.is_empty() {
        println!();
        println!("  {bold}Top offenders:{reset}");
        for offender in &summary.top_offenders {
            let level = offender.io_intensity_band;
            let level_color = interpret_color(level, colors);
            let co2_str = offender
                .co2_grams
                .map_or(String::new(), |co2| format!(", {co2:.6} gCO\u{2082}"));
            println!(
                "    - {}: IIS {:.1} {level_color}({}){reset} (service: {}){co2_str}",
                offender.endpoint,
                offender.io_intensity_score,
                level.short_label(),
                offender.service,
            );
        }
    }

    // Mandatory disclaimer: only shown when we actually emitted CO₂
    // estimates, to avoid noise when green scoring is disabled.
    // The "2× multiplicative uncertainty" framing matches the constants:
    // low = mid/2, high = mid×2 (log-symmetric interval, geometric mean = mid).
    if summary.co2.is_some() {
        println!();
        println!(
            "  {dim}Note: CO\u{2082} estimates have ~2\u{00d7} multiplicative uncertainty \
             (low = mid/2, high = mid\u{00d7}2). See docs/LIMITATIONS.md.{reset}"
        );
    }

    // One-liner on the interpret bands: they are anchored on the *default*
    // detector thresholds, not on the user's config. An endpoint still
    // labelled "high" after raising `n_plus_one_threshold` is not a bug;
    // see README "How to read the report" for the full explanation.
    println!(
        "  {dim}Note: `(healthy/moderate/high/critical)` bands use fixed heuristic \
         thresholds, independent of your `n_plus_one_threshold` / \
         `io_waste_ratio_max` overrides. See README \"How to read the report\".{reset}"
    );

    println!();
}

fn print_quality_gate(gate: &sentinel_core::report::QualityGate, force_color: bool) {
    let AnsiColors {
        bold,
        red,
        green,
        dim,
        reset,
        ..
    } = ansi_colors(force_color);

    let gate_color = if gate.passed { green } else { red };
    let gate_label = if gate.passed { "PASSED" } else { "FAILED" };
    println!("{bold}Quality gate: {gate_color}{gate_label}{reset}");
    for rule in &gate.rules {
        let status_color = if rule.passed { green } else { red };
        let status_label = if rule.passed { "PASS" } else { "FAIL" };
        println!(
            "  {dim}-{reset} {}: {} (actual {}) {status_color}{status_label}{reset}",
            rule.rule, rule.threshold, rule.actual,
        );
    }
    println!();
}

/// Emit a `DiffReport` in the requested format to the given writer.
///
/// `output = None` writes to stdout. Format defaults to text. The SARIF
/// path emits only the `new_findings` (resolved findings have no SARIF
/// equivalent) so existing PR-annotation pipelines that consume SARIF
/// surface only regressions.
///
/// # Errors
///
/// Returns an error if the output file cannot be opened or if
/// serialization fails.
pub(crate) fn emit_diff(
    diff: &sentinel_core::diff::DiffReport,
    format: Option<OutputFormat>,
    output: Option<&std::path::Path>,
) -> std::io::Result<()> {
    use std::io::Write;

    let mut writer: Box<dyn Write> = match output {
        Some(path) => Box::new(std::fs::File::create(path)?),
        None => Box::new(std::io::stdout().lock()),
    };
    // Force colors off when writing to a file. `ansi_colors` gates on
    // `stdout().is_terminal()`, which stays true even when the actual
    // writer is a File, and would otherwise leak escape codes into the
    // user-facing artifact.
    let colors = if output.is_some() {
        no_colors()
    } else {
        ansi_colors(false)
    };
    let effective_format = format.unwrap_or(OutputFormat::Text);
    match effective_format {
        OutputFormat::Text => write_diff_text(&mut writer, diff, colors)?,
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut writer, diff).map_err(std::io::Error::other)?;
            writeln!(writer)?;
        }
        OutputFormat::Sarif => {
            let sarif = sentinel_core::report::sarif::findings_to_sarif(&diff.new_findings);
            serde_json::to_writer_pretty(&mut writer, &sarif).map_err(std::io::Error::other)?;
            writeln!(writer)?;
        }
    }
    Ok(())
}

fn write_diff_text(
    writer: &mut dyn std::io::Write,
    diff: &sentinel_core::diff::DiffReport,
    colors: AnsiColors,
) -> std::io::Result<()> {
    let AnsiColors {
        bold,
        cyan,
        red,
        yellow,
        green,
        reset,
        ..
    } = colors;

    let new_count = diff.new_findings.len();
    let resolved_count = diff.resolved_findings.len();
    let changed_count = diff.severity_changes.len();
    let regression_count = diff
        .severity_changes
        .iter()
        .filter(|c| c.is_regression())
        .count();
    let endpoint_change_count = diff.endpoint_metric_deltas.len();

    writeln!(writer)?;
    writeln!(writer, "{bold}{cyan}=== perf-sentinel diff ==={reset}")?;
    writeln!(
        writer,
        "  {red}{new_count} new{reset}, \
         {green}{resolved_count} resolved{reset}, \
         {yellow}{changed_count} severity changed{reset} ({regression_count} regression(s)), \
         {endpoint_change_count} endpoint count change(s)"
    )?;
    writeln!(writer)?;

    write_new_findings_section(writer, &diff.new_findings, colors)?;
    write_resolved_findings_section(writer, &diff.resolved_findings, colors)?;
    write_severity_changes_section(writer, &diff.severity_changes, colors)?;
    write_endpoint_deltas_section(writer, &diff.endpoint_metric_deltas, colors)?;

    if new_count == 0 && resolved_count == 0 && changed_count == 0 && endpoint_change_count == 0 {
        writeln!(
            writer,
            "{green}No differences detected between the two trace sets.{reset}"
        )?;
    }
    Ok(())
}

fn write_new_findings_section(
    writer: &mut dyn std::io::Write,
    findings: &[sentinel_core::detect::Finding],
    colors: AnsiColors,
) -> std::io::Result<()> {
    if findings.is_empty() {
        return Ok(());
    }
    let AnsiColors {
        bold, red, reset, ..
    } = colors;
    writeln!(
        writer,
        "{bold}{red}New findings ({}):{reset}",
        findings.len()
    )?;
    for f in findings {
        writeln!(
            writer,
            "  {red}+{reset} [{}] {} on {} ({})",
            severity_label(&f.severity),
            f.finding_type.display_label(),
            f.source_endpoint,
            f.service,
        )?;
        write_finding_block(writer, f, colors)?;
    }
    writeln!(writer)
}

fn write_resolved_findings_section(
    writer: &mut dyn std::io::Write,
    findings: &[sentinel_core::detect::Finding],
    colors: AnsiColors,
) -> std::io::Result<()> {
    if findings.is_empty() {
        return Ok(());
    }
    let AnsiColors {
        bold, green, reset, ..
    } = colors;
    writeln!(
        writer,
        "{bold}{green}Resolved findings ({}):{reset}",
        findings.len()
    )?;
    for f in findings {
        writeln!(
            writer,
            "  {green}-{reset} [{}] {} on {} ({})",
            severity_label(&f.severity),
            f.finding_type.display_label(),
            f.source_endpoint,
            f.service,
        )?;
        write_finding_block(writer, f, colors)?;
    }
    writeln!(writer)
}

/// Indented detail block printed under each new or resolved finding in
/// the diff text output.
fn write_finding_block(
    writer: &mut dyn std::io::Write,
    f: &sentinel_core::detect::Finding,
    colors: AnsiColors,
) -> std::io::Result<()> {
    let AnsiColors {
        cyan, dim, reset, ..
    } = colors;

    writeln!(
        writer,
        "      {dim}{:<12}{reset} {}",
        "Template:",
        sanitize_for_terminal(&f.pattern.template)
    )?;
    writeln!(
        writer,
        "      {dim}{:<12}{reset} {}",
        "Occurrences:", f.pattern.occurrences
    )?;
    writeln!(
        writer,
        "      {dim}{:<12}{reset} {} -> {} ({})",
        "Window:",
        short_timestamp(&f.first_timestamp),
        short_timestamp(&f.last_timestamp),
        format_duration_compact(f.pattern.window_ms),
    )?;
    writeln!(
        writer,
        "      {cyan}{:<12}{reset} {}",
        "Suggestion:",
        sanitize_for_terminal(&f.suggestion)
    )?;
    if let Some(ref fix) = f.suggested_fix {
        let label = format!("Fix [{}]:", sanitize_for_terminal(&fix.framework));
        let recommendation = sanitize_for_terminal(&fix.recommendation);
        match fix.reference_url.as_deref().and_then(safe_url) {
            Some(url) => writeln!(
                writer,
                "      {cyan}{label:<12}{reset} {recommendation} ({url})"
            )?,
            None => writeln!(writer, "      {cyan}{label:<12}{reset} {recommendation}")?,
        }
    }
    if let Some(ref impact) = f.green_impact {
        let level = impact.io_intensity_band;
        let level_color = interpret_color(level, colors);
        writeln!(
            writer,
            "      {dim}{:<12}{reset} {:.1} {level_color}({}){reset}",
            "IIS:",
            impact.io_intensity_score,
            level.short_label(),
        )?;
        writeln!(
            writer,
            "      {dim}{:<12}{reset} {} avoidable ops",
            "Extra I/O:", impact.estimated_extra_io_ops,
        )?;
    }
    if let Some(ref loc) = f.code_location {
        let s = loc.display_string();
        if !s.is_empty() {
            writeln!(
                writer,
                "      {dim}{:<12}{reset} {}",
                "Location:",
                sanitize_for_terminal(&s)
            )?;
        }
    }
    if f.confidence != Confidence::CiBatch {
        writeln!(
            writer,
            "      {dim}{:<12}{reset} {}",
            "Confidence:",
            f.confidence.as_str()
        )?;
    }
    Ok(())
}

/// Trim an ISO-8601 timestamp to minute precision, falling back to the
/// full string when the input is shorter than 16 chars.
fn short_timestamp(ts: &str) -> &str {
    ts.get(..16).unwrap_or(ts)
}

/// Format a window duration in milliseconds as a compact human-readable
/// string: `Xms` under 1s, `Xs` under 1min, `XmYs` under 1h (omitting
/// `Ys` when zero), `XhYm` over 1h (omitting `Ym` when zero).
fn format_duration_compact(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let total_secs = ms / 1_000;
    if total_secs < 60 {
        return format!("{total_secs}s");
    }
    let total_mins = total_secs / 60;
    let secs = total_secs % 60;
    if total_mins < 60 {
        if secs == 0 {
            return format!("{total_mins}m");
        }
        return format!("{total_mins}m{secs}s");
    }
    let hours = total_mins / 60;
    let mins = total_mins % 60;
    if mins == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h{mins}m")
    }
}

fn write_severity_changes_section(
    writer: &mut dyn std::io::Write,
    changes: &[sentinel_core::diff::SeverityChange],
    colors: AnsiColors,
) -> std::io::Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let AnsiColors {
        bold,
        yellow,
        red,
        green,
        reset,
        ..
    } = colors;
    writeln!(
        writer,
        "{bold}{yellow}Severity changes ({}):{reset}",
        changes.len()
    )?;
    for change in changes {
        let arrow_color = if change.is_regression() { red } else { green };
        writeln!(
            writer,
            "  [{}] {arrow_color}->{reset} [{}] {} on {} ({})",
            severity_label(&change.before_severity),
            severity_label(&change.after_severity),
            change.finding.finding_type.display_label(),
            change.finding.source_endpoint,
            change.finding.service,
        )?;
    }
    writeln!(writer)
}

fn write_endpoint_deltas_section(
    writer: &mut dyn std::io::Write,
    deltas: &[sentinel_core::diff::EndpointDelta],
    colors: AnsiColors,
) -> std::io::Result<()> {
    if deltas.is_empty() {
        return Ok(());
    }
    let AnsiColors {
        bold,
        cyan,
        red,
        green,
        reset,
        ..
    } = colors;
    writeln!(
        writer,
        "{bold}{cyan}Endpoint I/O op deltas ({}):{reset}",
        deltas.len()
    )?;
    for d in deltas {
        let (color, sign) = if d.delta > 0 { (red, "+") } else { (green, "") };
        writeln!(
            writer,
            "  {color}{sign}{}{reset}  {} on {} ({} -> {})",
            d.delta, d.endpoint, d.service, d.before_io_ops, d.after_io_ops,
        )?;
    }
    writeln!(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_core::detect::suggestions::SuggestedFix;
    use sentinel_core::detect::{Finding, FindingType, GreenImpact, Pattern};
    use sentinel_core::diff::DiffReport;
    use sentinel_core::event::CodeLocation;
    use sentinel_core::report::interpret::InterpretationLevel;

    fn empty_diff() -> DiffReport {
        DiffReport {
            new_findings: vec![],
            resolved_findings: vec![],
            severity_changes: vec![],
            endpoint_metric_deltas: vec![],
        }
    }

    fn sample_finding() -> Finding {
        Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 7_000,
                distinct_params: 6,
            },
            suggestion: "Use WHERE ... IN (?) to batch 5 queries into one".to_string(),
            first_timestamp: "2026-04-20T10:00:01.000Z".to_string(),
            last_timestamp: "2026-04-20T10:00:08.000Z".to_string(),
            green_impact: Some(GreenImpact {
                estimated_extra_io_ops: 5,
                io_intensity_score: 6.0,
                io_intensity_band: InterpretationLevel::for_iis(6.0),
            }),
            confidence: Confidence::CiBatch,
            code_location: Some(CodeLocation {
                function: Some("findItems".to_string()),
                filepath: Some("src/main/java/orders/OrderService.java".to_string()),
                lineno: Some(118),
                namespace: Some("com.foo.orders.OrderService".to_string()),
            }),
            suggested_fix: Some(SuggestedFix {
                pattern: "n_plus_one_sql".to_string(),
                framework: "java_jpa".to_string(),
                recommendation: "Use @BatchSize on the lazy collection".to_string(),
                reference_url: Some("https://docs.example.com/batch".to_string()),
            }),
        }
    }

    fn diff_with_new(findings: Vec<Finding>) -> DiffReport {
        DiffReport {
            new_findings: findings,
            resolved_findings: vec![],
            severity_changes: vec![],
            endpoint_metric_deltas: vec![],
        }
    }

    fn render_text(diff: &DiffReport) -> String {
        let mut buf = Vec::new();
        write_diff_text(&mut buf, diff, no_colors()).unwrap();
        String::from_utf8(buf).expect("render output should be valid UTF-8")
    }

    /// Regression: `write_diff_text` must honor the `colors` argument,
    /// not probe stdout's TTY state. When `emit_diff` writes to a file,
    /// it passes `no_colors()` and the output must contain zero ESC
    /// bytes regardless of whether the process stdout is a terminal.
    #[test]
    fn write_diff_text_respects_colors_argument() {
        let diff = empty_diff();

        let forced = AnsiColors {
            bold: "\x1b[1m",
            cyan: "\x1b[36m",
            red: "\x1b[31m",
            yellow: "\x1b[33m",
            green: "\x1b[32m",
            dim: "\x1b[2m",
            reset: "\x1b[0m",
        };
        let mut colored_buf = Vec::new();
        write_diff_text(&mut colored_buf, &diff, forced).unwrap();
        assert!(
            colored_buf.contains(&0x1b),
            "forced palette must emit at least one ESC byte"
        );

        let mut plain_buf = Vec::new();
        write_diff_text(&mut plain_buf, &diff, no_colors()).unwrap();
        assert!(
            !plain_buf.contains(&0x1b),
            "no_colors palette must emit zero ESC bytes, got:\n{}",
            String::from_utf8_lossy(&plain_buf)
        );
    }

    #[test]
    fn new_finding_with_all_fields_renders_every_label() {
        let out = render_text(&diff_with_new(vec![sample_finding()]));
        assert!(out.contains("Template:"), "missing Template, got:\n{out}");
        assert!(
            out.contains("Occurrences:") && out.contains(" 6"),
            "missing Occurrences, got:\n{out}"
        );
        assert!(out.contains("Window:"), "missing Window, got:\n{out}");
        assert!(
            out.contains("2026-04-20T10:00 -> 2026-04-20T10:00 (7s)"),
            "window line wrong, got:\n{out}"
        );
        assert!(
            out.contains("Suggestion:"),
            "missing Suggestion, got:\n{out}"
        );
        assert!(
            out.contains("Fix [java_jpa]:")
                && out.contains("Use @BatchSize on the lazy collection")
                && out.contains("(https://docs.example.com/batch)"),
            "missing or wrong fix line, got:\n{out}"
        );
        assert!(out.contains("IIS:"), "missing IIS, got:\n{out}");
        assert!(
            out.contains("Extra I/O:") && out.contains("5 avoidable ops"),
            "missing Extra I/O, got:\n{out}"
        );
        assert!(out.contains("Location:"), "missing Location, got:\n{out}");
        assert!(
            out.contains("src/main/java/orders/OrderService.java:118"),
            "location filepath/lineno wrong, got:\n{out}"
        );
    }

    #[test]
    fn new_finding_without_green_impact_omits_iis_and_extra_io() {
        let mut f = sample_finding();
        f.green_impact = None;
        let out = render_text(&diff_with_new(vec![f]));
        assert!(!out.contains("IIS:"), "IIS leaked, got:\n{out}");
        assert!(!out.contains("Extra I/O:"), "Extra I/O leaked, got:\n{out}");
    }

    #[test]
    fn extra_io_is_printed_even_when_zero() {
        let mut f = sample_finding();
        if let Some(ref mut imp) = f.green_impact {
            imp.estimated_extra_io_ops = 0;
        }
        let out = render_text(&diff_with_new(vec![f]));
        assert!(
            out.contains("Extra I/O:") && out.contains("0 avoidable ops"),
            "Extra I/O must be printed at zero for parity with analyze, got:\n{out}"
        );
    }

    #[test]
    fn new_finding_without_suggested_fix_omits_fix_line() {
        let mut f = sample_finding();
        f.suggested_fix = None;
        let out = render_text(&diff_with_new(vec![f]));
        assert!(!out.contains("Fix ["), "fix line leaked, got:\n{out}");
    }

    #[test]
    fn new_finding_without_code_location_omits_location_line() {
        let mut f = sample_finding();
        f.code_location = None;
        let out = render_text(&diff_with_new(vec![f]));
        assert!(
            !out.contains("Location:"),
            "Location line leaked, got:\n{out}"
        );
    }

    #[test]
    fn ci_batch_confidence_is_omitted() {
        let f = sample_finding();
        assert_eq!(f.confidence, Confidence::CiBatch);
        let out = render_text(&diff_with_new(vec![f]));
        assert!(
            !out.contains("Confidence:"),
            "ci_batch confidence must not be printed, got:\n{out}"
        );
    }

    #[test]
    fn daemon_production_confidence_is_printed() {
        let mut f = sample_finding();
        f.confidence = Confidence::DaemonProduction;
        let out = render_text(&diff_with_new(vec![f]));
        assert!(
            out.contains("Confidence:") && out.contains("daemon_production"),
            "daemon_production confidence missing, got:\n{out}"
        );
    }

    #[test]
    fn window_under_one_minute_renders_in_seconds() {
        let mut f = sample_finding();
        f.pattern.window_ms = 5_000;
        let out = render_text(&diff_with_new(vec![f]));
        assert!(out.contains("(5s)"), "expected (5s), got:\n{out}");
    }

    #[test]
    fn window_over_two_hours_renders_with_hours_and_minutes() {
        let mut f = sample_finding();
        f.pattern.window_ms = (2 * 60 * 60 + 12 * 60) * 1_000;
        let out = render_text(&diff_with_new(vec![f]));
        assert!(out.contains("(2h12m)"), "expected (2h12m), got:\n{out}");
    }

    #[test]
    fn resolved_findings_use_same_enriched_format() {
        let diff = DiffReport {
            new_findings: vec![],
            resolved_findings: vec![sample_finding()],
            severity_changes: vec![],
            endpoint_metric_deltas: vec![],
        };
        let out = render_text(&diff);
        assert!(
            out.contains("Resolved findings"),
            "missing resolved header, got:\n{out}"
        );
        assert!(
            out.contains("Occurrences:") && out.contains(" 6"),
            "resolved finding must carry Occurrences, got:\n{out}"
        );
        assert!(
            out.contains("Suggestion:"),
            "resolved finding must carry Suggestion, got:\n{out}"
        );
        assert!(
            out.contains("Fix [java_jpa]:"),
            "resolved finding must carry Fix line, got:\n{out}"
        );
    }

    #[test]
    fn ansi_escape_in_template_is_stripped_from_text_output() {
        let mut f = sample_finding();
        f.pattern.template = "evil\x1b[2J\x1b[H wipe".to_string();
        let out = render_text(&diff_with_new(vec![f]));
        assert!(
            !out.as_bytes().contains(&0x1b),
            "ESC byte from user template leaked into terminal output, got:\n{out}"
        );
        assert!(
            out.contains("evil???[2J???[H wipe") || out.contains("evil?[2J?[H wipe"),
            "control chars must be replaced, got:\n{out}"
        );
    }

    #[test]
    fn osc8_hyperlink_in_suggestion_is_neutralised() {
        let mut f = sample_finding();
        f.suggestion = "click \x1b]8;;https://attacker/\x07here\x1b]8;;\x07".to_string();
        let out = render_text(&diff_with_new(vec![f]));
        assert!(
            !out.as_bytes().contains(&0x1b),
            "OSC 8 ESC leaked, got:\n{out}"
        );
        assert!(
            !out.as_bytes().contains(&0x07),
            "BEL terminator leaked, got:\n{out}"
        );
    }

    #[test]
    fn non_https_reference_url_is_omitted_from_fix_line() {
        let mut f = sample_finding();
        if let Some(ref mut fix) = f.suggested_fix {
            fix.reference_url = Some("http://insecure.example.com/doc".to_string());
        }
        let out = render_text(&diff_with_new(vec![f]));
        assert!(
            !out.contains("http://insecure.example.com/doc"),
            "non-HTTPS URL must not be printed, got:\n{out}"
        );
        assert!(
            out.contains("Fix [java_jpa]:") && out.contains("Use @BatchSize"),
            "fix recommendation must still render, got:\n{out}"
        );
    }

    #[test]
    fn javascript_scheme_reference_url_is_omitted() {
        let mut f = sample_finding();
        if let Some(ref mut fix) = f.suggested_fix {
            fix.reference_url = Some("javascript:alert(1)".to_string());
        }
        let out = render_text(&diff_with_new(vec![f]));
        assert!(
            !out.contains("javascript:"),
            "javascript: URL must not be printed, got:\n{out}"
        );
    }

    #[test]
    fn url_with_control_chars_is_omitted() {
        let mut f = sample_finding();
        if let Some(ref mut fix) = f.suggested_fix {
            fix.reference_url = Some("https://docs.example.com/\x1b[31m".to_string());
        }
        let out = render_text(&diff_with_new(vec![f]));
        assert!(
            !out.contains("https://docs.example.com/"),
            "URL with control chars must not be printed, got:\n{out}"
        );
    }

    fn finding_with(severity: Severity, ftype: FindingType) -> Finding {
        let mut f = sample_finding();
        f.severity = severity;
        f.finding_type = ftype;
        f
    }

    #[test]
    fn severity_breakdown_is_none_for_empty_input() {
        assert!(format_severity_breakdown(&[], no_colors()).is_none());
    }

    #[test]
    fn severity_breakdown_counts_each_bucket() {
        let findings = vec![
            finding_with(Severity::Critical, FindingType::NPlusOneSql),
            finding_with(Severity::Critical, FindingType::SlowSql),
            finding_with(Severity::Warning, FindingType::RedundantSql),
            finding_with(Severity::Info, FindingType::SlowHttp),
        ];
        let line = format_severity_breakdown(&findings, no_colors())
            .expect("non-empty input must yield a line");
        assert!(line.contains("2 critical"), "got: {line}");
        assert!(line.contains("1 warning"), "got: {line}");
        assert!(line.contains("1 info"), "got: {line}");
    }

    #[test]
    fn top_finding_types_skipped_when_five_or_fewer() {
        let findings = vec![
            finding_with(Severity::Warning, FindingType::NPlusOneSql),
            finding_with(Severity::Warning, FindingType::NPlusOneSql),
            finding_with(Severity::Warning, FindingType::SlowSql),
            finding_with(Severity::Warning, FindingType::RedundantSql),
            finding_with(Severity::Warning, FindingType::SlowHttp),
        ];
        assert!(
            format_top_finding_types(&findings).is_none(),
            "5 findings must not surface a `Most common` line"
        );
    }

    #[test]
    fn top_finding_types_takes_two_when_total_between_six_and_ten() {
        let mut findings = Vec::new();
        for _ in 0..3 {
            findings.push(finding_with(Severity::Warning, FindingType::NPlusOneSql));
        }
        for _ in 0..2 {
            findings.push(finding_with(Severity::Warning, FindingType::RedundantSql));
        }
        for _ in 0..2 {
            findings.push(finding_with(Severity::Warning, FindingType::SlowHttp));
        }
        let line = format_top_finding_types(&findings).expect("7 findings must surface a line");
        assert!(line.contains("Most common:"), "got: {line}");
        assert!(line.contains("n_plus_one_sql (3)"), "got: {line}");
        assert_eq!(
            line.matches('(').count(),
            2,
            "expected 2 entries, got: {line}"
        );
    }

    #[test]
    fn top_finding_types_takes_three_when_total_over_ten() {
        let mut findings = Vec::new();
        for _ in 0..5 {
            findings.push(finding_with(Severity::Warning, FindingType::NPlusOneSql));
        }
        for _ in 0..4 {
            findings.push(finding_with(Severity::Warning, FindingType::RedundantSql));
        }
        for _ in 0..3 {
            findings.push(finding_with(Severity::Warning, FindingType::SlowHttp));
        }
        for _ in 0..1 {
            findings.push(finding_with(Severity::Warning, FindingType::SlowSql));
        }
        let line = format_top_finding_types(&findings).expect("13 findings must surface a line");
        assert!(line.contains("n_plus_one_sql (5)"), "got: {line}");
        assert!(line.contains("redundant_sql (4)"), "got: {line}");
        assert!(line.contains("slow_http (3)"), "got: {line}");
        assert!(
            !line.contains("slow_sql (1)"),
            "should cap at top 3, got: {line}"
        );
    }

    #[test]
    fn top_finding_types_does_not_panic_on_thousand_entries() {
        let findings: Vec<Finding> = (0..1000)
            .map(|i| {
                let kind = match i % 4 {
                    0 => FindingType::NPlusOneSql,
                    1 => FindingType::RedundantSql,
                    2 => FindingType::SlowHttp,
                    _ => FindingType::SlowSql,
                };
                finding_with(Severity::Warning, kind)
            })
            .collect();
        let line = format_top_finding_types(&findings).expect("1000 findings must surface a line");
        assert!(line.contains("Most common:"), "got: {line}");
    }

    #[test]
    fn intensity_source_label_covers_every_variant() {
        assert_eq!(intensity_source_label(IntensitySource::Annual), "annual");
        assert_eq!(intensity_source_label(IntensitySource::Hourly), "hourly");
        assert_eq!(
            intensity_source_label(IntensitySource::MonthlyHourly),
            "monthly_hourly"
        );
        assert_eq!(
            intensity_source_label(IntensitySource::RealTime),
            "real_time"
        );
    }

    #[test]
    fn empty_diff_keeps_no_differences_message() {
        let out = render_text(&empty_diff());
        assert!(
            out.contains("No differences detected between the two trace sets."),
            "no-diff message missing, got:\n{out}"
        );
    }

    #[test]
    fn duration_format_covers_all_branches() {
        assert_eq!(format_duration_compact(0), "0ms");
        assert_eq!(format_duration_compact(750), "750ms");
        assert_eq!(format_duration_compact(1_000), "1s");
        assert_eq!(format_duration_compact(59_000), "59s");
        assert_eq!(format_duration_compact(60_000), "1m");
        assert_eq!(format_duration_compact(125_000), "2m5s");
        assert_eq!(format_duration_compact(3_600_000), "1h");
        assert_eq!(format_duration_compact(3_660_000), "1h1m");
    }

    #[test]
    fn short_timestamp_truncates_to_minute_and_falls_back_for_short_input() {
        assert_eq!(
            short_timestamp("2026-04-20T10:00:01.000Z"),
            "2026-04-20T10:00"
        );
        assert_eq!(short_timestamp("short"), "short");
    }
}
