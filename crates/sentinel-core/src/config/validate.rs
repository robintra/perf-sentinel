//! Validation of a parsed [`Config`]: bound checks, comfort-zone warnings,
//! and control-character rejection for every TOML section.

use std::collections::HashMap;

use crate::score::cloud_energy::config::{CloudEnergyConfig, ServiceCloudConfig};
use crate::score::kepler::{KeplerConfig, KeplerMetricKind};
use crate::score::redfish::{RedfishConfig, RedfishEndpoint};
use crate::score::scaphandre::ScaphandreConfig;

use super::{Config, RESERVED_DISCLOSE_OUTPUT_PATH_VERSION};

fn check_range<T: PartialOrd + std::fmt::Display>(
    name: &str,
    val: &T,
    min: &T,
    max: &T,
) -> Result<(), String> {
    if val < min {
        return Err(format!("{name} must be >= {min}, got {val}"));
    }
    if val > max {
        return Err(format!("{name} must be <= {max}, got {val}"));
    }
    Ok(())
}

fn check_min<T: PartialOrd + std::fmt::Display>(
    name: &str,
    val: &T,
    min: &T,
) -> Result<(), String> {
    if val < min {
        return Err(format!("{name} must be >= {min}, got {val}"));
    }
    Ok(())
}

/// Emit a single startup warning when `val` is inside the hard bounds but
/// outside the recommended "comfort zone" `[comfort_lo, comfort_hi]`.
///
/// See design doc 07 > "Comfort-zone warnings" for the rationale and the
/// list of bands per field.
fn warn_outside_comfort_zone<T>(
    name: &str,
    val: &T,
    comfort_lo: &T,
    comfort_hi: &T,
    note_low: &str,
    note_high: &str,
) where
    T: PartialOrd + std::fmt::Display,
{
    if val < comfort_lo {
        tracing::warn!(
            field = %name,
            value = %val,
            recommended_min = %comfort_lo,
            "{name} = {val} is below the recommended floor {comfort_lo}; {note_low}"
        );
    } else if val > comfort_hi {
        tracing::warn!(
            field = %name,
            value = %val,
            recommended_max = %comfort_hi,
            "{name} = {val} is above the recommended ceiling {comfort_hi}; {note_high}"
        );
    }
}

/// `true` if `s` contains any terminal control character: C0 (`< 0x20`),
/// DEL (`0x7F`), or C1 (`0x80..=0x9F`). The C1 range carries the single-byte
/// CSI (`U+009B`), ST (`U+009C`) and OSC (`U+009D`) introducers honoured by
/// VT-family terminals when 8-bit controls are enabled, so a TOML field that
/// reaches `tracing::warn!` on stderr must reject them at load time the same
/// way [`crate::text_safety::sanitize_for_terminal`] rejects them at render.
pub(crate) fn has_control_char(s: &str) -> bool {
    s.chars().any(|c| {
        let code = c as u32;
        code < 0x20 || code == 0x7F || (0x80..=0x9F).contains(&code)
    })
}

/// Validate the wildcard-mode interactions of `[daemon.cors] allowed_origins`.
///
/// - `["*"]` mixed with explicit origins is ambiguous and silently degrades to
///   wildcard mode in `build_cors_layer`. Reject the mix at config load.
/// - `["*"]` combined with `[daemon.ack] api_key` lets any browser origin
///   replay a captured `X-API-Key` header (header-based auth, not blocked by
///   `allow_credentials = false`). Reject the combination.
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
            "[daemon.cors] allowed_origins = [\"*\"] is incompatible with \
             [daemon.ack] api_key, since X-API-Key is sent on every cross-origin \
             request and would be replayable from any browser tab. \
             Use an explicit origin list or unset api_key for development"
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

/// Validate the authority portion of an HTTP(S) URI.
/// Rejects credentials, empty host, control characters, and invalid port.
/// Handles IPv6 bracket notation (`[::1]`, `[::1]:8080`).
pub(super) fn validate_http_authority(url: &str, label: &str) -> Result<(), String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
    if authority.is_empty() {
        return Err(format!("{label} '{url}' has no host"));
    }
    if authority.contains('@') {
        return Err(format!(
            "{label} must not contain credentials (userinfo): '{url}'"
        ));
    }
    if has_control_char(authority) {
        return Err(format!("{label} '{url}' contains control characters"));
    }
    // Port validation: skip for bare IPv6 without port (`[::1]`), handle
    // bracketed IPv6 with port (`[::1]:8080`) via the `]:` delimiter.
    if authority.starts_with('[') {
        // IPv6 bracket notation: port follows `]:` if present.
        if let Some(bracket_end) = authority.find(']') {
            let after_bracket = &authority[bracket_end + 1..];
            if let Some(port_str) = after_bracket.strip_prefix(':')
                && !port_str.is_empty()
                && port_str.parse::<u16>().is_err()
            {
                return Err(format!("{label} '{url}' has an invalid port"));
            }
        }
    } else if let Some(port_str) = authority.rsplit(':').next()
        && authority.contains(':')
        && port_str.parse::<u16>().is_err()
    {
        return Err(format!("{label} '{url}' has an invalid port"));
    }
    Ok(())
}

impl Config {
    /// Validate that config values are within acceptable bounds.
    ///
    /// # Errors
    ///
    /// Returns a `String` description of the first invalid value found.
    /// The caller (`load_from_str`) wraps this in `ConfigError::Validation`.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_daemon_limits()?;
        self.validate_detection_params()?;
        self.validate_rates()?;
        self.validate_tls()?;
        self.validate_green()?;
        self.validate_daemon_ack()?;
        self.validate_daemon_cors()?;
        self.validate_daemon_archive()?;
        self.validate_reporting()?;
        self.validate_cross_section_consistency()?;
        Ok(())
    }

    /// Emit the non-loopback security advisory if applicable.
    ///
    /// The default is `127.0.0.1` (loopback). Advanced users may override
    /// to `0.0.0.0` for container deployments behind a reverse proxy. We
    /// warn loudly rather than rejecting, because the user's intent is
    /// explicit (they changed the config) and a hard reject would force
    /// workarounds (e.g., iptables) that are harder to audit.
    ///
    /// Kept separate from `validate()` because it is the only check
    /// that depends on CLI overrides (`--listen-address`), so the daemon
    /// entrypoint calls it a second time after applying the overrides.
    /// The other advisory warnings inside `validate()` are config-only
    /// and must be emitted exactly once, at load time, to avoid making
    /// an operator believe the daemon validates the same config twice.
    pub fn warn_listen_addr_if_non_loopback(&self) {
        if self.daemon.listen_addr != "127.0.0.1" && self.daemon.listen_addr != "::1" {
            tracing::warn!(
                "Daemon configured to listen on non-loopback address: {}. \
                 Endpoints have no authentication, use a reverse proxy or \
                 network policy for security.",
                self.daemon.listen_addr
            );
        }
    }

    /// Validate `[reporting]` settings. Rejects unknown intent /
    /// confidentiality values and requires `org_config_path` when
    /// `intent = "official"`.
    fn validate_reporting(&self) -> Result<(), String> {
        if let Some(intent) = &self.reporting.intent {
            match intent.as_str() {
                "internal" | "official" | "audited" => {}
                other => {
                    return Err(format!(
                        "[reporting] intent must be one of \"internal\", \"official\", \"audited\", got {other:?}"
                    ));
                }
            }
        }
        if let Some(level) = &self.reporting.confidentiality_level {
            match level.as_str() {
                "internal" | "public" => {}
                other => {
                    return Err(format!(
                        "[reporting] confidentiality_level must be \"internal\" or \"public\", got {other:?}"
                    ));
                }
            }
        }
        if self.reporting.intent.as_deref() == Some("official")
            && self
                .reporting
                .org_config_path
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(
                "[reporting] org_config_path is required when intent = \"official\"".to_string(),
            );
        }
        Ok(())
    }

    /// Reporting-section advisory warnings emitted at load time only.
    /// Kept separate from `validate_reporting` because the daemon
    /// entrypoint re-runs `validate()` after applying CLI overrides
    /// (`--listen-address`, ports), and an advisory not affected by
    /// those overrides must not be re-emitted, otherwise an operator
    /// upgrading 0.6.2 -> 0.7.0 sees the same warning twice and
    /// suspects two daemon instances or a duplicated config layer.
    pub(super) fn warn_reporting_advisory(&self) {
        if self
            .reporting
            .disclose_output_path
            .as_deref()
            .is_some_and(|p| !p.is_empty())
        {
            tracing::warn!(
                "[reporting] disclose_output_path is set but currently unused. \
                 Reserved for daemon-triggered periodic disclosures (planned for {}). \
                 Reports today are produced exclusively via `perf-sentinel disclose --output`.",
                RESERVED_DISCLOSE_OUTPUT_PATH_VERSION
            );
        }
    }

    /// Validate `[daemon.archive]` settings when present.
    fn validate_daemon_archive(&self) -> Result<(), String> {
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

    /// Cross-section consistency checks that no individual section
    /// can validate alone. Today this is small (CORS-vs-API), but
    /// `validate` is intentionally extensible: any future "you set X
    /// but Y is off" trap belongs here.
    fn validate_cross_section_consistency(&self) -> Result<(), String> {
        if !self.daemon.api_enabled && !self.daemon.cors.allowed_origins.is_empty() {
            return Err(
                "[daemon.cors] allowed_origins is set but [daemon] api_enabled = false. \
                 The CORS layer would attach to a non-mounted /api/* sub-router and \
                 silently do nothing, which is almost always a misconfiguration. \
                 Either remove [daemon.cors] allowed_origins for this environment, or \
                 enable the API with [daemon] api_enabled = true."
                    .to_string(),
            );
        }
        if self.daemon.archive.is_some() && !self.green.enabled {
            return Err(
                "[daemon.archive] is configured but [green] enabled = false. The archive \
                 would write windows with zero carbon/energy, making `perf-sentinel disclose` \
                 produce a meaningless output. Either enable green scoring or remove the \
                 archive section."
                    .to_string(),
            );
        }
        Ok(())
    }

    pub(super) fn validate_daemon_cors(&self) -> Result<(), String> {
        let has_wildcard = self.daemon.cors.allowed_origins.iter().any(|o| o == "*");
        validate_cors_wildcard_mode(
            has_wildcard,
            self.daemon.cors.allowed_origins.len(),
            self.daemon.ack.api_key.is_some(),
        )?;
        for origin in &self.daemon.cors.allowed_origins {
            validate_cors_origin(origin)?;
        }
        Ok(())
    }

    /// Validate `[daemon.ack]` settings.
    pub(super) fn validate_daemon_ack(&self) -> Result<(), String> {
        if let Some(key) = &self.daemon.ack.api_key {
            if key.is_empty() {
                return Err("[daemon.ack] api_key must not be empty".to_string());
            }
            if has_control_char(key) {
                return Err("[daemon.ack] api_key contains control characters".to_string());
            }
            // Hard reject obviously-broken keys. The threat model is a
            // co-resident local attacker hitting the loopback API at
            // line rate, with no rate limiting on the daemon side.
            // 36^12 ~= 4.7e18 is well past the brute-force horizon for
            // any realistic deployment, 16+ remains the recommended
            // floor for production.
            if key.len() < 12 {
                return Err(format!(
                    "[daemon.ack] api_key is too short ({} chars), \
                     use at least 12 characters (16 recommended)",
                    key.len()
                ));
            }
            if key.len() < 16 {
                tracing::warn!(
                    len = key.len(),
                    "[daemon.ack] api_key is shorter than 16 characters, \
                     consider a longer secret to resist brute-force attempts"
                );
            }
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

    /// Validate TLS configuration: both paths must be set or both absent.
    /// When set, verify the files exist and warn if the key is
    /// world-readable on Unix.
    pub(super) fn validate_tls(&self) -> Result<(), String> {
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

    fn validate_green(&self) -> Result<(), String> {
        Self::validate_embodied_carbon(self.green.embodied_carbon_per_request_gco2)?;
        Self::validate_default_region(self.green.default_region.as_deref())?;
        Self::validate_service_regions(&self.green.service_regions)?;
        if let Some(cfg) = &self.green.scaphandre {
            Self::validate_scaphandre(cfg)?;
        }
        if let Some(cfg) = &self.green.kepler {
            Self::validate_kepler(cfg)?;
        }
        if let Some(cfg) = &self.green.redfish {
            Self::validate_redfish(cfg)?;
        }
        if let Some(cfg) = &self.green.cloud_energy {
            Self::validate_cloud_energy(cfg)?;
        }
        Self::validate_network_energy(self.green.network_energy_per_byte_kwh)?;
        self.validate_hourly_profiles_file()?;
        if let Some(cfg) = &self.green.electricity_maps {
            Self::validate_electricity_maps(cfg)?;
        }
        Ok(())
    }

    fn validate_embodied_carbon(value: f64) -> Result<(), String> {
        if !value.is_finite() {
            return Err(format!(
                "embodied_carbon_per_request_gco2 must be finite, got {value}"
            ));
        }
        if value < 0.0 {
            return Err(format!(
                "embodied_carbon_per_request_gco2 must be >= 0.0, got {value}"
            ));
        }
        Ok(())
    }

    /// Validate the optional `[green] default_region`. Config is trusted
    /// input, so typos surface loudly here rather than silently producing
    /// zeroed CO₂ rows downstream. Same validator used at the OTLP
    /// ingestion boundary (there, invalid values are silently dropped).
    fn validate_default_region(region: Option<&str>) -> Result<(), String> {
        let Some(region) = region else {
            return Ok(());
        };
        if crate::score::carbon::is_valid_region_id(region) {
            return Ok(());
        }
        Err(format!(
            "[green] default_region '{region}' contains invalid characters; \
             expected ASCII alphanumeric + '-' or '_', length 1-64"
        ))
    }

    /// Validate the `[green.service_regions]` map: cardinality cap, plus
    /// region-id syntax on every key/value pair.
    fn validate_service_regions(map: &HashMap<String, String>) -> Result<(), String> {
        /// Maximum number of entries in `[green.service_regions]`.
        /// Bounds the config-load memory footprint against fat-finger or
        /// malicious configs. 1024 is 4× `MAX_REGIONS` (256) and comfortably
        /// above any realistic multi-cloud deployment size.
        const MAX_SERVICE_REGIONS: usize = 1024;
        if map.len() > MAX_SERVICE_REGIONS {
            return Err(format!(
                "[green.service_regions] has {} entries; maximum is {MAX_SERVICE_REGIONS}",
                map.len()
            ));
        }
        for (service, region) in map {
            if !crate::score::carbon::is_valid_region_id(service) {
                return Err(format!(
                    "[green.service_regions] invalid service name '{service}'; \
                     expected ASCII alphanumeric + '-' or '_', length 1-64"
                ));
            }
            if !crate::score::carbon::is_valid_region_id(region) {
                return Err(format!(
                    "[green.service_regions] invalid region '{region}' for service '{service}'; \
                     expected ASCII alphanumeric + '-' or '_', length 1-64"
                ));
            }
        }
        Ok(())
    }

    fn validate_network_energy(value: f64) -> Result<(), String> {
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "network_energy_per_byte_kwh must be finite and >= 0.0, got {value}"
            ));
        }
        Ok(())
    }

    /// Validate `[green] hourly_profiles_file`: reject control characters
    /// in the path (log injection) and require that the file actually
    /// loaded when the field is configured.
    fn validate_hourly_profiles_file(&self) -> Result<(), String> {
        let Some(path) = &self.green.hourly_profiles_file else {
            return Ok(());
        };
        if has_control_char(path) {
            return Err("[green] hourly_profiles_file contains control characters".to_string());
        }
        if self.green.custom_hourly_profiles.is_none() {
            return Err(format!(
                "[green] hourly_profiles_file '{path}' was configured but \
                 failed to load. Remove the field to use embedded profiles only."
            ));
        }
        Ok(())
    }

    /// Validate a parsed `[green.electricity_maps]` config section.
    pub(super) fn validate_electricity_maps(
        cfg: &crate::score::electricity_maps::ElectricityMapsConfig,
    ) -> Result<(), String> {
        if cfg.auth_token.is_empty() {
            return Err(
                "[green.electricity_maps] api_key or PERF_SENTINEL_EMAPS_TOKEN is required"
                    .to_string(),
            );
        }
        if has_control_char(&cfg.auth_token) {
            return Err(
                "[green.electricity_maps] auth token contains control characters".to_string(),
            );
        }
        validate_http_authority(&cfg.api_endpoint, "[green.electricity_maps] endpoint")?;
        // Warn (but do not fail) when a non-empty auth token travels to an
        // http:// endpoint. The Electricity Maps production API is served
        // over https in practice; an http:// endpoint usually means a local
        // test server or a misconfiguration. Flag it so users do not
        // silently ship credentials in cleartext.
        if cfg.api_endpoint.starts_with("http://") && !cfg.auth_token.is_empty() {
            tracing::warn!(
                "[green.electricity_maps] auth token will be sent over http:// \
                 (no TLS). Use https:// for production or set the endpoint to \
                 a loopback/private address if this is intentional."
            );
        }
        let secs = cfg.poll_interval.as_secs();
        check_range(
            "[green.electricity_maps] poll_interval_secs",
            &secs,
            &60,
            &86400,
        )?;
        if cfg.region_map.is_empty() {
            return Err(
                "[green.electricity_maps] region_map must contain at least one entry".to_string(),
            );
        }
        for (region, zone) in &cfg.region_map {
            if zone.is_empty() {
                return Err(format!(
                    "[green.electricity_maps.region_map] zone for '{region}' is empty"
                ));
            }
            if has_control_char(zone)
                || zone.contains('&')
                || zone.contains('#')
                || zone.contains('=')
                || zone.contains('?')
                || zone.contains('%')
                || zone.contains(' ')
                || zone.contains('+')
            {
                return Err(format!(
                    "[green.electricity_maps.region_map] zone '{zone}' for '{region}' \
                     contains invalid characters"
                ));
            }
            if has_control_char(region) {
                return Err(format!(
                    "[green.electricity_maps.region_map] region key '{region}' \
                     contains control characters"
                ));
            }
        }
        Ok(())
    }

    /// Validate a parsed `[green.scaphandre]` config section.
    ///
    /// Rejects: empty endpoint, non-`http://` scheme, credentials in
    /// authority, control characters, invalid port, `scrape_interval_secs`
    /// outside [1, 3600], and `process_map` keys/values that are empty,
    /// >256 chars, or contain control characters.
    fn validate_scaphandre(cfg: &ScaphandreConfig) -> Result<(), String> {
        if cfg.endpoint.is_empty() {
            return Err(
                "[green.scaphandre] endpoint is required when the section is present".to_string(),
            );
        }
        if !cfg.endpoint.starts_with("http://") && !cfg.endpoint.starts_with("https://") {
            return Err(format!(
                "[green.scaphandre] endpoint '{}' must start with 'http://' or 'https://'",
                cfg.endpoint
            ));
        }
        validate_http_authority(&cfg.endpoint, "[green.scaphandre] endpoint")?;
        let secs = cfg.scrape_interval.as_secs();
        if !(1..=3600).contains(&secs) {
            return Err(format!(
                "[green.scaphandre] scrape_interval_secs must be in [1, 3600], got {secs}"
            ));
        }
        Self::validate_scaphandre_process_map(cfg)?;
        // The `AuthHeader` type lives in the `ingest` module, which is
        // only compiled when hyper is pulled in via one of the daemon /
        // tempo / jaeger-query features. Bare `cargo publish` builds
        // `sentinel-core` with no features and must skip the parse.
        #[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
        if let Some(auth) = cfg.auth_header.as_deref() {
            crate::ingest::auth_header::AuthHeader::parse(auth)
                .map_err(|msg| format!("[green.scaphandre] auth_header: {msg}"))?;
        }
        Ok(())
    }

    /// Validate a parsed `[green.kepler]` config section.
    ///
    /// Same shape as [`Self::validate_scaphandre`]: rejects empty
    /// endpoints, non-`http(s)` schemes, embedded credentials, control
    /// chars, invalid ports, `scrape_interval_secs` outside [1, 3600],
    /// and `service_mappings` keys/values outside [1, 256] chars or with
    /// control chars.
    pub(super) fn validate_kepler(cfg: &KeplerConfig) -> Result<(), String> {
        if cfg.endpoint.is_empty() {
            return Err(
                "[green.kepler] endpoint is required when the section is present".to_string(),
            );
        }
        if !cfg.endpoint.starts_with("http://") && !cfg.endpoint.starts_with("https://") {
            return Err(format!(
                "[green.kepler] endpoint '{}' must start with 'http://' or 'https://'",
                cfg.endpoint
            ));
        }
        validate_http_authority(&cfg.endpoint, "[green.kepler] endpoint")?;
        let secs = cfg.scrape_interval.as_secs();
        if !(1..=3600).contains(&secs) {
            return Err(format!(
                "[green.kepler] scrape_interval_secs must be in [1, 3600], got {secs}"
            ));
        }
        Self::validate_kepler_service_mappings(cfg)?;
        #[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
        if let Some(auth) = cfg.auth_header.as_deref() {
            crate::ingest::auth_header::AuthHeader::parse(auth)
                .map_err(|msg| format!("[green.kepler] auth_header: {msg}"))?;
        }
        Ok(())
    }

    /// Validate `[green.kepler].service_mappings` keys and values.
    /// Label cap depends on `metric_kind`: 256 for `Container` (full
    /// `container_name`), 15 for `Process` since the kernel truncates
    /// `comm` at `TASK_COMM_LEN - 1`. The cap is `len()` bytes, not
    /// chars, matching the kernel's byte-bounded truncation.
    fn validate_kepler_service_mappings(cfg: &KeplerConfig) -> Result<(), String> {
        /// Memory-footprint cap, mirrors `MAX_SERVICE_REGIONS`.
        const MAX_KEPLER_SERVICE_MAPPINGS: usize = 1024;
        if cfg.service_mappings.len() > MAX_KEPLER_SERVICE_MAPPINGS {
            return Err(format!(
                "[green.kepler] service_mappings has {} entries; maximum is {MAX_KEPLER_SERVICE_MAPPINGS}",
                cfg.service_mappings.len()
            ));
        }
        let (max_label_len, label_hint) = match cfg.metric_kind {
            KeplerMetricKind::Container => (256_usize, ""),
            KeplerMetricKind::Process => (
                15_usize,
                " (the Linux kernel truncates `comm` to 15 bytes, \
                  provide the truncated value, not the full binary path)",
            ),
        };
        for (service, label) in &cfg.service_mappings {
            // Reject control chars first so an ANSI-laden label is not
            // echoed back to stderr via the length-error `format!`.
            if has_control_char(service) {
                return Err("[green.kepler] service_mappings has a service name \
                     that contains control characters"
                    .to_string());
            }
            if has_control_char(label) {
                return Err(format!(
                    "[green.kepler] service_mappings has a label \
                     for service '{service}' that contains control characters"
                ));
            }
            if service.is_empty() || service.len() > 256 {
                return Err(format!(
                    "[green.kepler] service_mappings service name '{service}' must be 1-256 chars"
                ));
            }
            if label.is_empty() || label.len() > max_label_len {
                return Err(format!(
                    "[green.kepler] service_mappings label for service '{service}' \
                     must be 1-{max_label_len} chars, got '{label}'{label_hint}"
                ));
            }
        }
        Ok(())
    }

    /// Validate a parsed `[green.redfish]` config section.
    ///
    /// Enforces the BMC-specific scrape-interval lower bound
    /// (`MIN_SCRAPE_INTERVAL_SECS`), checks every endpoint URL, walks
    /// the service mapping for control chars + length bounds, ensures
    /// every mapped chassis exists in `endpoints`, and confirms that
    /// the `ca_bundle_path` file is readable when set.
    pub(super) fn validate_redfish(cfg: &RedfishConfig) -> Result<(), String> {
        use crate::score::redfish::config::{MAX_SCRAPE_INTERVAL_SECS, MIN_SCRAPE_INTERVAL_SECS};
        if cfg.endpoints.is_empty() {
            return Err(
                "[green.redfish] endpoints must contain at least one chassis when the section is present"
                    .to_string(),
            );
        }
        Self::validate_redfish_endpoints(&cfg.endpoints)?;
        let secs = cfg.scrape_interval.as_secs();
        if !(MIN_SCRAPE_INTERVAL_SECS..=MAX_SCRAPE_INTERVAL_SECS).contains(&secs) {
            return Err(format!(
                "[green.redfish] scrape_interval_secs must be in [{MIN_SCRAPE_INTERVAL_SECS}, {MAX_SCRAPE_INTERVAL_SECS}], got {secs}. \
                 The lower bound defends against BMC rate-limit retaliation."
            ));
        }
        Self::validate_redfish_service_mappings(&cfg.service_mappings, &cfg.endpoints)?;
        if let Some(bundle) = cfg.ca_bundle_path.as_deref()
            && bundle.is_empty()
        {
            return Err("[green.redfish] ca_bundle_path must be non-empty when set".to_string());
        }
        // No filesystem probe on `ca_bundle_path`: the scraper task
        // refuses to start the moment the field is set (see
        // `score/redfish/scraper.rs`), so a metadata() check here would
        // only add a path-probe attack surface for no operator benefit
        // until custom-CA TLS lands.
        #[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
        if let Some(auth) = cfg.auth_header.as_deref() {
            crate::ingest::auth_header::AuthHeader::parse(auth)
                .map_err(|msg| format!("[green.redfish] auth_header: {msg}"))?;
        }
        Ok(())
    }

    /// Validate each `chassis_id -> RedfishEndpoint` pair in
    /// `[green.redfish.endpoints]`. The `schema` field is type-checked
    /// by serde at deserialization, so only the URL needs runtime
    /// validation here.
    fn validate_redfish_endpoints(
        endpoints: &HashMap<String, RedfishEndpoint>,
    ) -> Result<(), String> {
        for (chassis_id, endpoint) in endpoints {
            if chassis_id.is_empty() || chassis_id.len() > 256 {
                return Err(format!(
                    "[green.redfish] endpoints chassis id '{chassis_id}' must be 1-256 chars"
                ));
            }
            if has_control_char(chassis_id) {
                return Err(format!(
                    "[green.redfish] endpoints chassis id '{chassis_id}' contains control characters"
                ));
            }
            let url = &endpoint.url;
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(format!(
                    "[green.redfish] endpoint URL for chassis '{chassis_id}' must start with 'http://' or 'https://', got '{url}'"
                ));
            }
            validate_http_authority(
                url,
                &format!("[green.redfish] endpoint URL for chassis '{chassis_id}'"),
            )?;
        }
        Ok(())
    }

    /// Validate each `service -> chassis_id` pair in `[green.redfish.service_mappings]`.
    /// Every mapped chassis must already be declared in `endpoints`.
    fn validate_redfish_service_mappings(
        service_mappings: &HashMap<String, String>,
        endpoints: &HashMap<String, RedfishEndpoint>,
    ) -> Result<(), String> {
        for (service, chassis_id) in service_mappings {
            if service.is_empty() || service.len() > 256 {
                return Err(format!(
                    "[green.redfish] service_mappings service name '{service}' must be 1-256 chars"
                ));
            }
            if has_control_char(service) {
                return Err(format!(
                    "[green.redfish] service_mappings service name '{service}' contains control characters"
                ));
            }
            if !endpoints.contains_key(chassis_id) {
                return Err(format!(
                    "[green.redfish] service '{service}' maps to chassis '{chassis_id}' which is not declared in [green.redfish.endpoints]"
                ));
            }
        }
        Ok(())
    }

    /// Validate `[green.scaphandre].process_map` keys and values.
    ///
    /// Service names (keys), `exe_contains` substrings and optional
    /// `cmdline_contains` substrings must be 1 to 256 chars and free
    /// of control characters. Service names are intentionally NOT run
    /// through `is_valid_region_id` because they may legitimately
    /// contain dots, slashes and similar.
    fn validate_scaphandre_process_map(cfg: &ScaphandreConfig) -> Result<(), String> {
        for (service, matcher) in &cfg.process_map {
            Self::validate_scaphandre_substring(service, "service name", service)?;
            Self::validate_scaphandre_substring(&matcher.exe_contains, "exe_contains", service)?;
            if let Some(cmdline) = matcher.cmdline_contains.as_deref() {
                Self::validate_scaphandre_substring(cmdline, "cmdline_contains", service)?;
            }
        }
        Ok(())
    }

    /// Length and control-char validation for one `process_map` string
    /// field. Extracted so [`validate_scaphandre_process_map`] stays
    /// below the cognitive-complexity ceiling. `kind` is the field
    /// label inserted into the error message (e.g. `"exe_contains"`),
    /// `service` is the surrounding service name used for operator
    /// context.
    fn validate_scaphandre_substring(value: &str, kind: &str, service: &str) -> Result<(), String> {
        if value.is_empty() || value.len() > 256 {
            return Err(format!(
                "[green.scaphandre] process_map {kind} for service '{service}' \
                 must be 1-256 chars, got '{value}'"
            ));
        }
        if has_control_char(value) {
            return Err(format!(
                "[green.scaphandre] process_map {kind} for service '{service}' \
                 contains control characters"
            ));
        }
        Ok(())
    }

    /// Validate a parsed `[green.cloud]` config section.
    fn validate_cloud_energy(cfg: &CloudEnergyConfig) -> Result<(), String> {
        Self::validate_cloud_endpoint(cfg)?;
        Self::validate_cloud_services(cfg)?;
        // See the twin note in `validate_scaphandre`: the `AuthHeader`
        // type is feature-gated, so bare no-features builds skip it.
        #[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
        if let Some(auth) = cfg.auth_header.as_deref() {
            crate::ingest::auth_header::AuthHeader::parse(auth)
                .map_err(|msg| format!("[green.cloud] auth_header: {msg}"))?;
        }
        Ok(())
    }

    /// Validate `[green.cloud]` endpoint, scrape interval, provider, and instance type.
    fn validate_cloud_endpoint(cfg: &CloudEnergyConfig) -> Result<(), String> {
        if cfg.prometheus_endpoint.is_empty() {
            return Err(
                "[green.cloud] prometheus_endpoint is required when the section is present"
                    .to_string(),
            );
        }
        if !cfg.prometheus_endpoint.starts_with("http://")
            && !cfg.prometheus_endpoint.starts_with("https://")
        {
            return Err(format!(
                "[green.cloud] prometheus_endpoint '{}' must start with 'http://' or 'https://'",
                cfg.prometheus_endpoint
            ));
        }
        validate_http_authority(
            &cfg.prometheus_endpoint,
            "[green.cloud] prometheus_endpoint",
        )?;
        let secs = cfg.scrape_interval.as_secs();
        if !(1..=3600).contains(&secs) {
            return Err(format!(
                "[green.cloud] scrape_interval_secs must be in [1, 3600], got {secs}"
            ));
        }
        if let Some(ref p) = cfg.default_provider
            && !matches!(p.as_str(), "aws" | "gcp" | "azure")
        {
            return Err(format!(
                "[green.cloud] default_provider must be 'aws', 'gcp', or 'azure', got '{p}'"
            ));
        }
        if let Some(ref it) = cfg.default_instance_type
            && !crate::score::cloud_energy::table::is_known_instance_type(it)
        {
            tracing::warn!(
                instance_type = %it,
                "[green.cloud] default_instance_type is not in the embedded \
                 SPECpower table; the provider default watts will be used"
            );
        }
        if let Some(ref m) = cfg.cpu_metric
            && has_control_char(m)
        {
            return Err("[green.cloud] cpu_metric contains control characters".to_string());
        }
        Ok(())
    }

    /// Validate per-service entries in `[green.cloud.services]`: cardinality
    /// cap, name/control-char checks, watts ranges, instance type lookup.
    fn validate_cloud_services(cfg: &CloudEnergyConfig) -> Result<(), String> {
        const MAX_CLOUD_SERVICES: usize = 256;
        if cfg.services.len() > MAX_CLOUD_SERVICES {
            return Err(format!(
                "[green.cloud.services] has {} entries; maximum is {MAX_CLOUD_SERVICES}",
                cfg.services.len()
            ));
        }
        for (service, svc_cfg) in &cfg.services {
            Self::validate_cloud_service_name(service)?;
            Self::validate_cloud_service_cpu_query(service, svc_cfg)?;
            match svc_cfg {
                ServiceCloudConfig::ManualWatts {
                    idle_watts,
                    max_watts,
                    ..
                } => Self::validate_manual_watts(service, *idle_watts, *max_watts)?,
                ServiceCloudConfig::InstanceType {
                    provider,
                    instance_type,
                    ..
                } => Self::validate_instance_type_variant(
                    service,
                    provider.as_deref(),
                    instance_type,
                )?,
            }
        }
        Ok(())
    }

    /// Shape + control-char check on a cloud service name.
    fn validate_cloud_service_name(service: &str) -> Result<(), String> {
        if service.is_empty() || service.len() > 256 {
            return Err(format!(
                "[green.cloud.services] service name '{service}' must be 1-256 chars"
            ));
        }
        if has_control_char(service) {
            return Err(format!(
                "[green.cloud.services] service name '{service}' contains control characters"
            ));
        }
        Ok(())
    }

    /// Reject control characters in a service's optional per-service
    /// `cpu_query` override (log-injection / Prometheus-label-injection
    /// guard).
    fn validate_cloud_service_cpu_query(
        service: &str,
        svc_cfg: &ServiceCloudConfig,
    ) -> Result<(), String> {
        let Some(q) = svc_cfg.cpu_query() else {
            return Ok(());
        };
        if has_control_char(q) {
            return Err(format!(
                "[green.cloud.services.{service}] cpu_query contains control characters"
            ));
        }
        Ok(())
    }

    /// Validate a [`ServiceCloudConfig::ManualWatts`] arm: both values
    /// finite and non-negative, and `max_watts >= idle_watts`.
    fn validate_manual_watts(service: &str, idle_watts: f64, max_watts: f64) -> Result<(), String> {
        if !idle_watts.is_finite() || idle_watts < 0.0 {
            return Err(format!(
                "[green.cloud.services.{service}] idle_watts must be finite and >= 0, \
                 got {idle_watts}"
            ));
        }
        if !max_watts.is_finite() || max_watts < 0.0 {
            return Err(format!(
                "[green.cloud.services.{service}] max_watts must be finite and >= 0, \
                 got {max_watts}"
            ));
        }
        if max_watts < idle_watts {
            return Err(format!(
                "[green.cloud.services.{service}] max_watts ({max_watts}) must be \
                 >= idle_watts ({idle_watts})"
            ));
        }
        Ok(())
    }

    /// Validate a [`ServiceCloudConfig::InstanceType`] arm: provider
    /// allow-list, control-char rejection on `instance_type`, and a
    /// soft warning when the type is not in the embedded `SPECpower`
    /// table (not an error, the provider default is used instead).
    fn validate_instance_type_variant(
        service: &str,
        provider: Option<&str>,
        instance_type: &str,
    ) -> Result<(), String> {
        if let Some(p) = provider
            && !matches!(p, "aws" | "gcp" | "azure")
        {
            return Err(format!(
                "[green.cloud.services.{service}] provider must be 'aws', 'gcp', \
                 or 'azure', got '{p}'"
            ));
        }
        if has_control_char(instance_type) {
            return Err(format!(
                "[green.cloud.services.{service}] instance_type contains control characters"
            ));
        }
        if !instance_type.is_empty()
            && !crate::score::cloud_energy::table::is_known_instance_type(instance_type)
        {
            tracing::warn!(
                service = %service,
                instance_type = %instance_type,
                "[green.cloud.services] instance_type is not in the embedded \
                 SPECpower table; provider default watts will be used"
            );
        }
        Ok(())
    }

    fn validate_daemon_limits(&self) -> Result<(), String> {
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
        check_range("trace_ttl_ms", &self.daemon.trace_ttl_ms, &100, &3_600_000)?;
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
        // The 16 MiB ceiling intentionally matches the `max_payload_size`
        // default value (see line 205). Default-at-ceiling is inclusive
        // (`..=`), so the canonical config emits no warning. A future
        // bump of the default must also raise this ceiling, otherwise
        // every fresh daemon would log a startup warning.
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
        // Skip the comfort-zone check when the store is intentionally
        // disabled (max_retained_findings == 0); warning on that would
        // be noise.
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
        warn_outside_comfort_zone(
            "trace_ttl_ms",
            &self.daemon.trace_ttl_ms,
            &1_000,
            &600_000,
            "TTL below 1s flushes traces before slow spans land",
            "TTL above 10min keeps near-dead traces in the active set",
        );
    }

    fn validate_detection_params(&self) -> Result<(), String> {
        check_min(
            "n_plus_one_threshold",
            &self.detection.n_plus_one_threshold,
            &1,
        )?;
        check_min("window_duration_ms", &self.detection.window_duration_ms, &1)?;
        check_min(
            "slow_query_threshold_ms",
            &self.detection.slow_query_threshold_ms,
            &1,
        )?;
        check_min(
            "slow_query_min_occurrences",
            &self.detection.slow_query_min_occurrences,
            &1,
        )?;
        check_range("max_fanout", &self.detection.max_fanout, &1, &100_000)?;
        warn_outside_comfort_zone(
            "max_fanout",
            &self.detection.max_fanout,
            &5,
            &1_000,
            "very low fanout floods the findings store with noise",
            "very high fanout suppresses most fan-out detections",
        );
        check_min(
            "chatty_service_min_calls",
            &self.detection.chatty_service_min_calls,
            &1,
        )?;
        check_min(
            "pool_saturation_concurrent_threshold",
            &self.detection.pool_saturation_concurrent_threshold,
            &2,
        )?;
        check_min(
            "serialized_min_sequential",
            &self.detection.serialized_min_sequential,
            &2,
        )?;
        Ok(())
    }

    fn validate_rates(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.daemon.sampling_rate) {
            return Err(format!(
                "sampling_rate must be in [0.0, 1.0], got {}",
                self.daemon.sampling_rate
            ));
        }
        if !(0.0..=1.0).contains(&self.thresholds.io_waste_ratio_max) {
            return Err(format!(
                "io_waste_ratio_max must be in [0.0, 1.0], got {}",
                self.thresholds.io_waste_ratio_max
            ));
        }
        Ok(())
    }
}
