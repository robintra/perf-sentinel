//! Electricity Maps API integration for real-time carbon intensity.
//!
//! Polls the Electricity Maps API for current grid carbon intensity
//! (gCO2eq/kWh) per zone, providing higher-fidelity intensity values
//! than the embedded static tables.

pub mod config;
#[cfg(feature = "daemon")]
pub mod scraper;
#[cfg(feature = "daemon")]
pub mod state;

pub use config::{ElectricityMapsConfig, EmissionFactorType, TemporalGranularity};
#[cfg(feature = "daemon")]
pub use scraper::spawn_electricity_maps_scraper;
#[cfg(feature = "daemon")]
pub use state::ElectricityMapsState;
