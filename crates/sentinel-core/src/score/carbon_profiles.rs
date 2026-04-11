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
/// annual value (except `eu-central-1` which has a known ~31% divergence).
pub(crate) static MONTHLY_PROFILES: &[(&str, [[f64; 24]; 12])] = &[
    // France (eu-west-3): nuclear baseload, seasonal gas peaking.
    // Winter: higher intensity from gas peaking for heating demand.
    // Summer: lower intensity, nuclear provides larger share.
    // Sources: ENTSO-E Transparency Platform, RTE eCO2mix (2022-2024 averages).
    // Annual mean in CARBON_TABLE: 56.0. Grand mean target: ~55.
    (
        "eu-west-3",
        [
            // January: winter peak, gas + imports from DE
            [
                58.0, 56.0, 55.0, 54.0, 55.0, 57.0, 62.0, 68.0, 72.0, 70.0, 68.0, 66.0, 64.0, 62.0,
                60.0, 62.0, 68.0, 78.0, 82.0, 78.0, 72.0, 66.0, 62.0, 60.0,
            ],
            // February: still winter, slightly less demand
            [
                56.0, 54.0, 53.0, 52.0, 53.0, 55.0, 60.0, 66.0, 70.0, 68.0, 66.0, 64.0, 62.0, 60.0,
                58.0, 60.0, 66.0, 76.0, 80.0, 76.0, 70.0, 64.0, 60.0, 58.0,
            ],
            // March: transition, heating demand drops
            [
                52.0, 50.0, 49.0, 48.0, 49.0, 51.0, 56.0, 62.0, 66.0, 64.0, 62.0, 60.0, 58.0, 56.0,
                54.0, 56.0, 62.0, 72.0, 76.0, 72.0, 66.0, 60.0, 56.0, 54.0,
            ],
            // April: spring, mild weather
            [
                48.0, 46.0, 45.0, 44.0, 45.0, 47.0, 52.0, 58.0, 62.0, 60.0, 58.0, 56.0, 54.0, 52.0,
                50.0, 52.0, 58.0, 68.0, 72.0, 68.0, 62.0, 56.0, 52.0, 50.0,
            ],
            // May: solar contribution increases
            [
                44.0, 42.0, 41.0, 40.0, 41.0, 43.0, 48.0, 54.0, 56.0, 54.0, 52.0, 50.0, 48.0, 46.0,
                44.0, 46.0, 52.0, 62.0, 66.0, 62.0, 56.0, 50.0, 46.0, 44.0,
            ],
            // June: summer, low demand, high nuclear share
            [
                40.0, 38.0, 37.0, 36.0, 37.0, 39.0, 44.0, 50.0, 52.0, 50.0, 48.0, 46.0, 44.0, 42.0,
                40.0, 42.0, 48.0, 56.0, 60.0, 56.0, 50.0, 44.0, 42.0, 40.0,
            ],
            // July: summer, AC load partially offsets
            [
                42.0, 40.0, 39.0, 38.0, 39.0, 41.0, 46.0, 52.0, 54.0, 52.0, 50.0, 48.0, 46.0, 44.0,
                42.0, 44.0, 50.0, 58.0, 62.0, 58.0, 52.0, 46.0, 44.0, 42.0,
            ],
            // August: similar to July
            [
                42.0, 40.0, 39.0, 38.0, 39.0, 41.0, 46.0, 52.0, 54.0, 52.0, 50.0, 48.0, 46.0, 44.0,
                42.0, 44.0, 50.0, 58.0, 62.0, 58.0, 52.0, 46.0, 44.0, 42.0,
            ],
            // September: transition back
            [
                46.0, 44.0, 43.0, 42.0, 43.0, 45.0, 50.0, 56.0, 60.0, 58.0, 56.0, 54.0, 52.0, 50.0,
                48.0, 50.0, 56.0, 66.0, 70.0, 66.0, 60.0, 54.0, 50.0, 48.0,
            ],
            // October: autumn, heating starts
            [
                50.0, 48.0, 47.0, 46.0, 47.0, 49.0, 54.0, 60.0, 64.0, 62.0, 60.0, 58.0, 56.0, 54.0,
                52.0, 54.0, 60.0, 70.0, 74.0, 70.0, 64.0, 58.0, 54.0, 52.0,
            ],
            // November: late autumn, rising demand
            [
                54.0, 52.0, 51.0, 50.0, 51.0, 53.0, 58.0, 64.0, 68.0, 66.0, 64.0, 62.0, 60.0, 58.0,
                56.0, 58.0, 64.0, 74.0, 78.0, 74.0, 68.0, 62.0, 58.0, 56.0,
            ],
            // December: winter peak, highest demand
            [
                58.0, 56.0, 55.0, 54.0, 55.0, 57.0, 62.0, 68.0, 72.0, 70.0, 68.0, 66.0, 64.0, 62.0,
                60.0, 62.0, 68.0, 78.0, 82.0, 78.0, 72.0, 66.0, 62.0, 60.0,
            ],
        ],
    ),
    // Germany (eu-central-1): coal + renewables, strong seasonal variance.
    // Winter: more coal, less solar. Summer: more solar, less coal.
    // Sources: ENTSO-E, Fraunhofer ISE energy-charts.info (2022-2024).
    // Annual mean in CARBON_TABLE: 338. Known divergence: grand mean ~442.
    (
        "eu-central-1",
        [
            // January: peak coal, low solar, high demand
            [
                420.0, 410.0, 405.0, 400.0, 410.0, 440.0, 500.0, 530.0, 540.0, 525.0, 510.0, 495.0,
                480.0, 465.0, 470.0, 485.0, 520.0, 560.0, 575.0, 550.0, 520.0, 490.0, 460.0, 440.0,
            ],
            // February: still high coal, slight improvement
            [
                410.0, 400.0, 395.0, 390.0, 400.0, 430.0, 490.0, 520.0, 530.0, 515.0, 500.0, 485.0,
                470.0, 455.0, 460.0, 475.0, 510.0, 550.0, 565.0, 540.0, 510.0, 480.0, 450.0, 430.0,
            ],
            // March: transition, more wind, some solar
            [
                390.0, 380.0, 375.0, 370.0, 380.0, 410.0, 465.0, 495.0, 500.0, 480.0, 460.0, 440.0,
                420.0, 405.0, 410.0, 430.0, 470.0, 520.0, 535.0, 510.0, 480.0, 450.0, 420.0, 405.0,
            ],
            // April: spring, growing solar midday dip
            [
                370.0, 360.0, 355.0, 350.0, 360.0, 385.0, 440.0, 470.0, 475.0, 455.0, 435.0, 415.0,
                395.0, 380.0, 385.0, 405.0, 445.0, 495.0, 510.0, 485.0, 455.0, 425.0, 395.0, 380.0,
            ],
            // May: significant solar, reduced coal
            [
                350.0, 340.0, 335.0, 330.0, 340.0, 365.0, 420.0, 445.0, 445.0, 420.0, 395.0, 375.0,
                360.0, 350.0, 360.0, 385.0, 425.0, 475.0, 490.0, 465.0, 435.0, 400.0, 370.0, 360.0,
            ],
            // June: peak solar, lowest coal use
            [
                340.0, 330.0, 325.0, 320.0, 330.0, 355.0, 405.0, 430.0, 425.0, 400.0, 375.0, 355.0,
                340.0, 330.0, 340.0, 365.0, 410.0, 460.0, 480.0, 455.0, 425.0, 390.0, 360.0, 350.0,
            ],
            // July: continued solar, slightly warmer (AC)
            [
                345.0, 335.0, 330.0, 325.0, 335.0, 360.0, 410.0, 435.0, 430.0, 405.0, 380.0, 360.0,
                345.0, 335.0, 345.0, 370.0, 415.0, 465.0, 485.0, 460.0, 430.0, 395.0, 365.0, 355.0,
            ],
            // August: similar to July, slightly rising
            [
                350.0, 340.0, 335.0, 330.0, 340.0, 365.0, 415.0, 440.0, 440.0, 415.0, 390.0, 370.0,
                355.0, 345.0, 355.0, 380.0, 420.0, 470.0, 490.0, 465.0, 435.0, 400.0, 370.0, 360.0,
            ],
            // September: solar declining, coal increasing
            [
                370.0, 360.0, 355.0, 350.0, 360.0, 385.0, 440.0, 465.0, 470.0, 450.0, 430.0, 415.0,
                400.0, 390.0, 395.0, 415.0, 455.0, 500.0, 515.0, 490.0, 460.0, 430.0, 400.0, 385.0,
            ],
            // October: autumn, more gas+coal
            [
                390.0, 380.0, 375.0, 370.0, 380.0, 405.0, 460.0, 490.0, 495.0, 475.0, 455.0, 440.0,
                425.0, 415.0, 420.0, 440.0, 475.0, 520.0, 535.0, 510.0, 480.0, 450.0, 420.0, 405.0,
            ],
            // November: late autumn, rising coal
            [
                405.0, 395.0, 390.0, 385.0, 395.0, 425.0, 480.0, 510.0, 520.0, 505.0, 490.0, 475.0,
                460.0, 445.0, 450.0, 465.0, 500.0, 540.0, 555.0, 530.0, 500.0, 470.0, 440.0, 420.0,
            ],
            // December: peak winter, highest coal
            [
                420.0, 410.0, 405.0, 400.0, 410.0, 440.0, 500.0, 530.0, 540.0, 525.0, 510.0, 495.0,
                480.0, 465.0, 470.0, 485.0, 520.0, 560.0, 575.0, 550.0, 520.0, 490.0, 460.0, 440.0,
            ],
        ],
    ),
    // UK (eu-west-2): wind + gas. Winter gas heating, summer more wind.
    // Sources: National Grid ESO Carbon Intensity API, ENTSO-E (2022-2024).
    // Annual mean in CARBON_TABLE: 231. Grand mean target: ~231.
    (
        "eu-west-2",
        [
            // January: high gas heating
            [
                215.0, 205.0, 200.0, 195.0, 205.0, 230.0, 270.0, 300.0, 295.0, 280.0, 265.0, 255.0,
                245.0, 235.0, 240.0, 255.0, 285.0, 315.0, 325.0, 305.0, 280.0, 260.0, 240.0, 225.0,
            ],
            // February: still winter
            [
                210.0, 200.0, 195.0, 190.0, 200.0, 225.0, 265.0, 295.0, 290.0, 275.0, 260.0, 250.0,
                240.0, 230.0, 235.0, 250.0, 280.0, 310.0, 320.0, 300.0, 275.0, 255.0, 235.0, 220.0,
            ],
            // March: transition, improving wind
            [
                200.0, 190.0, 185.0, 180.0, 190.0, 215.0, 250.0, 280.0, 275.0, 260.0, 245.0, 235.0,
                225.0, 215.0, 220.0, 235.0, 265.0, 295.0, 305.0, 285.0, 260.0, 240.0, 220.0, 210.0,
            ],
            // April: spring
            [
                190.0, 180.0, 175.0, 170.0, 180.0, 205.0, 240.0, 270.0, 265.0, 250.0, 235.0, 225.0,
                215.0, 205.0, 210.0, 225.0, 255.0, 285.0, 295.0, 275.0, 250.0, 230.0, 210.0, 200.0,
            ],
            // May: more wind and solar
            [
                180.0, 170.0, 165.0, 160.0, 170.0, 195.0, 228.0, 255.0, 250.0, 235.0, 220.0, 210.0,
                200.0, 190.0, 195.0, 210.0, 240.0, 270.0, 280.0, 260.0, 235.0, 215.0, 195.0, 185.0,
            ],
            // June: summer, good wind
            [
                172.0, 162.0, 157.0, 152.0, 162.0, 185.0, 218.0, 245.0, 240.0, 225.0, 210.0, 200.0,
                190.0, 180.0, 185.0, 200.0, 230.0, 260.0, 270.0, 250.0, 225.0, 205.0, 185.0, 177.0,
            ],
            // July: summer, variable wind
            [
                175.0, 165.0, 160.0, 155.0, 165.0, 188.0, 222.0, 250.0, 245.0, 230.0, 215.0, 205.0,
                195.0, 185.0, 190.0, 205.0, 235.0, 265.0, 275.0, 255.0, 230.0, 210.0, 190.0, 180.0,
            ],
            // August: late summer
            [
                178.0, 168.0, 163.0, 158.0, 168.0, 192.0, 225.0, 253.0, 248.0, 233.0, 218.0, 208.0,
                198.0, 188.0, 193.0, 208.0, 238.0, 268.0, 278.0, 258.0, 233.0, 213.0, 193.0, 183.0,
            ],
            // September: autumn transition
            [
                190.0, 180.0, 175.0, 170.0, 180.0, 205.0, 240.0, 268.0, 263.0, 248.0, 233.0, 223.0,
                213.0, 203.0, 208.0, 223.0, 253.0, 283.0, 293.0, 273.0, 248.0, 228.0, 208.0, 198.0,
            ],
            // October: autumn, more gas
            [
                200.0, 190.0, 185.0, 180.0, 190.0, 215.0, 250.0, 280.0, 275.0, 260.0, 245.0, 235.0,
                225.0, 215.0, 220.0, 235.0, 265.0, 295.0, 305.0, 285.0, 260.0, 240.0, 220.0, 210.0,
            ],
            // November: late autumn, high gas
            [
                210.0, 200.0, 195.0, 190.0, 200.0, 225.0, 260.0, 290.0, 285.0, 270.0, 255.0, 245.0,
                235.0, 225.0, 230.0, 245.0, 275.0, 305.0, 315.0, 295.0, 270.0, 250.0, 230.0, 215.0,
            ],
            // December: winter peak
            [
                215.0, 205.0, 200.0, 195.0, 205.0, 230.0, 270.0, 300.0, 295.0, 280.0, 265.0, 255.0,
                245.0, 235.0, 240.0, 255.0, 285.0, 315.0, 325.0, 305.0, 280.0, 260.0, 240.0, 225.0,
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
    // Source: ENTSO-E, EirGrid (2022-2024). Annual: 296.
    (
        "eu-west-1",
        [
            270.0, 260.0, 255.0, 250.0, 260.0, 275.0, 295.0, 315.0, 320.0, 315.0, 305.0, 295.0,
            290.0, 285.0, 290.0, 300.0, 315.0, 335.0, 340.0, 325.0, 310.0, 295.0, 280.0, 275.0,
        ],
    ),
    // Netherlands (eu-west-4): gas + wind. Gas sets the marginal rate.
    // Evening peak from gas generation for heating/cooking.
    // Source: ENTSO-E, TenneT (2022-2024). Annual: 328.
    (
        "eu-west-4",
        [
            300.0, 290.0, 285.0, 280.0, 290.0, 310.0, 335.0, 355.0, 360.0, 350.0, 340.0, 330.0,
            320.0, 310.0, 315.0, 330.0, 350.0, 370.0, 375.0, 360.0, 345.0, 330.0, 315.0, 305.0,
        ],
    ),
    // Sweden (eu-north-1): hydro + nuclear dominated. Very clean, nearly flat.
    // Slight bump during business hours from marginal fossil imports.
    // Source: ENTSO-E, Svenska Kraftnat (2022-2024). Annual: 8.
    (
        "eu-north-1",
        [
            7.0, 7.0, 7.0, 7.0, 7.0, 7.0, 8.0, 9.0, 9.0, 9.0, 9.0, 8.0, 8.0, 8.0, 8.0, 8.0, 9.0,
            9.0, 9.0, 9.0, 8.0, 8.0, 7.0, 7.0,
        ],
    ),
    // Belgium (europe-west1): nuclear + gas. Moderate diurnal variation.
    // Nuclear provides ~50% baseload; gas fills the rest.
    // Source: ENTSO-E, Elia (2022-2024). Annual: 187.
    (
        "europe-west1",
        [
            168.0, 162.0, 158.0, 155.0, 162.0, 175.0, 195.0, 210.0, 212.0, 205.0, 198.0, 190.0,
            185.0, 180.0, 182.0, 190.0, 205.0, 218.0, 222.0, 212.0, 200.0, 190.0, 178.0, 172.0,
        ],
    ),
    // Finland (europe-north1): nuclear + hydro + wind. Very clean, nearly flat.
    // Source: ENTSO-E, Fingrid (2022-2024). Annual: 8.
    (
        "europe-north1",
        [
            7.0, 7.0, 7.0, 7.0, 7.0, 7.0, 8.0, 9.0, 9.0, 9.0, 9.0, 8.0, 8.0, 8.0, 8.0, 8.0, 9.0,
            9.0, 9.0, 9.0, 8.0, 8.0, 7.0, 7.0,
        ],
    ),
    // Italy (eu-south-1 / europe-west8): gas + solar. Midday solar dip.
    // Source: ENTSO-E, Terna (2022-2024). Annual: 370.
    (
        "eu-south-1",
        [
            345.0, 335.0, 330.0, 325.0, 335.0, 350.0, 375.0, 395.0, 390.0, 370.0, 350.0, 340.0,
            335.0, 340.0, 350.0, 365.0, 385.0, 405.0, 415.0, 400.0, 385.0, 370.0, 358.0, 350.0,
        ],
    ),
    // Spain (europe-southwest1): solar + wind. Strong midday solar dip.
    // Source: ENTSO-E, REE (2022-2024). Annual: 200.
    (
        "europe-southwest1",
        [
            195.0, 188.0, 184.0, 180.0, 184.0, 192.0, 210.0, 225.0, 215.0, 195.0, 178.0, 170.0,
            168.0, 172.0, 180.0, 195.0, 215.0, 235.0, 240.0, 228.0, 215.0, 205.0, 200.0, 198.0,
        ],
    ),
    // Poland (europe-central2): coal-heavy. Relatively flat but high.
    // Slight evening peak from increased demand on coal baseload.
    // Source: ENTSO-E, PSE (2022-2024). Annual: 700.
    (
        "europe-central2",
        [
            660.0, 650.0, 645.0, 640.0, 650.0, 670.0, 705.0, 730.0, 735.0, 725.0, 715.0, 705.0,
            695.0, 690.0, 695.0, 710.0, 730.0, 750.0, 755.0, 740.0, 720.0, 705.0, 685.0, 670.0,
        ],
    ),
    // Norway (europe-north2): hydro-dominated. Very clean and flat.
    // Source: ENTSO-E, Statnett (2022-2024). Annual: 7.
    (
        "europe-north2",
        [
            6.0, 6.0, 6.0, 6.0, 6.0, 7.0, 7.0, 8.0, 8.0, 8.0, 8.0, 7.0, 7.0, 7.0, 7.0, 7.0, 8.0,
            8.0, 8.0, 8.0, 7.0, 7.0, 7.0, 6.0,
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
    // Source: estimated from EMA Singapore fuel mix (2023-2024). Annual: 408.
    (
        "ap-southeast-1",
        [
            395.0, 392.0, 390.0, 388.0, 392.0, 398.0, 408.0, 418.0, 420.0, 418.0, 415.0, 412.0,
            410.0, 408.0, 406.0, 408.0, 412.0, 418.0, 420.0, 418.0, 415.0, 410.0, 402.0, 398.0,
        ],
    ),
    // India / Mumbai (ap-south-1): coal-heavy, high intensity.
    // Coal provides ~70% of electricity. Mild evening peak.
    // Estimated from fuel mix, no hourly data source.
    // Source: estimated from POSOCO/CEA fuel mix (2023-2024). Annual: 708.
    (
        "ap-south-1",
        [
            680.0, 672.0, 668.0, 665.0, 672.0, 688.0, 710.0, 728.0, 732.0, 728.0, 720.0, 712.0,
            708.0, 705.0, 708.0, 715.0, 725.0, 738.0, 742.0, 735.0, 725.0, 715.0, 698.0, 688.0,
        ],
    ),
    // Brazil / Sao Paulo (sa-east-1): hydro-heavy, clean. Nearly flat.
    // ~60% hydro, ~20% wind/solar. Slight evening peak from thermal backup.
    // Estimated from fuel mix, no hourly data source.
    // Source: estimated from ONS Brazil fuel mix (2023-2024). Annual: 62.
    (
        "sa-east-1",
        [
            58.0, 56.0, 55.0, 54.0, 56.0, 58.0, 62.0, 66.0, 67.0, 66.0, 65.0, 64.0, 63.0, 62.0,
            61.0, 62.0, 64.0, 67.0, 68.0, 67.0, 65.0, 63.0, 60.0, 59.0,
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
