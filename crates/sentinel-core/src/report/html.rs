//! HTML dashboard sink (single-file output, vanilla JS, `textContent`-only).
//!
//! Emits a self-contained HTML file that renders a completed [`Report`]
//! as an interactive dashboard with Findings, Explain and (when green
//! scoring is enabled) `GreenOps` tabs.
//!
//! # Security model
//!
//! All user-controlled data is injected inside a
//! `<script id="report-data" type="application/json">` block and read
//! once at load time via `Element.textContent`. The bundled JS uses
//! `textContent` and `document.createElement()` exclusively and never
//! calls `innerHTML`, `insertAdjacentHTML`, `document.write`, `eval()`
//! or `new Function()`. The unit test
//! [`tests::no_forbidden_apis_in_template`] greps the template on every
//! build to enforce the rule.
//!
//! Additional defense: [`inject`] escapes the substring `</` in the
//! serialized JSON payload to `<\/` so a user-controlled value
//! (SQL template, HTTP URL, service name) cannot close the `<script>`
//! block early. `\/` is a permitted JSON string escape, so
//! `JSON.parse` recovers the original value unchanged.
//!
//! # Trace embedding
//!
//! Only traces that contain at least one finding are embedded (the empty
//! state in the Explain tab makes free navigation pointless). When
//! `max_traces_embedded` is `None`, the sink targets a ~5 MB HTML file
//! size by trimming the lowest-IIS traces first (top-waste fallback
//! reusing the `top_offenders` ordering). When the user sets
//! `max_traces_embedded` explicitly, that cap is honored exactly,
//! regardless of the size target.
//!
//! See `docs/design/07-CLI-CONFIG-RELEASE.md` for the full design
//! rationale.

use crate::correlate::Trace;
use crate::event::EventType;
use crate::normalize::NormalizedEvent;
use crate::report::Report;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const TEMPLATE: &str = include_str!("html_template.html");
const JSON_PLACEHOLDER: &str = "{{REPORT_JSON}}";
const DEFAULT_SIZE_TARGET_BYTES: usize = 5 * 1024 * 1024;

/// Options controlling HTML rendering.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Label shown in the top bar (filename, `-` for stdin, etc.).
    pub input_label: String,
    /// Explicit cap on embedded traces. When `None`, the sink trims to
    /// fit [`DEFAULT_SIZE_TARGET_BYTES`] using the top-waste fallback.
    pub max_traces_embedded: Option<usize>,
}

/// Render a report to a self-contained HTML string.
///
/// # Panics
///
/// Panics if `serde_json` fails to serialize the payload. The payload
/// is built from `Serialize` types with only string and number keys,
/// so this can only happen on serde internal errors (out-of-memory and
/// similar system-level failures), not on user input.
///
/// # Examples
///
/// ```no_run
/// use sentinel_core::report::html::{render, RenderOptions};
/// use sentinel_core::pipeline::analyze_with_traces;
/// # fn load_events() -> Vec<sentinel_core::event::SpanEvent> { vec![] }
/// let events = load_events();
/// let cfg = sentinel_core::config::Config::default();
/// let (report, traces) = analyze_with_traces(events, &cfg);
/// let html = render(&report, &traces, &RenderOptions {
///     input_label: "traces.json".to_string(),
///     max_traces_embedded: None,
/// });
/// assert!(html.starts_with("<!DOCTYPE html>"));
/// ```
#[must_use]
pub fn render(report: &Report, traces: &[Trace], options: &RenderOptions) -> String {
    let payload = build_payload(report, traces, options);
    // Serialization of our fixed-shape payload cannot fail: all nested
    // types are `Serialize`, keys are `&'static str`, and there are no
    // non-string map keys anywhere in the tree.
    let json = serde_json::to_string(&payload).expect("payload always serializes");
    inject(&json)
}

/// Render and write a rendered HTML dashboard to `output`.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] if the file cannot be
/// created or written.
///
/// # Panics
///
/// Panics if serialization fails for the same reason as [`render`].
pub fn write(
    report: &Report,
    traces: &[Trace],
    options: &RenderOptions,
    output: &Path,
) -> std::io::Result<()> {
    let html = render(report, traces, options);
    std::fs::write(output, html)
}

// --- internal ---

#[derive(Debug, Serialize)]
struct Payload<'a> {
    version: &'static str,
    input_label: &'a str,
    report: &'a Report,
    embedded_traces: Vec<EmbeddedTrace<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trimmed_traces: Option<TrimSummary>,
}

#[derive(Debug, Serialize)]
struct EmbeddedTrace<'a> {
    trace_id: &'a str,
    spans: Vec<EmbeddedSpan<'a>>,
}

#[derive(Debug, Serialize)]
struct EmbeddedSpan<'a> {
    span_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_span_id: Option<&'a str>,
    service: &'a str,
    endpoint: &'a str,
    event_type: &'static str,
    operation: &'a str,
    target: &'a str,
    template: &'a str,
    duration_us: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_code: Option<u16>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct TrimSummary {
    kept: usize,
    total: usize,
}

/// Inject a JSON payload into the template.
///
/// Escapes `</` to `<\/` so a user-controlled string cannot close the
/// `<script>` block early. `\/` is a permitted JSON string escape, so
/// round-tripping through `JSON.parse` recovers the original value.
fn inject(json: &str) -> String {
    let safe = json.replace("</", "<\\/");
    TEMPLATE.replacen(JSON_PLACEHOLDER, &safe, 1)
}

fn build_payload<'a>(
    report: &'a Report,
    traces: &'a [Trace],
    options: &'a RenderOptions,
) -> Payload<'a> {
    let ordered = order_candidates_by_iis(report, traces);
    let total = ordered.len();

    let (kept_refs, trimmed) = if let Some(cap) = options.max_traces_embedded {
        let take = cap.min(total);
        let summary = if take < total {
            Some(TrimSummary { kept: take, total })
        } else {
            None
        };
        (ordered.into_iter().take(take).collect::<Vec<_>>(), summary)
    } else {
        trim_to_size_target(ordered, report, &options.input_label)
    };

    let embedded_traces = kept_refs.iter().copied().map(embed_trace).collect();

    Payload {
        version: env!("CARGO_PKG_VERSION"),
        input_label: options.input_label.as_str(),
        report,
        embedded_traces,
        trimmed_traces: trimmed,
    }
}

/// Filter traces to those referenced by a finding and sort by
/// per-trace IIS (highest first). Lower `top_offenders` index means
/// higher IIS. Traces whose `(service, endpoint)` pairs are absent from
/// `top_offenders` rank as `usize::MAX` and sort last.
fn order_candidates_by_iis<'a>(report: &Report, traces: &'a [Trace]) -> Vec<&'a Trace> {
    let finding_trace_ids: HashSet<&str> =
        report.findings.iter().map(|f| f.trace_id.as_str()).collect();

    let mut rank: HashMap<(&str, &str), usize> = HashMap::new();
    for (i, off) in report.green_summary.top_offenders.iter().enumerate() {
        rank.insert((off.service.as_str(), off.endpoint.as_str()), i);
    }

    let mut scored: Vec<(usize, &'a Trace)> = traces
        .iter()
        .filter(|t| finding_trace_ids.contains(t.trace_id.as_str()))
        .map(|t| (trace_rank(t, &rank), t))
        .collect();
    scored.sort_by_key(|(score, _)| *score);
    scored.into_iter().map(|(_, t)| t).collect()
}

fn trace_rank(trace: &Trace, rank: &HashMap<(&str, &str), usize>) -> usize {
    trace
        .spans
        .iter()
        .map(|s| {
            rank.get(&(
                s.event.service.as_str(),
                s.event.source.endpoint.as_str(),
            ))
            .copied()
            .unwrap_or(usize::MAX)
        })
        .min()
        .unwrap_or(usize::MAX)
}

/// Greedy trim-to-size loop: serialize, measure, drop the lowest-ranked
/// trace if over budget. Bounded by the number of input traces. On
/// realistic inputs (few dozen traces, report JSON under ~200 KB) the
/// first iteration usually fits and no trimming happens.
fn trim_to_size_target<'a>(
    ordered: Vec<&'a Trace>,
    report: &Report,
    input_label: &str,
) -> (Vec<&'a Trace>, Option<TrimSummary>) {
    let total = ordered.len();
    let mut kept = ordered;
    loop {
        let trimmed = if kept.len() < total {
            Some(TrimSummary {
                kept: kept.len(),
                total,
            })
        } else {
            None
        };
        let embedded_traces: Vec<_> = kept.iter().copied().map(embed_trace).collect();
        let payload = Payload {
            version: env!("CARGO_PKG_VERSION"),
            input_label,
            report,
            embedded_traces,
            trimmed_traces: trimmed.clone(),
        };
        let serialized_len = serde_json::to_string(&payload).map_or(0, |s| s.len());
        if serialized_len + TEMPLATE.len() <= DEFAULT_SIZE_TARGET_BYTES || kept.is_empty() {
            return (kept, trimmed);
        }
        kept.pop();
    }
}

fn embed_trace(t: &Trace) -> EmbeddedTrace<'_> {
    EmbeddedTrace {
        trace_id: t.trace_id.as_str(),
        spans: t.spans.iter().map(embed_span).collect(),
    }
}

fn embed_span(e: &NormalizedEvent) -> EmbeddedSpan<'_> {
    EmbeddedSpan {
        span_id: e.event.span_id.as_str(),
        parent_span_id: e.event.parent_span_id.as_deref(),
        service: e.event.service.as_str(),
        endpoint: e.event.source.endpoint.as_str(),
        event_type: match e.event.event_type {
            EventType::Sql => "sql",
            EventType::HttpOut => "http_out",
        },
        operation: e.event.operation.as_str(),
        target: e.event.target.as_str(),
        template: e.template.as_str(),
        duration_us: e.event.duration_us,
        status_code: e.event.status_code,
    }
}
