//! Parse a user-supplied `--auth-header "Name: Value"` line into a
//! hyper-safe `(HeaderName, HeaderValue)` pair, shared between the
//! `tempo` and `jaeger-query` subcommands.
//!
//! The parsed value is marked `sensitive` so hyper omits it from its
//! own debug output and from HTTP/2 HPACK compression tables. The
//! struct also implements a manual `Debug` that never prints the
//! value, so a logged `AuthHeader` never leaks the credential.

use hyper::header::{HeaderName, HeaderValue};

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
    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        let (name_raw, value_raw) = raw
            .split_once(':')
            .ok_or("auth header must be 'Name: Value' format")?;
        let name = HeaderName::from_bytes(name_raw.trim().as_bytes())
            .map_err(|_| "invalid auth header name")?;
        let mut value = HeaderValue::from_str(value_raw.trim())
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
    fn rejects_crlf_in_value() {
        assert!(AuthHeader::parse("X: a\r\nY: b").is_err());
    }

    #[test]
    fn debug_redacts_value() {
        let auth = AuthHeader::parse("Authorization: Bearer topsecret").expect("valid");
        let dbg = format!("{auth:?}");
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains("topsecret"));
    }
}
