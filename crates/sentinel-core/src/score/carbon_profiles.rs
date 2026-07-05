//! Hourly carbon intensity profiles: static data tables and `HourlyProfile` types.
//!
//! Profiles are indexed by cloud region key (lowercase). Each region has either
//! a single representative 24-hour UTC profile (`FlatYear`) or 12 monthly
//! variants (`Monthly`). Country-code and cloud-provider aliases point to the
//! same static data via the `PROFILE_ALIASES` table.
//!
//! Sources:
//! - ENTSO-E Transparency Platform: hourly generation data for European bidding zones
//! - EIA Open Data API: hourly generation by fuel type for US Balancing Authorities
//! - AEMO NEM: 5-min generation data for Australia (aggregated to hourly)
//! - Hydro-Quebec / IESO: Canadian grid generation data
//! - Electricity Maps: annual reports with diurnal patterns (2023-2024)
//! - Cloud Carbon Footprint (CCF): annual grid intensities cross-reference

/// Temporal range covered by the embedded grid carbon-intensity
/// profiles. Most regions (ENTSO-E, EIA, AEMO, Hydro-Quebec) span
/// `2022-2024`; the Electricity Maps subset (Japan, Singapore, India,
/// generic fallback) is on `2023-2024`. The constant reports the
/// hourly-shape range; profile levels are renormalized to the annual
/// table whenever a dataset refresh moves an annual value beyond 5
/// percent (the `Annual:` number in each block comment is the current
/// level). Release procedure step 2.5 surfaces this string via `grep`.
/// Bump when any shape source is refreshed beyond the range.
#[allow(dead_code)]
pub(crate) const CARBON_PROFILES_VINTAGE: &str = "2022-2024 shapes, ember-2025 levels";

/// Hourly carbon intensity profile.
///
/// `FlatYear` uses the same 24-hour UTC profile for all months.
/// `Monthly` provides 12 distinct profiles (index 0 = January, 11 = December)
/// to capture seasonal variation in grid carbon intensity.
#[derive(Debug, Clone, PartialEq)]
pub enum HourlyProfile {
    /// Single representative 24-hour UTC profile, used all year.
    FlatYear([f64; 24]),
    /// 12 monthly profiles. Index 0 = January, 11 = December.
    Monthly(Box<[[f64; 24]; 12]>),
}

/// Borrowed reference into profile data. `Copy`-friendly,
/// used in the `LazyLock` map (with `'static`) and in
/// `HourlyProfile::as_ref()` (with a shorter lifetime).
#[derive(Debug, Clone, Copy)]
pub(crate) enum HourlyProfileRef<'a> {
    FlatYear(&'a [f64; 24]),
    Monthly(&'a [[f64; 24]; 12]),
}

impl HourlyProfileRef<'_> {
    /// Look up intensity for a given hour (0-23) and optional month (0-11).
    /// When `month` is `None` and the profile is `Monthly`, falls back to
    /// an annual average across all 12 months for that hour.
    #[inline]
    #[must_use]
    pub(crate) fn intensity_at(self, hour: u8, month: Option<u8>) -> f64 {
        debug_assert!(hour < 24, "hour must be 0..24, got {hour}");
        match self {
            Self::FlatYear(profile) => profile[hour as usize],
            Self::Monthly(profiles) => {
                if let Some(m) = month {
                    profiles[m.min(11) as usize][hour as usize]
                } else {
                    let h = hour as usize;
                    profiles.iter().map(|m| m[h]).sum::<f64>() / 12.0
                }
            }
        }
    }

    /// Whether this is a monthly profile (for `IntensitySource` tagging).
    #[inline]
    #[must_use]
    pub(crate) fn is_monthly(self) -> bool {
        matches!(self, Self::Monthly(_))
    }
}

impl HourlyProfile {
    /// Convert to a borrowed `HourlyProfileRef` for shared logic.
    #[inline]
    fn as_ref(&self) -> HourlyProfileRef<'_> {
        match self {
            Self::FlatYear(arr) => HourlyProfileRef::FlatYear(arr),
            Self::Monthly(arr) => HourlyProfileRef::Monthly(arr),
        }
    }

    /// Look up intensity for a given hour (0-23) and optional month (0-11).
    #[inline]
    #[must_use]
    pub fn intensity_at(&self, hour: u8, month: Option<u8>) -> f64 {
        self.as_ref().intensity_at(hour, month)
    }

    /// Whether this is a monthly profile.
    #[inline]
    #[must_use]
    pub fn is_monthly(&self) -> bool {
        self.as_ref().is_monthly()
    }

    /// Compute the arithmetic mean across all hours (and months).
    #[must_use]
    pub fn mean(&self) -> f64 {
        match self {
            Self::FlatYear(profile) => profile.iter().sum::<f64>() / 24.0,
            Self::Monthly(profiles) => {
                let total: f64 = profiles.iter().flat_map(|m| m.iter()).sum();
                total / (12.0 * 24.0)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Monthly profiles (12 x 24 values). Original 4 regions with seasonal data.
// ---------------------------------------------------------------------------

/// Monthly x hourly profiles. 12 months x 24 UTC hours per region.
/// Sources cited per entry. Grand mean must be within +/-5% of `CARBON_TABLE`
/// annual value.
pub(crate) static MONTHLY_PROFILES: &[(&str, [[f64; 24]; 12])] = &[
    // France (eu-west-3): nuclear baseload, seasonal gas peaking.
    // Winter: higher intensity from gas peaking for heating demand.
    // Summer: lower intensity, nuclear provides larger share.
    // Shape: ENTSO-E Transparency Platform, RTE eCO2mix. Level rescaled
    // to the Electricity Maps 2023-2024 consumption-based mean (41).
    // Annual mean in CARBON_TABLE: 41.0. Grand mean target: ~41.
    (
        "eu-west-3",
        [
            // January: winter peak, gas + imports from DE
            [
                42.9, 41.4, 40.7, 40.0, 40.7, 42.2, 45.9, 50.3, 53.3, 51.8, 50.3, 48.8, 47.4, 45.9,
                44.4, 45.9, 50.3, 57.7, 60.7, 57.7, 53.3, 48.8, 45.9, 44.4,
            ],
            // February: still winter, slightly less demand
            [
                41.4, 40.0, 39.2, 38.5, 39.2, 40.7, 44.4, 48.8, 51.8, 50.3, 48.8, 47.4, 45.9, 44.4,
                42.9, 44.4, 48.8, 56.2, 59.2, 56.2, 51.8, 47.4, 44.4, 42.9,
            ],
            // March: transition, heating demand drops
            [
                38.5, 37.0, 36.3, 35.5, 36.3, 37.7, 41.4, 45.9, 48.8, 47.4, 45.9, 44.4, 42.9, 41.4,
                40.0, 41.4, 45.9, 53.3, 56.2, 53.3, 48.8, 44.4, 41.4, 40.0,
            ],
            // April: spring, mild weather
            [
                35.5, 34.0, 33.3, 32.6, 33.3, 34.8, 38.5, 42.9, 45.9, 44.4, 42.9, 41.4, 40.0, 38.5,
                37.0, 38.5, 42.9, 50.3, 53.3, 50.3, 45.9, 41.4, 38.5, 37.0,
            ],
            // May: solar contribution increases
            [
                32.6, 31.1, 30.3, 29.6, 30.3, 31.8, 35.5, 40.0, 41.4, 40.0, 38.5, 37.0, 35.5, 34.0,
                32.6, 34.0, 38.5, 45.9, 48.8, 45.9, 41.4, 37.0, 34.0, 32.6,
            ],
            // June: summer, low demand, high nuclear share
            [
                29.6, 28.1, 27.4, 26.6, 27.4, 28.9, 32.6, 37.0, 38.5, 37.0, 35.5, 34.0, 32.6, 31.1,
                29.6, 31.1, 35.5, 41.4, 44.4, 41.4, 37.0, 32.6, 31.1, 29.6,
            ],
            // July: summer, AC load partially offsets
            [
                31.1, 29.6, 28.9, 28.1, 28.9, 30.3, 34.0, 38.5, 40.0, 38.5, 37.0, 35.5, 34.0, 32.6,
                31.1, 32.6, 37.0, 42.9, 45.9, 42.9, 38.5, 34.0, 32.6, 31.1,
            ],
            // August: similar to July
            [
                31.1, 29.6, 28.9, 28.1, 28.9, 30.3, 34.0, 38.5, 40.0, 38.5, 37.0, 35.5, 34.0, 32.6,
                31.1, 32.6, 37.0, 42.9, 45.9, 42.9, 38.5, 34.0, 32.6, 31.1,
            ],
            // September: transition back
            [
                34.0, 32.6, 31.8, 31.1, 31.8, 33.3, 37.0, 41.4, 44.4, 42.9, 41.4, 40.0, 38.5, 37.0,
                35.5, 37.0, 41.4, 48.8, 51.8, 48.8, 44.4, 40.0, 37.0, 35.5,
            ],
            // October: autumn, heating starts
            [
                37.0, 35.5, 34.8, 34.0, 34.8, 36.3, 40.0, 44.4, 47.4, 45.9, 44.4, 42.9, 41.4, 40.0,
                38.5, 40.0, 44.4, 51.8, 54.8, 51.8, 47.4, 42.9, 40.0, 38.5,
            ],
            // November: late autumn, rising demand
            [
                40.0, 38.5, 37.7, 37.0, 37.7, 39.2, 42.9, 47.4, 50.3, 48.8, 47.4, 45.9, 44.4, 42.9,
                41.4, 42.9, 47.4, 54.8, 57.7, 54.8, 50.3, 45.9, 42.9, 41.4,
            ],
            // December: winter peak, highest demand
            [
                42.9, 41.4, 40.7, 40.0, 40.7, 42.2, 45.9, 50.3, 53.3, 51.8, 50.3, 48.8, 47.4, 45.9,
                44.4, 45.9, 50.3, 57.7, 60.7, 57.7, 53.3, 48.8, 45.9, 44.4,
            ],
        ],
    ),
    // Germany (eu-central-1): coal + renewables, strong seasonal variance.
    // Winter: more coal, less solar. Summer: more solar, less coal.
    // Shape: ENTSO-E, Fraunhofer ISE energy-charts.info. The original
    // 2022-vintage level (grand mean ~431, coal-crisis Germany) was
    // rescaled to the Electricity Maps 2024 consumption-based level
    // (341), resolving the historical divergence from the annual table.
    // Annual mean in CARBON_TABLE: 338. Grand mean target: ~341.
    (
        "eu-central-1",
        [
            // January: peak coal, low solar, high demand
            [
                332.2, 324.3, 320.4, 316.4, 324.3, 348.1, 395.5, 419.3, 427.2, 415.3, 403.4, 391.6,
                379.7, 367.8, 371.8, 383.7, 411.3, 443.0, 454.8, 435.1, 411.3, 387.6, 363.9, 348.1,
            ],
            // February: still high coal, slight improvement
            [
                324.3, 316.4, 312.5, 308.5, 316.4, 340.1, 387.6, 411.3, 419.3, 407.4, 395.5, 383.7,
                371.8, 359.9, 363.9, 375.7, 403.4, 435.1, 446.9, 427.2, 403.4, 379.7, 356.0, 340.1,
            ],
            // March: transition, more wind, some solar
            [
                308.5, 300.6, 296.6, 292.7, 300.6, 324.3, 367.8, 391.6, 395.5, 379.7, 363.9, 348.1,
                332.2, 320.4, 324.3, 340.1, 371.8, 411.3, 423.2, 403.4, 379.7, 356.0, 332.2, 320.4,
            ],
            // April: spring, growing solar midday dip
            [
                292.7, 284.8, 280.8, 276.9, 284.8, 304.6, 348.1, 371.8, 375.7, 359.9, 344.1, 328.3,
                312.5, 300.6, 304.6, 320.4, 352.0, 391.6, 403.4, 383.7, 359.9, 336.2, 312.5, 300.6,
            ],
            // May: significant solar, reduced coal
            [
                276.9, 269.0, 265.0, 261.0, 269.0, 288.7, 332.2, 352.0, 352.0, 332.2, 312.5, 296.6,
                284.8, 276.9, 284.8, 304.6, 336.2, 375.7, 387.6, 367.8, 344.1, 316.4, 292.7, 284.8,
            ],
            // June: peak solar, lowest coal use
            [
                269.0, 261.0, 257.1, 253.1, 261.0, 280.8, 320.4, 340.1, 336.2, 316.4, 296.6, 280.8,
                269.0, 261.0, 269.0, 288.7, 324.3, 363.9, 379.7, 359.9, 336.2, 308.5, 284.8, 276.9,
            ],
            // July: continued solar, slightly warmer (AC)
            [
                272.9, 265.0, 261.0, 257.1, 265.0, 284.8, 324.3, 344.1, 340.1, 320.4, 300.6, 284.8,
                272.9, 265.0, 272.9, 292.7, 328.3, 367.8, 383.7, 363.9, 340.1, 312.5, 288.7, 280.8,
            ],
            // August: similar to July, slightly rising
            [
                276.9, 269.0, 265.0, 261.0, 269.0, 288.7, 328.3, 348.1, 348.1, 328.3, 308.5, 292.7,
                280.8, 272.9, 280.8, 300.6, 332.2, 371.8, 387.6, 367.8, 344.1, 316.4, 292.7, 284.8,
            ],
            // September: solar declining, coal increasing
            [
                292.7, 284.8, 280.8, 276.9, 284.8, 304.6, 348.1, 367.8, 371.8, 356.0, 340.1, 328.3,
                316.4, 308.5, 312.5, 328.3, 359.9, 395.5, 407.4, 387.6, 363.9, 340.1, 316.4, 304.6,
            ],
            // October: autumn, more gas+coal
            [
                308.5, 300.6, 296.6, 292.7, 300.6, 320.4, 363.9, 387.6, 391.6, 375.7, 359.9, 348.1,
                336.2, 328.3, 332.2, 348.1, 375.7, 411.3, 423.2, 403.4, 379.7, 356.0, 332.2, 320.4,
            ],
            // November: late autumn, rising coal
            [
                320.4, 312.5, 308.5, 304.6, 312.5, 336.2, 379.7, 403.4, 411.3, 399.5, 387.6, 375.7,
                363.9, 352.0, 356.0, 367.8, 395.5, 427.2, 439.0, 419.3, 395.5, 371.8, 348.1, 332.2,
            ],
            // December: peak winter, highest coal
            [
                332.2, 324.3, 320.4, 316.4, 324.3, 348.1, 395.5, 419.3, 427.2, 415.3, 403.4, 391.6,
                379.7, 367.8, 371.8, 383.7, 411.3, 443.0, 454.8, 435.1, 411.3, 387.6, 363.9, 348.1,
            ],
        ],
    ),
    // UK (eu-west-2): wind + gas. Winter gas heating, summer more wind.
    // Sources: National Grid ESO Carbon Intensity API, ENTSO-E (2022-2024).
    // Annual mean in CARBON_TABLE: 217.4. Grand mean target: ~217.4.
    (
        "eu-west-2",
        [
            // January: high gas heating
            [
                202.3, 192.9, 188.2, 183.5, 192.9, 216.5, 254.1, 282.3, 277.6, 263.5, 249.4, 240.0,
                230.6, 221.2, 225.9, 240.0, 268.2, 296.5, 305.9, 287.0, 263.5, 244.7, 225.9, 211.8,
            ],
            // February: still winter
            [
                197.6, 188.2, 183.5, 178.8, 188.2, 211.8, 249.4, 277.6, 272.9, 258.8, 244.7, 235.3,
                225.9, 216.5, 221.2, 235.3, 263.5, 291.7, 301.2, 282.3, 258.8, 240.0, 221.2, 207.0,
            ],
            // March: transition, improving wind
            [
                188.2, 178.8, 174.1, 169.4, 178.8, 202.3, 235.3, 263.5, 258.8, 244.7, 230.6, 221.2,
                211.8, 202.3, 207.0, 221.2, 249.4, 277.6, 287.0, 268.2, 244.7, 225.9, 207.0, 197.6,
            ],
            // April: spring
            [
                178.8, 169.4, 164.7, 160.0, 169.4, 192.9, 225.9, 254.1, 249.4, 235.3, 221.2, 211.8,
                202.3, 192.9, 197.6, 211.8, 240.0, 268.2, 277.6, 258.8, 235.3, 216.5, 197.6, 188.2,
            ],
            // May: more wind and solar
            [
                169.4, 160.0, 155.3, 150.6, 160.0, 183.5, 214.6, 240.0, 235.3, 221.2, 207.0, 197.6,
                188.2, 178.8, 183.5, 197.6, 225.9, 254.1, 263.5, 244.7, 221.2, 202.3, 183.5, 174.1,
            ],
            // June: summer, good wind
            [
                161.9, 152.5, 147.8, 143.1, 152.5, 174.1, 205.2, 230.6, 225.9, 211.8, 197.6, 188.2,
                178.8, 169.4, 174.1, 188.2, 216.5, 244.7, 254.1, 235.3, 211.8, 192.9, 174.1, 166.6,
            ],
            // July: summer, variable wind
            [
                164.7, 155.3, 150.6, 145.9, 155.3, 176.9, 208.9, 235.3, 230.6, 216.5, 202.3, 192.9,
                183.5, 174.1, 178.8, 192.9, 221.2, 249.4, 258.8, 240.0, 216.5, 197.6, 178.8, 169.4,
            ],
            // August: late summer
            [
                167.5, 158.1, 153.4, 148.7, 158.1, 180.7, 211.8, 238.1, 233.4, 219.3, 205.2, 195.8,
                186.3, 176.9, 181.6, 195.8, 224.0, 252.2, 261.6, 242.8, 219.3, 200.5, 181.6, 172.2,
            ],
            // September: autumn transition
            [
                178.8, 169.4, 164.7, 160.0, 169.4, 192.9, 225.9, 252.2, 247.5, 233.4, 219.3, 209.9,
                200.5, 191.0, 195.8, 209.9, 238.1, 266.3, 275.7, 256.9, 233.4, 214.6, 195.8, 186.3,
            ],
            // October: autumn, more gas
            [
                188.2, 178.8, 174.1, 169.4, 178.8, 202.3, 235.3, 263.5, 258.8, 244.7, 230.6, 221.2,
                211.8, 202.3, 207.0, 221.2, 249.4, 277.6, 287.0, 268.2, 244.7, 225.9, 207.0, 197.6,
            ],
            // November: late autumn, high gas
            [
                197.6, 188.2, 183.5, 178.8, 188.2, 211.8, 244.7, 272.9, 268.2, 254.1, 240.0, 230.6,
                221.2, 211.8, 216.5, 230.6, 258.8, 287.0, 296.5, 277.6, 254.1, 235.3, 216.5, 202.3,
            ],
            // December: winter peak
            [
                202.3, 192.9, 188.2, 183.5, 192.9, 216.5, 254.1, 282.3, 277.6, 263.5, 249.4, 240.0,
                230.6, 221.2, 225.9, 240.0, 268.2, 296.5, 305.9, 287.0, 263.5, 244.7, 225.9, 211.8,
            ],
        ],
    ),
    // US-East (us-east-1, Virginia, PJM territory): gas + coal + nuclear.
    // Summer AC load and winter heating both push intensity above spring/fall.
    // Sources: EIA Open Data API, PJM Interconnection (2022-2024 averages).
    // Annual mean in CARBON_TABLE: 379. Grand mean target: ~379.
    (
        "us-east-1",
        [
            // January: winter heating (gas)
            [
                360.0, 345.0, 335.0, 330.0, 340.0, 360.0, 385.0, 405.0, 420.0, 430.0, 435.0, 440.0,
                440.0, 445.0, 450.0, 445.0, 435.0, 420.0, 405.0, 390.0, 380.0, 375.0, 370.0, 365.0,
            ],
            // February: still winter
            [
                355.0, 340.0, 330.0, 325.0, 335.0, 355.0, 380.0, 400.0, 415.0, 425.0, 430.0, 435.0,
                435.0, 440.0, 445.0, 440.0, 430.0, 415.0, 400.0, 385.0, 375.0, 370.0, 365.0, 360.0,
            ],
            // March: transition
            [
                335.0, 320.0, 310.0, 305.0, 315.0, 335.0, 360.0, 380.0, 395.0, 405.0, 410.0, 415.0,
                415.0, 420.0, 425.0, 420.0, 410.0, 395.0, 380.0, 365.0, 355.0, 350.0, 345.0, 340.0,
            ],
            // April: spring, mild
            [
                320.0, 305.0, 295.0, 290.0, 300.0, 320.0, 345.0, 365.0, 380.0, 390.0, 395.0, 400.0,
                400.0, 405.0, 410.0, 405.0, 395.0, 380.0, 365.0, 350.0, 340.0, 335.0, 330.0, 325.0,
            ],
            // May: late spring
            [
                315.0, 300.0, 290.0, 285.0, 295.0, 315.0, 340.0, 360.0, 375.0, 385.0, 390.0, 395.0,
                395.0, 400.0, 405.0, 400.0, 390.0, 375.0, 360.0, 345.0, 335.0, 330.0, 325.0, 320.0,
            ],
            // June: summer AC begins
            [
                345.0, 330.0, 320.0, 315.0, 325.0, 345.0, 370.0, 390.0, 405.0, 420.0, 430.0, 435.0,
                435.0, 440.0, 445.0, 440.0, 430.0, 415.0, 400.0, 385.0, 375.0, 370.0, 360.0, 350.0,
            ],
            // July: peak AC load
            [
                355.0, 340.0, 330.0, 325.0, 335.0, 355.0, 380.0, 400.0, 415.0, 430.0, 440.0, 445.0,
                445.0, 450.0, 455.0, 450.0, 440.0, 425.0, 410.0, 395.0, 385.0, 380.0, 370.0, 360.0,
            ],
            // August: continued AC
            [
                350.0, 335.0, 325.0, 320.0, 330.0, 350.0, 375.0, 395.0, 410.0, 425.0, 435.0, 440.0,
                440.0, 445.0, 450.0, 445.0, 435.0, 420.0, 405.0, 390.0, 380.0, 375.0, 365.0, 355.0,
            ],
            // September: AC winding down
            [
                340.0, 325.0, 315.0, 310.0, 320.0, 340.0, 365.0, 385.0, 400.0, 412.0, 418.0, 422.0,
                422.0, 426.0, 430.0, 426.0, 418.0, 402.0, 388.0, 375.0, 365.0, 358.0, 350.0, 345.0,
            ],
            // October: autumn
            [
                330.0, 315.0, 305.0, 300.0, 310.0, 330.0, 355.0, 375.0, 390.0, 400.0, 405.0, 410.0,
                410.0, 415.0, 420.0, 415.0, 405.0, 390.0, 375.0, 360.0, 350.0, 345.0, 340.0, 335.0,
            ],
            // November: late autumn, heating starts
            [
                345.0, 330.0, 320.0, 315.0, 325.0, 345.0, 370.0, 390.0, 405.0, 415.0, 420.0, 425.0,
                425.0, 430.0, 435.0, 430.0, 420.0, 405.0, 390.0, 375.0, 365.0, 360.0, 355.0, 350.0,
            ],
            // December: winter peak
            [
                360.0, 345.0, 335.0, 330.0, 340.0, 360.0, 385.0, 405.0, 420.0, 430.0, 435.0, 440.0,
                440.0, 445.0, 450.0, 445.0, 435.0, 420.0, 405.0, 390.0, 380.0, 375.0, 370.0, 365.0,
            ],
        ],
    ),
];

// ---------------------------------------------------------------------------
// Flat-year profiles (24 UTC values, same all year).
// ---------------------------------------------------------------------------

/// Flat-year hourly profiles: 24 UTC values per region.
/// Each entry's arithmetic mean must be within +/-5% of the corresponding
/// annual value in `CARBON_TABLE` (validated by tests).
pub(crate) static FLAT_YEAR_PROFILES: &[(&str, [f64; 24])] = &[
    // ── 4a: ENTSO-E Europe ─────────────────────────────────────────

    // Ireland (eu-west-1): wind-heavy grid, flatter diurnal profile.
    // Wind availability is relatively constant across the day but demand
    // still creates an evening peak when gas marginal plants run.
    // Source: ENTSO-E, EirGrid (2022-2024). Annual: 255.9.
    (
        "eu-west-1",
        [
            233.4, 224.8, 220.5, 216.1, 224.8, 237.7, 255.0, 272.3, 276.6, 272.3, 263.7, 255.0,
            250.7, 246.4, 250.7, 259.4, 272.3, 289.6, 293.9, 281.0, 268.0, 255.0, 242.1, 237.7,
        ],
    ),
    // Netherlands (eu-west-4): gas + wind. Gas sets the marginal rate.
    // Evening peak from gas generation for heating/cooking.
    // Source: ENTSO-E, TenneT (2022-2024). Annual: 253.6.
    (
        "eu-west-4",
        [
            232.0, 224.2, 220.4, 216.5, 224.2, 239.7, 259.0, 274.5, 278.3, 270.6, 262.9, 255.1,
            247.4, 239.7, 243.5, 255.1, 270.6, 286.1, 289.9, 278.3, 266.7, 255.1, 243.5, 235.8,
        ],
    ),
    // Sweden (eu-north-1): hydro + nuclear dominated. Very clean, nearly flat.
    // Slight bump during business hours from marginal fossil imports.
    // Source: ENTSO-E, Svenska Kraftnat (2022-2024). Annual: 35.4.
    (
        "eu-north-1",
        [
            31.0, 31.0, 31.0, 31.0, 31.0, 31.0, 35.4, 39.8, 39.8, 39.8, 39.8, 35.4, 35.4, 35.4,
            35.4, 35.4, 39.8, 39.8, 39.8, 39.8, 35.4, 35.4, 31.0, 31.0,
        ],
    ),
    // Belgium (europe-west1): nuclear + gas. Moderate diurnal variation.
    // Nuclear provides ~50% baseload; gas fills the rest.
    // Shape: ENTSO-E, Elia. Level rescaled to the Electricity Maps
    // 2023-2024 consumption-based mean (165). Annual: 109.3.
    (
        "europe-west1",
        [
            97.4, 93.9, 91.6, 89.9, 93.9, 101.5, 113.1, 121.8, 122.9, 118.8, 114.8, 110.2, 107.2,
            104.4, 105.5, 110.2, 118.8, 126.4, 128.7, 122.9, 116.0, 110.2, 103.2, 99.8,
        ],
    ),
    // Finland (europe-north1): nuclear + hydro + wind. Very clean, nearly flat.
    // Source: ENTSO-E, Fingrid (2022-2024). Annual: 57.5.
    (
        "europe-north1",
        [
            50.3, 50.3, 50.3, 50.3, 50.3, 50.3, 57.5, 64.7, 64.7, 64.7, 64.7, 57.5, 57.5, 57.5,
            57.5, 57.5, 64.7, 64.7, 64.7, 64.7, 57.5, 57.5, 50.3, 50.3,
        ],
    ),
    // Italy (eu-south-1 / europe-west8): gas + solar. Midday solar dip.
    // Source: ENTSO-E, Terna (2022-2024). Annual: 284.6.
    (
        "eu-south-1",
        [
            265.4, 257.7, 253.8, 250.0, 257.7, 269.2, 288.4, 303.8, 300.0, 284.6, 269.2, 261.5,
            257.7, 261.5, 269.2, 280.8, 296.1, 311.5, 319.2, 307.7, 296.1, 284.6, 275.4, 269.2,
        ],
    ),
    // Spain (europe-southwest1): solar + wind. Strong midday solar dip.
    // Source: ENTSO-E, REE (2022-2024). Annual: 153.6.
    (
        "europe-southwest1",
        [
            149.8, 144.4, 141.3, 138.2, 141.3, 147.5, 161.3, 172.8, 165.1, 149.8, 136.7, 130.6,
            129.0, 132.1, 138.2, 149.8, 165.1, 180.5, 184.3, 175.1, 165.1, 157.4, 153.6, 152.1,
        ],
    ),
    // Poland (europe-central2): coal-heavy. Relatively flat but high.
    // Slight evening peak from increased demand on coal baseload.
    // Source: ENTSO-E, PSE (2022-2024). Annual: 590.8.
    (
        "europe-central2",
        [
            557.0, 548.6, 544.4, 540.2, 548.6, 565.5, 595.0, 616.1, 620.3, 611.9, 603.5, 595.0,
            586.6, 582.4, 586.6, 599.2, 616.1, 633.0, 637.2, 624.6, 607.7, 595.0, 578.1, 565.5,
        ],
    ),
    // Norway (europe-north2): hydro-dominated. Very clean and flat.
    // Source: ENTSO-E, Statnett (2022-2024). Annual: 28.1.
    (
        "europe-north2",
        [
            24.1, 24.1, 24.1, 24.1, 24.1, 28.1, 28.1, 32.1, 32.1, 32.1, 32.1, 28.1, 28.1, 28.1,
            28.1, 28.1, 32.1, 32.1, 32.1, 32.1, 28.1, 28.1, 28.1, 24.1,
        ],
    ),
    // ── 4b: US regions (EIA Open Data) ─────────────────────────────

    // Ohio (us-east-2, PJM/MISO): coal + gas + nuclear. Evening peak.
    // Source: EIA Open Data, PJM (2022-2024). Annual: 410.
    (
        "us-east-2",
        [
            375.0, 360.0, 350.0, 345.0, 355.0, 375.0, 400.0, 425.0, 435.0, 440.0, 445.0, 448.0,
            448.0, 450.0, 452.0, 448.0, 440.0, 425.0, 412.0, 400.0, 390.0, 385.0, 382.0, 378.0,
        ],
    ),
    // N. California (us-west-1, CAISO): solar + gas. Duck curve shape.
    // Source: EIA Open Data, CAISO (2022-2024). Annual: 200.
    // CAISO is UTC-8. Key hours in UTC:
    //   UTC 2-5  (local 6-9pm):  evening gas ramp, highest intensity
    //   UTC 8-14 (local 12-6am): night baseload, moderate
    //   UTC 15-17 (local 7-9am): morning ramp
    //   UTC 18-22 (local 10am-2pm): solar peak, lowest intensity
    //   UTC 23-1 (local 3-5pm): solar decline, gas starting
    (
        "us-west-1",
        [
            215.0, 225.0, 238.0, 245.0, 242.0, 230.0, 218.0, 210.0, 200.0, 195.0, 192.0, 190.0,
            188.0, 188.0, 190.0, 195.0, 188.0, 175.0, 162.0, 158.0, 160.0, 168.0, 185.0, 200.0,
        ],
    ),
    // Oregon (us-west-2, BPA territory): hydro-heavy. Clean and flat.
    // Source: EIA Open Data, BPA (2022-2024). Annual: 89.
    (
        "us-west-2",
        [
            82.0, 80.0, 78.0, 77.0, 80.0, 84.0, 90.0, 96.0, 98.0, 97.0, 95.0, 92.0, 90.0, 88.0,
            87.0, 89.0, 93.0, 97.0, 98.0, 96.0, 93.0, 90.0, 86.0, 84.0,
        ],
    ),
    // ── 4c: Canada + Australia ─────────────────────────────────────

    // Canada / Quebec (ca-central-1): Hydro-Quebec, ~95% hydro.
    // Extremely clean and flat. Pedagogical contrast.
    // Source: Hydro-Quebec annual report (2022-2024). Annual: 13.
    (
        "ca-central-1",
        [
            12.0, 12.0, 12.0, 12.0, 12.0, 12.0, 13.0, 14.0, 14.0, 14.0, 14.0, 13.0, 13.0, 13.0,
            13.0, 13.0, 14.0, 14.0, 14.0, 14.0, 13.0, 13.0, 12.0, 12.0,
        ],
    ),
    // Australia / Sydney (ap-southeast-2): AEMO NEM, coal + solar.
    // High coal baseload with midday solar dip. Australia is UTC+10/11,
    // so local midday is ~01-03 UTC.
    // Source: AEMO NEM, OpenNEM (2022-2024). Annual: 550.
    (
        "ap-southeast-2",
        [
            530.0, 520.0, 510.0, 505.0, 515.0, 530.0, 545.0, 555.0, 560.0, 565.0, 570.0, 575.0,
            575.0, 572.0, 568.0, 562.0, 555.0, 548.0, 545.0, 540.0, 542.0, 545.0, 540.0, 535.0,
        ],
    ),
    // ── 4d: Asia + South America (best-effort) ─────────────────────

    // Japan / Tokyo (ap-northeast-1): LNG + nuclear restart + solar.
    // Moderate evening peak. UTC+9, so local peak (18:00) = 09:00 UTC.
    // Estimated from fuel mix, no hourly data source.
    // Source: estimated from TEPCO fuel mix composition (2023-2024). Annual: 462.
    (
        "ap-northeast-1",
        [
            445.0, 440.0, 435.0, 430.0, 435.0, 445.0, 460.0, 475.0, 480.0, 485.0, 480.0, 475.0,
            470.0, 465.0, 460.0, 455.0, 458.0, 462.0, 468.0, 472.0, 470.0, 465.0, 458.0, 450.0,
        ],
    ),
    // Singapore (ap-southeast-1): gas-dominated, nearly flat.
    // ~95% natural gas, minimal renewables. Very flat profile.
    // Estimated from fuel mix, no hourly data source.
    // Source: estimated from EMA Singapore fuel mix (2023-2024). Annual: 497.1.
    (
        "ap-southeast-1",
        [
            481.3, 477.6, 475.2, 472.7, 477.6, 484.9, 497.1, 509.3, 511.7, 509.3, 505.6, 502.0,
            499.5, 497.1, 494.7, 497.1, 502.0, 509.3, 511.7, 509.3, 505.6, 499.5, 489.8, 484.9,
        ],
    ),
    // India / Mumbai (ap-south-1): coal-heavy, high intensity.
    // Coal provides ~70% of electricity. Mild evening peak.
    // Estimated from fuel mix, no hourly data source.
    // Source: estimated from POSOCO/CEA fuel mix (2023-2024). Annual: 670.5.
    (
        "ap-south-1",
        [
            644.0, 636.4, 632.6, 629.8, 636.4, 651.6, 672.4, 689.4, 693.2, 689.4, 681.9, 674.3,
            670.5, 667.7, 670.5, 677.1, 686.6, 698.9, 702.7, 696.1, 686.6, 677.1, 661.0, 651.6,
        ],
    ),
    // Brazil / Sao Paulo (sa-east-1): hydro-heavy, clean. Nearly flat.
    // ~60% hydro, ~20% wind/solar. Slight evening peak from thermal backup.
    // Estimated from fuel mix, no hourly data source.
    // Shape: estimated from ONS Brazil fuel mix. Level rescaled to the
    // Electricity Maps BR-CS 2023-2024 consumption-based mean (96).
    // Annual: 96.
    (
        "sa-east-1",
        [
            89.8, 86.7, 85.2, 83.6, 86.7, 89.8, 96.0, 102.2, 103.7, 102.2, 100.6, 99.1, 97.5, 96.0,
            94.5, 96.0, 99.1, 103.7, 105.3, 103.7, 100.6, 97.5, 92.9, 91.4,
        ],
    ),
];

// ---------------------------------------------------------------------------
// Aliases: (alias_key, canonical_key). Both must be lowercase.
// The LazyLock init inserts each alias as a separate HashMap entry pointing
// to the same static profile data as the canonical key.
// ---------------------------------------------------------------------------

/// Alias table mapping cloud-provider and ISO country-code keys to canonical
/// profile keys. Only covers regions that have an entry in either
/// `FLAT_YEAR_PROFILES` or `MONTHLY_PROFILES`.
pub(crate) static PROFILE_ALIASES: &[(&str, &str)] = &[
    // France
    ("fr", "eu-west-3"),
    ("francecentral", "eu-west-3"),
    ("europe-west9", "eu-west-3"),
    // Germany
    ("de", "eu-central-1"),
    // UK
    ("gb", "eu-west-2"),
    ("uk", "eu-west-2"),
    ("uksouth", "eu-west-2"),
    // Ireland
    ("ie", "eu-west-1"),
    ("northeurope", "eu-west-1"),
    // Netherlands
    ("nl", "eu-west-4"),
    ("westeurope", "eu-west-4"),
    ("europe-west4", "eu-west-4"),
    // Sweden
    ("se", "eu-north-1"),
    // Belgium
    ("be", "europe-west1"),
    // Finland
    ("fi", "europe-north1"),
    // Italy
    ("it", "eu-south-1"),
    ("europe-west8", "eu-south-1"),
    // Spain
    ("es", "europe-southwest1"),
    // Poland
    ("pl", "europe-central2"),
    // Norway
    ("no", "europe-north2"),
    // US-East (Virginia)
    ("us", "us-east-1"),
    ("eastus", "us-east-1"),
    ("us-east1", "us-east-1"),
    // US-West (Oregon, Azure)
    ("westus2", "us-west-2"),
    ("us-west1", "us-west-1"),
    // Canada
    ("ca", "ca-central-1"),
    // Australia
    ("au", "ap-southeast-2"),
    // Japan
    ("jp", "ap-northeast-1"),
    ("asia-northeast1", "ap-northeast-1"),
    // Singapore
    ("sg", "ap-southeast-1"),
    // India
    ("in", "ap-south-1"),
    // Brazil
    ("br", "sa-east-1"),
];

#[cfg(test)]
mod tests {
    use super::*;

    // ── HourlyProfileRef intensity_at ──────────────────────────────

    #[test]
    fn profile_ref_flat_year_ignores_month() {
        let data: &'static [f64; 24] = &[
            10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0, 20.0, 21.0, 22.0, 23.0,
            24.0, 25.0, 26.0, 27.0, 28.0, 29.0, 30.0, 31.0, 32.0, 33.0,
        ];
        let pr = HourlyProfileRef::FlatYear(data);
        assert!((pr.intensity_at(0, None) - 10.0).abs() < f64::EPSILON);
        assert!((pr.intensity_at(0, Some(6)) - 10.0).abs() < f64::EPSILON);
        assert!((pr.intensity_at(23, Some(11)) - 33.0).abs() < f64::EPSILON);
    }

    /// Minimal static monthly table shared by tests below: every month
    /// is 100 except July (index 6) which is 200. Each month has a
    /// distinguishing first-hour value via the full-row replacement.
    static TEST_MONTHLY_TABLE: [[f64; 24]; 12] = {
        let mut t = [[100.0; 24]; 12];
        t[6] = [200.0; 24];
        t
    };

    #[test]
    fn profile_ref_monthly_uses_month() {
        let pr = HourlyProfileRef::Monthly(&TEST_MONTHLY_TABLE);
        assert!((pr.intensity_at(0, Some(0)) - 100.0).abs() < f64::EPSILON);
        assert!((pr.intensity_at(0, Some(6)) - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn profile_ref_monthly_none_month_averages() {
        // When month is None, should average across all 12 months.
        let pr = HourlyProfileRef::Monthly(&TEST_MONTHLY_TABLE);
        let expected = (11.0 * 100.0 + 200.0) / 12.0;
        assert!((pr.intensity_at(0, None) - expected).abs() < 0.01);
    }

    #[test]
    fn profile_ref_is_monthly() {
        static DATA: [f64; 24] = [0.0; 24];
        static TABLE: [[f64; 24]; 12] = [[0.0; 24]; 12];
        assert!(!HourlyProfileRef::FlatYear(&DATA).is_monthly());
        assert!(HourlyProfileRef::Monthly(&TABLE).is_monthly());
    }

    // ── HourlyProfile mean ─────────────────────────────────────────

    #[test]
    fn hourly_profile_flat_year_mean() {
        let profile = HourlyProfile::FlatYear([50.0; 24]);
        assert!((profile.mean() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn hourly_profile_monthly_mean() {
        let mut months = [[100.0; 24]; 12];
        months[6] = [200.0; 24];
        let profile = HourlyProfile::Monthly(Box::new(months));
        let expected = (11.0 * 100.0 + 200.0) / 12.0;
        assert!((profile.mean() - expected).abs() < 0.01);
    }

    // ── Static data integrity ──────────────────────────────────────

    #[test]
    fn all_flat_year_profiles_have_24_values() {
        for (key, profile) in FLAT_YEAR_PROFILES {
            assert_eq!(
                profile.len(),
                24,
                "flat year profile for {key} must have 24 values"
            );
        }
    }

    #[test]
    fn all_monthly_profiles_have_12x24_values() {
        for (key, profile) in MONTHLY_PROFILES {
            assert_eq!(
                profile.len(),
                12,
                "monthly profile for {key} must have 12 months"
            );
            for (month_idx, month) in profile.iter().enumerate() {
                assert_eq!(
                    month.len(),
                    24,
                    "monthly profile for {key} month {month_idx} must have 24 values"
                );
            }
        }
    }

    #[test]
    fn all_profile_values_are_finite_and_non_negative() {
        for (key, profile) in FLAT_YEAR_PROFILES {
            for (h, &val) in profile.iter().enumerate() {
                assert!(
                    val.is_finite() && val >= 0.0,
                    "flat year {key} hour {h}: invalid value {val}"
                );
            }
        }
        for (key, months) in MONTHLY_PROFILES {
            for (m, month) in months.iter().enumerate() {
                for (h, &val) in month.iter().enumerate() {
                    assert!(
                        val.is_finite() && val >= 0.0,
                        "monthly {key} month {m} hour {h}: invalid value {val}"
                    );
                }
            }
        }
    }

    #[test]
    fn no_duplicate_keys_in_profiles() {
        let mut keys = std::collections::HashSet::new();
        for (key, _) in FLAT_YEAR_PROFILES {
            assert!(
                keys.insert(*key),
                "duplicate key in FLAT_YEAR_PROFILES: {key}"
            );
        }
        for (key, _) in MONTHLY_PROFILES {
            assert!(
                keys.insert(*key),
                "duplicate key in MONTHLY_PROFILES: {key}"
            );
        }
    }

    #[test]
    fn all_aliases_point_to_existing_canonical_keys() {
        let mut canonical_keys = std::collections::HashSet::new();
        for (key, _) in FLAT_YEAR_PROFILES {
            canonical_keys.insert(*key);
        }
        for (key, _) in MONTHLY_PROFILES {
            canonical_keys.insert(*key);
        }
        for &(alias, canonical) in PROFILE_ALIASES {
            assert!(
                canonical_keys.contains(canonical),
                "alias '{alias}' points to non-existent canonical key '{canonical}'"
            );
        }
    }

    #[test]
    fn no_duplicate_alias_keys() {
        let mut seen = std::collections::HashSet::new();
        for &(alias, _) in PROFILE_ALIASES {
            assert!(
                seen.insert(alias),
                "duplicate alias key in PROFILE_ALIASES: {alias}"
            );
        }
    }

    #[test]
    fn no_alias_shadows_canonical_key() {
        let mut canonical_keys = std::collections::HashSet::new();
        for (key, _) in FLAT_YEAR_PROFILES {
            canonical_keys.insert(*key);
        }
        for (key, _) in MONTHLY_PROFILES {
            canonical_keys.insert(*key);
        }
        for &(alias, _) in PROFILE_ALIASES {
            assert!(
                !canonical_keys.contains(alias),
                "alias '{alias}' shadows a canonical profile key"
            );
        }
    }
}
