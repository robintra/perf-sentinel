//! Trace-level sampling for the daemon event loop.
//!
//! Each batch of `SpanEvent`s is filtered by a hashed keep/drop decision
//! keyed on `trace_id` so that every event sharing a trace inherits the
//! same verdict.

use crate::event::SpanEvent;

/// Threshold above which the trace-id decision cache uses a `HashMap`.
/// Below this batch size, a linear `Vec` scan beats the `HashMap`
/// setup cost (sub-microsecond either way, but no heap allocation for
/// the cache backing on the small-batch path).
const SAMPLING_HASHMAP_THRESHOLD: usize = 16;

/// Apply trace-level sampling: cache decisions per `trace_id` to avoid
/// redundant hashing for events sharing a trace.
///
/// The cache is keyed on the u64 FNV-1a hash of the `trace_id` rather
/// than on a `String` clone, so a burst of 100k events with 10k
/// distinct traces incurs zero heap allocations for the cache keys.
/// Hash collisions are harmless: a collision only means two different
/// traces share the same keep/drop decision, which is the same
/// statistical behavior as rolling the dice independently.
///
/// Allocation behavior:
/// - `rate >= 1.0`: zero allocations, the input `Vec` is returned as-is.
/// - Batches `<= SAMPLING_HASHMAP_THRESHOLD` events: linear scan in a
///   stack-sized `Vec`, no `HashMap` allocation.
/// - Larger batches: a `HashMap` pre-sized to a quarter of the batch
///   length to absorb the typical 4:1 events-per-trace ratio without
///   growth reallocations.
pub(super) fn apply_sampling(events: Vec<SpanEvent>, rate: f64) -> Vec<SpanEvent> {
    if rate >= 1.0 {
        return events;
    }
    if events.len() <= SAMPLING_HASHMAP_THRESHOLD {
        // Tiny batches: linear-scan cache backed by a stack array so
        // we avoid heap allocation for the cache entirely. Each lookup
        // is at most `SAMPLING_HASHMAP_THRESHOLD` u64 comparisons, a
        // few cycles per event.
        let mut cache: [(u64, bool); SAMPLING_HASHMAP_THRESHOLD] =
            [(0_u64, false); SAMPLING_HASHMAP_THRESHOLD];
        let mut cache_len: usize = 0;
        events
            .into_iter()
            .filter(|e| {
                let h = hash_trace_id(&e.trace_id);
                if let Some(&(_, decision)) = cache[..cache_len].iter().find(|(k, _)| *k == h) {
                    return decision;
                }
                let decision = hash_to_decision(h, rate);
                // Safe: at most `SAMPLING_HASHMAP_THRESHOLD` distinct
                // trace_ids fit in a batch of the same size.
                if cache_len < SAMPLING_HASHMAP_THRESHOLD {
                    cache[cache_len] = (h, decision);
                    cache_len += 1;
                }
                decision
            })
            .collect()
    } else {
        // Pre-size the HashMap to the typical 4:1 events-per-trace
        // ratio so growth reallocations don't show up in the hot path.
        let mut cache = std::collections::HashMap::<u64, bool>::with_capacity(events.len() / 4);
        events
            .into_iter()
            .filter(|e| {
                let h = hash_trace_id(&e.trace_id);
                if let Some(&decision) = cache.get(&h) {
                    return decision;
                }
                let decision = hash_to_decision(h, rate);
                cache.insert(h, decision);
                decision
            })
            .collect()
    }
}

/// FNV-1a 64-bit hash of a `trace_id`. Extracted from `should_sample` so
/// it can be called once per event in `apply_sampling` and reused as
/// both the cache key and the sampling decision input.
#[inline]
fn hash_trace_id(trace_id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in trace_id.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Map a precomputed trace hash to a keep/drop decision.
#[inline]
#[allow(clippy::cast_precision_loss)] // rate comparison is approximate by design
fn hash_to_decision(hash: u64, rate: f64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    (hash as f64 / u64::MAX as f64) < rate
}

/// Deterministic per-trace sampling used by the unit tests. Production
/// code goes through [`hash_trace_id`] + [`hash_to_decision`] directly
/// in `apply_sampling` to avoid rehashing when the cache is consulted.
#[cfg(test)]
fn should_sample(trace_id: &str, rate: f64) -> bool {
    hash_to_decision(hash_trace_id(trace_id), rate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventSource, EventType, SpanEvent};

    fn make_event(trace_id: &str) -> SpanEvent {
        SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: "s1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: "SELECT 1".to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
        }
    }

    #[test]
    fn should_sample_deterministic() {
        // Same trace_id always produces the same result
        let r1 = should_sample("trace-abc-123", 0.5);
        let r2 = should_sample("trace-abc-123", 0.5);
        assert_eq!(r1, r2);
    }

    #[test]
    fn should_sample_rate_zero_drops_all() {
        assert!(!should_sample("any-trace", 0.0));
        assert!(!should_sample("another-trace", 0.0));
    }

    #[test]
    fn should_sample_rate_one_keeps_all() {
        assert!(should_sample("any-trace", 1.0));
        assert!(should_sample("another-trace", 1.0));
    }

    #[test]
    fn should_sample_rate_half_splits() {
        // With enough distinct trace IDs, roughly half should be sampled
        let sampled = (0..1000)
            .filter(|i| should_sample(&format!("trace-{i}"), 0.5))
            .count();
        // Allow wide margin: between 30% and 70%
        assert!(
            (300..=700).contains(&sampled),
            "expected ~500 sampled, got {sampled}"
        );
    }

    #[test]
    fn apply_sampling_full_rate_returns_all() {
        let events = vec![make_event("t1"), make_event("t2"), make_event("t3")];
        let sampled = apply_sampling(events, 1.0);
        assert_eq!(sampled.len(), 3);
    }

    #[test]
    fn apply_sampling_zero_rate_drops_all() {
        let events = vec![make_event("t1"), make_event("t2")];
        let sampled = apply_sampling(events, 0.0);
        assert!(sampled.is_empty());
    }

    #[test]
    fn apply_sampling_same_trace_id_cached_decision() {
        // The per-trace sampling cache must guarantee that every event
        // sharing a trace_id gets the same keep/drop verdict. At rate
        // 1.0 apply_sampling short-circuits to "keep all" before
        // touching the cache, so we also test a partial rate where the
        // cache-hit branch is actually exercised.
        let events = vec![
            make_event("same-trace"),
            make_event("same-trace"),
            make_event("same-trace"),
            make_event("same-trace"),
        ];
        let sampled = apply_sampling(events, 1.0);
        assert_eq!(
            sampled.len(),
            4,
            "rate 1.0 must keep every event regardless of trace_id"
        );

        // Partial rate: all three events share a trace_id, so the
        // cache forces a single decision. Acceptable outcomes are
        // 0 (all dropped) or 3 (all kept). Anything in between would
        // mean the cache lost the decision, which is exactly the
        // invariant this test is guarding.
        let events2 = vec![
            make_event("cached-trace"),
            make_event("cached-trace"),
            make_event("cached-trace"),
        ];
        let sampled2 = apply_sampling(events2, 0.5);
        assert!(
            sampled2.is_empty() || sampled2.len() == 3,
            "all events for the same trace_id must share the cached \
             decision, got {} of 3 kept (expected 0 or 3)",
            sampled2.len()
        );
    }

    #[test]
    fn apply_sampling_mixed_trace_ids_with_partial_rate() {
        // Sanity test: with 100 distinct trace IDs at rate 0.5, roughly
        // half go through. Exercises the cache-miss + `should_sample`
        // path in apply_sampling.
        let events: Vec<_> = (0..100)
            .map(|i| make_event(&format!("trace-{i}")))
            .collect();
        let sampled = apply_sampling(events, 0.5);
        assert!(
            (10..=90).contains(&sampled.len()),
            "expected ~50 sampled, got {}",
            sampled.len()
        );
    }
}
