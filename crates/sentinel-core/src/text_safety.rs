//! Pure helpers used by every text renderer that prints attacker-controlled
//! strings to a terminal: ANSI/OSC 8 stripping, BiDi/invisible-format
//! filtering and HTTPS URL gating.

use std::borrow::Cow;

/// True for the control ranges that a terminal can act on: C0 (`0x00..0x20`),
/// DEL (`0x7f`), and C1 (`0x80..=0x9F`). The C1 set includes the single-byte
/// CSI (`U+009B`), ST (`U+009C`) and OSC (`U+009D`) introducers honoured by
/// xterm and other VT-family terminals when 8-bit controls are enabled.
fn is_dangerous_control(c: char) -> bool {
    let code = c as u32;
    code < 0x20 || code == 0x7f || (0x80..=0x9F).contains(&code)
}

/// Return true for Unicode `BiDi` override and invisible format characters
/// that can confuse text renderers (Trojan Source class of attack,
/// CVE-2021-42574).
pub(crate) fn is_bidi_or_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}' // Soft hyphen (visually invisible mid-word)
        | '\u{061C}' // Arabic Letter Mark (BiDi formatting)
        | '\u{180E}' // Mongolian Vowel Separator (deprecated invisible)
        | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
        | '\u{2060}'..='\u{2064}' // Word Joiner + invisible operators
        | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
        | '\u{200B}'..='\u{200F}' // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | '\u{FEFF}' // BOM / zero-width no-break space
    )
}

/// Drop Unicode `BiDi`-override and invisible-format characters from a
/// free-text string before emitting it. Reused at signature construction
/// time (`acknowledgments::compute_signature`) so canonical signatures
/// cannot carry trojan characters that would spoof ack matching.
///
/// Probe-before-allocate: real-world inputs (service names, endpoints)
/// are clean, so the common case returns `Cow::Borrowed` zero-copy.
pub(crate) fn strip_bidi_and_invisible(s: &str) -> Cow<'_, str> {
    if s.chars().any(is_bidi_or_invisible) {
        Cow::Owned(s.chars().filter(|c| !is_bidi_or_invisible(*c)).collect())
    } else {
        Cow::Borrowed(s)
    }
}

/// Replace control characters with `?` and drop `BiDi`/invisible format
/// characters, so an attacker-controlled string in a JSON `Report` cannot
/// inject ANSI escape sequences, OSC 8 hyperlinks, cursor controls,
/// Trojan Source reordering or other terminal payloads. Covers C0, DEL
/// and C1 ranges (see [`is_dangerous_control`]) plus the
/// [`is_bidi_or_invisible`] set.
#[must_use]
pub fn sanitize_for_terminal(input: &str) -> Cow<'_, str> {
    let stripped = strip_bidi_and_invisible(input);
    if !stripped.chars().any(is_dangerous_control) {
        return stripped;
    }
    let cleaned: String = stripped
        .chars()
        .map(|c| if is_dangerous_control(c) { '?' } else { c })
        .collect();
    Cow::Owned(cleaned)
}

/// Strip markdown inline-code backticks from a recommendation string for
/// plain-text sinks (terminal, SARIF). The HTML report keeps the backticks
/// and renders the delimited tokens as code chips, everywhere else they are
/// noise. Returns the input untouched when it carries no backticks.
#[must_use]
pub fn strip_code_ticks(input: &str) -> Cow<'_, str> {
    if input.contains('`') {
        Cow::Owned(input.replace('`', ""))
    } else {
        Cow::Borrowed(input)
    }
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
    fn sanitize_strips_bidi_and_invisible_chars() {
        // Trojan Source class: RLO and ZWSP must not reach the terminal.
        let dirty = "user\u{202E}nimda\u{200B}x";
        assert_eq!(sanitize_for_terminal(dirty).as_ref(), "usernimdax");
    }

    #[test]
    fn sanitize_strips_bidi_and_replaces_controls_together() {
        let dirty = "a\u{202E}\x1bb";
        assert_eq!(sanitize_for_terminal(dirty).as_ref(), "a?b");
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
