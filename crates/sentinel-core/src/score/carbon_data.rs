// GENERATED FILE - DO NOT EDIT BY HAND.
// Regenerate with: python3 scripts/refresh-carbon-data.py
//
// Carbon intensity rows (region_key, gCO2eq/kWh, provider) for regions
// on effectively national grids. Keys are lowercase. Subnational rows
// (North America, Brazil BR-CS) live in `MANUAL_CARBON_ROWS`
// (carbon.rs).
//
// Source: Ember yearly electricity data (CC-BY-4.0), generation-based
// annual gCO2/kWh, national granularity, latest year per country.
// https://ember-energy.org - methodology notes in docs/METHODOLOGY.md.

use super::carbon::Provider;

/// Grep-audited by release procedure step 2.5, like `PUE_VINTAGE`.
/// Stamped `ember-<latest-data-year>` by the refresh script.
#[allow(dead_code)]
pub(crate) const CARBON_TABLE_VINTAGE: &str = "ember-2025";

pub(super) static GENERATED_CARBON_ROWS: &[(&str, f64, Provider)] = &[
    // AWS regions
    ("eu-west-1", 255.9, Provider::Aws),      // Ireland
    ("eu-west-2", 217.4, Provider::Aws),      // London
    ("eu-west-3", 41.5, Provider::Aws),       // Paris
    ("eu-central-1", 329.6, Provider::Aws),   // Frankfurt
    ("eu-north-1", 35.4, Provider::Aws),      // Stockholm
    ("ap-northeast-1", 477.4, Provider::Aws), // Tokyo
    ("ap-southeast-1", 497.1, Provider::Aws), // Singapore
    ("eu-west-4", 253.6, Provider::Aws),      // Netherlands (canonical hourly key)
    ("eu-south-1", 284.6, Provider::Aws),     // Milan (Italy)
    ("ap-southeast-2", 524.6, Provider::Aws), // Sydney
    ("ap-south-1", 670.5, Provider::Aws),     // Mumbai
    // GCP regions
    ("europe-west1", 109.3, Provider::Gcp),      // Belgium
    ("europe-west4", 253.6, Provider::Gcp),      // Netherlands
    ("europe-west9", 41.5, Provider::Gcp),       // Paris
    ("europe-north1", 57.5, Provider::Gcp),      // Finland
    ("europe-west8", 284.6, Provider::Gcp),      // Milan (Italy)
    ("europe-southwest1", 153.6, Provider::Gcp), // Madrid (Spain)
    ("europe-central2", 590.8, Provider::Gcp),   // Warsaw (Poland)
    ("europe-north2", 28.1, Provider::Gcp),      // Norway
    ("asia-northeast1", 477.4, Provider::Gcp),   // Tokyo
    // Azure regions
    ("westeurope", 253.6, Provider::Azure),  // Netherlands
    ("northeurope", 255.9, Provider::Azure), // Ireland
    ("francecentral", 41.5, Provider::Azure),
    ("uksouth", 217.4, Provider::Azure),
    // OVHcloud regions
    ("gra5", 41.5, Provider::Ovh),              // Gravelines
    ("gra7", 41.5, Provider::Ovh),              // Gravelines
    ("gra9", 41.5, Provider::Ovh),              // Gravelines
    ("gra11", 41.5, Provider::Ovh),             // Gravelines
    ("gra", 41.5, Provider::Ovh),               // Gravelines (S3 location)
    ("sbg5", 41.5, Provider::Ovh),              // Strasbourg
    ("sbg", 41.5, Provider::Ovh),               // Strasbourg (S3 location)
    ("eu-west-rbx", 41.5, Provider::Ovh),       // Roubaix
    ("rbx", 41.5, Provider::Ovh),               // Roubaix (S3 location)
    ("eu-west-par", 41.5, Provider::Ovh),       // Paris, 3-AZ
    ("eu-south-mil", 284.6, Provider::Ovh),     // Milan, 3-AZ
    ("de1", 329.6, Provider::Ovh),              // Limburg
    ("uk1", 217.4, Provider::Ovh),              // Erith
    ("waw1", 590.8, Provider::Ovh),             // Warsaw
    ("waw", 590.8, Provider::Ovh),              // Warsaw (S3 location)
    ("sgp1", 497.1, Provider::Ovh),             // Singapore
    ("sgp", 497.1, Provider::Ovh),              // Singapore (S3 location)
    ("syd1", 524.6, Provider::Ovh),             // Sydney
    ("ap-southeast-syd", 524.6, Provider::Ovh), // Sydney (S3 location)
    ("ap-south-mum", 670.5, Provider::Ovh),     // Mumbai
    // Scaleway regions
    ("fr-par", 41.5, Provider::Scaleway),   // Paris
    ("fr-par-1", 41.5, Provider::Scaleway), // Paris DC2
    ("fr-par-2", 41.5, Provider::Scaleway), // Paris DC5
    ("fr-par-3", 41.5, Provider::Scaleway), // Paris DC4
    ("nl-ams", 253.6, Provider::Scaleway),  // Amsterdam
    ("nl-ams-1", 253.6, Provider::Scaleway),
    ("nl-ams-2", 253.6, Provider::Scaleway),
    ("nl-ams-3", 253.6, Provider::Scaleway),
    ("pl-waw", 590.8, Provider::Scaleway), // Warsaw
    ("pl-waw-1", 590.8, Provider::Scaleway),
    ("pl-waw-2", 590.8, Provider::Scaleway),
    ("pl-waw-3", 590.8, Provider::Scaleway),
    ("it-mil", 284.6, Provider::Scaleway), // Milan
    // OUTSCALE regions (prefixed keys)
    ("outscale-eu-west-2", 41.5, Provider::Outscale), // Paris (AWS eu-west-2 is London)
    ("outscale-cloudgouv-eu-west-1", 41.5, Provider::Outscale), // SecNumCloud
    ("outscale-ap-northeast-1", 477.4, Provider::Outscale), // Tokyo
    // Country / ISO codes (generic PUE)
    ("fr", 41.5, Provider::Generic),
    ("de", 329.6, Provider::Generic),
    ("gb", 217.4, Provider::Generic),
    ("uk", 217.4, Provider::Generic),
    ("us", 384.4, Provider::Generic),
    ("ie", 255.9, Provider::Generic),
    ("se", 35.4, Provider::Generic),
    ("no", 28.1, Provider::Generic),
    ("jp", 477.4, Provider::Generic),
    ("in", 670.5, Provider::Generic),
    ("au", 524.6, Provider::Generic),
    ("sg", 497.1, Provider::Generic),
    ("nl", 253.6, Provider::Generic),
    ("be", 109.3, Provider::Generic),
    ("fi", 57.5, Provider::Generic),
    ("it", 284.6, Provider::Generic),
    ("es", 153.6, Provider::Generic),
    ("pl", 590.8, Provider::Generic),
];
