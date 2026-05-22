//! User-facing Kepler scraper configuration.

use std::collections::HashMap;
use std::time::Duration;

/// Which Kepler metric the scraper reads. Each variant targets a
/// different metric name and Prometheus label key in the upstream
/// Kepler exposition (Kepler v2 series, Kepler >= 0.10). The default
/// (`Container`) is the most natural fit for Kubernetes deployments
/// where Kepler runs as a `DaemonSet` and exports a
/// `kepler_container_cpu_joules_total` series per pod.
///
/// `Process` reads the per-process CPU joules counter
/// (`kepler_process_cpu_joules_total`) keyed by the `comm` label. The
/// Linux kernel truncates `comm` to 15 bytes (`TASK_COMM_LEN - 1`),
/// so `service_mappings` label values longer than 15 chars are
/// rejected at config-load time.
///
/// Kepler v1 / pre-0.10 deployments expose `kepler_container_joules_total`
/// and have no `_cpu_` infix, perf-sentinel will scrape successfully
/// against them (HTTP 200) but find zero matching samples. Upgrade
/// the cluster's Kepler to v0.10+ before enabling this section.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeplerMetricKind {
    /// `kepler_container_cpu_joules_total` keyed by `container_name`.
    #[default]
    Container,
    /// `kepler_process_cpu_joules_total` keyed by `comm`.
    Process,
}

impl KeplerMetricKind {
    /// Prometheus metric name corresponding to the variant.
    #[must_use]
    pub const fn metric_name(self) -> &'static str {
        match self {
            Self::Container => "kepler_container_cpu_joules_total",
            Self::Process => "kepler_process_cpu_joules_total",
        }
    }

    /// Prometheus label key used to route a sample to a service.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Container => "container_name",
            Self::Process => "comm",
        }
    }
}

/// User-facing configuration for the Kepler scraper.
///
/// Parsed from `[green.kepler]` in `.perf-sentinel.toml`:
///
/// ```toml
/// [green.kepler]
/// endpoint = "http://kepler.kube-system.svc.cluster.local:9102/metrics"
/// scrape_interval_secs = 5
/// metric_kind = "container"
/// service_mappings = { "order-svc" = "order-svc-deployment", "chat-svc" = "chat" }
/// ```
///
/// Absent config means no scraper spawned, every service falls back to
/// the proxy or cloud path. This struct is only constructed when the
/// user sets at least an `endpoint`.
#[derive(Clone)]
pub struct KeplerConfig {
    /// Full URL of the Kepler Prometheus `/metrics` endpoint.
    /// Must start with `http://` or `https://`.
    pub endpoint: String,
    /// How often to scrape. Default `5s`. Clamped to `[1, 3600]` at
    /// config load time.
    pub scrape_interval: Duration,
    /// Which Kepler metric to read.
    pub metric_kind: KeplerMetricKind,
    /// Maps perf-sentinel service names (from span `service.name`) to
    /// the Kepler label value identifying the same workload (container
    /// name for `Container`, process command name for `Process`). A
    /// service with no entry here falls back through the precedence
    /// chain regardless of whether the Kepler endpoint is reachable.
    pub service_mappings: HashMap<String, String>,
    /// Optional auth header in curl format (`"Name: Value"`) attached
    /// to every Kepler request. Required when the exporter sits behind
    /// a reverse proxy with basic auth or bearer-token enforcement.
    /// Resolved via the `PERF_SENTINEL_KEPLER_AUTH_HEADER` environment
    /// variable with fallback to this field, env wins when both are set.
    pub auth_header: Option<String>,
}

// Manual Debug impl to redact the auth header (potentially a secret).
impl std::fmt::Debug for KeplerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeplerConfig")
            .field("endpoint", &self.endpoint)
            .field("scrape_interval", &self.scrape_interval)
            .field("metric_kind", &self.metric_kind)
            .field("service_mappings", &self.service_mappings)
            .field(
                "auth_header",
                &self.auth_header.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> KeplerConfig {
        let mut mappings = HashMap::new();
        mappings.insert("order-svc".to_string(), "order-svc-deployment".to_string());
        KeplerConfig {
            endpoint: "http://kepler:9102/metrics".to_string(),
            scrape_interval: Duration::from_secs(5),
            metric_kind: KeplerMetricKind::Container,
            service_mappings: mappings,
            auth_header: Some("Authorization: Bearer super-secret-do-not-log".to_string()),
        }
    }

    #[test]
    fn debug_impl_redacts_auth_header() {
        let cfg = sample_config();
        crate::test_helpers::assert_debug_redacts_secret!(&cfg, "super-secret-do-not-log");
    }

    #[test]
    fn debug_impl_preserves_non_secret_fields() {
        let cfg = sample_config();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("endpoint"));
        assert!(dbg.contains("http://kepler:9102/metrics"));
        assert!(dbg.contains("order-svc"));
        assert!(dbg.contains("order-svc-deployment"));
    }

    #[test]
    fn metric_kind_names_and_labels() {
        assert_eq!(
            KeplerMetricKind::Container.metric_name(),
            "kepler_container_cpu_joules_total"
        );
        assert_eq!(KeplerMetricKind::Container.label_key(), "container_name");
        assert_eq!(
            KeplerMetricKind::Process.metric_name(),
            "kepler_process_cpu_joules_total"
        );
        assert_eq!(KeplerMetricKind::Process.label_key(), "comm");
    }
}
