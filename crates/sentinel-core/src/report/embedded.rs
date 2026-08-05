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
}

/// A single span of an [`EmbeddedTrace`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddedSpan {
    pub span_id: String,
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
}
