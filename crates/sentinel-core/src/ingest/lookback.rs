//! Shared search-window types for HTTP trace ingestion modules.
//!
//! Both `tempo` and `jaeger_query` subcommands accept a `--lookback`
//! string like `"1h"`, `"30m"`, `"7d"`, `"2h30m"` to bound their search window,
//! or a `--from`/`--to` pair for an absolute one. The parsing logic and
//! the window type live here once, each module wraps them with its own
//! error type.

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

/// Parse a human-readable duration string like `"1h"`, `"30m"`, `"7d"`, `"2h30m"`.
///
/// Accepts the unit suffixes `d`, `h`, `m`, `s` and composes them by
/// summing the contributions (so `"2h30m"` equals 2h + 30m = 9000s). All
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
                'd' => 86_400,
                'h' => 3600,
                'm' => 60,
                's' => 1,
                _ => {
                    return Err(LookbackError::Invalid(format!(
                        "unknown unit '{ch}', expected d/h/m/s"
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
            "number '{num_buf}' without a unit suffix (d/h/m/s)"
        )));
    }

    if total_secs == 0 {
        return Err(LookbackError::Zero);
    }

    Ok(Duration::from_secs(total_secs))
}

/// Errors from building or resolving a search window.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WindowError {
    #[error("window end {end} is not after start {start}, in epoch milliseconds")]
    NotOrdered { start: u64, end: u64 },

    #[error("invalid ISO 8601 UTC timestamp '{value}': {reason}")]
    InvalidTimestamp { value: String, reason: String },
}

/// How a trace search is bounded in time.
///
/// The two arms are not interchangeable on the wire. Tempo takes explicit
/// bounds either way, but the Jaeger query API has its own relative
/// parameter, and keeping [`Self::Lookback`] mapped onto it leaves every
/// request that existed before absolute windows byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchWindow {
    /// Relative to the moment the request is issued, so it drifts if the
    /// caller queues the request before sending it.
    Lookback(Duration),

    /// Fixed bounds in Unix epoch milliseconds, immune to that drift.
    ///
    /// Milliseconds rather than seconds because that is what ISO 8601
    /// carries and what the Jaeger query API accepts, once scaled to its
    /// own microseconds. Storing seconds here would discard precision one
    /// of the two backends can actually use.
    Absolute { start_ms: u64, end_ms: u64 },
}

impl SearchWindow {
    /// Build an absolute window from two ISO 8601 UTC timestamps, such as
    /// `2026-08-20T15:59:00Z`.
    ///
    /// Parsing lives here rather than in the caller because `crate::time`
    /// is the single source of truth for calendar arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`WindowError::InvalidTimestamp`] when either side does not
    /// parse, and [`WindowError::NotOrdered`] when the pair is empty or
    /// inverted, so a bad window fails at the flag rather than at the API.
    pub fn from_iso8601(from: &str, to: &str) -> Result<Self, WindowError> {
        let parse = |value: &str| {
            crate::time::parse_iso8601_utc_to_ms(value).map_err(|reason| {
                WindowError::InvalidTimestamp {
                    value: value.to_string(),
                    reason,
                }
            })
        };
        let window = Self::Absolute {
            start_ms: parse(from)?,
            end_ms: parse(to)?,
        };
        window.resolve()?;
        Ok(window)
    }

    /// Absolute bounds in Unix epoch milliseconds, as `(start, end)`.
    ///
    /// Each backend scales from here to the unit its own API takes, which
    /// is why this returns the finest unit either of them accepts rather
    /// than the coarsest they share.
    ///
    /// # Errors
    ///
    /// Returns [`WindowError::NotOrdered`] when the window is empty or
    /// inverted. A backend answers such a window with an empty result
    /// set rather than an error, which reads as "nothing happened".
    pub fn resolve(self) -> Result<(u64, u64), WindowError> {
        let (start, end) = match self {
            Self::Lookback(d) => {
                let now = u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(u64::MAX);
                (
                    now.saturating_sub(d.as_millis().try_into().unwrap_or(u64::MAX)),
                    now,
                )
            }
            Self::Absolute { start_ms, end_ms } => (start_ms, end_ms),
        };

        if end <= start {
            return Err(WindowError::NotOrdered { start, end });
        }
        Ok((start, end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_matches;

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
    fn days() {
        assert_eq!(parse("1d").unwrap(), Duration::from_hours(24));
        assert_eq!(parse("180d").unwrap(), Duration::from_hours(180 * 24));
    }

    #[test]
    fn combined() {
        assert_eq!(parse("2h30m").unwrap(), Duration::from_mins(150));
        assert_eq!(parse("1d12h").unwrap(), Duration::from_hours(36));
    }

    #[test]
    fn rejects_empty() {
        assert_matches!(parse(""), Err(LookbackError::Empty));
        assert_matches!(parse("   "), Err(LookbackError::Empty));
    }

    #[test]
    fn rejects_no_unit() {
        assert_matches!(parse("30"), Err(LookbackError::Invalid(_)));
    }

    #[test]
    fn rejects_unknown_unit() {
        assert_matches!(parse("5w"), Err(LookbackError::Invalid(_)));
        assert_matches!(parse("5y"), Err(LookbackError::Invalid(_)));
    }

    #[test]
    fn rejects_zero() {
        assert_matches!(parse("0h"), Err(LookbackError::Zero));
    }

    #[test]
    fn rejects_overflow_on_multiplication() {
        assert_matches!(parse("18446744073709551615h"), Err(LookbackError::Overflow));
    }

    #[test]
    fn rejects_overflow_on_addition() {
        // Two components each fitting in u64 but whose sum does not.
        let huge = format!("{0}h{0}h", u64::MAX / 3600);
        assert_matches!(parse(&huge), Err(LookbackError::Overflow));
    }

    #[test]
    fn absolute_window_resolves_to_its_own_bounds() {
        let w = SearchWindow::Absolute {
            start_ms: 1_787_838_000_000,
            end_ms: 1_787_839_200_500,
        };
        assert_eq!(w.resolve().unwrap(), (1_787_838_000_000, 1_787_839_200_500));
    }

    /// Sub-second bounds used to collapse to the same second and be
    /// rejected. The Jaeger query API takes microseconds, so they survive.
    #[test]
    fn a_sub_second_window_survives() {
        let w = SearchWindow::from_iso8601("2026-08-27T14:00:00.100Z", "2026-08-27T14:00:00.900Z")
            .expect("a sub-second window must build");
        let (start, end) = w.resolve().expect("resolve");
        assert_eq!(end - start, 800);
    }

    #[test]
    fn lookback_window_ends_now_and_spans_the_duration() {
        let (start, end) = SearchWindow::Lookback(Duration::from_hours(2))
            .resolve()
            .unwrap();
        assert_eq!(end - start, 7_200_000);
    }

    #[test]
    fn rejects_inverted_and_empty_windows() {
        let inverted = SearchWindow::Absolute {
            start_ms: 2_000,
            end_ms: 1_000,
        };
        assert_matches!(
            inverted.resolve(),
            Err(WindowError::NotOrdered {
                start: 2_000,
                end: 1_000
            })
        );

        let empty = SearchWindow::Absolute {
            start_ms: 1_000,
            end_ms: 1_000,
        };
        assert_matches!(empty.resolve(), Err(WindowError::NotOrdered { .. }));
    }

    #[test]
    fn builds_an_absolute_window_from_iso8601() {
        let w = SearchWindow::from_iso8601("2026-08-27T14:00:00Z", "2026-08-27T15:00:00Z")
            .expect("a well-formed ordered pair must build");
        let (start, end) = w.resolve().expect("resolve");
        assert_eq!(end - start, 3_600_000);
    }

    #[test]
    fn rejects_a_malformed_timestamp() {
        assert_matches!(
            SearchWindow::from_iso8601("yesterday", "2026-08-27T15:00:00Z"),
            Err(WindowError::InvalidTimestamp { .. })
        );
    }

    #[test]
    fn rejects_an_end_before_the_start_at_build_time() {
        assert_matches!(
            SearchWindow::from_iso8601("2026-08-27T15:00:00Z", "2026-08-27T14:00:00Z"),
            Err(WindowError::NotOrdered { .. })
        );
    }

    #[test]
    fn rejects_a_zero_lookback() {
        // The parser refuses "0h", but a caller can build the variant directly.
        assert_matches!(
            SearchWindow::Lookback(Duration::ZERO).resolve(),
            Err(WindowError::NotOrdered { .. })
        );
    }
}
