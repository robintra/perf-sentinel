//! HTTP query API for the daemon's internal state.
//!
//! Exposes findings, trace explanations, correlations, and status
//! alongside the existing `/v1/traces` and `/metrics` endpoints.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ack::{self, AckAction, AckEntry, AckError, AckStore};
use super::findings_store::{FindingsFilter, FindingsStore, StoredFinding};
use crate::acknowledgments::{Acknowledgment, compute_signature};
use crate::correlate::Trace;
use crate::correlate::window::TraceWindow;
use crate::detect::correlate_cross::{CrossTraceCorrelation, CrossTraceCorrelator};
#[cfg(test)]
use crate::detect::sanitizer_aware::SanitizerAwareMode;
use crate::detect::{self, DetectConfig};
use crate::explain;
use crate::report::metrics::{AckFailureReason, MetricsState};
use crate::report::{Analysis, GreenSummary, QualityGate, Report};

/// Upper bound for `?limit=` on `/api/findings`, caps response size
/// under the loopback API. Exposed `pub` so the CLI can reuse it for
/// its boot fetch cap and stay in lockstep, internal API not part of
/// the published surface.
#[doc(hidden)]
pub const MAX_FINDINGS_LIMIT: usize = 1000;

/// Upper bound for `/api/correlations` response size. Same rationale as
/// [`MAX_FINDINGS_LIMIT`]: cap response size under an unauthenticated
/// loopback API. In practice `max_tracked_pairs` (config default `10_000`)
/// already bounds the correlator's memory, but serializing all pairs
/// per poll is still an expensive operation we want to limit.
const MAX_CORRELATIONS_LIMIT: usize = 1000;

/// Upper bound on the entry count returned by `GET /api/acks`. Same
/// rationale as the other caps (loopback API, bounded JSON
/// serialization). Exposed to the CLI so the `perf-sentinel ack list`
/// footer can quote the same number ("showing up to N") without drift,
/// internal API not part of the published surface.
#[doc(hidden)]
pub const MAX_ACKS_RESPONSE: usize = 1000;

/// Shared state for query API route handlers.
pub struct QueryApiState {
    pub findings_store: Arc<FindingsStore>,
    pub window: Arc<tokio::sync::Mutex<TraceWindow>>,
    pub detect_config: DetectConfig,
    pub start_time: std::time::Instant,
    /// Optional cross-trace correlator. `None` when
    /// `[daemon.correlation] enabled = false`.
    pub correlator: Option<Arc<tokio::sync::Mutex<CrossTraceCorrelator>>>,
    /// Shared metrics registry. The `/api/export/report` handler reads
    /// lifetime counters (`events_processed_total`, `traces_analyzed_total`)
    /// to populate the `Report.analysis` fields, and bumps
    /// `export_report_requests_total` per call.
    pub metrics: Arc<MetricsState>,
    /// Active Electricity Maps scoring configuration, copied from the
    /// loaded `Config` at daemon startup. Surfaces on
    /// `/api/export/report` so the audit chip stays visible whenever
    /// Electricity Maps is configured. `None` otherwise.
    pub scoring_config: Option<crate::score::carbon::ScoringConfig>,
    /// Live `GreenSummary` refreshed by the event loop after each
    /// batch. `/api/export/report` clones this cell so the snapshot
    /// reflects the latest CO2 picture (regions, top offenders,
    /// avoidable I/O ratio) instead of `GreenSummary::disabled(0)`.
    /// Initialized to `disabled(0)` at daemon startup. The cold-start
    /// branch (`events_processed == 0 || traces_analyzed == 0`) returns
    /// the empty envelope with `disabled(0)` instead of reading this
    /// cell, so clients never observe a half-populated value.
    pub green_summary: Arc<tokio::sync::RwLock<GreenSummary>>,
    /// Daemon-side ack store (JSONL persistence). `None` when
    /// `[daemon.ack] enabled = false`, in which case the three ack
    /// endpoints return 503 Service Unavailable.
    pub ack_store: Option<Arc<AckStore>>,
    /// CI-side TOML acks loaded at daemon startup. Read-only baseline,
    /// unioned with `ack_store` at query time. TOML wins on conflict.
    /// The `expires_at` string is pre-parsed at startup into a
    /// [`ResolvedTomlAck`] so the hot query path does no chrono parse.
    pub toml_acks: Arc<HashMap<String, ResolvedTomlAck>>,
    /// Optional API key for ack `POST` / `DELETE`. `None` means no auth
    /// (the documented loopback-only deployment), `Some(key)` enforces
    /// constant-time `X-API-Key` comparison.
    pub ack_api_key: Option<String>,
    /// Daemon config frozen at startup (the daemon never reloads
    /// config), read by the tuning advisor so its hints can name the
    /// current value of each knob.
    pub daemon_config: crate::config::DaemonConfig,
    /// Which measured-energy backends are configured, frozen at startup.
    /// Lets `/api/energy` report `configured` truthfully instead of
    /// inferring it from zero-valued metrics (the gauges are
    /// pre-registered at 0 whether or not a backend exists).
    pub energy_backends: EnergyBackendsConfigured,
}

/// Configured-or-not flags for the four scraped energy backends.
/// Electricity Maps is not here: its presence is already carried by
/// `QueryApiState.scoring_config`.
// Four genuinely independent flags, not a disguised state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default)]
pub struct EnergyBackendsConfigured {
    pub scaphandre: bool,
    pub kepler: bool,
    pub redfish: bool,
    pub cloud_energy: bool,
}

/// Build the query API router.
pub fn query_api_router(state: Arc<QueryApiState>) -> Router {
    Router::new()
        .route("/api/findings", get(handle_findings))
        .route("/api/findings/{trace_id}", get(handle_findings_by_trace))
        .route("/api/explain/{trace_id}", get(handle_explain))
        .route("/api/correlations", get(handle_correlations))
        .route("/api/status", get(handle_status))
        .route("/api/config", get(handle_config))
        .route("/api/energy", get(handle_energy))
        .route("/api/export/report", get(handle_export_report))
        .route(
            "/api/findings/{signature}/ack",
            post(handle_ack).delete(handle_unack),
        )
        .route("/api/acks", get(handle_list_acks))
        .with_state(state)
}

// ── Query parameters ──────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct FindingsParams {
    service: Option<String>,
    #[serde(rename = "type")]
    finding_type: Option<String>,
    severity: Option<String>,
    limit: Option<usize>,
    /// Default `false`: filter out findings that are acked (CI TOML
    /// baseline + daemon JSONL store union). `true`: return all
    /// findings, with `acknowledged_by` populated for acked ones.
    #[serde(default)]
    include_acked: bool,
}

#[derive(Debug, Deserialize, Default)]
struct AckRequest {
    by: Option<String>,
    reason: Option<String>,
    expires_at: Option<DateTime<Utc>>,
}

/// CI TOML ack with its expiry pre-parsed at startup. Stored in
/// [`QueryApiState::toml_acks`] so the per-request `lookup_ack` path
/// avoids re-parsing the wire-format `expires_at: Option<String>` on
/// every finding.
#[derive(Debug, Clone)]
pub struct ResolvedTomlAck {
    pub inner: Acknowledgment,
    /// Pre-parsed expiry, end-of-day in UTC for the `YYYY-MM-DD` value.
    /// `None` means the ack never expires.
    pub expires_at_dt: Option<DateTime<Utc>>,
}

impl ResolvedTomlAck {
    /// Whether this TOML ack is still in force at `now`. Mirrors the
    /// daemon-side [`ack::is_expired`] predicate but adapted to the
    /// pre-parsed end-of-day datetime.
    #[must_use]
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.expires_at_dt.is_none_or(|e| e >= now)
    }
}

/// Source of an ack annotation on a finding response. TOML acks come
/// from the CI baseline file, daemon acks from the runtime JSONL store.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum AckSource {
    Toml {
        acknowledged_by: String,
        acknowledged_at: String,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_at: Option<String>,
    },
    Daemon {
        by: String,
        at: DateTime<Utc>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_at: Option<DateTime<Utc>>,
    },
}

/// Wrapper around [`StoredFinding`] adding an optional ack annotation.
///
/// `#[serde(flatten)]` keeps the JSON shape identical to
/// `StoredFinding` (preserving backward compatibility) when
/// `acknowledged_by` is `None`. The field appears only when the request
/// passed `?include_acked=true` and the finding has an active ack.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct FindingResponse {
    #[serde(flatten)]
    pub stored: StoredFinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_by: Option<AckSource>,
}

// ── Response types ────────────────────────────────────────────────

/// `GET /api/status` response. The gauge/capacity pairs (`active_traces`
/// vs `max_active_traces`, `analysis_queue_depth` vs
/// `analysis_queue_capacity`, `stored_findings` vs
/// `max_retained_findings`) back the headroom chart of `query monitor`'s
/// Trends tab: each pair reads as "how close is this runtime gauge to
/// its configured cap". Additive since 0.8.8, older clients ignore the
/// new fields.
#[derive(Serialize)]
struct StatusResponse {
    version: &'static str,
    uptime_seconds: u64,
    active_traces: usize,
    max_active_traces: usize,
    analysis_queue_depth: i64,
    analysis_queue_capacity: usize,
    stored_findings: usize,
    max_retained_findings: usize,
}

// ── Handlers ──────────────────────────────────────────────────────

async fn handle_findings(
    State(state): State<Arc<QueryApiState>>,
    Query(params): Query<FindingsParams>,
) -> Json<Vec<FindingResponse>> {
    // Cap the limit to protect the daemon from expensive responses
    // (large JSON serialization under an unauthenticated loopback API).
    let include_acked = params.include_acked;
    let filter = FindingsFilter {
        service: params.service,
        finding_type: params.finding_type,
        severity: params.severity,
        limit: params.limit.unwrap_or(100).min(MAX_FINDINGS_LIMIT),
    };
    let stored = state.findings_store.query(&filter).await;
    let daemon_snapshot: Arc<HashMap<String, AckEntry>> = match &state.ack_store {
        Some(s) => s.snapshot_active().await,
        None => Arc::new(HashMap::new()),
    };
    let now = Utc::now();
    let result: Vec<FindingResponse> = stored
        .into_iter()
        .filter_map(|s| {
            // event_loop calls `enrich_with_signatures` before storing.
            // Empty-sig is the pre-0.5.17 replay path; surfacing it
            // helps operators notice a bypassed enrich step.
            let owned_sig: String;
            let sig: &str = if s.finding.signature.is_empty() {
                tracing::warn!(
                    finding_type = s.finding.finding_type.as_str(),
                    "stored finding had empty signature, recomputing on the read path"
                );
                owned_sig = compute_signature(&s.finding);
                &owned_sig
            } else {
                &s.finding.signature
            };
            let ack = lookup_ack(sig, &state.toml_acks, &daemon_snapshot, now);
            match (include_acked, ack) {
                (false, Some(_)) => None,
                (false, None) => Some(FindingResponse {
                    stored: s,
                    acknowledged_by: None,
                }),
                (true, src) => Some(FindingResponse {
                    stored: s,
                    acknowledged_by: src,
                }),
            }
        })
        .collect();
    Json(result)
}

fn lookup_ack(
    signature: &str,
    toml: &HashMap<String, ResolvedTomlAck>,
    daemon: &HashMap<String, AckEntry>,
    now: DateTime<Utc>,
) -> Option<AckSource> {
    if let Some(t) = toml.get(signature)
        && t.is_active(now)
    {
        return Some(AckSource::Toml {
            acknowledged_by: t.inner.acknowledged_by.clone(),
            acknowledged_at: t.inner.acknowledged_at.clone(),
            reason: t.inner.reason.clone(),
            expires_at: t.inner.expires_at.clone(),
        });
    }
    if let Some(d) = daemon.get(signature)
        && !ack::is_expired(d, now)
    {
        return Some(AckSource::Daemon {
            by: d.by.clone(),
            at: d.at,
            reason: d.reason.clone(),
            expires_at: d.expires_at,
        });
    }
    None
}

async fn handle_findings_by_trace(
    State(state): State<Arc<QueryApiState>>,
    Path(trace_id): Path<String>,
) -> Json<Vec<StoredFinding>> {
    // Cap for defense-in-depth, consistent with `/api/findings`. In normal
    // traffic a trace has a handful of findings, but a pathological trace
    // with hundreds of N+1 clusters is possible; the cap prevents a large
    // serialization under an unauthenticated loopback API.
    let mut results = state.findings_store.by_trace_id(&trace_id).await;
    results.truncate(MAX_FINDINGS_LIMIT);
    Json(results)
}

async fn handle_explain(
    State(state): State<Arc<QueryApiState>>,
    Path(trace_id): Path<String>,
) -> Json<serde_json::Value> {
    // Look up the trace in the window (if still in memory). The clone
    // happens inside the window lock, but is bounded by
    // `max_events_per_trace` (config default 1000) so the critical
    // section stays short. A pathological trace with many spans could
    // briefly block `process_traces`; the `{}` scope releases the lock
    // as soon as the clone completes.
    let maybe_spans = {
        let window = state.window.lock().await;
        window.peek_clone(&trace_id)
    };

    let value = match maybe_spans {
        Some(spans) => {
            let trace = Trace {
                trace_id: trace_id.clone(),
                spans,
            };
            let findings = detect::detect(std::slice::from_ref(&trace), &state.detect_config);
            let tree = explain::build_tree(&trace, &findings);
            // Serialize directly to Value (one allocation) instead of
            // to_string + from_str (three allocations).
            serde_json::to_value(&tree)
                .unwrap_or_else(|_| serde_json::json!({"error": "failed to format explain tree"}))
        }
        None => serde_json::json!({"error": "trace not found in daemon memory"}),
    };
    Json(value)
}

async fn handle_correlations(
    State(state): State<Arc<QueryApiState>>,
) -> Json<Vec<CrossTraceCorrelation>> {
    match &state.correlator {
        Some(correlator) => {
            let mut correlations = correlator.lock().await.active_correlations();
            // Cap response size. Sort by confidence descending so the
            // most-significant correlations survive the truncation.
            // `f64::total_cmp` provides a total order and handles NaN
            // deterministically (NaN sorts last), so we do not need
            // `partial_cmp(...).unwrap_or(Equal)` to guard invariants.
            correlations.sort_by(|a, b| {
                b.confidence
                    .total_cmp(&a.confidence)
                    .then_with(|| b.co_occurrence_count.cmp(&a.co_occurrence_count))
            });
            correlations.truncate(MAX_CORRELATIONS_LIMIT);
            Json(correlations)
        }
        None => Json(vec![]),
    }
}

async fn handle_status(State(state): State<Arc<QueryApiState>>) -> Json<StatusResponse> {
    let uptime = state.start_time.elapsed().as_secs();
    let active_traces = state.window.lock().await.active_traces();
    let stored_findings = state.findings_store.len().await;
    Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: uptime,
        active_traces,
        max_active_traces: state.daemon_config.max_active_traces,
        analysis_queue_depth: state.metrics.analysis_queue_depth.get(),
        analysis_queue_capacity: state.daemon_config.analysis_queue_capacity,
        stored_findings,
        max_retained_findings: state.daemon_config.max_retained_findings,
    })
}

/// `GET /api/config` response: the daemon's effective `[daemon]`
/// configuration, read-only, for the monitor's Config tab. Built as an
/// explicit allowlist (never a blanket `Serialize` of `DaemonConfig`)
/// so no current or future secret leaks: TLS paths and the ack API key
/// are summarized to booleans, never echoed. Additive since 0.8.8.
// Independent config flags mirrored verbatim, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Serialize)]
struct ConfigResponse {
    listen_addr: String,
    listen_port: u16,
    listen_port_grpc: u16,
    json_socket: String,
    max_active_traces: usize,
    trace_ttl_ms: u64,
    sampling_rate: f64,
    max_events_per_trace: usize,
    max_payload_size: usize,
    environment: &'static str,
    max_retained_findings: usize,
    ingest_queue_capacity: usize,
    analysis_queue_capacity: usize,
    api_enabled: bool,
    /// True when both TLS cert and key paths are set (paths themselves
    /// never exposed).
    tls_configured: bool,
    ack_enabled: bool,
    /// True when an ack API key is configured (the key itself never
    /// exposed).
    ack_api_key_set: bool,
    cors_allowed_origins: Vec<String>,
    archive_configured: bool,
    correlation_enabled: bool,
    correlation_window_ms: u64,
    correlation_lag_threshold_ms: u64,
    correlation_min_co_occurrences: u32,
    correlation_min_confidence: f64,
    correlation_max_tracked_pairs: usize,
}

async fn handle_config(State(state): State<Arc<QueryApiState>>) -> Json<ConfigResponse> {
    let d = &state.daemon_config;
    Json(ConfigResponse {
        listen_addr: d.listen_addr.clone(),
        listen_port: d.listen_port,
        listen_port_grpc: d.listen_port_grpc,
        json_socket: d.json_socket.clone(),
        max_active_traces: d.max_active_traces,
        trace_ttl_ms: d.trace_ttl_ms,
        sampling_rate: d.sampling_rate,
        max_events_per_trace: d.max_events_per_trace,
        max_payload_size: d.max_payload_size,
        environment: d.environment.as_str(),
        max_retained_findings: d.max_retained_findings,
        ingest_queue_capacity: d.ingest_queue_capacity,
        analysis_queue_capacity: d.analysis_queue_capacity,
        api_enabled: d.api_enabled,
        tls_configured: d.tls.cert_path.is_some() && d.tls.key_path.is_some(),
        ack_enabled: d.ack.enabled,
        ack_api_key_set: d.ack.api_key.is_some(),
        cors_allowed_origins: d.cors.allowed_origins.clone(),
        archive_configured: d.archive.is_some(),
        correlation_enabled: d.correlation.enabled,
        correlation_window_ms: d.correlation.window_ms,
        correlation_lag_threshold_ms: d.correlation.lag_threshold_ms,
        correlation_min_co_occurrences: d.correlation.min_co_occurrences,
        correlation_min_confidence: d.correlation.min_confidence,
        correlation_max_tracked_pairs: d.correlation.max_tracked_pairs,
    })
}

/// One energy backend's live health on `/api/energy`.
///
/// `last_scrape_age_seconds` and the scrape counters are `None` when the
/// backend is not configured (its pre-registered metrics would read as a
/// misleading fresh 0) or when the backend has no such metric at all
/// (`cloud_energy` has no scrape counters, `electricity_maps` has no
/// freshness gauge: its liveness shows as `intensity_source = real_time`
/// on the report's region breakdown).
#[derive(Debug, Serialize, Deserialize)]
pub struct EnergyBackendStatus {
    /// Stable backend name: `scaphandre`, `kepler`, `redfish`,
    /// `cloud_energy` or `electricity_maps`.
    pub backend: String,
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scrape_age_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrapes_ok: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrapes_failed: Option<u64>,
}

/// `GET /api/energy` response: the five energy/intensity backends in a
/// fixed order. Additive surface for operator tooling (`query monitor`);
/// the per-service/per-region mix itself lives on `/api/export/report`.
#[derive(Debug, Serialize, Deserialize)]
pub struct EnergyStatusResponse {
    pub backends: Vec<EnergyBackendStatus>,
}

/// Live health of the energy backends, from the shared metrics registry
/// plus the configured flags frozen at startup. One table row per
/// backend keeps the name, flag, gauge and counters together: adding a
/// sixth backend is one row here plus its flag in
/// [`EnergyBackendsConfigured`], not three uncoupled edits.
/// Deliberately bumps no request counter: the monitor polls it once per
/// refresh tick, and the other read-only endpoints are not counted either.
async fn handle_energy(State(state): State<Arc<QueryApiState>>) -> Json<EnergyStatusResponse> {
    type CounterPair<'a> = (&'a prometheus::IntCounter, &'a prometheus::IntCounter);
    let m = &state.metrics;
    let b = state.energy_backends;
    let rows: [(&str, bool, Option<&prometheus::Gauge>, Option<CounterPair>); 5] = [
        (
            "scaphandre",
            b.scaphandre,
            Some(&m.scaphandre_last_scrape_age_seconds),
            Some((&m.scaphandre_scrape_success, &m.scaphandre_scrape_failed)),
        ),
        (
            "kepler",
            b.kepler,
            Some(&m.kepler_last_scrape_age_seconds),
            Some((&m.kepler_scrape_success, &m.kepler_scrape_failed)),
        ),
        (
            "redfish",
            b.redfish,
            Some(&m.redfish_last_scrape_age_seconds),
            Some((&m.redfish_scrape_success, &m.redfish_scrape_failed)),
        ),
        // No scrape counters by design (interval evaluation, not a scrape).
        (
            "cloud_energy",
            b.cloud_energy,
            Some(&m.cloud_energy_last_scrape_age_seconds),
            None,
        ),
        // No freshness gauge by design: liveness shows as real_time
        // intensity sources on the report's region breakdown.
        (
            "electricity_maps",
            state.scoring_config.is_some(),
            None,
            None,
        ),
    ];
    let backends = rows
        .into_iter()
        .map(|(name, configured, age, counters)| EnergyBackendStatus {
            backend: name.to_string(),
            configured,
            last_scrape_age_seconds: if configured {
                age.map(prometheus::Gauge::get)
            } else {
                None
            },
            scrapes_ok: if configured {
                counters.map(|(ok, _)| ok.get())
            } else {
                None
            },
            scrapes_failed: if configured {
                counters.map(|(_, failed)| failed.get())
            } else {
                None
            },
        })
        .collect();
    Json(EnergyStatusResponse { backends })
}

/// Snapshot the daemon's in-memory state as a [`Report`], in the same
/// JSON shape as `analyze --format json` (pipeable into
/// `perf-sentinel report --input -`).
///
/// Contract highlights (full semantics in
/// `docs/design/06-INGESTION-AND-DAEMON.md` § "/api/export/report
/// snapshot semantics"):
/// - `green_summary` is a **per-batch view** (most recent batch only);
///   `analysis.events_processed` / `traces_analyzed` are lifetime
///   counters; `analysis.duration_ms` is `0`.
/// - Cold start returns `200 OK` with an empty envelope, gated on the
///   double counter check (`events_processed > 0` AND
///   `traces_analyzed > 0`).
/// - Response size bounded (~3.5 MB worst case), sized for the
///   documented loopback posture.
///
/// TODO: the `Report` assembly below duplicates the one in
/// `pipeline::analyze`. When a third call site lands, factor into
/// `report::build_report(...)` and call it from both.
async fn handle_export_report(State(state): State<Arc<QueryApiState>>) -> Json<Report> {
    state.metrics.export_report_requests_total.inc();

    // Prometheus counters are f64 internally. Daemon-lifetime counts
    // easily fit in u64 and we never decrement, so a saturating cast
    // via `as` is safe. The two reads are not atomic as a pair, a
    // concurrent `inc_by` in the event loop could race between them,
    // the values are monotonic and informational so the worst case is
    // a report where `events_processed > 0` and `traces_analyzed = 0`
    // for a few microseconds around the first batch.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let events_processed = state.metrics.events_processed_total.get() as u64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let traces_analyzed = state.metrics.traces_analyzed_total.get() as u64;

    // Cold-start path: gating on both counters keeps the snapshot
    // self-consistent (the cell holds at least one real batch result by
    // the time `traces_analyzed > 0`). Return an empty envelope with a
    // warning string so consumers can distinguish "no events yet" from
    // "events exist, zero findings" without a 5xx HTTP status.
    if events_processed == 0 || traces_analyzed == 0 {
        let mut green_summary = GreenSummary::disabled(0);
        green_summary
            .scoring_config
            .clone_from(&state.scoring_config);
        return Json(Report {
            analysis: Analysis {
                duration_ms: 0,
                events_processed: 0,
                traces_analyzed: 0,
            },
            findings: Vec::new(),
            green_summary,
            quality_gate: QualityGate {
                passed: true,
                rules: Vec::new(),
            },
            per_endpoint_io_ops: Vec::new(),
            correlations: Vec::new(),
            warnings: vec!["daemon has not yet processed any events".to_string()],
            warning_details: vec![crate::report::Warning::new(
                crate::report::warnings::COLD_START,
                "daemon has not yet processed any events",
            )],
            acknowledged_findings: Vec::new(),
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
            disclosure_waste: None,
        });
    }

    // Snapshot findings. Cap at MAX_FINDINGS_LIMIT to mirror
    // `/api/findings`, a huge ring buffer should not serialize into
    // an unbounded response body.
    let stored = state
        .findings_store
        .query(&FindingsFilter {
            service: None,
            finding_type: None,
            severity: None,
            limit: MAX_FINDINGS_LIMIT,
        })
        .await;
    let findings: Vec<_> = stored.into_iter().map(|s| s.finding).collect();

    // Snapshot correlations, sorted + capped identically to
    // `/api/correlations` so both endpoints stay consistent.
    let correlations = if let Some(correlator) = &state.correlator {
        let mut list = correlator.lock().await.active_correlations();
        list.sort_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then_with(|| b.co_occurrence_count.cmp(&a.co_occurrence_count))
        });
        list.truncate(MAX_CORRELATIONS_LIMIT);
        list
    } else {
        vec![]
    };

    // Read the live `GreenSummary` populated by the event loop after
    // each batch. The event loop emits the per-batch summary without
    // the audit-trail metadata, the handler stitches `scoring_config`
    // back from the daemon's startup config.
    let mut green_summary = state.green_summary.read().await.clone();
    green_summary
        .scoring_config
        .clone_from(&state.scoring_config);
    let quality_gate = QualityGate {
        passed: true,
        rules: vec![],
    };

    // usize::try_from guards 32-bit targets where a 5-billion-event
    // counter would overflow a usize. On 64-bit the fallback branch is
    // unreachable in practice (2^63 events at 1 M/s = 290 000 years).
    // When we do saturate on 32-bit, surface a warning: a usize::MAX
    // counter in the dashboard is far more misleading than silent
    // truncation to an observably large number would be, so the log
    // record is the user's only signal that the field is saturated.
    let events_usize = usize::try_from(events_processed).unwrap_or_else(|_| {
        tracing::warn!(
            counter = events_processed,
            "events_processed overflowed usize on this target, saturating in export"
        );
        usize::MAX
    });
    let traces_usize = usize::try_from(traces_analyzed).unwrap_or_else(|_| {
        tracing::warn!(
            counter = traces_analyzed,
            "traces_analyzed overflowed usize on this target, saturating in export"
        );
        usize::MAX
    });

    let warning_details = collect_warning_details(&state.metrics, &state.daemon_config);

    let report = Report {
        analysis: Analysis {
            // Explicitly zero rather than the daemon uptime, see the
            // doc comment above for the rationale.
            duration_ms: 0,
            events_processed: events_usize,
            traces_analyzed: traces_usize,
        },
        findings,
        green_summary,
        quality_gate,
        per_endpoint_io_ops: vec![],
        correlations,
        warnings: vec![],
        warning_details,
        acknowledged_findings: vec![],
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        disclosure_waste: None,
    };

    Json(report)
}

/// Validate the two preconditions every ack endpoint shares: a valid
/// `X-API-Key` when `[daemon.ack] api_key` is set, and an enabled
/// store. Records the matching `AckFailureReason` before returning so
/// every error path is observable in `/metrics`.
fn check_ack_preconditions<'a>(
    state: &'a Arc<QueryApiState>,
    headers: &HeaderMap,
    action: AckAction,
) -> Result<&'a Arc<AckStore>, ErrorResponse> {
    if let Err(e) = check_ack_auth(headers, state.ack_api_key.as_deref()) {
        state
            .metrics
            .record_ack_failure(action, AckFailureReason::Unauthorized);
        return Err(e);
    }
    let Some(store) = state.ack_store.as_ref() else {
        state
            .metrics
            .record_ack_failure(action, AckFailureReason::NoStore);
        return Err(ErrorResponse::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ack store disabled",
        ));
    };
    Ok(store)
}

async fn handle_ack(
    State(state): State<Arc<QueryApiState>>,
    Path(signature): Path<String>,
    headers: HeaderMap,
    Json(body): Json<AckRequest>,
) -> Result<StatusCode, ErrorResponse> {
    let store = check_ack_preconditions(&state, &headers, AckAction::Ack)?;
    // Refuse to write a daemon ack on a signature that already has an
    // active TOML baseline. Without this check the daemon line would
    // be appended to JSONL but `lookup_ack` would silently surface the
    // TOML metadata in the response, leaving the operator confused
    // about which entry "took effect".
    if let Some(t) = state.toml_acks.get(&signature)
        && t.is_active(Utc::now())
    {
        state
            .metrics
            .record_ack_failure(AckAction::Ack, AckFailureReason::AlreadyAcked);
        return Err(ErrorResponse::new(
            StatusCode::CONFLICT,
            "signature is acked by the CI TOML baseline, edit the file via PR review",
        ));
    }
    let by = resolve_by(&headers, body.by.as_deref());
    let entry = AckEntry {
        action: AckAction::Ack,
        signature,
        by,
        reason: body.reason,
        at: Utc::now(),
        expires_at: body.expires_at,
    };
    match store.ack(entry).await {
        Ok(()) => {
            state.metrics.record_ack_success(AckAction::Ack);
            Ok(StatusCode::CREATED)
        }
        Err(AckError::AlreadyAcked) => {
            state
                .metrics
                .record_ack_failure(AckAction::Ack, AckFailureReason::AlreadyAcked);
            Err(ErrorResponse::new(StatusCode::CONFLICT, "already acked"))
        }
        Err(AckError::InvalidSignature) => {
            state
                .metrics
                .record_ack_failure(AckAction::Ack, AckFailureReason::InvalidSignature);
            Err(ErrorResponse::new(
                StatusCode::BAD_REQUEST,
                "invalid signature format",
            ))
        }
        Err(AckError::LimitReached) => {
            state
                .metrics
                .record_ack_failure(AckAction::Ack, AckFailureReason::LimitReached);
            Err(ErrorResponse::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "active ack limit reached",
            ))
        }
        Err(AckError::FileTooLarge) => {
            state
                .metrics
                .record_ack_failure(AckAction::Ack, AckFailureReason::FileTooLarge);
            Err(ErrorResponse::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "ack file size cap reached",
            ))
        }
        Err(AckError::EntryTooLarge) => {
            state
                .metrics
                .record_ack_failure(AckAction::Ack, AckFailureReason::EntryTooLarge);
            Err(ErrorResponse::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "ack entry size cap reached",
            ))
        }
        Err(e) => {
            state
                .metrics
                .record_ack_failure(AckAction::Ack, AckFailureReason::InternalError);
            tracing::error!(error = %e, "ack store write failed");
            Err(ErrorResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ack store write failed",
            ))
        }
    }
}

async fn handle_unack(
    State(state): State<Arc<QueryApiState>>,
    Path(signature): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ErrorResponse> {
    let store = check_ack_preconditions(&state, &headers, AckAction::Unack)?;
    let by = resolve_by(&headers, None);
    match store.unack(&signature, &by).await {
        Ok(()) => {
            state.metrics.record_ack_success(AckAction::Unack);
            Ok(StatusCode::NO_CONTENT)
        }
        Err(AckError::NotAcked) => {
            state
                .metrics
                .record_ack_failure(AckAction::Unack, AckFailureReason::NotAcked);
            Err(ErrorResponse::new(StatusCode::NOT_FOUND, "not acked"))
        }
        Err(AckError::InvalidSignature) => {
            state
                .metrics
                .record_ack_failure(AckAction::Unack, AckFailureReason::InvalidSignature);
            Err(ErrorResponse::new(
                StatusCode::BAD_REQUEST,
                "invalid signature format",
            ))
        }
        Err(e) => {
            state
                .metrics
                .record_ack_failure(AckAction::Unack, AckFailureReason::InternalError);
            tracing::error!(error = %e, "ack store unack failed");
            Err(ErrorResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ack store write failed",
            ))
        }
    }
}

async fn handle_list_acks(State(state): State<Arc<QueryApiState>>) -> Json<Vec<AckEntry>> {
    let mut all = match &state.ack_store {
        Some(s) => s.list_active().await,
        None => Vec::new(),
    };
    all.truncate(MAX_ACKS_RESPONSE);
    Json(all)
}

/// Resolve the audit `by` field: `X-User-Id` header (priority), JSON
/// body, then `"anonymous"` fallback. Stripped of `BiDi` / invisible
/// characters.
fn resolve_by(headers: &HeaderMap, body_by: Option<&str>) -> String {
    let raw = headers
        .get("X-User-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| body_by.map(str::to_string))
        .unwrap_or_else(|| "anonymous".to_string());
    crate::text_safety::strip_bidi_and_invisible(&raw).into_owned()
}

/// Validate the optional `X-API-Key` header against the configured
/// secret using a constant-time comparison.
fn check_ack_auth(headers: &HeaderMap, expected: Option<&str>) -> Result<(), ErrorResponse> {
    use subtle::ConstantTimeEq;
    let Some(expected_key) = expected else {
        return Ok(());
    };
    let provided = headers
        .get(crate::http_client::API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if provided.as_bytes().ct_eq(expected_key.as_bytes()).into() {
        Ok(())
    } else {
        Err(ErrorResponse::new(
            StatusCode::UNAUTHORIZED,
            "missing or invalid X-API-Key",
        ))
    }
}

struct ErrorResponse {
    status: StatusCode,
    message: &'static str,
}

impl ErrorResponse {
    const fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message }
    }
}

impl IntoResponse for ErrorResponse {
    fn into_response(self) -> Response {
        let body = serde_json::json!({"error": self.message});
        (self.status, Json(body)).into_response()
    }
}

/// Trace-window occupancy ratio above which the tuning advisor flags
/// `max_active_traces` as undersized.
const TUNING_ACTIVE_TRACES_RATIO: f64 = 0.9;

/// Minimum received-span count before zero retention is meaningful.
const TUNING_ZERO_RETENTION_MIN_RECEIVED: u64 = 1_000;

/// Surface aggregated soft conditions in `Report.warning_details`, on
/// top of the /metrics counters. Operators reading `/api/export/report`
/// do not always scrape Prometheus, so a count of dropped requests
/// visible here gives a fast "is the daemon backpressured?" signal.
///
/// The `tuning` entries are the daemon's settings advisor: each rule
/// compares a metric (lifetime counters, plus the point-in-time
/// `active_traces` gauge for the trace-window rule, which therefore
/// appears and disappears with the load) against the daemon config
/// frozen at startup and, when a knob looks undersized for the
/// observed load, emits a hint naming the knob, its current value and
/// the suggested adjustment. All inputs are trusted (Prometheus
/// counters and parsed config), so `Warning::new` applies.
///
/// Note: the cold-start branch in `handle_export_report` returns before
/// reaching this helper, so `cold_start` never appears together with
/// these kinds in a single response by design.
/// Sum of filtered OTLP spans that signal an instrumentation gap.
/// Excludes `NonSqlDatastore`: those drops are deliberate (Redis/Mongo and
/// other non-SQL stores are not modeled), so a cache-only fleet must not
/// trip the zero-retention warning.
fn instrumentation_gap_filtered(metrics: &MetricsState) -> u64 {
    use crate::report::metrics::OtlpSpanFilterReason;
    OtlpSpanFilterReason::ALL
        .iter()
        .filter(|r| !matches!(r, OtlpSpanFilterReason::NonSqlDatastore))
        .map(|r| {
            metrics
                .otlp_spans_filtered_total
                .with_label_values(&[r.as_str()])
                .get()
        })
        .sum()
}

fn collect_warning_details(
    metrics: &MetricsState,
    daemon: &crate::config::DaemonConfig,
) -> Vec<crate::report::Warning> {
    use crate::report::warnings::{INGESTION_DROPS, TUNING};

    let mut details = Vec::new();
    let dropped = metrics.otlp_rejected_channel_full.get();
    if dropped > 0 {
        details.push(crate::report::Warning::new(
            INGESTION_DROPS,
            format!(
                "{dropped} OTLP requests rejected since daemon start \
                 (channel saturation, see perf_sentinel_otlp_rejected_total)"
            ),
        ));
        let cap = daemon.ingest_queue_capacity;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "{dropped} OTLP requests hit a full ingest queue: raise \
                 [daemon] ingest_queue_capacity (currently {cap}) or \
                 spread ingestion across more daemons"
            ),
        ));
    }

    let shed = metrics.analysis_shed_batches_total.get();
    if shed > 0 {
        let cap = daemon.analysis_queue_capacity;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "analysis worker shed {shed} batches since daemon start: \
                 raise [daemon] analysis_queue_capacity (currently {cap}) \
                 or give the daemon more CPU so detection keeps up"
            ),
        ));
    }

    #[allow(clippy::cast_precision_loss)]
    let active_cap = daemon.max_active_traces as f64;
    let active = metrics.active_traces.get();
    if active >= active_cap * TUNING_ACTIVE_TRACES_RATIO {
        let cap = daemon.max_active_traces;
        let ttl = daemon.trace_ttl_ms;
        // Derive the displayed percentage from the const so the message
        // cannot drift from the actual threshold.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pct = (TUNING_ACTIVE_TRACES_RATIO * 100.0).round() as u32;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "active traces ({active:.0}) are within {pct}% of [daemon] \
                 max_active_traces ({cap}): raise the cap or lower \
                 trace_ttl_ms (currently {ttl} ms) so LRU eviction does \
                 not split live traces"
            ),
        ));
    }

    let overflow = metrics.service_io_ops_overflow_total.get();
    if overflow > 0 {
        let cap = super::event_loop::MAX_SERVICE_CARDINALITY;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "{overflow} I/O operations landed beyond the {cap}-service \
                 metering cap: per-service GreenOps attribution is \
                 undercounting, aggregate or reduce service names upstream"
            ),
        ));
    }

    let evicted = metrics.correlator_pairs_evicted_total.get();
    if daemon.correlation.enabled && evicted > 0 {
        let cap = daemon.correlation.max_tracked_pairs;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "{evicted} service pairs dropped at the correlation cap: \
                 raise [daemon.correlation] max_tracked_pairs (currently \
                 {cap}) or disable correlation on wide topologies"
            ),
        ));
    }

    // A high not_io share is healthy on a well-instrumented fleet
    // exporting all its spans; the actionable signal is ZERO retention:
    // spans keep arriving and not one is analyzable.
    let received = metrics.otlp_spans_received_total.get();
    if received >= TUNING_ZERO_RETENTION_MIN_RECEIVED {
        let filtered = instrumentation_gap_filtered(metrics);
        if filtered >= received {
            details.push(crate::report::Warning::new(
                TUNING,
                format!(
                    "all {received} received OTLP spans were filtered as \
                     non-analyzable (no db.statement, no http.url): the \
                     daemon will never produce findings, check the \
                     instrumentation exports I/O attributes or point \
                     instrumented services at this endpoint"
                ),
            ));
        }
    }

    details
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Build a `QueryApiState` for tests, wiring an optional correlator.
    /// The three concrete test-site constructions only differed by the
    /// correlator slot (None, Some(A), Some(B)); every other field used
    /// the same test defaults.
    fn make_state_with_correlator(
        correlator: Option<Arc<tokio::sync::Mutex<CrossTraceCorrelator>>>,
    ) -> Arc<QueryApiState> {
        use crate::correlate::window::WindowConfig;

        Arc::new(QueryApiState {
            findings_store: Arc::new(FindingsStore::new(100)),
            window: Arc::new(tokio::sync::Mutex::new(TraceWindow::new(
                WindowConfig::default(),
            ))),
            detect_config: DetectConfig {
                n_plus_one_threshold: 5,
                window_ms: 500,
                slow_threshold_ms: 500,
                slow_min_occurrences: 3,
                max_fanout: 20,
                chatty_service_min_calls: 15,
                pool_saturation_concurrent_threshold: 10,
                serialized_min_sequential: 3,
                sanitizer_aware_classification: SanitizerAwareMode::default(),
            },
            start_time: std::time::Instant::now(),
            correlator,
            metrics: Arc::new(MetricsState::new()),
            scoring_config: None,
            green_summary: Arc::new(tokio::sync::RwLock::new(GreenSummary::disabled(0))),
            ack_store: None,
            toml_acks: Arc::new(HashMap::new()),
            ack_api_key: None,
            daemon_config: crate::config::DaemonConfig::default(),
            energy_backends: EnergyBackendsConfigured::default(),
        })
    }

    fn make_state() -> Arc<QueryApiState> {
        make_state_with_correlator(None)
    }

    /// Seed a correlator with three rounds of paired events (an
    /// `NPlusOneSql` on `order-svc` immediately followed by
    /// `follow_up_kind` on `payment-svc`, 1 ms apart, 10 s between
    /// rounds). The shape is tuned to the default `min_co_occurrences
    /// = 2` + `min_confidence = 0.5` config used by the two tests
    /// that need at least one active correlation in the result.
    fn seed_correlator_with_pair(
        correlator: &mut CrossTraceCorrelator,
        follow_up_kind: &detect::FindingType,
    ) {
        for i in 0..3 {
            let t = 1_000_000 + i * 10_000;
            let mut fa = crate::test_helpers::make_finding(
                detect::FindingType::NPlusOneSql,
                detect::Severity::Warning,
            );
            fa.service = "order-svc".to_string();
            let _ = correlator.ingest(&[fa], t);
            let mut fb = crate::test_helpers::make_finding(
                follow_up_kind.clone(),
                detect::Severity::Warning,
            );
            fb.service = "payment-svc".to_string();
            let _ = correlator.ingest(&[fb], t + 1_000);
        }
    }

    #[tokio::test]
    async fn status_returns_200() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(status.get("version").is_some());
        assert!(status.get("uptime_seconds").is_some());
        assert!(status.get("active_traces").is_some());
        assert!(status.get("stored_findings").is_some());
        // Gauge/capacity pairs backing the Trends headroom chart (0.8.8).
        // The test config uses the DaemonConfig defaults, so the caps
        // must round-trip as non-zero values.
        assert!(status["max_active_traces"].as_u64().unwrap() > 0);
        assert!(status["analysis_queue_capacity"].as_u64().unwrap() > 0);
        assert!(status["max_retained_findings"].as_u64().unwrap() > 0);
        assert!(status.get("analysis_queue_depth").is_some());
    }

    #[tokio::test]
    async fn config_exposes_daemon_params_without_secrets() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Representative scalars and the correlation sub-block round-trip.
        assert!(cfg["max_active_traces"].as_u64().unwrap() > 0);
        assert_eq!(cfg["environment"], "staging");
        assert!(cfg.get("trace_ttl_ms").is_some());
        assert!(cfg.get("sampling_rate").is_some());
        assert!(cfg.get("correlation_enabled").is_some());
        // Secrets are summarized to booleans, never echoed: no raw key
        // or path fields exist on the response at all.
        assert_eq!(cfg["tls_configured"], false);
        assert_eq!(cfg["ack_api_key_set"], false);
        assert!(cfg.get("api_key").is_none());
        assert!(cfg.get("cert_path").is_none());
        assert!(cfg.get("key_path").is_none());
        assert!(cfg.get("tls").is_none());
    }

    #[tokio::test]
    async fn findings_returns_empty_array() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/findings")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let findings: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(findings.is_empty());
    }

    #[tokio::test]
    async fn correlations_returns_empty_array() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/correlations")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let correlations: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(correlations.is_empty());
    }

    #[tokio::test]
    async fn explain_unknown_trace_returns_error() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/explain/nonexistent-trace")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(val.get("error").is_some());
    }

    #[tokio::test]
    async fn findings_returns_populated_store() {
        let state = make_state();
        let finding = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        state.findings_store.push_batch(&[finding], 1000).await;

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/findings")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stored: Vec<StoredFinding> = serde_json::from_slice(&body).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].finding.finding_type,
            detect::FindingType::NPlusOneSql
        );
    }

    #[tokio::test]
    async fn findings_filters_by_service() {
        let state = make_state();
        let mut f1 = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        f1.service = "order-svc".to_string();
        let mut f2 = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        f2.service = "payment-svc".to_string();
        state.findings_store.push_batch(&[f1, f2], 1000).await;

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/findings?service=order-svc")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stored: Vec<StoredFinding> = serde_json::from_slice(&body).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].finding.service, "order-svc");
    }

    #[tokio::test]
    async fn findings_by_trace_id() {
        let state = make_state();
        let mut f1 = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        f1.trace_id = "trace-abc".to_string();
        let mut f2 = crate::test_helpers::make_finding(
            detect::FindingType::RedundantSql,
            detect::Severity::Info,
        );
        f2.trace_id = "trace-xyz".to_string();
        state.findings_store.push_batch(&[f1, f2], 1000).await;

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/findings/trace-abc")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stored: Vec<StoredFinding> = serde_json::from_slice(&body).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].finding.trace_id, "trace-abc");
    }

    #[test]
    fn stored_finding_serde_roundtrip() {
        let finding = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        let stored = StoredFinding {
            finding,
            stored_at_ms: 12345,
        };
        let json = serde_json::to_string(&stored).unwrap();
        let back: StoredFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back.stored_at_ms, 12345);
        assert_eq!(back.finding.finding_type, detect::FindingType::NPlusOneSql);
    }

    #[tokio::test]
    async fn correlations_returns_active_correlations_when_correlator_present() {
        use crate::detect::correlate_cross::{CorrelationConfig, CrossTraceCorrelator};

        // Build a correlator and ingest a small pattern that should produce
        // a detectable correlation.
        let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
            min_co_occurrences: 2,
            min_confidence: 0.5,
            lag_threshold_ms: 5_000,
            ..Default::default()
        });
        seed_correlator_with_pair(&mut correlator, &detect::FindingType::PoolSaturation);

        let state = make_state_with_correlator(Some(Arc::new(tokio::sync::Mutex::new(correlator))));

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/correlations")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let correlations: Vec<CrossTraceCorrelation> = serde_json::from_slice(&body).unwrap();
        assert!(
            !correlations.is_empty(),
            "expected at least one correlation to be returned"
        );
    }

    #[tokio::test]
    async fn findings_limit_is_capped() {
        let state = make_state();
        // Push more findings than the hard cap (MAX_FINDINGS_LIMIT = 1000).
        let findings: Vec<detect::Finding> = (0..50)
            .map(|i| {
                let mut f = crate::test_helpers::make_finding(
                    detect::FindingType::NPlusOneSql,
                    detect::Severity::Warning,
                );
                f.trace_id = format!("trace-{i}");
                f
            })
            .collect();
        state.findings_store.push_batch(&findings, 1000).await;

        let app = query_api_router(state);
        // Request a huge limit: handler should cap it to MAX_FINDINGS_LIMIT.
        let req = Request::builder()
            .uri("/api/findings?limit=100000")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Store has 50, cap is 1000: response should be 50 (bounded by store).
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stored: Vec<StoredFinding> = serde_json::from_slice(&body).unwrap();
        assert_eq!(stored.len(), 50);
    }

    #[tokio::test]
    async fn handle_export_report_returns_200_with_empty_envelope_on_cold_start() {
        // No events processed yet: the daemon has nothing meaningful
        // to snapshot. Returns 200 with an empty Report envelope and a
        // `warnings` entry. Pre-0.5.16 returned 503, which tripped
        // Kubernetes probes. The empty shape lets clients distinguish
        // "no events yet" from "ran and found nothing" without a 5xx.
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let report: Report =
            serde_json::from_slice(&body).expect("cold-start body must parse as Report");
        assert_eq!(report.analysis.events_processed, 0);
        assert_eq!(report.analysis.traces_analyzed, 0);
        assert_eq!(report.findings.len(), 0);
        assert_eq!(report.green_summary.total_io_ops, 0);
        assert_eq!(
            report.warnings,
            vec!["daemon has not yet processed any events".to_string()]
        );
    }

    #[tokio::test]
    async fn handle_export_report_returns_report_shape_when_events_ingested() {
        use crate::detect::correlate_cross::{CorrelationConfig, CrossTraceCorrelator};

        // Build a state whose lifetime counters are non-zero, whose
        // findings store has at least one entry, and whose correlator
        // holds at least one correlation. That exercises every slot
        // the handler assembles into the Report. The correlator config
        // mirrors `correlations_returns_active_correlations_when_correlator_present`
        // so a handful of co-occurrences is enough to clear the confidence bar.
        let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
            min_co_occurrences: 2,
            min_confidence: 0.5,
            lag_threshold_ms: 5_000,
            ..Default::default()
        });
        seed_correlator_with_pair(&mut correlator, &detect::FindingType::SlowHttp);

        let state = make_state_with_correlator(Some(Arc::new(tokio::sync::Mutex::new(correlator))));

        // Populate both counters + findings store so the handler sees
        // a non-cold-start signal and has a finding to emit.
        state.metrics.events_processed_total.inc_by(42.0);
        state.metrics.traces_analyzed_total.inc_by(5.0);
        let finding = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        state.findings_store.push_batch(&[finding], 1000).await;

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");

        assert_eq!(report.analysis.events_processed, 42);
        assert_eq!(report.analysis.traces_analyzed, 5);
        // duration_ms is intentionally 0 on the export path (see
        // handler doc), not the daemon uptime that an
        // `as_millis()` would produce.
        assert_eq!(report.analysis.duration_ms, 0);
        // The shared green_summary cell is initialized to disabled(0)
        // and this test does not seed it, so total_io_ops stays at 0.
        // The live-write path is exercised by
        // `handle_export_report_serves_live_green_summary_after_batch`.
        assert_eq!(report.green_summary.total_io_ops, 0);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.correlations.len(), 1);
        assert_eq!(report.correlations[0].source.service, "order-svc");
        assert_eq!(report.correlations[0].target.service, "payment-svc");
    }

    #[tokio::test]
    async fn handle_export_report_propagates_scoring_config_on_cold_start() {
        use crate::score::carbon::ScoringConfig;
        use crate::score::electricity_maps::config::{
            ApiVersion, EmissionFactorType, TemporalGranularity,
        };

        // Cold-start path mirror: even when no events have been ingested
        // yet, an operator pulling /api/export/report must see the
        // Electricity Maps audit chip if EM is configured at startup.
        // Regression-guards the `green_summary.scoring_config.clone_from`
        // call on the cold-start branch.
        let scoring = ScoringConfig {
            api_version: ApiVersion::V4,
            emission_factor_type: EmissionFactorType::Lifecycle,
            temporal_granularity: TemporalGranularity::Hourly,
        };

        let mut state_owned = make_state().clone_for_test();
        state_owned.scoring_config = Some(scoring.clone());
        let state = Arc::new(state_owned);
        // Both counters left at 0, exercises the cold-start branch.

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");
        assert_eq!(report.green_summary.scoring_config, Some(scoring));
        assert_eq!(report.warnings.len(), 1);
    }

    #[tokio::test]
    async fn handle_export_report_propagates_scoring_config_when_emaps_configured() {
        use crate::score::carbon::ScoringConfig;
        use crate::score::electricity_maps::config::{
            ApiVersion, EmissionFactorType, TemporalGranularity,
        };

        // Daemon path mirror: the daemon does not run scoring on the
        // /api/export/report snapshot, but the Electricity Maps client
        // configuration is known at startup. The handler must surface
        // it on green_summary.scoring_config so an operator pulling
        // the snapshot does not mistakenly conclude EM is off.
        let scoring = ScoringConfig {
            api_version: ApiVersion::V4,
            emission_factor_type: EmissionFactorType::Direct,
            temporal_granularity: TemporalGranularity::FifteenMinutes,
        };

        let mut state_owned = make_state().clone_for_test();
        state_owned.scoring_config = Some(scoring.clone());
        let state = Arc::new(state_owned);
        state.metrics.events_processed_total.inc_by(1.0);
        state.metrics.traces_analyzed_total.inc_by(1.0);

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");
        assert_eq!(report.green_summary.scoring_config, Some(scoring));
    }

    #[tokio::test]
    async fn handle_export_report_returns_200_with_warnings_when_events_in_but_no_batch_yet() {
        // Cold-start tail: events have been ingested
        // (`events_processed_total > 0`) but the first eviction tick
        // has not fired, so `traces_analyzed_total == 0` and the
        // green_summary cell is still `disabled(0)`. The handler must
        // serve the empty envelope (not 503) to avoid tripping
        // Kubernetes probes during this transient window.
        let state = make_state();
        state.metrics.events_processed_total.inc_by(5.0);
        // traces_analyzed_total left at 0
        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");
        // The handler clamps `events_processed` to 0 on cold-start since
        // the snapshot is meant to look "empty", regardless of the raw
        // counter that may have already incremented.
        assert_eq!(report.analysis.events_processed, 0);
        assert_eq!(report.analysis.traces_analyzed, 0);
        assert_eq!(
            report.warnings,
            vec!["daemon has not yet processed any events".to_string()]
        );
    }

    #[tokio::test]
    async fn handle_export_report_serves_live_green_summary_after_batch() {
        // The cell is mutated by the event loop after each batch. The
        // handler must clone that cell instead of emitting
        // GreenSummary::disabled(0), so live daemon snapshots carry
        // the latest CO2 picture.
        let state = make_state();
        state.metrics.events_processed_total.inc_by(10.0);
        state.metrics.traces_analyzed_total.inc_by(1.0);

        {
            let mut guard = state.green_summary.write().await;
            guard.total_io_ops = 42;
            guard.avoidable_io_ops = 7;
            guard.io_waste_ratio = 0.166;
        }

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");
        assert_eq!(report.green_summary.total_io_ops, 42);
        assert_eq!(report.green_summary.avoidable_io_ops, 7);
        assert!((report.green_summary.io_waste_ratio - 0.166).abs() < 1e-9);
    }

    #[tokio::test]
    async fn handle_export_report_omits_scoring_config_when_emaps_not_configured() {
        // Symmetric guard: when EM is not configured at daemon
        // startup, the snapshot must not advertise a methodology.
        let state = make_state();
        state.metrics.events_processed_total.inc_by(1.0);
        state.metrics.traces_analyzed_total.inc_by(1.0);

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");
        assert!(report.green_summary.scoring_config.is_none());
    }

    #[tokio::test]
    async fn export_report_warning_details_includes_cold_start_kind() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("parses");
        assert_eq!(report.warning_details.len(), 1);
        assert_eq!(report.warning_details[0].kind, "cold_start");
        assert_eq!(
            report.warning_details[0].message,
            "daemon has not yet processed any events"
        );
    }

    #[tokio::test]
    async fn export_report_warning_details_includes_ingestion_drops_when_counter_positive() {
        let state = make_state();
        // Make the cold-start guard pass so the normal path runs.
        state.metrics.events_processed_total.inc_by(1.0);
        state.metrics.traces_analyzed_total.inc_by(1.0);
        // Pre-load the channel_full counter so the normal path picks
        // it up and surfaces an `ingestion_drops` warning.
        state
            .metrics
            .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);
        state
            .metrics
            .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);
        state
            .metrics
            .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);
        state
            .metrics
            .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);
        state
            .metrics
            .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("parses");
        let drops = report
            .warning_details
            .iter()
            .find(|w| w.kind == "ingestion_drops")
            .expect("ingestion_drops warning present");
        assert!(
            drops.message.contains("5 ") && drops.message.contains("OTLP"),
            "message should reference the count and OTLP, got: {}",
            drops.message
        );
        let tuning = report
            .warning_details
            .iter()
            .find(|w| w.kind == "tuning")
            .expect("channel saturation also yields a tuning hint");
        assert!(
            tuning.message.contains("ingest_queue_capacity") && tuning.message.contains("1024"),
            "hint should name the knob and its current value, got: {}",
            tuning.message
        );
    }

    /// Collect only the `tuning` messages for the given state.
    fn tuning_messages(
        metrics: &MetricsState,
        daemon: &crate::config::DaemonConfig,
    ) -> Vec<String> {
        collect_warning_details(metrics, daemon)
            .into_iter()
            .filter(|w| w.kind == crate::report::warnings::TUNING)
            .map(|w| w.message)
            .collect()
    }

    #[test]
    fn tuning_advisor_stays_silent_on_healthy_counters() {
        let metrics = MetricsState::new();
        assert!(tuning_messages(&metrics, &crate::config::DaemonConfig::default()).is_empty());
    }

    #[test]
    fn tuning_advisor_flags_analysis_sheds_with_queue_capacity() {
        let metrics = MetricsState::new();
        metrics.analysis_shed_batches_total.inc_by(7);
        let msgs = tuning_messages(&metrics, &crate::config::DaemonConfig::default());
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].contains("shed 7 batches")
                && msgs[0].contains("analysis_queue_capacity")
                && msgs[0].contains("1024"),
            "got: {}",
            msgs[0]
        );
    }

    #[test]
    fn tuning_advisor_flags_trace_window_near_cap() {
        let metrics = MetricsState::new();
        metrics.active_traces.set(9_500.0);
        let msgs = tuning_messages(&metrics, &crate::config::DaemonConfig::default());
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].contains("max_active_traces")
                && msgs[0].contains("10000")
                && msgs[0].contains("trace_ttl_ms")
                && msgs[0].contains("30000 ms"),
            "got: {}",
            msgs[0]
        );

        metrics.active_traces.set(8_000.0);
        assert!(
            tuning_messages(&metrics, &crate::config::DaemonConfig::default()).is_empty(),
            "below 90% of the cap must not warn"
        );
    }

    #[test]
    fn tuning_advisor_flags_service_cardinality_overflow() {
        let metrics = MetricsState::new();
        metrics.service_io_ops_overflow_total.inc_by(42);
        let msgs = tuning_messages(&metrics, &crate::config::DaemonConfig::default());
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].contains("42 ") && msgs[0].contains("1024-service"),
            "got: {}",
            msgs[0]
        );
    }

    #[test]
    fn tuning_advisor_flags_pair_evictions_only_when_correlation_enabled() {
        let metrics = MetricsState::new();
        metrics.correlator_pairs_evicted_total.inc_by(900);

        let disabled = crate::config::DaemonConfig::default();
        assert!(!disabled.correlation.enabled, "default is opt-in");
        assert!(
            tuning_messages(&metrics, &disabled).is_empty(),
            "no correlator wired, the counter cannot be actionable"
        );

        let mut enabled = crate::config::DaemonConfig::default();
        enabled.correlation.enabled = true;
        let msgs = tuning_messages(&metrics, &enabled);
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].contains("900 service pairs") && msgs[0].contains("max_tracked_pairs"),
            "got: {}",
            msgs[0]
        );
    }

    #[test]
    fn tuning_advisor_flags_zero_span_retention() {
        let metrics = MetricsState::new();
        metrics.otlp_spans_received_total.inc_by(10_000);
        metrics
            .otlp_spans_filtered_total
            .with_label_values(&["not_io"])
            .inc_by(9_000);
        metrics
            .otlp_spans_filtered_total
            .with_label_values(&["missing_db_statement"])
            .inc_by(1_000);
        let msgs = tuning_messages(&metrics, &crate::config::DaemonConfig::default());
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].contains("all 10000 received") && msgs[0].contains("never produce findings"),
            "got: {}",
            msgs[0]
        );
    }

    #[test]
    fn tuning_advisor_tolerates_dominant_but_partial_filtering() {
        // A well-instrumented fleet exporting every span legitimately
        // filters most of them as not_io. As long as SOME spans are
        // retained, the advisor must stay silent.
        let metrics = MetricsState::new();
        metrics.otlp_spans_received_total.inc_by(10_000);
        metrics
            .otlp_spans_filtered_total
            .with_label_values(&["not_io"])
            .inc_by(9_990);
        assert!(
            tuning_messages(&metrics, &crate::config::DaemonConfig::default()).is_empty(),
            "10 retained spans out of 10000 is a healthy fleet, not a defect"
        );
    }

    #[test]
    fn tuning_advisor_ignores_zero_retention_below_min_volume() {
        let metrics = MetricsState::new();
        metrics.otlp_spans_received_total.inc_by(999);
        metrics
            .otlp_spans_filtered_total
            .with_label_values(&["not_io"])
            .inc_by(999);
        assert!(
            tuning_messages(&metrics, &crate::config::DaemonConfig::default()).is_empty(),
            "under {TUNING_ZERO_RETENTION_MIN_RECEIVED} received spans the signal is noise"
        );
    }

    /// GET /api/energy against the given state, parsed.
    async fn fetch_energy(state: Arc<QueryApiState>) -> EnergyStatusResponse {
        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/energy")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&body).expect("parses as EnergyStatusResponse")
    }

    #[tokio::test]
    async fn energy_endpoint_reports_unconfigured_backends_without_metrics() {
        // No backend configured: every row is configured=false with no
        // age/counter fields (the pre-registered zero gauges must not
        // read as a misleading fresh scrape).
        let energy = fetch_energy(make_state()).await;
        assert_eq!(energy.backends.len(), 5);
        for b in &energy.backends {
            assert!(!b.configured, "{} should be unconfigured", b.backend);
            assert!(b.last_scrape_age_seconds.is_none(), "{}", b.backend);
            assert!(b.scrapes_ok.is_none(), "{}", b.backend);
        }
        let names: Vec<&str> = energy.backends.iter().map(|b| b.backend.as_str()).collect();
        assert_eq!(
            names,
            [
                "scaphandre",
                "kepler",
                "redfish",
                "cloud_energy",
                "electricity_maps"
            ]
        );
    }

    #[tokio::test]
    async fn energy_endpoint_reports_configured_backend_metrics() {
        let mut state = (*make_state()).clone_for_test();
        state.energy_backends.scaphandre = true;
        state.metrics.scaphandre_scrape_success.inc_by(7);
        state.metrics.scaphandre_scrape_failed.inc_by(2);
        state.metrics.scaphandre_last_scrape_age_seconds.set(3.5);

        let energy = fetch_energy(Arc::new(state)).await;
        let scaphandre = energy
            .backends
            .iter()
            .find(|b| b.backend == "scaphandre")
            .expect("scaphandre row");
        assert!(scaphandre.configured);
        assert_eq!(scaphandre.scrapes_ok, Some(7));
        assert_eq!(scaphandre.scrapes_failed, Some(2));
        assert!((scaphandre.last_scrape_age_seconds.unwrap() - 3.5).abs() < f64::EPSILON);
        // The others stay unconfigured and field-less.
        let kepler = energy
            .backends
            .iter()
            .find(|b| b.backend == "kepler")
            .unwrap();
        assert!(!kepler.configured);
        assert!(kepler.scrapes_ok.is_none());
    }

    #[tokio::test]
    async fn energy_endpoint_derives_electricity_maps_from_scoring_config() {
        use crate::score::carbon::ScoringConfig;
        use crate::score::electricity_maps::config::{
            ApiVersion, EmissionFactorType, TemporalGranularity,
        };

        let mut state = (*make_state()).clone_for_test();
        state.scoring_config = Some(ScoringConfig {
            api_version: ApiVersion::V4,
            emission_factor_type: EmissionFactorType::Lifecycle,
            temporal_granularity: TemporalGranularity::Hourly,
        });
        let energy = fetch_energy(Arc::new(state)).await;
        let emaps = energy
            .backends
            .iter()
            .find(|b| b.backend == "electricity_maps")
            .expect("electricity_maps row");
        assert!(emaps.configured);
        // No freshness gauge exists for the EM API by design.
        assert!(emaps.last_scrape_age_seconds.is_none());
    }

    #[allow(clippy::unused_async)]
    async fn make_state_with_acks(
        ack_store: Option<Arc<AckStore>>,
        toml_acks: HashMap<String, ResolvedTomlAck>,
        ack_api_key: Option<String>,
    ) -> Arc<QueryApiState> {
        let mut state = (*make_state_with_correlator(None)).clone_for_test();
        state.ack_store = ack_store;
        state.toml_acks = Arc::new(toml_acks);
        state.ack_api_key = ack_api_key;
        Arc::new(state)
    }

    async fn fresh_ack_store() -> (tempfile::TempDir, Arc<AckStore>) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = AckStore::new(dir.path().join("acks.jsonl")).await.unwrap();
        (dir, store)
    }

    /// Test fixture: a TOML baseline ack with no expiry, attributed to
    /// the canned `ci-bot` author. Reused across tests that exercise
    /// the TOML-wins conflict path.
    fn toml_baseline_fixture(sig: &str) -> ResolvedTomlAck {
        ResolvedTomlAck {
            inner: Acknowledgment {
                signature: sig.to_string(),
                acknowledged_by: "ci-bot".to_string(),
                acknowledged_at: "2026-05-04".to_string(),
                reason: "permanent baseline".to_string(),
                expires_at: None,
            },
            expires_at_dt: None,
        }
    }

    /// Build a POST `/api/findings/{sig}/ack` request with an empty
    /// JSON body and no auth headers. Centralizes the boilerplate so
    /// the per-test focus is the assertion, not the HTTP setup.
    fn post_ack_request(sig: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("/api/findings/{sig}/ack"))
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap()
    }

    /// Build a DELETE `/api/findings/{sig}/ack` request, no body, no
    /// auth headers.
    fn delete_ack_request(sig: &str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/findings/{sig}/ack"))
            .body(Body::empty())
            .unwrap()
    }

    /// Build a GET request to `path`, no body.
    fn get_request(path: &str) -> Request<Body> {
        Request::builder().uri(path).body(Body::empty()).unwrap()
    }

    async fn seed_finding(state: &Arc<QueryApiState>, service: &str) -> String {
        let mut f = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        f.service = service.to_string();
        let sig = compute_signature(&f);
        state.findings_store.push_batch(&[f], 1000).await;
        sig
    }

    #[tokio::test]
    async fn ack_endpoint_persists_and_filters_finding() {
        let (_dir, store) = fresh_ack_store().await;
        let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
        let sig = seed_finding(&state, "order-svc").await;

        let app = query_api_router(Arc::clone(&state));
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/findings/{sig}/ack"))
            .header("Content-Type", "application/json")
            .header("X-User-Id", "alice@example.com")
            .body(Body::from("{\"reason\":\"deferred\"}"))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(state.metrics.ack_operations_ack_success.get(), 1);

        let req = Request::builder()
            .uri("/api/findings")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(list.is_empty(), "acked finding should not appear: {list:?}");
    }

    #[tokio::test]
    async fn ack_endpoint_returns_409_when_already_acked() {
        let (_dir, store) = fresh_ack_store().await;
        let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
        let sig = seed_finding(&state, "order-svc").await;
        let app = query_api_router(Arc::clone(&state));
        let resp = app.clone().oneshot(post_ack_request(&sig)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let resp = app.oneshot(post_ack_request(&sig)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            state
                .metrics
                .ack_operations_failed_total
                .with_label_values(&["ack", "already_acked"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn unack_endpoint_makes_finding_reappear() {
        let (_dir, store) = fresh_ack_store().await;
        let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
        let sig = seed_finding(&state, "order-svc").await;
        let app = query_api_router(Arc::clone(&state));

        let resp = app.clone().oneshot(post_ack_request(&sig)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp = app.clone().oneshot(delete_ack_request(&sig)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(state.metrics.ack_operations_unack_success.get(), 1);

        let resp = app.oneshot(get_request("/api/findings")).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn findings_with_include_acked_annotates_daemon_source() {
        let (_dir, store) = fresh_ack_store().await;
        let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
        let sig = seed_finding(&state, "order-svc").await;
        let app = query_api_router(Arc::clone(&state));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/findings/{sig}/ack"))
                    .header("Content-Type", "application/json")
                    .header("X-User-Id", "alice")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp = app
            .oneshot(get_request("/api/findings?include_acked=true"))
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.len(), 1);
        let ack = &list[0]["acknowledged_by"];
        assert_eq!(ack["source"], "daemon");
        assert_eq!(ack["by"], "alice");
    }

    #[tokio::test]
    async fn toml_acks_win_over_daemon_on_conflict() {
        let (_dir, store) = fresh_ack_store().await;
        let state = make_state_with_correlator(None);
        let sig = seed_finding(&state, "order-svc").await;
        let mut toml = HashMap::new();
        toml.insert(sig.clone(), toml_baseline_fixture(&sig));
        let state = make_state_with_acks(Some(store), toml, None).await;
        // Re-seed since make_state_with_acks rebuilt state.
        let sig2 = seed_finding(&state, "order-svc").await;
        assert_eq!(sig, sig2);

        let app = query_api_router(Arc::clone(&state));
        // POST on a TOML-acked signature returns 409 (the daemon will not
        // shadow the immutable baseline with a runtime line that has no
        // visible effect).
        let resp = app.clone().oneshot(post_ack_request(&sig)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        // The TOML ack is still surfaced on the read path with
        // `acknowledged_by.source == "toml"`.
        let resp = app
            .oneshot(get_request("/api/findings?include_acked=true"))
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["acknowledged_by"]["source"], "toml");
        assert_eq!(list[0]["acknowledged_by"]["acknowledged_by"], "ci-bot");
    }

    #[tokio::test]
    async fn ack_endpoint_requires_api_key_when_configured() {
        let (_dir, store) = fresh_ack_store().await;
        let state = make_state_with_acks(
            Some(store),
            HashMap::new(),
            Some("a-long-enough-secret".to_string()),
        )
        .await;
        let sig = seed_finding(&state, "order-svc").await;
        let app = query_api_router(Arc::clone(&state));

        // Missing key: 401
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/findings/{sig}/ack"))
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Wrong key: 401
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/findings/{sig}/ack"))
                    .header("Content-Type", "application/json")
                    .header("X-API-Key", "wrong-key-xxxxxxxxxx")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Correct key: 201
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/findings/{sig}/ack"))
                    .header("Content-Type", "application/json")
                    .header("X-API-Key", "a-long-enough-secret")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        assert_eq!(
            state
                .metrics
                .ack_operations_failed_total
                .with_label_values(&["ack", "unauthorized"])
                .get(),
            2,
            "missing key + wrong key both bump unauthorized"
        );
        assert_eq!(state.metrics.ack_operations_ack_success.get(), 1);
    }

    #[tokio::test]
    async fn ack_failure_increments_no_store_when_disabled() {
        let state = make_state_with_acks(None, HashMap::new(), None).await;
        let sig = seed_finding(&state, "order-svc").await;
        let app = query_api_router(Arc::clone(&state));

        let resp = app.oneshot(post_ack_request(&sig)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            state
                .metrics
                .ack_operations_failed_total
                .with_label_values(&["ack", "no_store"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn toml_conflict_increments_already_acked() {
        let (_dir, store) = fresh_ack_store().await;
        let bootstrap = make_state_with_correlator(None);
        let sig = seed_finding(&bootstrap, "order-svc").await;
        let mut toml = HashMap::new();
        toml.insert(sig.clone(), toml_baseline_fixture(&sig));
        let state = make_state_with_acks(Some(store), toml, None).await;
        let sig2 = seed_finding(&state, "order-svc").await;
        assert_eq!(sig, sig2);

        let app = query_api_router(Arc::clone(&state));
        let resp = app.oneshot(post_ack_request(&sig)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            state
                .metrics
                .ack_operations_failed_total
                .with_label_values(&["ack", "already_acked"])
                .get(),
            1,
            "TOML conflict bumps the same series as AckError::AlreadyAcked"
        );
    }

    #[tokio::test]
    async fn ack_failure_increments_invalid_signature() {
        let (_dir, store) = fresh_ack_store().await;
        let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
        let app = query_api_router(Arc::clone(&state));

        // Tail uppercase hex fails the canonical-format check in
        // `daemon::ack::validate_signature` which requires lowercase
        // hex on the trailing 16-char SHA prefix.
        let bad_sig = "foo:bar:0123456789ABCDEF";
        let resp = app.oneshot(post_ack_request(bad_sig)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            state
                .metrics
                .ack_operations_failed_total
                .with_label_values(&["ack", "invalid_signature"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn list_acks_endpoint_returns_active() {
        let (_dir, store) = fresh_ack_store().await;
        let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
        let sig = seed_finding(&state, "order-svc").await;
        let app = query_api_router(state);

        app.clone().oneshot(post_ack_request(&sig)).await.unwrap();

        let resp = app.oneshot(get_request("/api/acks")).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["signature"], sig);
    }

    #[test]
    fn finding_response_does_not_collide_with_stored_finding_fields() {
        // Pin the JSON shape: `acknowledged_by` lives at the top level
        // alongside the flattened StoredFinding fields, never nested
        // under `finding`. A future refactor that adds an
        // `acknowledged_by` to either StoredFinding or Finding would
        // shadow this and break clients that parse the source field.
        let finding = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        let resp = FindingResponse {
            stored: StoredFinding {
                finding,
                stored_at_ms: 1234,
            },
            acknowledged_by: Some(AckSource::Daemon {
                by: "alice".to_string(),
                at: Utc::now(),
                reason: None,
                expires_at: None,
            }),
        };
        let v = serde_json::to_value(&resp).unwrap();
        let obj = v
            .as_object()
            .expect("FindingResponse serializes as an object");
        assert!(obj.contains_key("stored_at_ms"));
        assert!(obj.contains_key("finding"));
        assert!(obj.contains_key("acknowledged_by"));
        let inner = obj.get("finding").unwrap().as_object().unwrap();
        assert!(
            !inner.contains_key("acknowledged_by"),
            "acknowledged_by must stay at the top level, not nest inside finding"
        );
    }

    impl QueryApiState {
        /// Test-only shallow clone, mirrors every slot via Arc cloning.
        /// `scoring_config` is the only field the new test mutates,
        /// every other field is shared with the original Arc.
        fn clone_for_test(&self) -> Self {
            Self {
                findings_store: Arc::clone(&self.findings_store),
                window: Arc::clone(&self.window),
                detect_config: self.detect_config.clone(),
                start_time: self.start_time,
                correlator: self.correlator.clone(),
                metrics: Arc::clone(&self.metrics),
                scoring_config: self.scoring_config.clone(),
                green_summary: Arc::clone(&self.green_summary),
                ack_store: self.ack_store.clone(),
                toml_acks: Arc::clone(&self.toml_acks),
                ack_api_key: self.ack_api_key.clone(),
                daemon_config: self.daemon_config.clone(),
                energy_backends: self.energy_backends,
            }
        }
    }
}
