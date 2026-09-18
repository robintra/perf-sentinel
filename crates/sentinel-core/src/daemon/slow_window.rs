//! Cross-batch slow window: counts slow episodes of one template across
//! analysis batches, so a template slow in `slow_min_occurrences` separate
//! episodes within the window yields a slow finding even when no single
//! batch holds that many of its spans.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::correlate::Trace;
use crate::detect::{Finding, FindingType};
use crate::event::{EventType, SpanEvent};

/// (event type, service, grouping key and value, normalized template).
type Key = (EventType, Arc<str>, Option<(Arc<str>, Arc<str>)>, Arc<str>);

/// Partition of a batch slow finding: service is not part of it.
type Triple<'a> = (FindingType, &'a str, Option<(&'a str, &'a str)>);

/// Cap on tracked keys, new keys past it are refused and counted.
pub(super) const MAX_SLOW_WINDOW_KEYS: usize = 1024;

struct Episode {
    start_ms: u64,
    /// Slowest span of the episode, `target` cleared to drop the raw query.
    worst: SpanEvent,
}

#[derive(Default)]
struct Entry {
    episodes: VecDeque<Episode>,
    reported_at_ms: Option<u64>,
}

/// Worker-owned state, fed once per analysis batch in FIFO order.
pub(super) struct SlowWindowTracker {
    entries: HashMap<Key, Entry>,
    window_ms: u64,
    episode_ms: u64,
    threshold_us: u64,
    min_occ: usize,
}

impl SlowWindowTracker {
    pub(super) fn new(
        window_ms: u64,
        episode_ms: u64,
        threshold_ms: u64,
        min_occurrences: u32,
    ) -> Self {
        let episode_ms = episode_ms.max(1);
        if u64::from(min_occurrences.saturating_sub(1)).saturating_mul(episode_ms) >= window_ms {
            tracing::warn!(
                window_ms,
                episode_ms,
                min_occurrences,
                "[detection] slow_query_window_minutes cannot hold \
                 slow_query_min_occurrences slow episodes, the cross-batch \
                 slow window never fires"
            );
        }
        Self {
            entries: HashMap::new(),
            window_ms,
            episode_ms,
            threshold_us: threshold_ms.saturating_mul(1000),
            min_occ: min_occurrences as usize,
        }
    }

    /// Feed one batch, return the windowed findings and the refused spans.
    pub(super) fn observe(
        &mut self,
        traces: &[Trace],
        batch_findings: &[Finding],
        now_ms: u64,
    ) -> (Vec<Finding>, usize) {
        let suppressed = suppressed_triples(batch_findings);
        let refused = self.feed(traces, &suppressed, now_ms);
        (self.sweep(now_ms), refused)
    }

    fn feed(&mut self, traces: &[Trace], suppressed: &HashSet<Triple<'_>>, now_ms: u64) -> usize {
        let mut refused = 0;
        for span in traces.iter().flat_map(|t| &t.spans) {
            let ev = &span.event;
            if ev.duration_us <= self.threshold_us {
                continue;
            }
            let key: Key = (
                ev.event_type.clone(),
                ev.service.clone(),
                ev.effective_grouping()
                    .map(|g| (g.key.clone(), g.value.clone())),
                span.template.clone(),
            );
            let triple = (
                FindingType::from_event_type_slow(&ev.event_type),
                &*span.template,
                ev.grouping_identity(),
            );
            if suppressed.contains(&triple) {
                // Already in a batch finding: count it as a report, not an episode.
                if let Some(entry) = self.entries.get_mut(&key) {
                    entry.episodes.clear();
                    entry.reported_at_ms = Some(now_ms);
                }
                continue;
            }
            if !self.entries.contains_key(&key) && self.entries.len() >= MAX_SLOW_WINDOW_KEYS {
                refused += 1;
                continue;
            }
            let entry = self.entries.entry(key).or_default();
            record(&mut entry.episodes, ev, now_ms, self.episode_ms);
        }
        refused
    }

    fn sweep(&mut self, now_ms: u64) -> Vec<Finding> {
        let (window_ms, min_occ, threshold_us) = (self.window_ms, self.min_occ, self.threshold_us);
        let mut emitted = Vec::new();
        self.entries.retain(|key, entry| {
            while entry
                .episodes
                .front()
                .is_some_and(|e| e.start_ms.saturating_add(window_ms) <= now_ms)
            {
                entry.episodes.pop_front();
            }
            let in_cooldown = entry
                .reported_at_ms
                .is_some_and(|t| now_ms < t.saturating_add(window_ms));
            if !in_cooldown {
                entry.reported_at_ms = None;
            }
            let opened_now = entry.episodes.back().is_some_and(|e| e.start_ms == now_ms);
            if opened_now
                && !in_cooldown
                && entry.episodes.len() >= min_occ
                && let Some(finding) = build_finding(key, &entry.episodes, min_occ, threshold_us)
            {
                emitted.push(finding);
                entry.episodes.clear();
                entry.reported_at_ms = Some(now_ms);
            }
            !entry.episodes.is_empty() || entry.reported_at_ms.is_some()
        });
        emitted
    }
}

fn suppressed_triples(findings: &[Finding]) -> HashSet<Triple<'_>> {
    findings
        .iter()
        .filter(|f| {
            matches!(
                f.finding_type,
                FindingType::SlowSql | FindingType::SlowHttp | FindingType::SlowMessaging
            )
        })
        .map(|f| {
            (
                f.finding_type.clone(),
                f.pattern.template.as_str(),
                f.grouping_identity(),
            )
        })
        .collect()
}

/// Merge into the open episode, or open a new one.
fn record(episodes: &mut VecDeque<Episode>, ev: &SpanEvent, now_ms: u64, episode_ms: u64) {
    match episodes.back_mut() {
        Some(last) if now_ms.saturating_sub(last.start_ms) < episode_ms => {
            if ev.duration_us > last.worst.duration_us {
                last.worst = without_target(ev);
            }
        }
        _ => episodes.push_back(Episode {
            start_ms: now_ms,
            worst: without_target(ev),
        }),
    }
}

fn without_target(ev: &SpanEvent) -> SpanEvent {
    SpanEvent {
        target: String::new(),
        ..ev.clone()
    }
}

fn build_finding(
    key: &Key,
    episodes: &VecDeque<Episode>,
    min_occ: usize,
    threshold_us: u64,
) -> Option<Finding> {
    let entries: Vec<(u64, &str, &str, &SpanEvent)> = episodes
        .iter()
        .map(|e| {
            (
                e.worst.duration_us,
                e.worst.trace_id.as_str(),
                e.worst.timestamp.as_str(),
                &e.worst,
            )
        })
        .collect();
    crate::detect::slow::build_cross_trace_finding(&key.0, &key.3, &entries, min_occ, threshold_us)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize;
    use crate::test_helpers::make_sql_event_with_duration;

    const MIN: u64 = 60_000;
    const T0: u64 = 1_000_000_000;

    fn tracker() -> SlowWindowTracker {
        SlowWindowTracker::new(15 * MIN, MIN, 500, 3)
    }

    fn slow(trace_id: &str, service: &str, ts: &str, duration_us: u64) -> Trace {
        let mut ev = make_sql_event_with_duration(
            trace_id,
            "s1",
            "SELECT * FROM orders WHERE id = 42",
            ts,
            duration_us,
        );
        ev.service = Arc::from(service);
        Trace {
            trace_id: trace_id.to_string(),
            spans: vec![normalize::normalize(ev)],
        }
    }

    fn slow_a(trace_id: &str) -> Trace {
        slow(trace_id, "svc-a", "2025-07-10T14:32:01.000Z", 600_000)
    }

    fn emitted(t: &mut SlowWindowTracker, traces: &[Trace], now_ms: u64) -> Vec<Finding> {
        t.observe(traces, &[], now_ms).0
    }

    fn episode_counts(t: &SlowWindowTracker) -> Vec<usize> {
        t.entries.values().map(|e| e.episodes.len()).collect()
    }

    #[test]
    fn isolated_slow_span_never_emits() {
        let mut win = tracker();
        for i in 0..4 {
            let id = format!("lock-{i}");
            assert!(emitted(&mut win, &[slow_a(&id)], T0 + i * 15_000).is_empty());
        }
        for k in 1..=4 {
            let id = format!("iso-{k}");
            assert!(emitted(&mut win, &[slow_a(&id)], T0 + k * 20 * MIN).is_empty());
        }
    }

    #[test]
    fn lock_burst_across_consecutive_ticks_is_one_episode() {
        let mut win = tracker();
        assert!(emitted(&mut win, &[slow_a("a"), slow_a("b")], T0).is_empty());
        assert!(emitted(&mut win, &[slow_a("c")], T0 + 15_000).is_empty());
        assert!(emitted(&mut win, &[slow_a("d")], T0 + 45_000).is_empty());
        assert_eq!(episode_counts(&win), vec![1]);
    }

    fn three_episodes() -> [Trace; 3] {
        [
            slow("a", "svc-a", "2025-07-10T14:32:01.000Z", 600_000),
            slow("b", "svc-a", "2025-07-10T14:34:01.000Z", 900_000),
            slow("c", "svc-a", "2025-07-10T14:37:01.000Z", 700_000),
        ]
    }

    #[test]
    fn recurring_template_emits_once() {
        let mut win = tracker();
        let [a, b, c] = three_episodes();
        assert!(emitted(&mut win, &[a], T0).is_empty());
        assert!(emitted(&mut win, &[b], T0 + 2 * MIN).is_empty());
        let findings = emitted(&mut win, &[c], T0 + 5 * MIN);
        assert_eq!(findings.len(), 1);
        let f0 = &findings[0];
        assert_eq!(f0.finding_type, FindingType::SlowSql);
        assert_eq!(f0.pattern.occurrences, 3);
        assert_eq!(f0.first_timestamp, "2025-07-10T14:32:01.000Z");
        assert_eq!(f0.last_timestamp, "2025-07-10T14:37:01.000Z");
        assert_eq!(f0.trace_id, "b");
    }

    #[test]
    fn emitted_finding_matches_batch_cross_trace_shape() {
        let mut win = tracker();
        let [a, b, c] = three_episodes();
        emitted(&mut win, std::slice::from_ref(&a), T0);
        emitted(&mut win, std::slice::from_ref(&b), T0 + 2 * MIN);
        let mut windowed = emitted(&mut win, std::slice::from_ref(&c), T0 + 5 * MIN);
        let mut batch = crate::detect::slow::detect_slow_cross_trace(&[a, b, c], 500, 3);
        crate::acknowledgments::enrich_with_signatures(&mut windowed);
        crate::acknowledgments::enrich_with_signatures(&mut batch);
        assert_eq!(windowed.len(), 1);
        assert!(!windowed[0].signature.is_empty());
        assert_eq!(windowed, batch);
    }

    #[test]
    fn episodes_expire_outside_window() {
        let mut win = tracker();
        assert!(emitted(&mut win, &[slow_a("a")], T0).is_empty());
        assert!(emitted(&mut win, &[slow_a("b")], T0 + 8 * MIN).is_empty());
        assert!(emitted(&mut win, &[slow_a("c")], T0 + 16 * MIN).is_empty());
        assert_eq!(episode_counts(&win), vec![2]);
    }

    #[test]
    fn same_batch_spans_after_emission_do_not_open_new_episode() {
        let mut win = tracker();
        emitted(&mut win, &[slow_a("a")], T0);
        emitted(&mut win, &[slow_a("b")], T0 + 2 * MIN);
        let findings = emitted(&mut win, &[slow_a("c"), slow_a("d")], T0 + 5 * MIN);
        assert_eq!(findings.len(), 1);
        assert_eq!(episode_counts(&win), vec![0]);
    }

    #[test]
    fn cooldown_then_reports_persisting_problem() {
        let mut win = tracker();
        emitted(&mut win, &[slow_a("a")], T0);
        emitted(&mut win, &[slow_a("b")], T0 + 2 * MIN);
        assert_eq!(emitted(&mut win, &[slow_a("c")], T0 + 5 * MIN).len(), 1);
        // Cooldown runs until T0 + 20 min.
        for (i, m) in [7, 9, 12].into_iter().enumerate() {
            let id = format!("cd-{i}");
            assert!(emitted(&mut win, &[slow_a(&id)], T0 + m * MIN).is_empty());
        }
        // Three episodes in the window, but no span of the key.
        assert!(emitted(&mut win, &[], T0 + 21 * MIN).is_empty());
        // The 7 min episode expires, 9, 12 and 22 remain.
        assert_eq!(emitted(&mut win, &[slow_a("e")], T0 + 22 * MIN).len(), 1);
    }

    #[test]
    fn batch_slow_finding_suppresses_and_resets() {
        let mut win = tracker();
        let b_span = |id: &str| slow(id, "svc-b", "2025-07-10T14:32:01.000Z", 600_000);
        emitted(&mut win, &[slow_a("a0"), b_span("b0")], T0);
        let batch = [slow_a("a1"), slow_a("a2"), b_span("b1")];
        let batch_findings = crate::detect::slow::detect_slow_cross_trace(&batch, 500, 3);
        assert_eq!(batch_findings.len(), 1);
        let (findings, _) = win.observe(&batch, &batch_findings, T0 + 2 * MIN);
        assert!(findings.is_empty());
        assert_eq!(win.entries.len(), 2);
        for entry in win.entries.values() {
            assert!(entry.episodes.is_empty());
            assert_eq!(entry.reported_at_ms, Some(T0 + 2 * MIN));
        }
    }

    #[test]
    fn services_are_not_merged() {
        let mut win = tracker();
        let ts = "2025-07-10T14:32:01.000Z";
        assert!(emitted(&mut win, &[slow("a", "svc-a", ts, 600_000)], T0).is_empty());
        assert!(emitted(&mut win, &[slow("b", "svc-b", ts, 600_000)], T0 + 2 * MIN).is_empty());
        assert!(emitted(&mut win, &[slow("c", "svc-c", ts, 600_000)], T0 + 5 * MIN).is_empty());
    }

    #[test]
    fn single_trace_id_does_not_emit() {
        let mut win = tracker();
        for m in [0, 2, 5] {
            assert!(emitted(&mut win, &[slow_a("same")], T0 + m * MIN).is_empty());
        }
        assert_eq!(episode_counts(&win), vec![3]);
    }

    #[test]
    fn clock_step_back_does_not_panic() {
        let mut win = tracker();
        for now in [T0 + 10 * MIN, T0, T0 + MIN, 0, T0 + 3 * MIN] {
            assert!(emitted(&mut win, &[slow_a("a")], now).is_empty());
        }
    }

    #[test]
    fn key_cap_refuses_new_keys() {
        let mut win = tracker();
        let table = |name: &str, id: &str| {
            let ev = make_sql_event_with_duration(
                id,
                "s1",
                &format!("SELECT * FROM {name} WHERE id = 1"),
                "2025-07-10T14:32:01.000Z",
                600_000,
            );
            Trace {
                trace_id: id.to_string(),
                spans: vec![normalize::normalize(ev)],
            }
        };
        let fill: Vec<Trace> = (0..MAX_SLOW_WINDOW_KEYS)
            .map(|i| table(&format!("t{i}"), &format!("f{i}")))
            .collect();
        assert_eq!(win.observe(&fill, &[], T0).1, 0);
        let (_, refused) = win.observe(&[table("t0", "x"), table("extra", "y")], &[], T0 + 2 * MIN);
        assert_eq!(refused, 1);
        assert_eq!(win.entries.len(), MAX_SLOW_WINDOW_KEYS);
        let t0_episodes = win
            .entries
            .iter()
            .find(|(k, _)| k.3.contains(" t0 "))
            .map(|(_, e)| e.episodes.len());
        assert_eq!(t0_episodes, Some(2));
    }
}
