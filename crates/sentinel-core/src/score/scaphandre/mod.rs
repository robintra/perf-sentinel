//! Scaphandre per-process power scraper.
//!
//! Optional opt-in path for users running on Linux with Intel RAPL
//! support. When configured via `[green.scaphandre]` in
//! `.perf-sentinel.toml`, the daemon spawns a background task that
//! scrapes a user-provided Prometheus `/metrics` endpoint (typically
//! Scaphandre's own exporter) every `scrape_interval_secs` and caches
//! the latest per-service energy-per-op coefficient.
//!
//! The scoring stage then uses those measured coefficients instead of
//! the fixed [`crate::score::carbon::ENERGY_PER_IO_OP_KWH`] constant
//! when computing CO₂ for spans belonging to mapped services.
//!
//! # Module layout
//!
//! This module is split into five sub-modules, each with a single
//! responsibility:
//!
//! - [`config`] — `ScaphandreConfig` (the parsed TOML section).
//! - [`state`] — `ScaphandreState` (ArcSwap-backed shared snapshot of
//!   per-service energy coefficients) plus `monotonic_ms`.
//! - [`parser`] — `parse_scaphandre_metrics` and the Prometheus text
//!   exposition helpers (escape-aware `exe` label extraction).
//! - [`ops`] — `OpsSnapshotDiff` (zero-clone per-service op delta),
//!   `compute_energy_per_op_kwh` (pure formula), and `apply_scrape`
//!   (glue that publishes a new state from readings + deltas).
//! - [`scraper`] — the runtime entry points: `spawn_scraper`,
//!   `run_scraper_loop`, `fetch_metrics_once`, `ScraperError`, and
//!   the `redact_endpoint` helper used for logging.
//!
//! Integration tests covering the full path (config -> parse ->
//! ops -> state) live in a single [`tests`] module at this level
//! because they cross the sub-module boundaries.
//!
//! # Precision bounds
//!
//! Scaphandre improves the **per-service** energy coefficient; it
//! does NOT give per-finding attribution. Two findings in the same
//! process during the same scrape window share the same coefficient
//! by construction — RAPL is a process-level measurement, not a
//! span-level one. The 5-second default scrape interval is NOT the
//! precision bottleneck; the process-granularity is. See
//! `docs/LIMITATIONS.md` section "Scaphandre precision bounds".
//!
//! # Network egress
//!
//! The perf-sentinel "no network egress" rule in `CLAUDE.md` refers
//! to unsolicited outbound connections (e.g. fetching the carbon
//! table on the fly). An explicitly-user-configured local endpoint
//! URL is acceptable — the user is opting in.

pub mod config;
pub mod ops;
pub mod parser;
pub mod scraper;
pub mod state;

// Public re-exports: the three types daemon/config code touches, and
// the one entry point the daemon calls at startup. Everything else
// (parser internals, ops helpers, scraper internals, ScraperError)
// stays visible at `pub(crate)` or `pub(super)` for the integration
// tests and the sub-modules that need it.
pub use config::ScaphandreConfig;
pub use scraper::spawn_scraper;
pub use state::{ScaphandreState, monotonic_ms};

// The sub-modules keep their internal types at `pub(super)` so the
// crate-level API surface stays as narrow as the pre-split version.
// Test-only integration code at [`tests`] reaches them via
// `super::scraper::*`, `super::ops::*`, etc. — no re-export needed
// since `tests` is a sibling of the sub-modules, one level down from
// `scaphandre`.

#[cfg(test)]
mod tests;
