//! Scoring stage: computes `GreenOps` I/O intensity scores.

use crate::correlate::Trace;
use crate::detect::Finding;

/// `GreenOps` score for an endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct GreenScore {
    /// Endpoint identifier (service + path).
    pub endpoint: String,
    /// I/O Intensity Score: total I/O ops for endpoint / invocations of endpoint.
    pub io_intensity: f64,
    /// Waste ratio: avoidable I/O ops (from findings) / total I/O ops.
    pub waste_ratio: f64,
}

/// Compute `GreenOps` scores for a set of traces and their findings (stub).
#[must_use]
pub fn score(_traces: &[Trace], _findings: &[Finding]) -> Vec<GreenScore> {
    // TODO: implement IIS and waste ratio computation
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_returns_no_scores() {
        let scores = score(&[], &[]);
        assert!(scores.is_empty());
    }
}
