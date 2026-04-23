//! User-facing configuration for cloud-native energy estimation.

use std::collections::HashMap;
use std::time::Duration;

/// Per-service cloud energy configuration.
///
/// A service can be configured either by instance type (looked up in
/// the embedded `SPECpower` table) or by explicit idle/max watts
/// (for on-premise hardware not in the table).
#[derive(Debug, Clone)]
pub enum ServiceCloudConfig {
    /// Cloud provider instance with CPU% scraped from Prometheus.
    InstanceType {
        /// Cloud provider override for this service. When `None`, the
        /// top-level `default_provider` is used for table lookup fallback.
        provider: Option<String>,
        /// Instance type string (e.g. `"c5.4xlarge"`). Looked up in the
        /// embedded `SPECpower` table.
        instance_type: String,
        /// Optional custom `PromQL` query for this service's CPU%.
        /// When `None`, the top-level `cpu_metric` is used.
        cpu_query: Option<String>,
    },
    /// Manual watts specification (on-premise or custom hardware).
    ManualWatts {
        /// Power draw at near-zero CPU load (watts).
        idle_watts: f64,
        /// Power draw at 100% CPU utilization (watts).
        max_watts: f64,
        /// Optional custom `PromQL` query for this service's CPU%.
        cpu_query: Option<String>,
    },
}

/// Configuration for the cloud-native energy estimation subsystem.
///
/// Parsed from `[green.cloud]` in `.perf-sentinel.toml`. The subsystem
/// is only active when `prometheus_endpoint` is set.
#[derive(Clone)]
pub struct CloudEnergyConfig {
    /// Prometheus/VictoriaMetrics HTTP API endpoint
    /// (e.g. `"http://prometheus:9090"`). Must start with `http://`.
    pub prometheus_endpoint: String,
    /// How often to scrape CPU metrics. Default 15 s, clamped to
    /// `[1, 3600]` at config load.
    pub scrape_interval: Duration,
    /// Default cloud provider for services that don't specify one.
    /// Used as the `SPECpower` table fallback key. One of `"aws"`,
    /// `"gcp"`, `"azure"` or `None` for generic.
    pub default_provider: Option<String>,
    /// Default instance type for services that specify neither
    /// `instance_type` nor manual watts.
    pub default_instance_type: Option<String>,
    /// Default `PromQL` metric name / query template for CPU%.
    /// Used when a service has no `cpu_query` override.
    pub cpu_metric: Option<String>,
    /// Per-service configuration mapping service name to either an
    /// instance type lookup or manual watts override.
    pub services: HashMap<String, ServiceCloudConfig>,
    /// Optional auth header in curl format (`"Name: Value"`) attached
    /// to every Prometheus request. Required for Grafana Cloud, Grafana
    /// Mimir or any ingress that enforces bearer/basic auth. Stored as
    /// plain `String` (not `secrecy::SecretString`) to avoid adding a
    /// dependency. The manual `Debug` impl below redacts this field so
    /// `tracing::debug!(?config)` never leaks the credential. Resolved
    /// via the `PERF_SENTINEL_CLOUD_AUTH_HEADER` environment variable
    /// with fallback to this field; env wins when both are set.
    pub auth_header: Option<String>,
}

// Manual Debug impl to redact the auth header (potentially a secret).
impl std::fmt::Debug for CloudEnergyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudEnergyConfig")
            .field("prometheus_endpoint", &self.prometheus_endpoint)
            .field("scrape_interval", &self.scrape_interval)
            .field("default_provider", &self.default_provider)
            .field("default_instance_type", &self.default_instance_type)
            .field("cpu_metric", &self.cpu_metric)
            .field("services", &self.services)
            .field(
                "auth_header",
                &self.auth_header.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl ServiceCloudConfig {
    /// Returns the custom `PromQL` query, if any.
    #[must_use]
    pub fn cpu_query(&self) -> Option<&str> {
        match self {
            Self::InstanceType { cpu_query, .. } | Self::ManualWatts { cpu_query, .. } => {
                cpu_query.as_deref()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> CloudEnergyConfig {
        CloudEnergyConfig {
            prometheus_endpoint: "http://prometheus:9090".to_string(),
            scrape_interval: Duration::from_secs(15),
            default_provider: Some("aws".to_string()),
            default_instance_type: Some("c5.xlarge".to_string()),
            cpu_metric: None,
            services: HashMap::new(),
            auth_header: Some("Authorization: Bearer super-secret-do-not-log".to_string()),
        }
    }

    #[test]
    fn debug_impl_redacts_auth_header() {
        // Regression guard against `#[derive(Debug)]` being
        // reintroduced on the struct, which would print the credential.
        let cfg = sample_config();
        crate::test_helpers::assert_debug_redacts_secret!(&cfg, "super-secret-do-not-log");
    }

    #[test]
    fn debug_impl_preserves_non_secret_fields() {
        let cfg = sample_config();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("prometheus_endpoint"));
        assert!(dbg.contains("http://prometheus:9090"));
        assert!(dbg.contains("c5.xlarge"));
        assert!(dbg.contains("aws"));
    }
}
