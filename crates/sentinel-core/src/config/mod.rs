//! Configuration parsing for `.perf-sentinel.toml`.
//!
//! Supports both the new sectioned format (`[thresholds]`, `[detection]`, `[green]`, `[daemon]`)
//! and the legacy flat format for backward compatibility.

use std::borrow::Cow;
use std::collections::HashMap;
#[cfg(test)]
use std::time::Duration;

use crate::detect::Confidence;
use crate::score::alumet::AlumetConfig;
use crate::score::carbon::DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2;
use crate::score::cloud_energy::config::CloudEnergyConfig;
use crate::score::kepler::KeplerConfig;
use crate::score::redfish::RedfishConfig;
#[cfg(test)]
use crate::score::redfish::RedfishEndpoint;
use crate::score::scaphandre::ScaphandreConfig;

/// Top-level configuration for perf-sentinel.
///
/// Mirrors the four `.perf-sentinel.toml` sections (`[thresholds]`,
/// `[detection]`, `[green]`, `[daemon]`) into typed sub-structs so a
/// consumer that touches only thresholds does not pull a daemon-shaped
/// import surface. The 0.5.x flat layout was unfolded in 0.6.0; see
/// `docs/CONFIGURATION.md` for the rename matrix.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Quality-gate thresholds enforced by `analyze --ci`.
    pub thresholds: ThresholdsConfig,
    /// Per-detector knobs that drive `detect::detect`.
    pub detection: DetectionConfig,
    /// `GreenOps` / SCI-v1.0 scoring config.
    pub green: GreenConfig,
    /// Daemon (`perf-sentinel watch`) runtime config: listeners, ack
    /// store, TLS, CORS, cross-trace correlation.
    pub daemon: DaemonConfig,
    /// Periodic disclosure report config (intent, org-config path, output
    /// destination). Drives daemon startup validation when
    /// `intent = "official"` and is consumed by `perf-sentinel disclose`.
    pub reporting: ReportingConfig,
}

/// Maps 1:1 to `[reporting]` in TOML. All fields optional: an absent
/// section means the operator never asked for a periodic disclosure.
#[derive(Debug, Clone, Default)]
pub struct ReportingConfig {
    /// `"internal"`, `"official"`, or `"audited"`. `None` means no
    /// reporting intent declared.
    pub intent: Option<String>,
    /// `"internal"` or `"public"`. Drives G1 vs G2 granularity.
    pub confidentiality_level: Option<String>,
    /// Path to the operator's organisation/scope/methodology TOML.
    /// Required by daemon startup when `intent = "official"`.
    pub org_config_path: Option<String>,
    /// Path where `perf-sentinel disclose` writes the produced JSON.
    /// Hint only, the CLI accepts an explicit `--output`.
    pub disclose_output_path: Option<String>,
    /// Period selector hint: `"calendar-quarter"`, `"calendar-month"`,
    /// `"calendar-year"`, or `"custom"`. Pure hint for scheduled runs.
    pub disclose_period: Option<String>,
    /// Sigstore signing target. Empty defaults to the public Sigstore
    /// instance. perf-sentinel does not sign itself; this value lives
    /// in the report so `verify-hash` knows which Rekor to query.
    pub sigstore: SigstoreConfig,
}

/// Sigstore Rekor + Fulcio endpoints used by `verify-hash` and reported
/// in `integrity.signature.rekor_url`. Maps to `[reporting.sigstore]`.
#[derive(Debug, Clone)]
pub struct SigstoreConfig {
    pub rekor_url: String,
    pub fulcio_url: String,
}

impl Default for SigstoreConfig {
    fn default() -> Self {
        Self {
            rekor_url: DEFAULT_REKOR_URL.to_string(),
            fulcio_url: DEFAULT_FULCIO_URL.to_string(),
        }
    }
}

/// Public Sigstore Rekor transparency log.
pub const DEFAULT_REKOR_URL: &str = "https://rekor.sigstore.dev";
/// Public Sigstore Fulcio certificate authority.
pub const DEFAULT_FULCIO_URL: &str = "https://fulcio.sigstore.dev";

/// Workspace version that turns `[reporting] disclose_output_path`
/// into a functional field (daemon-triggered periodic disclosures).
/// Bump here when the timeline slips. The same value appears as a
/// TOML comment in `docs/REPORTING.md` and `docs/FR/REPORTING-FR.md`,
/// kept in sync by grep at release time.
const RESERVED_DISCLOSE_OUTPUT_PATH_VERSION: &str = "0.8.0";

/// Maps to `[daemon.archive]` in TOML. When `Some`, the daemon writes
/// each per-window `Report` as one NDJSON line to `path`, with
/// size-triggered rotation and `max_files` count-based pruning.
#[derive(Debug, Clone)]
pub struct DaemonArchiveConfig {
    pub path: String,
    pub max_size_mb: u64,
    pub max_files: u32,
}

impl Default for DaemonArchiveConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            max_size_mb: 100,
            max_files: 12,
        }
    }
}

/// Quality-gate thresholds. Maps 1:1 to `[thresholds]` in TOML.
/// `#[non_exhaustive]` so a future field stays a minor bump rather than
/// a breaking change: external crates cannot construct it with a
/// struct literal, only read it or deserialize into it.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ThresholdsConfig {
    /// Maximum allowed critical N+1 SQL findings before quality gate fails.
    pub n_plus_one_sql_critical_max: u32,
    /// Maximum allowed warning+ N+1 HTTP findings before quality gate fails.
    pub n_plus_one_http_warning_max: u32,
    /// Maximum allowed warning+ N+1 messaging findings before the gate fails.
    /// Warning+ rather than critical-only, like HTTP: a Kafka client may
    /// already batch the publishes it buffers, so the occurrence count is an
    /// upper bound there, see `docs/LIMITATIONS.md`.
    pub n_plus_one_messaging_warning_max: u32,
    /// Maximum allowed I/O waste ratio before quality gate fails.
    pub io_waste_ratio_max: f64,
    /// Minimum share of I/O-shaped spans that must be analyzable before
    /// the gate fails, guarding against a false green from unusable
    /// instrumentation (SQL spans without `db.statement`, HTTP spans
    /// without `http.url`). `None` (the default) disables the rule; it
    /// also stays silent when the input carries no OTLP filter tally.
    pub min_usable_span_ratio: Option<f64>,
}

/// Per-detector knobs. Maps 1:1 to `[detection]` in TOML.
/// `#[non_exhaustive]` so a future field stays a minor bump rather than
/// a breaking change: external crates cannot construct it with a
/// struct literal, only read it or deserialize into it.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DetectionConfig {
    /// N+1 detection threshold: minimum repeated similar queries to flag.
    pub n_plus_one_threshold: u32,
    /// Sliding window duration in milliseconds for N+1 detection.
    pub window_duration_ms: u64,
    /// Threshold in milliseconds above which an operation is considered slow.
    pub slow_query_threshold_ms: u64,
    /// Minimum occurrences of a slow template to flag as a finding.
    pub slow_query_min_occurrences: u32,
    /// Maximum child spans per parent before flagging excessive fanout.
    pub max_fanout: u32,
    /// Minimum HTTP outbound calls per trace to flag as chatty service.
    pub chatty_service_min_calls: u32,
    /// Peak concurrent SQL spans per service to flag pool saturation.
    pub pool_saturation_concurrent_threshold: u32,
    /// Minimum sequential independent sibling calls to flag as serialized.
    pub serialized_min_sequential: u32,
    /// Sanitizer-aware classification mode for SQL N+1 vs redundant.
    /// See [`crate::detect::sanitizer_aware::SanitizerAwareMode`].
    pub sanitizer_aware_classification: crate::detect::sanitizer_aware::SanitizerAwareMode,
    /// Coefficient of variation of per-span durations above which a sanitized
    /// group reads as N+1 rather than a cached repeat. Raise it on runtimes
    /// whose scheduling jitter spreads identical queries past the default.
    pub sanitizer_aware_min_cv: f64,
    /// Resource or span attributes captured to separate deployments, most
    /// specific first. The first one present on a span decides its identity,
    /// the others are still captured and shown. Capped at
    /// [`MAX_GROUPING_ATTRIBUTES`] so a config cannot grow every span.
    pub grouping_attributes: Vec<String>,
}

/// Default for [`DetectionConfig::grouping_attributes`]. Kubernetes first,
/// since `service.namespace` often carries a constant such as a product name.
pub const DEFAULT_GROUPING_ATTRIBUTES: [&str; 2] = ["k8s.namespace.name", "service.namespace"];

/// Upper bound on configured grouping attributes. Each one is captured per
/// span, so an unbounded list is a memory multiplier on the hot path.
pub const MAX_GROUPING_ATTRIBUTES: usize = 8;

/// `GreenOps` / carbon scoring config. Maps to `[green]` in TOML.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Config aggregates the [green] toggles from .perf-sentinel.toml
pub struct GreenConfig {
    pub enabled: bool,
    /// Fallback region for CO₂ scoring (e.g. `"eu-west-3"`).
    pub default_region: Option<String>,
    /// Per-service region overrides. Keys lowercased at load time.
    pub service_regions: HashMap<String, String>,
    /// SCI `M` term: embodied carbon per request (gCO₂eq).
    pub embodied_carbon_per_request_gco2: f64,
    /// Use 24-hour carbon intensity profiles when available.
    pub use_hourly_profiles: bool,
    /// Scaphandre RAPL scraper config (daemon only).
    pub scaphandre: Option<ScaphandreConfig>,
    /// Kepler eBPF energy scraper config (daemon only).
    pub kepler: Option<KeplerConfig>,
    /// Alumet energy scraper config (daemon only). Highest
    /// measured-energy precedence, overrides Scaphandre.
    pub alumet: Option<AlumetConfig>,
    /// Redfish BMC wall-plug-power scraper config (daemon only).
    pub redfish: Option<RedfishConfig>,
    /// Cloud CPU% + `SPECpower` config (daemon only).
    pub cloud_energy: Option<CloudEnergyConfig>,
    /// Declared broker cluster (`[green.broker_static]`, daemon only).
    /// Needs no agent, so it covers managed brokers.
    pub broker_static: Option<crate::score::broker_static::StaticBrokerConfig>,
    /// Whether to use per-operation energy coefficients (SQL verb weighting,
    /// HTTP payload size tiers) in the proxy model. Default: `true`.
    pub per_operation_coefficients: bool,
    /// Deprecated since 0.9.25, retained for API compatibility. The
    /// transport term is always computed, displayed and disclosed, so
    /// this always reads `true` whatever the TOML said.
    #[deprecated(
        since = "0.9.25",
        note = "the transport term is always shown; this value has no effect"
    )]
    pub include_network_transport: bool,
    /// Deprecated since 0.9.25, retained for API compatibility. The
    /// coefficient is fixed, so this always reads
    /// [`DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH`](crate::score::carbon::DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH),
    /// the value scoring actually applies, whatever the TOML said.
    #[deprecated(
        since = "0.9.25",
        note = "the transport coefficient is fixed; this value has no effect"
    )]
    pub network_energy_per_byte_kwh: f64,
    /// Path to user-supplied hourly profiles JSON file. `None` when not
    /// configured (uses only embedded profiles).
    pub hourly_profiles_file: Option<String>,
    /// Pre-parsed custom hourly profiles, loaded at config parse time.
    /// `None` when `hourly_profiles_file` is not set or failed to load.
    pub custom_hourly_profiles:
        Option<std::sync::Arc<HashMap<String, crate::score::carbon::HourlyProfile>>>,
    /// Path to a calibration TOML file generated by `perf-sentinel calibrate`.
    pub calibration_file: Option<String>,
    /// Pre-loaded calibration data, parsed at config load time.
    /// `None` when `calibration_file` is not set or failed to load.
    pub calibration: Option<crate::calibrate::CalibrationData>,
    /// Electricity Maps real-time carbon intensity config (daemon only).
    pub electricity_maps: Option<crate::score::electricity_maps::ElectricityMapsConfig>,
}

/// Daemon runtime config. Maps to `[daemon]` plus its `[daemon.tls]`,
/// `[daemon.ack]`, `[daemon.cors]` and `[daemon.correlation]` sub-tables.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub listen_addr: String,
    /// Port for OTLP HTTP receiver.
    pub listen_port: u16,
    /// Port for OTLP gRPC receiver.
    pub listen_port_grpc: u16,
    pub json_socket: String,
    /// Maximum number of active traces in streaming mode.
    pub max_active_traces: usize,
    /// Trace TTL in milliseconds for streaming mode eviction.
    pub trace_ttl_ms: u64,
    /// Sampling rate for incoming traces (0.0 - 1.0).
    pub sampling_rate: f64,
    /// Per-trace cap applied independently to retained events, inbound endpoint
    /// contexts (endpoint plus optional parent link), and span-ancestry entries
    /// (intermediate parent link plus optional resolved endpoint). Endpoint
    /// ambiguity state is additionally bounded by the number of services with a
    /// retained endpoint context.
    pub max_events_per_trace: usize,
    /// Maximum payload size in bytes for JSON deserialization.
    pub max_payload_size: usize,
    /// Deployment environment label used to stamp findings with a
    /// [`Confidence`] value, so a downstream consumer can boost
    /// severity on production traffic. Ignored in `analyze` batch mode,
    /// which always emits [`Confidence::CiBatch`].
    pub environment: DaemonEnvironment,
    /// Maximum number of findings retained by the daemon query API.
    pub max_retained_findings: usize,
    /// Maximum number of findings carried by one `/api/export/report`
    /// snapshot. Separate from the `/api/findings` cap, which paginates a
    /// browsing API, where this one sizes a deliberate export: a store
    /// holding tens of thousands of findings ships a slice of its most
    /// recent, and the default keeps the historical size. Raising it
    /// grows the response body and the HTML rendered from it by a few KB
    /// per finding, so it trades report weight for coverage. The cap also
    /// bounds what the exported `quality_gate` counts, so zero exports the
    /// envelope alone and the gate's finding-count rules pass whatever the
    /// daemon detected. `io_waste_ratio_max` reads `green_summary`, which
    /// no cap empties, so the verdict still moves, it just stops reflecting
    /// the findings: a probe polling that shape is reading half a verdict.
    pub max_export_findings: usize,
    /// Maximum number of traces whose masked spans are retained for
    /// `/api/export/report`, so an exported report still draws a span
    /// tree. Zero disables retention. Costs memory in proportion to
    /// `max_events_per_trace`, which is why the default is small next to
    /// `max_retained_findings`.
    pub max_retained_traces: usize,
    /// Capacity of the ingestion channel: span-event batches buffered
    /// between the listeners and the event loop. Provides ingestion
    /// backpressure once full.
    pub ingest_queue_capacity: usize,
    /// Capacity of the analysis worker queue: evicted/expired batches
    /// awaiting detect+score. When full, whole batches are shed (counted
    /// on `perf_sentinel_analysis_shed_*`).
    pub analysis_queue_capacity: usize,
    /// Whether `/metrics` breaks findings and slow-span durations down
    /// by service, under the daemon's cardinality caps (overflow folds
    /// into `service="_other"`). `false` leaves the label empty,
    /// restoring the pre-0.18 shape. The per-service I/O counters are
    /// unaffected: per-service is their only shape.
    pub per_service_labels: bool,
    /// Whether the same series, and the three per-service I/O counters,
    /// carry a `grouping` label next to `service`: the span's first
    /// `[detection] grouping_attributes` value present, under per-run
    /// caps on admitted (service, grouping) pairs (a pair past the cap
    /// folds its grouping into `grouping="_other"`).
    /// `false` leaves the label empty on every series, restoring the
    /// 0.18 shape. Since 0.19.0.
    pub per_grouping_labels: bool,
    /// Memory-pressure admission control, as a percentage of the cgroup v2
    /// memory limit (1-100). When the pod's `memory.current / memory.max`
    /// crosses this high-water mark, OTLP ingest is rejected with a
    /// retryable status (counted on `perf_sentinel_otlp_rejected_total`
    /// `{reason="memory_pressure"}`) until usage falls back below the mark,
    /// so RSS is bounded independently of queue depth. `0` disables the
    /// guard (default). Linux/cgroup-v2 only, inert elsewhere.
    pub memory_high_water_pct: u8,
    pub api_enabled: bool,
    /// TLS material for the OTLP listeners. When `cert_path` and
    /// `key_path` are both `Some`, both gRPC and HTTP listen TLS; when
    /// both are `None`, plain TCP (default).
    pub tls: DaemonTlsConfig,
    /// Daemon-side ack store (JSONL persistence + HTTP API).
    pub ack: DaemonAckConfig,
    /// CORS layer for the daemon HTTP API.
    pub cors: DaemonCorsConfig,
    /// Cross-trace correlation. `enabled = false` by default; the
    /// daemon never wires the correlator when off, so the other fields
    /// only apply when `enabled = true`.
    pub correlation: crate::detect::correlate_cross::CorrelationConfig,
    /// Optional per-window `Report` archive writer. `None` (default)
    /// means no archive is written. Consumed by `perf-sentinel disclose`.
    pub archive: Option<DaemonArchiveConfig>,
    /// Optional batched exporter to `PerfSentinelHub`.
    pub hub_export: DaemonHubExportConfig,
}

/// Bounded, opt-in export of live daemon findings to `PerfSentinelHub`.
#[derive(Debug, Clone)]
pub struct DaemonHubExportConfig {
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub source_id: Option<String>,
    pub api_key_file: Option<String>,
    pub batch_size: usize,
    pub flush_interval_secs: u64,
    pub max_pending: usize,
}

/// TLS material. Both fields must be set together (or both `None`).
#[derive(Debug, Clone, Default)]
pub struct DaemonTlsConfig {
    /// Path to PEM-encoded TLS certificate chain for the OTLP receivers.
    pub cert_path: Option<String>,
    /// Path to PEM-encoded TLS private key for the OTLP receivers.
    pub key_path: Option<String>,
}

/// Daemon-side ack store config.
#[derive(Debug, Clone)]
pub struct DaemonAckConfig {
    /// Whether the daemon-side ack store (JSONL persistence + HTTP API)
    /// is enabled. Default `true`. Disabling skips both the TOML acks
    /// load and the JSONL store init at startup, and the three ack
    /// routes return 503 Service Unavailable.
    pub enabled: bool,
    /// Optional override for the JSONL storage path. Default resolves
    /// at runtime via `dirs::data_local_dir()` to
    /// `<data_local>/perf-sentinel/acks.jsonl`.
    pub storage_path: Option<String>,
    /// Optional opt-in API key. When set, `POST` and `DELETE` on
    /// `/api/findings/<sig>/ack` require an `X-API-Key` header
    /// matching this value (constant-time compared). Default `None`
    /// means no auth, suitable for the loopback-only deployment.
    pub api_key: Option<String>,
    /// Optional override for the CI ack TOML file path read at daemon
    /// startup. Default `.perf-sentinel-acknowledgments.toml` in CWD.
    pub toml_path: Option<String>,
}

/// Daemon HTTP API CORS layer config.
#[derive(Debug, Clone, Default)]
pub struct DaemonCorsConfig {
    /// Allowed origins for the daemon HTTP API CORS layer. Empty (default)
    /// means no CORS headers are emitted, which preserves the pre-CORS
    /// behavior. `["*"]` is wildcard mode, intended for development. A
    /// non-wildcard list is the production posture: each entry must be a
    /// full origin (scheme + host + optional port), e.g.
    /// `"https://reports.example.com"`. Configured via
    /// `[daemon.cors] allowed_origins` in TOML.
    pub allowed_origins: Vec<String>,
}

/// Deployment environment for the daemon's `watch` mode.
///
/// Maps 1:1 to [`Confidence`] via [`Config::confidence`]:
/// - [`Self::Staging`] → [`Confidence::DaemonStaging`]
/// - [`Self::Production`] → [`Confidence::DaemonProduction`]
///
/// Parsed from the `[daemon] environment` TOML field as case-insensitive
/// `"staging"` or `"production"`. Any other value is rejected at load time
/// with a clear validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DaemonEnvironment {
    /// Staging traffic, medium confidence. Default.
    #[default]
    Staging,
    /// Production traffic, high confidence.
    Production,
}

impl DaemonEnvironment {
    /// Returns the lowercase string label used in the TOML config.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }
}

impl Default for ThresholdsConfig {
    fn default() -> Self {
        Self {
            n_plus_one_sql_critical_max: 0,
            n_plus_one_http_warning_max: 3,
            n_plus_one_messaging_warning_max: 3,
            io_waste_ratio_max: 0.30,
            min_usable_span_ratio: None,
        }
    }
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            n_plus_one_threshold: 5,
            window_duration_ms: 500,
            slow_query_threshold_ms: 500,
            slow_query_min_occurrences: 3,
            max_fanout: 20,
            chatty_service_min_calls: 15,
            pool_saturation_concurrent_threshold: 10,
            serialized_min_sequential: 3,
            sanitizer_aware_classification:
                crate::detect::sanitizer_aware::SanitizerAwareMode::default(),
            sanitizer_aware_min_cv: crate::detect::sanitizer_aware::DEFAULT_MIN_CV,
            grouping_attributes: DEFAULT_GROUPING_ATTRIBUTES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

impl Default for GreenConfig {
    // The two deprecated transport fields are set to what scoring
    // actually applies, so a downstream reader is never misled.
    #[allow(deprecated)]
    fn default() -> Self {
        Self {
            include_network_transport: true,
            network_energy_per_byte_kwh: crate::score::carbon::DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH,
            enabled: true,
            default_region: None,
            service_regions: HashMap::new(),
            embodied_carbon_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
            use_hourly_profiles: true,
            scaphandre: None,
            kepler: None,
            alumet: None,
            redfish: None,
            cloud_energy: None,
            broker_static: None,
            per_operation_coefficients: true,
            hourly_profiles_file: None,
            custom_hourly_profiles: None,
            calibration_file: None,
            calibration: None,
            electricity_maps: None,
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1".to_string(),
            listen_port: 4318,
            listen_port_grpc: 4317,
            json_socket: "/tmp/perf-sentinel.sock".to_string(),
            max_active_traces: 10_000,
            trace_ttl_ms: 30_000,
            sampling_rate: 1.0,
            max_events_per_trace: 1_000,
            // 16 MiB, comfort-zone ceiling (warn_unusual_daemon_limits)
            max_payload_size: 16 * 1024 * 1024,
            environment: DaemonEnvironment::Staging,
            max_retained_findings: 10_000,
            // Matches the historical hardcoded export cap, so an operator
            // who sets nothing sees the snapshot they saw before.
            max_export_findings: 1_000,
            max_retained_traces: 50,
            ingest_queue_capacity: 1024,
            analysis_queue_capacity: 1024,
            per_service_labels: true,
            per_grouping_labels: true,
            memory_high_water_pct: 0,
            api_enabled: true,
            tls: DaemonTlsConfig::default(),
            ack: DaemonAckConfig::default(),
            cors: DaemonCorsConfig::default(),
            correlation: crate::detect::correlate_cross::CorrelationConfig::default(),
            archive: None,
            hub_export: DaemonHubExportConfig::default(),
        }
    }
}

impl Default for DaemonHubExportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: None,
            source_id: None,
            api_key_file: None,
            batch_size: 100,
            flush_interval_secs: 5,
            max_pending: 10_000,
        }
    }
}

impl Default for DaemonAckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            storage_path: None,
            api_key: None,
            toml_path: None,
        }
    }
}

impl Config {
    /// Map the daemon environment to a [`Confidence`] value.
    ///
    /// Used by `daemon::run` to stamp findings after detection. `analyze`
    /// batch mode does not call this; it picks `CiBatch` or `LocalBatch`
    /// from the host CI environment in `pipeline::analyze_with_traces`
    /// instead (see `pipeline::ci_environment_detected`).
    #[must_use]
    pub const fn confidence(&self) -> Confidence {
        match self.daemon.environment {
            DaemonEnvironment::Staging => Confidence::DaemonStaging,
            DaemonEnvironment::Production => Confidence::DaemonProduction,
        }
    }

    /// Build a [`CarbonContext`] from the green config fields.
    ///
    /// Returns a context with `energy_snapshot: None`. The daemon clones
    /// this and patches in the measured energy snapshot per tick; the
    /// batch pipeline uses it as-is (no scrapers in batch mode).
    /// The embodied coefficient scoring actually applies. Zero is
    /// deprecated (no hardware has zero embodied carbon) and clamped
    /// here rather than in the TOML layer alone, so a `Config` built by
    /// hand cannot erase the SCI `M` term either.
    #[must_use]
    pub fn effective_embodied_per_request_gco2(&self) -> f64 {
        let declared = self.green.embodied_carbon_per_request_gco2;
        if declared > 0.0 {
            declared
        } else {
            DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2
        }
    }

    #[must_use]
    #[allow(deprecated)] // the transport toggle is retained for API compatibility only
    pub fn carbon_context(&self) -> crate::score::carbon::CarbonContext {
        let scoring_config = self.green.enabled.then(|| self.scoring_config());
        crate::score::carbon::CarbonContext {
            include_network_transport: true,
            default_region: self.green.default_region.clone(),
            service_regions: self.green.service_regions.clone(),
            embodied_per_request_gco2: self.effective_embodied_per_request_gco2(),
            use_hourly_profiles: self.green.use_hourly_profiles,
            energy_snapshot: None,
            per_operation_coefficients: self.green.per_operation_coefficients,
            network_energy_per_byte_kwh: crate::score::carbon::DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH,
            custom_hourly_profiles: self.green.custom_hourly_profiles.clone(),
            calibration: self.green.calibration.clone(),
            real_time_intensity: None, // set per-tick in daemon via build_tick_ctx
            scoring_config,
            // None here so batch runs fall back to the estimated figure.
            // The daemon injects the declaration (see `daemon::run`), it
            // is the only mode that can deliver measured window energy.
            db_energy: None,
            broker_energy: None,
        }
    }

    /// Build the [`ScoringConfig`](crate::score::carbon::ScoringConfig)
    /// regardless of `green.enabled`. Always built, not only under
    /// Electricity Maps: it carries the applied coefficients into every
    /// archived window and the transport display setting the dashboards
    /// honour. `carbon_context` gates it on `green.enabled`, the query
    /// API reports it ungated so a configured backend stays visible.
    #[must_use]
    pub fn scoring_config(&self) -> crate::score::carbon::ScoringConfig {
        let mut scoring_config = self.green.electricity_maps.as_ref().map_or_else(
            || crate::score::carbon::ScoringConfig {
                electricity_maps: Some(false),
                ..Default::default()
            },
            crate::score::carbon::ScoringConfig::from_electricity_maps,
        );
        scoring_config.embodied_per_request_gco2 = Some(self.effective_embodied_per_request_gco2());
        scoring_config.network_energy_per_byte_kwh =
            Some(crate::score::carbon::DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH);
        scoring_config.per_operation_coefficients = Some(self.green.per_operation_coefficients);
        scoring_config.use_hourly_profiles = Some(self.green.use_hourly_profiles);
        scoring_config
    }
}

mod raw;
mod toml_paths;
mod validate;

use raw::{
    RawConfig, parse_daemon_environment, parse_kepler_metric_kind, validate_alumet_raw,
    validate_broker_static_raw,
};
use toml_paths::normalize_toml_path_strings;
pub(crate) use validate::has_control_char;

// Re-imports so `use super::*;` in the tests module keeps resolving the
// names that moved into submodules.
#[cfg(test)]
use raw::{
    AlumetDatabaseSection, AlumetSection, CloudSection, ElectricityMapsSection, KeplerSection,
    RedfishSection, ScaphandreSection, convert_alumet_section_with_env,
    convert_cloud_section_with_env, convert_electricity_maps_section_with_env,
    convert_kepler_section_with_env, convert_redfish_section_with_env,
    convert_scaphandre_section_with_env,
};
#[cfg(test)]
use toml_paths::{TOML_PATH_STRING_KEYS, find_basic_string_end};
#[cfg(test)]
use validate::validate_http_authority;

/// Top-level TOML keys that perf-sentinel accepted in 0.5.x as legacy
/// flat aliases for sectioned fields. Removed in 0.6.0; loading a config
/// that still uses any of them returns
/// [`ConfigError::Validation`] with the new section path so the operator
/// can migrate without grep-around. Tuple is `(legacy_top_level_key,
/// new_section_path)`. The list is intentionally exhaustive: a 0.5.x
/// config that loads on 0.6.x without a clear error is the worst-case
/// outcome we want to avoid.
const REMOVED_LEGACY_TOP_LEVEL_KEYS: &[(&str, &str)] = &[
    (
        "n_plus_one_threshold",
        "[detection] n_plus_one_min_occurrences",
    ),
    ("window_duration_ms", "[detection] window_duration_ms"),
    ("listen_addr", "[daemon] listen_address"),
    ("listen_port", "[daemon] listen_port_http"),
    ("max_active_traces", "[daemon] max_active_traces"),
    ("trace_ttl_ms", "[daemon] trace_ttl_ms"),
    ("max_events_per_trace", "[daemon] max_events_per_trace"),
    ("max_payload_size", "[daemon] max_payload_size"),
];

/// Reject 0.5.x legacy top-level keys with a migration hint.
///
/// Runs before the typed `RawConfig` parse: a typed parse with no
/// `deny_unknown_fields` would silently drop these keys (operator never
/// sees a warning, defaults silently apply). A typed parse WITH
/// `deny_unknown_fields` would surface a serde error like "unknown field
/// `listen_port`" without the migration path. The bespoke check below
/// prints both pieces of information in one error.
fn reject_legacy_top_level_keys(content: &str) -> Result<(), ConfigError> {
    let value: toml::Value = toml::from_str(content).map_err(ConfigError::Parse)?;
    reject_legacy_top_level_value(&value).map_err(ConfigError::Validation)
}

fn reject_legacy_top_level_value(value: &toml::Value) -> Result<(), String> {
    let toml::Value::Table(table) = value else {
        return Ok(());
    };
    for (legacy, replacement) in REMOVED_LEGACY_TOP_LEVEL_KEYS {
        if table.contains_key(*legacy) {
            return Err(format!(
                "config: top-level '{legacy}' was removed in 0.6.0; \
                 use '{replacement}' instead. \
                 See the 0.6.0 migration notes for the full list of renamed keys."
            ));
        }
    }
    Ok(())
}

/// Load configuration from a TOML string.
///
/// Validates that all values are within acceptable bounds after parsing.
///
/// # Errors
///
/// Returns `ConfigError::Parse` if the TOML is malformed, or
/// `ConfigError::Validation` if a field value is out of bounds, or if a
/// 0.5.x legacy top-level key is present (see
/// [`REMOVED_LEGACY_TOP_LEVEL_KEYS`]).
pub fn load_from_str(content: &str) -> Result<Config, ConfigError> {
    let normalized = normalize_toml_path_strings(content);
    reject_legacy_top_level_keys(normalized.as_ref())?;
    let raw: RawConfig = match toml::from_str(normalized.as_ref()) {
        Ok(raw) => raw,
        Err(norm_err) => {
            if matches!(normalized, Cow::Owned(_)) {
                // Path normalization fallback. See design doc 07 >
                // "Windows path normalization" for the rationale.
                tracing::debug!(
                    normalized_error = %norm_err,
                    "path normalization produced invalid TOML; retrying with original input"
                );
                toml::from_str(content).map_err(ConfigError::Parse)?
            } else {
                return Err(ConfigError::Parse(norm_err));
            }
        }
    };
    validate_raw_config(raw)
}

/// Load and deeply merge ordered TOML documents. Two tables merge recursively.
/// Any other later value replaces the earlier value at the same key.
///
/// # Errors
///
/// Returns parse and validation errors naming the responsible fragment when
/// it can be identified.
pub fn load_from_fragments(fragments: &[(&str, &str)]) -> Result<Config, ConfigError> {
    let mut merged = toml::Value::Table(toml::Table::new());
    let mut origins = HashMap::new();
    for (name, content) in fragments {
        let normalized = normalize_toml_path_strings(content);
        let value = match toml::from_str(normalized.as_ref()) {
            Ok(value) => value,
            Err(normalized_error) if matches!(normalized, Cow::Owned(_)) => {
                tracing::debug!(
                    %normalized_error,
                    fragment = *name,
                    "path normalization produced invalid TOML; retrying with original input"
                );
                toml::from_str(content).map_err(|source| ConfigError::FragmentParse {
                    name: (*name).to_string(),
                    source,
                })?
            }
            Err(source) => {
                return Err(ConfigError::FragmentParse {
                    name: (*name).to_string(),
                    source,
                });
            }
        };
        reject_legacy_top_level_value(&value).map_err(|message| {
            ConfigError::FragmentValidation {
                name: (*name).to_string(),
                message,
            }
        })?;
        merge_toml_value(&mut merged, value, "", name, &mut origins);
    }
    let raw: RawConfig = serde_path_to_error::deserialize(merged).map_err(|error| {
        let path = error.path().to_string();
        let name = origin_for_path(&origins, &path)
            .or_else(|| unique_origin_for_subtree(&origins, &path))
            .unwrap_or("merged configuration")
            .to_string();
        ConfigError::FragmentParse {
            name,
            source: error.into_inner(),
        }
    })?;
    validate_raw_config(raw).map_err(|error| match error {
        ConfigError::Validation(message) => ConfigError::FragmentValidation {
            name: origin_for_validation(&origins, &message)
                .unwrap_or("merged configuration")
                .to_string(),
            message,
        },
        other => other,
    })
}

fn merge_toml_value(
    base: &mut toml::Value,
    overlay: toml::Value,
    path: &str,
    fragment: &str,
    origins: &mut HashMap<String, String>,
) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if let Some(existing) = base.get_mut(&key) {
                    merge_toml_value(existing, value, &child, fragment, origins);
                } else {
                    record_toml_origins(&value, &child, fragment, origins);
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => {
            if matches!(base, toml::Value::Table(_)) {
                let prefix = format!("{path}.");
                origins.retain(|key, _| key != path && !key.starts_with(&prefix));
            } else {
                origins.remove(path);
            }
            record_toml_origins(&overlay, path, fragment, origins);
            *base = overlay;
        }
    }
}

fn record_toml_origins(
    value: &toml::Value,
    path: &str,
    fragment: &str,
    origins: &mut HashMap<String, String>,
) {
    if let toml::Value::Table(table) = value {
        for (key, value) in table {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            record_toml_origins(value, &child, fragment, origins);
        }
    } else if !path.is_empty() {
        origins.insert(path.to_string(), fragment.to_string());
    }
}

fn origin_for_path<'a>(origins: &'a HashMap<String, String>, path: &str) -> Option<&'a str> {
    origins
        .iter()
        .filter(|(candidate, _)| {
            path == candidate.as_str()
                || path
                    .strip_prefix(candidate.as_str())
                    .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('['))
        })
        .max_by_key(|(candidate, _)| candidate.len())
        .map(|(_, fragment)| fragment.as_str())
}

/// Attribute a `[section] field ...` validation message to the fragment
/// that set it. `None` when two fragments claim different fields of the
/// message, which would make the attribution a guess.
fn origin_for_sectioned_message<'a>(
    origins: &'a HashMap<String, String>,
    section: &str,
    detail: &str,
) -> Option<&'a str> {
    let mut matched = None;
    for field in detail.split(|character: char| {
        !(character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_')
    }) {
        if field.is_empty() {
            continue;
        }
        if let Some(fragment) = unique_origin_for_subtree(origins, &format!("{section}.{field}")) {
            if matched.is_some_and(|existing| existing != fragment) {
                return None;
            }
            matched = Some(fragment);
        }
    }
    matched.or_else(|| unique_origin_for_subtree(origins, section))
}

fn origin_for_validation<'a>(
    origins: &'a HashMap<String, String>,
    message: &str,
) -> Option<&'a str> {
    if let Some(rest) = message.strip_prefix('[')
        && let Some((section, detail)) = rest.split_once(']')
    {
        return origin_for_sectioned_message(origins, section, detail);
    }
    let field = first_config_identifier(message)?;
    let field = match field {
        "n_plus_one_threshold" => "n_plus_one_min_occurrences",
        other => other,
    };
    let suffix = format!(".{field}");
    let mut matching = origins
        .iter()
        .filter(|(path, _)| path == &field || path.ends_with(&suffix))
        .map(|(_, fragment)| fragment.as_str());
    let first = matching.next()?;
    matching.all(|fragment| fragment == first).then_some(first)
}

fn unique_origin_for_subtree<'a>(
    origins: &'a HashMap<String, String>,
    path: &str,
) -> Option<&'a str> {
    let prefix = format!("{path}.");
    let mut leaves = origins
        .iter()
        .filter(|(candidate, _)| candidate.as_str() == path || candidate.starts_with(&prefix))
        .map(|(_, fragment)| fragment.as_str());
    let first = leaves.next()?;
    leaves.all(|fragment| fragment == first).then_some(first)
}

fn first_config_identifier(input: &str) -> Option<&str> {
    let identifier = input
        .trim_start()
        .split(|character: char| {
            !(character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_')
        })
        .next()?;
    (!identifier.is_empty()).then_some(identifier)
}

fn validate_raw_config(raw: RawConfig) -> Result<Config, ConfigError> {
    // Validate before the lossy `Config::from` conversion: a typo like
    // `envrionment = "prod"` would otherwise silently downgrade to
    // Staging instead of erroring.
    if let Some(env_str) = raw.daemon.environment.as_deref()
        && parse_daemon_environment(env_str).is_none()
    {
        return Err(ConfigError::Validation(format!(
            "[daemon] environment '{env_str}' is invalid; \
             expected 'staging' or 'production' (case-insensitive)"
        )));
    }
    // Same pattern for `[green.kepler] metric_kind`: the From conversion
    // would otherwise downgrade an invalid value to a tracing::error log
    // and silently drop the whole section, which on a v0.7.4 → v0.7.5
    // upgrade would translate an operator's `metric_kind = "process_package"`
    // into a silent Kepler disable instead of the documented loud error.
    parse_kepler_metric_kind(raw.green.kepler.metric_kind.as_deref())
        .map_err(ConfigError::Validation)?;
    // Same pattern for `[green.alumet]`: `metric_name` and `label_key`
    // are mandatory once an endpoint is set and have no safe default,
    // so a missing one must be a loud error rather than a silently
    // dropped section.
    validate_alumet_raw(&raw.green.alumet).map_err(ConfigError::Validation)?;
    validate_broker_static_raw(&raw.green.broker_static).map_err(ConfigError::Validation)?;
    let config = Config::from(raw);
    config.validate().map_err(ConfigError::Validation)?;
    config.warn_listen_addr_if_non_loopback();
    config.warn_reporting_advisory();
    Ok(config)
}

/// Errors that can occur during configuration loading.
///
/// `#[non_exhaustive]` so that adding future variants (e.g. a new
/// validation failure when a new config section lands) stays a
/// SemVer-minor change.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// TOML parsing error.
    #[error("config parse error: {0}")]
    Parse(#[from] toml::de::Error),
    /// TOML parsing error attributed to one named file. The name is the
    /// file itself, fragment or main config: the main `.perf-sentinel.toml`
    /// travels the same loader and must not be called a fragment.
    #[error("in {name}: {source}")]
    FragmentParse {
        name: String,
        #[source]
        source: toml::de::Error,
    },
    /// Validation error attributed to one named file, same naming rule as
    /// [`ConfigError::FragmentParse`].
    #[error("in {name}: {message}")]
    FragmentValidation { name: String, message: String },
    /// Validation error (out-of-range values).
    #[error("config validation error: {0}")]
    Validation(String),
}

#[cfg(test)]
mod tests;
