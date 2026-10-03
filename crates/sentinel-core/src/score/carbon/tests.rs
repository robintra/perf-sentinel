use super::*;

// --- hourly profile tests ---

#[test]
fn hourly_profile_present_for_key_regions() {
    // The 4 Monthly regions plus FlatYear regions.
    assert!(hourly_profile_for_region_lower("eu-west-3").is_some());
    assert!(hourly_profile_for_region_lower("eu-central-1").is_some());
    assert!(hourly_profile_for_region_lower("eu-west-2").is_some());
    assert!(hourly_profile_for_region_lower("us-east-1").is_some());
    // FlatYear regions.
    assert!(hourly_profile_for_region_lower("eu-west-1").is_some());
    assert!(hourly_profile_for_region_lower("eu-west-4").is_some());
    assert!(hourly_profile_for_region_lower("eu-north-1").is_some());
    assert!(hourly_profile_for_region_lower("europe-west1").is_some());
    assert!(hourly_profile_for_region_lower("europe-north1").is_some());
    assert!(hourly_profile_for_region_lower("us-east-2").is_some());
    assert!(hourly_profile_for_region_lower("us-west-1").is_some());
    assert!(hourly_profile_for_region_lower("us-west-2").is_some());
    assert!(hourly_profile_for_region_lower("ca-central-1").is_some());
    assert!(hourly_profile_for_region_lower("ap-southeast-2").is_some());
    assert!(hourly_profile_for_region_lower("ap-northeast-1").is_some());
    assert!(hourly_profile_for_region_lower("ap-southeast-1").is_some());
    assert!(hourly_profile_for_region_lower("ap-south-1").is_some());
    assert!(hourly_profile_for_region_lower("sa-east-1").is_some());
}

#[test]
fn hourly_profile_absent_for_unknown_region() {
    assert!(hourly_profile_for_region_lower("mars-1").is_none());
    assert!(hourly_profile_for_region_lower("unknown-region").is_none());
}

#[test]
fn hourly_profile_aliases_resolve() {
    // Country-code aliases should point to the same profile.
    assert!(hourly_profile_for_region_lower("fr").is_some());
    assert!(hourly_profile_for_region_lower("de").is_some());
    assert!(hourly_profile_for_region_lower("gb").is_some());
    assert!(hourly_profile_for_region_lower("ie").is_some());
    assert!(hourly_profile_for_region_lower("nl").is_some());
    assert!(hourly_profile_for_region_lower("se").is_some());
    assert!(hourly_profile_for_region_lower("no").is_some());
    assert!(hourly_profile_for_region_lower("jp").is_some());
    assert!(hourly_profile_for_region_lower("br").is_some());
    // Cloud-provider aliases.
    assert!(hourly_profile_for_region_lower("westeurope").is_some());
    assert!(hourly_profile_for_region_lower("northeurope").is_some());
    assert!(hourly_profile_for_region_lower("uksouth").is_some());
    assert!(hourly_profile_for_region_lower("francecentral").is_some());
}

#[test]
fn hourly_profile_original_4_are_monthly() {
    // The 4 regions with Monthly profiles.
    assert!(
        hourly_profile_for_region_lower("eu-west-3")
            .unwrap()
            .is_monthly()
    );
    assert!(
        hourly_profile_for_region_lower("eu-central-1")
            .unwrap()
            .is_monthly()
    );
    assert!(
        hourly_profile_for_region_lower("eu-west-2")
            .unwrap()
            .is_monthly()
    );
    assert!(
        hourly_profile_for_region_lower("us-east-1")
            .unwrap()
            .is_monthly()
    );
}

#[test]
fn hourly_profile_new_regions_are_flat_year() {
    assert!(
        !hourly_profile_for_region_lower("eu-west-1")
            .unwrap()
            .is_monthly()
    );
    assert!(
        !hourly_profile_for_region_lower("us-east-2")
            .unwrap()
            .is_monthly()
    );
    assert!(
        !hourly_profile_for_region_lower("ca-central-1")
            .unwrap()
            .is_monthly()
    );
}

#[test]
fn hourly_intensity_lookup_returns_hour_value() {
    // France at July (month 6): night should be less than evening peak.
    let night_fr = lookup_hourly_intensity_lower("eu-west-3", 3, Some(6)).unwrap();
    let evening_fr = lookup_hourly_intensity_lower("eu-west-3", 18, Some(6)).unwrap();
    assert!(
        night_fr < evening_fr,
        "expected night ({night_fr}) < evening peak ({evening_fr}) in eu-west-3 (July)"
    );
}

#[test]
fn hourly_intensity_unknown_region_returns_none() {
    assert!(lookup_hourly_intensity_lower("mars-1", 10, None).is_none());
}

#[test]
fn hourly_intensity_invalid_hour_returns_none() {
    assert!(lookup_hourly_intensity_lower("eu-west-3", 24, None).is_none());
    assert!(lookup_hourly_intensity_lower("eu-west-3", 99, None).is_none());
}

#[test]
fn hourly_intensity_invalid_month_returns_none() {
    assert!(lookup_hourly_intensity_lower("eu-west-3", 12, Some(12)).is_none());
    assert!(lookup_hourly_intensity_lower("eu-west-3", 12, Some(99)).is_none());
}

/// Grand mean of a profile (monthly or flat year).
fn profile_grand_mean(pr: HourlyProfileRef<'_>) -> f64 {
    match pr {
        HourlyProfileRef::FlatYear(profile) => profile.iter().sum::<f64>() / 24.0,
        HourlyProfileRef::Monthly(profiles) => {
            let total: f64 = profiles.iter().flat_map(|m| m.iter()).sum();
            total / (12.0 * 24.0)
        }
    }
}

#[test]
fn hourly_profile_mean_close_to_annual_for_fr() {
    let pr = hourly_profile_for_region_lower("eu-west-3").unwrap();
    let mean = profile_grand_mean(pr);
    let annual = lookup_region_lower("eu-west-3").unwrap().0;
    let deviation = (mean - annual).abs() / annual;
    assert!(
        deviation < 0.05,
        "fr grand mean {mean:.1} deviates {deviation:.3} from annual {annual}"
    );
}

#[test]
fn hourly_profile_mean_close_to_annual_for_us_east() {
    let pr = hourly_profile_for_region_lower("us-east-1").unwrap();
    let mean = profile_grand_mean(pr);
    let annual = lookup_region_lower("us-east-1").unwrap().0;
    let deviation = (mean - annual).abs() / annual;
    assert!(
        deviation < 0.05,
        "us-east-1 grand mean {mean:.1} deviates {deviation:.3} from annual {annual}"
    );
}

#[test]
fn hourly_profile_mean_close_to_annual_for_gb() {
    let pr = hourly_profile_for_region_lower("eu-west-2").unwrap();
    let mean = profile_grand_mean(pr);
    let annual = lookup_region_lower("eu-west-2").unwrap().0;
    let deviation = (mean - annual).abs() / annual;
    assert!(
        deviation < 0.05,
        "gb grand mean {mean:.1} deviates {deviation:.3} from annual {annual}"
    );
}

#[test]
fn hourly_profile_de_mean_close_to_annual() {
    // The profile is rescaled from its 2022-vintage level (grand mean
    // ~431, ~31% off the annual table) to the Electricity Maps 2024 level.
    let pr = hourly_profile_for_region_lower("eu-central-1").unwrap();
    let mean = profile_grand_mean(pr);
    let annual = lookup_region_lower("eu-central-1").unwrap().0;
    let deviation = (mean - annual).abs() / annual;
    assert!(
        deviation < 0.05,
        "eu-central-1 grand mean {mean:.1} deviates {deviation:.3} from annual {annual}"
    );
}

// Mean invariant for all FlatYear regions.
#[test]
fn hourly_profile_mean_close_to_annual_for_all_flat_year_regions() {
    for &(key, ref profile) in crate::score::carbon_profiles::FLAT_YEAR_PROFILES {
        let vals: &[f64; 24] = profile;
        let mean: f64 = vals.iter().sum::<f64>() / 24.0;
        let (annual, _) = lookup_region_lower(key).unwrap_or_else(|| {
            panic!(
                "{key} is a canonical profile key but is missing from the carbon intensity table"
            )
        });
        let deviation = (mean - annual).abs() / annual;
        assert!(
            deviation < 0.05,
            "{key} hourly mean {mean:.1} deviates {deviation:.3} from annual {annual}"
        );
    }
}

#[test]
fn hourly_profile_mean_close_to_annual_for_all_monthly_regions() {
    for &(key, ref months) in crate::score::carbon_profiles::MONTHLY_PROFILES {
        let total: f64 = months.iter().flat_map(|m| m.iter()).sum();
        let mean = total / (12.0 * 24.0);
        let (annual, _) = lookup_region_lower(key).unwrap_or_else(|| {
            panic!(
                "{key} is a canonical monthly profile key but is missing from the carbon intensity table"
            )
        });
        let deviation = (mean - annual).abs() / annual;
        assert!(
            deviation < 0.05,
            "{key} monthly grand mean {mean:.1} deviates {deviation:.3} from annual {annual}"
        );
    }
}

#[test]
fn monthly_profile_seasonal_variation_fr() {
    // France: winter months should have higher mean than summer months.
    let pr = hourly_profile_for_region_lower("eu-west-3").unwrap();
    let jan_mean = (0..24).map(|h| pr.intensity_at(h, Some(0))).sum::<f64>() / 24.0;
    let jul_mean = (0..24).map(|h| pr.intensity_at(h, Some(6))).sum::<f64>() / 24.0;
    assert!(
        jan_mean > jul_mean,
        "FR January mean ({jan_mean:.1}) should be higher than July ({jul_mean:.1})"
    );
}

#[test]
fn monthly_profile_seasonal_variation_de() {
    let pr = hourly_profile_for_region_lower("eu-central-1").unwrap();
    let jan_mean = (0..24).map(|h| pr.intensity_at(h, Some(0))).sum::<f64>() / 24.0;
    let jun_mean = (0..24).map(|h| pr.intensity_at(h, Some(5))).sum::<f64>() / 24.0;
    assert!(
        jan_mean > jun_mean,
        "DE January mean ({jan_mean:.1}) should be higher than June ({jun_mean:.1})"
    );
}

// --- profile shape tests for solar-heavy grids ---

#[test]
fn caiso_profile_has_midday_solar_dip() {
    // CAISO duck curve: intensity at peak solar (UTC 18-21, local 10am-1pm)
    // must be well below the evening gas ramp (UTC 2-4, local 6-8pm).
    let pr = hourly_profile_for_region_lower("us-west-1").unwrap();
    let solar_min = (18..=21)
        .map(|h| pr.intensity_at(h, None))
        .fold(f64::INFINITY, f64::min);
    let evening_max = (2..=4)
        .map(|h| pr.intensity_at(h, None))
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        solar_min < evening_max * 0.80,
        "CAISO solar dip ({solar_min:.0}) should be well below evening peak ({evening_max:.0})"
    );
}

#[test]
fn spain_profile_has_midday_solar_dip() {
    // Spain: solar peak at local noon-2pm (UTC 10-12, CET=UTC+1).
    let pr = hourly_profile_for_region_lower("europe-southwest1").unwrap();
    let solar_min = (10..=13)
        .map(|h| pr.intensity_at(h, None))
        .fold(f64::INFINITY, f64::min);
    let evening_max = (17..=19)
        .map(|h| pr.intensity_at(h, None))
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        solar_min < evening_max * 0.85,
        "Spain solar dip ({solar_min:.0}) should be below evening peak ({evening_max:.0})"
    );
}

#[test]
fn hydro_profiles_are_nearly_flat() {
    // Hydro-dominated grids (SE, NO, CA) should have very low variation.
    for region in ["eu-north-1", "europe-north2", "ca-central-1"] {
        let pr = hourly_profile_for_region_lower(region).unwrap();
        let min = (0..24)
            .map(|h| pr.intensity_at(h, None))
            .fold(f64::INFINITY, f64::min);
        let max = (0..24)
            .map(|h| pr.intensity_at(h, None))
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max <= min * 2.5,
            "{region} hydro profile should be nearly flat (min={min:.0}, max={max:.0})"
        );
    }
}

// --- resolve_hourly_intensity tests ---

#[test]
fn resolve_hourly_intensity_custom_takes_precedence() {
    let mut custom = HashMap::new();
    custom.insert(
        "eu-west-3".to_string(),
        HourlyProfile::FlatYear([999.0; 24]),
    );
    let (val, src) = resolve_hourly_intensity("eu-west-3", 12, None, Some(&custom)).unwrap();
    assert!((val - 999.0).abs() < f64::EPSILON);
    assert_eq!(src, IntensitySource::Hourly);
}

#[test]
fn resolve_hourly_intensity_falls_through_to_embedded() {
    let (val, src) = resolve_hourly_intensity("eu-west-1", 12, None, None).unwrap();
    assert!(val > 0.0);
    assert_eq!(src, IntensitySource::Hourly); // eu-west-1 is FlatYear
}

#[test]
fn resolve_hourly_intensity_monthly_embedded() {
    let (val, src) = resolve_hourly_intensity("eu-west-3", 12, Some(6), None).unwrap();
    assert!(val > 0.0);
    assert_eq!(src, IntensitySource::MonthlyHourly);
}

#[test]
fn resolve_hourly_intensity_unknown_region_returns_none() {
    assert!(resolve_hourly_intensity("mars-1", 12, None, None).is_none());
}

#[test]
fn resolve_hourly_intensity_rejects_invalid_month() {
    assert!(resolve_hourly_intensity("eu-west-3", 12, Some(12), None).is_none());
    assert!(resolve_hourly_intensity("eu-west-3", 12, Some(99), None).is_none());
}

#[test]
fn resolve_hourly_intensity_rejects_invalid_hour() {
    assert!(resolve_hourly_intensity("eu-west-3", 24, None, None).is_none());
    assert!(resolve_hourly_intensity("eu-west-3", 99, None, None).is_none());
}

// --- load_custom_profiles tests ---

#[test]
fn load_custom_profiles_flat_year() {
    let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_flat.json");
    let hours: Vec<f64> = (0..24).map(|h| 50.0 + f64::from(h)).collect();
    let json =
        format!(r#"{{"profiles": {{"my-dc": {{"type": "flat_year", "hours": {hours:?}}}}}}}"#);
    std::fs::write(&path, &json).unwrap();
    let result = load_custom_profiles(&path).unwrap();
    assert!(result.contains_key("my-dc"));
    assert!(!result["my-dc"].is_monthly());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_custom_profiles_monthly() {
    let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_monthly.json");
    let month: Vec<f64> = vec![100.0; 24];
    let months: Vec<Vec<f64>> = vec![month; 12];
    let json =
        format!(r#"{{"profiles": {{"my-dc": {{"type": "monthly", "months": {months:?}}}}}}}"#);
    std::fs::write(&path, &json).unwrap();
    let result = load_custom_profiles(&path).unwrap();
    assert!(result["my-dc"].is_monthly());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_custom_profiles_rejects_wrong_dimensions() {
    let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_bad_dim.json");
    let json = r#"{"profiles": {"my-dc": {"type": "flat_year", "hours": [1.0, 2.0]}}}"#;
    std::fs::write(&path, json).unwrap();
    assert!(load_custom_profiles(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_custom_profiles_rejects_negative() {
    let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_neg.json");
    let mut hours = vec![50.0; 24];
    hours[5] = -1.0;
    let json =
        format!(r#"{{"profiles": {{"my-dc": {{"type": "flat_year", "hours": {hours:?}}}}}}}"#);
    std::fs::write(&path, &json).unwrap();
    assert!(load_custom_profiles(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_custom_profiles_rejects_nan() {
    let dir = std::env::temp_dir().join("perf_sentinel_test_profiles");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_nan.json");
    // NaN is not valid JSON, so we use null which will fail to parse as f64.
    let json = r#"{"profiles": {"my-dc": {"type": "flat_year", "hours": [null, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0, 20.0, 21.0, 22.0, 23.0]}}}"#;
    std::fs::write(&path, json).unwrap();
    assert!(load_custom_profiles(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn per_op_gco2_single_source() {
    // verify the per_op helper matches the compute_operational
    // formula so the two paths stay in sync.
    let per_op = per_op_gco2(ENERGY_PER_IO_OP_KWH, 100.0, 1.2);
    let bulk = compute_operational_gco2(1, 100.0, 1.2);
    assert!((per_op - bulk).abs() < 1e-18);
    let bulk10 = compute_operational_gco2(10, 100.0, 1.2);
    assert!((per_op * 10.0 - bulk10).abs() < 1e-18);
}

#[test]
fn carbon_estimate_with_model_tags() {
    // The `_with_model` constructors must carry the
    // supplied model tag all the way through.
    let e = CarbonEstimate::sci_numerator_with_model(0.001, CO2_MODEL_V2);
    assert_eq!(e.model, "io_proxy_v2");
    assert_eq!(e.methodology, "sci_v1_numerator");
    let e = CarbonEstimate::operational_ratio_with_model(0.001, CO2_MODEL_SCAPHANDRE);
    assert_eq!(e.model, "scaphandre_rapl");
    assert_eq!(e.methodology, "sci_v1_operational_ratio");
}

// PUE values are hand-maintained in this file, so pinning them is
// fine. Intensities come from the generated table: assert the
// same-country relation instead of a value a refresh will move.
#[test]
fn lookup_known_aws_region() {
    let (intensity, pue) = lookup_region("eu-west-3").expect("eu-west-3");
    let (fr_intensity, _) = lookup_region("fr").expect("fr");
    assert!((intensity - fr_intensity).abs() < f64::EPSILON);
    assert!((pue - 1.15).abs() < f64::EPSILON);
}

#[test]
fn lookup_known_gcp_region() {
    let (intensity, pue) = lookup_region("europe-west9").expect("europe-west9");
    let (fr_intensity, _) = lookup_region("fr").expect("fr");
    assert!((intensity - fr_intensity).abs() < f64::EPSILON);
    assert!((pue - 1.09).abs() < f64::EPSILON);
}

#[test]
fn lookup_known_ovh_region() {
    let (fr_intensity, _) = lookup_region("fr").expect("fr");
    // Every French OVHcloud key, whichever API named it: the Public
    // Cloud region (`gra11`), the S3 location string (`gra`, which
    // the zone code `GRA` also lowercases to) and the 3-AZ region
    // (`eu-west-par`, a different site on the same grid).
    for key in ["gra11", "gra", "eu-west-par"] {
        let (intensity, pue) = lookup_region(key).unwrap_or_else(|| panic!("{key}"));
        assert!((intensity - fr_intensity).abs() < f64::EPSILON, "{key}");
        assert!((pue - 1.24).abs() < f64::EPSILON, "{key}");
    }
}

#[test]
fn lookup_known_scaleway_region() {
    let (intensity, pue) = lookup_region("fr-par-2").expect("fr-par-2");
    let (fr_intensity, _) = lookup_region("fr").expect("fr");
    assert!((intensity - fr_intensity).abs() < f64::EPSILON);
    assert!((pue - 1.375).abs() < f64::EPSILON);
}

/// OUTSCALE reuses AWS region identifiers for different places, so its
/// keys are prefixed. Reading one for the other would put a Paris
/// workload on the London grid, a factor of five.
#[test]
fn outscale_keys_are_prefixed_and_never_shadow_aws() {
    let (outscale, pue) = lookup_region("outscale-eu-west-2").expect("outscale-eu-west-2");
    let (fr_intensity, _) = lookup_region("fr").expect("fr");
    assert!(
        (outscale - fr_intensity).abs() < f64::EPSILON,
        "Paris, not London"
    );
    // No published PUE, so the rows carry the generic figure.
    assert!((pue - GENERIC_PUE).abs() < f64::EPSILON);

    let (aws, _) = lookup_region("eu-west-2").expect("eu-west-2");
    let (gb_intensity, _) = lookup_region("gb").expect("gb");
    assert!(
        (aws - gb_intensity).abs() < f64::EPSILON,
        "AWS eu-west-2 stays London"
    );
    assert!(
        (outscale - aws).abs() > f64::EPSILON,
        "the two eu-west-2 must not resolve to the same grid"
    );

    // Every OUTSCALE row must carry the prefix, or it would collide.
    for &(key, _, provider) in super::super::carbon_data::GENERATED_CARBON_ROWS
        .iter()
        .chain(MANUAL_CARBON_ROWS)
    {
        if provider == Provider::Outscale {
            assert!(
                key.starts_with("outscale-"),
                "unprefixed OUTSCALE key: {key}"
            );
        }
    }
}

#[test]
fn lookup_country_code() {
    let (intensity, pue) = lookup_region("FR").expect("FR");
    assert!(intensity > 0.0);
    assert!((pue - 1.5).abs() < f64::EPSILON);
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
    let val = io_ops_to_co2_grams(1000, "eu-west-3").expect("eu-west-3");
    let (intensity, pue) = lookup_region("eu-west-3").expect("eu-west-3");
    let expected = 1000.0 * ENERGY_PER_IO_OP_KWH * intensity * pue;
    assert!((val - expected).abs() < 1e-9);
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
    let high = io_ops_to_co2_grams(1000, "ap-south-1").unwrap(); // India (coal-heavy)
    let low = io_ops_to_co2_grams(1000, "eu-north-1").unwrap(); // Stockholm (hydro/nuclear)
    assert!(high > low * 5.0, "India should be much higher than Sweden");
}

// The generated/manual split relies on disjoint keys: a HashMap
// collision would silently shadow a generated row with a stale
// manual one.
#[test]
fn generated_and_manual_carbon_keys_are_disjoint() {
    assert_eq!(
        REGION_MAP.len(),
        super::super::carbon_data::GENERATED_CARBON_ROWS.len() + MANUAL_CARBON_ROWS.len(),
        "a manual carbon row shadows a generated one"
    );
}

// Upstream data can publish a corrupt value (0, negative, or
// mis-scaled). No real grid sits outside (0, 2000] gCO2eq/kWh.
#[test]
fn all_carbon_rows_are_plausible() {
    for &(key, intensity, _) in super::super::carbon_data::GENERATED_CARBON_ROWS
        .iter()
        .chain(MANUAL_CARBON_ROWS)
    {
        assert!(
            intensity > 0.0 && intensity <= 2000.0,
            "{key}: implausible carbon intensity {intensity}"
        );
    }
}

#[test]
fn lookup_azure_region() {
    let result = lookup_region("eastus");
    assert!(result.is_some());
    let (_, pue) = result.unwrap();
    assert!(
        (pue - 1.17).abs() < f64::EPSILON,
        "Azure PUE should be 1.17"
    );
}

// ----- CarbonEstimate / CarbonReport / resolve_region tests -----

use std::sync::Arc;

use crate::event::{EventSource, EventType, SpanEvent};

fn make_event(service: &str, cloud_region: Option<&str>) -> SpanEvent {
    SpanEvent {
        timestamp: "2025-07-10T14:32:01.000Z".to_string(),
        trace_id: "trace-1".to_string(),
        span_id: "span-1".to_string(),
        parent_span_id: None,
        link_trace_id: None,
        service: Arc::from(service),
        grouping: Vec::new(),
        cloud_region: cloud_region.map(Arc::from),
        event_type: EventType::Sql,
        operation: "SELECT".to_string(),
        target: "SELECT 1".to_string(),
        duration_us: 1000,
        source: EventSource {
            endpoint: "GET /test".to_string(),
            method: "Test::method".to_string(),
        },
        status_code: None,
        response_size_bytes: None,
        code_function: None,
        code_filepath: None,
        code_lineno: None,
        code_namespace: None,
        instrumentation_scopes: Vec::new(),
    }
}

#[test]
fn carbon_estimate_sci_numerator_labels() {
    let est = CarbonEstimate::sci_numerator(0.000_100);
    assert!((est.mid - 0.000_100).abs() < f64::EPSILON);
    assert!((est.low - 0.000_050).abs() < f64::EPSILON);
    assert!((est.high - 0.000_200).abs() < f64::EPSILON);
    assert_eq!(est.model, "io_proxy_v1");
    assert_eq!(est.methodology, "sci_v1_numerator");
}

#[test]
fn carbon_estimate_operational_ratio_labels() {
    let est = CarbonEstimate::operational_ratio(0.000_050);
    assert!((est.mid - 0.000_050).abs() < f64::EPSILON);
    assert!((est.low - 0.000_025).abs() < f64::EPSILON);
    assert!((est.high - 0.000_100).abs() < f64::EPSILON);
    assert_eq!(est.model, "io_proxy_v1");
    assert_eq!(est.methodology, "sci_v1_operational_ratio");
}

#[test]
fn carbon_estimate_methodology_constants_are_distinct() {
    assert_ne!(METHODOLOGY_SCI_NUMERATOR, METHODOLOGY_OPERATIONAL_RATIO);
    assert_eq!(METHODOLOGY_SCI_NUMERATOR, "sci_v1_numerator");
    assert_eq!(METHODOLOGY_OPERATIONAL_RATIO, "sci_v1_operational_ratio");
}

#[test]
fn intensity_source_ordering_by_fidelity() {
    // Pin the derived Ord so reordering variants is caught.
    assert!(IntensitySource::Annual < IntensitySource::Hourly);
    assert!(IntensitySource::Hourly < IntensitySource::MonthlyHourly);
}

#[test]
fn carbon_estimate_from_zero_midpoint() {
    let est = CarbonEstimate::sci_numerator(0.0);
    assert!((est.low - 0.0).abs() < f64::EPSILON);
    assert!((est.mid - 0.0).abs() < f64::EPSILON);
    assert!((est.high - 0.0).abs() < f64::EPSILON);
}

#[test]
fn confidence_interval_factors_are_2x_multiplicative() {
    // The constants encode a 2× multiplicative uncertainty bracket
    // (not a symmetric ±50% window): low = mid/2, high = mid×2.
    // The geometric mean of low and high equals mid, making the
    // interval log-symmetric around the midpoint.
    let mid = 12.34_f64;
    let est = CarbonEstimate::sci_numerator(mid);
    assert!((est.low - mid * CO2_LOW_FACTOR).abs() < f64::EPSILON);
    assert!((est.high - mid * CO2_HIGH_FACTOR).abs() < f64::EPSILON);
    assert!((CO2_LOW_FACTOR - 0.5).abs() < f64::EPSILON);
    assert!((CO2_HIGH_FACTOR - 2.0).abs() < f64::EPSILON);
    // Geometric mean of low and high ≈ mid (log-symmetric).
    let geo_mean = (est.low * est.high).sqrt();
    assert!((geo_mean - mid).abs() < 1e-9);
}

#[test]
fn compute_operational_gco2_matches_expected() {
    // Hand-computed: 1000 ops × 1e-7 kWh × 56 gCO₂/kWh × 1.15 PUE = 6.440e-3 g
    let result = compute_operational_gco2(1000, 56.0, 1.15);
    assert!((result - 0.006_440).abs() < 1e-9);
}

#[test]
fn compute_operational_gco2_zero_ops() {
    assert!((compute_operational_gco2(0, 56.0, 1.15) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn io_ops_to_co2_grams_delegates_to_helper() {
    // Cross-check: the public scalar API and the internal helper
    // must produce the same result for the same inputs.
    let scalar = io_ops_to_co2_grams(1000, "eu-west-3").unwrap();
    let (intensity, pue) = lookup_region_lower("eu-west-3").unwrap();
    let helper = compute_operational_gco2(1000, intensity, pue);
    assert!((scalar - helper).abs() < f64::EPSILON);
}

#[test]
fn is_valid_region_id_accepts_valid() {
    assert!(is_valid_region_id("eu-west-3"));
    assert!(is_valid_region_id("us-east-1"));
    assert!(is_valid_region_id("europe-west9"));
    assert!(is_valid_region_id("francecentral"));
    assert!(is_valid_region_id("fr"));
    assert!(is_valid_region_id("unknown"));
    assert!(is_valid_region_id("mars-1"));
    assert!(is_valid_region_id("my_region_42"));
}

#[test]
fn is_valid_region_id_rejects_invalid() {
    assert!(!is_valid_region_id(""), "empty string");
    assert!(!is_valid_region_id(&"a".repeat(65)), "too long");
    assert!(!is_valid_region_id("eu west 3"), "space");
    assert!(!is_valid_region_id("eu.west.3"), "dot");
    assert!(!is_valid_region_id("eu/west/3"), "slash");
    assert!(!is_valid_region_id("eu-west-3\n"), "newline");
    assert!(!is_valid_region_id("eu-west-3\0"), "null byte");
    assert!(!is_valid_region_id("région"), "non-ASCII");
}

#[test]
fn is_valid_region_id_accepts_exact_64_chars() {
    let max_len = "a".repeat(64);
    assert!(is_valid_region_id(&max_len));
}

#[test]
fn resolve_region_prefers_event_attribute() {
    let mut service_regions = HashMap::new();
    service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
    let ctx = CarbonContext {
        default_region: Some("eu-west-3".to_string()),
        service_regions,
        embodied_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
        use_hourly_profiles: true,
        energy_snapshot: None,
        ..CarbonContext::default()
    };
    let event = make_event("order-svc", Some("ap-south-1"));
    assert_eq!(resolve_region(&event, &ctx), Some("ap-south-1"));
}

#[test]
fn resolve_region_falls_back_to_service_map() {
    let mut service_regions = HashMap::new();
    service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
    let ctx = CarbonContext {
        default_region: Some("eu-west-3".to_string()),
        service_regions,
        embodied_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
        use_hourly_profiles: true,
        energy_snapshot: None,
        ..CarbonContext::default()
    };
    let event = make_event("order-svc", None);
    assert_eq!(resolve_region(&event, &ctx), Some("us-east-1"));
}

#[test]
fn resolve_region_falls_back_to_default() {
    let ctx = CarbonContext {
        default_region: Some("eu-west-3".to_string()),
        service_regions: HashMap::new(),
        embodied_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
        use_hourly_profiles: true,
        energy_snapshot: None,
        ..CarbonContext::default()
    };
    let event = make_event("unknown-svc", None);
    assert_eq!(resolve_region(&event, &ctx), Some("eu-west-3"));
}

#[test]
fn resolve_region_returns_none_when_all_unset() {
    let ctx = CarbonContext::default();
    let event = make_event("any-svc", None);
    assert_eq!(resolve_region(&event, &ctx), None);
}

#[test]
fn resolve_region_service_map_does_not_shadow_event_attribute() {
    // Even if the service has a config override, the span's own
    // cloud.region should win (most authoritative source).
    let mut service_regions = HashMap::new();
    service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
    let ctx = CarbonContext {
        default_region: None,
        service_regions,
        embodied_per_request_gco2: DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2,
        use_hourly_profiles: true,
        energy_snapshot: None,
        ..CarbonContext::default()
    };
    let event = make_event("order-svc", Some("eu-north-1"));
    assert_eq!(resolve_region(&event, &ctx), Some("eu-north-1"));
}

#[test]
fn resolve_region_service_map_is_case_insensitive() {
    // Config loader lowercases service_regions keys. Incoming span
    // events may carry mixed-case service names (e.g. "Order-Svc" from
    // an older .NET SDK). resolve_region lowercases event.service
    // before lookup so they still match.
    let mut service_regions = HashMap::new();
    service_regions.insert("order-svc".to_string(), "us-east-1".to_string());
    let ctx = CarbonContext {
        default_region: None,
        service_regions,
        embodied_per_request_gco2: 0.0,
        use_hourly_profiles: true,
        energy_snapshot: None,
        ..CarbonContext::default()
    };
    // Mixed-case service name on the event, should still match.
    let event = make_event("Order-Svc", None);
    assert_eq!(resolve_region(&event, &ctx), Some("us-east-1"));
    // Upper-case service name.
    let event_upper = make_event("ORDER-SVC", None);
    assert_eq!(resolve_region(&event_upper, &ctx), Some("us-east-1"));
}

// ── energy_coefficient tests ───────────────────────────────────

fn make_sql_target_event(target: &str) -> SpanEvent {
    SpanEvent {
        timestamp: "2025-07-10T14:32:01.000Z".to_string(),
        trace_id: "trace-1".to_string(),
        span_id: "span-1".to_string(),
        parent_span_id: None,
        link_trace_id: None,
        service: Arc::from("test"),
        grouping: Vec::new(),
        cloud_region: None,
        event_type: EventType::Sql,
        operation: "postgresql".to_string(),
        target: target.to_string(),
        duration_us: 1000,
        source: EventSource {
            endpoint: "GET /test".to_string(),
            method: "Test::method".to_string(),
        },
        status_code: None,
        response_size_bytes: None,
        code_function: None,
        code_filepath: None,
        code_lineno: None,
        code_namespace: None,
        instrumentation_scopes: Vec::new(),
    }
}

fn make_http_size_event(response_size_bytes: Option<u64>) -> SpanEvent {
    SpanEvent {
        timestamp: "2025-07-10T14:32:01.000Z".to_string(),
        trace_id: "trace-1".to_string(),
        span_id: "span-1".to_string(),
        parent_span_id: None,
        link_trace_id: None,
        service: Arc::from("test"),
        grouping: Vec::new(),
        cloud_region: None,
        event_type: EventType::HttpOut,
        operation: "GET".to_string(),
        target: "http://user-svc:5000/api/users/123".to_string(),
        duration_us: 1000,
        source: EventSource {
            endpoint: "GET /test".to_string(),
            method: "Test::method".to_string(),
        },
        status_code: Some(200),
        response_size_bytes,
        code_function: None,
        code_filepath: None,
        code_lineno: None,
        code_namespace: None,
        instrumentation_scopes: Vec::new(),
    }
}

#[test]
fn energy_coefficient_sql_select() {
    let event = make_sql_target_event("SELECT * FROM users WHERE id = 1");
    assert!((energy_coefficient(&event) - SQL_SELECT_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_sql_insert() {
    let event = make_sql_target_event("INSERT INTO users (name) VALUES ('Alice')");
    assert!((energy_coefficient(&event) - SQL_INSERT_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_sql_update() {
    let event = make_sql_target_event("UPDATE users SET name = 'Bob' WHERE id = 1");
    assert!((energy_coefficient(&event) - SQL_UPDATE_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_sql_delete() {
    let event = make_sql_target_event("DELETE FROM users WHERE id = 1");
    assert!((energy_coefficient(&event) - SQL_DELETE_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_sql_other() {
    let event = make_sql_target_event("CREATE TABLE users (id INT)");
    assert!((energy_coefficient(&event) - SQL_OTHER_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_sql_case_insensitive() {
    let event = make_sql_target_event("select * from users");
    assert!((energy_coefficient(&event) - SQL_SELECT_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_http_small() {
    let event = make_http_size_event(Some(1024)); // 1 KB
    assert!((energy_coefficient(&event) - HTTP_SMALL_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_http_medium() {
    let event = make_http_size_event(Some(100 * 1024)); // 100 KB
    assert!((energy_coefficient(&event) - HTTP_MEDIUM_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_http_large() {
    let event = make_http_size_event(Some(2 * 1024 * 1024)); // 2 MB
    assert!((energy_coefficient(&event) - HTTP_LARGE_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_http_no_size() {
    let event = make_http_size_event(None);
    assert!((energy_coefficient(&event) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_http_boundary_small_threshold() {
    // Exactly at the small/medium boundary (10 KB) should be medium.
    let event = make_http_size_event(Some(HTTP_SMALL_THRESHOLD));
    assert!((energy_coefficient(&event) - HTTP_MEDIUM_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_http_boundary_large_threshold() {
    // Exactly at the large boundary (1 MB) is still medium, and >1 MB is large.
    let event = make_http_size_event(Some(HTTP_LARGE_THRESHOLD));
    assert!((energy_coefficient(&event) - HTTP_MEDIUM_COEFF).abs() < f64::EPSILON);
    let event_over = make_http_size_event(Some(HTTP_LARGE_THRESHOLD + 1));
    assert!((energy_coefficient(&event_over) - HTTP_LARGE_COEFF).abs() < f64::EPSILON);
}

// ── extract_hostname tests ─────────────────────────────────────

#[test]
fn extract_hostname_http_with_port() {
    assert_eq!(
        extract_hostname("http://user-svc:5000/api/users"),
        Some("user-svc")
    );
}

#[test]
fn extract_hostname_http_no_port() {
    assert_eq!(
        extract_hostname("http://user-svc/api/users"),
        Some("user-svc")
    );
}

#[test]
fn extract_hostname_https() {
    assert_eq!(
        extract_hostname("https://api.example.com/path"),
        Some("api.example.com")
    );
}

#[test]
fn extract_hostname_empty() {
    assert_eq!(extract_hostname(""), None);
}

#[test]
fn extract_hostname_no_scheme() {
    assert_eq!(extract_hostname("/api/users"), None);
}

#[test]
fn extract_hostname_empty_host() {
    assert_eq!(extract_hostname("http:///path"), None);
}

#[test]
fn extract_hostname_with_userinfo() {
    // RFC 3986 userinfo: "user:pass@host:port" should extract "host"
    assert_eq!(
        extract_hostname("http://user:pass@order-api:8080/api/orders"),
        Some("order-api")
    );
}

#[test]
fn extract_hostname_with_user_only() {
    assert_eq!(
        extract_hostname("http://admin@order-api/api"),
        Some("order-api")
    );
}

#[test]
fn energy_coefficient_http_zero_bytes() {
    let event = make_http_size_event(Some(0));
    assert!((energy_coefficient(&event) - HTTP_SMALL_COEFF).abs() < f64::EPSILON);
}

#[test]
fn energy_coefficient_sql_empty_target() {
    let event = make_sql_target_event("");
    assert!((energy_coefficient(&event) - SQL_OTHER_COEFF).abs() < f64::EPSILON);
}

// --- ScoringConfig (audit-trail surface) ---

#[test]
fn scoring_config_default_is_v4_lifecycle_hourly() {
    let cfg = ScoringConfig::default();
    assert_eq!(cfg.api_version, ApiVersion::V4);
    assert_eq!(cfg.emission_factor_type, EmissionFactorType::Lifecycle);
    assert_eq!(cfg.temporal_granularity, TemporalGranularity::Hourly);
}

/// A v4 Electricity Maps config, the shape both endpoint-detection
/// tests need. Shared so the two cannot drift apart.
fn v4_electricity_maps_config() -> ElectricityMapsConfig {
    ElectricityMapsConfig {
        api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
        auth_token: "test-token".to_string(),
        poll_interval: std::time::Duration::from_mins(5),
        region_map: HashMap::new(),
        emission_factor_type: EmissionFactorType::Lifecycle,
        temporal_granularity: TemporalGranularity::Hourly,
    }
}

#[test]
fn scoring_config_only_claims_electricity_maps_when_built_from_it() {
    let legacy = ScoringConfig::default();
    assert!(legacy.uses_electricity_maps());
    let em = v4_electricity_maps_config();
    assert_eq!(
        ScoringConfig::from_electricity_maps(&em).electricity_maps,
        Some(true)
    );
    assert!(
        !ScoringConfig {
            electricity_maps: Some(false),
            ..ScoringConfig::default()
        }
        .uses_electricity_maps()
    );
}

#[test]
fn scoring_config_retains_eq_compatibility() {
    fn requires_eq<T: Eq>() {}
    requires_eq::<ScoringConfig>();
}

#[test]
fn scoring_config_round_trip_json_all_defaults() {
    let cfg = ScoringConfig::default();
    let json = serde_json::to_string(&cfg).unwrap();
    let back: ScoringConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(cfg, back);
    assert!(json.contains("\"v4\""));
    assert!(json.contains("\"lifecycle\""));
    assert!(json.contains("\"hourly\""));
}

#[test]
fn scoring_config_round_trip_json_all_optins() {
    let cfg = ScoringConfig {
        api_version: ApiVersion::V3,
        emission_factor_type: EmissionFactorType::Direct,
        temporal_granularity: TemporalGranularity::FiveMinutes,
        ..ScoringConfig::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: ScoringConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(cfg, back);
    assert!(json.contains("\"v3\""));
    assert!(json.contains("\"direct\""));
    assert!(json.contains("\"5_minutes\""));
}

#[test]
fn scoring_config_from_electricity_maps_derives_api_version_from_endpoint() {
    // ElectricityMapsConfig has no Default impl (auth_token is
    // mandatory), so build it manually. The test asserts that the
    // api_version field is derived from the endpoint URL and the
    // two knobs are copied through verbatim.
    let cfg = ElectricityMapsConfig {
        api_endpoint: "https://api.electricitymaps.com/v3".to_string(),
        auth_token: "test-token".to_string(),
        poll_interval: std::time::Duration::from_mins(5),
        region_map: HashMap::new(),
        emission_factor_type: EmissionFactorType::Direct,
        temporal_granularity: TemporalGranularity::FifteenMinutes,
    };
    let scoring = ScoringConfig::from_electricity_maps(&cfg);
    assert_eq!(scoring.api_version, ApiVersion::V3);
    assert_eq!(scoring.emission_factor_type, EmissionFactorType::Direct);
    assert_eq!(
        scoring.temporal_granularity,
        TemporalGranularity::FifteenMinutes
    );
}

#[test]
fn scoring_config_from_electricity_maps_v4_default_endpoint() {
    // Lock the v4 path so a future short-circuit on V3 in
    // `from_electricity_maps` cannot regress the default detection.
    let cfg = v4_electricity_maps_config();
    let scoring = ScoringConfig::from_electricity_maps(&cfg);
    assert_eq!(scoring.api_version, ApiVersion::V4);
    assert_eq!(scoring.emission_factor_type, EmissionFactorType::Lifecycle);
    assert_eq!(scoring.temporal_granularity, TemporalGranularity::Hourly);
}

#[test]
fn scoring_config_from_electricity_maps_custom_endpoint() {
    // Lock the Custom path so an enterprise proxy or mock URL
    // without a `/vN` suffix surfaces correctly on the
    // `green_summary.scoring_config.api_version` chip.
    let cfg = ElectricityMapsConfig {
        api_endpoint: "https://corp-proxy.acme.internal/electricity-maps".to_string(),
        auth_token: "test-token".to_string(),
        poll_interval: std::time::Duration::from_mins(5),
        region_map: HashMap::new(),
        emission_factor_type: EmissionFactorType::Lifecycle,
        temporal_granularity: TemporalGranularity::Hourly,
    };
    let scoring = ScoringConfig::from_electricity_maps(&cfg);
    assert_eq!(scoring.api_version, ApiVersion::Custom);
}
