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
#[derive(Debug, Clone)]
pub struct CloudEnergyConfig {
    /// Prometheus/VictoriaMetrics HTTP API endpoint
    /// (e.g. `"http://prometheus:9090"`). Must start with `http://`.
    pub prometheus_endpoint: String,
    /// How often to scrape CPU metrics. Default 15 s, clamped to
    /// `[1, 3600]` at config load.
    pub scrape_interval: Duration,
    /// Default cloud provider for services that don't specify one.
    /// Used as the `SPECpower` table fallback key. One of `"aws"`,
    /// `"gcp"`, `"azure"`, or `None` for generic.
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
