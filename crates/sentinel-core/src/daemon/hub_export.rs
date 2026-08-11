use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use chrono::Utc;
use serde::Serialize;

use super::ack::{AckEntry, AckStore};
use super::findings_store::StoredFinding;
use super::query_api::{FindingResponse, ResolvedTomlAck, lookup_ack};
use crate::config::DaemonHubExportConfig;
use crate::detect::Finding;
use crate::http_client;
use crate::report::metrics::MetricsState;

/// The acknowledgment sources the query API reads, shared with the exporter so
/// a pushed envelope carries the same `acknowledged_by` as a polled one.
struct AckSources {
    toml: Arc<HashMap<String, ResolvedTomlAck>>,
    store: Option<Arc<AckStore>>,
}

impl AckSources {
    async fn snapshot(&self) -> Arc<HashMap<String, AckEntry>> {
        match &self.store {
            Some(store) => store.snapshot_active().await,
            None => Arc::new(HashMap::new()),
        }
    }
}

const MAX_EXPORT_BODY_BYTES: usize = 2 * 1024 * 1024;
const EXPORT_REFRESH_INTERVAL_MS: u64 = 60 * 60 * 1_000;
/// Backoff ceiling for a busy Hub: `retry_delay(3, _)` stays within a few
/// seconds, matching the `Retry-After: 1` the Hub sends with its 503.
const BUSY_FAILURE_CAP: u32 = 3;

#[derive(Clone)]
struct PendingExport {
    finding: StoredFinding,
    revision: u64,
}

struct SentExport {
    sent_at_ms: u64,
    severity: crate::detect::Severity,
    revision: u64,
}

#[derive(Default)]
struct PendingState {
    entries: HashMap<String, PendingExport>,
    order: VecDeque<(String, u64)>,
    sent_entries: HashMap<String, SentExport>,
    sent_order: VecDeque<(String, u64)>,
    next_revision: u64,
}

pub(super) struct HubExportBuffer {
    inner: Mutex<PendingState>,
    max_pending: usize,
}

pub(super) struct HubExporter {
    buffer: Arc<HubExportBuffer>,
    handle: tokio::task::JoinHandle<()>,
}

impl HubExporter {
    pub(super) fn spawn(
        config: &DaemonHubExportConfig,
        metrics: Arc<MetricsState>,
        toml_acks: Arc<HashMap<String, ResolvedTomlAck>>,
        ack_store: Option<Arc<AckStore>>,
    ) -> Result<Option<Self>, super::DaemonError> {
        if !config.enabled {
            return Ok(None);
        }
        let key_path = config.api_key_file.as_deref().expect("config validation");
        let api_key = std::fs::read_to_string(key_path).map_err(|source| {
            super::DaemonError::HubExportSecretRead {
                path: key_path.to_string(),
                source,
            }
        })?;
        let api_key = api_key.trim().to_string();
        if api_key.len() < 32 || api_key.chars().any(char::is_control) {
            return Err(super::DaemonError::HubExportSecretInvalid {
                path: key_path.to_string(),
            });
        }
        let endpoint = format!(
            "{}?source_id={}",
            config.endpoint.as_deref().expect("config validation"),
            config.source_id.as_deref().expect("config validation"),
        );
        let uri = endpoint
            .parse::<http_client::Uri>()
            .map_err(|_| super::DaemonError::HubExportEndpoint)?;
        let buffer = Arc::new(HubExportBuffer::new(config.max_pending));
        let handle = tokio::spawn(run_exporter(
            buffer.clone(),
            metrics,
            uri,
            api_key,
            config.batch_size,
            Duration::from_secs(config.flush_interval_secs),
            AckSources {
                toml: toml_acks,
                store: ack_store,
            },
        ));
        Ok(Some(Self { buffer, handle }))
    }

    pub(super) fn buffer(&self) -> Arc<HubExportBuffer> {
        self.buffer.clone()
    }
}

impl Drop for HubExporter {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl HubExportBuffer {
    pub(super) fn new(max_pending: usize) -> Self {
        Self {
            inner: Mutex::new(PendingState::default()),
            max_pending,
        }
    }

    pub(super) fn push_batch(&self, findings: &[Finding], now_ms: u64) -> u64 {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut dropped = 0;
        for finding in findings {
            if finding.signature.is_empty() {
                dropped += 1;
                continue;
            }
            if state.entries.contains_key(&finding.signature) {
                merge_pending(&mut state, finding, now_ms);
                continue;
            }
            if suppressed_by_recent_send(&state, finding, now_ms) {
                continue;
            }
            insert_pending(&mut state, finding, now_ms);
        }

        dropped + evict_overflow(&mut state, self.max_pending)
    }

    fn snapshot(&self, limit: usize) -> Vec<PendingExport> {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .order
            .iter()
            .filter_map(|(signature, revision)| {
                state
                    .entries
                    .get(signature)
                    .filter(|entry| entry.revision == *revision)
            })
            .take(limit)
            .cloned()
            .collect()
    }

    fn acknowledge(&self, sent: &[PendingExport], acknowledged_at_ms: u64) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for item in sent {
            let signature = &item.finding.finding.signature;
            if state
                .entries
                .get(signature)
                .is_some_and(|entry| entry.revision == item.revision)
            {
                state.entries.remove(signature);
            }
            state.next_revision = state.next_revision.wrapping_add(1);
            let revision = state.next_revision;
            state.sent_entries.insert(
                signature.clone(),
                SentExport {
                    sent_at_ms: acknowledged_at_ms,
                    severity: item.finding.finding.severity.clone(),
                    revision,
                },
            );
            state.sent_order.push_back((signature.clone(), revision));
        }
        while state.sent_entries.len() > self.max_pending {
            let Some((signature, revision)) = state.sent_order.pop_front() else {
                break;
            };
            if state
                .sent_entries
                .get(&signature)
                .is_some_and(|entry| entry.revision == revision)
            {
                state.sent_entries.remove(&signature);
            }
        }
        if state.sent_order.len() > self.max_pending.saturating_mul(2) {
            let mut order = std::mem::take(&mut state.sent_order);
            order.retain(|(signature, revision)| {
                state
                    .sent_entries
                    .get(signature)
                    .is_some_and(|entry| entry.revision == *revision)
            });
            state.sent_order = order;
        }
    }

    pub(super) fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len()
    }
}

/// Fold a repeat of an already-pending signature into its entry.
///
/// `seen_count` and `first_seen_ms` accumulate the way the findings store
/// folds at read time, so a hot finding is not exported as a single
/// detection. The revision only moves when the severity worsens: an equal
/// repeat that lands while the batch is in flight must still be cleared by
/// its acknowledgment, or the exporter re-sends the same batch every round
/// trip instead of honouring the flush interval. The trade is that such a
/// repeat refreshes the payload the acknowledgment then drops, so the Hub
/// keeps the snapshot's trace id, never one newer than the send.
fn merge_pending(state: &mut PendingState, finding: &Finding, now_ms: u64) {
    // Severity is ordered Critical < Warning < Info: strictly smaller is worse.
    let worsened = state
        .entries
        .get(&finding.signature)
        .is_some_and(|pending| finding.severity < pending.finding.finding.severity);
    let revision = if worsened {
        state.next_revision = state.next_revision.wrapping_add(1);
        Some(state.next_revision)
    } else {
        None
    };
    if let Some(pending) = state.entries.get_mut(&finding.signature) {
        pending.finding.seen_count = pending.finding.seen_count.saturating_add(1);
        pending.finding.first_seen_ms = pending.finding.first_seen_ms.min(now_ms);
        pending.finding.stored_at_ms = pending.finding.stored_at_ms.max(now_ms);
        if finding.severity <= pending.finding.finding.severity {
            pending.finding.finding = finding.clone();
        }
        if let Some(revision) = revision {
            pending.revision = revision;
        }
    }
    if let Some(revision) = revision {
        state.order.push_back((finding.signature.clone(), revision));
    }
}

fn suppressed_by_recent_send(state: &PendingState, finding: &Finding, now_ms: u64) -> bool {
    state
        .sent_entries
        .get(&finding.signature)
        .is_some_and(|sent| {
            now_ms.saturating_sub(sent.sent_at_ms) < EXPORT_REFRESH_INTERVAL_MS
                && finding.severity >= sent.severity
        })
}

fn insert_pending(state: &mut PendingState, finding: &Finding, now_ms: u64) {
    state.next_revision = state.next_revision.wrapping_add(1);
    let revision = state.next_revision;
    let signature = finding.signature.clone();
    state.sent_entries.remove(&signature);
    state.entries.insert(
        signature.clone(),
        PendingExport {
            finding: StoredFinding {
                finding: finding.clone(),
                stored_at_ms: now_ms,
                first_seen_ms: now_ms,
                seen_count: 1,
            },
            revision,
        },
    );
    state.order.push_back((signature, revision));
}

fn evict_overflow(state: &mut PendingState, max_pending: usize) -> u64 {
    let mut dropped = 0;
    while state.entries.len() > max_pending {
        let Some((signature, revision)) = state.order.pop_front() else {
            break;
        };
        if state
            .entries
            .get(&signature)
            .is_some_and(|entry| entry.revision == revision)
        {
            state.entries.remove(&signature);
            dropped += 1;
        }
    }
    if state.order.len() > max_pending.saturating_mul(2) {
        let mut order = std::mem::take(&mut state.order);
        order.retain(|(signature, revision)| {
            state
                .entries
                .get(signature)
                .is_some_and(|entry| entry.revision == *revision)
        });
        state.order = order;
    }
    dropped
}

#[derive(Serialize)]
struct ExportPayload<'a> {
    producer_version: &'static str,
    findings: &'a [FindingResponse],
}

/// What the Hub's answer means for the batch that produced it.
enum ExportOutcome {
    Accepted,
    /// A rejection no retry can change: the payload or the route is wrong.
    /// Retaining the batch would retry it forever while live findings are
    /// evicted from the bounded buffer.
    Rejected(u16),
    /// Advisory backpressure from a Hub that is busy writing. Retry soon
    /// rather than escalating to the multi-minute ceiling.
    Busy,
    /// Transient: a network error, a 5xx, or a credential the operator can
    /// still fix. Keep the batch and back off.
    Retry(String),
}

async fn run_exporter(
    buffer: Arc<HubExportBuffer>,
    metrics: Arc<MetricsState>,
    uri: http_client::Uri,
    api_key: String,
    batch_size: usize,
    flush_interval: Duration,
    acks: AckSources,
) {
    let client = http_client::build_client_with_body();
    let mut delay = flush_interval;
    let mut failures = 0_u32;
    loop {
        tokio::time::sleep(delay).await;
        let mut batch = buffer.snapshot(batch_size);
        if batch.is_empty() {
            delay = flush_interval;
            continue;
        }

        let mut annotated = annotate(&batch, &acks).await;
        let Some(body) = bounded_payload(&mut annotated) else {
            buffer.acknowledge(&batch[..1], unix_time_ms());
            metrics.hub_export_dropped_total.inc();
            metrics.hub_export_pending.set(buffer.len() as f64);
            delay = Duration::ZERO;
            continue;
        };
        // bounded_payload only pops from the end, so truncating keeps the
        // acknowledgment aligned with what actually went over the wire.
        batch.truncate(annotated.len());

        let outcome = match http_client::fetch_with_body(
            &client,
            hyper::Method::POST,
            &uri,
            "perf-sentinel-hub-export",
            Duration::from_secs(10),
            Some(&api_key),
            Bytes::from(body),
        )
        .await
        {
            Ok((status, _)) => classify(status),
            Err(error) => ExportOutcome::Retry(error.to_string()),
        };
        delay = apply_outcome(
            &outcome,
            &batch,
            &buffer,
            &metrics,
            &uri,
            flush_interval,
            &mut failures,
        );
    }
}

fn classify(status: hyper::StatusCode) -> ExportOutcome {
    if status.is_success() {
        ExportOutcome::Accepted
    } else if status == hyper::StatusCode::SERVICE_UNAVAILABLE {
        ExportOutcome::Busy
    } else if is_permanent_rejection(status) {
        ExportOutcome::Rejected(status.as_u16())
    } else {
        ExportOutcome::Retry(format!("status {}", status.as_u16()))
    }
}

/// A 4xx a retry cannot change. 401/403 stay retryable so a rotated secret
/// recovers once the operator fixes it, and 408/429 are transient by
/// definition.
fn is_permanent_rejection(status: hyper::StatusCode) -> bool {
    status.is_client_error()
        && !matches!(
            status,
            hyper::StatusCode::UNAUTHORIZED
                | hyper::StatusCode::FORBIDDEN
                | hyper::StatusCode::REQUEST_TIMEOUT
                | hyper::StatusCode::TOO_MANY_REQUESTS
        )
}

fn apply_outcome(
    outcome: &ExportOutcome,
    batch: &[PendingExport],
    buffer: &HubExportBuffer,
    metrics: &MetricsState,
    uri: &http_client::Uri,
    flush_interval: Duration,
    failures: &mut u32,
) -> Duration {
    let delay = match outcome {
        ExportOutcome::Accepted => {
            buffer.acknowledge(batch, unix_time_ms());
            *failures = 0;
            if buffer.len() == 0 {
                flush_interval
            } else {
                Duration::ZERO
            }
        }
        ExportOutcome::Rejected(status) => {
            buffer.acknowledge(batch, unix_time_ms());
            metrics.hub_export_dropped_total.inc_by(batch.len() as u64);
            *failures = 0;
            tracing::error!(
                endpoint = %http_client::redact_endpoint(uri),
                status = *status,
                dropped = batch.len(),
                "PerfSentinelHub rejected the batch permanently; dropping it"
            );
            flush_interval
        }
        ExportOutcome::Busy => {
            // Advisory backpressure, not a fault: stay near the Hub's own
            // Retry-After instead of climbing to the five-minute ceiling.
            *failures = failures.saturating_add(1).min(BUSY_FAILURE_CAP);
            retry_delay(*failures, jitter_seed())
        }
        ExportOutcome::Retry(reason) => {
            *failures = failures.saturating_add(1);
            let delay = retry_delay(*failures, jitter_seed());
            tracing::warn!(
                endpoint = %http_client::redact_endpoint(uri),
                reason = %reason,
                retry_seconds = delay.as_secs_f64(),
                "PerfSentinelHub export failed; retaining the coalesced batch"
            );
            delay
        }
    };
    metrics.hub_export_pending.set(buffer.len() as f64);
    delay
}

/// Annotate each pending finding with its active acknowledgment, producing
/// the same envelope shape the daemon's own `/api/findings` serves.
async fn annotate(batch: &[PendingExport], acks: &AckSources) -> Vec<FindingResponse> {
    let daemon = acks.snapshot().await;
    let now = Utc::now();
    batch
        .iter()
        .map(|item| FindingResponse {
            acknowledged_by: lookup_ack(&item.finding.finding.signature, &acks.toml, &daemon, now),
            stored: item.finding.clone(),
        })
        .collect()
}

fn bounded_payload(batch: &mut Vec<FindingResponse>) -> Option<Vec<u8>> {
    while !batch.is_empty() {
        let payload = ExportPayload {
            producer_version: env!("CARGO_PKG_VERSION"),
            findings: batch,
        };
        match serde_json::to_vec(&payload) {
            Ok(body) if body.len() <= MAX_EXPORT_BODY_BYTES => return Some(body),
            Ok(_) | Err(_) if batch.len() > 1 => {
                batch.pop();
            }
            Ok(_) | Err(_) => return None,
        }
    }
    None
}

fn retry_delay(failures: u32, jitter: u64) -> Duration {
    let base_secs = 1_u64
        .checked_shl(failures.saturating_sub(1).min(8))
        .unwrap_or(256);
    let capped_ms = base_secs.min(300) * 1_000;
    let percent = 80 + jitter % 41;
    Duration::from_millis(capped_ms * percent / 100)
}

fn jitter_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| u64::from(duration.subsec_nanos()))
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::query_api::{AckSource, ResolvedTomlAck};
    use crate::detect::{Confidence, Finding, FindingType, Pattern, Severity};

    fn finding(signature: &str, trace_id: &str) -> Finding {
        Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: trace_id.to_string(),
            service: "checkout".to_string(),
            grouping: Vec::new(),
            source_endpoint: "POST /checkout".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM orders".to_string(),
                occurrences: 5,
                window_ms: 200,
                distinct_params: 5,
                ..Default::default()
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2026-08-10T10:00:00Z".to_string(),
            last_timestamp: "2026-08-10T10:00:01Z".to_string(),
            green_impact: None,
            confidence: Confidence::DaemonProduction,
            classification_method: None,
            code_location: None,
            instrumentation_scopes: Vec::new(),
            suggested_fix: None,
            signature: signature.to_string(),
        }
    }

    #[test]
    fn pending_table_coalesces_and_evicts_the_least_recent_signature() {
        let buffer = HubExportBuffer::new(2);

        assert_eq!(buffer.push_batch(&[finding("a", "old")], 1), 0);
        assert_eq!(
            buffer.push_batch(&[finding("a", "new"), finding("b", "b")], 2),
            0
        );
        assert_eq!(buffer.push_batch(&[finding("c", "c")], 3), 1);

        let snapshot = buffer.snapshot(100);
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].finding.finding.signature, "b");
        assert_eq!(snapshot[1].finding.finding.signature, "c");
    }

    #[test]
    fn acknowledging_an_old_revision_does_not_delete_a_severity_escalation() {
        let buffer = HubExportBuffer::new(10);
        buffer.push_batch(&[finding("a", "warning")], 1);
        let sent = buffer.snapshot(100);
        let mut critical = finding("a", "critical");
        critical.severity = Severity::Critical;
        buffer.push_batch(&[critical], 2);

        buffer.acknowledge(&sent, 1);

        let pending = buffer.snapshot(100);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].finding.finding.trace_id, "critical");
    }

    #[test]
    fn an_equal_repeat_in_flight_is_cleared_by_its_acknowledgment() {
        let buffer = HubExportBuffer::new(10);
        buffer.push_batch(&[finding("a", "first")], 1);
        let sent = buffer.snapshot(100);
        buffer.push_batch(&[finding("a", "repeat")], 2);

        buffer.acknowledge(&sent, 2);

        // Retaining it would make run_exporter re-POST the same batch on
        // every round trip instead of honouring the flush interval.
        assert!(buffer.snapshot(100).is_empty());
    }

    #[test]
    fn repeats_accumulate_the_occurrence_count_the_hub_reads() {
        let buffer = HubExportBuffer::new(10);

        buffer.push_batch(&[finding("a", "one"), finding("a", "two")], 10);
        buffer.push_batch(&[finding("a", "three")], 20);

        let pending = buffer.snapshot(100);
        assert_eq!(pending[0].finding.seen_count, 3);
        assert_eq!(pending[0].finding.first_seen_ms, 10);
        assert_eq!(pending[0].finding.stored_at_ms, 20);
    }

    #[test]
    fn pending_repeat_keeps_the_latest_finding_payload() {
        let buffer = HubExportBuffer::new(10);

        buffer.push_batch(&[finding("a", "old-trace")], 10);
        buffer.push_batch(&[finding("a", "latest-trace")], 20);

        assert_eq!(
            buffer.snapshot(100)[0].finding.finding.trace_id,
            "latest-trace"
        );
    }

    #[test]
    fn acknowledged_signatures_only_refresh_hourly_unless_severity_worsens() {
        let buffer = HubExportBuffer::new(10);
        buffer.push_batch(&[finding("a", "first")], 1);
        buffer.acknowledge(&buffer.snapshot(100), 1);

        buffer.push_batch(&[finding("a", "duplicate")], 2);
        assert!(buffer.snapshot(100).is_empty());

        let mut critical = finding("a", "critical");
        critical.severity = Severity::Critical;
        buffer.push_batch(&[critical], 3);
        assert_eq!(buffer.snapshot(100).len(), 1);
        buffer.acknowledge(&buffer.snapshot(100), 3);

        buffer.push_batch(&[finding("a", "refresh")], 3_600_003);
        assert_eq!(buffer.snapshot(100).len(), 1);
    }

    #[test]
    fn refresh_interval_starts_when_the_hub_acknowledges() {
        let buffer = HubExportBuffer::new(10);
        buffer.push_batch(&[finding("a", "before-outage")], 1);
        buffer.acknowledge(&buffer.snapshot(100), 3_600_001);

        buffer.push_batch(&[finding("a", "after-ack")], 3_600_002);

        assert!(buffer.snapshot(100).is_empty());
    }

    #[test]
    fn acknowledged_signature_cache_is_bounded() {
        let buffer = HubExportBuffer::new(2);
        for signature in ["a", "b", "c"] {
            buffer.push_batch(&[finding(signature, signature)], 1);
            buffer.acknowledge(&buffer.snapshot(100), 1);
        }

        let state = buffer
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.sent_entries.len(), 2);
        assert!(!state.sent_entries.contains_key("a"));
    }

    #[test]
    fn acknowledged_signature_order_is_compacted() {
        let buffer = HubExportBuffer::new(2);
        for hour in 0..6 {
            let now = hour * EXPORT_REFRESH_INTERVAL_MS;
            buffer.push_batch(&[finding("a", "a")], now);
            buffer.acknowledge(&buffer.snapshot(100), now);
        }

        let state = buffer
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.sent_order.len() <= 4);
    }

    #[test]
    fn pending_coalescing_keeps_the_worst_severity() {
        let buffer = HubExportBuffer::new(10);
        let mut critical = finding("a", "critical");
        critical.severity = Severity::Critical;

        buffer.push_batch(&[critical], 1);
        buffer.push_batch(&[finding("a", "warning")], 2);

        assert_eq!(
            buffer.snapshot(100)[0].finding.finding.severity,
            Severity::Critical
        );
    }

    #[tokio::test]
    async fn payload_matches_the_hub_batch_contract_and_backoff_is_bounded() {
        let buffer = HubExportBuffer::new(10);
        buffer.push_batch(&[finding("a", "trace")], 1);
        let batch = buffer.snapshot(100);

        let mut annotated = annotate(&batch, &acks(None)).await;
        let body = bounded_payload(&mut annotated).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["producer_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["findings"].as_array().unwrap().len(), 1);
        assert_eq!(json["findings"][0]["seen_count"], 1);
        assert!(json["findings"][0].get("acknowledged_by").is_none());
        assert_eq!(retry_delay(1, 0), Duration::from_millis(800));
        assert!(retry_delay(32, 40) <= Duration::from_mins(6));
    }

    #[tokio::test]
    async fn pushed_envelope_carries_the_acknowledgment_the_poll_path_adds() {
        let buffer = HubExportBuffer::new(10);
        buffer.push_batch(&[finding("acked-signature", "trace")], 1);
        let batch = buffer.snapshot(100);

        let annotated = annotate(&batch, &acks(Some("acked-signature"))).await;

        // Without it a pushed envelope overwrites the acked one the Hub
        // stored from the poll, and the finding reappears as open.
        assert!(matches!(
            annotated[0].acknowledged_by,
            Some(AckSource::Toml { .. })
        ));
    }

    #[test]
    fn only_an_unfixable_rejection_drops_the_batch() {
        use hyper::StatusCode;

        assert!(matches!(
            classify(StatusCode::BAD_REQUEST),
            ExportOutcome::Rejected(400)
        ));
        assert!(matches!(
            classify(StatusCode::PAYLOAD_TOO_LARGE),
            ExportOutcome::Rejected(413)
        ));
        // A rotated secret and a busy or broken Hub must keep the batch.
        assert!(matches!(
            classify(StatusCode::UNAUTHORIZED),
            ExportOutcome::Retry(_)
        ));
        assert!(matches!(
            classify(StatusCode::TOO_MANY_REQUESTS),
            ExportOutcome::Retry(_)
        ));
        assert!(matches!(
            classify(StatusCode::INTERNAL_SERVER_ERROR),
            ExportOutcome::Retry(_)
        ));
        assert!(matches!(
            classify(StatusCode::SERVICE_UNAVAILABLE),
            ExportOutcome::Busy
        ));
        assert!(matches!(classify(StatusCode::OK), ExportOutcome::Accepted));
        // Advisory backpressure stays near the Hub's own Retry-After.
        assert!(retry_delay(BUSY_FAILURE_CAP, 40) <= Duration::from_secs(6));
    }

    fn acks(acked_signature: Option<&str>) -> AckSources {
        let mut toml = HashMap::new();
        if let Some(signature) = acked_signature {
            toml.insert(
                signature.to_string(),
                ResolvedTomlAck {
                    inner: crate::acknowledgments::Acknowledgment {
                        signature: signature.to_string(),
                        acknowledged_by: "team@example.com".to_string(),
                        acknowledged_at: "2026-08-01".to_string(),
                        reason: "accepted debt".to_string(),
                        expires_at: None,
                        service: None,
                        source_endpoint: None,
                    },
                    expires_at_dt: None,
                },
            );
        }
        AckSources {
            toml: Arc::new(toml),
            store: None,
        }
    }
}
