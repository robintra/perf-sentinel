//! Pure helpers used by every text renderer that prints attacker-controlled
//! strings to a terminal: ANSI/OSC 8 stripping and HTTPS URL gating.

use std::borrow::Cow;

/// True for the control ranges that a terminal can act on: C0 (`0x00..0x20`),
/// DEL (`0x7f`), and C1 (`0x80..=0x9F`). The C1 set includes the single-byte
/// CSI (`U+009B`), ST (`U+009C`) and OSC (`U+009D`) introducers honoured by
/// xterm and other VT-family terminals when 8-bit controls are enabled.
fn is_dangerous_control(c: char) -> bool {
    let code = c as u32;
    code < 0x20 || code == 0x7f || (0x80..=0x9F).contains(&code)
}

/// Replace control characters with `?` so an attacker-controlled string in a
/// JSON `Report` cannot inject ANSI escape sequences, OSC 8 hyperlinks, cursor
/// controls or other terminal payloads. Covers C0, DEL and C1 ranges (see
/// [`is_dangerous_control`]).
#[must_use]
pub fn sanitize_for_terminal(input: &str) -> Cow<'_, str> {
    if !input.chars().any(is_dangerous_control) {
        return Cow::Borrowed(input);
    }
    let cleaned: String = input
        .chars()
        .map(|c| if is_dangerous_control(c) { '?' } else { c })
        .collect();
    Cow::Owned(cleaned)
}

/// Return the URL only when it is HTTPS and free of control chars.
/// Defends against schema spoofing and OSC 8 hyperlink injection from
/// `suggested_fix.reference_url` values planted in a deserialized report.
#[must_use]
pub fn safe_url(url: &str) -> Option<&str> {
    if url.starts_with("https://") && !url.chars().any(is_dangerous_control) {
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
    fn sanitize_replaces_c1_control_range() {
        // U+0080..=U+009F encodes as `0xC2 0x80..0x9F` in UTF-8 and survives
        // a byte-level filter that only checks `< 0x20`. xterm with 8-bit
        // controls enabled honours U+009B (CSI), U+009C (ST), U+009D (OSC).
        let dirty = "a\u{009b}[31mb\u{009d}OSC\u{009c}c";
        let cleaned = sanitize_for_terminal(dirty);
        assert_eq!(cleaned.as_ref(), "a?[31mb?OSC?c");
    }

    #[test]
    fn safe_url_rejects_c1_control_chars() {
        assert_eq!(safe_url("https://a.com/\u{009b}[0m"), None);
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
