//! Cross-trace temporal correlation engine for daemon mode.
//!
//! Detects recurring co-occurrences between findings from different
//! services/traces within a configurable time window. Findings pair on
//! their own first-span timestamps (event time), while retention and
//! eviction run on the daemon's ingest clock.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde::Serialize;

use super::FindingType;
use crate::detect::Finding;

/// Configuration for cross-trace correlation.
#[derive(Debug, Clone)]
pub struct CorrelationConfig {
    /// Whether cross-trace correlation runs at all. Opt-in, `false` by
    /// default: the daemon then never builds a `CrossTraceCorrelator`
    /// and the other fields are irrelevant.
    pub enabled: bool,
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
    /// Extra ingest-time reach so findings analysed in different ticks
    /// still pair. Derived by the daemon from `trace_ttl_ms`, not a TOML key.
    pub ingest_skew_ms: u64,
}

impl Default for CorrelationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            window_ms: 600_000,
            lag_threshold_ms: 5_000,
            min_co_occurrences: 5,
            min_confidence: 0.7,
            max_tracked_pairs: 10_000,
            ingest_skew_ms: 60_000,
        }
    }
}

/// Counts pairs refused at the `max_tracked_pairs` cap within one batch.
///
/// Deduplicating is the useful semantics, one refused pair counts once
/// per batch however many horizon occurrences matched it, but the set
/// has to be bounded: a batch on a wide topology walks the cross product
/// of the incoming findings and the horizon, which reaches millions of
/// distinct keys and a table far larger than the map they were refused
/// from. Past the ceiling the count degrades to occurrences, which
/// overstates rather than hides.
#[derive(Debug, Default)]
struct RefusedPairs {
    seen: std::collections::HashSet<PairKey>,
    beyond_ceiling: usize,
}

impl RefusedPairs {
    /// Deduplicated up to here, then counted. A `PairKey` is two `Arc`s,
    /// so 8192 of them land in a 16384-bucket table at 17 bytes each,
    /// 272 KiB. Any deployment refusing more than that per batch is past
    /// the point where an exact figure tells the operator anything the
    /// order of magnitude does not.
    const CEILING: usize = 8_192;

    fn record(&mut self, key: PairKey) {
        if self.seen.len() < Self::CEILING {
            self.seen.insert(key);
        } else {
            self.beyond_ceiling += 1;
        }
    }

    fn total(&self) -> usize {
        self.seen.len() + self.beyond_ceiling
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
    /// Attribute name that supplied [`Self::grouping_value`]. Absent on
    /// replayed baselines that predate configurable grouping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping_key: Option<String>,
    /// Effective grouping value, so two deployments never share a pair.
    /// Absent on replayed baselines that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping_value: Option<String>,
}

/// A detected temporal correlation between findings across services.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CrossTraceCorrelation {
    /// Leading endpoint: the finding with the earlier first-span
    /// timestamp in each co-occurrence.
    pub source: CorrelationEndpoint,
    /// Trailing endpoint: the finding that started after the source
    /// within the lag threshold.
    pub target: CorrelationEndpoint,
    /// Times source and target fired together over roughly the rolling
    /// window (half-window bucket estimate, not a lifetime counter).
    pub co_occurrence_count: u32,
    /// Total occurrences of the source endpoint over the same buckets.
    pub source_total_occurrences: u32,
    /// Ratio `co_occurrence_count / source_total_occurrences`, in `[0, 1]`.
    pub confidence: f64,
    /// Median event-time lag, in milliseconds, between source and target.
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
    /// Trace id of the source-side finding of the most recent
    /// co-occurrence (the leading finding in the source -> target order).
    /// Lets the dashboard open both sides of the pair in Explain.
    /// `None` in batch mode and for replayed baselines that predate this
    /// field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sample_trace_id: Option<String>,
}

/// Key for a correlation pair in the internal map.
///
/// Both sides are interned `Arc`s from the endpoint registry, so cloning
/// a key is two pointer bumps and equality short-circuits on the pointer.
#[derive(Debug, Clone)]
struct PairKey {
    source: Arc<CorrelationEndpoint>,
    target: Arc<CorrelationEndpoint>,
}

impl PartialEq for PairKey {
    fn eq(&self, other: &Self) -> bool {
        (Arc::ptr_eq(&self.source, &other.source) || self.source == other.source)
            && (Arc::ptr_eq(&self.target, &other.target) || self.target == other.target)
    }
}

impl Eq for PairKey {}

impl std::hash::Hash for PairKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Hash the values, not the pointers, to stay consistent with `eq`.
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
const MAX_LAG_SAMPLES: usize = 64;

/// Defense-in-depth cap on stored trace ids, matching the upstream
/// `sanitize_span_event` trace-id cap. Prevents a hostile or malformed
/// upstream from inflating the daemon's memory and exported report size
/// via an arbitrarily long trace id.
const MAX_SAMPLE_TRACE_ID_BYTES: usize = 128;

/// Count over the last half-to-full window on the shared grid `now_ms / (window_ms / 2)`.
#[derive(Debug, Default)]
struct HalfWindowCount {
    idx: u64,
    cur: u32,
    prev: u32,
}

impl HalfWindowCount {
    fn total(&self, now_idx: u64) -> u32 {
        match now_idx.saturating_sub(self.idx) {
            0 => self.prev.saturating_add(self.cur),
            1 => self.cur,
            _ => 0,
        }
    }

    /// Count one at grid index `idx`; indices older than the previous bucket are dropped.
    fn add_at(&mut self, idx: u64) {
        if idx + 1 == self.idx {
            self.prev = self.prev.saturating_add(1);
            return;
        }
        if idx < self.idx {
            return;
        }
        if idx != self.idx {
            self.prev = if idx - self.idx == 1 { self.cur } else { 0 };
            self.cur = 0;
            self.idx = idx;
        }
        self.cur = self.cur.saturating_add(1);
    }
}

/// Internal state for a correlation pair.
struct PairState {
    /// Co-occurrences on the global half-window grid.
    co: HalfWindowCount,
    /// Bounded reservoir of event-time lag samples (max `MAX_LAG_SAMPLES`).
    lags_ms: Vec<f64>,
    /// Total number of lag observations seen (independent of reservoir size).
    /// Used by Algorithm R to decide replacement probability.
    total_observations: u64,
    /// `SplitMix64` PRNG state used to drive reservoir sampling.
    rng_state: u64,
    /// Ingest time of the pair's creation.
    first_seen_ms: u64,
    /// Ingest time of the latest match; drives the window TTL.
    last_seen_ms: u64,
    /// Trace id of the most recent target-side finding that completed
    /// this pair. Capped at [`MAX_SAMPLE_TRACE_ID_BYTES`].
    last_trace_id: Option<String>,
    /// Trace id of the source-side finding of the same match. Capped at
    /// [`MAX_SAMPLE_TRACE_ID_BYTES`].
    last_source_trace_id: Option<String>,
}

impl PairState {
    fn new(now_ms: u64, source: &CorrelationEndpoint, target: &CorrelationEndpoint) -> Self {
        Self {
            co: HalfWindowCount::default(),
            lags_ms: Vec::new(),
            total_observations: 0,
            // Mix the endpoints in so pairs created on the same tick
            // evolve independent sample streams.
            rng_state: now_ms ^ (hash_endpoint(source) << 17) ^ hash_endpoint(target),
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
            last_trace_id: None,
            last_source_trace_id: None,
        }
    }

    /// Append a lag sample using Algorithm R reservoir sampling:
    /// append while the reservoir has space, then replace slot `r`
    /// when a uniform draw `r` in `[0, n)` lands below
    /// `MAX_LAG_SAMPLES`. Driven by `SplitMix64` (no `rand`
    /// dependency); a biased draw freezes the reservoir, see the
    /// `reservoir_continues_to_sample_after_many_observations` test.
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

    /// Record the source and target trace ids of the latest match.
    fn update_sample_trace_ids(&mut self, source_trace_id: &str, target_trace_id: &str) {
        update_sample_trace_id(&mut self.last_source_trace_id, source_trace_id);
        update_sample_trace_id(&mut self.last_trace_id, target_trace_id);
    }
}

/// Store `trace_id` in `slot` unless it is empty or already stored.
/// Trace ids longer than [`MAX_SAMPLE_TRACE_ID_BYTES`] are truncated on
/// a UTF-8 boundary.
fn update_sample_trace_id(slot: &mut Option<String>, trace_id: &str) {
    if trace_id.is_empty() || slot.as_deref() == Some(trace_id) {
        return;
    }
    let capped = truncate_to_utf8_boundary(trace_id, MAX_SAMPLE_TRACE_ID_BYTES);
    *slot = Some(capped.to_string());
}

/// Truncate `s` to at most `max_bytes`, moving the cut backwards to the
/// nearest UTF-8 character boundary. Trace ids are ASCII in every known
/// emitter but the correlator cannot assume so when ingesting from
/// replay files, so the boundary walk guards against slicing inside a
/// multibyte codepoint.
fn truncate_to_utf8_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
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
/// `PairState` PRNG seeds. FNV-1a rather than `DefaultHasher` because
/// the latter's per-process `RandomState` would make reservoir samples
/// (and median lags) differ across runs on the same replayed input;
/// determinism is what users rely on when debugging by replay.
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

/// Whether two endpoints may form a pair: different services in the
/// same grouping. Pairing across namespaces invents a causal link
/// between two deployments.
fn pairable(a: &Arc<CorrelationEndpoint>, b: &Arc<CorrelationEndpoint>) -> bool {
    !Arc::ptr_eq(a, b)
        && a.service != b.service
        && a.grouping_key == b.grouping_key
        && a.grouping_value == b.grouping_value
}

/// A recent finding occurrence, kept for the pairing horizon only.
struct FindingOccurrence {
    /// Interned endpoint shared with the registry and the pair keys.
    endpoint: Arc<CorrelationEndpoint>,
    /// First-span timestamp of the finding, the pairing clock.
    event_ms: u64,
    /// Daemon time at ingest, the eviction clock.
    ingest_ms: u64,
    /// Grid index at ingest, the bucket its pairs are credited to as a source.
    ingest_idx: u64,
    /// Capped trace id, the sample when this occurrence is a target.
    trace_id: Box<str>,
    /// Targets this occurrence already counted for as a source.
    counted_targets: Vec<Arc<CorrelationEndpoint>>,
}

/// Cross-trace correlator. Owned by the daemon event loop.
///
/// Keeps the findings of the last `lag_threshold_ms + ingest_skew_ms` of
/// ingest time for pairing, and window-scoped counters for reporting,
/// so memory grows with distinct endpoints in `window_ms`, not occurrences.
pub struct CrossTraceCorrelator {
    occurrences: VecDeque<FindingOccurrence>,
    pair_counts: HashMap<PairKey, PairState>,
    // ponytail: uncapped, bounded by distinct endpoints seen per window; cap like pairs if it ever dominates RSS
    endpoints: HashMap<Arc<CorrelationEndpoint>, HalfWindowCount>,
    /// Current index on the global half-window grid, never decreasing.
    now_idx: u64,
    /// Grid index of the last endpoint prune.
    pruned_idx: u64,
    config: CorrelationConfig,
}

impl CrossTraceCorrelator {
    #[must_use]
    pub fn new(config: CorrelationConfig) -> Self {
        Self {
            occurrences: VecDeque::new(),
            pair_counts: HashMap::new(),
            endpoints: HashMap::new(),
            now_idx: 0,
            pruned_idx: 0,
            config,
        }
    }

    /// Ingest a batch of findings from `process_traces`.
    ///
    /// Evicts stale state, then pairs each new finding with every
    /// horizon occurrence whose first-span timestamp lies within
    /// `lag_threshold_ms` of its own. Returns the number of pairs lost
    /// to the `max_tracked_pairs` cap in this batch (refusals + incumbents
    /// evicted at batch end). One pair matched by several occurrences
    /// counts once while the refused set is under its ceiling and once
    /// per occurrence above it, and a pair refused again on a later batch
    /// counts again: the lifetime counter reads as "pair-batches lost".
    #[must_use = "the eviction count feeds perf_sentinel_correlator_pairs_evicted_total"]
    pub fn ingest(&mut self, findings: &[Finding], now_ms: u64) -> usize {
        let half_window_ms = (self.config.window_ms / 2).max(1);
        self.now_idx = self.now_idx.max(now_ms / half_window_ms);
        self.evict_stale(now_ms);
        let cutoff = now_ms.saturating_sub(self.config.window_ms);
        self.pair_counts
            .retain(|_, state| state.last_seen_ms >= cutoff);
        self.prune_endpoints();

        let mut refused = RefusedPairs::default();
        for finding in findings {
            let endpoint = self.intern(finding);
            let mut incoming = FindingOccurrence {
                endpoint,
                event_ms: crate::time::parse_iso8601_utc_to_ms(&finding.first_timestamp)
                    .unwrap_or(now_ms),
                ingest_ms: now_ms,
                ingest_idx: self.now_idx,
                trace_id: truncate_to_utf8_boundary(&finding.trace_id, MAX_SAMPLE_TRACE_ID_BYTES)
                    .into(),
                counted_targets: Vec::new(),
            };
            self.record_co_occurrences(&mut incoming, now_ms, &mut refused);
            self.occurrences.push_back(incoming);
        }
        let not_admitted = refused.total();

        // Under admission pressure, free room at batch end so refused
        // newcomers are admitted on the next batch instead of letting
        // early-window noise squat the map for a full window.
        let evicted = if not_admitted > 0 {
            self.enforce_pair_cap()
        } else {
            0
        };
        evicted + not_admitted
    }

    /// Return the shared endpoint `Arc` for `finding` and count one
    /// occurrence of it on the grid.
    fn intern(&mut self, finding: &Finding) -> Arc<CorrelationEndpoint> {
        let grouping = finding.effective_grouping();
        let ep = CorrelationEndpoint {
            finding_type: finding.finding_type.clone(),
            service: finding.service.clone(),
            template: finding.pattern.template.clone(),
            grouping_key: grouping.map(|g| g.key.to_string()),
            grouping_value: grouping.map(|g| g.value.to_string()),
        };
        let endpoint = match self.endpoints.get_key_value(&ep) {
            Some((interned, _)) => Arc::clone(interned),
            None => Arc::new(ep),
        };
        self.endpoints
            .entry(Arc::clone(&endpoint))
            .or_default()
            .add_at(self.now_idx);
        endpoint
    }

    /// Drop endpoints with no count left in the window and no pair or
    /// horizon occurrence pinning them. Runs once per grid step.
    fn prune_endpoints(&mut self) {
        if self.now_idx == self.pruned_idx {
            return;
        }
        self.pruned_idx = self.now_idx;
        let now_idx = self.now_idx;
        self.endpoints
            .retain(|ep, count| count.total(now_idx) > 0 || Arc::strong_count(ep) > 1);
    }

    /// Drop occurrences past the pairing horizon, in ingest order.
    fn evict_stale(&mut self, now_ms: u64) {
        let reach = self
            .config
            .lag_threshold_ms
            .saturating_add(self.config.ingest_skew_ms);
        while self
            .occurrences
            .front()
            .is_some_and(|front| front.ingest_ms.saturating_add(reach) < now_ms)
        {
            self.occurrences.pop_front();
        }
    }

    /// Pair `incoming` with every horizon occurrence within
    /// `lag_threshold_ms` of event time and count the matches.
    ///
    /// The earlier event is the source (ties keep arrival order). One
    /// source occurrence counts once per pair, tracked on the source's
    /// `counted_targets`, so the count does not depend on arrival order.
    /// Pairs refused at the `max_tracked_pairs` cap go to `refused`
    /// instead of being stored: this admission control is what bounds
    /// intra-batch growth on wide topologies.
    fn record_co_occurrences(
        &mut self,
        incoming: &mut FindingOccurrence,
        now_ms: u64,
        refused: &mut RefusedPairs,
    ) {
        let lag_threshold_ms = self.config.lag_threshold_ms;
        let max_tracked_pairs = self.config.max_tracked_pairs;
        // ponytail: linear scan over the horizon; index by event-time slot if trace_ttl_ms reaches tens of minutes
        for occ in &mut self.occurrences {
            let delta = occ.event_ms.abs_diff(incoming.event_ms);
            if delta > lag_threshold_ms || !pairable(&occ.endpoint, &incoming.endpoint) {
                continue;
            }
            let (source, target) = if incoming.event_ms < occ.event_ms {
                (&mut *incoming, &*occ)
            } else {
                (occ, &*incoming)
            };
            let key = PairKey {
                source: Arc::clone(&source.endpoint),
                target: Arc::clone(&target.endpoint),
            };
            // Single-hash admission: read the length before `entry` so
            // the vacant arm can refuse without a second lookup.
            let len = self.pair_counts.len();
            let state = match self.pair_counts.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    if len >= max_tracked_pairs {
                        refused.record(v.into_key());
                        continue;
                    }
                    v.insert(PairState::new(now_ms, &source.endpoint, &target.endpoint))
                }
            };
            state.last_seen_ms = now_ms;
            state.update_sample_trace_ids(&source.trace_id, &target.trace_id);
            if source
                .counted_targets
                .iter()
                .any(|t| Arc::ptr_eq(t, &target.endpoint))
            {
                continue;
            }
            source.counted_targets.push(Arc::clone(&target.endpoint));
            // Same bucket as the source's total, so confidence compares like with like.
            state.co.add_at(source.ingest_idx);
            // Exact below 2^53 ms, far beyond any lag threshold.
            #[allow(clippy::cast_precision_loss)]
            let lag = delta as f64;
            state.record_lag(lag);
        }
    }

    /// Evict pairs down to 90% of `max_tracked_pairs`, lowest windowed
    /// co-occurrence count first, then stalest, and return how many were
    /// removed. Called when a batch hit admission refusals at the cap.
    ///
    /// Evicting down to 90% in one pass amortizes the O(n) work over
    /// 10% of churn, and the threshold comes from `select_nth_unstable`
    /// on a `Vec<(u32, u64)>` so only the ~10% of keys actually removed
    /// pay the `PairKey` clone cost.
    fn enforce_pair_cap(&mut self) -> usize {
        // The emptiness guard matters for `max_tracked_pairs = 0`
        // (admitted by config): the map stays empty while admission
        // refuses everything, and `select_nth_unstable` below would
        // panic on an empty slice.
        if self.pair_counts.is_empty() || self.pair_counts.len() < self.config.max_tracked_pairs {
            return 0;
        }
        let cap = self.config.max_tracked_pairs;
        let target = cap - cap / 10;
        let to_remove = self.pair_counts.len().saturating_sub(target).max(1);

        let now_idx = self.now_idx;
        let rank = |state: &PairState| (state.co.total(now_idx), state.last_seen_ms);
        let mut ranks: Vec<(u32, u64)> = self.pair_counts.values().map(rank).collect();
        // The (to_remove - 1)-th smallest: at least `to_remove` pairs
        // rank at or below it.
        let threshold = *ranks.select_nth_unstable(to_remove - 1).1;

        // Everything strictly below the threshold, then threshold ties
        // up to exactly `to_remove`. Only these keys pay the clone cost.
        let mut doomed: Vec<PairKey> = self
            .pair_counts
            .iter()
            .filter(|(_, v)| rank(v) < threshold)
            .map(|(k, _)| k.clone())
            .collect();
        if doomed.len() < to_remove {
            let extra_needed = to_remove - doomed.len();
            doomed.extend(
                self.pair_counts
                    .iter()
                    .filter(|(_, v)| rank(v) == threshold)
                    .take(extra_needed)
                    .map(|(k, _)| k.clone()),
            );
        }
        let mut removed = 0;
        for key in doomed {
            if self.pair_counts.remove(&key).is_some() {
                removed += 1;
            }
        }
        removed
    }

    /// Return all active correlations above the configured thresholds.
    #[must_use]
    pub fn active_correlations(&self) -> Vec<CrossTraceCorrelation> {
        self.pair_counts
            .iter()
            .filter_map(|(key, state)| {
                let co_occurrences = state.co.total(self.now_idx);
                if co_occurrences < self.config.min_co_occurrences {
                    return None;
                }
                // A source with no count in the window has nothing to
                // measure the pair against.
                let source_total = self
                    .endpoints
                    .get(key.source.as_ref())
                    .map(|count| count.total(self.now_idx))
                    .filter(|&total| total > 0)?;
                // Defensive: per bucket a pair never outcounts its source.
                let confidence = (f64::from(co_occurrences) / f64::from(source_total)).min(1.0);
                if confidence < self.config.min_confidence {
                    return None;
                }
                Some(CrossTraceCorrelation {
                    source: (*key.source).clone(),
                    target: (*key.target).clone(),
                    co_occurrence_count: co_occurrences,
                    source_total_occurrences: source_total,
                    confidence,
                    median_lag_ms: median(&state.lags_ms),
                    first_seen: crate::time::millis_to_iso8601(state.first_seen_ms),
                    last_seen: crate::time::millis_to_iso8601(state.last_seen_ms),
                    sample_trace_id: state.last_trace_id.clone(),
                    source_sample_trace_id: state.last_source_trace_id.clone(),
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
/// `MAX_LAG_SAMPLES = 64` f64 (512 B per call), which is acceptable
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
        // Regression: one batch of findings from MANY distinct services
        // used to insert every cross-service pair before the batch-end
        // eviction ran, exploding the map (and the process RSS) inside
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
        // co-occurrence first) so the NEXT batch admits new pairs,
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
        // The whole point of the type: a wide topology walks the cross
        // product of the batch and the horizon, so the set has to stop
        // growing while the figure it feeds stays truthful.
        let mut refused = RefusedPairs::default();
        let extra = 500;
        // Distinct keys only up to the ceiling, then the same key again:
        // dedup below and the documented degradation above are the two
        // halves of what this type does.
        let key_for = |i: usize| PairKey {
            source: std::sync::Arc::new(CorrelationEndpoint {
                finding_type: FindingType::NPlusOneSql,
                service: format!("svc-{i}"),
                template: "tpl".to_string(),
                grouping_key: None,
                grouping_value: None,
            }),
            target: std::sync::Arc::new(CorrelationEndpoint {
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
        // The shape that produced "conf 150%" in a live report: one source
        // occurrence followed by several targets inside the lag window
        // used to score one co-occurrence per (source, target) couple.
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
        // Lifetime counting made every mature pair report exactly 1.0:
        // the count grew forever while the denominator only spanned the
        // window. With window-scoped counts, a pair co-occurring on half
        // its source occurrences stays near 0.5 however long it lives.
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

        // Fire the same A -> B pair 10x MAX_LAG_SAMPLES times.
        // Without the reservoir, lags_ms would grow to ~640 entries.
        let total = MAX_LAG_SAMPLES * 10;
        for i in 0..total {
            let t = 1_000_000 + i as u64 * 10;
            let fa = make_finding("order-svc", FindingType::NPlusOneSql, "SELECT 1");
            let _ = ingest_at(&mut correlator, &[fa], t);
            let fb = make_finding("payment-svc", FindingType::PoolSaturation, "payment-svc");
            let _ = ingest_at(&mut correlator, &[fb], t + 1);
        }

        // Directional pairs: A -> B, and B -> A with the next round's A
        // (10 ms later). Both directions should have bounded reservoirs.
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
        let expected =
            f64::from(pair.co_occurrence_count) / f64::from(pair.source_total_occurrences);
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
        let expected =
            f64::from(pair.co_occurrence_count) / f64::from(pair.source_total_occurrences);
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
}
