//! Shared Alumet state. Thin wrapper around `AgedEnergyMap` via the
//! `impl_energy_state!` macro, with `AlumetState` kept as a distinct
//! nominal type so the daemon cannot accidentally swap a Scaphandre or
//! Kepler state for an Alumet one.

/// Row type used by [`super::apply::apply_scrape`] when constructing
/// fresh entries. Aliased to the shared [`EnergyRow`] so every energy
/// state has one definition.
///
/// [`EnergyRow`]: crate::score::energy_state::EnergyRow
pub(super) use crate::score::energy_state::EnergyRow as ServiceEnergy;

// Re-export the monotonic clock the same way `kepler::state` does, so
// consumers can `use super::state::monotonic_ms` instead of reaching
// across to the Scaphandre module path.
pub(super) use crate::score::scaphandre::state::monotonic_ms;

crate::score::energy_state::impl_energy_state! {
    /// Runtime state shared between the Alumet scraper task and the
    /// scoring path.
    #[derive(Debug, Default)]
    pub struct AlumetState;
}
