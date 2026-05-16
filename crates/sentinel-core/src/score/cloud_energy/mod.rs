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

/// Vintage of the embedded `SPECpower` and CCF coefficients that drive
/// `cloud_specpower` energy attribution. Reports surface this string in
/// `methodology.calibration_inputs.binary_specpower_vintage` so consumers
/// can disambiguate it from the operator-supplied
/// `specpower_table_version` declared in the org config TOML.
#[must_use]
pub fn embedded_specpower_vintage() -> &'static str {
    table::SPECPOWER_VINTAGE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_vintage_matches_table_const() {
        // Guards against a maintainer hardcoding a vintage string in
        // disclose.rs or fixtures while forgetting to bump
        // `table::SPECPOWER_VINTAGE`, which is the canonical source for
        // the release procedure step 2.5 `grep VINTAGE` audit.
        assert_eq!(embedded_specpower_vintage(), table::SPECPOWER_VINTAGE);
    }
}
