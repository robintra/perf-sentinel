//! Pure helpers used by every text renderer that prints attacker-controlled
//! strings to a terminal: ANSI/OSC 8 stripping and HTTPS URL gating.

use std::borrow::Cow;

/// Replace ASCII control characters with `?` so an attacker-controlled
/// string in a JSON `Report` cannot inject ANSI escape sequences,
/// OSC 8 hyperlinks, cursor controls or other terminal payloads.
#[must_use]
pub fn sanitize_for_terminal(input: &str) -> Cow<'_, str> {
    if !input.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Cow::Borrowed(input);
    }
    let cleaned: String = input
        .chars()
        .map(|c| {
            let code = c as u32;
            if code < 0x20 || code == 0x7f { '?' } else { c }
        })
        .collect();
    Cow::Owned(cleaned)
}

/// Return the URL only when it is HTTPS and free of control chars.
/// Defends against schema spoofing and OSC 8 hyperlink injection from
/// `suggested_fix.reference_url` values planted in a deserialized report.
#[must_use]
pub fn safe_url(url: &str) -> Option<&str> {
    if url.starts_with("https://") && !url.bytes().any(|b| b < 0x20 || b == 0x7f) {
        Some(url)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_borrows_clean_input() {
        match sanitize_for_terminal("clean ascii") {
            Cow::Borrowed(s) => assert_eq!(s, "clean ascii"),
            Cow::Owned(_) => panic!("clean input should not allocate"),
        }
    }

    #[test]
    fn sanitize_replaces_all_control_chars() {
        let dirty = "a\x1bb\x07c\x00d\x7fe\nf";
        let cleaned = sanitize_for_terminal(dirty);
        assert_eq!(cleaned.as_ref(), "a?b?c?d?e?f");
    }

    #[test]
    fn safe_url_accepts_clean_https() {
        assert_eq!(
            safe_url("https://example.com/x"),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn safe_url_rejects_non_https_and_control_chars() {
        assert_eq!(safe_url("http://example.com"), None);
        assert_eq!(safe_url("javascript:alert(1)"), None);
        assert_eq!(safe_url("ftp://example.com"), None);
        assert_eq!(safe_url("https://a.com/\x1b[0m"), None);
        assert_eq!(safe_url(""), None);
    }
}
