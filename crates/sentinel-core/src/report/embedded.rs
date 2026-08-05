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
/// `#[non_exhaustive]` so an added field (a span timestamp is the
/// obvious next one) stays a minor bump for downstream crates.
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

    /// The masked counterpart of [`Self::from_event`]: same shape, the
    /// template in place of the raw target.
    fn to_normalized(&self) -> NormalizedEvent {
        NormalizedEvent {
            event: crate::event::SpanEvent {
                timestamp: String::new(),
                trace_id: String::new(),
                span_id: self.span_id.clone(),
                parent_span_id: self.parent_span_id.clone(),
                link_trace_id: None,
                service: self.service.as_str().into(),
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
