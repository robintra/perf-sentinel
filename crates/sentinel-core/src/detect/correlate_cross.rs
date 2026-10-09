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
/// Each refused pair counts once per batch, however many horizon
/// occurrences matched it, but the set has to be bounded. A batch on a
/// wide topology walks the cross product of the incoming findings and
/// the horizon, which reaches millions of distinct keys and a table far
/// larger than the map they were refused from. Past the ceiling the
/// count degrades to occurrences, which overstates rather than hides.
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, serde::Deserialize)]
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
    /// this pair (the trailing finding in the source-to-target order).
    /// Lets the dashboard jump from a correlation row to Explain and
    /// render a representative tree. `None` in batch mode and for
    /// replayed baselines that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_trace_id: Option<String>,
    /// Trace id of the source-side finding of the most recent
    /// co-occurrence (the leading finding in the source-to-target order).
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

    /// Count one at grid index `idx`. Indices older than the previous bucket are dropped.
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
    /// Ingest time of the latest match. Drives the window TTL.
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
    /// dependency). A biased draw freezes the reservoir (see the
    /// `reservoir_continues_to_sample_after_many_observations` test).
    fn record_lag(&mut self, lag_ms: f64) {
        self.total_observations = self.total_observations.saturating_add(1);
        if self.lags_ms.len() < MAX_LAG_SAMPLES {
            self.lags_ms.push(lag_ms);
            return;
        }
        // Algorithm R: draw r uniform in `[0, n)`. When `r < k`, use r
        // itself as the slot index. This is unbiased because, conditional
        // on `r < k`, `r` is uniform in `[0, k)`, the uniform slot we
        // need. Saves a second PRNG draw versus sampling the slot
        // independently.
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
/// (and median lags) differ across runs on the same replayed input.
/// Users rely on that determinism when debugging by replay.
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
    /// per occurrence above it. A pair refused again on a later batch
    /// counts again, so the lifetime counter reads as "pair-batches lost".
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
    /// instead of being stored. This admission control bounds
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
    /// on a `Vec<(u32, u64)>` so only the ~10% of keys that are removed
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
            // Threshold ties in pair order, so the same pairs go whatever
            // the map's iteration order.
            let mut tied: Vec<PairKey> = self
                .pair_counts
                .iter()
                .filter(|(_, v)| rank(v) == threshold)
                .map(|(k, _)| k.clone())
                .collect();
            tied.sort_unstable_by(|a, b| {
                a.source
                    .cmp(&b.source)
                    .then_with(|| a.target.cmp(&b.target))
            });
            tied.truncate(extra_needed);
            doomed.append(&mut tied);
        }
        let mut removed = 0;
        for key in doomed {
            if self.pair_counts.remove(&key).is_some() {
                removed += 1;
            }
        }
        removed
    }

    /// Return all active correlations above the configured thresholds,
    /// by confidence then co-occurrence count, both descending, then by
    /// source and target, so the order does not follow the pair map.
    #[must_use]
    pub fn active_correlations(&self) -> Vec<CrossTraceCorrelation> {
        let mut correlations: Vec<CrossTraceCorrelation> = self
            .pair_counts
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
            .collect();
        // `total_cmp` orders a NaN confidence deterministically, last.
        correlations.sort_unstable_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then_with(|| b.co_occurrence_count.cmp(&a.co_occurrence_count))
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.target.cmp(&b.target))
        });
        correlations
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
mod tests;
