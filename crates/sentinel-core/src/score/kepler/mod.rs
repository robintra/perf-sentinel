//! Kepler eBPF energy scraper (opt-in, daemon only).
//!
//! Scrapes Kepler's Prometheus `/metrics` endpoint for cumulative
//! per-container or per-process joule counters and provides measured
//! per-service energy-per-op coefficients to the scoring stage.
//!
//! Unlike Scaphandre (microwatt gauge), Kepler exports a monotonically
//! increasing joules counter, so the scraper holds the previous raw
//! value per service and computes a joules-delta each tick.
//!
//! See `docs/design/05-GREENOPS-AND-CARBON.md` for architecture details
//! and `docs/LIMITATIONS.md` for the precision-bounds discussion.

#[cfg(feature = "daemon")]
pub mod apply;
pub mod config;
pub mod parser;
#[cfg(feature = "daemon")]
pub mod scraper;
#[cfg(feature = "daemon")]
pub mod state;

pub use config::{KeplerConfig, KeplerMetricKind};
#[cfg(feature = "daemon")]
pub use scraper::spawn_scraper;
#[cfg(feature = "daemon")]
pub use state::KeplerState;

#[cfg(all(test, feature = "daemon"))]
mod tests;
