//! Colored CLI rendering helpers: ANSI palette, findings/offender/gate
//! pretty-printers and the top-level `emit_report_and_gate` used by every
//! command that produces a `Report`.

use sentinel_core::detect::Severity;
use sentinel_core::report::json::JsonReportSink;
use sentinel_core::report::{Report, ReportSink};

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
    println!();
    for (i, finding) in findings.iter().enumerate() {
        print_finding_entry(i, finding, colors);
        println!();
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
    println!("    {dim}Trace:{reset}    {}", finding.trace_id);
    println!("    {dim}Service:{reset}  {}", finding.service);
    println!("    {dim}Endpoint:{reset} {}", finding.source_endpoint);
    if let Some(ref loc) = finding.code_location {
        let src = loc.display_string();
        if !src.is_empty() {
            println!("    {dim}Source:{reset}   {src}");
        }
    }
    println!("    {dim}Template:{reset} {}", finding.pattern.template);
    println!(
        "    {dim}Hits:{reset}     {} occurrences, {} distinct params, {}ms window",
        finding.pattern.occurrences, finding.pattern.distinct_params, finding.pattern.window_ms
    );
    println!(
        "    {dim}Window:{reset}   {} -> {}",
        finding.first_timestamp, finding.last_timestamp
    );
    println!("    {cyan}Suggestion:{reset} {}", finding.suggestion);
    if let Some(ref fix) = finding.suggested_fix {
        match fix.reference_url.as_ref() {
            Some(url) => println!(
                "    {cyan}Suggested fix:{reset} {} (see: {url})",
                fix.recommendation
            ),
            None => println!("    {cyan}Suggested fix:{reset} {}", fix.recommendation),
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

    // Per-region breakdown when more than one region was resolved.
    if summary.regions.len() > 1 {
        println!();
        println!("  {bold}Per-region breakdown:{reset}");
        for region in &summary.regions {
            println!(
                "    - {}: {} I/O ops, {:.6} gCO\u{2082}",
                region.region, region.io_ops, region.co2_gco2,
            );
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
        reset,
        ..
    } = ansi_colors(force_color);

    let gate_color = if gate.passed { green } else { red };
    let gate_label = if gate.passed { "PASSED" } else { "FAILED" };
    println!("{bold}Quality gate: {gate_color}{gate_label}{reset}");
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
        dim,
        reset,
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

    if !diff.new_findings.is_empty() {
        writeln!(writer, "{bold}{red}New findings ({new_count}):{reset}")?;
        for f in &diff.new_findings {
            writeln!(
                writer,
                "  {red}+{reset} [{}] {} on {} ({})",
                severity_label(&f.severity),
                f.finding_type.display_label(),
                f.source_endpoint,
                f.service,
            )?;
            writeln!(writer, "      {dim}template:{reset} {}", f.pattern.template)?;
        }
        writeln!(writer)?;
    }

    if !diff.resolved_findings.is_empty() {
        writeln!(
            writer,
            "{bold}{green}Resolved findings ({resolved_count}):{reset}"
        )?;
        for f in &diff.resolved_findings {
            writeln!(
                writer,
                "  {green}-{reset} [{}] {} on {} ({})",
                severity_label(&f.severity),
                f.finding_type.display_label(),
                f.source_endpoint,
                f.service,
            )?;
            writeln!(writer, "      {dim}template:{reset} {}", f.pattern.template)?;
        }
        writeln!(writer)?;
    }

    if !diff.severity_changes.is_empty() {
        writeln!(
            writer,
            "{bold}{yellow}Severity changes ({changed_count}):{reset}"
        )?;
        for change in &diff.severity_changes {
            let arrow = if change.is_regression() {
                format!("{red}->{reset}")
            } else {
                format!("{green}->{reset}")
            };
            writeln!(
                writer,
                "  [{}] {arrow} [{}] {} on {} ({})",
                severity_label(&change.before_severity),
                severity_label(&change.after_severity),
                change.finding.finding_type.display_label(),
                change.finding.source_endpoint,
                change.finding.service,
            )?;
        }
        writeln!(writer)?;
    }

    if !diff.endpoint_metric_deltas.is_empty() {
        writeln!(
            writer,
            "{bold}{cyan}Endpoint I/O op deltas ({endpoint_change_count}):{reset}"
        )?;
        for d in &diff.endpoint_metric_deltas {
            let (color, sign) = if d.delta > 0 { (red, "+") } else { (green, "") };
            writeln!(
                writer,
                "  {color}{sign}{}{reset}  {} on {} ({} -> {})",
                d.delta, d.endpoint, d.service, d.before_io_ops, d.after_io_ops,
            )?;
        }
        writeln!(writer)?;
    }

    if new_count == 0 && resolved_count == 0 && changed_count == 0 && endpoint_change_count == 0 {
        writeln!(
            writer,
            "{green}No differences detected between the two trace sets.{reset}"
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_diff() -> sentinel_core::diff::DiffReport {
        sentinel_core::diff::DiffReport {
            new_findings: vec![],
            resolved_findings: vec![],
            severity_changes: vec![],
            endpoint_metric_deltas: vec![],
        }
    }

    /// Regression: `write_diff_text` must honor the `colors` argument,
    /// not probe stdout's TTY state. When `emit_diff` writes to a file,
    /// it passes `no_colors()` and the output must contain zero ESC
    /// bytes regardless of whether the process stdout is a terminal.
    #[test]
    fn write_diff_text_respects_colors_argument() {
        let diff = empty_diff();

        // Forced-color palette: output MUST contain ESC bytes.
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

        // no_colors() palette: output MUST NOT contain any ESC byte.
        let mut plain_buf = Vec::new();
        write_diff_text(&mut plain_buf, &diff, no_colors()).unwrap();
        assert!(
            !plain_buf.contains(&0x1b),
            "no_colors palette must emit zero ESC bytes, got:\n{}",
            String::from_utf8_lossy(&plain_buf)
        );
    }
}
