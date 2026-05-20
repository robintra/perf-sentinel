//! Redfish BMC wall-plug-power scraper (opt-in, daemon only).
//!
//! Polls one or more BMC chassis via the Redfish `/Power` resource for
//! `PowerConsumedWatts`, converts the node-level reading into a
//! per-service energy-per-op coefficient using the shared
//! [`ops_snapshot_diff`] tracker (every service mapped to the chassis
//! receives the same coefficient), and publishes the result to
//! [`RedfishState`].
//!
//! Node-level granularity: two services on the same chassis share a
//! single coefficient. See `docs/LIMITATIONS.md` "Redfish BMC precision
//! bounds" for the methodology trade-off.
//!
//! [`ops_snapshot_diff`]: crate::score::ops_snapshot_diff

#[cfg(feature = "daemon")]
pub mod apply;
pub mod config;
pub mod parser;
#[cfg(feature = "daemon")]
pub mod scraper;
#[cfg(feature = "daemon")]
pub mod state;

pub use config::RedfishConfig;
#[cfg(feature = "daemon")]
pub use scraper::spawn_scraper;
#[cfg(feature = "daemon")]
pub use state::RedfishState;

#[cfg(all(test, feature = "daemon"))]
mod tests;
