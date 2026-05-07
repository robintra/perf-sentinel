//! Shared URL helpers for HTTP trace ingestion modules.
//!
//! Both `tempo` and `jaeger_query` build query strings by hand and
//! validate user-supplied endpoints with the same rules. This module
//! hosts those two helpers once, each module applies its own error
//! type at the call site.

/// Minimal percent-encoding for URI query parameter values.
/// Encodes `&`, `=`, `#`, `+`, space, and any non-ASCII byte.
///
/// The alternative would be pulling in the `percent-encoding` crate
/// for twelve lines, so we keep a hand-rolled version.
#[must_use]
pub(crate) fn percent_encode_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'&' | b'=' | b'#' | b'+' | b' ' | b'%' | 0x00..=0x1F | 0x7F..=0xFF => {
                out.push('%');
                out.push(char::from(b"0123456789ABCDEF"[(b >> 4) as usize]));
                out.push(char::from(b"0123456789ABCDEF"[(b & 0x0f) as usize]));
            }
            _ => out.push(char::from(b)),
        }
    }
    out
}

/// Validate that an HTTP endpoint string is `http://` or `https://`
/// scheme and does not embed credentials in the authority section.
///
/// The check is intentionally narrow (authority only). A literal `@`
/// in the path or query string stays accepted so URIs like
/// `/api/traces?owner=foo%40example.com` work. Returns the error
/// message as a `&'static str` the caller converts into its own error
/// variant.
///
/// # Errors
///
/// Returns `Err(&'static str)` when the scheme is not `http(s)://` or
/// when userinfo is present in the authority.
pub(crate) fn validate_http_endpoint(endpoint: &str) -> Result<(), &'static str> {
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err("endpoint must start with http:// or https://");
    }
    // Control bytes can survive `hyper::Uri` on some path shapes and
    // land verbatim in tracing output via the redacted endpoint.
    if endpoint.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err("endpoint must not contain ASCII control characters");
    }
    let after_scheme = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or("");
    let authority_end = after_scheme.find(['/', '?']).unwrap_or(after_scheme.len());
    if after_scheme[..authority_end].contains('@') {
        return Err("endpoint must not contain credentials (user:pass@host)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_escapes_reserved_bytes() {
        assert_eq!(percent_encode_query_value("a&b=c"), "a%26b%3Dc");
        assert_eq!(percent_encode_query_value("hello world"), "hello%20world");
        assert_eq!(percent_encode_query_value("plain"), "plain");
    }

    #[test]
    fn validate_http_endpoint_accepts_plain_http() {
        assert!(validate_http_endpoint("http://tempo:3200").is_ok());
        assert!(validate_http_endpoint("https://jaeger.prod/api").is_ok());
    }

    #[test]
    fn validate_http_endpoint_rejects_non_http_scheme() {
        assert!(validate_http_endpoint("ftp://x").is_err());
        assert!(validate_http_endpoint("x").is_err());
    }

    #[test]
    fn validate_http_endpoint_rejects_credentials() {
        assert!(validate_http_endpoint("http://user:pass@host").is_err());
        assert!(validate_http_endpoint("https://u@jaeger").is_err());
    }

    #[test]
    fn validate_http_endpoint_accepts_at_in_query_string() {
        assert!(validate_http_endpoint("http://host/api?owner=foo%40example.com").is_ok());
    }

    #[test]
    fn validate_http_endpoint_rejects_control_chars() {
        assert!(validate_http_endpoint("http://host\nfoo").is_err());
        assert!(validate_http_endpoint("http://host\u{7f}foo").is_err());
    }
}
