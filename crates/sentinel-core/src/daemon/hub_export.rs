use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde::Serialize;

use super::findings_store::StoredFinding;
use crate::config::DaemonHubExportConfig;
use crate::detect::Finding;
use crate::http_client;
use crate::report::metrics::MetricsState;

const MAX_EXPORT_BODY_BYTES: usize = 2 * 1024 * 1024;
const EXPORT_REFRESH_INTERVAL_MS: u64 = 60 * 60 * 1_000;

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
            if state
                .entries
                .get(&finding.signature)
                .is_some_and(|pending| pending.finding.finding.severity < finding.severity)
            {
                continue;
            }
            if !state.entries.contains_key(&finding.signature)
                && state
                    .sent_entries
                    .get(&finding.signature)
                    .is_some_and(|sent| {
                        now_ms.saturating_sub(sent.sent_at_ms) < EXPORT_REFRESH_INTERVAL_MS
                            && finding.severity >= sent.severity
                    })
            {
                continue;
            }
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

        while state.entries.len() > self.max_pending {
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
        if state.order.len() > self.max_pending.saturating_mul(2) {
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

#[derive(Serialize)]
struct ExportPayload<'a> {
    producer_version: &'static str,
    findings: Vec<&'a StoredFinding>,
}

async fn run_exporter(
    buffer: Arc<HubExportBuffer>,
    metrics: Arc<MetricsState>,
    uri: http_client::Uri,
    api_key: String,
    batch_size: usize,
    flush_interval: Duration,
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

        let Some(body) = bounded_payload(&mut batch) else {
            buffer.acknowledge(&batch[..1], unix_time_ms());
            metrics.hub_export_dropped_total.inc();
            metrics.hub_export_pending.set(buffer.len() as f64);
            delay = Duration::ZERO;
            continue;
        };
        match http_client::fetch_with_body(
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
            Ok((status, _)) if status.is_success() => {
                buffer.acknowledge(&batch, unix_time_ms());
                metrics.hub_export_pending.set(buffer.len() as f64);
                failures = 0;
                delay = if buffer.len() == 0 {
                    flush_interval
                } else {
                    Duration::ZERO
                };
            }
            Ok((status, _)) => {
                failures = failures.saturating_add(1);
                delay = retry_delay(failures, jitter_seed());
                tracing::warn!(
                    endpoint = %http_client::redact_endpoint(&uri),
                    status = status.as_u16(),
                    retry_seconds = delay.as_secs_f64(),
                    "PerfSentinelHub export rejected; retaining the coalesced batch"
                );
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                delay = retry_delay(failures, jitter_seed());
                tracing::warn!(
                    endpoint = %http_client::redact_endpoint(&uri),
                    error = %error,
                    retry_seconds = delay.as_secs_f64(),
                    "PerfSentinelHub export failed; retaining the coalesced batch"
                );
            }
        }
    }
}

fn bounded_payload(batch: &mut Vec<PendingExport>) -> Option<Vec<u8>> {
    while !batch.is_empty() {
        let payload = ExportPayload {
            producer_version: env!("CARGO_PKG_VERSION"),
            findings: batch.iter().map(|item| &item.finding).collect(),
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
    fn acknowledging_an_old_revision_does_not_delete_a_newer_update() {
        let buffer = HubExportBuffer::new(10);
        buffer.push_batch(&[finding("a", "old")], 1);
        let sent = buffer.snapshot(100);
        buffer.push_batch(&[finding("a", "new")], 2);

        buffer.acknowledge(&sent, 1);

        let pending = buffer.snapshot(100);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].finding.finding.trace_id, "new");
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

    #[test]
    fn payload_matches_the_hub_batch_contract_and_backoff_is_bounded() {
        let buffer = HubExportBuffer::new(10);
        buffer.push_batch(&[finding("a", "trace")], 1);
        let mut batch = buffer.snapshot(100);

        let body = bounded_payload(&mut batch).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["producer_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["findings"].as_array().unwrap().len(), 1);
        assert_eq!(retry_delay(1, 0), Duration::from_millis(800));
        assert!(retry_delay(32, 40) <= Duration::from_mins(6));
    }
}
