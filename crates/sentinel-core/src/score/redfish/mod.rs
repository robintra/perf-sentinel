//! Redfish BMC wall-plug-power scraper (opt-in, daemon only).
//!
//! Polls one or more BMC chassis via the Redfish power resource and
//! converts the node-level reading into a per-service energy-per-op
//! coefficient using the shared [`ops_snapshot_diff`] tracker (every
//! service mapped to the chassis receives the same coefficient).
//! Publishes the result to [`RedfishState`].
//!
//! Two endpoint schemas are supported, declared per-endpoint via the
//! [`RedfishSchema`] enum: the legacy `/Power` resource with
//! `PowerControl[0].PowerConsumedWatts` (still mandatory on BMC
//! firmware as of 2026), and the modern `EnvironmentMetrics` resource
//! with `PowerWatts.Reading` (DMTF Release 2020.4+).
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

pub use config::{RedfishConfig, RedfishEndpoint, RedfishSchema};
#[cfg(feature = "daemon")]
pub use scraper::spawn_scraper;
#[cfg(feature = "daemon")]
pub use state::RedfishState;

#[cfg(all(test, feature = "daemon"))]
mod tests;
