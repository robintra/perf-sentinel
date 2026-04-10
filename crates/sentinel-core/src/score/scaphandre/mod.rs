//! Scaphandre per-process RAPL power scraper (opt-in, daemon only).
//!
//! Scrapes a Prometheus `/metrics` endpoint and provides measured
//! per-service energy-per-op coefficients to the scoring stage.
//! See `docs/design/05-GREENOPS-AND-CARBON.md` for architecture details.

pub mod config;
#[cfg(feature = "daemon")]
pub mod ops;
pub mod parser;
#[cfg(feature = "daemon")]
pub mod scraper;
#[cfg(feature = "daemon")]
pub mod state;

// Public re-exports for daemon and config code.
pub use config::ScaphandreConfig;
#[cfg(feature = "daemon")]
pub use scraper::spawn_scraper;
#[cfg(feature = "daemon")]
pub use state::{ScaphandreState, monotonic_ms};

#[cfg(all(test, feature = "daemon"))]
mod tests;
