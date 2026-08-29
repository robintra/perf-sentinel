//! Span shapes carried inside a [`Report`](super::Report) for the HTML
//! dashboard's Explain tab.
//!
//! Masked fields only. The raw `event.target` (the original
//! `db.statement` or URL) carries literals and must never travel here,
//! see [`EmbeddedSpan::from_event`].

use serde::{Deserialize, Serialize};

use crate::correlate::Trace;
use crate::event::EventType;
use crate::normalize::NormalizedEvent;

/// One trace's spans, in the masked form the dashboard renders.
/// `#[non_exhaustive]` so an added field stays a minor bump for downstream
/// crates.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddedTrace {
    pub trace_id: String,
    pub spans: Vec<EmbeddedSpan>,
}

impl EmbeddedTrace {
    /// Mask a correlated trace down to what the Explain tab draws.
    #[must_use]
    pub fn from_trace(trace: &Trace) -> Self {
        Self {
            trace_id: trace.trace_id.clone(),
            spans: trace.spans.iter().map(EmbeddedSpan::from_event).collect(),
        }
    }

    /// Rebuild a masked [`Trace`] for consumers that render span trees
    /// from a report rather than from live input (the TUI on a daemon
    /// snapshot). The raw target was never embedded, so the template
    /// stands in for it and the absent fields stay neutral.
    #[must_use]
    pub fn to_trace(&self) -> Trace {
        Trace {
            trace_id: self.trace_id.clone(),
            spans: self.spans.iter().map(EmbeddedSpan::to_normalized).collect(),
        }
    }
}

/// A single span of an [`EmbeddedTrace`].
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddedSpan {
    pub span_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub service: String,
    pub endpoint: String,
    pub event_type: EventType,
    pub operation: String,
    /// Normalized template. The only query text that ever reaches a
    /// report, the raw statement stays behind.
    pub template: String,
    pub duration_us: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
}

impl EmbeddedSpan {
    /// Mask one normalized event. `event.target` is dropped on purpose:
    /// it holds the literals the template exists to replace.
    #[must_use]
    pub fn from_event(event: &NormalizedEvent) -> Self {
        Self {
            span_id: event.event.span_id.clone(),
            timestamp: event.event.timestamp.clone(),
            parent_span_id: event.event.parent_span_id.clone(),
            service: event.event.service.to_string(),
            endpoint: event.event.source.endpoint.clone(),
            event_type: event.event.event_type.clone(),
            operation: event.event.operation.clone(),
            template: event.template.to_string(),
            duration_us: event.event.duration_us,
            status_code: event.event.status_code,
        }
    }

    /// The masked counterpart of [`Self::from_event`]: same shape, the
    /// template in place of the raw target.
    fn to_normalized(&self) -> NormalizedEvent {
        NormalizedEvent {
            event: crate::event::SpanEvent {
                timestamp: self.timestamp.clone(),
                trace_id: String::new(),
                span_id: self.span_id.clone(),
                parent_span_id: self.parent_span_id.clone(),
                link_trace_id: None,
                service: self.service.as_str().into(),
                grouping: Vec::new(),
                cloud_region: None,
                event_type: self.event_type.clone(),
                operation: self.operation.clone(),
                target: self.template.clone(),
                duration_us: self.duration_us,
                source: crate::event::EventSource {
                    endpoint: self.endpoint.clone(),
                    method: String::new(),
                },
                status_code: self.status_code,
                response_size_bytes: None,
                code_function: None,
                code_filepath: None,
                code_lineno: None,
                code_namespace: None,
                instrumentation_scopes: Vec::new(),
            },
            template: self.template.as_str().into(),
            params: vec![],
        }
    }
}

/// Safety ceiling on the serialized spans [`embed_finding_traces`] adds
/// to a report. Not a size target, the HTML sink owns that at render
/// time: this exists because every downstream reader bounds what it
/// accepts, and the tightest known consumer caps the whole JSON at
/// 256 MiB when it reads it off the producer's stdout. Sized under that
/// with room for the findings themselves, and far above any realistic
/// query window.
const EMBED_SAFETY_BUDGET_BYTES: usize = 192 * 1024 * 1024;

/// Rank each trace id by the first finding that references it. The one
/// definition of the selection rule: the embed budget below and both of
/// the HTML sink's selection paths order candidate traces with it, so
/// the trees kept are always the ones the top findings point at.
pub(crate) fn first_reference_rank(
    findings: &[crate::detect::Finding],
) -> std::collections::HashMap<&str, usize> {
    let mut rank: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, f) in findings.iter().enumerate() {
        rank.entry(f.trace_id.as_str()).or_insert(i);
    }
    rank
}

/// `io::Write` sink that counts bytes. Measuring a trace by serializing
/// into it costs the serialization but not the throwaway `String` the
/// obvious `to_string` would allocate per trace.
struct ByteCounter(usize);

impl std::io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Carry masked spans for the traces the report's findings point at.
///
/// For the backend-query subcommands, whose JSON is rendered later by
/// `report --input` and so travels without its input. The HTML sink
/// applies the real size target at render time, this only refuses to
/// write a file the readers would refuse to open. When the ceiling
/// bites, the traces kept are the ones the first findings point at, the
/// same rule the sink uses, and the drop is logged rather than silent.
/// Sorted by trace id because `correlate` returns `HashMap` order and
/// `--format json` must stay stable.
pub fn embed_finding_traces(report: &mut super::Report, traces: &[Trace]) {
    embed_finding_traces_with_budget(report, traces, EMBED_SAFETY_BUDGET_BYTES);
}

fn embed_finding_traces_with_budget(report: &mut super::Report, traces: &[Trace], budget: usize) {
    let rank_by_trace = first_reference_rank(&report.findings);
    let mut ranked: Vec<(usize, &Trace)> = traces
        .iter()
        .filter_map(|t| rank_by_trace.get(t.trace_id.as_str()).map(|r| (*r, t)))
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);

    let total = ranked.len();
    let mut spent = 0usize;
    let mut embedded: Vec<EmbeddedTrace> = ranked
        .into_iter()
        .map(|(_, t)| EmbeddedTrace::from_trace(t))
        .take_while(|t| {
            let mut counter = ByteCounter(0);
            let size = serde_json::to_writer(&mut counter, t).map_or(usize::MAX, |()| counter.0);
            spent = spent.saturating_add(size);
            spent <= budget
        })
        .collect();
    if embedded.len() < total {
        tracing::warn!(
            kept = embedded.len(),
            total,
            budget_bytes = budget,
            "embedded span trees hit the safety ceiling, the traces of the \
             lowest-ranked findings travel without one"
        );
    }
    embedded.sort_by(|a, b| a.trace_id.cmp(&b.trace_id));
    report.embedded_traces = embedded;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventSource, SpanEvent};

    fn event(span_id: &str, target: &str, template: &str) -> NormalizedEvent {
        NormalizedEvent {
            event: SpanEvent {
                timestamp: "2025-07-10T14:32:01.123Z".to_string(),
                trace_id: "t1".to_string(),
                span_id: span_id.to_string(),
                parent_span_id: None,
                link_trace_id: None,
                service: "svc".into(),
                grouping: Vec::new(),
                cloud_region: None,
                event_type: EventType::Sql,
                operation: "SELECT".to_string(),
                target: target.to_string(),
                duration_us: 1_200,
                source: EventSource {
                    endpoint: "GET /api/x".to_string(),
                    method: "GET".to_string(),
                },
                status_code: None,
                response_size_bytes: None,
                code_function: None,
                code_filepath: None,
                code_lineno: None,
                code_namespace: None,
                instrumentation_scopes: Vec::new(),
            },
            template: template.into(),
            params: vec![],
        }
    }

    #[test]
    fn masks_the_raw_target() {
        let raw = "select * from users where email = 'a@b.c'";
        let span =
            EmbeddedSpan::from_event(&event("s1", raw, "select * from users where email = ?"));
        let json = serde_json::to_string(&span).expect("serializes");
        assert!(!json.contains("a@b.c"), "raw literals must not be embedded");
        assert!(json.contains("email = ?"), "the template is what travels");
    }

    #[test]
    fn event_type_serializes_snake_case() {
        let span = EmbeddedSpan::from_event(&event("s1", "x", "x"));
        let json = serde_json::to_string(&span).expect("serializes");
        assert!(json.contains(r#""event_type":"sql""#), "got {json}");
    }

    #[test]
    fn to_trace_rebuilds_masked_spans() {
        let raw = "select * from users where email = 'a@b.c'";
        let embedded = EmbeddedTrace::from_trace(&Trace {
            trace_id: "t1".to_string(),
            spans: vec![event("s1", raw, "select * from users where email = ?")],
        });
        let back = embedded.to_trace();
        assert_eq!(back.trace_id, "t1");
        assert_eq!(back.spans.len(), 1);
        let span = &back.spans[0];
        assert_eq!(span.event.span_id, "s1");
        assert_eq!(span.event.timestamp, "2025-07-10T14:32:01.123Z");
        assert_eq!(span.event.duration_us, 1_200);
        assert_eq!(
            span.event.target, "select * from users where email = ?",
            "the template stands in for the never-embedded raw target"
        );
        assert_eq!(
            span.template.as_ref(),
            "select * from users where email = ?"
        );
    }

    #[test]
    fn embedded_span_without_timestamp_remains_backward_compatible() {
        let span = EmbeddedSpan::from_event(&event("s1", "raw", "tpl"));
        let mut value = serde_json::to_value(span).unwrap();
        value.as_object_mut().unwrap().remove("timestamp");

        let back: EmbeddedSpan = serde_json::from_value(value).unwrap();

        assert!(back.timestamp.is_empty());
    }

    #[test]
    fn only_the_traces_findings_point_at_are_embedded_and_they_are_sorted() {
        use crate::detect::{FindingType, Severity};
        use crate::test_helpers::{empty_report, make_finding};

        let mut report = empty_report();
        for id in ["b", "a"] {
            let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Critical);
            finding.trace_id = id.to_string();
            report.findings.push(finding);
        }
        let traces: Vec<Trace> = ["c", "b", "a"]
            .iter()
            .map(|id| Trace {
                trace_id: (*id).to_string(),
                spans: vec![event("s1", "raw", "tpl")],
            })
            .collect();

        embed_finding_traces(&mut report, &traces);

        let ids: Vec<&str> = report
            .embedded_traces
            .iter()
            .map(|t| t.trace_id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn the_safety_ceiling_keeps_the_top_findings_traces_and_drops_the_tail() {
        use crate::detect::{FindingType, Severity};
        use crate::test_helpers::{empty_report, make_finding};

        let mut report = empty_report();
        for id in ["big", "small"] {
            let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Critical);
            finding.trace_id = id.to_string();
            report.findings.push(finding);
        }
        let traces: Vec<Trace> = [("big", 40), ("small", 1)]
            .iter()
            .map(|(id, n)| Trace {
                trace_id: (*id).to_string(),
                spans: (0..*n)
                    .map(|i| event(&format!("s{i}"), "raw", "tpl"))
                    .collect(),
            })
            .collect();
        let one_big = serde_json::to_string(&EmbeddedTrace::from_trace(&traces[0]))
            .expect("serializes")
            .len();

        // Budget fits the first finding's trace and nothing more: the tail
        // is dropped, never the head, whatever their relative sizes.
        embed_finding_traces_with_budget(&mut report, &traces, one_big);

        let ids: Vec<&str> = report
            .embedded_traces
            .iter()
            .map(|t| t.trace_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["big"],
            "the first finding's trace survives the ceiling"
        );
    }

    #[test]
    fn trace_roundtrips() {
        let trace = Trace {
            trace_id: "t1".to_string(),
            spans: vec![event("s1", "raw", "tpl")],
        };
        let embedded = EmbeddedTrace::from_trace(&trace);
        let json = serde_json::to_string(&embedded).expect("serializes");
        let back: EmbeddedTrace = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, embedded);
    }

    fn finding_at(trace_id: &str) -> crate::detect::Finding {
        use crate::detect::{FindingType, Severity};
        let mut finding =
            crate::test_helpers::make_finding(FindingType::NPlusOneSql, Severity::Critical);
        finding.trace_id = trace_id.to_string();
        finding
    }

    /// A report whose findings point at `trace_ids`, in that order.
    fn report_for(trace_ids: &[&str]) -> crate::report::Report {
        let mut report = crate::test_helpers::empty_report();
        for id in trace_ids {
            report.findings.push(finding_at(id));
        }
        report
    }

    fn trace_of(trace_id: &str, spans: usize) -> Trace {
        Trace {
            trace_id: trace_id.to_string(),
            spans: (0..spans)
                .map(|i| event(&format!("s{i}"), "raw", "tpl"))
                .collect(),
        }
    }

    fn embedded_size(trace: &Trace) -> usize {
        serde_json::to_string(&EmbeddedTrace::from_trace(trace))
            .expect("serializes")
            .len()
    }

    fn embedded_ids(report: &crate::report::Report) -> Vec<&str> {
        report
            .embedded_traces
            .iter()
            .map(|t| t.trace_id.as_str())
            .collect()
    }

    #[test]
    fn first_reference_rank_keeps_the_earliest_index_of_a_repeated_trace() {
        let report = report_for(&["a", "b", "a", "c"]);

        let rank = first_reference_rank(&report.findings);

        assert_eq!(rank.get("a"), Some(&0), "the later reference must not win");
        assert_eq!(rank.get("b"), Some(&1));
        assert_eq!(rank.get("c"), Some(&3));
        assert_eq!(rank.len(), 3, "one rank per distinct trace id");
    }

    #[test]
    fn the_kept_trees_are_the_ones_the_top_findings_reference() {
        // Findings deliberately out of trace-id order: the rule follows the
        // findings, not the ids.
        let mut report = report_for(&["t3", "t1", "t4", "t2", "t5"]);
        let traces: Vec<Trace> = ["t5", "t4", "t3", "t2", "t1"]
            .iter()
            .map(|id| trace_of(id, 3))
            .collect();
        let budget = 2 * embedded_size(&traces[0]);

        embed_finding_traces_with_budget(&mut report, &traces, budget);

        assert_eq!(
            embedded_ids(&report),
            ["t1", "t3"],
            "findings 1 and 2 point at t3 and t1, output stays sorted by id"
        );
    }

    #[test]
    fn a_trace_referenced_by_the_first_finding_outranks_a_bigger_one_referenced_by_the_fifth() {
        let mut report = report_for(&["early", "gone", "gone", "gone", "late"]);
        let traces = vec![trace_of("late", 40), trace_of("early", 1)];
        // Room for the big trace alone: rank, not size, decides who gets it.
        let budget = embedded_size(&traces[0]);

        embed_finding_traces_with_budget(&mut report, &traces, budget);

        assert_eq!(
            embedded_ids(&report),
            ["early"],
            "first reference ranks, the candidate order and the sizes do not"
        );
    }

    #[test]
    fn a_finding_whose_trace_is_not_a_candidate_is_skipped_without_spending_the_budget() {
        let traces = vec![trace_of("kept", 3)];
        let mut report = report_for(&["ghost", "kept"]);

        // Budget for exactly one trace: the absent top-ranked one must not
        // take the slot.
        embed_finding_traces_with_budget(&mut report, &traces, embedded_size(&traces[0]));

        assert_eq!(embedded_ids(&report), ["kept"]);

        let mut only_ghosts = report_for(&["ghost"]);
        embed_finding_traces(&mut only_ghosts, &traces);
        assert!(
            only_ghosts.embedded_traces.is_empty(),
            "no finding points at a candidate"
        );
    }

    #[test]
    fn the_cap_is_inclusive_at_the_boundary_and_one_byte_short_drops_the_last_trace() {
        let traces = vec![trace_of("t1", 4), trace_of("t2", 4)];
        let pair = embedded_size(&traces[0]) + embedded_size(&traces[1]);

        let mut report = report_for(&["t1", "t2"]);
        embed_finding_traces_with_budget(&mut report, &traces, pair);
        assert_eq!(
            embedded_ids(&report),
            ["t1", "t2"],
            "spending exactly the budget still fits"
        );

        let mut report = report_for(&["t1", "t2"]);
        embed_finding_traces_with_budget(&mut report, &traces, pair - 1);
        assert_eq!(
            embedded_ids(&report),
            ["t1"],
            "one byte short drops the lowest-ranked trace"
        );
    }
}
