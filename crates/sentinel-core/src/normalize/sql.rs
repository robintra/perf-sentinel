//! Homemade SQL tokenizer/normalizer.
//!
//! Replaces numeric literals, string literals, and UUIDs with `?` placeholders.
//! Collapses `IN (?, ?, ?)` into `IN (?)`.

use regex::Regex;
use std::borrow::Cow;
use std::sync::LazyLock;

static IN_LIST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)IN\s*\(\s*\?(?:\s*,\s*\?)*\s*\)").unwrap());

/// Result of SQL normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlNormalized {
    pub template: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Normal,
    InString,
    InNumber,
}

/// Mutable tokenizer state carried between steps.
struct Tokenizer<'a> {
    query: &'a str,
    bytes: &'a [u8],
    template: String,
    params: Vec<String>,
    i: usize,
    state: State,
    current_value: String,
    seen_dot: bool,
    has_in_list: bool,
    normal_start: usize,
}

/// Normalize a SQL query by replacing literal values with `?` placeholders.
#[must_use]
pub fn normalize_sql(query: &str) -> SqlNormalized {
    let mut t = Tokenizer {
        query,
        bytes: query.as_bytes(),
        template: String::with_capacity(query.len()),
        params: Vec::new(),
        i: 0,
        state: State::Normal,
        current_value: String::new(),
        seen_dot: false,
        has_in_list: false,
        normal_start: 0,
    };

    while t.i < t.bytes.len() {
        match t.state {
            State::Normal => step_normal(&mut t),
            State::InString => step_in_string(&mut t),
            State::InNumber => step_in_number(&mut t),
        }
    }

    flush_pending(&mut t);
    collapse_in_lists(t.template, t.has_in_list, t.params)
}

fn step_normal(t: &mut Tokenizer<'_>) {
    let b = t.bytes[t.i];
    if b == b'\'' {
        flush_normal_run(t);
        t.state = State::InString;
        t.current_value.clear();
    } else if b.is_ascii_digit() && !is_identifier_byte_before(t.i, t.bytes) {
        flush_normal_run(t);
        t.state = State::InNumber;
        t.seen_dot = false;
        t.current_value.clear();
        t.current_value.push(b as char);
    } else if !t.has_in_list {
        t.has_in_list = is_in_keyword(t.i, t.bytes);
    }
    t.i += 1;
}

fn step_in_string(t: &mut Tokenizer<'_>) {
    let b = t.bytes[t.i];
    if b == b'\'' {
        if t.i + 1 < t.bytes.len() && t.bytes[t.i + 1] == b'\'' {
            t.current_value.push('\'');
            t.i += 2;
        } else {
            t.params.push(std::mem::take(&mut t.current_value));
            t.template.push('?');
            t.state = State::Normal;
            t.i += 1;
            t.normal_start = t.i;
        }
    } else {
        t.current_value.push(b as char);
        t.i += 1;
    }
}

fn step_in_number(t: &mut Tokenizer<'_>) {
    let b = t.bytes[t.i];
    if b.is_ascii_digit() {
        t.current_value.push(b as char);
        t.i += 1;
    } else if b == b'.' && !t.seen_dot {
        t.seen_dot = true;
        t.current_value.push('.');
        t.i += 1;
    } else {
        t.params.push(std::mem::take(&mut t.current_value));
        t.template.push('?');
        t.state = State::Normal;
        t.normal_start = t.i;
    }
}

fn flush_normal_run(t: &mut Tokenizer<'_>) {
    if t.i > t.normal_start {
        t.template.push_str(&t.query[t.normal_start..t.i]);
    }
}

fn flush_pending(t: &mut Tokenizer<'_>) {
    match t.state {
        State::InNumber | State::InString => {
            t.params.push(std::mem::take(&mut t.current_value));
            t.template.push('?');
        }
        State::Normal => {
            let len = t.bytes.len();
            if len > t.normal_start {
                t.template.push_str(&t.query[t.normal_start..len]);
            }
        }
    }
}

fn is_in_keyword(i: usize, bytes: &[u8]) -> bool {
    let b = bytes[i];
    (b == b'I' || b == b'i')
        && i + 1 < bytes.len()
        && (bytes[i + 1] == b'N' || bytes[i + 1] == b'n')
        && (i == 0 || bytes[i - 1].is_ascii_whitespace())
        && (i + 2 >= bytes.len() || !bytes[i + 2].is_ascii_alphanumeric())
}

fn collapse_in_lists(template: String, has_in_list: bool, params: Vec<String>) -> SqlNormalized {
    let template = if has_in_list {
        match IN_LIST_RE.replace_all(&template, "IN (?)") {
            Cow::Borrowed(_) => template,
            Cow::Owned(s) => s,
        }
    } else {
        template
    };
    SqlNormalized { template, params }
}

/// Check if the byte before position `i` is part of an identifier
/// (letter, digit, or underscore), meaning the digit at `i` is NOT
/// the start of a standalone numeric literal.
fn is_identifier_byte_before(i: usize, bytes: &[u8]) -> bool {
    if i == 0 {
        return false;
    }
    let prev = bytes[i - 1];
    prev.is_ascii_alphanumeric() || prev == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_literal() {
        let r = normalize_sql("SELECT * FROM player WHERE game_id = 42");
        assert_eq!(r.template, "SELECT * FROM player WHERE game_id = ?");
        assert_eq!(r.params, vec!["42"]);
    }

    #[test]
    fn float_literal() {
        let r = normalize_sql("SELECT * FROM t WHERE price > 3.14");
        assert_eq!(r.template, "SELECT * FROM t WHERE price > ?");
        assert_eq!(r.params, vec!["3.14"]);
    }

    #[test]
    fn string_literal() {
        let r = normalize_sql("SELECT * FROM users WHERE name = 'Alice'");
        assert_eq!(r.template, "SELECT * FROM users WHERE name = ?");
        assert_eq!(r.params, vec!["Alice"]);
    }

    #[test]
    fn uuid_in_string() {
        let r = normalize_sql("SELECT * FROM t WHERE id = 'a1b2c3d4-e5f6-7890-abcd-ef1234567890'");
        assert_eq!(r.template, "SELECT * FROM t WHERE id = ?");
        assert_eq!(r.params, vec!["a1b2c3d4-e5f6-7890-abcd-ef1234567890"]);
    }

    #[test]
    fn in_list_collapsed() {
        let r = normalize_sql("SELECT * FROM t WHERE id IN (1, 2, 3)");
        assert_eq!(r.template, "SELECT * FROM t WHERE id IN (?)");
        assert_eq!(r.params, vec!["1", "2", "3"]);
    }

    #[test]
    fn in_list_strings_collapsed() {
        let r = normalize_sql("SELECT * FROM t WHERE name IN ('a', 'b', 'c')");
        assert_eq!(r.template, "SELECT * FROM t WHERE name IN (?)");
        assert_eq!(r.params, vec!["a", "b", "c"]);
    }

    #[test]
    fn escaped_quotes() {
        let r = normalize_sql("SELECT * FROM t WHERE name = 'O''Brien'");
        assert_eq!(r.template, "SELECT * FROM t WHERE name = ?");
        assert_eq!(r.params, vec!["O'Brien"]);
    }

    #[test]
    fn table_names_with_digits_preserved() {
        let r = normalize_sql("SELECT * FROM player2 WHERE id = 1");
        assert_eq!(r.template, "SELECT * FROM player2 WHERE id = ?");
        assert_eq!(r.params, vec!["1"]);
    }

    #[test]
    fn join_query() {
        let r = normalize_sql(
            "SELECT p.name FROM player p JOIN game g ON p.game_id = g.id WHERE g.id = 42",
        );
        assert_eq!(
            r.template,
            "SELECT p.name FROM player p JOIN game g ON p.game_id = g.id WHERE g.id = ?"
        );
        assert_eq!(r.params, vec!["42"]);
    }

    #[test]
    fn multiple_params() {
        let r = normalize_sql("UPDATE t SET a = 1, b = 'foo' WHERE id = 99");
        assert_eq!(r.template, "UPDATE t SET a = ?, b = ? WHERE id = ?");
        assert_eq!(r.params, vec!["1", "foo", "99"]);
    }

    #[test]
    fn no_literals() {
        let r = normalize_sql("SELECT count(*) FROM users");
        assert_eq!(r.template, "SELECT count(*) FROM users");
        assert!(r.params.is_empty());
    }

    #[test]
    fn multi_dot_number_rejected() {
        // 1.2.3 should be treated as 1.2 then .3, not a single number
        let r = normalize_sql("SELECT * FROM t WHERE x = 1.2.3");
        assert_eq!(r.params[0], "1.2");
    }

    #[test]
    fn unterminated_string_flushed() {
        let r = normalize_sql("SELECT * FROM t WHERE name = 'unterminated");
        assert_eq!(r.template, "SELECT * FROM t WHERE name = ?");
        assert_eq!(r.params, vec!["unterminated"]);
    }

    #[test]
    fn empty_query() {
        let r = normalize_sql("");
        assert_eq!(r.template, "");
        assert!(r.params.is_empty());
    }

    #[test]
    fn number_at_start_of_query() {
        let r = normalize_sql("42");
        assert_eq!(r.template, "?");
        assert_eq!(r.params, vec!["42"]);
    }

    #[test]
    fn multi_dot_full_template() {
        // 1.2.3 -> "1.2" is a float, then ".3" remains: dot in template, 3 is a new number
        let r = normalize_sql("SELECT * FROM t WHERE x = 1.2.3");
        assert_eq!(r.template, "SELECT * FROM t WHERE x = ?.?");
        assert_eq!(r.params, vec!["1.2", "3"]);
    }

    #[test]
    fn empty_string_literal() {
        let r = normalize_sql("SELECT * FROM t WHERE name = ''");
        assert_eq!(r.template, "SELECT * FROM t WHERE name = ?");
        assert_eq!(r.params, vec![""]);
    }

    #[test]
    fn digit_in_string_literal() {
        let r = normalize_sql("SELECT * FROM t WHERE code = '42'");
        assert_eq!(r.template, "SELECT * FROM t WHERE code = ?");
        assert_eq!(r.params, vec!["42"]);
    }

    #[test]
    fn underscore_before_digit_preserved() {
        // col_1 is an identifier, the 1 is part of it
        let r = normalize_sql("SELECT col_1 FROM t");
        assert_eq!(r.template, "SELECT col_1 FROM t");
        assert!(r.params.is_empty());
    }

    #[test]
    fn number_only_query_at_eof() {
        // Number flush at EOF
        let r = normalize_sql("SELECT * FROM t LIMIT 100");
        assert_eq!(r.template, "SELECT * FROM t LIMIT ?");
        assert_eq!(r.params, vec!["100"]);
    }

    #[test]
    fn cow_borrowed_path_no_in_list() {
        // No IN clause -> Cow::Borrowed path
        let r = normalize_sql("SELECT 1");
        assert_eq!(r.template, "SELECT ?");
        assert_eq!(r.params, vec!["1"]);
    }

    #[test]
    fn negative_number_not_collapsed() {
        // Minus sign is not part of the number token
        let r = normalize_sql("SELECT * FROM t WHERE x = -5");
        assert_eq!(r.template, "SELECT * FROM t WHERE x = -?");
        assert_eq!(r.params, vec!["5"]);
    }
}
