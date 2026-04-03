//! `GreenOps` gCO₂eq conversion: static region-based carbon intensity table.
//!
//! Embeds carbon intensity values per region (gCO₂eq/kWh) and cloud provider PUE.
//! No network calls, all data is embedded at compile time.
//! Sources: Cloud Carbon Footprint (CCF), Electricity Maps annual averages.

/// Estimated energy consumed per I/O operation in kWh.
///
/// This is a rough order-of-magnitude approximation (~0.1 µWh per I/O op).
/// It accounts for a typical database query or HTTP round-trip on cloud
/// infrastructure, including CPU, memory, and network overhead.
///
/// **This is NOT a measured value.** The actual energy depends on I/O type,
/// latency, payload size, and hardware. This constant is used to convert
/// I/O operation counts into estimated gCO₂eq as an indicative metric,
/// not a precise measurement.
///
/// For SCI (ISO/IEC 21031:2024) compliance, this approximation must be
/// disclosed as methodology in reports and documentation.
const ENERGY_PER_IO_OP_KWH: f64 = 0.000_000_1;

/// Cloud provider identifier for PUE lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    Aws,
    Gcp,
    Azure,
    Generic,
}

impl Provider {
    /// Power Usage Effectiveness for this provider.
    const fn pue(self) -> f64 {
        match self {
            Self::Aws => 1.135,
            Self::Gcp => 1.10,
            Self::Azure => 1.185,
            Self::Generic => 1.2,
        }
    }
}

/// Static carbon intensity table: (`region_key`, gCO₂eq/kWh, provider).
///
/// Region keys are lowercase for case-insensitive matching.
/// Data from CCF and Electricity Maps (2023-2024 annual averages).
static CARBON_TABLE: &[(&str, f64, Provider)] = &[
    // AWS regions
    ("us-east-1", 379.0, Provider::Aws),
    ("us-east-2", 410.0, Provider::Aws),
    ("us-west-1", 200.0, Provider::Aws),
    ("us-west-2", 89.0, Provider::Aws),
    ("eu-west-1", 296.0, Provider::Aws),      // Ireland
    ("eu-west-2", 231.0, Provider::Aws),      // London
    ("eu-west-3", 56.0, Provider::Aws),       // Paris
    ("eu-central-1", 338.0, Provider::Aws),   // Frankfurt
    ("eu-north-1", 8.0, Provider::Aws),       // Stockholm
    ("ap-northeast-1", 462.0, Provider::Aws), // Tokyo
    ("ap-southeast-1", 408.0, Provider::Aws), // Singapore
    ("ap-southeast-2", 550.0, Provider::Aws), // Sydney
    ("ap-south-1", 708.0, Provider::Aws),     // Mumbai
    ("ca-central-1", 13.0, Provider::Aws),    // Canada
    ("sa-east-1", 62.0, Provider::Aws),       // São Paulo
    // GCP regions
    ("us-central1", 426.0, Provider::Gcp),
    ("us-east1", 379.0, Provider::Gcp),
    ("us-west1", 89.0, Provider::Gcp),
    ("europe-west1", 187.0, Provider::Gcp),    // Belgium
    ("europe-west4", 328.0, Provider::Gcp),    // Netherlands
    ("europe-west9", 56.0, Provider::Gcp),     // Paris
    ("europe-north1", 8.0, Provider::Gcp),     // Finland
    ("asia-northeast1", 462.0, Provider::Gcp), // Tokyo
    // Azure regions
    ("eastus", 379.0, Provider::Azure),
    ("westus2", 89.0, Provider::Azure),
    ("westeurope", 328.0, Provider::Azure),  // Netherlands
    ("northeurope", 296.0, Provider::Azure), // Ireland
    ("francecentral", 56.0, Provider::Azure),
    ("uksouth", 231.0, Provider::Azure),
    // Country / ISO codes (generic PUE)
    ("fr", 56.0, Provider::Generic),
    ("de", 338.0, Provider::Generic),
    ("gb", 231.0, Provider::Generic),
    ("uk", 231.0, Provider::Generic),
    ("us", 379.0, Provider::Generic),
    ("ie", 296.0, Provider::Generic),
    ("se", 8.0, Provider::Generic),
    ("no", 7.0, Provider::Generic),
    ("ca", 13.0, Provider::Generic),
    ("jp", 462.0, Provider::Generic),
    ("in", 708.0, Provider::Generic),
    ("au", 550.0, Provider::Generic),
    ("br", 62.0, Provider::Generic),
    ("sg", 408.0, Provider::Generic),
    ("nl", 328.0, Provider::Generic),
    ("be", 187.0, Provider::Generic),
    ("fi", 8.0, Provider::Generic),
];

/// Pre-built map for O(1) region lookup (keys are lowercase).
static REGION_MAP: std::sync::LazyLock<std::collections::HashMap<&'static str, (f64, Provider)>> =
    std::sync::LazyLock::new(|| {
        CARBON_TABLE
            .iter()
            .map(|&(key, intensity, provider)| (key, (intensity, provider)))
            .collect()
    });

/// Look up carbon intensity for a region string.
///
/// Returns `(carbon_intensity_gco2_per_kwh, pue)` if the region is found.
/// Matching is case-insensitive (input is lowercased before lookup).
#[must_use]
pub fn lookup_region(region: &str) -> Option<(f64, f64)> {
    let lower = region.to_ascii_lowercase();
    lookup_region_lower(&lower)
}

/// Look up carbon intensity for a **pre-lowercased** region string.
///
/// Use this when the caller has already lowercased the region to avoid
/// a redundant allocation.
#[must_use]
fn lookup_region_lower(region: &str) -> Option<(f64, f64)> {
    REGION_MAP
        .get(region)
        .map(|(intensity, provider)| (*intensity, provider.pue()))
}

/// Convert I/O operations to estimated gCO₂eq for a **pre-lowercased** region.
///
/// Formula: `gCO₂eq = io_ops × ENERGY_PER_IO_OP_KWH × carbon_intensity × PUE`
///
/// Returns `None` if the region is not recognized.
#[must_use]
pub fn io_ops_to_co2_grams(io_ops: usize, region: &str) -> Option<f64> {
    let (intensity, pue) = lookup_region_lower(region)?;
    Some(io_ops as f64 * ENERGY_PER_IO_OP_KWH * intensity * pue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_aws_region() {
        let result = lookup_region("eu-west-3");
        assert!(result.is_some());
        let (intensity, pue) = result.unwrap();
        assert!((intensity - 56.0).abs() < f64::EPSILON);
        assert!((pue - 1.135).abs() < f64::EPSILON);
    }

    #[test]
    fn lookup_known_gcp_region() {
        let result = lookup_region("europe-west9");
        assert!(result.is_some());
        let (intensity, pue) = result.unwrap();
        assert!((intensity - 56.0).abs() < f64::EPSILON);
        assert!((pue - 1.10).abs() < f64::EPSILON);
    }

    #[test]
    fn lookup_country_code() {
        let result = lookup_region("FR");
        assert!(result.is_some());
        let (intensity, pue) = result.unwrap();
        assert!((intensity - 56.0).abs() < f64::EPSILON);
        assert!((pue - 1.2).abs() < f64::EPSILON);
    }

    #[test]
    fn lookup_case_insensitive() {
        assert!(lookup_region("EU-WEST-3").is_some());
        assert!(lookup_region("Us-East-1").is_some());
        assert!(lookup_region("fr").is_some());
        assert!(lookup_region("FR").is_some());
    }

    #[test]
    fn lookup_unknown_region_returns_none() {
        assert!(lookup_region("unknown-region").is_none());
        assert!(lookup_region("").is_none());
    }

    #[test]
    fn io_ops_to_co2_known_region() {
        let co2 = io_ops_to_co2_grams(1000, "eu-west-3");
        assert!(co2.is_some());
        let val = co2.unwrap();
        // 1000 * 0.0000001 * 56.0 * 1.135 = 0.006356
        assert!((val - 0.006_356).abs() < 1e-9);
    }

    #[test]
    fn io_ops_to_co2_unknown_region() {
        assert!(io_ops_to_co2_grams(1000, "mars-1").is_none());
    }

    #[test]
    fn io_ops_to_co2_zero_ops() {
        let co2 = io_ops_to_co2_grams(0, "eu-west-3");
        assert!(co2.is_some());
        assert!((co2.unwrap() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn high_carbon_region_vs_low() {
        let high = io_ops_to_co2_grams(1000, "ap-south-1").unwrap(); // India, 708
        let low = io_ops_to_co2_grams(1000, "eu-north-1").unwrap(); // Stockholm, 8
        assert!(high > low * 10.0, "India should be much higher than Sweden");
    }
}
