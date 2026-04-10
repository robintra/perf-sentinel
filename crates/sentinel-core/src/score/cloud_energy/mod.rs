//! Cloud-native energy estimation via CPU% + `SPECpower` interpolation.
//!
//! See `docs/design/05-GREENOPS-AND-CARBON.md` for architecture details.

pub mod config;
#[cfg(feature = "daemon")]
pub mod scraper;
#[cfg(feature = "daemon")]
pub mod state;
pub mod table;

pub use config::CloudEnergyConfig;
#[cfg(feature = "daemon")]
pub use scraper::spawn_cloud_scraper;
#[cfg(feature = "daemon")]
pub use state::CloudEnergyState;
