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
#[must_use]
pub fn correlate(events: Vec<NormalizedEvent>) -> Vec<Trace> {
    // Heuristic: ~10 events per trace on average. Caps at the input
    // size to avoid over-allocating for single-trace workloads (explain mode).
    let estimated_traces = (events.len() / 10).max(1).min(events.len());
    let mut map: HashMap<String, Vec<NormalizedEvent>> = HashMap::with_capacity(estimated_traces);
    for event in events {
        if let Some(vec) = map.get_mut(event.event.trace_id.as_str()) {
            vec.push(event);
        } else {
            let key = event.event.trace_id.clone();
            map.insert(key, vec![event]);
        }
    }
    map.into_iter()
        .map(|(trace_id, spans)| Trace { trace_id, spans })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventSource, EventType, SpanEvent};
    use crate::normalize;

    fn make_event(trace_id: &str, span_id: &str) -> SpanEvent {
        SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: span_id.to_string(),
            parent_span_id: None,
            service: "test".to_string(),
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
}
