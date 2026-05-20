//! Shared Redfish state. Thin wrapper around `AgedEnergyMap` via the
//! `impl_energy_state!` macro, with `RedfishState` kept as a distinct
//! nominal type so the daemon cannot accidentally swap a Scaphandre /
//! Kepler / cloud state for a Redfish one.

pub(super) use crate::score::energy_state::EnergyRow as ServiceEnergy;

// Re-export the monotonic clock the same way `cloud_energy::state` does,
// so consumers can `use super::state::monotonic_ms` instead of reaching
// across to the Scaphandre module path.
pub(super) use crate::score::scaphandre::state::monotonic_ms;

crate::score::energy_state::impl_energy_state! {
    /// Runtime state shared between the Redfish scraper task and the
    /// scoring path.
    #[derive(Debug, Default)]
    pub struct RedfishState;
}
