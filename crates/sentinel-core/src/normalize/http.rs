//! HTTP URL normalizer.
//!
//! Replaces numeric path segments with `{id}`, UUID segments with `{uuid}`,
//! strips query parameters, and prepends the HTTP method.

use regex::Regex;
use std::sync::LazyLock;

static UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
        .unwrap()
});

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

/// Normalize an HTTP target URL.
///
/// Strips scheme+authority, replaces numeric segments with `{id}`,
/// UUID segments with `{uuid}`, strips query params, and prepends the method.
pub fn normalize_http(method: &str, target: &str) -> HttpNormalized {
    let mut params = Vec::new();

    // Strip scheme + authority if present
    let path_and_query = strip_origin(target);

    // Strip query params
    let (path, query_params) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };

    // Collect query params as extracted values
    if let Some(q) = query_params {
        for pair in q.split('&') {
            params.push(pair.to_string());
        }
    }

    // Normalize path segments — build directly into a String
    let normalized_path: String = if path.is_empty() || path == "/" {
        "/".to_string()
    } else {
        let mut result = String::with_capacity(path.len() + 8);
        for (idx, seg) in path.split('/').enumerate() {
            if idx > 0 {
                result.push('/');
            }
            if seg.is_empty() {
                // leading or trailing slash — nothing to push
            } else if seg.len() == 36 && UUID_RE.is_match(seg) {
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
    };

    let template = format!("{method} {normalized_path}");
    HttpNormalized { template, params }
}

/// Strip scheme and authority from a URL, returning just the path (+ query).
fn strip_origin(target: &str) -> &str {
    target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
        .map_or(target, |rest| {
            rest.find('/').map_or("/", |idx| &rest[idx..])
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_path_with_numeric_id() {
        let r = normalize_http("GET", "/api/game/42/start");
        assert_eq!(r.template, "GET /api/game/{id}/start");
        assert_eq!(r.params, vec!["42"]);
    }

    #[test]
    fn uuid_segment() {
        let r = normalize_http("GET", "/api/account/a1b2c3d4-e5f6-7890-abcd-ef1234567890");
        assert_eq!(r.template, "GET /api/account/{uuid}");
        assert_eq!(r.params, vec!["a1b2c3d4-e5f6-7890-abcd-ef1234567890"]);
    }

    #[test]
    fn full_url_strips_origin() {
        let r = normalize_http("GET", "http://account-chat:5000/api/account/player-123");
        assert_eq!(r.template, "GET /api/account/player-123");
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
        assert_eq!(r.template, "POST /api/items/{id}");
        assert_eq!(r.params, vec!["expand=true", "99"]);
    }

    #[test]
    fn multiple_numeric_segments() {
        let r = normalize_http("DELETE", "/api/game/42/player/7");
        assert_eq!(r.template, "DELETE /api/game/{id}/player/{id}");
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
        let r = normalize_http("GET", "http://localhost:8080/api/items");
        assert_eq!(r.template, "GET /api/items");
    }

    #[test]
    fn url_without_path_returns_root() {
        let r = normalize_http("GET", "http://example.com");
        assert_eq!(r.template, "GET /");
        assert!(r.params.is_empty());
    }

    #[test]
    fn https_url_without_path() {
        let r = normalize_http("GET", "https://example.com");
        assert_eq!(r.template, "GET /");
    }

    #[test]
    fn non_uuid_36_char_segment_not_replaced() {
        // 36 chars but not a valid UUID format
        let r = normalize_http("GET", "/api/account/abcdefghijklmnopqrstuvwxyz1234567890");
        assert_eq!(
            r.template,
            "GET /api/account/abcdefghijklmnopqrstuvwxyz1234567890"
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
}
