//! Pipeline: wires all stages together.

use crate::config::Config;
use crate::correlate;
use crate::detect;
use crate::event::SpanEvent;
use crate::report::Report;
use crate::score;

/// Run the full analysis pipeline on a batch of events.
pub fn analyze(events: Vec<SpanEvent>, config: &Config) -> Report {
    let traces = correlate::correlate(events);
    let detections = detect::detect(&traces, config.n_plus_one_threshold);
    let scores = score::score(&traces, &detections);
    Report { detections, scores }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pipeline_produces_empty_report() {
        let config = Config::default();
        let report = analyze(vec![], &config);
        assert!(report.detections.is_empty());
        assert!(report.scores.is_empty());
    }
}
