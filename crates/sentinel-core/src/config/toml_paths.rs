//! TOML path-string normalization: rewrites Windows-style backslashes in
//! path-like config fields so they parse as literal separators, not escapes.

use std::borrow::Cow;

pub(super) const TOML_PATH_STRING_KEYS: &[&str] = &[
    "hourly_profiles_file",
    "calibration_file",
    "json_socket",
    "tls_cert_path",
    "tls_key_path",
    "storage_path",
    "toml_path",
];

/// Rewrite path-like config fields so Windows-style backslashes are treated
/// as literal separators instead of TOML escapes.
///
/// See `docs/design/07-CLI-CONFIG-RELEASE.md` > "Windows path normalization"
/// for the full algorithm, the UNC rule, and the fallback design.
pub(super) fn normalize_toml_path_strings(content: &str) -> Cow<'_, str> {
    let mut changed = false;
    let mut normalized = String::with_capacity(content.len());

    for line in content.split_inclusive('\n') {
        let rewritten = normalize_toml_path_line(line);
        changed |= matches!(rewritten, Cow::Owned(_));
        normalized.push_str(rewritten.as_ref());
    }

    if changed {
        Cow::Owned(normalized)
    } else {
        Cow::Borrowed(content)
    }
}

fn normalize_toml_path_line(line: &str) -> Cow<'_, str> {
    let leading_ws = line.len() - line.trim_start_matches([' ', '\t']).len();
    let trimmed = &line[leading_ws..];
    let Some(eq_idx) = trimmed.find('=') else {
        return Cow::Borrowed(line);
    };

    let key = trimmed[..eq_idx].trim();
    if !TOML_PATH_STRING_KEYS.contains(&key) {
        return Cow::Borrowed(line);
    }

    let after_eq = &trimmed[eq_idx + 1..];
    let value_ws = after_eq.len() - after_eq.trim_start_matches([' ', '\t']).len();
    let value_start = leading_ws + eq_idx + 1 + value_ws;
    let value = &line[value_start..];
    if !value.starts_with('"') {
        return Cow::Borrowed(line);
    }

    let Some(closing_quote) = find_basic_string_end(value) else {
        return Cow::Borrowed(line);
    };
    let inner = &value[1..closing_quote];
    let Cow::Owned(normalized_inner) = escape_toml_path_backslashes(inner) else {
        return Cow::Borrowed(line);
    };

    // Push the opening `"` explicitly so `value_start` is never used as
    // the end of an inclusive byte range. See design doc 07 > "Windows
    // path normalization" for the UTF-8 invariant.
    let mut out =
        String::with_capacity(line.len() + normalized_inner.len().saturating_sub(inner.len()));
    out.push_str(&line[..value_start]);
    out.push('"');
    out.push_str(&normalized_inner);
    out.push_str(&value[closing_quote..]);
    Cow::Owned(out)
}

/// Return the byte offset of the closing `"` that terminates a TOML basic
/// string starting at `value[0]` or `None` if the string is unterminated.
///
/// Linear: the `run` counter avoids an O(n²) lookbehind on inputs full of
/// `\`. See design doc 07 > "Windows path normalization" for context.
pub(super) fn find_basic_string_end(value: &str) -> Option<usize> {
    debug_assert!(value.starts_with('"'));

    let bytes = value.as_bytes();
    let mut run: usize = 0;
    let mut idx = 1;
    while idx < bytes.len() {
        match bytes[idx] {
            b'"' if run.is_multiple_of(2) => return Some(idx),
            b'\\' => run += 1,
            _ => run = 0,
        }
        idx += 1;
    }
    None
}

/// Escape single backslashes inside a TOML basic-string path so its value
/// round-trips as a literal separator.
///
/// See design doc 07 > "Windows path normalization" for the per-run rules
/// (single `\`, escape pairs, raw UNC prefix). Returns `Cow::Borrowed(inner)`
/// when no rewrite is needed.
pub(super) fn escape_toml_path_backslashes(inner: &str) -> Cow<'_, str> {
    if !inner.contains('\\') {
        return Cow::Borrowed(inner);
    }

    let bytes = inner.as_bytes();
    let mut out = String::with_capacity(inner.len() + 4);
    let mut changed = false;
    let mut idx = 0;

    while idx < bytes.len() {
        if bytes[idx] != b'\\' {
            idx = copy_until_backslash(inner, bytes, idx, &mut out);
            continue;
        }

        let run_start = idx;
        idx = skip_backslash_run(bytes, idx);
        let run_len = idx - run_start;
        let emit_len = backslash_emit_len(run_start, run_len, bytes.get(idx).copied());
        changed |= emit_len != run_len;
        for _ in 0..emit_len {
            out.push('\\');
        }
    }

    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(inner)
    }
}

/// Copy bytes from `start` up to (but not including) the next `\` into
/// `out`, and return the index where the run of `\` begins (or
/// `bytes.len()` if no more `\` is found).
fn copy_until_backslash(inner: &str, bytes: &[u8], start: usize, out: &mut String) -> usize {
    let mut idx = start;
    while idx < bytes.len() && bytes[idx] != b'\\' {
        idx += 1;
    }
    out.push_str(&inner[start..idx]);
    idx
}

/// Skip a run of consecutive `\` starting at `start` and return the index
/// of the first non-`\` byte (or `bytes.len()`).
fn skip_backslash_run(bytes: &[u8], start: usize) -> usize {
    let mut idx = start;
    while idx < bytes.len() && bytes[idx] == b'\\' {
        idx += 1;
    }
    idx
}

/// Decide how many `\` to emit for a run of `run_len` backslashes
/// starting at byte offset `run_start`. `next_byte` is the byte
/// immediately after the run (used to disambiguate UNC prefixes).
fn backslash_emit_len(run_start: usize, run_len: usize, next_byte: Option<u8>) -> usize {
    let raw_unc_prefix = run_start == 0 && run_len == 2 && next_byte != Some(b'\\');
    if raw_unc_prefix {
        4
    } else if run_len == 1 {
        2
    } else {
        run_len
    }
}
