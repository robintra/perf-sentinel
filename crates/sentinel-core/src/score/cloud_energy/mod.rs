//! Cloud-native energy estimation via CPU% + `SPECpower` interpolation.
//!
//! Provides an alternative energy estimation path for cloud VMs (AWS,
//! GCP, Azure) that do not expose Intel RAPL to guests, making the
//! Scaphandre per-process integration unusable. Instead, this module:
//!
//! 1. Scrapes CPU utilization from a Prometheus/VictoriaMetrics endpoint.
//! 2. Looks up the instance type's idle/max watts from an embedded
//!    `SPECpower` table (data sourced from Cloud Carbon Footprint).
//! 3. Interpolates: `watts = idle + (max - idle) * (cpu% / 100)`.
//! 4. Computes: `energy_kwh_per_op = (watts/1000) * (interval/3600) / ops`.
//!
//! The model tag is `"cloud_specpower"`. Precedence in the scoring
//! pipeline: `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v2` > `io_proxy_v1`.
//!
//! Module structure mirrors [`super::scaphandre`]:
//! - [`config`] — user-facing configuration types
//! - [`state`] — `ArcSwap`-backed shared snapshot
//! - [`table`] — embedded `SPECpower` instance power lookup
//! - [`scraper`] — Prometheus JSON API scraper and energy computation

pub mod config;
pub mod scraper;
pub mod state;
pub mod table;

pub use config::CloudEnergyConfig;
pub use scraper::spawn_cloud_scraper;
pub use state::CloudEnergyState;
