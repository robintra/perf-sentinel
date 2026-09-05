//! Observed-service incidents, and the findings that were live when one
//! happened.
//!
//! perf-sentinel does not detect a crash and cannot see an observed
//! service's memory: it has no OTLP metrics path, and a service that
//! saturates usually keeps emitting spans, more slowly. The operator's
//! alerting owns the moment. What perf-sentinel owns is the findings of
//! a period, and it is the only thing that can freeze them before the
//! FIFO ring evicts them, which on a busy fleet takes minutes.
//!
//! So an incident arrives by POST and resolves its window immediately.
//! Nothing here polls, scrapes or judges whether a service is alive.
//!
//! This is about observed services. The daemon's own cgroup pressure is
//! `super::mem_pressure`, and its own liveness is `super::health`.

use std::collections::VecDeque;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc};

use super::findings_store::StoredFinding;

/// Longest `detail` kept, in bytes. An annotation can carry a stack
/// trace, and this string reaches a terminal, the HTML sink and the Hub.
const MAX_DETAIL_BYTES: usize = 512;

/// What happened to the service, as the alert declared it.
///
/// A closed set, because it labels a Prometheus counter. An unrecognized
/// value folds into [`IncidentKind::Other`] rather than minting a label,
/// so a caller cannot widen cardinality by inventing a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentKind {
    /// The kernel killed the process for exceeding its memory limit.
    OomKill,
    /// Memory climbing toward the limit, without a kill.
    MemorySaturation,
    /// The process restarted, for any other reason.
    Restart,
    /// A deploy, which is worth recording so it is not read as a crash.
    Deploy,
    /// Anything else, including every unrecognized declared kind.
    Other,
}

impl IncidentKind {
    /// Bounded label value for `perf_sentinel_incidents_total`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OomKill => "oom_kill",
            Self::MemorySaturation => "memory_saturation",
            Self::Restart => "restart",
            Self::Deploy => "deploy",
            Self::Other => "other",
        }
    }

    /// Every variant, so the counter can be pre-warmed at zero. A series
    /// that appears only on the first incident reads as a scrape failure
    /// rather than as "nothing happened".
    pub const ALL: [Self; 5] = [
        Self::OomKill,
        Self::MemorySaturation,
        Self::Restart,
        Self::Deploy,
        Self::Other,
    ];

    /// Parse a declared kind. Deliberately exact rather than a keyword
    /// heuristic: an operator who wants a precise kind writes it, and
    /// everything else is honestly `Other` instead of silently guessed.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == raw)
            .unwrap_or(Self::Other)
    }
}

/// One recorded incident, with the findings that were live in its window.
///
/// `#[non_exhaustive]` so a later field stays a minor bump, matching
/// [`StoredFinding`].
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct Incident {
    /// Content-derived id: `sha2` over `service|kind|at_ms`, 32 hex
    /// characters, the same shape and taste as an acknowledgment
    /// signature. Reposting the same alert is idempotent without a
    /// dedup table and without a server-assigned id round trip.
    pub id: String,
    /// The perf-sentinel service the incident is about. This is the join
    /// key to the findings, so an alert without one is refused.
    pub service: String,
    /// What happened, as declared.
    pub kind: IncidentKind,
    /// When it started, Unix epoch milliseconds.
    pub at_ms: u64,
    /// When it ended, absent while it is still firing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    /// Free text from the alert, sanitized and capped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Lower bound of the window the findings were taken from, `at_ms`
    /// minus the configured lookback. Stored rather than derived because
    /// the archive must describe itself after a config change.
    pub window_from_ms: u64,
    /// Upper bound of that window, past `at_ms` by the settle margin, so
    /// the traces live at the incident land inside it once analysed. The
    /// tail can therefore hold the first traces of a restarted process.
    pub window_to_ms: u64,
    /// Detection time of the oldest finding the ring held when the
    /// window was resolved, absent when it was empty. Below
    /// `window_from_ms` it means the capture is complete. Above it, the
    /// ring had already evicted part of the window and `findings` is
    /// short of what fired, which the archive may still answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_finding_ms: Option<u64>,
    /// The findings of the window, folded by signature over the window
    /// alone, so `seen_count` and `first_seen_ms` describe the window.
    /// Frozen at reception, then merged once with the settle pass, which
    /// can add rows and raise counts but never remove.
    pub findings: Vec<StoredFinding>,
}

impl Incident {
    /// Content-derived id, so the same alert posted twice is the same
    /// incident.
    #[must_use]
    pub fn compute_id(service: &str, kind: IncidentKind, at_ms: u64) -> String {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;

        let mut hasher = Sha256::new();
        hasher.update(service.as_bytes());
        hasher.update(b"|");
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"|");
        hasher.update(at_ms.to_string().as_bytes());
        let digest = hasher.finalize();
        let mut out = String::with_capacity(32);
        for byte in &digest[..16] {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}

/// Bounded ring of recorded incidents, newest last.
///
/// The shape of [`super::findings_store::FindingsStore`] on purpose,
/// copied rather than generalized into a shared ring: two similar rings
/// beat a trait with two implementations.
///
/// It lives in memory and dies with the daemon. A node-level memory
/// event that kills the observed service often takes a co-located daemon
/// with it, so the durable record is the archive, see [`spawn_archive`].
#[derive(Debug)]
pub struct IncidentStore {
    inner: RwLock<VecDeque<Incident>>,
    max_size: usize,
}

impl IncidentStore {
    /// Ring holding at most `max_size` incidents.
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: RwLock::new(VecDeque::new()),
            max_size,
        }
    }

    /// Record a new incident. Returns `false`, and keeps the existing
    /// record untouched, when the id is already present.
    ///
    /// Never a replace: the first capture is the one taken closest to the
    /// incident, and a repost has nothing better to offer. What a repost
    /// may carry is an end, see [`Self::close`], and the settle pass
    /// merges rather than replaces, see [`Self::merge`].
    pub async fn record(&self, incident: Incident) -> bool {
        if self.max_size == 0 {
            return false;
        }
        let mut buf = self.inner.write().await;
        if buf.iter().any(|i| i.id == incident.id) {
            return false;
        }
        buf.push_back(incident);
        while buf.len() > self.max_size {
            buf.pop_front();
        }
        true
    }

    /// Whether an incident with this id is retained.
    pub async fn contains(&self, id: &str) -> bool {
        self.inner.read().await.iter().any(|i| i.id == id)
    }

    /// Set the end of a retained incident that had none. Returns the
    /// updated record when that transition happened, so the caller can
    /// archive the closed line, and `None` otherwise.
    pub async fn close(&self, id: &str, ended_at_ms: u64) -> Option<Incident> {
        let mut buf = self.inner.write().await;
        let incident = buf.iter_mut().find(|i| i.id == id)?;
        if incident.ended_at_ms.is_some() {
            return None;
        }
        incident.ended_at_ms = Some(ended_at_ms);
        Some(incident.clone())
    }

    /// Merge a later resolution of the same window into the retained
    /// record, by fold key, so it can only grow. The settle pass calls it
    /// once the traces live at the incident have been analysed. An
    /// eviction in between cannot degrade the record: rows the later fold
    /// lost are kept, and `oldest_finding_ms` keeps the earlier stamp.
    /// Returns the merged record, or `None` when the id is gone.
    pub async fn merge(&self, later: Incident) -> Option<Incident> {
        let mut buf = self.inner.write().await;
        let slot = buf.iter_mut().find(|i| i.id == later.id)?;
        super::findings_store::merge_folded(&mut slot.findings, later.findings);
        slot.oldest_finding_ms = match (slot.oldest_finding_ms, later.oldest_finding_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Some(slot.clone())
    }

    /// Recorded incidents, newest first, skipping `offset` and capped at
    /// `limit`.
    pub async fn list(&self, service: Option<&str>, offset: usize, limit: usize) -> Vec<Incident> {
        let buf = self.inner.read().await;
        buf.iter()
            .rev()
            .filter(|i| service.is_none_or(|s| i.service == s))
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    }
}

// ── The archive ───────────────────────────────────────────────────

/// Records waiting for the writer. Incidents are rare, so a full channel
/// means the disk stalled, and dropping with a count beats blocking a
/// webhook on it.
const ARCHIVE_CHANNEL_CAPACITY: usize = 256;

/// The single writer of the incident archive, and the sender the query
/// API hands records to.
#[derive(Debug)]
pub struct ArchiveHandle {
    pub tx: mpsc::Sender<Vec<u8>>,
    pub join: tokio::task::JoinHandle<()>,
}

/// Open the archive and spawn its writer.
///
/// One writer task, on the model of `super::archive`: every record is one
/// `write` of one line, so two deliveries or a delivery and a settle pass
/// cannot interleave, and a webhook handler dropped by a timeout has
/// already handed its record over. The open runs at startup, so a
/// symlink, a weak mode, a missing directory or a read-only filesystem
/// fail the daemon rather than the first incident.
///
/// # Errors
///
/// The open error, a refused symlink or a refused mode.
pub fn spawn_archive(
    path: &str,
    metrics: Arc<crate::report::metrics::MetricsState>,
) -> std::io::Result<ArchiveHandle> {
    let file = open_archive(std::path::Path::new(path))?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>(ARCHIVE_CHANNEL_CAPACITY);
    let join = tokio::spawn(run_writer(rx, file, metrics));
    Ok(ArchiveHandle { tx, join })
}

/// Hand one serialized record to the writer without blocking. A full or
/// closed channel drops it, counted on
/// `perf_sentinel_incidents_archive_failed_total` and logged. The ring
/// still holds the incident, durability is what was lost.
pub fn try_send(
    tx: &mpsc::Sender<Vec<u8>>,
    line: Vec<u8>,
    metrics: &crate::report::metrics::MetricsState,
) {
    if let Err(error) = tx.try_send(line) {
        metrics.incidents_archive_failed_total.inc();
        tracing::warn!(%error, "Incident archive record dropped, the ring still holds it");
    }
}

/// Open the archive for appending, with the guards the other appenders
/// have: no symlink, an open that never follows one, owner-only mode on
/// an existing file since `mode(0o600)` applies on creation only, and a
/// crash-truncated last line sealed. Append-only, last record of an id wins, no rotation,
/// point logrotate at it.
///
/// # Errors
///
/// The underlying I/O error, or a refused symlink or mode.
pub fn open_archive(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    if super::archive::is_symlink(path) {
        return Err(std::io::Error::other("incident archive path is a symlink"));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    super::archive::no_follow(&mut options);
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if super::archive::is_reparse_point(&metadata) {
        return Err(std::io::Error::other(
            "incident archive path is a symlink or another reparse point",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(std::io::Error::other(format!(
                "incident archive has mode {mode:o}, refusing to append detail and templates to it"
            )));
        }
    }
    super::archive::terminate_incomplete_line(&mut file)?;
    Ok(file)
}

/// Drain the channel into the file, one `write` per record.
async fn run_writer(
    mut rx: mpsc::Receiver<Vec<u8>>,
    mut file: std::fs::File,
    metrics: Arc<crate::report::metrics::MetricsState>,
) {
    use std::io::Write as _;

    while let Some(mut line) = rx.recv().await {
        line.push(b'\n');
        if let Err(error) = file.write_all(&line) {
            metrics.incidents_archive_failed_total.inc();
            tracing::warn!(%error, "Incident archive write failed, the ring still holds it");
            // A partial line may have landed, seal it so the next record
            // is not glued to it.
            let _ = super::archive::terminate_incomplete_line(&mut file);
        }
    }
}

// ── The Alertmanager webhook envelope ─────────────────────────────

/// One alert inside a webhook delivery.
#[derive(Debug, Deserialize)]
pub struct WebhookAlert {
    /// `firing` or `resolved`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub annotations: std::collections::HashMap<String, String>,
    /// RFC 3339, any offset.
    #[serde(rename = "startsAt", default)]
    pub starts_at: String,
    /// RFC 3339, any offset. Alertmanager sends the zero time while firing.
    #[serde(rename = "endsAt", default)]
    pub ends_at: String,
}

/// The webhook body Alertmanager posts.
///
/// This is the only accepted shape, because it is the only one an
/// operator cannot produce otherwise: `webhook_config` has no body
/// template. Any script can emit this envelope with `curl`, so accepting
/// a second bespoke shape would buy nothing and cost a second parser.
#[derive(Debug, Deserialize)]
pub struct Webhook {
    #[serde(default)]
    pub alerts: Vec<WebhookAlert>,
}

/// Why one alert of a delivery was not recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectedAlert {
    /// No `service_label` on the alert, so nothing to join findings to.
    NoService,
    /// `startsAt` absent or not an RFC 3339 timestamp.
    UnparsableTime,
}

/// What one alert asks for, once validated. The caller resolves the
/// window and builds the [`Incident`], which keeps this function free of
/// the store and testable on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentRequest {
    /// [`Incident::compute_id`] of the three fields below, computed once.
    pub id: String,
    pub service: String,
    pub kind: IncidentKind,
    pub at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub detail: Option<String>,
}

/// Read one alert into an [`IncidentRequest`].
///
/// `service_label` and `kind_label` name the labels to read. A missing
/// or unrecognized kind is [`IncidentKind::Other`], which is honest,
/// whereas guessing from `alertname` would be a heuristic nobody can
/// see failing.
///
/// # Errors
///
/// Returns the reason the alert cannot become an incident.
pub fn read_alert(
    alert: &WebhookAlert,
    service_label: &str,
    kind_label: &str,
) -> Result<IncidentRequest, RejectedAlert> {
    // Labels are trimmed like the stamp is: quoted YAML keeps a trailing
    // space, and the service is the join key to the findings.
    let service = alert
        .labels
        .get(service_label)
        .map(|s| crate::text_safety::strip_bidi_and_invisible(s.trim()).into_owned())
        .filter(|s| !s.is_empty())
        .ok_or(RejectedAlert::NoService)?;
    let at_ms = parse_rfc3339_ms(&alert.starts_at).ok_or(RejectedAlert::UnparsableTime)?;
    // Alertmanager sends the zero time while an alert is firing, which
    // is before 1970 and does not fit a `u64`, so a failure here is
    // simply "still firing". An end before the start is a bad clock or a
    // bad body, and is treated the same way rather than sealed for good.
    let ended_at_ms = (alert.status == "resolved")
        .then(|| parse_rfc3339_ms(&alert.ends_at))
        .flatten()
        .filter(|ended| *ended >= at_ms);
    let kind = alert
        .labels
        .get(kind_label)
        .map_or(IncidentKind::Other, |v| IncidentKind::parse(v.trim()));
    let detail = alert
        .annotations
        .get("summary")
        .or_else(|| alert.annotations.get("description"))
        .map(|d| cap_detail(d));
    Ok(IncidentRequest {
        id: Incident::compute_id(&service, kind, at_ms),
        service,
        kind,
        at_ms,
        ended_at_ms,
        detail,
    })
}

/// Sanitize and cap free text before it can reach a terminal, the HTML
/// sink or the Hub. A count cap alone is not enough on a string derived
/// from a stack trace, and a byte cap alone could split a character.
fn cap_detail(raw: &str) -> String {
    let mut safe = crate::text_safety::sanitize_for_terminal(raw).into_owned();
    crate::event::truncate_field(&mut safe, MAX_DETAIL_BYTES);
    safe
}

/// Epoch milliseconds from an RFC 3339 stamp with any offset.
///
/// Alertmanager serializes with Go's `time.Time`, in the process's own
/// zone, so `+02:00` is the common case and the crate's `Z`-only span
/// parser, kept strict for the hot path, would refuse every alert from a
/// non-UTC deployment.
fn parse_rfc3339_ms(raw: &str) -> Option<u64> {
    let stamp = chrono::DateTime::parse_from_rfc3339(raw.trim()).ok()?;
    u64::try_from(stamp.timestamp_millis()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acknowledgments::enrich_with_signatures;
    use crate::detect::{FindingType, Severity};

    fn alert(labels: &[(&str, &str)], starts_at: &str, status: &str) -> WebhookAlert {
        WebhookAlert {
            status: status.to_string(),
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            annotations: std::collections::HashMap::new(),
            starts_at: starts_at.to_string(),
            ends_at: String::new(),
        }
    }

    fn incident(service: &str, kind: IncidentKind, at_ms: u64) -> Incident {
        Incident {
            id: Incident::compute_id(service, kind, at_ms),
            service: service.to_string(),
            kind,
            at_ms,
            ended_at_ms: None,
            detail: None,
            window_from_ms: at_ms.saturating_sub(1000),
            window_to_ms: at_ms,
            oldest_finding_ms: None,
            findings: Vec::new(),
        }
    }

    fn stored(template: &str, stamp: u64) -> StoredFinding {
        let mut f = crate::test_helpers::make_finding(FindingType::RedundantSql, Severity::Warning);
        f.pattern.template = template.to_string();
        enrich_with_signatures(std::slice::from_mut(&mut f));
        StoredFinding {
            finding: f,
            stored_at_ms: stamp,
            first_seen_ms: stamp,
            seen_count: 1,
        }
    }

    #[test]
    fn an_alert_without_a_service_cannot_become_an_incident() {
        let a = alert(
            &[("alertname", "KubePodOOM")],
            "2026-09-05T14:03:00Z",
            "firing",
        );
        assert_eq!(
            read_alert(&a, "service", "perf_sentinel_kind"),
            Err(RejectedAlert::NoService),
            "the service is the join key to the findings, there is no useful fallback"
        );
    }

    #[test]
    fn an_unrecognized_kind_is_other_rather_than_guessed() {
        let a = alert(
            &[
                ("service", "cart-svc"),
                ("perf_sentinel_kind", "KubePodOOMKilled"),
            ],
            "2026-09-05T14:03:00Z",
            "firing",
        );
        let req = read_alert(&a, "service", "perf_sentinel_kind").unwrap();
        assert_eq!(req.kind, IncidentKind::Other);
        assert_eq!(req.service, "cart-svc");
        assert_eq!(req.at_ms, 1_788_616_980_000);
        assert_eq!(
            req.id,
            Incident::compute_id("cart-svc", IncidentKind::Other, req.at_ms)
        );

        let a = alert(
            &[("service", "cart-svc"), ("perf_sentinel_kind", "oom_kill")],
            "2026-09-05T14:03:00Z",
            "firing",
        );
        assert_eq!(
            read_alert(&a, "service", "perf_sentinel_kind")
                .unwrap()
                .kind,
            IncidentKind::OomKill
        );
    }

    #[test]
    fn labels_are_trimmed_like_the_stamp_is() {
        // Quoted YAML keeps a trailing space, and a padded join key would
        // match no finding while a padded stamp is accepted.
        let a = alert(
            &[
                ("service", " cart-svc "),
                ("perf_sentinel_kind", "oom_kill "),
            ],
            " 2026-09-05T14:03:00Z ",
            "firing",
        );
        let req = read_alert(&a, "service", "perf_sentinel_kind").unwrap();
        assert_eq!(req.service, "cart-svc");
        assert_eq!(req.kind, IncidentKind::OomKill);
    }

    #[test]
    fn a_firing_alert_has_no_end_even_when_alertmanager_sends_the_zero_time() {
        let mut a = alert(&[("service", "svc")], "2026-09-05T14:03:00Z", "firing");
        a.ends_at = "0001-01-01T00:00:00Z".to_string();
        assert_eq!(
            read_alert(&a, "service", "perf_sentinel_kind")
                .unwrap()
                .ended_at_ms,
            None
        );

        let mut a = alert(&[("service", "svc")], "2026-09-05T14:03:00Z", "resolved");
        a.ends_at = "2026-09-05T14:09:00Z".to_string();
        assert_eq!(
            read_alert(&a, "service", "perf_sentinel_kind")
                .unwrap()
                .ended_at_ms,
            Some(1_788_617_340_000)
        );
    }

    #[test]
    fn an_end_before_the_start_is_not_an_end() {
        // A skewed clock or a hand-rolled body: sealing it would refuse the
        // corrected resolve that follows.
        let mut a = alert(&[("service", "svc")], "2026-09-05T14:03:00Z", "resolved");
        a.ends_at = "1970-01-01T00:00:00Z".to_string();
        assert_eq!(
            read_alert(&a, "service", "perf_sentinel_kind")
                .unwrap()
                .ended_at_ms,
            None
        );
        a.ends_at = "2026-09-05T14:03:00Z".to_string();
        assert_eq!(
            read_alert(&a, "service", "perf_sentinel_kind")
                .unwrap()
                .ended_at_ms,
            Some(1_788_616_980_000),
            "an end equal to the start is the shortest incident, not a bad one"
        );
    }

    #[test]
    fn a_non_utc_alertmanager_stamp_is_read_not_refused() {
        // Go serializes in the process zone, so this is the common case.
        let a = alert(
            &[("service", "svc")],
            "2026-09-05T16:03:00.123456789+02:00",
            "firing",
        );
        assert_eq!(
            read_alert(&a, "service", "perf_sentinel_kind")
                .unwrap()
                .at_ms,
            1_788_616_980_123
        );
        let a = alert(&[("service", "svc")], "not a stamp", "firing");
        assert_eq!(
            read_alert(&a, "service", "perf_sentinel_kind"),
            Err(RejectedAlert::UnparsableTime)
        );
    }

    #[test]
    fn every_kind_round_trips_through_its_label() {
        for kind in IncidentKind::ALL {
            assert_eq!(IncidentKind::parse(kind.as_str()), kind);
        }
        assert_eq!(IncidentKind::parse("KubePodOOMKilled"), IncidentKind::Other);
    }

    #[test]
    fn detail_is_sanitized_and_capped_on_a_character_boundary() {
        let mut a = alert(&[("service", "svc")], "2026-09-05T14:03:00Z", "firing");
        a.annotations.insert("summary".to_string(), "é".repeat(400));
        let detail = read_alert(&a, "service", "perf_sentinel_kind")
            .unwrap()
            .detail
            .unwrap();
        assert!(detail.len() <= MAX_DETAIL_BYTES);
        assert!(
            detail.chars().all(|c| c == 'é'),
            "the cap must not split a character"
        );
    }

    #[tokio::test]
    async fn a_repost_keeps_the_first_capture_and_can_only_close_it() {
        let store = IncidentStore::new(10);
        let mut first = incident("svc", IncidentKind::OomKill, 5000);
        first.oldest_finding_ms = Some(1);
        assert!(store.record(first).await, "the first delivery is new");
        let mut degraded = incident("svc", IncidentKind::OomKill, 5000);
        degraded.oldest_finding_ms = Some(4999);
        assert!(
            !store.record(degraded).await,
            "a repost is not a second incident, or the counter would climb on its own"
        );
        let all = store.list(None, 0, 10).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].oldest_finding_ms, Some(1), "the first capture stays");
        assert!(store.contains(&all[0].id).await);

        let closed = store.close(&all[0].id, 9000).await;
        assert_eq!(closed.map(|i| i.ended_at_ms), Some(Some(9000)));
        assert!(
            store.close(&all[0].id, 9500).await.is_none(),
            "closing twice is not a second transition to archive"
        );
        assert!(store.close("missing", 1).await.is_none());
    }

    #[tokio::test]
    async fn merge_grows_the_record_and_keeps_the_earlier_oldest_stamp() {
        let store = IncidentStore::new(10);
        let mut first = incident("svc", IncidentKind::Restart, 5000);
        first.findings = vec![stored("SELECT a", 1000), stored("SELECT b", 2000)];
        first.oldest_finding_ms = Some(500);
        store.record(first).await;
        let id = Incident::compute_id("svc", IncidentKind::Restart, 5000);

        // The settle fold lost A to eviction and gained C, with the ring's
        // front now past the window start.
        let mut later = incident("svc", IncidentKind::Restart, 5000);
        later.findings = vec![stored("SELECT b", 5000), stored("SELECT c", 6000)];
        later.oldest_finding_ms = Some(4000);
        let merged = store.merge(later).await.expect("the id is retained");
        let templates: Vec<&str> = merged
            .findings
            .iter()
            .map(|sf| sf.finding.pattern.template.as_str())
            .collect();
        assert_eq!(
            templates,
            ["SELECT a", "SELECT b", "SELECT c"],
            "grows, never loses"
        );
        assert_eq!(
            merged.oldest_finding_ms,
            Some(500),
            "a complete capture is not relabeled incomplete by a later eviction"
        );

        let mut unknown = incident("other", IncidentKind::Restart, 1);
        unknown.id = "not-there".to_string();
        assert!(store.merge(unknown).await.is_none());
        assert!(store.contains(&id).await);
    }

    #[tokio::test]
    async fn the_ring_evicts_oldest_first_filters_by_service_and_pages() {
        let store = IncidentStore::new(2);
        for (svc, at) in [("a", 1000), ("b", 2000), ("c", 3000)] {
            assert!(store.record(incident(svc, IncidentKind::Restart, at)).await);
        }
        let all = store.list(None, 0, 10).await;
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].service, "c", "newest first");
        assert_eq!(
            store.list(None, 1, 10).await[0].service,
            "b",
            "offset skips the newest"
        );
        assert_eq!(store.list(Some("b"), 0, 10).await.len(), 1);
        assert!(
            store.list(Some("a"), 0, 10).await.is_empty(),
            "a was evicted"
        );
    }

    #[tokio::test]
    async fn a_zero_sized_ring_records_nothing() {
        let store = IncidentStore::new(0);
        assert!(!store.record(incident("svc", IncidentKind::Deploy, 1)).await);
        assert!(store.list(None, 0, 10).await.is_empty());
    }

    #[test]
    fn the_archive_open_seals_a_torn_line_and_refuses_a_weak_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("incidents.ndjson");
        {
            use std::io::Write as _;
            let mut file = open_archive(&path).expect("the parent directory is created");
            file.write_all(b"{\"a\":1}\n{\"half\":").unwrap();
        }
        // A crash mid-write left a torn line: reopening seals it so the next
        // record is not glued onto it.
        drop(open_archive(&path).unwrap());
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 2);
        assert!(body.ends_with('\n'));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "detail and templates are not world-readable"
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                open_archive(&path).is_err(),
                "mode(0o600) applies on creation only, a weakened file is refused"
            );
            let link = dir.path().join("link.ndjson");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(open_archive(&link).is_err());
        }
    }

    #[tokio::test]
    async fn the_writer_appends_one_line_per_record_and_drains_on_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incidents.ndjson");
        let metrics = Arc::new(crate::report::metrics::MetricsState::new());
        let handle = spawn_archive(path.to_str().unwrap(), Arc::clone(&metrics)).unwrap();
        let first = incident("svc", IncidentKind::OomKill, 5000);
        try_send(&handle.tx, serde_json::to_vec(&first).unwrap(), &metrics);
        try_send(&handle.tx, serde_json::to_vec(&first).unwrap(), &metrics);
        drop(handle.tx);
        handle.join.await.unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "one line per record, append-only");
        let last: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(last["id"], first.id);
        assert_eq!(metrics.incidents_archive_failed_total.get(), 0);
    }

    #[test]
    fn the_id_depends_on_every_component() {
        let base = Incident::compute_id("svc", IncidentKind::OomKill, 1000);
        assert_eq!(base.len(), 32);
        assert_ne!(
            base,
            Incident::compute_id("other", IncidentKind::OomKill, 1000)
        );
        assert_ne!(
            base,
            Incident::compute_id("svc", IncidentKind::Restart, 1000)
        );
        assert_ne!(
            base,
            Incident::compute_id("svc", IncidentKind::OomKill, 1001)
        );
    }
}
