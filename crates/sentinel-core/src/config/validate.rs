//! Validation of a parsed [`Config`]: bound checks, comfort-zone warnings,
//! and control-character rejection for every TOML section.

mod daemon;
mod green;

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

/// Body limit the query clients read `/api/export/report` with, mirroring
/// `http_client::MAX_BODY_BYTES`. Spelled out here because config
/// validation compiles without the features that gate `http_client`, and
/// `snapshot_read_limit_matches_the_client_cap` fails if the two drift.
pub(super) const SNAPSHOT_READ_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// Rough serialized weight of one exported finding and one retained span
/// tree, measured on a production daemon (~3.5 KB and ~20 KB). Orders of
/// magnitude, not a contract: they exist to catch a configuration whose
/// snapshot cannot be read back.
const APPROX_FINDING_BYTES: usize = 3_500;
const APPROX_TRACE_BYTES: usize = 20_000;

/// Ceiling the exported span trees can reach, mirroring
/// `query_api::EMBEDDED_TRACES_BYTE_BUDGET` (half the body limit).
/// `traces_store::snapshot_for` measures each tree and skips the ones
/// that would cross it, so projecting `max_retained_traces` unclamped
/// warns about bytes that never ship and points the operator at the one
/// knob already bounded. `embedded_traces_budget_matches_the_export`
/// fails if the two drift.
pub(super) const EMBEDDED_TRACES_BUDGET_BYTES: usize = SNAPSHOT_READ_LIMIT_BYTES / 2;

/// `Some(message)` when `max_export_findings` and `max_retained_traces`
/// together project a snapshot past the body limit the query clients read
/// it with.
///
/// The two knobs fill the same response body, but each is comfort-checked
/// alone, so a pair sitting inside both zones (2000 findings, 400 traces)
/// still projects ~11 MiB. Nothing downstream reports that: `fetch_json`
/// reduces the oversize read to `None`, and `query inspect` then renders
/// its "no analysis summary" hint as though the daemon were empty.
pub(super) fn snapshot_budget_warning(findings: usize, traces: usize) -> Option<String> {
    let limit = SNAPSHOT_READ_LIMIT_BYTES;
    let projected = findings
        .saturating_mul(APPROX_FINDING_BYTES)
        .saturating_add(
            traces
                .saturating_mul(APPROX_TRACE_BYTES)
                .min(EMBEDDED_TRACES_BUDGET_BYTES),
        );
    if projected <= limit {
        return None;
    }
    // Rounded, not truncated: 11.2 MiB reported as "10 MiB" would understate
    // the overrun the operator is being asked to fix.
    let mib = |b: usize| b.div_ceil(1024 * 1024);
    Some(format!(
        "max_export_findings = {findings} and max_retained_traces = {traces} project a \
         snapshot around {} MiB, past the {} MiB body limit `query inspect` and `query \
         monitor` fetch /api/export/report with. Over it they show no data rather than \
         an error. Lower either knob.",
        mib(projected),
        mib(limit),
    ))
}

/// `true` if `s` contains any terminal control character: C0 (`< 0x20`),
/// DEL (`0x7F`), or C1 (`0x80..=0x9F`). The C1 range carries the single-byte
/// CSI (`U+009B`), ST (`U+009C`) and OSC (`U+009D`) introducers honoured by
/// VT-family terminals when 8-bit controls are enabled. A TOML field that
/// reaches `tracing::warn!` on stderr must reject them at load time, the same
/// way [`crate::text_safety::sanitize_for_terminal`] rejects them at render.
pub(crate) fn has_control_char(s: &str) -> bool {
    s.chars().any(|c| {
        let code = c as u32;
        code < 0x20 || code == 0x7F || (0x80..=0x9F).contains(&code)
    })
}

/// Shared declared-workload field checks, used by the raw TOML pass
/// (fail loud at load) and the typed pass (defense in depth for
/// programmatic construction). Control chars are rejected before the
/// value can reach an error message. `section` names the caller's TOML
/// block so a broker misconfiguration does not point at the database.
pub(super) fn validate_workload_fields(
    section: &str,
    label_value: &str,
    region: Option<&str>,
) -> Result<(), String> {
    if has_control_char(label_value) {
        return Err(format!("{section} label_value contains control characters"));
    }
    if label_value.trim().is_empty() || label_value.len() > 256 {
        return Err(format!(
            "{section} label_value must be 1-256 chars and not blank, got '{label_value}'"
        ));
    }
    if let Some(region) = region
        && !crate::score::carbon::is_valid_region_id(region)
    {
        return Err(format!(
            "{section} region '{region}' contains invalid characters; \
             allowed: ASCII letters, digits, '-' and '_', 1-64 chars"
        ));
    }
    Ok(())
}

/// Warn when a charset-valid region is absent from the embedded table.
/// Unknown ids are legitimate (custom on-prem ids covered by Electricity
/// Maps), so this warns instead of rejecting.
pub(super) fn warn_unknown_region(section: &str, region: Option<&str>) {
    if let Some(region) = region
        && crate::score::carbon::lookup_region_lower(&region.to_ascii_lowercase()).is_none()
    {
        tracing::warn!(
            region,
            section,
            "declared region is not in the embedded intensity table: \
             waste_gco2 will be absent unless Electricity Maps real-time \
             intensity covers it. Check for a typo (e.g. eu-west-3)."
        );
    }
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
        self.validate_daemon_incidents()?;
        self.validate_daemon_read_key()?;
        self.validate_daemon_cors()?;
        self.validate_daemon_archive()?;
        self.validate_daemon_hub_export()?;
        self.validate_reporting()?;
        self.validate_cross_section_consistency()?;
        Ok(())
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

    /// Reporting-section advisory warnings, emitted once by the loader
    /// after `validate()` on the merged configuration.
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

    /// Cross-section consistency checks that no individual section
    /// can validate alone. Today this is small (CORS-vs-API), but any
    /// future "you set X but Y is off" trap belongs here.
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
        check_range(
            "slow_query_window_minutes",
            &self.detection.slow_query_window_minutes,
            &0,
            &60,
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
        // A CV past 10 never fires and silently disables the signal, which is
        // what `sanitizer_aware_classification = "never"` is for.
        let cv = self.detection.sanitizer_aware_min_cv;
        if !cv.is_finite() || cv <= 0.0 || cv > 10.0 {
            return Err(format!(
                "sanitizer_aware_min_cv must be a finite number in (0, 10], got {cv}"
            ));
        }
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
        if let Some(ratio) = self.thresholds.min_usable_span_ratio
            && !(0.0..=1.0).contains(&ratio)
        {
            return Err(format!(
                "min_usable_span_ratio must be in [0.0, 1.0], got {ratio}"
            ));
        }
        Ok(())
    }
}
