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

/// Extract the UTC hour (0-23) from an ISO 8601 timestamp string.
///
/// Accepts the canonical `YYYY-MM-DDTHH:MM:SS[.fff]Z` form and the
/// space-separated `YYYY-MM-DD HH:MM:SS[.fff]Z` form. Returns `None` for:
/// - strings shorter than 13 bytes
/// - strings without a `T` or space at position 10
/// - non-numeric hour digits at positions 11-12
/// - hours outside `0..24`
/// - strings that do not end with `Z` (non-UTC offsets like `+02:00`
///   are rejected rather than silently shifted — the embedded hourly
///   carbon profile table is UTC-anchored, so naive offset handling
///   would poison CO₂ estimates)
///
/// Used by the hourly carbon profile path in
/// `score::compute_carbon_report`. Callers that receive `None` should
/// fall back to the flat annual intensity for the region (no sentinel
/// hour — a wrong hour would silently skew the estimate).
#[must_use]
pub(crate) fn parse_utc_hour(ts: &str) -> Option<u8> {
    // Strict ASCII-only parsing. If the string contains non-ASCII bytes
    // the indexing into `bytes` below would misalign with character
    // boundaries, but ASCII is all we need for ISO 8601.
    if !ts.is_ascii() {
        return None;
    }
    let bytes = ts.as_bytes();
    if bytes.len() < 13 {
        return None;
    }
    // Position 10 must be the date/time separator: 'T' (canonical) or
    // ' ' (space-separated variant used by some loggers).
    if bytes[10] != b'T' && bytes[10] != b' ' {
        return None;
    }
    // Positions 11-12 are the two-digit hour. Manually validate each
    // digit so `"1a"` or `"  "` produce `None` instead of silently
    // landing on hour 0.
    let h1 = bytes[11].checked_sub(b'0').filter(|&d| d <= 9)?;
    let h2 = bytes[12].checked_sub(b'0').filter(|&d| d <= 9)?;
    let hour = h1 * 10 + h2;
    if hour >= 24 {
        return None;
    }
    // Must end with 'Z' to be UTC. This rejects the `+HH:MM` / `-HH:MM`
    // offset forms deliberately — they would require a proper offset
    // subtraction that we don't support yet, and silently treating
    // local hours as UTC would bias the carbon estimate systematically.
    if !ts.ends_with('Z') {
        return None;
    }
    Some(hour)
}

/// Extract the UTC month (0-indexed: 0 = January, 11 = December) from
/// an ISO 8601 timestamp string.
///
/// Parses positions 5-6 of `YYYY-MM-DD...` (the `MM` field). Same
/// ASCII-only and UTC-only constraints as [`parse_utc_hour`], but with
/// a lower minimum length (7 bytes vs 13). In practice, always called
/// alongside `parse_utc_hour` which provides the stricter check.
///
/// Returns `None` for strings shorter than 7 bytes, non-numeric month
/// digits, or months outside 01..=12. The returned value is 0-indexed
/// for direct use as an array index into `[[f64; 24]; 12]`.
#[must_use]
pub(crate) fn parse_utc_month(ts: &str) -> Option<u8> {
    if !ts.is_ascii() {
        return None;
    }
    let bytes = ts.as_bytes();
    if bytes.len() < 7 {
        return None;
    }
    // Position 4 must be '-' (YYYY-MM).
    if bytes[4] != b'-' {
        return None;
    }
    let m1 = bytes[5].checked_sub(b'0').filter(|&d| d <= 9)?;
    let m2 = bytes[6].checked_sub(b'0').filter(|&d| d <= 9)?;
    let month = m1 * 10 + m2;
    if !(1..=12).contains(&month) {
        return None;
    }
    // Same UTC check as parse_utc_hour.
    if !ts.ends_with('Z') {
        return None;
    }
    Some(month - 1) // 0-indexed
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

    // --- parse_utc_hour ---

    #[test]
    fn parse_utc_hour_canonical() {
        assert_eq!(parse_utc_hour("2025-07-10T14:32:01.123Z"), Some(14));
        assert_eq!(parse_utc_hour("2025-07-10T00:00:00.000Z"), Some(0));
        assert_eq!(parse_utc_hour("2025-07-10T23:59:59.999Z"), Some(23));
    }

    #[test]
    fn parse_utc_hour_no_fraction() {
        // "2025-07-10T09:30:00Z" — no millisecond fraction is still valid.
        assert_eq!(parse_utc_hour("2025-07-10T09:30:00Z"), Some(9));
    }

    #[test]
    fn parse_utc_hour_space_separator() {
        // Some loggers emit a space instead of 'T' between date and time.
        assert_eq!(parse_utc_hour("2025-07-10 14:32:01.123Z"), Some(14));
    }

    #[test]
    fn parse_utc_hour_rejects_nonutc_offset() {
        // +02:00 offset must NOT be silently treated as UTC.
        assert_eq!(parse_utc_hour("2025-07-10T14:32:01.123+02:00"), None);
        assert_eq!(parse_utc_hour("2025-07-10T14:32:01-05:00"), None);
    }

    #[test]
    fn parse_utc_hour_rejects_truncated_string() {
        assert_eq!(parse_utc_hour(""), None);
        assert_eq!(parse_utc_hour("2025-07-10"), None);
        assert_eq!(parse_utc_hour("2025-07-10T14"), None); // len 13 but no Z
    }

    #[test]
    fn parse_utc_hour_rejects_invalid_separator() {
        // Position 10 must be 'T' or ' '. An underscore or hyphen there
        // is not a valid ISO 8601 variant we support.
        assert_eq!(parse_utc_hour("2025-07-10_14:32:01Z"), None);
        assert_eq!(parse_utc_hour("2025-07-10-14:32:01Z"), None);
    }

    #[test]
    fn parse_utc_hour_rejects_non_numeric_hour() {
        assert_eq!(parse_utc_hour("2025-07-10Tab:32:01Z"), None);
        assert_eq!(parse_utc_hour("2025-07-10T  :32:01Z"), None);
    }

    #[test]
    fn parse_utc_hour_rejects_hour_24_or_above() {
        // Hour 24 is not a valid ISO 8601 value (use 00 next day instead).
        assert_eq!(parse_utc_hour("2025-07-10T24:00:00Z"), None);
        assert_eq!(parse_utc_hour("2025-07-10T99:00:00Z"), None);
    }

    #[test]
    fn parse_utc_hour_rejects_missing_trailing_z() {
        // Must end with 'Z'. This rules out naked local-time strings.
        assert_eq!(parse_utc_hour("2025-07-10T14:32:01.123"), None);
    }

    #[test]
    fn parse_utc_hour_rejects_non_ascii() {
        // Multi-byte characters would misalign the byte indexing above.
        // The function returns None instead of panicking.
        assert_eq!(parse_utc_hour("2025-07-10T14\u{00E9}:32:01Z"), None);
    }

    // --- parse_utc_month ---

    #[test]
    fn parse_utc_month_canonical() {
        assert_eq!(parse_utc_month("2025-01-10T14:32:01.123Z"), Some(0)); // Jan
        assert_eq!(parse_utc_month("2025-06-15T00:00:00.000Z"), Some(5)); // Jun
        assert_eq!(parse_utc_month("2025-07-10T14:32:01.123Z"), Some(6)); // Jul
        assert_eq!(parse_utc_month("2025-12-31T23:59:59.999Z"), Some(11)); // Dec
    }

    #[test]
    fn parse_utc_month_all_months() {
        for m in 1..=12_u8 {
            let ts = format!("2025-{m:02}-10T12:00:00Z");
            assert_eq!(parse_utc_month(&ts), Some(m - 1), "month {m:02}");
        }
    }

    #[test]
    fn parse_utc_month_rejects_month_00() {
        assert_eq!(parse_utc_month("2025-00-10T14:32:01Z"), None);
    }

    #[test]
    fn parse_utc_month_rejects_month_13() {
        assert_eq!(parse_utc_month("2025-13-10T14:32:01Z"), None);
    }

    #[test]
    fn parse_utc_month_rejects_truncated() {
        assert_eq!(parse_utc_month(""), None);
        assert_eq!(parse_utc_month("2025-0"), None);
        assert_eq!(parse_utc_month("2025"), None);
    }

    #[test]
    fn parse_utc_month_rejects_non_utc() {
        assert_eq!(parse_utc_month("2025-07-10T14:32:01+02:00"), None);
        assert_eq!(parse_utc_month("2025-07-10T14:32:01-05:00"), None);
    }

    #[test]
    fn parse_utc_month_rejects_non_ascii() {
        assert_eq!(parse_utc_month("2025\u{00E9}07-10T14:32:01Z"), None);
    }

    #[test]
    fn parse_utc_month_rejects_non_numeric() {
        assert_eq!(parse_utc_month("2025-ab-10T14:32:01Z"), None);
    }

    #[test]
    fn parse_utc_month_rejects_missing_dash() {
        assert_eq!(parse_utc_month("2025007-10T14:32:01Z"), None);
    }
}
