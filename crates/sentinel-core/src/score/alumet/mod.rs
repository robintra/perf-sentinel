//! Alumet energy scraper (opt-in, daemon only).
//!
//! Scrapes the `/metrics` endpoint of Alumet's `prometheus-exporter`
//! plugin and provides measured per-service energy-per-op coefficients
//! to the scoring stage.
//!
//! Alumet's reading is a third shape, distinct from both existing
//! Prometheus backends: Scaphandre exports an instantaneous microwatt
//! gauge, Kepler a cumulative joules counter, while Alumet exports the
//! joules consumed during one source `poll_interval`, published as a
//! gauge holding the last flushed value. The scraper therefore divides
//! by the operator-declared `energy_interval_secs` to recover watts,
//! and keeps no counter state across ticks.
//!
//! Ranks above Scaphandre in the measured-energy precedence chain.
//!
//! See `docs/design/05-GREENOPS-AND-CARBON.md` for architecture details
//! and `docs/LIMITATIONS.md#alumet-precision-bounds` for the precision
//! bounds and the interval-mismatch failure mode.

#[cfg(feature = "daemon")]
pub mod apply;
pub mod config;
#[cfg(feature = "daemon")]
pub mod scraper;
#[cfg(feature = "daemon")]
pub mod state;

pub use config::AlumetConfig;
#[cfg(feature = "daemon")]
pub use scraper::spawn_scraper;
#[cfg(feature = "daemon")]
pub use state::AlumetState;

#[cfg(all(test, feature = "daemon"))]
mod tests;
