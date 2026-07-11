//! HTTP URL normalizer.
//!
//! Replaces numeric path segments with `{id}`, UUID segments with `{uuid}`,
//! strips query parameters, and prepends the HTTP method.

use std::borrow::Cow;

/// Check if a string is a UUID (8-4-4-4-12 hex with dashes).
/// Hand-coded for performance, avoids regex engine overhead on the hot path.
fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let b = s.as_bytes();
    b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, &c)| matches!(i, 8 | 13 | 18 | 23) || c.is_ascii_hexdigit())
}

/// Result of HTTP URL normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpNormalized {
    pub template: String,
    pub params: Vec<String>,
}

/// Check if a segment is purely numeric (ASCII digits, non-empty).
fn is_numeric(seg: &str) -> bool {
    !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit())
}

/// Count occurrences of `target` in `s`.
fn bytecount(s: &str, target: u8) -> usize {
    s.bytes().filter(|&b| b == target).count()
}

/// Normalize an HTTP target URL.
///
/// Replaces numeric segments with `{id}`, UUID segments with `{uuid}`,
/// strips query params, and prepends the method. The callee host is kept in
/// the template for DNS-addressed calls (`GET user-svc/api/x`) so two calls
/// to the same path on different backends do not merge into one group and
/// raise a false redundant/N+1 finding. IP-literal authorities are dropped,
/// so load-balanced replicas (pods behind one service) still group together.
#[must_use]
pub fn normalize_http(method: &str, target: &str) -> HttpNormalized {
    // Split scheme + authority from the path, keeping the authority so a
    // DNS host can stay in the grouping template.
    let (authority, path_and_query) = split_origin(target);

    // Strip query params
    let (path, query_params) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };

    // Collect query params as extracted values (capped to prevent unbounded allocation).
    // Each pair is heap-allocated via to_string(). A Cow<str> backed by the source
    // would avoid this, but NormalizedEvent.params is Vec<String> throughout the
    // pipeline, so the allocation is unavoidable without a larger refactor. Pre-size
    // the Vec from the ampersand count to avoid the doubling growth on the hot path.
    let mut params = match query_params {
        Some(q) => {
            let cap = (bytecount(q, b'&') + 1).min(100);
            let mut out = Vec::with_capacity(cap);
            for pair in q.split('&').take(100) {
                out.push(pair.to_string());
            }
            out
        }
        None => Vec::new(),
    };

    let normalized_path = normalize_path_segments(path, &mut params);

    // `normalized_path` starts with `/` whenever an authority was present
    // (it is sliced from the authority's trailing `/`), so a DNS host slots
    // in as `GET host/path` with no extra separator handling.
    let template = match authority.and_then(host_group_prefix) {
        Some(host) => format!("{method} {host}{normalized_path}"),
        None => format!("{method} {normalized_path}"),
    };
    HttpNormalized { template, params }
}

/// Normalize path segments: replace numeric with `{id}`, UUIDs with `{uuid}`.
fn normalize_path_segments(path: &str, params: &mut Vec<String>) -> String {
    if path.is_empty() || path == "/" {
        return "/".to_string();
    }
    let mut result = String::with_capacity(path.len() + 8);
    for (idx, seg) in path.split('/').enumerate() {
        if idx > 0 {
            result.push('/');
        }
        if seg.is_empty() {
            // leading or trailing slash
        } else if is_uuid(seg) {
            params.push(seg.to_string());
            result.push_str("{uuid}");
        } else if is_numeric(seg) {
            params.push(seg.to_string());
            result.push_str("{id}");
        } else {
            result.push_str(seg);
        }
    }
    result
}

/// Split scheme + authority from an `http(s)` URL. Returns
/// `(authority, path_and_query)`: `authority` is `None` for a relative URL
/// (no scheme), and `path_and_query` defaults to `/` when the URL has an
/// authority but no path (`http://host`).
fn split_origin(target: &str) -> (Option<&str>, &str) {
    match target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
    {
        // RFC 3986: the authority ends at the first '/', '?' or '#'.
        // Terminating only on '/' would fold a query or fragment into the
        // authority, leaking it (verbatim, secrets included) into the
        // grouping template on a URL with no path (`http://host?token=...`).
        // A '#' terminator means there is no path (fragments never carry a
        // path), and the fragment is never sent to the server, so the path
        // is just `/`. A fragment that follows an actual path is left in the
        // path unchanged (it is handled by `normalize_path_segments`).
        Some(rest) => match rest.find(['/', '?', '#']) {
            Some(idx) if rest.as_bytes()[idx] == b'#' => (Some(&rest[..idx]), "/"),
            Some(idx) => (Some(&rest[..idx]), &rest[idx..]),
            None => (Some(rest), "/"),
        },
        None => (None, target),
    }
}

/// The DNS host to keep in the grouping template, or `None` when the
/// authority is an IP literal (kept anonymous so load-balanced replicas
/// still dedup) or empty. Strips RFC 3986 userinfo and the port, drops a
/// single trailing DNS root dot (`svc.` == `svc`), and lowercases the host
/// (DNS is case-insensitive) so casing variants group.
fn host_group_prefix(authority: &str) -> Option<Cow<'_, str>> {
    // Strip userinfo: "user:pass@host:port" -> "host:port". Safe because
    // split_origin already trimmed any query/fragment (which may contain
    // '@'), so the remaining '@' can only be the userinfo delimiter.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 literal ("[::1]:8080"): always an address, drop it.
    if host_port.starts_with('[') {
        return None;
    }
    // Strip the port: "host:port" -> "host", then the DNS root dot.
    let host = host_port.split(':').next().unwrap_or(host_port);
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || is_ipv4_literal(host) {
        return None;
    }
    if host.bytes().any(|b| b.is_ascii_uppercase()) {
        Some(Cow::Owned(host.to_ascii_lowercase()))
    } else {
        Some(Cow::Borrowed(host))
    }
}

/// Whether `host` is a dotted-decimal IPv4 literal (exactly four all-digit
/// octets). Such authorities are load-balanced replica addresses, so they
/// are dropped from the template while DNS hostnames are kept. Counts in
/// `usize` and bails past four labels so a host with hundreds of numeric
/// dot-labels cannot overflow the counter.
fn is_ipv4_literal(host: &str) -> bool {
    let mut octets = 0usize;
    for part in host.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        octets += 1;
        if octets > 4 {
            return false;
        }
    }
    octets == 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_path_with_numeric_id() {
        let r = normalize_http("GET", "/api/orders/42/submit");
        assert_eq!(r.template, "GET /api/orders/{id}/submit");
        assert_eq!(r.params, vec!["42"]);
    }

    #[test]
    fn uuid_segment() {
        let r = normalize_http("GET", "/api/users/a1b2c3d4-e5f6-7890-abcd-ef1234567890");
        assert_eq!(r.template, "GET /api/users/{uuid}");
        assert_eq!(r.params, vec!["a1b2c3d4-e5f6-7890-abcd-ef1234567890"]);
    }

    #[test]
    fn full_url_keeps_dns_host() {
        // The DNS host stays in the template (and the port is dropped) so
        // calls to the same path on different backends stay distinct groups.
        let r = normalize_http("GET", "http://user-svc:5000/api/users/user-123");
        assert_eq!(r.template, "GET user-svc/api/users/user-123");
    }

    #[test]
    fn query_params_stripped() {
        let r = normalize_http("GET", "/api/users?page=2&size=10");
        assert_eq!(r.template, "GET /api/users");
        assert_eq!(r.params, vec!["page=2", "size=10"]);
    }

    #[test]
    fn full_url_with_query() {
        let r = normalize_http("POST", "https://svc.internal/api/items/99?expand=true");
        assert_eq!(r.template, "POST svc.internal/api/items/{id}");
        assert_eq!(r.params, vec!["expand=true", "99"]);
    }

    #[test]
    fn multiple_numeric_segments() {
        let r = normalize_http("DELETE", "/api/orders/42/items/7");
        assert_eq!(r.template, "DELETE /api/orders/{id}/items/{id}");
        assert_eq!(r.params, vec!["42", "7"]);
    }

    #[test]
    fn root_path() {
        let r = normalize_http("GET", "/");
        assert_eq!(r.template, "GET /");
        assert!(r.params.is_empty());
    }

    #[test]
    fn no_numeric_or_uuid_segments() {
        let r = normalize_http("GET", "/api/health");
        assert_eq!(r.template, "GET /api/health");
        assert!(r.params.is_empty());
    }

    #[test]
    fn port_in_url_not_treated_as_id() {
        // Host kept, port dropped, and the port digits are not an {id}.
        let r = normalize_http("GET", "http://localhost:8080/api/items");
        assert_eq!(r.template, "GET localhost/api/items");
    }

    #[test]
    fn url_without_path_keeps_host() {
        let r = normalize_http("GET", "http://example.com");
        assert_eq!(r.template, "GET example.com/");
        assert!(r.params.is_empty());
    }

    #[test]
    fn https_url_without_path() {
        let r = normalize_http("GET", "https://example.com");
        assert_eq!(r.template, "GET example.com/");
    }

    #[test]
    fn dns_hosts_disambiguate_same_path() {
        // The core fix: same method + path on two DNS backends must NOT
        // collapse into one template (which would raise a false redundant).
        let a = normalize_http("POST", "http://ms-23205/vs2nqhh1hq");
        let b = normalize_http("POST", "http://ms-53745/vs2nqhh1hq");
        assert_eq!(a.template, "POST ms-23205/vs2nqhh1hq");
        assert_eq!(b.template, "POST ms-53745/vs2nqhh1hq");
        assert_ne!(a.template, b.template);
    }

    #[test]
    fn ipv4_hosts_are_dropped_keeping_replica_dedup() {
        // Load-balanced pod replicas share a service; their IP authorities
        // must collapse to one template so the dedup stays intentional.
        let a = normalize_http("GET", "http://10.0.0.1:8080/api/x");
        let b = normalize_http("GET", "http://10.0.0.2:8080/api/x");
        assert_eq!(a.template, "GET /api/x");
        assert_eq!(a.template, b.template);
    }

    #[test]
    fn ipv6_host_is_dropped() {
        let r = normalize_http("GET", "http://[2001:db8::1]:8080/api/x");
        assert_eq!(r.template, "GET /api/x");
    }

    #[test]
    fn host_is_lowercased() {
        let r = normalize_http("GET", "http://User-SVC.Example.COM/api/x");
        assert_eq!(r.template, "GET user-svc.example.com/api/x");
    }

    #[test]
    fn userinfo_is_stripped_from_host() {
        let r = normalize_http("GET", "http://user:pass@svc.internal/api/x");
        assert_eq!(r.template, "GET svc.internal/api/x");
    }

    #[test]
    fn relative_url_has_no_host() {
        // No authority to key on, behavior unchanged from before the fix.
        let r = normalize_http("GET", "/api/x");
        assert_eq!(r.template, "GET /api/x");
    }

    #[test]
    fn query_only_url_does_not_leak_into_host() {
        // Regression: a query on a path-less URL must not fold into the
        // authority and leak (e.g. a token) verbatim into the template.
        let r = normalize_http("GET", "http://api.example.com?token=abc123secret");
        assert_eq!(r.template, "GET api.example.com/");
        assert!(!r.template.contains("token"), "{}", r.template);
    }

    #[test]
    fn query_with_userinfo_does_not_leak() {
        let r = normalize_http("GET", "http://user:pass@svc.internal?token=xyz");
        assert_eq!(r.template, "GET svc.internal/");
        assert!(!r.template.contains("token"), "{}", r.template);
    }

    #[test]
    fn fragment_only_url_does_not_pollute_host() {
        // A path-less fragment is never sent to the server, so it is dropped
        // and must not become part of the host token.
        let r = normalize_http("GET", "http://svc.internal#section");
        assert_eq!(r.template, "GET svc.internal/");
    }

    #[test]
    fn trailing_dns_dot_groups_with_bare_host() {
        // `svc.` (DNS root label) and `svc` are the same host.
        let dotted = normalize_http("GET", "http://user-svc./api/x");
        let bare = normalize_http("GET", "http://user-svc/api/x");
        assert_eq!(dotted.template, "GET user-svc/api/x");
        assert_eq!(dotted.template, bare.template);
    }

    #[test]
    fn pathological_numeric_host_does_not_overflow() {
        // 260 all-numeric dot-labels must not overflow the octet counter
        // (debug panic) nor wrap-classify as an IPv4 literal.
        let host = vec!["1"; 260].join(".");
        let r = normalize_http("GET", &format!("http://{host}/x"));
        // Not four octets, so it is treated as a DNS host and kept.
        assert_eq!(r.template, format!("GET {host}/x"));
        assert!(super::is_ipv4_literal("1.2.3.4"));
        assert!(!super::is_ipv4_literal(&host));
    }

    #[test]
    fn non_uuid_36_char_segment_not_replaced() {
        // 36 chars but not a valid UUID format
        let r = normalize_http("GET", "/api/users/abcdefghijklmnopqrstuvwxyz1234567890");
        assert_eq!(
            r.template,
            "GET /api/users/abcdefghijklmnopqrstuvwxyz1234567890"
        );
        assert!(r.params.is_empty());
    }

    #[test]
    fn empty_path() {
        let r = normalize_http("GET", "");
        assert_eq!(r.template, "GET /");
    }

    #[test]
    fn trailing_slash() {
        let r = normalize_http("GET", "/api/users/");
        assert_eq!(r.template, "GET /api/users/");
        assert!(r.params.is_empty());
    }

    #[test]
    fn single_numeric_segment() {
        let r = normalize_http("GET", "/42");
        assert_eq!(r.template, "GET /{id}");
        assert_eq!(r.params, vec!["42"]);
    }

    #[test]
    fn mixed_uuid_and_numeric() {
        let r = normalize_http(
            "PUT",
            "/api/org/a1b2c3d4-e5f6-7890-abcd-ef1234567890/user/99",
        );
        assert_eq!(r.template, "PUT /api/org/{uuid}/user/{id}");
        assert_eq!(r.params, vec!["a1b2c3d4-e5f6-7890-abcd-ef1234567890", "99"]);
    }

    #[test]
    fn is_uuid_valid() {
        assert!(is_uuid("a1b2c3d4-e5f6-7890-abcd-ef1234567890"));
        assert!(is_uuid("00000000-0000-0000-0000-000000000000"));
        assert!(is_uuid("AAAABBBB-CCCC-DDDD-EEEE-FFFFFFFFFFFF"));
    }

    #[test]
    fn is_uuid_invalid() {
        assert!(!is_uuid("not-a-uuid-at-all"));
        assert!(!is_uuid("")); // too short
        assert!(!is_uuid("a1b2c3d4-e5f6-7890-abcd-ef123456789")); // 35 chars
        assert!(!is_uuid("a1b2c3d4-e5f6-7890-abcd-ef12345678901")); // 37 chars
        assert!(!is_uuid("a1b2c3d4xe5f6-7890-abcd-ef1234567890")); // wrong dash pos
        assert!(!is_uuid("g1b2c3d4-e5f6-7890-abcd-ef1234567890")); // 'g' not hex
    }

    #[test]
    fn uppercase_uuid_detected() {
        let r = normalize_http("GET", "/api/item/A1B2C3D4-E5F6-7890-ABCD-EF1234567890");
        assert_eq!(r.template, "GET /api/item/{uuid}");
    }

    // -- Fragment handling --

    #[test]
    fn fragment_not_stripped_from_path() {
        // Fragments are rare in server-side URLs; the segment "42#section" is not
        // purely numeric so it passes through as-is (fragment is not separated)
        let r = normalize_http("GET", "/api/users/42#section");
        assert_eq!(r.template, "GET /api/users/42#section");
    }

    // -- Malformed/edge-case query params --

    #[test]
    fn trailing_question_mark_only() {
        let r = normalize_http("GET", "/api/users?");
        assert_eq!(r.template, "GET /api/users");
        assert_eq!(r.params, vec![""]);
    }

    #[test]
    fn empty_query_param_values() {
        let r = normalize_http("GET", "/api/users?id=&name=");
        assert_eq!(r.template, "GET /api/users");
        assert_eq!(r.params, vec!["id=", "name="]);
    }

    #[test]
    fn double_ampersand_in_query() {
        let r = normalize_http("GET", "/api/users?a=1&&b=2");
        assert_eq!(r.template, "GET /api/users");
        assert_eq!(r.params, vec!["a=1", "", "b=2"]);
    }

    // -- Double slashes --

    #[test]
    fn double_slash_in_path_preserved() {
        let r = normalize_http("GET", "/api//users/42");
        assert_eq!(r.template, "GET /api//users/{id}");
    }

    // -- URL-encoded segments (pass through as-is) --

    #[test]
    fn url_encoded_numeric_not_detected() {
        // %34%32 = "42" but URL-encoded, not decoded before detection
        let r = normalize_http("GET", "/api/users/%34%32");
        assert_eq!(r.template, "GET /api/users/%34%32");
        assert!(r.params.is_empty());
    }

    // -- Query params capped at 100 --

    #[test]
    fn query_params_capped_at_100() {
        let params: Vec<String> = (0..200).map(|i| format!("p{i}={i}")).collect();
        let url = format!("/api/test?{}", params.join("&"));
        let r = normalize_http("GET", &url);
        assert_eq!(r.params.len(), 100);
    }
}
