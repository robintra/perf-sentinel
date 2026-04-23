//! Ingestion stage: reads raw events from various sources.

#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
pub mod auth_header;
pub mod jaeger;
#[cfg(feature = "jaeger-query")]
pub mod jaeger_query;
pub mod json;
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
pub mod lookback;
pub mod otlp;
pub mod pg_stat;
#[cfg(feature = "tempo")]
pub mod tempo;
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
pub(crate) mod url_enc;
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
