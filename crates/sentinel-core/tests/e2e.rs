//! End-to-end tests for perf-sentinel.

use sentinel_core::config::Config;
use sentinel_core::pipeline;

#[test]
fn empty_input_produces_empty_report() {
    let config = Config::default();
    let report = pipeline::analyze(vec![], &config);
    assert!(report.detections.is_empty());
    assert!(report.scores.is_empty());
}
