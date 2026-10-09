use super::*;

fn make_finding(service: &str, finding_type: FindingType, template: &str) -> Finding {
    Finding {
        finding_type,
        severity: crate::detect::Severity::Warning,
        trace_id: format!("trace-{service}"),
        service: service.to_string(),
        grouping: Vec::new(),
        source_endpoint: "POST /api/test".to_string(),
        pattern: crate::detect::Pattern {
            template: template.to_string(),
            occurrences: 5,
            window_ms: 200,
            distinct_params: 5,
            ..Default::default()
        },
        suggestion: "batch".to_string(),
        first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
        last_timestamp: "2025-07-10T14:32:01.200Z".to_string(),
        green_impact: None,
        confidence: crate::detect::Confidence::default(),
        classification_method: None,
        code_location: None,
        instrumentation_scopes: Vec::new(),
        suggested_fix: None,
        signature: String::new(),
    }
}

/// Stamp `f` with event time `t`.
fn at(mut f: Finding, t: u64) -> Finding {
    f.first_timestamp = crate::time::millis_to_iso8601(t);
    f
}

/// Ingest `findings` at `t` with event time = ingest time.
fn ingest_at(correlator: &mut CrossTraceCorrelator, findings: &[Finding], t: u64) -> usize {
    let stamped: Vec<Finding> = findings.iter().cloned().map(|f| at(f, t)).collect();
    correlator.ingest(&stamped, t)
}

/// A correlator with the permissive thresholds the cap/admission
/// tests share (long lag window, count 1, confidence 0), varying
/// only `max_tracked_pairs`.
fn capped_correlator(max_tracked_pairs: usize) -> CrossTraceCorrelator {
    CrossTraceCorrelator::new(CorrelationConfig {
        max_tracked_pairs,
        lag_threshold_ms: 100_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ..Default::default()
    })
}

/// `n` findings each from a distinct service and template, so every
/// cross-service pair is new (the wide-topology stress shape).
fn wide_batch(n: usize) -> Vec<Finding> {
    (0..n)
        .map(|i| {
            make_finding(
                &format!("svc-{i:03}"),
                FindingType::NPlusOneSql,
                &format!("tpl-{i:03}"),
            )
        })
        .collect()
}

#[test]
fn detects_simple_a_then_b_pattern() {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        min_co_occurrences: 2,
        min_confidence: 0.5,
        lag_threshold_ms: 5_000,
        ..Default::default()
    });

    // Simulate 5 occurrences of A followed by B within lag threshold.
    for i in 0..5 {
        let t = 1_000_000 + i * 10_000;
        let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT * FROM t");
        let _ = ingest_at(&mut correlator, &[fa], t);
        let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
        let _ = ingest_at(&mut correlator, &[fb], t + 2_000);
    }

    let correlations = correlator.active_correlations();
    assert!(
        !correlations.is_empty(),
        "expected at least one correlation"
    );
    let c = &correlations[0];
    assert_eq!(c.source.service, "order-svc");
    assert_eq!(c.target.service, "payment-svc");
    assert!(c.co_occurrence_count >= 2);
    assert!(c.confidence > 0.0);
    // `make_finding` sets trace_id to "trace-<service>", and the
    // target-side finding drives the trace id recorded on the
    // pair. Every B-ingest was keyed on payment-svc, so the
    // surfaced sample trace must match that.
    assert_eq!(
        c.sample_trace_id.as_deref(),
        Some("trace-payment-svc"),
        "correlator must record the latest target-side trace id on each pair"
    );
    assert_eq!(c.source_sample_trace_id.as_deref(), Some("trace-order-svc"));
}

#[test]
fn sample_trace_id_truncated_to_max_bytes() {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        min_co_occurrences: 1,
        min_confidence: 0.1,
        lag_threshold_ms: 5_000,
        ..Default::default()
    });

    // Build a finding with an oversized trace id. The correlator
    // must cap what it records so exported reports stay bounded.
    let oversized = "a".repeat(MAX_SAMPLE_TRACE_ID_BYTES * 4);
    let mut fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT 1");
    fa.trace_id = oversized.clone();
    let _ = ingest_at(&mut correlator, std::slice::from_ref(&fa), 1_000);
    let mut fb = make_finding("payment-svc", FindingType::PoolSaturation, "svc");
    fb.trace_id = oversized.clone();
    let _ = ingest_at(&mut correlator, &[fb], 2_000);
    // Second round so the pair clears the min_co_occurrences floor.
    let _ = ingest_at(&mut correlator, &[fa], 3_000);
    let mut fb2 = make_finding("payment-svc", FindingType::PoolSaturation, "svc");
    fb2.trace_id = oversized;
    let _ = ingest_at(&mut correlator, &[fb2], 4_000);

    let correlations = correlator.active_correlations();
    let c = correlations.first().expect("expected one correlation");
    let id = c.sample_trace_id.as_deref().expect("sample trace id set");
    assert!(
        id.len() <= MAX_SAMPLE_TRACE_ID_BYTES,
        "sample_trace_id must be truncated to {} bytes, got {}",
        MAX_SAMPLE_TRACE_ID_BYTES,
        id.len()
    );
    let source_id = c
        .source_sample_trace_id
        .as_deref()
        .expect("source sample trace id set");
    assert_eq!(source_id.len(), MAX_SAMPLE_TRACE_ID_BYTES);
}

#[test]
fn same_service_not_correlated() {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        min_co_occurrences: 2,
        min_confidence: 0.1,
        ..Default::default()
    });

    // Findings from the same service should not be correlated.
    for i in 0..5 {
        let t = 1_000_000 + i * 10_000;
        let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT * FROM t");
        let fb = make_finding("order-svc", FindingType::RedundantSql, "SELECT * FROM t");
        let _ = ingest_at(&mut correlator, &[fa, fb], t);
    }

    let correlations = correlator.active_correlations();
    assert!(
        correlations.is_empty(),
        "same-service findings should not be correlated"
    );
}

#[test]
fn eviction_removes_stale_entries() {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        window_ms: 10_000,
        min_co_occurrences: 1,
        min_confidence: 0.1,
        ..Default::default()
    });

    let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT 1");
    let _ = ingest_at(&mut correlator, &[fa], 1_000);
    let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
    let _ = ingest_at(&mut correlator, &[fb], 2_000);

    // After window expires, occurrences are evicted.
    let fa2 = make_finding("other-svc", FindingType::SlowSql, "SELECT 2");
    let _ = ingest_at(&mut correlator, &[fa2], 100_000);

    assert!(
        correlator.occurrences.len() <= 2,
        "stale entries should be evicted"
    );
}

#[test]
fn max_tracked_pairs_enforced() {
    let mut correlator = capped_correlator(5);

    // Create many distinct pairs, summing the reported evictions.
    let mut evicted_total = 0;
    for i in 0..20 {
        let fa = make_finding(
            &format!("svc-a-{i}"),
            FindingType::NPlusOneSql,
            &format!("tpl-{i}"),
        );
        evicted_total += ingest_at(&mut correlator, &[fa], 1000);
        let fb = make_finding(
            &format!("svc-b-{i}"),
            FindingType::RedundantSql,
            &format!("tpl-{i}"),
        );
        evicted_total += ingest_at(&mut correlator, &[fb], 1001);
    }

    assert!(
        correlator.pair_counts.len() <= 5,
        "pair count should be capped at max_tracked_pairs"
    );
    assert!(
        evicted_total > 0,
        "cap trips should report the evicted pair count"
    );
}

#[test]
fn wide_topology_single_batch_stays_at_cap() {
    // One batch of findings from many distinct services must not
    // insert every cross-service pair before the batch-end eviction
    // runs, which would explode the map (and the process RSS) inside
    // a single ingest call. Admission control bounds it at the cap.
    let mut correlator = capped_correlator(50);
    let findings = wide_batch(200);

    let lost = ingest_at(&mut correlator, &findings, 1_000);

    assert!(
        correlator.pair_counts.len() <= 50,
        "pair map must never exceed the cap inside one batch, got {}",
        correlator.pair_counts.len()
    );
    assert!(
        lost > 0,
        "pairs lost to the cap must be reported for the eviction counter"
    );
}

#[test]
fn admission_pressure_frees_room_for_the_next_batch() {
    // Refused newcomers must trigger a batch-end eviction (lowest
    // co-occurrence first) so the next batch admits new pairs,
    // instead of early-window noise squatting the map until TTL.
    let mut correlator = capped_correlator(50);
    let batch = wide_batch(200);
    let lost = ingest_at(&mut correlator, &batch, 1_000);
    assert!(lost > 0, "the wide batch must hit the cap");
    assert!(
        correlator.pair_counts.len() <= 45,
        "batch-end eviction must leave headroom below the cap, got {}",
        correlator.pair_counts.len()
    );

    // Past the lag threshold so the fresh pair only matches itself,
    // not the 200 occurrences still in the window.
    let before = correlator.pair_counts.len();
    let fa = make_finding("svc-new-a", FindingType::NPlusOneSql, "tpl-new");
    let fb = make_finding("svc-new-b", FindingType::RedundantSql, "tpl-new");
    assert_eq!(ingest_at(&mut correlator, &[fa], 200_000), 0);
    assert_eq!(ingest_at(&mut correlator, &[fb], 200_001), 0);
    assert!(
        correlator.pair_counts.len() > before,
        "a fresh pair must be admitted after the eviction freed room"
    );
}

#[test]
fn cap_zero_refuses_everything_without_panicking() {
    // max_tracked_pairs = 0 passes config validation: every pair is
    // refused, the map stays empty, and the batch-end eviction must
    // not panic on the empty selection.
    let mut correlator = capped_correlator(0);
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    assert_eq!(ingest_at(&mut correlator, &[fa], 1_000), 0);
    assert_eq!(
        ingest_at(&mut correlator, &[fb], 1_001),
        1,
        "one distinct pair refused"
    );
    assert!(correlator.pair_counts.is_empty());
}

#[test]
fn refused_pairs_count_distinct_keys_not_occurrences() {
    // One source endpoint with several occurrences inside the lag
    // window must count a refused pair once per batch, not once per
    // matching occurrence.
    let mut correlator = capped_correlator(0);
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    for i in 0..5 {
        assert_eq!(
            ingest_at(&mut correlator, std::slice::from_ref(&fa), 1_000 + i),
            0
        );
    }
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    assert_eq!(
        ingest_at(&mut correlator, &[fb], 1_010),
        1,
        "five matching occurrences of the same refused pair must count once"
    );
}

#[test]
fn refused_pairs_stops_collecting_at_the_ceiling_but_keeps_counting() {
    // A wide topology walks the cross product of the batch and the
    // horizon, so the set has to stop growing while the figure it
    // feeds still counts every refused pair.
    let mut refused = RefusedPairs::default();
    let extra = 500;
    // Distinct keys only up to the ceiling, then the same key again,
    // to cover both the dedup below the ceiling and the documented
    // degradation above it.
    let key_for = |i: usize| PairKey {
        source: Arc::new(CorrelationEndpoint {
            finding_type: FindingType::NPlusOneSql,
            service: format!("svc-{i}"),
            template: "tpl".to_string(),
            grouping_key: None,
            grouping_value: None,
        }),
        target: Arc::new(CorrelationEndpoint {
            finding_type: FindingType::RedundantSql,
            service: "target".to_string(),
            template: "tpl".to_string(),
            grouping_key: None,
            grouping_value: None,
        }),
    };
    // The same pair twice, well under the ceiling: counted once.
    refused.record(key_for(0));
    refused.record(key_for(0));
    assert_eq!(
        refused.total(),
        1,
        "a repeated pair counts once below the ceiling"
    );

    let mut refused = RefusedPairs::default();
    for i in 0..(RefusedPairs::CEILING + extra) {
        refused.record(key_for(i));
    }

    assert_eq!(
        refused.seen.len(),
        RefusedPairs::CEILING,
        "the set must stop growing at the ceiling"
    );
    assert_eq!(
        refused.total(),
        RefusedPairs::CEILING + extra,
        "every refusal must still reach the counter"
    );

    // Past the ceiling the dedup is gone, which the doc calls
    // overstating rather than hiding: a key already in the set
    // counts again.
    let before = refused.total();
    refused.record(key_for(0));
    assert_eq!(
        refused.total(),
        before + 1,
        "above the ceiling a known pair counts again"
    );
}

#[test]
fn confidence_never_exceeds_one_when_targets_outnumber_sources() {
    // One source occurrence followed by several targets inside the
    // lag window. Scoring one co-occurrence per (source, target)
    // couple would push confidence past 100% on this shape ("conf 150%"
    // in a live report).
    let mut correlator = capped_correlator(CorrelationConfig::default().max_tracked_pairs);
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    for i in 0..2 {
        assert_eq!(
            ingest_at(&mut correlator, std::slice::from_ref(&fa), 1_000 + i),
            0
        );
    }
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    for i in 0..3 {
        assert_eq!(
            ingest_at(&mut correlator, std::slice::from_ref(&fb), 1_010 + i),
            0
        );
    }

    let correlations = correlator.active_correlations();
    let pair = correlations
        .iter()
        .find(|c| c.source.service == "svc-a" && c.target.service == "svc-b")
        .expect("svc-a -> svc-b pair");
    assert_eq!(
        pair.co_occurrence_count, 2,
        "each source occurrence counts once, not once per following target"
    );
    assert_eq!(pair.source_total_occurrences, 2);
    assert!(
        pair.confidence <= 1.0,
        "confidence must stay in [0, 1], got {}",
        pair.confidence
    );
}

#[test]
fn long_lived_pair_confidence_does_not_saturate_at_one() {
    // Lifetime counting would make every mature pair report exactly
    // 1.0: the count grows forever while the denominator only spans
    // the window. With window-scoped counts, a pair co-occurring on
    // half its source occurrences stays near 0.5 however long it lives.
    let window_ms = CorrelationConfig::default().window_ms;
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        lag_threshold_ms: 1_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ..Default::default()
    });
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    // Two windows' worth of rounds. Per round, one source inside the
    // target's lag window and one far outside it: half the sources
    // co-occur.
    let round_gap = window_ms / 4;
    for round in 0..8u64 {
        let t = 1_000 + round * round_gap;
        assert_eq!(ingest_at(&mut correlator, std::slice::from_ref(&fa), t), 0);
        assert_eq!(
            ingest_at(&mut correlator, std::slice::from_ref(&fb), t + 20),
            0
        );
        assert_eq!(
            ingest_at(&mut correlator, std::slice::from_ref(&fa), t + 5_000),
            0
        );
    }
    let correlations = correlator.active_correlations();
    let pair = correlations
        .iter()
        .find(|c| c.source.service == "svc-a")
        .expect("pair");
    assert!(
        pair.confidence < 0.9,
        "a pair co-occurring on half its sources must not read as certain, got {}",
        pair.confidence
    );
}

#[test]
fn quiesced_pair_decays_while_unrelated_traffic_continues() {
    // One window after its last co-occurrence a quiet pair is gone, however busy the daemon.
    let window_ms = CorrelationConfig::default().window_ms;
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        lag_threshold_ms: 1_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ..Default::default()
    });
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    assert_eq!(
        ingest_at(&mut correlator, std::slice::from_ref(&fa), 1_000),
        0
    );
    assert_eq!(
        ingest_at(&mut correlator, std::slice::from_ref(&fb), 1_020),
        0
    );
    assert_eq!(correlator.active_correlations().len(), 1);

    // The pair goes quiet, unrelated services keep the daemon busy.
    let fc = make_finding("svc-c", FindingType::SlowSql, "other");
    let t = 1_020 + window_ms + 1;
    assert_eq!(ingest_at(&mut correlator, std::slice::from_ref(&fc), t), 0);
    assert!(
        correlator.active_correlations().is_empty(),
        "a pair with no co-occurrence in the last window must not survive"
    );
}

#[test]
fn windowed_count_never_covers_more_than_one_window() {
    // The two-bucket sum spans at most one window.
    let window_ms = CorrelationConfig::default().window_ms;
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        lag_threshold_ms: 1_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ..Default::default()
    });
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    // One co-occurrence every 0.95 half-window: a true window holds
    // at most 3 of them.
    let interval = window_ms / 2 * 95 / 100;
    for round in 0..10u64 {
        let t = 1_000 + round * interval;
        assert_eq!(ingest_at(&mut correlator, std::slice::from_ref(&fa), t), 0);
        assert_eq!(
            ingest_at(&mut correlator, std::slice::from_ref(&fb), t + 20),
            0
        );
    }
    let correlations = correlator.active_correlations();
    let pair = correlations
        .iter()
        .find(|c| c.source.service == "svc-a")
        .expect("pair");
    assert!(
        pair.co_occurrence_count <= 3,
        "windowed count must not exceed one window's worth, got {}",
        pair.co_occurrence_count
    );
}

#[test]
fn sample_trace_id_tracks_the_most_recent_target() {
    // The once-per-source dedup must not freeze the sample on the first target.
    let mut correlator = capped_correlator(CorrelationConfig::default().max_tracked_pairs);
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    assert_eq!(
        ingest_at(&mut correlator, std::slice::from_ref(&fa), 1_000),
        0
    );
    let mut fb1 = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    fb1.trace_id = "trace-old".to_string();
    assert_eq!(
        ingest_at(&mut correlator, std::slice::from_ref(&fb1), 1_010),
        0
    );
    let mut fb2 = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    fb2.trace_id = "trace-new".to_string();
    assert_eq!(
        ingest_at(&mut correlator, std::slice::from_ref(&fb2), 1_020),
        0
    );

    let correlations = correlator.active_correlations();
    let pair = correlations
        .iter()
        .find(|c| c.source.service == "svc-a")
        .expect("pair");
    assert_eq!(pair.sample_trace_id.as_deref(), Some("trace-new"));
    assert_eq!(pair.source_sample_trace_id.as_deref(), Some("trace-svc-a"));
}

#[test]
fn ingest_under_cap_reports_zero_evictions() {
    let mut correlator = capped_correlator(CorrelationConfig::default().max_tracked_pairs);

    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    assert_eq!(ingest_at(&mut correlator, &[fa], 1_000), 0);
    assert_eq!(ingest_at(&mut correlator, &[fb], 1_001), 0);
}

#[test]
fn low_confidence_filtered_out() {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        min_co_occurrences: 1,
        min_confidence: 0.9,
        lag_threshold_ms: 5_000,
        ..Default::default()
    });

    // A occurs 10 times, B follows only 2 times.
    for i in 0..10 {
        let t = 1_000_000 + i * 10_000;
        let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT * FROM t");
        let _ = ingest_at(&mut correlator, &[fa], t);
        if i < 2 {
            let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
            let _ = ingest_at(&mut correlator, &[fb], t + 1_000);
        }
    }

    let correlations = correlator.active_correlations();
    assert!(
        correlations.is_empty(),
        "low confidence pairs should be filtered"
    );
}

#[test]
fn delay_exceeding_lag_threshold_not_counted() {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        lag_threshold_ms: 1_000,
        min_co_occurrences: 1,
        min_confidence: 0.1,
        ..Default::default()
    });

    // A at t=1000, B at t=10000 (9s later, exceeds 1s threshold).
    let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT 1");
    let _ = ingest_at(&mut correlator, &[fa], 1_000);
    let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
    let _ = ingest_at(&mut correlator, &[fb], 10_000);

    let correlations = correlator.active_correlations();
    assert!(
        correlations.is_empty(),
        "findings outside lag threshold should not be correlated"
    );
}

#[test]
fn lags_ms_bounded_by_reservoir_cap() {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        min_co_occurrences: 1,
        min_confidence: 0.1,
        lag_threshold_ms: 10_000,
        window_ms: 10_000_000,
        ..Default::default()
    });

    // Fire the same A-then-B pair 10x MAX_LAG_SAMPLES times.
    // Without the reservoir, lags_ms would grow to ~640 entries.
    let total = MAX_LAG_SAMPLES * 10;
    for i in 0..total {
        let t = 1_000_000 + i as u64 * 10;
        let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT 1");
        let _ = ingest_at(&mut correlator, &[fa], t);
        let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
        let _ = ingest_at(&mut correlator, &[fb], t + 1);
    }

    // Directional pairs: A to B, and B to the next round's A (10 ms
    // later). Both directions should have bounded reservoirs.
    assert!(
        !correlator.pair_counts.is_empty(),
        "expected at least one tracked pair"
    );
    for state in correlator.pair_counts.values() {
        assert!(
            state.lags_ms.len() <= MAX_LAG_SAMPLES,
            "lags_ms must be bounded: got {}",
            state.lags_ms.len()
        );
        // Hot pair total_observations should vastly exceed reservoir size.
        assert!(
            state.total_observations > MAX_LAG_SAMPLES as u64,
            "total_observations should track every hit, got {}",
            state.total_observations
        );
    }
}

#[test]
fn reservoir_continues_to_sample_after_many_observations() {
    // Guards against a draw like
    // `fnv1a(total_observations) % total_observations`, which freezes
    // the reservoir after a few thousand observations (deterministic
    // hash + modulo = biased index).
    //
    // Feeds the reservoir with monotonically increasing lag values
    // and checks two properties:
    //
    // 1. **Mean tracks the population mean** within 20%. For a
    //    population uniform on [0, n), the true mean is (n-1)/2.
    //    Reservoir-size-k sample mean has standard error
    //    sigma_pop / sqrt(k). With n=1280, k=64, sigma_pop ~= 370,
    //    the expected SE ~= 46, so 20% of 639.5 ~= 128 is ~2.8 sigma.
    //    Still generous enough to avoid flakes across different PRNG
    //    seeds.
    //
    // 2. **Variance is non-trivial**. A frozen reservoir would have
    //    all samples from the first MAX_LAG_SAMPLES values, giving
    //    a variance bounded by (MAX_LAG_SAMPLES/2)^2 ~= 1024. A
    //    healthy reservoir covers the full range so variance should
    //    be at least 1/4 of the population variance
    //    (pop_variance = n^2/12 for uniform on [0, n)).
    let mut state = PairState {
        co: HalfWindowCount::default(),
        lags_ms: Vec::new(),
        total_observations: 0,
        rng_state: 0x1234_5678_9ABC_DEF0,
        first_seen_ms: 0,
        last_seen_ms: 0,
        last_trace_id: None,
        last_source_trace_id: None,
    };
    let n = MAX_LAG_SAMPLES * 20;
    for i in 0..n {
        state.record_lag(i as f64);
    }
    let mean: f64 = state.lags_ms.iter().sum::<f64>() / state.lags_ms.len() as f64;
    let expected_mean = (n - 1) as f64 / 2.0;
    let tolerance = expected_mean * 0.20;
    assert!(
        (mean - expected_mean).abs() < tolerance,
        "reservoir mean {mean} should be within {tolerance} of {expected_mean} \
             (a frozen/biased reservoir would produce a much lower mean)"
    );

    // Variance check: a frozen reservoir covers only the first
    // MAX_LAG_SAMPLES samples, giving variance well below the
    // population variance n^2/12.
    let variance: f64 = state
        .lags_ms
        .iter()
        .map(|&x| (x - mean).powi(2))
        .sum::<f64>()
        / state.lags_ms.len() as f64;
    let pop_variance = (n as f64).powi(2) / 12.0;
    assert!(
        variance > pop_variance * 0.25,
        "reservoir variance {variance} should be at least 25% of population \
             variance {pop_variance}; a frozen reservoir would be orders of \
             magnitude below this"
    );
}

/// Permissive thresholds for the event-time tests: count 1,
/// confidence 0, 2 s lag.
fn event_time_correlator() -> CrossTraceCorrelator {
    CrossTraceCorrelator::new(CorrelationConfig {
        lag_threshold_ms: 2_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ..Default::default()
    })
}

fn find_pair<'a>(
    correlations: &'a [CrossTraceCorrelation],
    source: &str,
    target: &str,
) -> Option<&'a CrossTraceCorrelation> {
    correlations
        .iter()
        .find(|c| c.source.service == source && c.target.service == target)
}

#[test]
fn endpoints_pruned_after_window_and_horizon() {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        window_ms: 1_000,
        min_co_occurrences: 1,
        min_confidence: 0.1,
        ..Default::default()
    });
    let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT 1");
    let _ = ingest_at(&mut correlator, &[fa], 1_000);
    assert_eq!(correlator.endpoints.len(), 1);

    // Past both the horizon (lag + skew) and the window.
    let fb = make_finding("other-svc", FindingType::NPlusOneSql, "SELECT 2");
    let _ = ingest_at(&mut correlator, &[fb], 100_000);
    assert_eq!(
        correlator.endpoints.len(),
        1,
        "stale endpoint must be pruned"
    );
    assert!(
        correlator
            .endpoints
            .keys()
            .all(|ep| ep.service == "other-svc")
    );
}

#[test]
fn cross_tick_pairs_on_event_time() {
    let mut correlator = event_time_correlator();
    let t = 1_000_000;
    let fa = at(make_finding("svc-a", FindingType::NPlusOneSql, "tpl"), t);
    let fb = at(
        make_finding("svc-b", FindingType::RedundantSql, "tpl"),
        t + 1_000,
    );
    assert_eq!(correlator.ingest(&[fa], t + 15_000), 0);
    assert_eq!(correlator.ingest(&[fb], t + 30_000), 0);

    let correlations = correlator.active_correlations();
    assert_eq!(correlations.len(), 1);
    let pair = find_pair(&correlations, "svc-a", "svc-b").expect("A -> B");
    assert!((pair.median_lag_ms - 1_000.0).abs() < f64::EPSILON);
}

#[test]
fn incoming_earlier_event_becomes_source() {
    let mut correlator = event_time_correlator();
    let t = 1_000_000;
    let fb = at(
        make_finding("svc-b", FindingType::RedundantSql, "tpl"),
        t + 1_000,
    );
    let fa = at(make_finding("svc-a", FindingType::NPlusOneSql, "tpl"), t);
    assert_eq!(correlator.ingest(&[fb], t + 15_000), 0);
    assert_eq!(correlator.ingest(&[fa], t + 30_000), 0);

    let correlations = correlator.active_correlations();
    assert_eq!(correlations.len(), 1, "no reverse B -> A pair");
    let pair = find_pair(&correlations, "svc-a", "svc-b").expect("A -> B");
    assert_eq!(pair.sample_trace_id.as_deref(), Some("trace-svc-b"));
    assert_eq!(pair.source_sample_trace_id.as_deref(), Some("trace-svc-a"));
}

#[test]
fn same_batch_far_apart_events_do_not_pair() {
    let mut correlator = event_time_correlator();
    let t = 1_000_000;
    let fa = at(make_finding("svc-a", FindingType::NPlusOneSql, "tpl"), t);
    let fb = at(
        make_finding("svc-b", FindingType::RedundantSql, "tpl"),
        t + 10_000,
    );
    assert_eq!(correlator.ingest(&[fa, fb], t + 20_000), 0);
    assert!(correlator.pair_counts.is_empty());
}

#[test]
fn one_source_counts_once_per_pair_regardless_of_arrival_order() {
    let t = 1_000_000;
    let source = at(make_finding("svc-a", FindingType::NPlusOneSql, "tpl"), t);
    let targets: Vec<Finding> = (1..=3)
        .map(|i| {
            at(
                make_finding("svc-b", FindingType::RedundantSql, "tpl"),
                t + i * 100,
            )
        })
        .collect();
    for source_first in [true, false] {
        let mut correlator = event_time_correlator();
        let mut arrivals = targets.clone();
        if source_first {
            arrivals.insert(0, source.clone());
        } else {
            arrivals.push(source.clone());
        }
        for (i, finding) in arrivals.into_iter().enumerate() {
            let _ = correlator.ingest(&[finding], t + 10_000 + i as u64 * 1_000);
        }
        let correlations = correlator.active_correlations();
        let pair = find_pair(&correlations, "svc-a", "svc-b").expect("A -> B");
        assert_eq!(pair.co_occurrence_count, 1, "source_first = {source_first}");
        assert_eq!(correlations.len(), 1, "no reverse pair");
    }
}

#[test]
fn occurrence_deque_bounded_by_horizon_not_window() {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        window_ms: 1_440 * 60_000,
        lag_threshold_ms: 2_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ..Default::default()
    });
    let reach = correlator.config.lag_threshold_ms + correlator.config.ingest_skew_ms;
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    // 10,000 findings, alternating A and B every 720 ms: 2 h of ingest.
    let mut now = 0;
    for i in 0..10_000u64 {
        now = 1_000_000 + i * 720;
        let f = if i % 2 == 0 { &fa } else { &fb };
        let _ = ingest_at(&mut correlator, std::slice::from_ref(f), now);
    }
    let oldest = correlator
        .occurrences
        .front()
        .expect("occurrences")
        .ingest_ms;
    assert!(now - oldest <= reach, "deque must span the horizon only");
    assert!(correlator.occurrences.len() <= (reach / 720 + 1) as usize);

    let correlations = correlator.active_correlations();
    let pair = find_pair(&correlations, "svc-a", "svc-b").expect("A -> B");
    assert_eq!(
        pair.source_total_occurrences, 5_000,
        "totals span the window"
    );
    assert_eq!(pair.co_occurrence_count, 5_000);
}

#[test]
fn numerator_and_denominator_share_the_grid() {
    // Half window 5 s. The pair is created mid-bucket at 7 s, then
    // counts across two grid steps.
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        window_ms: 10_000,
        lag_threshold_ms: 1_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ..Default::default()
    });
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    for t in [7_000, 12_000, 17_000] {
        let _ = ingest_at(&mut correlator, std::slice::from_ref(&fa), t);
        let _ = ingest_at(&mut correlator, std::slice::from_ref(&fb), t + 500);
    }
    // One more source without a target, in the last bucket.
    let _ = ingest_at(&mut correlator, std::slice::from_ref(&fa), 18_000);

    let correlations = correlator.active_correlations();
    let pair = find_pair(&correlations, "svc-a", "svc-b").expect("A -> B");
    assert_eq!(pair.co_occurrence_count, 2, "idx 2 + idx 3");
    assert_eq!(pair.source_total_occurrences, 3, "same buckets");
    let expected = f64::from(pair.co_occurrence_count) / f64::from(pair.source_total_occurrences);
    assert!(pair.confidence <= 1.0);
    assert!((pair.confidence - expected).abs() < f64::EPSILON);
}

#[test]
fn endpoints_are_interned() {
    // Window 1 s, reach 55 s: a pair can outlive its source's count.
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        window_ms: 1_000,
        lag_threshold_ms: 5_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ingest_skew_ms: 50_000,
        ..Default::default()
    });
    let fa = at(
        make_finding("svc-a", FindingType::NPlusOneSql, "tpl"),
        1_000,
    );
    let fb = at(
        make_finding("svc-b", FindingType::RedundantSql, "tpl"),
        1_500,
    );
    let _ = correlator.ingest(&[fa.clone(), fa], 1_000);
    assert!(Arc::ptr_eq(
        &correlator.occurrences[0].endpoint,
        &correlator.occurrences[1].endpoint
    ));
    let _ = correlator.ingest(&[fb], 55_900);
    let key = correlator.pair_counts.keys().next().expect("pair").clone();
    assert!(Arc::ptr_eq(
        &correlator.occurrences[0].endpoint,
        &key.source
    ));

    // A's occurrences leave the horizon and its count the window, but
    // the pair still pins the endpoint.
    let fc = make_finding("svc-c", FindingType::SlowSql, "other");
    let _ = ingest_at(&mut correlator, std::slice::from_ref(&fc), 56_500);
    assert!(
        correlator
            .occurrences
            .iter()
            .all(|o| o.endpoint.service != "svc-a")
    );
    let (interned, count) = correlator
        .endpoints
        .get_key_value(key.source.as_ref())
        .expect("pinned endpoint survives the prune");
    assert!(Arc::ptr_eq(interned, &key.source));
    assert_eq!(count.total(correlator.now_idx), 0);
}

#[test]
fn missing_or_zero_source_total_drops_correlation() {
    // The pair counts in A's bucket, so it leaves the 1 s window with A's total.
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        window_ms: 1_000,
        lag_threshold_ms: 5_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ingest_skew_ms: 50_000,
        ..Default::default()
    });
    let fa = at(
        make_finding("svc-a", FindingType::NPlusOneSql, "tpl"),
        1_000,
    );
    let fb = at(
        make_finding("svc-b", FindingType::RedundantSql, "tpl"),
        1_500,
    );
    let _ = correlator.ingest(&[fa], 1_000);
    let _ = correlator.ingest(&[fb], 55_900);
    let state = correlator.pair_counts.values().next().expect("pair");
    assert_eq!(state.co.total(correlator.now_idx), 0);
    assert!(correlator.active_correlations().is_empty());

    // Zero, then missing: reported until the source's count or entry goes.
    let mut correlator = event_time_correlator();
    let fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    let fb = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    let _ = ingest_at(&mut correlator, &[fa], 1_000);
    let _ = ingest_at(&mut correlator, &[fb], 1_500);
    assert_eq!(correlator.active_correlations().len(), 1);
    for (ep, count) in &mut correlator.endpoints {
        if ep.service == "svc-a" {
            *count = HalfWindowCount::default();
        }
    }
    assert!(correlator.active_correlations().is_empty());
    correlator.endpoints.retain(|ep, _| ep.service != "svc-a");
    assert!(correlator.active_correlations().is_empty());
}

#[test]
fn pair_counts_in_the_source_bucket_across_a_grid_step() {
    // window_minutes = 1: 30 s buckets, reach 5 s + 2 x 30 s TTL.
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        window_ms: 60_000,
        lag_threshold_ms: 5_000,
        min_co_occurrences: 1,
        min_confidence: 0.0,
        ingest_skew_ms: 60_000,
        ..Default::default()
    });
    let a = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    let b = make_finding("svc-b", FindingType::RedundantSql, "tpl");
    let noise = make_finding("svc-c", FindingType::SlowSql, "other");
    // (finding, event ms, ingest ms): A1 in bucket 1, its target B1 50 s later in bucket 2.
    let arrivals = [
        (&a, 34_000, 35_000),
        (&a, 64_000, 65_000),
        (&b, 65_000, 66_000),
        (&a, 75_000, 76_000),
        (&b, 36_000, 85_000),
        (&noise, 90_000, 90_000),
    ];
    for (f, event, ingest) in arrivals {
        let _ = correlator.ingest(&[at(f.clone(), event)], ingest);
    }
    assert_eq!(correlator.now_idx, 3);

    // Bucket 2 only: sources A2 and A3, of which A2 paired.
    let correlations = correlator.active_correlations();
    let pair = find_pair(&correlations, "svc-a", "svc-b").expect("A -> B");
    assert_eq!(pair.co_occurrence_count, 1);
    assert_eq!(pair.source_total_occurrences, 2);
    let expected = f64::from(pair.co_occurrence_count) / f64::from(pair.source_total_occurrences);
    assert!((pair.confidence - expected).abs() < f64::EPSILON);
    assert!((pair.confidence - 0.5).abs() < f64::EPSILON);
}

#[test]
fn unparsable_first_timestamp_falls_back_to_now_ms() {
    let mut correlator = event_time_correlator();
    let mut fa = make_finding("svc-a", FindingType::NPlusOneSql, "tpl");
    fa.first_timestamp = "not a timestamp".to_string();
    let fb = at(
        make_finding("svc-b", FindingType::RedundantSql, "tpl"),
        1_000_500,
    );
    let _ = correlator.ingest(&[fa], 1_000_000);
    let _ = correlator.ingest(&[fb], 1_000_500);
    let correlations = correlator.active_correlations();
    let pair = find_pair(&correlations, "svc-a", "svc-b").expect("A -> B");
    assert!((pair.median_lag_ms - 500.0).abs() < f64::EPSILON);
}

#[test]
fn cap_eviction_prefers_low_count_then_stale_pairs() {
    let mut correlator = capped_correlator(20);
    correlator.now_idx = 10;
    let endpoint = |name: &str| {
        Arc::new(CorrelationEndpoint {
            finding_type: FindingType::NPlusOneSql,
            service: name.to_string(),
            template: "tpl".to_string(),
            grouping_key: None,
            grouping_value: None,
        })
    };
    let target = endpoint("target");
    let mut insert = |name: &str, count: u32, last_seen_ms: u64| {
        let source = endpoint(name);
        let mut state = PairState::new(0, &source, &target);
        for _ in 0..count {
            state.co.add_at(10);
        }
        state.last_seen_ms = last_seen_ms;
        correlator.pair_counts.insert(
            PairKey {
                source,
                target: Arc::clone(&target),
            },
            state,
        );
    };
    insert("low-fresh", 1, 900);
    insert("mid-stale", 3, 100);
    insert("high-stalest", 5, 50);
    for i in 0..17 {
        insert(&format!("mid-{i}"), 3, 500);
    }

    // Cap 20, down to 18: two pairs go.
    assert_eq!(correlator.enforce_pair_cap(), 2);
    let survivors: Vec<&str> = correlator
        .pair_counts
        .keys()
        .map(|k| k.source.service.as_str())
        .collect();
    assert!(!survivors.contains(&"low-fresh"), "lowest count goes first");
    assert!(!survivors.contains(&"mid-stale"), "then the stalest");
    assert!(
        survivors.contains(&"high-stalest"),
        "a high count outranks staleness"
    );
}

/// Same A-then-B shape as `detects_simple_a_then_b_pattern`, with a
/// grouping attribute on each side.
fn grouped_pairs(source: (&str, &str), target: (&str, &str)) -> Vec<CrossTraceCorrelation> {
    let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
        min_co_occurrences: 2,
        min_confidence: 0.5,
        lag_threshold_ms: 5_000,
        ..Default::default()
    });
    for i in 0..5 {
        let t = 1_000_000 + i * 10_000;
        let mut fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT * FROM t");
        fa.grouping = crate::test_helpers::grouping(source.0, source.1);
        let _ = ingest_at(&mut correlator, &[fa], t);
        let mut fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
        fb.grouping = crate::test_helpers::grouping(target.0, target.1);
        let _ = ingest_at(&mut correlator, &[fb], t + 2_000);
    }
    correlator.active_correlations()
}

#[test]
fn findings_in_different_namespaces_never_pair() {
    assert!(
        grouped_pairs(
            ("k8s.namespace.name", "prod-eu"),
            ("k8s.namespace.name", "staging")
        )
        .is_empty()
    );
}

#[test]
fn findings_in_the_same_namespace_still_pair() {
    let correlations = grouped_pairs(
        ("k8s.namespace.name", "prod-eu"),
        ("k8s.namespace.name", "prod-eu"),
    );
    assert_eq!(correlations.len(), 1);
    assert_eq!(
        correlations[0].source.grouping_value.as_deref(),
        Some("prod-eu")
    );
    assert_eq!(
        correlations[0].target.grouping_value.as_deref(),
        Some("prod-eu")
    );
}

#[test]
fn equal_values_from_different_grouping_keys_never_pair() {
    assert!(grouped_pairs(("tenant.id", "prod"), ("k8s.namespace.name", "prod")).is_empty());
}

#[test]
fn correlation_serde_roundtrip() {
    // Field present: serialize + deserialize must preserve it.
    let c = CrossTraceCorrelation {
        source: CorrelationEndpoint {
            finding_type: FindingType::NPlusOneSql,
            service: "order-svc".to_string(),
            template: "SELECT * FROM t".to_string(),
            grouping_key: Some("k8s.namespace.name".to_string()),
            grouping_value: Some("prod-eu".to_string()),
        },
        target: CorrelationEndpoint {
            finding_type: FindingType::PoolSaturation,
            service: "payment-svc".to_string(),
            template: "payment-svc".to_string(),
            grouping_key: Some("k8s.namespace.name".to_string()),
            grouping_value: Some("prod-eu".to_string()),
        },
        co_occurrence_count: 12,
        source_total_occurrences: 15,
        confidence: 0.8,
        median_lag_ms: 1200.0,
        first_seen: "2025-07-10T14:32:00.000Z".to_string(),
        last_seen: "2025-07-10T14:42:00.000Z".to_string(),
        sample_trace_id: Some("trace-abc".to_string()),
        source_sample_trace_id: Some("trace-src".to_string()),
    };
    let json = serde_json::to_string(&c).unwrap();
    let back: CrossTraceCorrelation = serde_json::from_str(&json).unwrap();
    assert_eq!(back.co_occurrence_count, 12);
    assert_eq!(back.source.service, "order-svc");
    assert_eq!(back.target.service, "payment-svc");
    assert!((back.confidence - 0.8).abs() < f64::EPSILON);
    assert_eq!(back.sample_trace_id.as_deref(), Some("trace-abc"));
    assert_eq!(back.source_sample_trace_id.as_deref(), Some("trace-src"));
    assert!(
        json.contains("\"sample_trace_id\":\"trace-abc\""),
        "field must be present in JSON when populated"
    );

    // Field absent on the wire (legacy baseline): `serde(default)`
    // restores it as `None`, preserving forward-compat.
    let legacy_json = r#"{
            "source": {"finding_type": "n_plus_one_sql", "service": "a", "template": "t"},
            "target": {"finding_type": "pool_saturation", "service": "b", "template": "t"},
            "co_occurrence_count": 1,
            "source_total_occurrences": 1,
            "confidence": 1.0,
            "median_lag_ms": 0.0,
            "first_seen": "2025-01-01T00:00:00Z",
            "last_seen": "2025-01-01T00:00:00Z"
        }"#;
    let legacy: CrossTraceCorrelation = serde_json::from_str(legacy_json).unwrap();
    assert!(legacy.sample_trace_id.is_none());
    assert!(legacy.source_sample_trace_id.is_none());

    // `None` must skip the field so batch-mode reports stay
    // byte-identical to legacy outputs.
    let none_variant = CrossTraceCorrelation {
        sample_trace_id: None,
        source_sample_trace_id: None,
        ..c
    };
    let none_json = serde_json::to_string(&none_variant).unwrap();
    assert!(!none_json.contains("sample_trace_id"));
}

/// Each correlator draws fresh hash keys, so an order leaked from the pair
/// map would differ across these runs.
const RUNS: usize = 32;

fn pair_names(correlations: &[CrossTraceCorrelation]) -> Vec<(String, String)> {
    correlations
        .iter()
        .map(|c| (c.source.service.clone(), c.target.service.clone()))
        .collect()
}

#[test]
fn tied_correlations_come_out_in_pair_order() {
    for _ in 0..RUNS {
        let mut correlator = capped_correlator(10_000);
        ingest_at(&mut correlator, &wide_batch(4), 1_000);
        let pairs = pair_names(&correlator.active_correlations());
        let mut sorted = pairs.clone();
        sorted.sort();
        assert_eq!(pairs.len(), 6);
        assert_eq!(pairs, sorted);
    }
}

#[test]
fn the_cap_evicts_the_same_tied_pairs_on_every_run() {
    let survivors = || {
        let mut correlator = capped_correlator(10);
        ingest_at(&mut correlator, &wide_batch(6), 1_000);
        pair_names(&correlator.active_correlations())
    };
    let first = survivors();
    assert!(first.len() < 15, "the cap must have evicted: {first:?}");
    for _ in 1..RUNS {
        assert_eq!(survivors(), first);
    }
}
