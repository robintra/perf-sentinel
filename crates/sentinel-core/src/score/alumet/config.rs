//! User-facing Alumet scraper configuration.

use std::collections::HashMap;
use std::time::Duration;

/// Default `energy_interval_secs`, mirroring the upstream default
/// `poll_interval` of Alumet's `rapl` source (1 Hz).
pub const DEFAULT_ENERGY_INTERVAL_SECS: f64 = 1.0;

/// User-facing configuration for the Alumet scraper.
///
/// Parsed from `[green.alumet]` in `.perf-sentinel.toml`:
///
/// ```toml
/// [green.alumet]
/// endpoint = "http://localhost:9091/metrics"
/// scrape_interval_secs = 5
/// metric_name = "attributed_energy_cpu_alumet"
/// label_key = "name"
/// energy_interval_secs = 1.0
/// service_mappings = { "checkout" = "checkout-pod" }
/// ```
///
/// Unlike Kepler, whose metric name and label key are pinned by an enum,
/// both are operator-supplied here: Alumet's `prometheus-exporter`
/// applies a configurable `prefix`/`suffix` to every metric name
/// (default suffix `_alumet`), and the per-service series comes from an
/// `energy-attribution` formula whose name the operator chooses. There is
/// no default that is correct for every deployment, so both fields are
/// required, see `docs/CONFIGURATION.md`.
///
/// Absent config means no scraper spawned, every service falls back to
/// the proxy or cloud path. This struct is only constructed when the
/// user sets at least an `endpoint`.
#[derive(Clone)]
pub struct AlumetConfig {
    /// Full URL of the Alumet `prometheus-exporter` `/metrics` endpoint.
    /// Must start with `http://` or `https://`. Upstream default port
    /// is 9091.
    pub endpoint: String,
    /// How often to scrape. Default `5s`. Clamped to `[1, 3600]` at
    /// config load time.
    pub scrape_interval: Duration,
    /// Prometheus metric name to read, exactly as it appears on the
    /// wire (including the exporter's `prefix`/`suffix`).
    pub metric_name: String,
    /// Prometheus label key used to route a sample to a service. `name`
    /// for the `k8s` source (pod name), `resource_consumer_id` for the
    /// `procfs` source (a PID, rarely useful), `domain` for a raw
    /// per-RAPL-domain series.
    pub label_key: String,
    /// Wall-clock seconds the scraped joules value covers, i.e. the
    /// `poll_interval` of the Alumet source feeding the metric.
    ///
    /// Alumet's exporter publishes every measurement as a Prometheus
    /// gauge holding the value of the last flush, and `rapl_consumed_energy`
    /// is a `CounterDiff`: the joules burned during one `poll_interval`,
    /// not a cumulative counter and not a power reading. The interval is
    /// nowhere on the wire, so it has to be declared here and **must**
    /// match the Alumet-side config. A mismatch scales energy and carbon
    /// linearly and silently, see `docs/LIMITATIONS.md#alumet-precision-bounds`.
    pub energy_interval_secs: f64,
    /// Maps perf-sentinel service names (from span `service.name`) to
    /// the Alumet label value identifying the same workload. A service
    /// with no entry here falls back through the precedence chain
    /// regardless of whether the Alumet endpoint is reachable.
    pub service_mappings: HashMap<String, String>,
    /// Optional auth header in curl format (`"Name: Value"`) attached to
    /// every Alumet request. Resolved via the
    /// `PERF_SENTINEL_ALUMET_AUTH_HEADER` environment variable with
    /// fallback to this field, env wins when both are set.
    pub auth_header: Option<String>,
}

// Manual Debug impl to redact the auth header (potentially a secret).
impl std::fmt::Debug for AlumetConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlumetConfig")
            .field("endpoint", &self.endpoint)
            .field("scrape_interval", &self.scrape_interval)
            .field("metric_name", &self.metric_name)
            .field("label_key", &self.label_key)
            .field("energy_interval_secs", &self.energy_interval_secs)
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

    fn sample_config() -> AlumetConfig {
        let mut mappings = HashMap::new();
        mappings.insert("checkout".to_string(), "checkout-pod".to_string());
        AlumetConfig {
            endpoint: "http://localhost:9091/metrics".to_string(),
            scrape_interval: Duration::from_secs(5),
            metric_name: "attributed_energy_cpu_alumet".to_string(),
            label_key: "name".to_string(),
            energy_interval_secs: DEFAULT_ENERGY_INTERVAL_SECS,
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
        assert!(dbg.contains("http://localhost:9091/metrics"));
        assert!(dbg.contains("attributed_energy_cpu_alumet"));
        assert!(dbg.contains("checkout-pod"));
    }

    #[test]
    fn default_energy_interval_matches_alumet_rapl_poll_default() {
        // Upstream `plugins/rapl/src/lib.rs` defaults poll_interval to
        // 1s. Drifting from it silently rescales every reading.
        assert!((DEFAULT_ENERGY_INTERVAL_SECS - 1.0).abs() < f64::EPSILON);
    }
}
