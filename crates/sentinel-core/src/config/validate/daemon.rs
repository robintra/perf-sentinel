//! Validation of the `[daemon]` section: limits, TLS, CORS, API keys and its subsections.

use crate::config::K8S_NAMESPACE_ATTRIBUTE;

use super::{
    Config, check_range, has_control_char, snapshot_budget_warning, validate_http_authority,
    warn_outside_comfort_zone,
};

/// The shared-secret floor for every write route on the daemon API.
///
/// Hard reject below 12: the threat model is a co-resident local attacker
/// hitting the loopback API at line rate with no rate limiting, and
/// 36^12 is past the brute-force horizon for any realistic deployment.
/// 16 stays the recommended production floor, warned rather than refused.
fn check_api_key(field: &str, key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if has_control_char(key) {
        return Err(format!("{field} contains control characters"));
    }
    if key.len() < 12 {
        return Err(format!(
            "{field} is too short ({} chars), \
             use at least 12 characters (16 recommended)",
            key.len()
        ));
    }
    if key.len() < 16 {
        tracing::warn!(
            len = key.len(),
            "{field} is shorter than 16 characters, \
             consider a longer secret to resist brute-force attempts"
        );
    }
    Ok(())
}

/// Validate the wildcard-mode interactions of `[daemon.cors] allowed_origins`.
///
/// - `["*"]` mixed with explicit origins is ambiguous and silently degrades to
///   wildcard mode in `build_cors_layer`. Reject the mix at config load.
/// - `["*"]` combined with a write key (`[daemon.ack] api_key` or
///   `[daemon.incidents] api_key`) lets any browser origin replay a captured
///   `X-API-Key` header (header-based auth, not blocked by
///   `allow_credentials = false`). Reject the combination. A lone
///   `[daemon] read_api_key` is never compared, so it is not a replay target.
fn validate_cors_wildcard_mode(
    has_wildcard: bool,
    origin_count: usize,
    has_api_key: bool,
) -> Result<(), String> {
    if has_wildcard && origin_count > 1 {
        return Err(
            "[daemon.cors] allowed_origins cannot mix \"*\" with explicit origins, \
             either use [\"*\"] for wildcard mode or list every origin explicitly"
                .to_string(),
        );
    }
    if has_wildcard && has_api_key {
        return Err(
            "[daemon.cors] allowed_origins = [\"*\"] is incompatible with a daemon \
             write api_key ([daemon.ack] or [daemon.incidents]), since X-API-Key \
             is sent on every cross-origin request and would be replayable from \
             any browser tab. Use an explicit origin list or unset the keys for \
             development"
                .to_string(),
        );
    }
    Ok(())
}

/// Validate a single `[daemon.cors] allowed_origins` entry: rejects empty
/// strings, control characters, missing scheme and trailing slashes. The
/// literal `"*"` is accepted (wildcard-mode interactions live in
/// [`validate_cors_wildcard_mode`]).
fn validate_cors_origin(origin: &str) -> Result<(), String> {
    if origin.is_empty() {
        return Err(
            "[daemon.cors] allowed_origins entry is empty, drop it or set a value".to_string(),
        );
    }
    if has_control_char(origin) {
        return Err(format!(
            "[daemon.cors] allowed_origins entry '{origin}' contains control characters"
        ));
    }
    if origin == "*" {
        return Ok(());
    }
    if !(origin.starts_with("http://") || origin.starts_with("https://")) {
        return Err(format!(
            "[daemon.cors] allowed_origins entry '{origin}' must start with http:// or https:// (or be \"*\" for wildcard mode)"
        ));
    }
    if origin.ends_with('/') {
        return Err(format!(
            "[daemon.cors] allowed_origins entry '{origin}' must not end with a trailing slash, an origin is scheme + host + optional port"
        ));
    }
    Ok(())
}

impl Config {
    /// Emit the non-loopback security advisory if applicable.
    ///
    /// The default is `127.0.0.1` (loopback). Advanced users may override
    /// to `0.0.0.0` for container deployments behind a reverse proxy. We
    /// warn loudly rather than rejecting, because the user's intent is
    /// explicit (they changed the config) and a hard reject would force
    /// workarounds (e.g., iptables) that are harder to audit.
    ///
    /// Called once by the loader, after `validate()`, on the merged
    /// configuration. The `watch` flags reach it as a last TOML fragment, so
    /// the address it reads is the one the daemon binds.
    pub fn warn_listen_addr_if_non_loopback(&self) {
        if !self.daemon_is_loopback() {
            tracing::warn!(
                "Daemon configured to listen on non-loopback address: {}. \
                 OTLP ingest, /metrics and the read endpoints are never \
                 authenticated. Put a reverse proxy or a network policy in front.",
                self.daemon.listen_addr
            );
        }
    }

    fn daemon_is_loopback(&self) -> bool {
        // Parse the whole 127.0.0.0/8 and ::1 range, not just the two canonical
        // spellings, so the advisory does not fire on 127.0.0.2. Strip IPv6
        // brackets first ("[::1]" is the spelling the listeners bind). A non-IP
        // host (e.g. "localhost") counts as non-loopback.
        self.daemon
            .listen_addr
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    }

    /// Validate `[daemon.archive]` settings when present.
    pub(super) fn validate_daemon_archive(&self) -> Result<(), String> {
        let Some(archive) = &self.daemon.archive else {
            return Ok(());
        };
        if archive.path.trim().is_empty() {
            return Err("[daemon.archive] path must not be empty".to_string());
        }
        if has_control_char(&archive.path) {
            return Err("[daemon.archive] path contains control characters".to_string());
        }
        if archive.max_size_mb < 1 {
            return Err("[daemon.archive] max_size_mb must be >= 1".to_string());
        }
        if archive.max_files < 1 {
            return Err("[daemon.archive] max_files must be >= 1".to_string());
        }
        Ok(())
    }

    pub(in crate::config) fn validate_daemon_cors(&self) -> Result<(), String> {
        let has_wildcard = self.daemon.cors.allowed_origins.iter().any(|o| o == "*");
        validate_cors_wildcard_mode(
            has_wildcard,
            self.daemon.cors.allowed_origins.len(),
            self.daemon.ack.api_key.is_some() || self.daemon.incidents.api_key.is_some(),
        )?;
        for origin in &self.daemon.cors.allowed_origins {
            validate_cors_origin(origin)?;
        }
        Ok(())
    }

    /// Validate `[daemon.ack]` settings.
    pub(in crate::config) fn validate_daemon_ack(&self) -> Result<(), String> {
        if let Some(key) = &self.daemon.ack.api_key {
            check_api_key("[daemon.ack] api_key", key)?;
        }
        if let Some(path) = &self.daemon.ack.storage_path
            && has_control_char(path)
        {
            return Err("[daemon.ack] storage_path contains control characters".to_string());
        }
        if let Some(path) = &self.daemon.ack.toml_path
            && has_control_char(path)
        {
            return Err("[daemon.ack] toml_path contains control characters".to_string());
        }
        Ok(())
    }

    /// `[daemon] read_api_key` opens the GETs a write key gates without
    /// the power to write, which only holds while it differs from both
    /// write keys: equal to one of them, it is that key.
    pub(super) fn validate_daemon_read_key(&self) -> Result<(), String> {
        let Some(key) = &self.daemon.read_api_key else {
            return Ok(());
        };
        check_api_key("[daemon] read_api_key", key)?;
        if self.daemon.ack.api_key.as_deref() == Some(key) {
            return Err("[daemon] read_api_key must differ from [daemon.ack] api_key".to_string());
        }
        if self.daemon.incidents.api_key.as_deref() == Some(key) {
            return Err(
                "[daemon] read_api_key must differ from [daemon.incidents] api_key".to_string(),
            );
        }
        Ok(())
    }

    /// `[daemon.incidents]` is an inbound WRITE surface, so an enabled
    /// section without a resolvable key is a hard error rather than a
    /// warning: the alternative is an unauthenticated POST that anyone
    /// reaching the port can use to fabricate an incident record.
    pub(super) fn validate_daemon_incidents(&self) -> Result<(), String> {
        let incidents = &self.daemon.incidents;
        if !incidents.enabled {
            return Ok(());
        }
        let Some(key) = &incidents.api_key else {
            return Err("[daemon.incidents] enabled requires an api_key, \
                        set it or PERF_SENTINEL_INCIDENTS_API_KEY"
                .to_string());
        };
        check_api_key("[daemon.incidents] api_key", key)?;
        check_range(
            "[daemon.incidents] lookback_ms",
            &incidents.lookback_ms,
            &1_000,
            &86_400_000,
        )?;
        // Each incident carries up to `MAX_FINDINGS_LIMIT` frozen findings,
        // so the ring's memory is this times that, and 1000 already allows
        // more than a year of a daily incident.
        check_range(
            "[daemon.incidents] max_retained",
            &incidents.max_retained,
            &1,
            &1_000,
        )?;
        let required = [
            ("service_label", &incidents.service_label),
            ("kind_label", &incidents.kind_label),
            ("namespace_label", &incidents.namespace_label),
        ];
        let optional = incidents.archive_path.as_ref().map(|p| ("archive_path", p));
        for (name, value) in required.into_iter().chain(optional) {
            if value.is_empty() {
                return Err(format!("[daemon.incidents] {name} must not be empty"));
            }
            if has_control_char(value) {
                return Err(format!(
                    "[daemon.incidents] {name} contains control characters"
                ));
            }
        }
        // Advisory only: the namespace an alert carries narrows the freeze
        // through this grouping attribute alone, so without it every
        // incident freezes by service, which nothing else would say.
        if !self
            .detection
            .grouping_attributes
            .iter()
            .any(|a| a.as_str() == K8S_NAMESPACE_ATTRIBUTE)
        {
            tracing::warn!(
                "[daemon.incidents] is enabled without {K8S_NAMESPACE_ATTRIBUTE} among \
                 [detection] grouping_attributes: an incident's namespace cannot narrow \
                 its frozen findings, every incident freezes by service alone"
            );
        }
        Ok(())
    }

    pub(super) fn validate_daemon_hub_export(&self) -> Result<(), String> {
        let export = &self.daemon.hub_export;
        check_range("hub_export.batch_size", &export.batch_size, &1, &100)?;
        check_range(
            "hub_export.flush_interval_secs",
            &export.flush_interval_secs,
            &1,
            &300,
        )?;
        check_range(
            "hub_export.max_pending",
            &export.max_pending,
            &1,
            &1_000_000,
        )?;
        if !export.enabled {
            return Ok(());
        }

        let endpoint = export
            .endpoint
            .as_deref()
            .ok_or("[daemon.hub_export] endpoint is required when enabled = true")?;
        if has_control_char(endpoint)
            || (!endpoint.starts_with("http://") && !endpoint.starts_with("https://"))
            || endpoint.contains(['?', '#'])
            || !endpoint.ends_with("/api/import/findings")
        {
            return Err(
                "[daemon.hub_export] endpoint must be an HTTP(S) URL ending in /api/import/findings without query, fragment, credentials, or controls"
                    .to_string(),
            );
        }
        validate_http_authority(endpoint, "[daemon.hub_export] endpoint")?;

        let source_id = export
            .source_id
            .as_deref()
            .ok_or("[daemon.hub_export] source_id is required when enabled = true")?;
        if source_id.is_empty()
            || source_id.len() > 64
            || !source_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(
                "[daemon.hub_export] source_id must contain 1-64 ASCII letters, digits, '.', '_' or '-'"
                    .to_string(),
            );
        }

        let key_file = export
            .api_key_file
            .as_deref()
            .ok_or("[daemon.hub_export] api_key_file is required when enabled = true")?;
        if key_file.trim().is_empty() || has_control_char(key_file) {
            return Err(
                "[daemon.hub_export] api_key_file is blank or contains controls".to_string(),
            );
        }
        Ok(())
    }

    /// Validate TLS configuration: both paths must be set or both absent.
    /// When set, verify the files exist and warn if the key is
    /// world-readable on Unix.
    pub(in crate::config) fn validate_tls(&self) -> Result<(), String> {
        match (&self.daemon.tls.cert_path, &self.daemon.tls.key_path) {
            (Some(cert), Some(key)) => {
                if has_control_char(cert) {
                    return Err("[daemon] tls.cert_path contains control characters".to_string());
                }
                if has_control_char(key) {
                    return Err("[daemon] tls.key_path contains control characters".to_string());
                }
                if !std::path::Path::new(cert).exists() {
                    return Err(format!("[daemon] tls.cert_path '{cert}' does not exist"));
                }
                if !std::path::Path::new(key).exists() {
                    return Err(format!("[daemon] tls.key_path '{key}' does not exist"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(key) {
                        let mode = meta.permissions().mode();
                        if mode & 0o077 != 0 {
                            tracing::warn!(
                                "TLS key file '{key}' is readable by group/others \
                                 (mode {mode:o}). Consider restricting to owner-only \
                                 (chmod 600)."
                            );
                        }
                    }
                }
                tracing::info!("TLS enabled for daemon OTLP receivers (cert: {cert})");
                Ok(())
            }
            (None, None) => Ok(()),
            (Some(_), None) => {
                Err("[daemon] tls.cert_path is set but tls.key_path is missing".to_string())
            }
            (None, Some(_)) => {
                Err("[daemon] tls.key_path is set but tls.cert_path is missing".to_string())
            }
        }
    }

    pub(super) fn validate_daemon_limits(&self) -> Result<(), String> {
        check_range(
            "max_payload_size",
            &self.daemon.max_payload_size,
            &1024,
            &(100 * 1024 * 1024),
        )?;
        check_range(
            "max_active_traces",
            &self.daemon.max_active_traces,
            &1,
            &1_000_000,
        )?;
        check_range(
            "max_events_per_trace",
            &self.daemon.max_events_per_trace,
            &1,
            &100_000,
        )?;
        // 0 is documented as "disable the findings store entirely". Cap
        // the upper end at 10M so a typo can't OOM the daemon.
        check_range(
            "max_retained_findings",
            &self.daemon.max_retained_findings,
            &0,
            &10_000_000,
        )?;
        // Bounds one response body, not the store: a finding serializes to
        // a few KB, so 100k already means a snapshot in the hundreds of MB.
        // 0 is documented as "envelope only, no findings".
        check_range(
            "max_export_findings",
            &self.daemon.max_export_findings,
            &0,
            &100_000,
        )?;
        // Each entry is a whole span tree bounded by max_events_per_trace,
        // far heavier than one finding, hence the much lower cap.
        check_range(
            "max_retained_traces",
            &self.daemon.max_retained_traces,
            &0,
            &10_000,
        )?;
        check_range("trace_ttl_ms", &self.daemon.trace_ttl_ms, &100, &3_600_000)?;
        // 0 would make the half window 0, and 7 days is the ceiling.
        if self.daemon.correlation.enabled {
            check_range(
                "correlation.window_minutes",
                &(self.daemon.correlation.window_ms / 60_000),
                &1,
                &10_080,
            )?;
        }
        check_range(
            "ingest_queue_capacity",
            &self.daemon.ingest_queue_capacity,
            &1,
            &1_048_576,
        )?;
        check_range(
            "analysis_queue_capacity",
            &self.daemon.analysis_queue_capacity,
            &1,
            &1_048_576,
        )?;
        // 0 disables the memory-pressure admission guard. Otherwise the
        // percentage must clear the 5-point hysteresis band, else the
        // flag's low-water bound would sit at or below zero and the
        // guard could never un-reject once tripped.
        if self.daemon.memory_high_water_pct != 0 {
            check_range(
                "memory_high_water_pct",
                &self.daemon.memory_high_water_pct,
                &6,
                &100,
            )
            .map_err(|e| format!("{e} (0 disables the guard; 1..=5 would make the 5-point hysteresis low bound unreachable)"))?;
        }
        check_range("listen_port_http", &self.daemon.listen_port, &1, &65535)?;
        check_range(
            "listen_port_grpc",
            &self.daemon.listen_port_grpc,
            &1,
            &65535,
        )?;
        self.warn_unusual_daemon_limits();
        Ok(())
    }

    /// Soft startup warnings for daemon-limit values inside the hard
    /// bounds but outside their recommended comfort zone.
    ///
    /// See design doc 07 > "Comfort-zone warnings" for the band table
    /// and the rationale.
    fn warn_unusual_daemon_limits(&self) {
        // The 16 MiB ceiling matches the `max_payload_size` default value
        // (see `impl Default for DaemonConfig` in `config/mod.rs`).
        // Default-at-ceiling is inclusive (`..=`), so the canonical config
        // emits no warning. A future bump of the default must also raise
        // this ceiling, otherwise every fresh daemon would log a startup
        // warning.
        warn_outside_comfort_zone(
            "max_payload_size",
            &self.daemon.max_payload_size,
            &(256 * 1024),
            &(16 * 1024 * 1024),
            "tiny payloads may reject legitimate OTLP batches",
            "large payloads increase ingest latency and memory pressure",
        );
        warn_outside_comfort_zone(
            "max_active_traces",
            &self.daemon.max_active_traces,
            &1_000,
            &100_000,
            "aggressive LRU eviction is likely under load",
            "memory footprint grows roughly linearly with this cap",
        );
        warn_outside_comfort_zone(
            "max_events_per_trace",
            &self.daemon.max_events_per_trace,
            &100,
            &10_000,
            "complex traces will be truncated by the per-trace ring buffer",
            "very wide ring buffers rarely improve detection quality",
        );
        // Skip the comfort-zone check when the store is disabled
        // (max_retained_findings == 0), since warning on that would be
        // noise.
        if self.daemon.max_retained_findings > 0 {
            warn_outside_comfort_zone(
                "max_retained_findings",
                &self.daemon.max_retained_findings,
                &100,
                &100_000,
                "old findings will be evicted before /api/findings can serve them",
                "the findings store will hold a large in-memory backlog",
            );
        }
        // The daemon zeroes the traces store when nothing can serve it,
        // so a sized knob in that shape is silently inert: say so
        // instead of comfort-checking a value that does nothing. The same
        // pair gates the export slice, which reads that same store.
        let traces_store_served = self.daemon.api_enabled && self.daemon.max_retained_findings > 0;
        if self.daemon.max_export_findings > 0 && !traces_store_served {
            tracing::warn!(
                field = "max_export_findings",
                value = self.daemon.max_export_findings,
                "max_export_findings is ignored: only /api/export/report reads it, and \
                 api_enabled = false or max_retained_findings = 0 leaves it nothing to ship"
            );
        }
        if traces_store_served {
            // The floor guards correctness, not comfort: at 0 the export
            // carries no findings, so the gate's three finding-count rules
            // count none and pass whatever the daemon saw. The fourth rule,
            // io_waste_ratio_max, reads green_summary, which no cap empties,
            // so the verdict is not a blanket pass, only blind to findings.
            // The ceiling is the 8 MiB body limit `query inspect` and `query
            // monitor` read the snapshot through, at a few KB per finding.
            warn_outside_comfort_zone(
                "max_export_findings",
                &self.daemon.max_export_findings,
                &1,
                &2_000,
                "the export ships no findings, so the gate's finding-count rules count none and \
                 pass whatever the daemon detected, while io_waste_ratio_max still reads the \
                 batch summary and can still fail",
                "the snapshot can outgrow the 8 MiB body limit the query clients fetch it with",
            );
        }
        // Checked on the pair, after each knob's own zone: both fill the
        // same response body, and neither advisory above sees the sum.
        if traces_store_served
            && let Some(msg) = snapshot_budget_warning(
                self.daemon.max_export_findings,
                self.daemon.max_retained_traces,
            )
        {
            tracing::warn!(field = "max_export_findings+max_retained_traces", "{msg}");
        }
        if self.daemon.max_retained_traces > 0 && !traces_store_served {
            tracing::warn!(
                field = "max_retained_traces",
                value = self.daemon.max_retained_traces,
                "max_retained_traces is ignored: only /api/export/report reads the retained \
                 traces, and api_enabled = false or max_retained_findings = 0 disables it"
            );
        }
        if self.daemon.max_retained_traces > 0 && traces_store_served {
            warn_outside_comfort_zone(
                "max_retained_traces",
                &self.daemon.max_retained_traces,
                &10,
                &500,
                "exported reports will draw span trees for few findings",
                "each retained trace holds up to max_events_per_trace owned spans",
            );
        }
        warn_outside_comfort_zone(
            "trace_ttl_ms",
            &self.daemon.trace_ttl_ms,
            &1_000,
            &600_000,
            "TTL below 1s flushes traces before slow spans land",
            "TTL above 10min keeps near-dead traces in the active set",
        );
    }
}
