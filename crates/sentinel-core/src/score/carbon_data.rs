// GENERATED FILE - DO NOT EDIT BY HAND.
// Regenerate with: python3 scripts/refresh-carbon-data.py
//
// Carbon intensity rows (region_key, gCO2eq/kWh, provider) for regions
// on effectively national grids. Keys are lowercase. Subnational North
// American regions live in `MANUAL_CARBON_ROWS` (carbon.rs).
//
// Current values: CCF and Electricity Maps 2023-2024 annual averages,
// consumption-based with imports. The first scripted refresh switches
// to Ember yearly data (CC-BY-4.0, generation-based, national). See
// docs/METHODOLOGY.md.
//
// Vintage notes: eu-west-3/fr tracks the EM consumption mean (2023=49,
// 2024=33). eu-central-1/de keeps the multi-source 338, matched by its
// hourly profile. br carries the BR-CS zone value, not a national one.

use super::carbon::Provider;

/// Grep-audited by release procedure step 2.5, like `PUE_VINTAGE`.
/// Stamped `ember-<latest-data-year>` by the refresh script.
#[allow(dead_code)]
pub(crate) const CARBON_TABLE_VINTAGE: &str = "em-ccf-2023-2024";

pub(super) static GENERATED_CARBON_ROWS: &[(&str, f64, Provider)] = &[
    // AWS regions
    ("eu-west-1", 296.0, Provider::Aws),      // Ireland
    ("eu-west-2", 231.0, Provider::Aws),      // London
    ("eu-west-3", 41.0, Provider::Aws),       // Paris
    ("eu-central-1", 338.0, Provider::Aws),   // Frankfurt
    ("eu-north-1", 8.0, Provider::Aws),       // Stockholm
    ("ap-northeast-1", 462.0, Provider::Aws), // Tokyo
    ("ap-southeast-1", 408.0, Provider::Aws), // Singapore
    ("eu-west-4", 328.0, Provider::Aws),      // Netherlands (canonical hourly key)
    ("eu-south-1", 370.0, Provider::Aws),     // Milan (Italy)
    ("ap-southeast-2", 550.0, Provider::Aws), // Sydney
    ("ap-south-1", 708.0, Provider::Aws),     // Mumbai
    ("sa-east-1", 96.0, Provider::Aws),       // São Paulo
    // GCP regions
    ("europe-west1", 165.0, Provider::Gcp),      // Belgium
    ("europe-west4", 328.0, Provider::Gcp),      // Netherlands
    ("europe-west9", 41.0, Provider::Gcp),       // Paris
    ("europe-north1", 8.0, Provider::Gcp),       // Finland
    ("europe-west8", 370.0, Provider::Gcp),      // Milan (Italy)
    ("europe-southwest1", 200.0, Provider::Gcp), // Madrid (Spain)
    ("europe-central2", 700.0, Provider::Gcp),   // Warsaw (Poland)
    ("europe-north2", 7.0, Provider::Gcp),       // Oslo-ish (Norway)
    ("asia-northeast1", 462.0, Provider::Gcp),   // Tokyo
    // Azure regions
    ("westeurope", 328.0, Provider::Azure),  // Netherlands
    ("northeurope", 296.0, Provider::Azure), // Ireland
    ("francecentral", 41.0, Provider::Azure),
    ("uksouth", 231.0, Provider::Azure),
    // Country / ISO codes (generic PUE)
    ("fr", 41.0, Provider::Generic),
    ("de", 338.0, Provider::Generic),
    ("gb", 231.0, Provider::Generic),
    ("uk", 231.0, Provider::Generic),
    ("us", 379.0, Provider::Generic),
    ("ie", 296.0, Provider::Generic),
    ("se", 8.0, Provider::Generic),
    ("no", 7.0, Provider::Generic),
    ("jp", 462.0, Provider::Generic),
    ("in", 708.0, Provider::Generic),
    ("au", 550.0, Provider::Generic),
    ("br", 96.0, Provider::Generic),
    ("sg", 408.0, Provider::Generic),
    ("nl", 328.0, Provider::Generic),
    ("be", 165.0, Provider::Generic),
    ("fi", 8.0, Provider::Generic),
    ("it", 370.0, Provider::Generic),
    ("es", 200.0, Provider::Generic),
    ("pl", 700.0, Provider::Generic),
];
