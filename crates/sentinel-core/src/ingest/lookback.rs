//! Shared lookback-duration parser for HTTP trace ingestion modules.
//!
//! Both `tempo` and `jaeger_query` subcommands accept a `--lookback`
//! string like `"1h"`, `"30m"`, `"2h30m"` to bound their search window.
//! The parsing logic lives here once, each module wraps it with its
//! own error type.

use std::time::Duration;

/// Errors from lookback-duration parsing.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LookbackError {
    #[error("empty string")]
    Empty,

    #[error("{0}")]
    Invalid(String),

    #[error("duration must be greater than zero")]
    Zero,

    #[error("duration overflows u64 seconds")]
    Overflow,
}

/// Parse a human-readable duration string like `"1h"`, `"30m"`, `"24h"`, `"2h30m"`.
///
/// Accepts the unit suffixes `h`, `m`, `s` and composes them by summing
/// the contributions (so `"2h30m"` equals 2h + 30m = 9000s). All
/// arithmetic is checked, so pathological inputs like `"999999999h"`
/// surface as `LookbackError::Overflow` instead of wrapping silently
/// in release builds.
///
/// # Errors
///
/// Returns `LookbackError` for empty, unit-less, unknown-unit,
/// zero-valued, or overflowing inputs.
pub fn parse(s: &str) -> Result<Duration, LookbackError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(LookbackError::Empty);
    }

    let mut total_secs: u64 = 0;
    let mut num_buf = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else {
            if num_buf.is_empty() {
                return Err(LookbackError::Invalid(format!(
                    "unexpected '{ch}' without a preceding number"
                )));
            }
            let n: u64 = num_buf
                .parse()
                .map_err(|_| LookbackError::Invalid(format!("invalid number: {num_buf}")))?;
            num_buf.clear();
            let multiplier: u64 = match ch {
                'h' => 3600,
                'm' => 60,
                's' => 1,
                _ => {
                    return Err(LookbackError::Invalid(format!(
                        "unknown unit '{ch}', expected h/m/s"
                    )));
                }
            };
            let component = n.checked_mul(multiplier).ok_or(LookbackError::Overflow)?;
            total_secs = total_secs
                .checked_add(component)
                .ok_or(LookbackError::Overflow)?;
        }
    }

    if !num_buf.is_empty() {
        return Err(LookbackError::Invalid(format!(
            "number '{num_buf}' without a unit suffix (h/m/s)"
        )));
    }

    if total_secs == 0 {
        return Err(LookbackError::Zero);
    }

    Ok(Duration::from_secs(total_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours() {
        assert_eq!(parse("1h").unwrap(), Duration::from_hours(1));
        assert_eq!(parse("24h").unwrap(), Duration::from_hours(24));
    }

    #[test]
    fn minutes() {
        assert_eq!(parse("30m").unwrap(), Duration::from_mins(30));
    }

    #[test]
    fn seconds() {
        assert_eq!(parse("90s").unwrap(), Duration::from_secs(90));
    }

    #[test]
    fn combined() {
        assert_eq!(parse("2h30m").unwrap(), Duration::from_mins(150));
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(parse(""), Err(LookbackError::Empty)));
        assert!(matches!(parse("   "), Err(LookbackError::Empty)));
    }

    #[test]
    fn rejects_no_unit() {
        assert!(matches!(parse("30"), Err(LookbackError::Invalid(_))));
    }

    #[test]
    fn rejects_unknown_unit() {
        assert!(matches!(parse("5d"), Err(LookbackError::Invalid(_))));
    }

    #[test]
    fn rejects_zero() {
        assert!(matches!(parse("0h"), Err(LookbackError::Zero)));
    }

    #[test]
    fn rejects_overflow_on_multiplication() {
        assert!(matches!(
            parse("18446744073709551615h"),
            Err(LookbackError::Overflow)
        ));
    }

    #[test]
    fn rejects_overflow_on_addition() {
        // Two components each fitting in u64 but whose sum does not.
        let huge = format!("{0}h{0}h", u64::MAX / 3600);
        assert!(matches!(parse(&huge), Err(LookbackError::Overflow)));
    }
}
