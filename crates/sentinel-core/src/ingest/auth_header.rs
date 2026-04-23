//! Parse a user-supplied `--auth-header "Name: Value"` line into a
//! hyper-safe `(HeaderName, HeaderValue)` pair, shared between the
//! `tempo` and `jaeger-query` subcommands.
//!
//! The parsed value is marked `sensitive` so hyper omits it from its
//! own debug output and from HTTP/2 HPACK compression tables. The
//! struct also implements a manual `Debug` that never prints the
//! value, so a logged `AuthHeader` never leaks the credential.
//!
//! # Validation rules
//!
//! Parsing is intentionally strict. Beyond the hyper-level checks
//! (token-only name, VCHAR + SP + HTAB value, so internal tabs and
//! spaces inside the value ARE preserved as-is, only CR/LF and
//! non-visible ASCII are rejected) we reject:
//!
//! - Raw inputs longer than 8 KiB, to bound the per-task clone in the
//!   Tempo parallel fanout and stop a pathological `--auth-header
//!   "X: $(cat /dev/urandom | head -c 50M | base64)"` at the door.
//! - Values that are empty after trimming, which would send a
//!   pointless `Authorization:` to the backend and produce a confusing
//!   401.
//! - Header names that would enable request smuggling or authority
//!   override if user-supplied: `Host`, `Content-Length`,
//!   `Transfer-Encoding`, `Connection`, `Upgrade`, `TE`,
//!   `Proxy-Connection`. Users wanting to tweak those should use a
//!   local proxy, not this flag.

use hyper::header::{HeaderName, HeaderValue};

/// Maximum raw input length accepted by `AuthHeader::parse`, in bytes.
/// A typical JWT is 2 to 4 KiB; 8 KiB leaves headroom for long
/// multi-claim tokens without opening the door to arbitrary blobs.
pub(crate) const MAX_AUTH_HEADER_INPUT_BYTES: usize = 8 * 1024;

/// Header names that `--auth-header` must not set. Allowing any of
/// these would give a remote operator (or a malicious environment
/// variable expansion in CI) the ability to spoof the target host,
/// trigger HTTP request smuggling, or bypass keep-alive semantics.
/// Comparison is case-insensitive per RFC 7230.
const FORBIDDEN_HEADER_NAMES: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "proxy-connection",
];

/// Parsed auth header ready to attach to a `hyper::Request::builder()`.
#[derive(Clone)]
pub struct AuthHeader {
    pub(crate) name: HeaderName,
    pub(crate) value: HeaderValue,
}

impl AuthHeader {
    /// Parse a curl-style header line (`"Name: Value"`) into a validated
    /// `AuthHeader`. The value is forwarded to `HeaderValue::from_str`
    /// which rejects CR, LF and any non-visible ASCII, so header
    /// injection through a malformed user input cannot happen. The
    /// stored value is marked `sensitive` so hyper redacts it from
    /// debug output and HPACK tables.
    ///
    /// # Errors
    ///
    /// Returns `&'static str` describing the failure. Callers wrap
    /// into their own error variant.
    #[must_use = "parsed auth header must be attached to a request to take effect"]
    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        if raw.len() > MAX_AUTH_HEADER_INPUT_BYTES {
            return Err("auth header exceeds 8 KiB input cap");
        }
        let (name_raw, value_raw) = raw
            .split_once(':')
            .ok_or("auth header must be 'Name: Value' format")?;

        let name_trimmed = name_raw.trim();
        if name_trimmed.is_empty() {
            return Err("auth header name is empty");
        }
        let name = HeaderName::from_bytes(name_trimmed.as_bytes())
            .map_err(|_| "invalid auth header name")?;
        if FORBIDDEN_HEADER_NAMES
            .iter()
            .any(|forbidden| name.as_str().eq_ignore_ascii_case(forbidden))
        {
            return Err(
                "auth header name not permitted (hop-by-hop, authority, or framing header)",
            );
        }

        let value_trimmed = value_raw.trim();
        if value_trimmed.is_empty() {
            return Err("auth header value is empty");
        }
        let mut value = HeaderValue::from_str(value_trimmed)
            .map_err(|_| "invalid auth header value (CR, LF or non-visible ASCII forbidden)")?;
        value.set_sensitive(true);

        Ok(Self { name, value })
    }
}

// Manual Debug guarantees the value is never printed, even if a
// future refactor drops hyper's sensitive flag for some reason.
impl std::fmt::Debug for AuthHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthHeader")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bearer_line() {
        let auth = AuthHeader::parse("Authorization: Bearer abc123").expect("valid header");
        assert_eq!(auth.name.as_str(), "authorization");
        assert!(auth.value.is_sensitive());
    }

    #[test]
    fn parses_custom_header() {
        let auth = AuthHeader::parse("X-API-Key: secret").expect("valid header");
        assert_eq!(auth.name.as_str(), "x-api-key");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let auth = AuthHeader::parse("  Authorization  :  Bearer foo  ").expect("valid");
        assert_eq!(auth.name.as_str(), "authorization");
        assert_eq!(auth.value.to_str().expect("visible ascii"), "Bearer foo");
    }

    #[test]
    fn rejects_missing_colon() {
        assert!(AuthHeader::parse("NoColonHere").is_err());
    }

    #[test]
    fn rejects_invalid_name() {
        assert!(AuthHeader::parse("Bad Name: value").is_err());
    }

    #[test]
    fn rejects_empty_name() {
        let err = AuthHeader::parse(": value").expect_err("empty name must fail");
        assert!(err.contains("name"));
    }

    #[test]
    fn rejects_empty_value() {
        let err = AuthHeader::parse("Authorization: ").expect_err("empty value must fail");
        assert!(err.contains("value"));
        let err = AuthHeader::parse("Authorization:").expect_err("empty value must fail");
        assert!(err.contains("value"));
    }

    #[test]
    fn rejects_crlf_in_value() {
        assert!(AuthHeader::parse("X: a\r\nY: b").is_err());
    }

    /// Confirms the documented behaviour that internal whitespace in
    /// the value (including horizontal tabs, per RFC 7230 VCHAR + SP +
    /// HTAB) is preserved as-is. Only surrounding whitespace is
    /// trimmed; only CR/LF/non-visible ASCII is rejected.
    #[test]
    fn preserves_internal_tabs_and_spaces() {
        let auth = AuthHeader::parse("Authorization: Bearer\tfoo bar").expect("valid");
        assert_eq!(
            auth.value.to_str().expect("visible ascii"),
            "Bearer\tfoo bar"
        );
    }

    #[test]
    fn rejects_oversized_input() {
        let huge = format!("X: {}", "a".repeat(MAX_AUTH_HEADER_INPUT_BYTES));
        let err = AuthHeader::parse(&huge).expect_err("over-cap input must fail");
        assert!(err.contains("cap") || err.contains("8 KiB"));
    }

    #[test]
    fn rejects_host_header() {
        let err = AuthHeader::parse("Host: attacker.com").expect_err("Host must be blocked");
        assert!(err.contains("not permitted"));
    }

    #[test]
    fn rejects_content_length() {
        assert!(AuthHeader::parse("Content-Length: 0").is_err());
    }

    #[test]
    fn rejects_transfer_encoding() {
        assert!(AuthHeader::parse("Transfer-Encoding: chunked").is_err());
    }

    #[test]
    fn rejects_connection_header() {
        assert!(AuthHeader::parse("Connection: upgrade").is_err());
    }

    #[test]
    fn forbidden_check_is_case_insensitive() {
        assert!(AuthHeader::parse("HOST: x").is_err());
        assert!(AuthHeader::parse("host: x").is_err());
        assert!(AuthHeader::parse("Host: x").is_err());
    }

    #[test]
    fn debug_redacts_value() {
        let auth = AuthHeader::parse("Authorization: Bearer topsecret").expect("valid");
        let dbg = format!("{auth:?}");
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains("topsecret"));
    }
}
