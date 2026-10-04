//! Validation of the `[green]` section and its energy-backend subsections.

use std::collections::HashMap;

use crate::score::alumet::AlumetConfig;
use crate::score::cloud_energy::config::{CloudEnergyConfig, ServiceCloudConfig};
use crate::score::kepler::{KeplerConfig, KeplerMetricKind};
use crate::score::redfish::{RedfishConfig, RedfishEndpoint};
use crate::score::scaphandre::ScaphandreConfig;

use super::{
    Config, check_range, has_control_char, validate_http_authority, validate_workload_fields,
    warn_unknown_region,
};

/// Validate `[green.broker_static]`: a declared cluster must be
/// countable and its instance type nameable.
pub(super) fn validate_broker_static(
    cfg: &crate::score::broker_static::StaticBrokerConfig,
) -> Result<(), String> {
    if cfg.nodes == 0 {
        return Err("[green.broker_static] nodes must be at least 1".to_string());
    }
    if cfg.nodes > 10_000 {
        return Err(format!(
            "[green.broker_static] nodes = {} is implausible, cap is 10000",
            cfg.nodes
        ));
    }
    if cfg.instance_type.is_empty()
        || cfg.instance_type.len() > 256
        || has_control_char(&cfg.instance_type)
    {
        return Err(
            "[green.broker_static] instance_type must be 1-256 chars and free of \
             control characters"
                .to_string(),
        );
    }
    // Same allow-list as [green.cloud.services]: an unrecognised provider
    // would silently resolve to the generic on-prem watts, a very
    // different number from the AWS default, under a typo.
    if !matches!(
        cfg.provider.as_str(),
        "aws" | "gcp" | "azure" | "scaleway" | "generic"
    ) {
        return Err(format!(
            "[green.broker_static] provider must be 'aws', 'gcp', 'azure', \
             'scaleway' or 'generic', got '{}'",
            cfg.provider
        ));
    }
    if let Some(region) = cfg.region.as_deref()
        && (region.is_empty() || !crate::score::carbon::is_valid_region_id(region))
    {
        return Err(format!(
            "[green.broker_static] region '{region}' is not a valid region id"
        ));
    }
    warn_unknown_region("[green.broker_static]", cfg.region.as_deref());
    // An unknown type still yields a provider default, so warn rather
    // than reject: the figure stays sound, just coarser.
    if !crate::score::cloud_energy::table::is_known_instance_type(&cfg.instance_type) {
        tracing::warn!(
            instance_type = %cfg.instance_type,
            "[green.broker_static] instance_type is absent from the embedded \
             SPECpower table, falling back to the provider default watts"
        );
    }
    Ok(())
}

impl Config {
    pub(super) fn validate_green(&self) -> Result<(), String> {
        Self::validate_embodied_carbon(self.green.embodied_carbon_per_request_gco2)?;
        Self::validate_default_region(self.green.default_region.as_deref())?;
        Self::validate_service_regions(&self.green.service_regions)?;
        if let Some(cfg) = &self.green.scaphandre {
            Self::validate_scaphandre(cfg)?;
        }
        if let Some(cfg) = &self.green.kepler {
            Self::validate_kepler(cfg)?;
        }
        if let Some(cfg) = &self.green.alumet {
            Self::validate_alumet(cfg)?;
        }
        if let Some(cfg) = &self.green.redfish {
            Self::validate_redfish(cfg)?;
        }
        if let Some(cfg) = &self.green.cloud_energy {
            Self::validate_cloud_energy(cfg)?;
        }
        if let Some(cfg) = &self.green.broker_static {
            validate_broker_static(cfg)?;
        }
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

    /// Validate `[green] hourly_profiles_file`: reject control characters
    /// in the path (log injection) and require that the file loaded when
    /// the field is configured.
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
    pub(in crate::config) fn validate_electricity_maps(
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
        // over https in practice. An http:// endpoint usually means a local
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
    /// a blank `zone`, and `service_mappings` keys/values outside
    /// [1, 256] chars or with control chars.
    pub(in crate::config) fn validate_kepler(cfg: &KeplerConfig) -> Result<(), String> {
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
        Self::validate_kepler_zone(&cfg.zone)?;
        Self::validate_kepler_service_mappings(cfg)?;
        #[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
        if let Some(auth) = cfg.auth_header.as_deref() {
            crate::ingest::auth_header::AuthHeader::parse(auth)
                .map_err(|msg| format!("[green.kepler] auth_header: {msg}"))?;
        }
        Ok(())
    }

    /// The zone is matched verbatim against the `zone` label. No fixed
    /// set applies: hwmon zones are named after the host's sensors.
    fn validate_kepler_zone(zone: &str) -> Result<(), String> {
        if has_control_char(zone) {
            return Err("[green.kepler] zone contains control characters".to_string());
        }
        if zone.trim().is_empty() {
            return Err(format!(
                "[green.kepler] zone '{zone}' is blank; remove the field for the default 'package'"
            ));
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

    /// Validate a parsed `[green.alumet]` config section.
    ///
    /// Beyond the shared endpoint and interval checks, this guards the
    /// two operator-supplied parser inputs (`metric_name`, `label_key`)
    /// and `energy_interval_secs`, the value that silently rescales
    /// every reading when it drifts from the Alumet-side
    /// `poll_interval`. Neither string reaches a Prometheus label (they
    /// are matched against the scraped body), so there is no cardinality
    /// exposure here.
    pub(super) fn validate_alumet(cfg: &AlumetConfig) -> Result<(), String> {
        if cfg.endpoint.is_empty() {
            return Err(
                "[green.alumet] endpoint is required when the section is present".to_string(),
            );
        }
        if !cfg.endpoint.starts_with("http://") && !cfg.endpoint.starts_with("https://") {
            return Err(format!(
                "[green.alumet] endpoint '{}' must start with 'http://' or 'https://'",
                cfg.endpoint
            ));
        }
        validate_http_authority(&cfg.endpoint, "[green.alumet] endpoint")?;
        let secs = cfg.scrape_interval.as_secs();
        if !(1..=3600).contains(&secs) {
            return Err(format!(
                "[green.alumet] scrape_interval_secs must be in [1, 3600], got {secs}"
            ));
        }
        Self::validate_alumet_parser_field(&cfg.metric_name, "metric_name")?;
        Self::validate_alumet_parser_field(&cfg.label_key, "label_key")?;
        let interval = cfg.energy_interval_secs;
        if !interval.is_finite() || interval <= 0.0 || interval > 3600.0 {
            return Err(format!(
                "[green.alumet] energy_interval_secs must be a finite value in (0, 3600], \
                 got {interval}"
            ));
        }
        Self::validate_alumet_service_mappings(cfg)?;
        if let Some(db) = &cfg.database {
            Self::validate_workload_declaration(
                "[green.alumet.database]",
                "database waste",
                &db.label_value,
                db.region.as_deref(),
                cfg,
            )?;
        }
        if let Some(broker) = &cfg.broker {
            Self::validate_workload_declaration(
                "[green.alumet.broker]",
                "messaging waste",
                &broker.label_value,
                broker.region.as_deref(),
                cfg,
            )?;
            // One cgroup cannot be both workloads either: that would
            // count the same joules in two separate figures.
            if cfg
                .database
                .as_ref()
                .is_some_and(|db| db.label_value == broker.label_value)
            {
                return Err(format!(
                    "[green.alumet.broker] label_value '{}' is also declared as the \
                     database workload; one cgroup cannot feed both waste figures",
                    broker.label_value
                ));
            }
        }
        #[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
        if let Some(auth) = cfg.auth_header.as_deref() {
            crate::ingest::auth_header::AuthHeader::parse(auth)
                .map_err(|msg| format!("[green.alumet] auth_header: {msg}"))?;
        }
        Ok(())
    }

    /// Shared checks of one declared workload (database or broker):
    /// field validity, no collision with `service_mappings`, and a warn
    /// on a region absent from the embedded table. Charset-valid but
    /// unknown regions are legitimate (custom ids covered by Electricity
    /// Maps), so warn instead of rejecting.
    fn validate_workload_declaration(
        section: &str,
        figure: &str,
        label_value: &str,
        region: Option<&str>,
        cfg: &AlumetConfig,
    ) -> Result<(), String> {
        validate_workload_fields(section, label_value, region)?;
        if cfg.service_mappings.values().any(|v| v == label_value) {
            return Err(format!(
                "{section} label_value '{label_value}' also appears in \
                 service_mappings; one cgroup cannot feed both the energy \
                 totals and the {figure} figure"
            ));
        }
        warn_unknown_region(section, region);
        Ok(())
    }

    /// Bound one of the two operator-supplied parser inputs. Control
    /// chars are rejected before the value reaches an error message.
    fn validate_alumet_parser_field(value: &str, field: &str) -> Result<(), String> {
        /// Prometheus metric and label names are far shorter than this
        /// in practice. The cap only bounds the memory a hostile config
        /// can pin per scrape.
        const MAX_PARSER_FIELD_LEN: usize = 256;
        if has_control_char(value) {
            return Err(format!(
                "[green.alumet] {field} contains control characters"
            ));
        }
        if value.is_empty() || value.len() > MAX_PARSER_FIELD_LEN {
            return Err(format!(
                "[green.alumet] {field} must be 1-{MAX_PARSER_FIELD_LEN} chars, got '{value}'"
            ));
        }
        Ok(())
    }

    /// Validate `[green.alumet].service_mappings` keys and values.
    /// Label values are Alumet label values (a pod name, a cgroup id, a
    /// RAPL domain), all well under the shared 256-byte cap.
    fn validate_alumet_service_mappings(cfg: &AlumetConfig) -> Result<(), String> {
        /// Memory-footprint cap, mirrors `MAX_KEPLER_SERVICE_MAPPINGS`.
        const MAX_ALUMET_SERVICE_MAPPINGS: usize = 1024;
        if cfg.service_mappings.len() > MAX_ALUMET_SERVICE_MAPPINGS {
            return Err(format!(
                "[green.alumet] service_mappings has {} entries; maximum is {MAX_ALUMET_SERVICE_MAPPINGS}",
                cfg.service_mappings.len()
            ));
        }
        for (service, label) in &cfg.service_mappings {
            // Reject control chars first so an ANSI-laden label is not
            // echoed back to stderr via the length-error `format!`.
            if has_control_char(service) {
                return Err("[green.alumet] service_mappings has a service name \
                     that contains control characters"
                    .to_string());
            }
            if has_control_char(label) {
                return Err(format!(
                    "[green.alumet] service_mappings has a label \
                     for service '{service}' that contains control characters"
                ));
            }
            if service.is_empty() || service.len() > 256 {
                return Err(format!(
                    "[green.alumet] service_mappings service name '{service}' must be 1-256 chars"
                ));
            }
            if label.is_empty() || label.len() > 256 {
                return Err(format!(
                    "[green.alumet] service_mappings label for service '{service}' \
                     must be 1-256 chars, got '{label}'"
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
    pub(in crate::config) fn validate_redfish(cfg: &RedfishConfig) -> Result<(), String> {
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
    /// of control characters. Service names are NOT run through
    /// `is_valid_region_id` because they may legitimately contain dots,
    /// slashes and similar.
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
            && !matches!(p.as_str(), "aws" | "gcp" | "azure" | "scaleway")
        {
            return Err(format!(
                "[green.cloud] default_provider must be 'aws', 'gcp', 'azure' or \
                 'scaleway', got '{p}'"
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
            && !matches!(p, "aws" | "gcp" | "azure" | "scaleway")
        {
            return Err(format!(
                "[green.cloud.services.{service}] provider must be 'aws', 'gcp', \
                 'azure' or 'scaleway', got '{p}'"
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
}
