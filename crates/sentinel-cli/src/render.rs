//! Colored CLI rendering helpers: ANSI palette, findings/offender/gate
//! pretty-printers and the top-level `emit_report_and_gate` used by every
//! command that produces a `Report`.

use std::borrow::Cow;
use std::collections::HashMap;

use sentinel_core::detect::Severity;
use sentinel_core::report::json::JsonReportSink;
use sentinel_core::report::{Report, ReportSink};
use sentinel_core::score::carbon::IntensitySource;
use sentinel_core::text_safety::{safe_url, sanitize_for_terminal, strip_code_ticks};

use crate::OutputFormat;

/// Emit the final report in the requested format and enforce the quality
/// gate in CI mode. A failed gate when `ci` is true exits with status `1`.
/// A report write failure exits with `crate::EXIT_TOOLING_ERROR`. The two
/// stay distinct because only the gate-failure exit means a threshold
/// breach, see docs/CI.md "Exit codes".
///
/// The gate check runs *after* the write but takes precedence over a write
/// failure. If a regression coincides with a broken pipe or a full disk,
/// the process still exits `1`, never the tolerable `75`, so the
/// regression is never masked as a tooling blip. See `exit_code_after_gate`.
///
/// `show_acknowledged` controls whether the structured sinks (JSON, SARIF)
/// surface `acknowledged_findings`. The text sink always prints a one-line
/// count when acks matched and prints per-ack details only when
/// `show_acknowledged` is true.
///
/// Takes `&mut Report` so the JSON / SARIF emit paths can hide
/// `acknowledged_findings` via a zero-copy `mem::take`+restore around
/// the emit call, avoiding a deep clone of the whole report on large
/// baselines.
pub(crate) fn emit_report_and_gate(
    report: &mut Report,
    format: Option<OutputFormat>,
    ci: bool,
    label: &str,
    sort: Option<FindingsSort>,
    embed_traces: Option<Vec<sentinel_core::correlate::Trace>>,
    show_acknowledged: bool,
) {
    let effective_format = effective_format(format, ci);
    // Shared by every subcommand that ends here. The sort is applied
    // after the caller's ack pass (a masked finding must not weigh in the
    // aggregate) and before any sink, so `--format json --sort impact`
    // comes out ranked. The embed comes AFTER the sort, because its byte
    // budget keeps the traces of the first findings: budget-trimmed before
    // sorting, it would keep the detector-order head and leave the top
    // rows of the sorted list without traces. Only the JSON sink
    // serializes the trees. The text and SARIF paths never pay the clones.
    if let Some(mode) = sort {
        sort_findings(&mut report.findings, mode);
    }
    if let Some(traces) = embed_traces
        && matches!(effective_format, OutputFormat::Json)
    {
        sentinel_core::report::embedded::embed_finding_traces(report, &traces);
    }

    // Capture the write outcome instead of exiting on it inline: the gate
    // check below must be able to override a write failure.
    let write_result: Result<(), String> = match effective_format {
        OutputFormat::Text => {
            // The text sink always sees the live report so the count
            // footer can surface acked entries even when their full
            // detail is suppressed. `println!` cannot report a write
            // error here (it panics on EPIPE), so Text is always Ok.
            format_colored_report_with_acks(report, label, false, show_acknowledged);
            Ok(())
        }
        OutputFormat::Json => with_optional_acks_hidden(report, show_acknowledged, |r| {
            let sink = JsonReportSink;
            sink.emit(r)
                .map_err(|e| format!("Error writing report: {e}"))
        }),
        OutputFormat::Sarif => with_optional_acks_hidden(report, show_acknowledged, |r| {
            sentinel_core::report::sarif::emit_sarif(r)
                .map_err(|e| format!("Error writing SARIF report: {e}"))
        }),
    };

    let gate_failed = ci && !report.quality_gate.passed;
    match exit_code_after_gate(gate_failed, write_result.is_err()) {
        None => {}
        Some(1) => {
            eprintln!("Quality gate FAILED");
            std::process::exit(1);
        }
        Some(code) => {
            if let Err(msg) = write_result {
                eprintln!("{msg}");
            }
            std::process::exit(code);
        }
    }
}

/// The sink a `format` flag resolves to: `--ci` defaults to JSON so a
/// pipeline reads structured output, everything else to text.
pub(crate) fn effective_format(format: Option<OutputFormat>, ci: bool) -> OutputFormat {
    format.unwrap_or(if ci {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    })
}

/// Decide the process exit code after a report emit under the CI gate.
/// A gate breach (`Some(1)`) takes precedence over a report write failure
/// (`Some(EXIT_TOOLING_ERROR)`): a regression must block even when
/// the write also failed, so it is never masked as a tolerable tooling
/// blip on a broken pipe or a full disk. `None` means clean success.
fn exit_code_after_gate(gate_failed: bool, write_failed: bool) -> Option<i32> {
    if gate_failed {
        Some(1)
    } else if write_failed {
        Some(crate::EXIT_TOOLING_ERROR)
    } else {
        None
    }
}

/// Hide `report.acknowledged_findings` for the duration of `emit`, then
/// restore it. Avoids cloning the full Report when the operator chose
/// not to surface the ack details. Returns the emit result (mapped to a
/// caller-supplied error message) rather than exiting, so the caller can
/// let a quality-gate breach take precedence over a write failure. A
/// write failure here (disk full, permission denied) happens after the
/// report was already computed successfully, so it is never itself a
/// quality-gate breach.
fn with_optional_acks_hidden<F>(
    report: &mut Report,
    show_acknowledged: bool,
    emit: F,
) -> Result<(), String>
where
    F: FnOnce(&Report) -> Result<(), String>,
{
    let stash = if show_acknowledged || report.acknowledged_findings.is_empty() {
        Vec::new()
    } else {
        std::mem::take(&mut report.acknowledged_findings)
    };
    let result = emit(report);
    if !stash.is_empty() {
        report.acknowledged_findings = stash;
    }
    result
}

pub(crate) fn print_colored_report(report: &Report, title: &str) {
    format_colored_report(report, title, false);
}

/// ANSI color codes bundled for CLI rendering.
///
/// Named fields avoid destructuring a 7-tuple by counting underscores,
/// which is easy to get wrong. Every field is either an SGR escape
/// sequence or an empty string (when the output is not a terminal).
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

/// Per-span timing statistics of a finding's pattern, as one line, or
/// `None` when the detector populated none of the three (only n+1 and
/// slow do). Shared with the TUI so the two terminal surfaces cannot
/// drift from each other. Same three figures as the dashboard, and the
/// coefficient of variation reads identically there (`cv_x1000` is
/// scaled by 1000, so 523 reads 52.3%). The durations do not: the
/// dashboard's `formatDurationUs` always prints milliseconds while the
/// terminal follows `explain`'s µs/ms/s scale.
pub(crate) fn format_span_timing(pattern: &sentinel_core::detect::Pattern) -> Option<String> {
    let mut parts = Vec::with_capacity(3);
    if let Some(p50) = pattern.span_duration_us_p50 {
        parts.push(format!("p50 {}", format_duration_us(p50)));
    }
    if let Some(p99) = pattern.span_duration_us_p99 {
        parts.push(format!("p99 {}", format_duration_us(p99)));
    }
    if let Some(cv) = pattern.span_duration_cv_x1000 {
        parts.push(format!("CV {:.1}%", f64::from(cv) / 10.0));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Microseconds on the terminal scale, the one `explain`'s span tree
/// already uses: µs under a millisecond, milliseconds under a second,
/// seconds above. This differs from the dashboard's `formatDurationUs`,
/// which prints milliseconds at every magnitude (`0.80 ms`, `2500 ms`).
fn format_duration_us(us: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let us_f = us as f64;
    if us < 1_000 {
        format!("{us} µs")
    } else if us < 1_000_000 {
        format!("{:.1} ms", us_f / 1_000.0)
    } else {
        format!("{:.2} s", us_f / 1_000_000.0)
    }
}

/// How a finding's type was decided, for the surfaces that show it.
/// `None` outside the n+1 family, where the question does not arise.
pub(crate) fn classification_label(
    finding: &sentinel_core::detect::Finding,
) -> Option<&'static str> {
    if !finding.finding_type.as_str().starts_with("n_plus_one_") {
        return None;
    }
    Some(match finding.classification_method {
        Some(sentinel_core::detect::ClassificationMethod::SanitizerHeuristic) => {
            "sanitizer heuristic"
        }
        _ => "direct",
    })
}

/// Suffix appended to the per-region terminal line surfacing the
/// `Electricity Maps` `isEstimated` / `estimationMethod` metadata.
/// Empty when the region carries no estimation metadata (older JSON
/// reports without the field, non-`Electricity Maps` sources). The
/// returned value still has to go through `sanitize_for_terminal` at
/// the print sink because `estimation_method` may originate from a
/// user-supplied `--input` JSON that bypasses the API-side sanitizer.
fn format_estimation_suffix(
    is_estimated: Option<bool>,
    estimation_method: Option<&str>,
) -> Cow<'static, str> {
    match (is_estimated, estimation_method) {
        (Some(true), Some(method)) => Cow::Owned(format!(", estimated/{method}")),
        (Some(true), None) => Cow::Borrowed(", estimated"),
        (Some(false), _) => Cow::Borrowed(", measured"),
        (None, _) => Cow::Borrowed(""),
    }
}

/// Build the "Carbon scoring: Electricity Maps ..." header line printed
/// before the per-region breakdown. Two borrowed arms cover the most
/// common shapes (v4/v3 with both knobs at default), and the fallback
/// allocates for opt-in combinations. All three fields originate from
/// typed enums with bounded variants, so no terminal-sanitization is
/// needed at the print sink (unlike `intensity_estimation_method`,
/// which carries a free-form `String` from `--input` JSON).
fn format_scoring_config_line(
    cfg: &sentinel_core::score::carbon::ScoringConfig,
) -> Cow<'static, str> {
    use sentinel_core::score::electricity_maps::config::ApiVersion;
    use sentinel_core::score::electricity_maps::config::EmissionFactorType;
    use sentinel_core::score::electricity_maps::config::TemporalGranularity;
    match (
        cfg.api_version,
        cfg.emission_factor_type,
        cfg.temporal_granularity,
    ) {
        (ApiVersion::V4, EmissionFactorType::Lifecycle, TemporalGranularity::Hourly) => {
            Cow::Borrowed("  Carbon scoring: Electricity Maps v4, lifecycle, hourly")
        }
        (ApiVersion::V3, EmissionFactorType::Lifecycle, TemporalGranularity::Hourly) => {
            Cow::Borrowed("  Carbon scoring: Electricity Maps v3, lifecycle, hourly")
        }
        (api, efm, tg) => Cow::Owned(format!(
            "  Carbon scoring: Electricity Maps {}, {}, {}",
            api.as_chip_label(),
            efm.as_query_value(),
            tg.as_query_value(),
        )),
    }
}

pub(crate) fn format_colored_report(report: &Report, title: &str, force_color: bool) {
    format_colored_report_with_acks(report, title, force_color, false);
}

pub(crate) fn format_colored_report_with_acks(
    report: &Report,
    title: &str,
    force_color: bool,
    show_acknowledged: bool,
) {
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
    // Surface the OTLP filter tally so the reader of a thin report can
    // tell a clean run from unusable instrumentation.
    if let Some(ingest) = &report.analysis.ingest
        && ingest.spans_filtered > 0
    {
        let gaps = ingest.filtered_missing_db_statement + ingest.filtered_missing_http_url;
        let gap_note = if gaps > 0 {
            format!(", {gaps} I/O span(s) missing db.statement/http.url")
        } else {
            String::new()
        };
        println!(
            "{dim}Ingested {} spans, {} filtered as non-analyzable{gap_note}{reset}",
            ingest.spans_received, ingest.spans_filtered
        );
    }
    println!();

    print_warnings(report, force_color);

    if report.findings.is_empty() {
        println!("{green}No performance anti-patterns detected.{reset}");
    } else {
        print_findings(&report.findings, force_color);
    }

    print_green_summary(
        &report.green_summary,
        report.analysis.traces_analyzed,
        force_color,
    );
    // Demo never enforces the gate (always exits 0), so flag its verdict as
    // informational. Only `cmd_demo` renders with the "demo" title.
    print_quality_gate(&report.quality_gate, force_color, title == "demo");
    print_acknowledged_summary(report, force_color, show_acknowledged);
}

/// The report's warnings in structured form, whichever field carries
/// them: `warning_details` (0.5.19+) when non-empty, otherwise the
/// legacy `warnings: Vec<String>` a pre-0.5.19 report or daemon sends.
/// One helper so every surface applies the same fallback, instead of
/// each caller remembering the older field exists. Only the TUI surfaces
/// need it as data. The text report formats the two forms in place.
#[cfg(feature = "tui")]
pub(crate) fn effective_warnings(report: &Report) -> Vec<sentinel_core::report::warnings::Warning> {
    if report.warning_details.is_empty() {
        report
            .warnings
            .iter()
            .map(|m| sentinel_core::report::warnings::Warning::from_untrusted("warning", m))
            .collect()
    } else {
        report.warning_details.clone()
    }
}

/// Surface snapshot warnings before the findings list. Prefers the
/// structured `warning_details` (0.5.19+) when non-empty, falls back to
/// the legacy `warnings: Vec<String>` field for pre-0.5.19 baselines.
fn print_warnings(report: &Report, force_color: bool) {
    let colors = ansi_colors(force_color);
    let AnsiColors {
        bold,
        yellow,
        reset,
        ..
    } = colors;

    if !report.warning_details.is_empty() {
        println!("{bold}{yellow}Warnings:{reset}");
        for w in &report.warning_details {
            // Backticks live in the data so the HTML can render code chips.
            // A terminal shows them as literal noise.
            let plain = strip_code_ticks(&w.message);
            println!(
                "  [{}] {}",
                sanitize_for_terminal(&w.kind),
                sanitize_for_terminal(&plain),
            );
        }
        println!();
    } else if !report.warnings.is_empty() {
        println!("{bold}{yellow}Warnings:{reset}");
        for w in &report.warnings {
            println!("  {}", sanitize_for_terminal(w));
        }
        println!();
    }
}

/// Surface acknowledged findings at the bottom of the terminal report.
/// Always prints a count line when acks matched. Prints per-ack details
/// only when `show_acknowledged` is true.
fn print_acknowledged_summary(report: &Report, force_color: bool, show_acknowledged: bool) {
    if report.acknowledged_findings.is_empty() {
        return;
    }
    let colors = ansi_colors(force_color);
    let AnsiColors {
        bold, dim, reset, ..
    } = colors;
    let count = report.acknowledged_findings.len();
    println!();
    if show_acknowledged {
        println!(
            "{bold}{count} acknowledged finding{plural} suppressed.{reset}",
            plural = if count == 1 { "" } else { "s" },
        );
        for ack_pair in &report.acknowledged_findings {
            let f = &ack_pair.finding;
            let a = &ack_pair.acknowledgment;
            println!(
                "  {dim}[ack]{reset} {sig} ({by}, {at}): {reason}",
                sig = sanitize_for_terminal(&f.signature),
                by = sanitize_for_terminal(&a.acknowledged_by),
                at = sanitize_for_terminal(&a.acknowledged_at),
                reason = sanitize_for_terminal(&a.reason),
            );
        }
    } else {
        println!(
            "{dim}{count} additional finding{plural} acknowledged. \
             Run with --show-acknowledged to see them.{reset}",
            plural = if count == 1 { "" } else { "s" },
        );
    }
}

pub(crate) fn print_findings(findings: &[sentinel_core::detect::Finding], force_color: bool) {
    print_findings_with_recurrence(findings, force_color, None);
}

/// Same rendering with the recurrence tallies supplied by the caller:
/// `query findings` receives rows the daemon already folded, so counting
/// signatures here would find 1 everywhere.
pub(crate) fn print_findings_with_recurrence(
    findings: &[sentinel_core::detect::Finding],
    force_color: bool,
    external: Option<HashMap<String, RecurrenceStats>>,
) {
    let colors = ansi_colors(force_color);
    println!(
        "{}Found {} finding(s):{}",
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
    // Detection is per trace, so a recurring problem is many findings.
    // The first one prints in full with its recurrence tally, and the
    // repeats compact to one line each: on real captures the repeats
    // were the bulk of the output without adding anything to read.
    let recurrence = external.unwrap_or_else(|| build_recurrence_index(findings));
    let mut first_seen: HashMap<String, usize> = HashMap::new();
    // Worst severity already printed in full per signature. Signatures are
    // severity-independent, so folding on first-seen alone would stub out a
    // critical detection behind a warning's block and hide its occurrence
    // count, timestamps and suggestion.
    let mut printed_worst: HashMap<String, Severity> = HashMap::new();
    let mut last_was_stub = false;
    for (i, finding) in findings.iter().enumerate() {
        let key = recurrence_key(finding);
        let outranks_printed = printed_worst
            .get(&key)
            .is_none_or(|worst| finding.severity < *worst);
        if !outranks_printed {
            let first = first_seen.get(&key).copied().unwrap_or(i + 1);
            print_repeat_stub(i, finding, first, colors);
            last_was_stub = true;
            continue;
        }
        if last_was_stub {
            println!();
            last_was_stub = false;
        }
        printed_worst.insert(key.clone(), finding.severity.clone());
        first_seen.entry(key.clone()).or_insert(i + 1);
        print_finding_entry(i, finding, colors);
        if let Some(stats) = recurrence.get(&key).filter(|s| s.count > 1) {
            let AnsiColors { cyan, reset, .. } = colors;
            let ops = if stats.total_ops > 0 {
                format!(" \u{b7} ~{} avoidable ops in total", stats.total_ops)
            } else {
                String::new()
            };
            println!(
                "    {cyan}Recurrence:{reset} detected in {} traces{ops}",
                stats.count
            );
        }
        println!();
    }
    if last_was_stub {
        println!();
    }
}

/// Aggregate view of one signature across its detections, the terminal
/// twin of the dashboard's recurrence index.
pub(crate) struct RecurrenceStats {
    pub(crate) count: usize,
    pub(crate) total_ops: usize,
}

/// Group detections like acknowledgments do, falling back to the
/// stable fields when a finding predates signatures.
pub(crate) fn recurrence_key(f: &sentinel_core::detect::Finding) -> String {
    let signature = if f.signature.is_empty() {
        format!(
            "{}|{}|{}|{}",
            f.finding_type.as_str(),
            f.service,
            f.source_endpoint,
            f.pattern.template
        )
    } else {
        f.signature.clone()
    };
    if let Some(grouping) = f.effective_grouping() {
        format!(
            "{}:{}|{}:{}{}",
            grouping.key.len(),
            grouping.key,
            grouping.value.len(),
            grouping.value,
            signature
        )
    } else {
        signature
    }
}

fn extra_ops_of(f: &sentinel_core::detect::Finding) -> usize {
    f.green_impact
        .as_ref()
        .map_or(0, |gi| gi.estimated_extra_io_ops)
}

fn build_recurrence_index(
    findings: &[sentinel_core::detect::Finding],
) -> HashMap<String, RecurrenceStats> {
    let mut index: HashMap<String, RecurrenceStats> = HashMap::new();
    for f in findings {
        let entry = index.entry(recurrence_key(f)).or_insert(RecurrenceStats {
            count: 0,
            total_ops: 0,
        });
        entry.count += 1;
        entry.total_ops += extra_ops_of(f);
    }
    index
}

/// One-line entry for a detection whose signature already printed in
/// full: only what differs from the first block, the trace id.
fn print_repeat_stub(
    index: usize,
    finding: &sentinel_core::detect::Finding,
    first_entry: usize,
    colors: AnsiColors,
) {
    let AnsiColors { dim, reset, .. } = colors;
    let severity_color = severity_color(&finding.severity, colors);
    let severity_label = severity_label(&finding.severity);
    println!(
        "  {severity_color}[{severity_label}]{reset} {dim}#{} {} \u{b7} repeat of #{first_entry} \u{b7} trace {}{reset}",
        index + 1,
        finding.finding_type.display_label(),
        sanitize_for_terminal(&finding.trace_id)
    );
}

/// Sort order for the findings list, the CLI mirror of the dashboard's
/// sort control. Both orders are descending with the other axis as the
/// tie-break, matching the dashboard's defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub(crate) enum FindingsSort {
    /// Highest aggregate avoidable I/O per signature first (the sum over
    /// every detection sharing it), worst severity among equals. Listed
    /// first because it is what the dashboard and the TUI open on, and
    /// the default `report` renders with when no `--sort` is given.
    #[default]
    Impact,
    /// Worst unitary severity first, highest aggregate impact among equals.
    Severity,
}

/// The one place the sort contract lives, so the surfaces the CHANGELOG
/// promises are in parity cannot drift: descending primary key, the
/// other axis as the tie-break. Severity orders Critical < Warning <
/// Info, so ascending severity is worst-first.
pub(crate) fn compare_severity_impact(
    mode: FindingsSort,
    a: (&Severity, u64),
    b: (&Severity, u64),
) -> std::cmp::Ordering {
    let by_severity = a.0.cmp(b.0);
    let by_impact = b.1.cmp(&a.1);
    match mode {
        FindingsSort::Severity => by_severity.then(by_impact),
        FindingsSort::Impact => by_impact.then(by_severity),
    }
}

/// Stable sort, so ties keep the canonical detector order.
pub(crate) fn sort_findings(findings: &mut [sentinel_core::detect::Finding], mode: FindingsSort) {
    let index = build_recurrence_index(findings);
    // Decorate first: `recurrence_key` allocates its `String` per call, so
    // resolving it inside the comparator would pay two allocations per
    // comparison, O(n log n) of them, instead of one per finding.
    let ops: Vec<u64> = findings
        .iter()
        .map(|f| index.get(&recurrence_key(f)).map_or(0, |s| s.total_ops) as u64)
        .collect();
    let mut order: Vec<usize> = (0..findings.len()).collect();
    order.sort_by(|&a, &b| {
        compare_severity_impact(
            mode,
            (&findings[a].severity, ops[a]),
            (&findings[b].severity, ops[b]),
        )
    });
    apply_permutation(findings, &order);
}

/// Reorder `items` in place so that position `i` receives `items[order[i]]`,
/// one swap chase per cycle. Avoids cloning the findings, which carry owned
/// strings and span metadata.
///
/// `order` is the source convention a sorted index vector produces. The
/// cycle-chase below walks the destination convention, so invert first:
/// applied unconverted it performs the inverse permutation, which misorders
/// every cycle of length three or more while leaving swaps intact. A
/// two-element test cannot catch that.
fn apply_permutation<T>(items: &mut [T], order: &[usize]) {
    let mut destination = vec![0usize; order.len()];
    for (position, &source) in order.iter().enumerate() {
        destination[source] = position;
    }
    for i in 0..destination.len() {
        while destination[i] != i {
            let target = destination[i];
            items.swap(i, target);
            destination.swap(i, target);
        }
    }
}

/// One-line severity breakdown printed under the `Found N finding(s):`
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
    // Without this line two identical blocks from two deployments read as
    // a duplicate: the recurrence key splits them and nothing says why. The
    // label is the attribute name, since it is operator config.
    for attr in &finding.grouping {
        println!(
            "    {dim}{}:{reset} {}",
            sanitize_for_terminal(&attr.key),
            sanitize_for_terminal(&attr.value)
        );
    }
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
    if let Some(timing) = format_span_timing(&finding.pattern) {
        println!("    {dim}Timing:{reset}   {timing}");
    }
    if let Some(label) = classification_label(finding) {
        println!("    {dim}Class:{reset}    {label}");
    }
    println!(
        "    {dim}Window:{reset}   {} -> {}",
        fmt_local_iso(&finding.first_timestamp, LOCAL_TIME_FORMAT),
        fmt_local_iso(&finding.last_timestamp, LOCAL_TIME_FORMAT)
    );
    println!(
        "    {cyan}Suggestion:{reset} {}",
        sanitize_for_terminal(&strip_code_ticks(&finding.suggestion))
    );
    if !finding.confidence.is_batch() {
        println!(
            "    {dim}Confidence:{reset} {}",
            finding.confidence.as_str()
        );
    }
    if let Some(ref fix) = finding.suggested_fix {
        let plain = strip_code_ticks(&fix.recommendation);
        let recommendation = sanitize_for_terminal(&plain);
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

/// Sub-1e-5 magnitudes switch to scientific notation instead of
/// collapsing to zeros in fixed-point. Shared by the text report and
/// the monitor TUI so the same value renders identically in both.
pub(crate) fn fmt_tiny(v: f64) -> String {
    // Normalize negative zero so an empty sum never renders "-0.000000".
    let v = if v == 0.0 { 0.0 } else { v };
    if v == 0.0 || v >= 1e-5 {
        format!("{v:.6}")
    } else {
        format!("{v:.3e}")
    }
}

/// The fields one waste line needs, borrowed from either twin.
struct WasteLine<'a> {
    label: &'a str,
    ratio_label: &'a str,
    waste_kwh: f64,
    energy_kwh: f64,
    ratio: f64,
    model: &'a str,
    region: Option<&'a str>,
    waste_gco2: Option<f64>,
}

/// One waste text line, database or broker. Pure so the sanitization and
/// the measured-versus-estimated label are assertable. `region` and
/// `model` flow through user-supplied `--input` JSON, so they are
/// sanitized at the sink.
fn format_waste_line(w: &WasteLine<'_>) -> String {
    let gco2 = w
        .waste_gco2
        .map(|g| format!(", {} gCO\u{2082}", fmt_tiny(g)))
        .unwrap_or_default();
    let region = w.region.map_or_else(String::new, |r| {
        format!(", region {}", sanitize_for_terminal(r))
    });
    let model = if w.model.is_empty() {
        "-".into()
    } else {
        sanitize_for_terminal(w.model)
    };
    // Only the estimated figure is a re-presented share of the report
    // totals. Measured and declared models are additional energy.
    let scope = if w.model == sentinel_core::report::DB_WASTE_MODEL_ESTIMATED {
        "[within the report totals]"
    } else {
        "[excluded from totals]"
    };
    format!(
        // Padded to the same column as the other summary lines.
        "  {:<19}{} kWh of {} kWh ({:.0}% {} ratio, model {model}){gco2}{region} {scope}",
        w.label,
        fmt_tiny(w.waste_kwh),
        fmt_tiny(w.energy_kwh),
        w.ratio * 100.0,
        w.ratio_label,
    )
}

fn format_database_waste_line(db: &sentinel_core::report::DatabaseWaste) -> String {
    format_waste_line(&WasteLine {
        label: "Database waste:",
        ratio_label: "SQL",
        waste_kwh: db.waste_kwh,
        energy_kwh: db.energy_kwh,
        ratio: db.sql_waste_ratio,
        model: &db.model,
        region: db.region.as_deref(),
        waste_gco2: db.waste_gco2,
    })
}

fn format_messaging_waste_line(mw: &sentinel_core::report::MessagingWaste) -> String {
    format_waste_line(&WasteLine {
        label: "Broker waste:",
        ratio_label: "messaging",
        waste_kwh: mw.waste_kwh,
        energy_kwh: mw.energy_kwh,
        ratio: mw.messaging_waste_ratio,
        model: &mw.model,
        region: mw.region.as_deref(),
        waste_gco2: mw.waste_gco2,
    })
}

/// The CO2 block of the `GreenOps` summary. Split out of
/// [`print_green_summary`] to keep that function's branching shallow.
fn print_carbon_summary(carbon: &sentinel_core::score::carbon::CarbonReport) {
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

/// One `Per-region breakdown` line.
///
/// `region` and `estimation_method` reach here from the OTLP `cloud.region`
/// span attribute or a user-supplied `--input` JSON, so both are sanitized at
/// this sink: a hostile producer must not inject ANSI / OSC 8 / control bytes
/// into the operator's terminal.
fn format_region_line(region: &sentinel_core::score::carbon::RegionBreakdown) -> String {
    let region_label = sanitize_for_terminal(&region.region);
    // Unresolved regions carry placeholder zero intensity. Render an explicit
    // marker rather than `0 gCO2/kWh, source: annual` so a reader does not
    // mistake an unknown region for a clean grid.
    if region.status == sentinel_core::score::carbon::REGION_STATUS_UNRESOLVED {
        return format!(
            "    - {region_label}: {} I/O ops, {:.6} gCO\u{2082} (intensity: unresolved, source: -)",
            region.io_ops, region.co2_gco2,
        );
    }
    let source_str = intensity_source_label(region.intensity_source);
    let raw_suffix = format_estimation_suffix(
        region.intensity_estimated,
        region.intensity_estimation_method.as_deref(),
    );
    let estimation_suffix = sanitize_for_terminal(&raw_suffix);
    format!(
        "    - {region_label}: {} I/O ops, {:.6} gCO\u{2082} ({:.0} gCO\u{2082}/kWh, source: {source_str}{estimation_suffix})",
        region.io_ops, region.co2_gco2, region.grid_intensity_gco2_kwh,
    )
}

/// The three figures that can legitimately be absent. Each prints greyed
/// with its cause rather than vanishing, so a reader sees why a number is
/// missing.
fn print_absent_aware_figures(
    summary: &sentinel_core::report::GreenSummary,
    traces_analyzed: usize,
    dim: &str,
    reset: &str,
) {
    match summary.co2.as_ref() {
        Some(carbon) => print_carbon_summary(carbon),
        // The trace count is the only reliable discriminant: a daemon stamps
        // scoring_config whenever Electricity Maps is configured, green
        // scoring off included.
        None if traces_analyzed == 0 => {
            println!("  {dim}Carbon:            not computed (no traces analyzed){reset}");
        }
        None => println!("  {dim}Carbon:            not computed ([green] enabled = false){reset}"),
    }
    match &summary.database_waste {
        Some(db) => println!("{}", format_database_waste_line(db)),
        None => println!(
            "  {dim}Database waste:    not measured (no SQL activity, a measured reading needs [green.alumet.database]){reset}"
        ),
    }
    match &summary.messaging_waste {
        Some(mw) => println!("{}", format_messaging_waste_line(mw)),
        None => println!(
            "  {dim}Broker waste:      not measured (no broker publish spans, or no broker energy backend){reset}"
        ),
    }
}

fn print_green_summary(
    summary: &sentinel_core::report::GreenSummary,
    traces_analyzed: usize,
    force_color: bool,
) {
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
    // Hidden when no SQL ops were seen (HTTP-only run, or a baseline
    // from a version predating the split, which parses as 0).
    if summary.total_sql_io_ops > 0 {
        println!(
            "  SQL share:         {} of {} SQL ops avoidable",
            summary.avoidable_sql_io_ops, summary.total_sql_io_ops,
        );
    }
    // Read the pre-computed band from the struct field (see print_findings).
    let waste_level = summary.io_waste_ratio_band;
    let waste_color = interpret_color(waste_level, colors);
    println!(
        "  I/O waste ratio:   {:.1}% {waste_color}({}){reset}",
        summary.io_waste_ratio * 100.0,
        waste_level.short_label(),
    );

    print_absent_aware_figures(summary, traces_analyzed, dim, reset);

    // Carbon scoring config header. Hidden when Electricity Maps is not
    // configured: `scoring_config` is built on every green-scored run, so its
    // presence alone does not mean the API is in use. The 3 fields are
    // typed enums with bounded variants, so no terminal sanitization is
    // needed.
    if let Some(scoring) = summary
        .scoring_config
        .as_ref()
        .filter(|cfg| cfg.uses_electricity_maps())
    {
        println!();
        println!("{}", format_scoring_config_line(scoring));
    }

    // Per-region breakdown whenever at least one region was resolved.
    // Always emitted (even mono-region) so the intensity source is
    // visible to the user, matching the dashboard's per-region table.
    if !summary.regions.is_empty() {
        println!();
        println!("  {bold}Per-region breakdown:{reset}");
        for region in &summary.regions {
            println!("{}", format_region_line(region));
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
            // `endpoint` and `service` come from span attributes (OTLP
            // sender controls them) or from a `--input` JSON baseline.
            // Sanitize before printing for the same reason as `region`
            // above.
            let endpoint = sanitize_for_terminal(&offender.endpoint);
            let service = sanitize_for_terminal(&offender.service);
            println!(
                "    - {endpoint}: IIS {:.1} {level_color}({}){reset} (service: {service}){co2_str}",
                offender.io_intensity_score,
                level.short_label(),
            );
        }
    }

    // Mandatory disclaimer: only shown when CO₂ estimates were emitted,
    // to avoid noise when green scoring is disabled.
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
    // labelled "high" after raising `n_plus_one_min_occurrences` is not a bug.
    // See README "How to read the report" for the full explanation.
    println!(
        "  {dim}Note: `(healthy/moderate/high/critical)` bands use fixed heuristic \
         thresholds, independent of your `n_plus_one_min_occurrences` / \
         `io_waste_ratio_max` overrides. See README \"How to read the report\".{reset}"
    );

    println!();
}

fn print_quality_gate(
    gate: &sentinel_core::report::QualityGate,
    force_color: bool,
    demo_context: bool,
) {
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
    // Appended after `{reset}` so the note stays uncolored and ANSI-free in pipes.
    let demo_note = if demo_context && !gate.passed {
        " (informational in demo, would exit 1 under analyze --ci)"
    } else {
        ""
    };
    println!("{bold}Quality gate: {gate_color}{gate_label}{reset}{demo_note}");
    for rule in &gate.rules {
        let status_color = if rule.passed { green } else { red };
        let status_label = if rule.passed { "PASS" } else { "FAIL" };
        // A known key is a literal. An unknown one comes off the
        // daemon-snapshot path (`/api/export/report` fed into `report`) and is
        // attacker-influenced, so it sanitizes like the other report fields.
        let rule_name = sentinel_core::quality_gate::rule_label(&rule.rule).map_or_else(
            || sanitize_for_terminal(&rule.rule).into_owned(),
            str::to_string,
        );
        println!(
            "  {dim}-{reset} {rule_name}: {} (actual {}) {status_color}{status_label}{reset}",
            rule.threshold, rule.actual,
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
        Some(path) => {
            // O_NOFOLLOW so a pre-planted symlink at the diff output
            // path cannot redirect writes outside the operator's tree.
            use std::fs::OpenOptions;
            let mut opts = OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.custom_flags(libc::O_NOFOLLOW);
            }
            Box::new(opts.open(path)?)
        }
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
    let mutated_count = diff.mutated_findings.len();
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
         {yellow}{mutated_count} mutated{reset}, \
         {yellow}{changed_count} severity changed{reset} ({regression_count} regression(s)), \
         {endpoint_change_count} endpoint count change(s)"
    )?;
    writeln!(writer)?;

    if !diff.warning_details.is_empty() {
        writeln!(writer, "{bold}{yellow}Warnings (after run):{reset}")?;
        for w in &diff.warning_details {
            // Same guard as `print_warnings`: a baseline JSON is operator
            // input, and ack warnings carry user-authored signatures.
            let plain = strip_code_ticks(&w.message);
            writeln!(
                writer,
                "  [{}] {}",
                sanitize_for_terminal(&w.kind),
                sanitize_for_terminal(&plain)
            )?;
        }
        writeln!(writer)?;
    }

    write_new_findings_section(writer, &diff.new_findings, colors)?;
    write_resolved_findings_section(writer, &diff.resolved_findings, colors)?;
    write_mutated_findings_section(writer, &diff.mutated_findings, colors)?;
    write_severity_changes_section(writer, &diff.severity_changes, colors)?;
    write_endpoint_deltas_section(writer, &diff.endpoint_metric_deltas, colors)?;

    if new_count == 0
        && resolved_count == 0
        && mutated_count == 0
        && changed_count == 0
        && endpoint_change_count == 0
    {
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

/// Template mutations: the same detector on the same service and
/// endpoint whose normalized template changed between the two runs.
/// Printed as a before/after template pair so a reader can judge the
/// pairing at a glance, counted neither as new nor as resolved. A pair
/// whose severity worsened is colored red and shows the transition: the
/// escalation never reaches `severity_changes`, so this line is the only
/// place it shows.
fn write_mutated_findings_section(
    writer: &mut dyn std::io::Write,
    mutated: &[sentinel_core::diff::MutatedFinding],
    colors: AnsiColors,
) -> std::io::Result<()> {
    if mutated.is_empty() {
        return Ok(());
    }
    let AnsiColors {
        bold,
        yellow,
        red,
        dim,
        reset,
        ..
    } = colors;
    writeln!(
        writer,
        "{bold}{yellow}Mutated findings ({}):{reset}",
        mutated.len()
    )?;
    for pair in mutated {
        // Severity derives Ord with declaration order: worse = lower.
        let worsened = pair.after.severity < pair.before.severity;
        let marker_color = if worsened { red } else { yellow };
        let severity = if pair.after.severity == pair.before.severity {
            severity_label(&pair.after.severity).to_string()
        } else {
            format!(
                "{}\u{2192}{}",
                severity_label(&pair.before.severity),
                severity_label(&pair.after.severity)
            )
        };
        writeln!(
            writer,
            "  {marker_color}~{reset} [{severity}] {} on {} ({})",
            pair.after.finding_type.display_label(),
            pair.after.source_endpoint,
            pair.after.service,
        )?;
        writeln!(
            writer,
            "    {dim}Before:{reset} {}",
            sanitize_for_terminal(&pair.before.pattern.template)
        )?;
        writeln!(
            writer,
            "    {dim}After:{reset}  {}",
            sanitize_for_terminal(&pair.after.pattern.template)
        )?;
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

    for attr in &f.grouping {
        writeln!(
            writer,
            "      {dim}{:<12}{reset} {}",
            format!("{}:", sanitize_for_terminal(&attr.key)),
            sanitize_for_terminal(&attr.value)
        )?;
    }
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
        fmt_local_iso(&f.first_timestamp, "%Y-%m-%d %H:%M"),
        fmt_local_iso(&f.last_timestamp, "%Y-%m-%d %H:%M"),
        format_duration_compact(f.pattern.window_ms),
    )?;
    writeln!(
        writer,
        "      {cyan}{:<12}{reset} {}",
        "Suggestion:",
        sanitize_for_terminal(&strip_code_ticks(&f.suggestion))
    )?;
    if let Some(ref fix) = f.suggested_fix {
        let label = format!("Fix [{}]:", sanitize_for_terminal(&fix.framework));
        let plain = strip_code_ticks(&fix.recommendation);
        let recommendation = sanitize_for_terminal(&plain);
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
    if !f.confidence.is_batch() {
        writeln!(
            writer,
            "      {dim}{:<12}{reset} {}",
            "Confidence:",
            f.confidence.as_str()
        )?;
    }
    Ok(())
}

/// Format of human-facing timestamps, always shown in the local time zone.
pub(crate) const LOCAL_TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// A UTC ISO 8601 timestamp in local time, or the sanitized input when it does not parse.
pub(crate) fn fmt_local_iso(iso: &str, format: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso).map_or_else(
        |_| sanitize_for_terminal(iso).into_owned(),
        |t| to_local(&t).format(format).to_string(),
    )
}

/// `t` in the local time zone. Every human-facing time goes through here.
pub(crate) fn to_local<Z: chrono::TimeZone>(
    t: &chrono::DateTime<Z>,
) -> chrono::DateTime<chrono::FixedOffset> {
    static EMBEDDED: std::sync::OnceLock<Option<chrono_tz::Tz>> = std::sync::OnceLock::new();
    let embedded = EMBEDDED.get_or_init(|| {
        embedded_zone(std::env::var("TZ").ok().as_deref(), |name| {
            std::path::Path::new("/usr/share/zoneinfo")
                .join(name)
                .is_file()
        })
    });
    match embedded {
        Some(tz) => t.with_timezone(tz).fixed_offset(),
        None => t.with_timezone(&chrono::Local).fixed_offset(),
    }
}

/// The embedded IANA zone to use when `TZ` names one the system database
/// lacks, as in the `FROM scratch` image, where `chrono::Local` would
/// silently fall back to UTC. `None` leaves the zone to `chrono::Local`:
/// `TZ` unset, a path, a POSIX rule such as `JST-9`, or a name the system
/// database has, which is preferred for being the one the host keeps current.
fn embedded_zone(tz: Option<&str>, system_has: impl Fn(&str) -> bool) -> Option<chrono_tz::Tz> {
    let tz = tz?;
    let name = tz.strip_prefix(':').unwrap_or(tz);
    if name.is_empty() || name.starts_with('/') || system_has(name) {
        return None;
    }
    name.parse().ok()
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
mod tests;
