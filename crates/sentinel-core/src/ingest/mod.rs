//! Ingestion stage: reads raw events from various sources.

pub mod json;

use crate::event::SpanEvent;

/// Trait for event ingestion sources.
pub trait IngestSource {
    /// Error type for this source.
    type Error: std::error::Error;

    /// Ingest events from the source and return them.
    fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error>;
}
