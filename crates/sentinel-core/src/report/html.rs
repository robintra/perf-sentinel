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
use crate::diff::DiffReport;
use crate::event::EventType;
use crate::ingest::pg_stat::PgStatReport;
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
    /// Optional `pg_stat_statements` report embedded alongside the
    /// analysis. When `Some`, the HTML dashboard exposes a `pg_stat` tab
    /// plus the Explain-to-`pg_stat` cross-navigation for matching SQL
    /// templates.
    pub pg_stat: Option<PgStatReport>,
    /// Optional diff against a baseline run embedded alongside the
    /// analysis. When `Some`, the HTML dashboard exposes a Diff tab
    /// with new/resolved findings, severity changes, and per-endpoint
    /// deltas.
    pub diff: Option<DiffReport>,
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
///     pg_stat: None,
///     diff: None,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pg_stat: Option<&'a PgStatReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<&'a DiffReport>,
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
        trim_to_size_target(ordered, report, options)
    };

    let embedded_traces = kept_refs.iter().copied().map(embed_trace).collect();

    Payload {
        version: env!("CARGO_PKG_VERSION"),
        input_label: options.input_label.as_str(),
        report,
        embedded_traces,
        trimmed_traces: trimmed,
        pg_stat: options.pg_stat.as_ref(),
        diff: options.diff.as_ref(),
    }
}

/// Filter traces to those referenced by a finding and sort by
/// per-trace IIS (highest first). Lower `top_offenders` index means
/// higher IIS. Traces whose `(service, endpoint)` pairs are absent from
/// `top_offenders` rank as `usize::MAX` and sort last.
fn order_candidates_by_iis<'a>(report: &Report, traces: &'a [Trace]) -> Vec<&'a Trace> {
    let finding_trace_ids: HashSet<&str> = report
        .findings
        .iter()
        .map(|f| f.trace_id.as_str())
        .collect();

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
            rank.get(&(s.event.service.as_str(), s.event.source.endpoint.as_str()))
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
    options: &'a RenderOptions,
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
            input_label: options.input_label.as_str(),
            report,
            embedded_traces,
            trimmed_traces: trimmed.clone(),
            pg_stat: options.pg_stat.as_ref(),
            diff: options.diff.as_ref(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlate::Trace;
    use crate::detect::{Confidence, Finding, FindingType, Pattern, Severity};
    use crate::event::{EventSource, EventType, SpanEvent};
    use crate::ingest::IngestSource;
    use crate::normalize::NormalizedEvent;
    use crate::report::interpret::InterpretationLevel;
    use crate::report::{Analysis, GreenSummary, QualityGate, Report, TopOffender};

    fn span(
        trace_id: &str,
        span_id: &str,
        parent: Option<&str>,
        service: &str,
        endpoint: &str,
        template: &str,
    ) -> NormalizedEvent {
        NormalizedEvent {
            event: SpanEvent {
                timestamp: "2026-04-21T00:00:00Z".into(),
                trace_id: trace_id.into(),
                span_id: span_id.into(),
                parent_span_id: parent.map(ToString::to_string),
                service: service.into(),
                cloud_region: None,
                event_type: EventType::Sql,
                operation: "SELECT".into(),
                target: template.into(),
                duration_us: 1200,
                source: EventSource {
                    endpoint: endpoint.into(),
                    method: "get".into(),
                },
                status_code: None,
                response_size_bytes: None,
                code_function: None,
                code_filepath: None,
                code_lineno: None,
                code_namespace: None,
            },
            template: template.into(),
            params: vec![],
        }
    }

    fn finding(trace_id: &str, service: &str, endpoint: &str, template: &str) -> Finding {
        Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Critical,
            trace_id: trace_id.into(),
            service: service.into(),
            source_endpoint: endpoint.into(),
            pattern: Pattern {
                template: template.into(),
                occurrences: 12,
                window_ms: 100,
                distinct_params: 12,
            },
            suggestion: "use JOIN FETCH".into(),
            first_timestamp: "2026-04-21T00:00:00Z".into(),
            last_timestamp: "2026-04-21T00:00:01Z".into(),
            green_impact: None,
            confidence: Confidence::CiBatch,
            code_location: None,
            suggested_fix: None,
        }
    }

    fn minimal_report(findings: Vec<Finding>) -> Report {
        Report {
            analysis: Analysis {
                duration_ms: 10,
                events_processed: 1,
                traces_analyzed: 1,
            },
            findings,
            green_summary: GreenSummary {
                total_io_ops: 10,
                avoidable_io_ops: 4,
                io_waste_ratio: 0.4,
                io_waste_ratio_band: InterpretationLevel::Moderate,
                top_offenders: vec![TopOffender {
                    endpoint: "/api/orders".into(),
                    service: "order-svc".into(),
                    io_intensity_score: 6.4,
                    io_intensity_band: InterpretationLevel::High,
                    co2_grams: Some(0.000_050),
                }],
                co2: None,
                regions: vec![],
                transport_gco2: None,
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
        }
    }

    fn opts(label: &str, cap: Option<usize>) -> RenderOptions {
        RenderOptions {
            input_label: label.into(),
            max_traces_embedded: cap,
            pg_stat: None,
            diff: None,
        }
    }

    #[test]
    fn renders_minimal_report_to_valid_html() {
        // Load the shared raw-trace fixture, run the pipeline, render.
        // Exercises end-to-end at the crate boundary without needing
        // the CLI binary. The fixture is deterministic: two findings
        // (one N+1 SQL, one redundant SQL) on a single trace.
        let path = format!(
            "{}/../../tests/fixtures/report_minimal.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = std::fs::read(&path).expect("fixture readable");
        let cfg = crate::config::Config::default();
        let events = crate::ingest::json::JsonIngest::new(cfg.max_payload_size)
            .ingest(&raw)
            .expect("fixture parses");
        let (report, traces) = crate::pipeline::analyze_with_traces(events, &cfg);

        assert_eq!(report.findings.len(), 2, "fixture must yield 2 findings");

        let html = render(&report, &traces, &opts("report_minimal.json", None));
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains(r#"<script id="report-data""#));
        assert!(html.contains("trace-report-minimal"));
        assert!(html.contains("order-svc"));
    }

    #[test]
    fn escapes_closing_script_tag_in_embedded_json() {
        let hostile = "</script><img src=x onerror=alert(1)>";
        let f = finding("t1", "svc", "/ep", hostile);
        let report = minimal_report(vec![f]);
        let trace = Trace {
            trace_id: "t1".into(),
            spans: vec![span("t1", "s1", None, "svc", "/ep", hostile)],
        };
        let html = render(&report, &[trace], &opts("-", None));
        // The raw closing tag must not appear anywhere in the payload.
        // Count the occurrences in the entire document: the static shell
        // has one (`</script>` closing the JSON block) and one (`</script>`
        // closing the app JS), so the total must be exactly 2.
        assert_eq!(
            html.matches("</script>").count(),
            2,
            "user-controlled </script> leaked into the document"
        );
        // And the escaped form must appear (proof that the hostile
        // string survived as data, not as markup).
        assert!(html.contains("<\\/script>"));

        // The JSON payload must still round-trip cleanly.
        let start = html.find("<script id=\"report-data\"").expect("script tag");
        let open = html[start..]
            .find('>')
            .expect("script open")
            .saturating_add(1);
        let rest = &html[start + open..];
        let end = rest.find("</script>").expect("script close");
        let json_blob = rest[..end].trim().replace("<\\/", "</");
        let value: serde_json::Value =
            serde_json::from_str(&json_blob).expect("JSON blob parses after <\\/ reversal");
        let finding_tpl = value["report"]["findings"][0]["pattern"]["template"]
            .as_str()
            .expect("template present");
        assert_eq!(finding_tpl, hostile);
    }

    #[test]
    fn escapes_adversarial_control_chars() {
        // Null byte, low control char, DEL, and a 4-byte emoji at a
        // string boundary. serde_json must produce a JSON-safe encoding
        // that parses back losslessly.
        let weird = "a\0b\x01c\x7fd\u{1F600}";
        let f = finding("t1", "svc", "/ep", weird);
        let report = minimal_report(vec![f]);
        let html = render(&report, &[], &opts("traces.json", None));

        let start = html.find("<script id=\"report-data\"").expect("script tag");
        let open = html[start..]
            .find('>')
            .expect("script open")
            .saturating_add(1);
        let rest = &html[start + open..];
        let end = rest.find("</script>").expect("script close");
        let json_blob = rest[..end].trim().replace("<\\/", "</");
        let value: serde_json::Value = serde_json::from_str(&json_blob).expect("JSON round-trips");
        assert_eq!(
            value["report"]["findings"][0]["pattern"]["template"]
                .as_str()
                .unwrap(),
            weird
        );
    }

    #[test]
    fn applies_max_traces_embedded_cap_via_top_waste_fallback() {
        // 100 synthetic traces, one finding per trace, cap = 10.
        let mut findings = Vec::new();
        let mut traces = Vec::new();
        let mut offenders = Vec::new();
        for i in 0..100 {
            let tid = format!("t{i:03}");
            let svc = format!("svc-{i}");
            let ep = format!("/ep-{i}");
            let tpl = format!("SELECT * FROM t{i} WHERE id = ?");
            findings.push(finding(&tid, &svc, &ep, &tpl));
            traces.push(Trace {
                trace_id: tid.clone(),
                spans: vec![span(&tid, "s", None, &svc, &ep, &tpl)],
            });
            // Feed top_offenders so every trace has a rank (no
            // usize::MAX ties that would leave ordering unspecified).
            offenders.push(TopOffender {
                endpoint: ep.clone(),
                service: svc.clone(),
                io_intensity_score: 100.0 - f64::from(i),
                io_intensity_band: InterpretationLevel::High,
                co2_grams: None,
            });
        }
        let mut report = minimal_report(findings);
        report.green_summary.top_offenders = offenders;

        let html = render(&report, &traces, &opts("-", Some(10)));
        let json_blob = extract_payload_json(&html);
        let value: serde_json::Value = serde_json::from_str(&json_blob).unwrap();
        let embedded = value["embedded_traces"].as_array().expect("array");
        assert_eq!(embedded.len(), 10, "exactly 10 traces kept");
        let summary = &value["trimmed_traces"];
        assert_eq!(summary["kept"].as_u64().unwrap(), 10);
        assert_eq!(summary["total"].as_u64().unwrap(), 100);
    }

    #[test]
    fn omits_greenops_section_when_green_disabled() {
        // A GreenSummary where `co2` is None (green scoring disabled)
        // must hide the GreenOps tab on the client side. We verify by
        // asserting the payload reflects the disabled state; the JS
        // bootstrap checks `report.green_summary.co2` to decide tab
        // visibility.
        let f = finding("t1", "svc", "/ep", "SELECT * FROM t");
        let mut report = minimal_report(vec![f.clone()]);
        report.green_summary = GreenSummary::disabled(1);
        let trace = Trace {
            trace_id: "t1".into(),
            spans: vec![span("t1", "s1", None, "svc", "/ep", "SELECT * FROM t")],
        };
        let html = render(&report, &[trace], &opts("-", None));
        let json_blob = extract_payload_json(&html);
        let value: serde_json::Value = serde_json::from_str(&json_blob).unwrap();
        assert!(
            value["report"]["green_summary"]["co2"].is_null()
                || value["report"]["green_summary"].get("co2").is_none(),
            "co2 must be absent when green disabled"
        );
        // Sanity: the static panel-green scaffolding is in the template
        // (starts hidden via inline style), and the JS only reveals it
        // when co2 is present. We assert the template still has the
        // scaffolding (regression guard) but not that the client
        // *shows* it (that's a browser-level assertion).
        assert!(html.contains(r#"id="panel-green""#));
    }

    #[test]
    fn no_forbidden_apis_in_template() {
        let forbidden = [
            ".innerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function(",
            // We intentionally omit a bare `Function(` check: the string
            // "function (" appears many times as IIFE / callback syntax.
            // `new Function(` is the only constructor shape that
            // executes a string as code; a literal `Function(` without
            // `new` would need `window.Function(` to be callable, which
            // is caught by the same `Function(` heuristic below.
        ];
        for needle in forbidden {
            assert!(
                !TEMPLATE.contains(needle),
                "template contains forbidden API: {needle}"
            );
        }
        // Guard against the two spellings of the string-exec
        // constructor that don't start with `new `.
        assert!(!TEMPLATE.contains("window.Function("));
        assert!(!TEMPLATE.contains("globalThis.Function("));
    }

    /// Extract the raw JSON text from the `<script id="report-data">`
    /// block and un-escape `<\/` back to `</` so it parses as JSON.
    fn extract_payload_json(html: &str) -> String {
        let start = html.find("<script id=\"report-data\"").expect("script tag");
        let open = html[start..]
            .find('>')
            .expect("script open")
            .saturating_add(1);
        let rest = &html[start + open..];
        let end = rest.find("</script>").expect("script close");
        rest[..end].trim().replace("<\\/", "</")
    }

    fn synthetic_pg_stat() -> PgStatReport {
        use crate::ingest::pg_stat::{PgStatEntry, PgStatRanking, PgStatReport};
        let entries = vec![
            PgStatEntry {
                query: "SELECT * FROM order_item WHERE order_id = 42".into(),
                normalized_template: "SELECT * FROM order_item WHERE order_id = ?".into(),
                calls: 120,
                total_exec_time_ms: 840.0,
                mean_exec_time_ms: 7.0,
                rows: 500,
                shared_blks_hit: 1000,
                shared_blks_read: 0,
                seen_in_traces: true,
            },
            PgStatEntry {
                query: "SELECT id FROM orders WHERE id = 7".into(),
                normalized_template: "SELECT id FROM orders WHERE id = ?".into(),
                calls: 30,
                total_exec_time_ms: 60.0,
                mean_exec_time_ms: 2.0,
                rows: 30,
                shared_blks_hit: 120,
                shared_blks_read: 0,
                seen_in_traces: false,
            },
        ];
        PgStatReport {
            total_entries: 2,
            top_n: 2,
            rankings: vec![PgStatRanking {
                label: "top by total_exec_time".into(),
                entries,
            }],
        }
    }

    #[test]
    fn embeds_pg_stat_when_provided() {
        let f = finding("t1", "svc", "/ep", "SELECT * FROM t");
        let report = minimal_report(vec![f]);
        let mut options = opts("-", None);
        options.pg_stat = Some(synthetic_pg_stat());
        let html = render(&report, &[], &options);
        let blob = extract_payload_json(&html);
        let value: serde_json::Value = serde_json::from_str(&blob).unwrap();
        let entries = value["pg_stat"]["rankings"][0]["entries"]
            .as_array()
            .expect("entries array");
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0]["normalized_template"].as_str().unwrap(),
            "SELECT * FROM order_item WHERE order_id = ?"
        );
    }

    #[test]
    fn omits_pg_stat_when_absent() {
        let f = finding("t1", "svc", "/ep", "SELECT * FROM t");
        let report = minimal_report(vec![f]);
        let html = render(&report, &[], &opts("-", None));
        let blob = extract_payload_json(&html);
        let value: serde_json::Value = serde_json::from_str(&blob).unwrap();
        assert!(
            value.get("pg_stat").is_none(),
            "pg_stat must be absent when not provided (skip_serializing_if)"
        );
        // The static panel-pgstat scaffolding stays in the template; the
        // JS hides it at runtime based on payload presence. Assert the
        // scaffolding is there so the cross-nav wiring has an anchor to
        // reach even before the tab is registered.
        assert!(html.contains(r#"id="panel-pgstat""#));
    }

    #[test]
    fn embeds_diff_report_when_before_provided() {
        // before: 1 finding. after: 2 findings (same first one + one new).
        let before_finding = finding("t1", "svc", "/ep", "SELECT * FROM t");
        let before = minimal_report(vec![before_finding.clone()]);
        let after_extra = finding("t2", "svc", "/ep2", "SELECT * FROM u");
        let after = minimal_report(vec![before_finding, after_extra]);

        let diff = crate::diff::diff_runs(&before, &after);
        let mut options = opts("-", None);
        options.diff = Some(diff);
        let html = render(&after, &[], &options);
        let blob = extract_payload_json(&html);
        let value: serde_json::Value = serde_json::from_str(&blob).unwrap();
        let new = value["diff"]["new_findings"].as_array().expect("new array");
        assert_eq!(new.len(), 1, "one new finding introduced in 'after'");
        let resolved = value["diff"]["resolved_findings"]
            .as_array()
            .expect("resolved array");
        assert_eq!(resolved.len(), 0, "nothing was removed");
    }

    #[test]
    fn omits_diff_when_absent() {
        let f = finding("t1", "svc", "/ep", "SELECT * FROM t");
        let report = minimal_report(vec![f]);
        let html = render(&report, &[], &opts("-", None));
        let blob = extract_payload_json(&html);
        let value: serde_json::Value = serde_json::from_str(&blob).unwrap();
        assert!(value.get("diff").is_none());
        assert!(html.contains(r#"id="panel-diff""#));
    }

    #[test]
    fn cross_nav_pgstat_link_added_only_when_pg_stat_present() {
        // Build a trace whose SQL span's normalized template matches a
        // pg_stat row. The ps-span-pgstat-link class is added by the JS
        // at render time, not by the Rust sink; what we verify here is
        // that the Rust payload carries everything the JS needs for the
        // link to fire, i.e. a `pg_stat` section with the same
        // `normalized_template` the span carries.
        let tpl = "SELECT * FROM order_item WHERE order_id = ?";
        let f = finding("abc", "svc", "/ep", tpl);
        let report = minimal_report(vec![f]);
        let trace = Trace {
            trace_id: "abc".into(),
            spans: vec![span("abc", "s1", None, "svc", "/ep", tpl)],
        };

        // With pg_stat: the template appears in both the span list and
        // the pg_stat rankings.
        let mut with_pg = opts("-", None);
        with_pg.pg_stat = Some(synthetic_pg_stat());
        let html_with = render(&report, std::slice::from_ref(&trace), &with_pg);
        let blob_with = extract_payload_json(&html_with);
        let v_with: serde_json::Value = serde_json::from_str(&blob_with).unwrap();
        let pg_templates: Vec<&str> = v_with["pg_stat"]["rankings"][0]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["normalized_template"].as_str().unwrap())
            .collect();
        assert!(
            pg_templates.contains(&tpl),
            "pg_stat carries the span template"
        );
        let span_templates: Vec<&str> = v_with["embedded_traces"][0]["spans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["template"].as_str().unwrap())
            .collect();
        assert!(
            span_templates.contains(&tpl),
            "trace carries the same template"
        );

        // The template file also carries the ps-span-pgstat-link class
        // (the JS adds it to rows with matching templates). The grep is
        // on the static template, not on the per-render output, since
        // class assignment happens at DOM construction time in the JS.
        assert!(
            TEMPLATE.contains("ps-span-pgstat-link"),
            "template contains the cross-nav class"
        );

        // Without pg_stat: the embedded JSON has no pg_stat key, so
        // `hasPgStat` in the JS is false and no cross-nav handler ever
        // attaches.
        let without_pg = opts("-", None);
        let html_without = render(&report, &[trace], &without_pg);
        let blob_without = extract_payload_json(&html_without);
        let v_without: serde_json::Value = serde_json::from_str(&blob_without).unwrap();
        assert!(v_without.get("pg_stat").is_none());
    }

    #[test]
    fn pg_stat_sub_switcher_exposes_all_ranking_labels() {
        // The sub-switcher is built client-side from `payload.pg_stat.rankings`,
        // so a server-side render on its own does not produce the chip
        // <button> elements. What we assert here is the contract the JS
        // depends on: the static template still carries the four human
        // labels, the sub-switcher sets `data-ranking-index` via
        // `setAttribute` (not as an inline HTML attribute), and the
        // payload exposes all four rankings in the stable order.
        let labels = [
            "\"Total time\"",
            "\"Calls\"",
            "\"Mean time\"",
            "\"I/O blocks\"",
        ];
        for needle in labels {
            assert!(
                TEMPLATE.contains(needle),
                "template is missing sub-switcher label {needle}"
            );
        }
        // `data-ranking-index` must only appear via `setAttr(..., "data-ranking-index", ...)`.
        // A literal HTML attribute `data-ranking-index="..."` would bypass
        // textContent-only guarantees for attributes carrying dynamic data,
        // so guard against it.
        assert!(
            TEMPLATE.contains("\"data-ranking-index\""),
            "setAttr path must use the attribute name as a string literal"
        );
        assert!(
            !TEMPLATE.contains("data-ranking-index=\""),
            "template must not hard-code a literal data-ranking-index attribute"
        );

        // Payload exposes all four rankings when a full PgStatReport is
        // embedded. Verify against the real `rank_pg_stat` output rather
        // than a hand-built synthetic, so test output matches production.
        let entries = crate::ingest::pg_stat::parse_pg_stat(
            b"query,calls,total_exec_time,mean_exec_time,rows,shared_blks_hit,shared_blks_read\n\
              SELECT a FROM t1,10,100.0,10.0,10,20,5\n\
              SELECT b FROM t2,20,50.0,2.5,20,100,0\n\
              SELECT c FROM t3,5,200.0,40.0,5,200,50\n",
            1_048_576,
        )
        .expect("fixture parses");
        let pg_stat = crate::ingest::pg_stat::rank_pg_stat(&entries, 10);
        let f = finding("t1", "svc", "/ep", "SELECT * FROM t");
        let report = minimal_report(vec![f]);
        let mut options = opts("-", None);
        options.pg_stat = Some(pg_stat);
        let html = render(&report, &[], &options);
        let blob = extract_payload_json(&html);
        let value: serde_json::Value = serde_json::from_str(&blob).unwrap();
        let rankings = value["pg_stat"]["rankings"].as_array().unwrap();
        assert_eq!(rankings.len(), 4, "payload carries all four rankings");
        assert_eq!(
            rankings[0]["label"].as_str().unwrap(),
            "top by total_exec_time"
        );
        assert_eq!(
            rankings[3]["label"].as_str().unwrap(),
            "top by shared_blks_total"
        );
    }

    #[test]
    fn embeds_correlations_when_report_carries_them() {
        // Daemon-produced Reports can carry cross-trace correlations.
        // The template's Correlations tab is guarded on
        // `report.correlations?.length > 0`, so the tab lights up
        // automatically when the field is non-empty. Assert the
        // serialized payload carries the field with the expected shape
        // so the JS sees what it expects.
        use crate::detect::FindingType;
        use crate::detect::correlate_cross::{CorrelationEndpoint, CrossTraceCorrelation};

        let correlation = CrossTraceCorrelation {
            source: CorrelationEndpoint {
                finding_type: FindingType::NPlusOneSql,
                service: "order-svc".to_string(),
                template: "SELECT * FROM o WHERE id = ?".to_string(),
            },
            target: CorrelationEndpoint {
                finding_type: FindingType::SlowHttp,
                service: "payment-svc".to_string(),
                template: "POST /api/charge".to_string(),
            },
            co_occurrence_count: 8,
            source_total_occurrences: 10,
            confidence: 0.8,
            median_lag_ms: 120.0,
            first_seen: "2026-04-21T10:00:00Z".to_string(),
            last_seen: "2026-04-21T10:05:00Z".to_string(),
        };

        let f = finding("t1", "svc", "/ep", "SELECT * FROM t");
        let mut report = minimal_report(vec![f]);
        report.correlations = vec![correlation];
        let html = render(&report, &[], &opts("-", None));
        let blob = extract_payload_json(&html);
        let value: serde_json::Value = serde_json::from_str(&blob).unwrap();

        let corrs = value["report"]["correlations"].as_array().unwrap();
        assert_eq!(corrs.len(), 1);
        assert_eq!(corrs[0]["source"]["service"].as_str().unwrap(), "order-svc");
        assert_eq!(
            corrs[0]["target"]["service"].as_str().unwrap(),
            "payment-svc"
        );
        assert_eq!(corrs[0]["co_occurrence_count"].as_u64().unwrap(), 8);

        // The Correlations panel scaffolding must exist in the static
        // shell so the JS can reveal it without touching innerHTML.
        assert!(html.contains(r#"id="panel-correlations""#));
    }
}
