//! Correlation stage: groups normalized events by trace ID into traces.

pub mod window;

use crate::normalize::NormalizedEvent;
use std::collections::HashMap;

/// A correlated trace containing all normalized events sharing the same trace ID.
#[derive(Debug, Clone)]
pub struct Trace {
    pub trace_id: String,
    pub spans: Vec<NormalizedEvent>,
}

/// Group normalized events into traces by `trace_id`.
///
/// Traces come out in the order their first span appears in `events`, so
/// every order-sensitive consumer (floating-point sums, first-seen picks,
/// tie-breaks) reproduces from one run to the next.
#[must_use]
pub fn correlate(events: Vec<NormalizedEvent>) -> Vec<Trace> {
    let estimated_traces = (events.len() / 10).max(events.len().min(1));
    let mut index: HashMap<String, usize> = HashMap::with_capacity(estimated_traces);
    let mut traces: Vec<Trace> = Vec::with_capacity(estimated_traces);
    for event in events {
        if let Some(&i) = index.get(event.event.trace_id.as_str()) {
            traces[i].spans.push(event);
        } else {
            index.insert(event.event.trace_id.clone(), traces.len());
            traces.push(Trace {
                trace_id: event.event.trace_id.clone(),
                spans: vec![event],
            });
        }
    }
    traces
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::event::{EventSource, EventType, SpanEvent};
    use crate::normalize;

    fn make_event(trace_id: &str, span_id: &str) -> SpanEvent {
        SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: span_id.to_string(),
            parent_span_id: None,
            link_trace_id: None,
            service: Arc::from("test"),
            grouping: Vec::new(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: "SELECT 1".to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
            instrumentation_scopes: Vec::new(),
        }
    }

    #[test]
    fn empty_input_gives_empty_output() {
        let traces = correlate(vec![]);
        assert!(traces.is_empty());
    }

    #[test]
    fn groups_spans_by_trace_id() {
        let events = vec![
            make_event("trace-1", "span-1"),
            make_event("trace-2", "span-2"),
            make_event("trace-1", "span-3"),
        ];
        let normalized = normalize::normalize_all(events);
        let traces = correlate(normalized);
        assert_eq!(traces.len(), 2);

        let t1 = traces.iter().find(|t| t.trace_id == "trace-1").unwrap();
        assert_eq!(t1.spans.len(), 2);

        let t2 = traces.iter().find(|t| t.trace_id == "trace-2").unwrap();
        assert_eq!(t2.spans.len(), 1);
    }

    /// Every run draws fresh hash keys, so an order leaked from a map would
    /// differ across these runs.
    #[test]
    fn traces_follow_first_appearance_in_the_input() {
        let ids = ["t3", "t1", "t3", "t5", "t2", "t1", "t4"];
        for _ in 0..32 {
            let events = ids
                .iter()
                .enumerate()
                .map(|(i, id)| make_event(id, &format!("span-{i}")))
                .collect();
            let traces = correlate(normalize::normalize_all(events));
            let order: Vec<_> = traces.iter().map(|t| t.trace_id.as_str()).collect();
            assert_eq!(order, ["t3", "t1", "t5", "t2", "t4"]);
            let t1_spans: Vec<_> = traces[1]
                .spans
                .iter()
                .map(|s| s.event.span_id.as_str())
                .collect();
            assert_eq!(t1_spans, ["span-1", "span-5"]);
        }
    }
}
