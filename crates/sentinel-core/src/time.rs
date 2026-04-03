//! Shared timestamp conversion helpers.

/// Convert nanoseconds since epoch to an ISO 8601 timestamp string.
///
/// Format: `YYYY-MM-DDTHH:MM:SS.mmmZ` (always UTC, 3 fractional digits).
#[must_use]
pub(crate) fn nanos_to_iso8601(nanos: u64) -> String {
    const NANOS_PER_SEC: u64 = 1_000_000_000;
    const SECS_PER_MIN: u64 = 60;
    const SECS_PER_HOUR: u64 = 3600;
    const SECS_PER_DAY: u64 = 86400;

    let total_secs = nanos / NANOS_PER_SEC;
    let millis = (nanos % NANOS_PER_SEC) / 1_000_000;

    // Days since epoch
    let mut days = total_secs / SECS_PER_DAY;
    let day_secs = total_secs % SECS_PER_DAY;

    let hours = day_secs / SECS_PER_HOUR;
    let minutes = (day_secs % SECS_PER_HOUR) / SECS_PER_MIN;
    let seconds = day_secs % SECS_PER_MIN;

    // Convert days since epoch to year/month/day
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    days += 719_468; // shift to 0000-03-01
    let era = days / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}

/// Convert microseconds since epoch to an ISO 8601 timestamp string.
#[must_use]
pub(crate) fn micros_to_iso8601(micros: u64) -> String {
    nanos_to_iso8601(micros.saturating_mul(1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanos_basic() {
        let ts = nanos_to_iso8601(1_720_621_921_123_000_000);
        assert_eq!(ts, "2024-07-10T14:32:01.123Z");
    }

    #[test]
    fn micros_basic() {
        let ts = micros_to_iso8601(1_720_621_921_123_000);
        assert_eq!(ts, "2024-07-10T14:32:01.123Z");
    }

    #[test]
    fn zero_epoch() {
        assert_eq!(nanos_to_iso8601(0), "1970-01-01T00:00:00.000Z");
    }
}
