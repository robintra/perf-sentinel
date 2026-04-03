//! Ingestion stage: reads raw events from various sources.

pub mod jaeger;
pub mod json;
pub mod otlp;
pub mod zipkin;

use crate::event::SpanEvent;

/// Trait for event ingestion sources.
pub trait IngestSource {
    /// Error type for this source.
    type Error: std::error::Error;

    /// Ingest events from the source and return them.
    ///
    /// # Errors
    ///
    /// Returns an error if the raw input cannot be parsed or exceeds size limits.
    fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error>;
}
