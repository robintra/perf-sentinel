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
    /// Inside a double-quoted identifier (e.g., `"MyTable"`). Preserved as-is.
    InDoubleQuote,
    /// Inside a `PostgreSQL` dollar-quoted string (e.g., `$$body$$` or `$tag$body$tag$`).
    InDollarQuote,
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
    /// The closing tag for dollar-quoted strings (e.g., `$$` or `$tag$`).
    dollar_tag: Vec<u8>,
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
        dollar_tag: Vec::new(),
    };

    while t.i < t.bytes.len() {
        match t.state {
            State::Normal => step_normal(&mut t),
            State::InString => step_in_string(&mut t),
            State::InNumber => step_in_number(&mut t),
            State::InDoubleQuote => step_in_double_quote(&mut t),
            State::InDollarQuote => step_in_dollar_quote(&mut t),
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
    } else if b == b'"' {
        // Double-quoted identifier: preserve as-is (don't replace literals inside)
        t.state = State::InDoubleQuote;
        t.i += 1;
        return;
    } else if b == b'$' && is_dollar_quote_start(t.i, t.bytes) {
        // PostgreSQL dollar-quoted string: $$ or $tag$
        let tag = extract_dollar_tag(t.i, t.bytes);
        flush_normal_run(t);
        let tag_len = tag.len();
        t.dollar_tag = tag;
        t.state = State::InDollarQuote;
        t.current_value.clear();
        t.i += tag_len;
        return;
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

fn step_in_double_quote(t: &mut Tokenizer<'_>) {
    let b = t.bytes[t.i];
    if b == b'"' {
        // End of double-quoted identifier; content stays in the template as-is
        t.i += 1;
    } else {
        t.i += 1;
    }
    // Both branches advance; if we hit the closing quote, return to Normal
    if b == b'"' {
        t.state = State::Normal;
        // normal_start is already correct (pointing before the opening ")
    }
}

fn step_in_dollar_quote(t: &mut Tokenizer<'_>) {
    // Look for the closing dollar tag at current position
    let remaining = &t.bytes[t.i..];
    if remaining.starts_with(&t.dollar_tag) {
        // Found closing tag -- replace entire dollar-quoted content with ?
        t.params.push(std::mem::take(&mut t.current_value));
        t.template.push('?');
        t.i += t.dollar_tag.len();
        t.state = State::Normal;
        t.normal_start = t.i;
    } else {
        t.current_value.push(t.bytes[t.i] as char);
        t.i += 1;
    }
}

/// Check if position `i` starts a dollar-quote tag (`$$` or `$identifier$`).
fn is_dollar_quote_start(i: usize, bytes: &[u8]) -> bool {
    if i >= bytes.len() || bytes[i] != b'$' {
        return false;
    }
    // $$ case
    if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
        return true;
    }
    // $tag$ case: $ followed by identifier chars then $
    let mut j = i + 1;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        j += 1;
    }
    j > i + 1 && j < bytes.len() && bytes[j] == b'$'
}

/// Extract the dollar-quote tag starting at position `i` (e.g., `$$` or `$tag$`).
fn extract_dollar_tag(i: usize, bytes: &[u8]) -> Vec<u8> {
    // $$ case
    if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
        return vec![b'$', b'$'];
    }
    // $tag$ case
    let mut j = i + 1;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        j += 1;
    }
    bytes[i..=j].to_vec()
}

fn flush_normal_run(t: &mut Tokenizer<'_>) {
    if t.i > t.normal_start {
        t.template.push_str(&t.query[t.normal_start..t.i]);
    }
}

fn flush_pending(t: &mut Tokenizer<'_>) {
    match t.state {
        State::InNumber | State::InString | State::InDollarQuote => {
            t.params.push(std::mem::take(&mut t.current_value));
            t.template.push('?');
        }
        State::Normal | State::InDoubleQuote => {
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
        let r = normalize_sql("SELECT * FROM order_item WHERE order_id = 42");
        assert_eq!(r.template, "SELECT * FROM order_item WHERE order_id = ?");
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
        let r = normalize_sql("SELECT * FROM order_item2 WHERE id = 1");
        assert_eq!(r.template, "SELECT * FROM order_item2 WHERE id = ?");
        assert_eq!(r.params, vec!["1"]);
    }

    #[test]
    fn join_query() {
        let r = normalize_sql(
            "SELECT p.name FROM order_item p JOIN orders g ON p.order_id = g.id WHERE g.id = 42",
        );
        assert_eq!(
            r.template,
            "SELECT p.name FROM order_item p JOIN orders g ON p.order_id = g.id WHERE g.id = ?"
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

    // -- CTE support --

    #[test]
    fn cte_basic() {
        let r = normalize_sql(
            "WITH active AS (SELECT id FROM users WHERE status = 'active') \
             SELECT * FROM orders WHERE user_id IN (SELECT id FROM active) AND total > 100",
        );
        assert_eq!(
            r.template,
            "WITH active AS (SELECT id FROM users WHERE status = ?) \
             SELECT * FROM orders WHERE user_id IN (SELECT id FROM active) AND total > ?"
        );
        assert_eq!(r.params, vec!["active", "100"]);
    }

    #[test]
    fn cte_nested() {
        let r = normalize_sql(
            "WITH a AS (SELECT 1), b AS (SELECT * FROM a WHERE x = 'test') \
             SELECT * FROM b WHERE id = 42",
        );
        assert_eq!(
            r.template,
            "WITH a AS (SELECT ?), b AS (SELECT * FROM a WHERE x = ?) \
             SELECT * FROM b WHERE id = ?"
        );
        assert_eq!(r.params, vec!["1", "test", "42"]);
    }

    // -- Double-quoted identifiers --

    #[test]
    fn double_quoted_identifier_preserved() {
        let r = normalize_sql(r#"SELECT * FROM "MyTable" WHERE "Column" = 42"#);
        assert_eq!(r.template, r#"SELECT * FROM "MyTable" WHERE "Column" = ?"#);
        assert_eq!(r.params, vec!["42"]);
    }

    #[test]
    fn double_quoted_with_digits_preserved() {
        // Digits inside double quotes should NOT be treated as literals
        let r = normalize_sql(r#"SELECT * FROM "table_2" WHERE "col_3" = 'value'"#);
        assert_eq!(r.template, r#"SELECT * FROM "table_2" WHERE "col_3" = ?"#);
        assert_eq!(r.params, vec!["value"]);
    }

    // -- Dollar-quoted strings (PostgreSQL) --

    #[test]
    fn dollar_quote_basic() {
        let r = normalize_sql("SELECT $$hello world$$ AS greeting");
        assert_eq!(r.template, "SELECT ? AS greeting");
        assert_eq!(r.params, vec!["hello world"]);
    }

    #[test]
    fn dollar_quote_tagged() {
        let r = normalize_sql("SELECT $tag$some body$tag$ AS body");
        assert_eq!(r.template, "SELECT ? AS body");
        assert_eq!(r.params, vec!["some body"]);
    }

    #[test]
    fn dollar_quote_in_function() {
        let r = normalize_sql(
            "CREATE FUNCTION foo() RETURNS void AS $$ BEGIN RAISE NOTICE 'hi'; END; $$ LANGUAGE plpgsql",
        );
        assert_eq!(
            r.template,
            "CREATE FUNCTION foo() RETURNS void AS ? LANGUAGE plpgsql"
        );
    }

    // -- CALL statements --

    #[test]
    fn call_with_params() {
        let r = normalize_sql("CALL process_order(42, 'rush', NOW())");
        assert_eq!(r.template, "CALL process_order(?, ?, NOW())");
        assert_eq!(r.params, vec!["42", "rush"]);
    }

    #[test]
    fn call_with_interval() {
        let r = normalize_sql("CALL schedule_task(1, INTERVAL '2 days')");
        assert_eq!(r.template, "CALL schedule_task(?, INTERVAL ?)");
        assert_eq!(r.params, vec!["1", "2 days"]);
    }
}
