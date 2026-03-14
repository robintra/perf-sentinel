//! Detection stage: identifies performance anti-patterns in traces.

use crate::correlate::Trace;

/// A detected performance anti-pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub trace_id: String,
    pub pattern: PatternType,
    pub description: String,
    pub span_count: usize,
}

/// Types of performance anti-patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternType {
    NPlusOneSql,
    NPlusOneHttp,
    RedundantQuery,
}

/// Run all detectors on a set of traces (stub).
pub fn detect(_traces: &[Trace], _threshold: u32) -> Vec<Detection> {
    // TODO: implement N+1 SQL, N+1 HTTP, redundant query detection
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_returns_no_detections() {
        let detections = detect(&[], 5);
        assert!(detections.is_empty());
    }
}
