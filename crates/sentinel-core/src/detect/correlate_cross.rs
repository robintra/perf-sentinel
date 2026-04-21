//! Cross-trace temporal correlation engine for daemon mode.
//!
//! Detects recurring co-occurrences between findings from different
//! services/traces within a configurable time window.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;

use super::FindingType;
use crate::detect::Finding;

/// Configuration for cross-trace correlation.
#[derive(Debug, Clone)]
pub struct CorrelationConfig {
    /// Rolling window in milliseconds (default 600,000 = 10 min).
    pub window_ms: u64,
    /// Max delay between correlated findings in milliseconds (default 5,000).
    pub lag_threshold_ms: u64,
    /// Minimum co-occurrence count to report a correlation.
    pub min_co_occurrences: u32,
    /// Minimum confidence to report a correlation.
    pub min_confidence: f64,
    /// Maximum tracked pairs to prevent unbounded memory growth.
    pub max_tracked_pairs: usize,
}

impl Default for CorrelationConfig {
    fn default() -> Self {
        Self {
            window_ms: 600_000,
            lag_threshold_ms: 5_000,
            min_co_occurrences: 5,
            min_confidence: 0.7,
            max_tracked_pairs: 10_000,
        }
    }
}

/// One side of a cross-trace correlation pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub struct CorrelationEndpoint {
    /// Finding type for this correlation side (e.g. `n_plus_one_sql`).
    pub finding_type: FindingType,
    /// Service name that produced the finding.
    pub service: String,
    /// Normalized query or URL template associated with the finding.
    pub template: String,
}

/// A detected temporal correlation between findings across services.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CrossTraceCorrelation {
    /// Leading endpoint: the finding observed first in each co-occurrence.
    pub source: CorrelationEndpoint,
    /// Trailing endpoint: the finding observed after the source within lag.
    pub target: CorrelationEndpoint,
    /// Number of times source and target fired together within the window.
    pub co_occurrence_count: u32,
    /// Total occurrences of the source endpoint over the rolling window.
    pub source_total_occurrences: u32,
    /// Ratio `co_occurrence_count / source_total_occurrences`, in `[0, 1]`.
    pub confidence: f64,
    /// Median observed lag, in milliseconds, between source and target.
    pub median_lag_ms: f64,
    /// ISO 8601 timestamp of the first observed co-occurrence.
    pub first_seen: String,
    /// ISO 8601 timestamp of the most recent observed co-occurrence.
    pub last_seen: String,
    /// Trace id of the most recent target-side finding that completed
    /// this pair (the trailing finding in the source -> target order).
    /// Lets the dashboard jump from a correlation row to Explain and
    /// render a representative tree. `None` in batch mode and for
    /// replayed baselines that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_trace_id: Option<String>,
}

/// Key for a correlation pair in the internal map.
///
/// Holds `Arc<CorrelationEndpoint>` on both sides so inner-loop cloning
/// in `ingest()` is a pointer bump instead of 3 `String` clones per
/// endpoint. Interning is handled by `intern_endpoint` which shares
/// `Arc`s across `occurrences` entries that reference the same endpoint.
#[derive(Debug, Clone)]
struct PairKey {
    source: std::sync::Arc<CorrelationEndpoint>,
    target: std::sync::Arc<CorrelationEndpoint>,
}

impl PartialEq for PairKey {
    fn eq(&self, other: &Self) -> bool {
        // Compare Arc by pointer first (cheap, works for interned endpoints);
        // fall back to value equality for cross-Arc pairs.
        (std::sync::Arc::ptr_eq(&self.source, &other.source) || self.source == other.source)
            && (std::sync::Arc::ptr_eq(&self.target, &other.target) || self.target == other.target)
    }
}

impl Eq for PairKey {}

impl std::hash::Hash for PairKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Hash the CorrelationEndpoint values, not the Arc pointers, so
        // two PairKeys with structurally equal but distinct-Arc endpoints
        // hash to the same bucket.
        self.source.hash(state);
        self.target.hash(state);
    }
}

/// Maximum number of lag samples kept per tracked pair.
///
/// Uses reservoir sampling to bound memory per pair: a hot pair firing
/// thousands of times only keeps `MAX_LAG_SAMPLES` values for median
/// computation. The estimate is unbiased since every observed lag has
/// equal probability of being in the reservoir.
const MAX_LAG_SAMPLES: usize = 256;

/// Internal state for a correlation pair.
struct PairState {
    co_occurrence_count: u32,
    /// Bounded reservoir of lag samples (max `MAX_LAG_SAMPLES`).
    lags_ms: Vec<f64>,
    /// Total number of lag observations seen (independent of reservoir size).
    /// Used by Algorithm R to decide replacement probability.
    total_observations: u64,
    /// `SplitMix64` PRNG state used to drive reservoir sampling. Seeded
    /// from `first_seen_ms` when the pair is first inserted so different
    /// pairs evolve independent sample streams.
    rng_state: u64,
    first_seen_ms: u64,
    last_seen_ms: u64,
    /// Trace id of the most recent target-side finding that completed
    /// this pair (the trailing finding in the source -> target order).
    /// Overwritten on every co-occurrence so the value tracks the
    /// latest observation. Adds one `Option<String>` (~40 bytes worst
    /// case) per active pair, well under the correlator's 20 MB
    /// lag-reservoir budget at the 10,000 pair cap.
    last_trace_id: Option<String>,
}

impl PairState {
    /// Append a lag sample using Algorithm R reservoir sampling.
    ///
    /// While the reservoir has space, append. Once full, draw a random
    /// replacement probability `k/n` (k = `MAX_LAG_SAMPLES`, n =
    /// `total_observations`). When the draw succeeds, the slot itself
    /// is chosen uniformly in `[0, k)`.
    ///
    /// Uses `SplitMix64` (10 lines, no `rand` dependency) which has
    /// excellent statistical properties over its full period. A prior
    /// implementation used `fnv1a(total_observations) % total_observations`
    /// which is biased (FNV of consecutive integers is not uniform in
    /// small-range modulos) and caused the reservoir to freeze after a
    /// few thousand observations.
    fn record_lag(&mut self, lag_ms: f64) {
        self.total_observations = self.total_observations.saturating_add(1);
        if self.lags_ms.len() < MAX_LAG_SAMPLES {
            self.lags_ms.push(lag_ms);
            return;
        }
        // Algorithm R: draw r uniform in `[0, n)`. When `r < k`, use r
        // itself as the slot index. This is unbiased because, conditional
        // on `r < k`, `r` is uniform in `[0, k)`, which is exactly the
        // uniform slot we need. Saves a second PRNG draw versus sampling
        // the slot independently.
        let r = splitmix64(&mut self.rng_state) % self.total_observations;
        if r < MAX_LAG_SAMPLES as u64 {
            self.lags_ms[r as usize] = lag_ms;
        }
    }
}

/// `SplitMix64` PRNG. Excellent distribution for Algorithm R, 10 lines,
/// zero dependencies. Advances state in place and returns a fresh u64.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Cheap 64-bit hash of a `CorrelationEndpoint`, used only to diversify
/// `PairState` PRNG seeds. Not cryptographic, not exposed externally.
///
/// Uses FNV-1a rather than `std::hash::DefaultHasher` on purpose: the
/// default hasher is seeded with a per-process `RandomState`, which
/// makes the correlator produce different reservoir samples across runs
/// given identical traffic. Determinism matters here because users debug
/// correlations by replaying trace files: two runs on the same input
/// should produce the same median-lag values. FNV-1a has a fixed seed
/// and no observable security-relevant side channel (the correlator is
/// not adversarial, inputs come from our own detectors).
fn hash_endpoint(ep: &CorrelationEndpoint) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x100_0000_01b3;
    let mut h: u64 = FNV_OFFSET;
    // Mix in the enum discriminant via its `as_str()` label so different
    // finding types do not collide.
    for b in ep.finding_type.as_str().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h ^= 0xFF; // domain separator between finding_type and service
    for b in ep.service.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h ^= 0xFE; // domain separator between service and template
    for b in ep.template.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// A recent finding occurrence in the rolling window.
struct FindingOccurrence {
    /// `Arc`-wrapped endpoint so cloning into `PairKey` slots inside the
    /// inner correlation loop is a pointer bump instead of 3 `String`
    /// clones per match.
    endpoint: std::sync::Arc<CorrelationEndpoint>,
    timestamp_ms: u64,
}

/// Cross-trace correlator. Owned by the daemon event loop.
///
/// Maintains a rolling window of recent finding occurrences and detects
/// recurring temporal co-occurrences between different services.
pub struct CrossTraceCorrelator {
    occurrences: VecDeque<FindingOccurrence>,
    pair_counts: HashMap<PairKey, PairState>,
    source_totals: HashMap<CorrelationEndpoint, u32>,
    config: CorrelationConfig,
}

impl CrossTraceCorrelator {
    #[must_use]
    pub fn new(config: CorrelationConfig) -> Self {
        Self {
            occurrences: VecDeque::new(),
            pair_counts: HashMap::new(),
            source_totals: HashMap::new(),
            config,
        }
    }

    /// Decrement the count for `endpoint` in `source_totals`. When the
    /// count reaches zero, remove the entry entirely so the map stays
    /// bounded by the number of distinct endpoints currently in the
    /// window. Written as an associated function so callers can invoke
    /// it under a `&mut self.occurrences` borrow.
    fn decrement_source_total(
        source_totals: &mut HashMap<CorrelationEndpoint, u32>,
        endpoint: &CorrelationEndpoint,
    ) {
        if let Some(count) = source_totals.get_mut(endpoint) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                source_totals.remove(endpoint);
            }
        }
    }

    /// Ingest a batch of findings from `process_traces`.
    ///
    /// Evicts stale entries, then checks for co-occurrences between
    /// the new findings and recent ones from different services.
    ///
    /// `source_totals` is maintained incrementally: increment on
    /// `push_back`, decrement on `pop_front`. Removing an entry when its
    /// count drops to 0 keeps the map size bounded by the number of
    /// distinct endpoints currently in the window. This avoids the
    /// O(occurrences) per-tick rebuild that a `clear + repopulate`
    /// approach would require.
    pub fn ingest(&mut self, findings: &[Finding], now_ms: u64) {
        let cutoff = now_ms.saturating_sub(self.config.window_ms);
        self.evict_stale(cutoff);
        self.pair_counts
            .retain(|_, state| state.last_seen_ms >= cutoff);

        for finding in findings {
            let endpoint = std::sync::Arc::new(CorrelationEndpoint {
                finding_type: finding.finding_type.clone(),
                service: finding.service.clone(),
                template: finding.pattern.template.clone(),
            });
            self.record_co_occurrences(&endpoint, now_ms, finding.trace_id.as_str());
            *self.source_totals.entry((*endpoint).clone()).or_insert(0) += 1;
            self.occurrences.push_back(FindingOccurrence {
                endpoint,
                timestamp_ms: now_ms,
            });
        }

        self.enforce_pair_cap();
    }

    /// Drop occurrences older than `cutoff`, decrementing `source_totals`.
    ///
    /// The `loop + match` pattern avoids both an `.expect()` on the pop
    /// and the duplication of the staleness check across peek and pop.
    fn evict_stale(&mut self, cutoff: u64) {
        loop {
            match self.occurrences.front() {
                Some(front) if front.timestamp_ms < cutoff => {
                    if let Some(expired) = self.occurrences.pop_front() {
                        Self::decrement_source_total(&mut self.source_totals, &expired.endpoint);
                    }
                }
                _ => break,
            }
        }
    }

    /// Scan recent occurrences for entries from a different service within
    /// `lag_threshold_ms` and increment the matching pair counters.
    ///
    /// `trace_id` is the incoming target-side finding's trace id. It is
    /// stored on every matching [`PairState`] so [`active_correlations`]
    /// can surface a representative trace id for UI jump-through.
    fn record_co_occurrences(
        &mut self,
        endpoint: &std::sync::Arc<CorrelationEndpoint>,
        now_ms: u64,
        trace_id: &str,
    ) {
        for occ in self.occurrences.iter().rev() {
            let age = now_ms.saturating_sub(occ.timestamp_ms);
            if age > self.config.lag_threshold_ms {
                break;
            }
            if occ.endpoint.service == endpoint.service {
                continue;
            }

            let key = PairKey {
                source: occ.endpoint.clone(), // Arc clone: pointer bump
                target: endpoint.clone(),     // Arc clone: pointer bump
            };
            // Lag fits in f64 for any reasonable window. `as f64` loses
            // precision only for values above 2^53 ms (~285k years).
            #[allow(clippy::cast_precision_loss)]
            let lag = age as f64;
            let state = self.pair_counts.entry(key).or_insert_with(|| PairState {
                co_occurrence_count: 0,
                lags_ms: Vec::new(),
                total_observations: 0,
                // Seed the PRNG from first_seen_ms so different pairs
                // evolve independent sample streams. Pairs created at
                // the same tick get the same seed; we mix in the
                // endpoint's hash to diversify.
                rng_state: now_ms ^ (hash_endpoint(&occ.endpoint) << 17) ^ hash_endpoint(endpoint),
                first_seen_ms: now_ms,
                last_seen_ms: now_ms,
                last_trace_id: None,
            });
            state.co_occurrence_count = state.co_occurrence_count.saturating_add(1);
            state.record_lag(lag);
            state.last_seen_ms = now_ms;
            // Overwrite on every hit so the surfaced trace id matches
            // the most recent observation. Empty incoming ids fall back
            // to whatever was recorded previously, so replayed streams
            // with stripped trace ids do not clobber a real value.
            if !trace_id.is_empty() {
                state.last_trace_id = Some(trace_id.to_string());
            }
        }
    }

    /// Enforce `max_tracked_pairs` cap. Evicts the pairs with the lowest
    /// `co_occurrence_count`.
    ///
    /// Two optimizations over the naive "clone all keys then select":
    ///
    /// 1. **Amortization**: when we overflow, evict down to 90% of the
    ///    cap in one pass so the O(n) work is paid once per 10% of
    ///    overflow rather than once per insert beyond the cap.
    /// 2. **Lazy cloning**: find the eviction threshold via a
    ///    `select_nth_unstable` on a `Vec<u32>` (Copy, no String pair
    ///    clones), then clone keys only for the ~10% we'll actually
    ///    remove. Previously we cloned every `PairKey` in the map
    ///    (two `CorrelationEndpoint` strings each) on every cap hit.
    fn enforce_pair_cap(&mut self) {
        if self.pair_counts.len() <= self.config.max_tracked_pairs {
            return;
        }
        // Evict down to 90% of cap so subsequent inserts don't re-trip
        // the cap after a single-element overflow.
        let cap = self.config.max_tracked_pairs;
        let target = cap - cap / 10;
        let to_remove = self.pair_counts.len().saturating_sub(target).max(1);

        // O(n) threshold computation. Only u32 counts are copied.
        let mut counts: Vec<u32> = self
            .pair_counts
            .values()
            .map(|v| v.co_occurrence_count)
            .collect();
        // `select_nth_unstable(k)` positions the k-th smallest at
        // index k. We want the value such that at least `to_remove`
        // elements are `<= value`, so the (to_remove - 1)-th smallest.
        let pivot_index = to_remove - 1;
        let threshold = *counts.select_nth_unstable(pivot_index).1;

        // Collect the keys to evict: all pairs strictly below the
        // threshold, plus as many at-threshold pairs as needed to hit
        // exactly `to_remove`. Only these keys pay the clone cost.
        let mut below_threshold: Vec<PairKey> = self
            .pair_counts
            .iter()
            .filter(|(_, v)| v.co_occurrence_count < threshold)
            .map(|(k, _)| k.clone())
            .collect();
        if below_threshold.len() < to_remove {
            let extra_needed = to_remove - below_threshold.len();
            below_threshold.extend(
                self.pair_counts
                    .iter()
                    .filter(|(_, v)| v.co_occurrence_count == threshold)
                    .take(extra_needed)
                    .map(|(k, _)| k.clone()),
            );
        }
        for key in below_threshold {
            self.pair_counts.remove(&key);
        }
    }

    /// Return all active correlations above the configured thresholds.
    #[must_use]
    pub fn active_correlations(&self) -> Vec<CrossTraceCorrelation> {
        self.pair_counts
            .iter()
            .filter_map(|(key, state)| {
                if state.co_occurrence_count < self.config.min_co_occurrences {
                    return None;
                }
                let source_total = self
                    .source_totals
                    .get(key.source.as_ref())
                    .copied()
                    .unwrap_or(1);
                let confidence =
                    f64::from(state.co_occurrence_count) / f64::from(source_total.max(1));
                if confidence < self.config.min_confidence {
                    return None;
                }
                let median_lag = median(&state.lags_ms);
                Some(CrossTraceCorrelation {
                    source: (*key.source).clone(),
                    target: (*key.target).clone(),
                    co_occurrence_count: state.co_occurrence_count,
                    source_total_occurrences: source_total,
                    confidence,
                    median_lag_ms: median_lag,
                    first_seen: crate::time::millis_to_iso8601(state.first_seen_ms),
                    last_seen: crate::time::millis_to_iso8601(state.last_seen_ms),
                    sample_trace_id: state.last_trace_id.clone(),
                })
            })
            .collect()
    }
}

/// Compute the median of a slice of lag values.
///
/// Clones the slice into a fresh `Vec` before sorting so the caller's
/// reservoir is preserved (other `active_correlations()` calls would
/// otherwise see a permuted reservoir). The clone is bounded by
/// `MAX_LAG_SAMPLES = 256` f64 (2 KB per call), which is acceptable
/// for the query API path (not called per-event).
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        f64::midpoint(sorted[mid - 1], sorted[mid])
    } else {
        sorted[mid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_finding(service: &str, finding_type: FindingType, template: &str) -> Finding {
        Finding {
            finding_type,
            severity: crate::detect::Severity::Warning,
            trace_id: format!("trace-{service}"),
            service: service.to_string(),
            source_endpoint: "POST /api/test".to_string(),
            pattern: crate::detect::Pattern {
                template: template.to_string(),
                occurrences: 5,
                window_ms: 200,
                distinct_params: 5,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.200Z".to_string(),
            green_impact: None,
            confidence: crate::detect::Confidence::default(),
            code_location: None,
            suggested_fix: None,
        }
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
            correlator.ingest(&[fa], t);
            let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
            correlator.ingest(&[fb], t + 2_000);
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
            correlator.ingest(&[fa, fb], t);
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
        correlator.ingest(&[fa], 1_000);
        let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
        correlator.ingest(&[fb], 2_000);

        // After window expires, occurrences are evicted.
        let fa2 = make_finding("other-svc", FindingType::SlowSql, "SELECT 2");
        correlator.ingest(&[fa2], 100_000);

        assert!(
            correlator.occurrences.len() <= 2,
            "stale entries should be evicted"
        );
    }

    #[test]
    fn max_tracked_pairs_enforced() {
        let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
            max_tracked_pairs: 5,
            lag_threshold_ms: 100_000,
            min_co_occurrences: 1,
            min_confidence: 0.0,
            ..Default::default()
        });

        // Create many distinct pairs.
        for i in 0..20 {
            let fa = make_finding(
                &format!("svc-a-{i}"),
                FindingType::NPlusOneSql,
                &format!("tpl-{i}"),
            );
            correlator.ingest(&[fa], 1000);
            let fb = make_finding(
                &format!("svc-b-{i}"),
                FindingType::RedundantSql,
                &format!("tpl-{i}"),
            );
            correlator.ingest(&[fb], 1001);
        }

        assert!(
            correlator.pair_counts.len() <= 5,
            "pair count should be capped at max_tracked_pairs"
        );
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
            correlator.ingest(&[fa], t);
            if i < 2 {
                let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
                correlator.ingest(&[fb], t + 1_000);
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
        correlator.ingest(&[fa], 1_000);
        let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
        correlator.ingest(&[fb], 10_000);

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

        // Fire the same A -> B pair 10x MAX_LAG_SAMPLES times.
        // Without the reservoir, lags_ms would grow to ~2560 entries.
        let total = MAX_LAG_SAMPLES * 10;
        for i in 0..total {
            let t = 1_000_000 + i as u64 * 10;
            let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT 1");
            correlator.ingest(&[fa], t);
            let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
            correlator.ingest(&[fb], t + 1);
        }

        // Directional pairs: both (A->B) and (B->A) are tracked because
        // each finding scans the window for prior different-service
        // occurrences. Both directions should have bounded reservoirs.
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
        // Regression guard for a previous implementation that used
        // `fnv1a(total_observations) % total_observations` as the draw,
        // which caused the reservoir to freeze after a few thousand
        // observations (deterministic hash + modulo = biased index).
        //
        // Feeds the reservoir with monotonically increasing lag values
        // and checks two properties:
        //
        // 1. **Mean tracks the population mean** within 10%. For a
        //    population uniform on [0, n), the true mean is (n-1)/2.
        //    Reservoir-size-k sample mean has standard error
        //    sigma_pop / sqrt(k). With n=5120, k=256, sigma_pop ~= 1478,
        //    the expected SE ~= 92, so 10% of 2559.5 ~= 256 is ~2.8 sigma.
        //    Still generous enough to avoid flakes across different PRNG
        //    seeds.
        //
        // 2. **Variance is non-trivial**. A frozen reservoir would have
        //    all samples from the first MAX_LAG_SAMPLES values, giving
        //    a variance bounded by (MAX_LAG_SAMPLES/2)^2 ~= 16384. A
        //    healthy reservoir covers the full range so variance should
        //    be at least 1/4 of the population variance
        //    (pop_variance = n^2/12 for uniform on [0, n)).
        let mut state = PairState {
            co_occurrence_count: 0,
            lags_ms: Vec::new(),
            total_observations: 0,
            rng_state: 0x1234_5678_9ABC_DEF0,
            first_seen_ms: 0,
            last_seen_ms: 0,
            last_trace_id: None,
        };
        let n = MAX_LAG_SAMPLES * 20;
        for i in 0..n {
            state.record_lag(i as f64);
        }
        let mean: f64 = state.lags_ms.iter().sum::<f64>() / state.lags_ms.len() as f64;
        let expected_mean = (n - 1) as f64 / 2.0;
        let tolerance = expected_mean * 0.10;
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

    #[test]
    fn source_totals_rebuilt_from_window_on_each_ingest() {
        let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
            window_ms: 1_000,
            min_co_occurrences: 1,
            min_confidence: 0.1,
            ..Default::default()
        });

        // Ingest a finding, then let it expire.
        let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT 1");
        correlator.ingest(&[fa], 1_000);
        assert_eq!(correlator.source_totals.len(), 1);

        // Next ingest after the window has elapsed: stale entry must be
        // evicted from source_totals by the rebuild, not leaked.
        let fb = make_finding("other-svc", FindingType::NPlusOneSql, "SELECT 2");
        correlator.ingest(&[fb], 10_000);
        // Only the current finding's endpoint should remain.
        assert!(
            correlator.source_totals.len() <= 1,
            "source_totals should not retain stale entries"
        );
    }

    #[test]
    fn correlation_serde_roundtrip() {
        // Field present: serialize + deserialize must preserve it.
        let c = CrossTraceCorrelation {
            source: CorrelationEndpoint {
                finding_type: FindingType::NPlusOneSql,
                service: "order-svc".to_string(),
                template: "SELECT * FROM t".to_string(),
            },
            target: CorrelationEndpoint {
                finding_type: FindingType::PoolSaturation,
                service: "payment-svc".to_string(),
                template: "payment-svc".to_string(),
            },
            co_occurrence_count: 12,
            source_total_occurrences: 15,
            confidence: 0.8,
            median_lag_ms: 1200.0,
            first_seen: "2025-07-10T14:32:00.000Z".to_string(),
            last_seen: "2025-07-10T14:42:00.000Z".to_string(),
            sample_trace_id: Some("trace-abc".to_string()),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: CrossTraceCorrelation = serde_json::from_str(&json).unwrap();
        assert_eq!(back.co_occurrence_count, 12);
        assert_eq!(back.source.service, "order-svc");
        assert_eq!(back.target.service, "payment-svc");
        assert!((back.confidence - 0.8).abs() < f64::EPSILON);
        assert_eq!(back.sample_trace_id.as_deref(), Some("trace-abc"));
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

        // `None` must skip the field entirely in serialization so
        // batch-mode reports stay byte-identical to v0.5.0 outputs.
        let none_variant = CrossTraceCorrelation {
            sample_trace_id: None,
            ..c
        };
        let none_json = serde_json::to_string(&none_variant).unwrap();
        assert!(
            !none_json.contains("sample_trace_id"),
            "None value must be skipped, kept report shape stable for legacy consumers"
        );
    }
}
