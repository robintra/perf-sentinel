//! Core event types for the perf-sentinel pipeline.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single span event representing an I/O operation (SQL query, HTTP call, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanEvent {
    /// Unique identifier for this span.
    pub span_id: String,
    /// Trace identifier grouping related spans.
    pub trace_id: String,
    /// Optional parent span identifier.
    pub parent_span_id: Option<String>,
    /// Name of the service that emitted this span.
    pub service_name: String,
    /// Type of operation (e.g., "sql", "http").
    pub operation_type: OperationType,
    /// The operation content (SQL query text, HTTP URL, etc.).
    pub operation: String,
    /// HTTP method if applicable.
    pub http_method: Option<String>,
    /// HTTP status code if applicable.
    pub http_status_code: Option<u16>,
    /// Start time as Unix timestamp in microseconds.
    pub start_time_us: u64,
    /// Duration in microseconds.
    pub duration_us: u64,
    /// Additional metadata.
    #[serde(default)]
    pub attributes: HashMap<String, String>,
}

/// The type of I/O operation a span represents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationType {
    Sql,
    Http,
    Grpc,
    Other(String),
}
