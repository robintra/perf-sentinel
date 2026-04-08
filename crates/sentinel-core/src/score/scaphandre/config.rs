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
#[derive(Debug, Clone)]
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
}
