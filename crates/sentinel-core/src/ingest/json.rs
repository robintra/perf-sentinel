//! JSON ingestion source (stub).

use crate::event::SpanEvent;
use crate::ingest::IngestSource;

/// Ingests span events from JSON input.
pub struct JsonIngest {
    max_size: usize,
}

impl JsonIngest {
    pub fn new(max_size: usize) -> Self {
        Self { max_size }
    }
}

impl IngestSource for JsonIngest {
    type Error = JsonIngestError;

    fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error> {
        if raw.len() > self.max_size {
            return Err(JsonIngestError::PayloadTooLarge {
                size: raw.len(),
                max: self.max_size,
            });
        }
        let events: Vec<SpanEvent> = serde_json::from_slice(raw).map_err(JsonIngestError::Parse)?;
        Ok(events)
    }
}

/// Errors that can occur during JSON ingestion.
#[derive(Debug, thiserror::Error)]
pub enum JsonIngestError {
    #[error("payload too large: {size} bytes exceeds maximum of {max} bytes")]
    PayloadTooLarge { size: usize, max: usize },
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_payload() {
        let ingest = JsonIngest::new(10);
        let result = ingest.ingest(&[0u8; 100]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_empty_array() {
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(b"[]").unwrap();
        assert!(events.is_empty());
    }
}
