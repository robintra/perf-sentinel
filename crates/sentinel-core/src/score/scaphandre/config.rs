//! User-facing Scaphandre scraper configuration.

use std::collections::HashMap;
use std::time::Duration;

/// User-facing configuration for the Scaphandre scraper.
///
/// Parsed from `[green.scaphandre]` in `.perf-sentinel.toml`:
///
/// ```toml
/// [green.scaphandre]
/// endpoint = "http://localhost:8080/metrics"
/// scrape_interval_secs = 5
/// process_map = { "order-svc" = "java", "chat-svc" = "dotnet" }
/// ```
///
/// Absent config → no scraper spawned → all services fall back to the
/// proxy model. This struct is only constructed when the user sets at
/// least an `endpoint`.
#[derive(Clone)]
pub struct ScaphandreConfig {
    /// Full URL of the Prometheus-format metrics endpoint. No TLS
    /// support is implemented; the endpoint MUST be `http://...` on
    /// localhost or a trusted host on the same network segment.
    pub endpoint: String,
    /// How often to scrape. Default `5s`. Clamped to `[1, 3600]` at
    /// config load time.
    pub scrape_interval: Duration,
    /// Maps perf-sentinel service names (from span `service.name`) to
    /// Scaphandre process `exe` labels. A service with no entry here
    /// falls back to the proxy model regardless of whether the Scaphandre
    /// endpoint is reachable.
    pub process_map: HashMap<String, String>,
    /// Optional auth header in curl format (`"Name: Value"`) attached
    /// to every Scaphandre request. Required when the exporter sits
    /// behind a reverse proxy with basic auth or bearer-token enforcement.
    /// Stored as plain `String` (not `secrecy::SecretString`) to avoid
    /// adding a dependency. The manual `Debug` impl below redacts this
    /// field. Resolved via the `PERF_SENTINEL_SCAPHANDRE_AUTH_HEADER`
    /// environment variable with fallback to this field; env wins when
    /// both are set.
    pub auth_header: Option<String>,
}

// Manual Debug impl to redact the auth header (potentially a secret).
impl std::fmt::Debug for ScaphandreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScaphandreConfig")
            .field("endpoint", &self.endpoint)
            .field("scrape_interval", &self.scrape_interval)
            .field("process_map", &self.process_map)
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

    fn sample_config() -> ScaphandreConfig {
        let mut process_map = HashMap::new();
        process_map.insert("order-svc".to_string(), "java".to_string());
        ScaphandreConfig {
            endpoint: "http://localhost:8080/metrics".to_string(),
            scrape_interval: Duration::from_secs(5),
            process_map,
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
        assert!(dbg.contains("endpoint"));
        assert!(dbg.contains("http://localhost:8080/metrics"));
        assert!(dbg.contains("order-svc"));
        assert!(dbg.contains("java"));
    }
}
