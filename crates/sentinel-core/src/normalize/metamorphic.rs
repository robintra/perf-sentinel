//! Metamorphic property tests for SQL normalization.
//!
//! Same approach as `detect::metamorphic`: assert *relations* between
//! normalization runs on transformed inputs instead of expected outputs,
//! so the tokenizer is exercised on thousands of generated statements
//! without a hand-labeled corpus:
//!
//! - **idempotence**: a produced template is a fixed point,
//!   `normalize(normalize(x).template)` changes nothing and extracts
//!   no params
//! - **literal invariance**: two statements differing only in literal
//!   values (including IN-list arity) share one template
//! - **containment**: params are extracted exactly, in encounter order,
//!   and no literal content survives in the template

use proptest::prelude::*;

use super::sql::normalize_sql;

/// Marker stamped on every generated string literal so a leak into the
/// template is detectable by substring search (generated skeletons
/// never contain it, and no literal is empty).
const MARKER: &str = "zzq";

/// One SQL literal: its source text and the param `normalize_sql` must
/// extract for it.
#[derive(Debug, Clone)]
struct Lit {
    sql: String,
    param: String,
}

fn int_lit() -> impl Strategy<Value = Lit> {
    (0u64..=99_999_999).prop_map(|n| Lit {
        sql: n.to_string(),
        param: n.to_string(),
    })
}

fn float_lit() -> impl Strategy<Value = Lit> {
    (0u32..=9999, 0u32..=99).prop_map(|(int_part, frac_part)| {
        let s = format!("{int_part}.{frac_part}");
        Lit {
            sql: s.clone(),
            param: s,
        }
    })
}

/// Quoted string literal. Content mixes letters, digits, spaces,
/// backslashes, and single quotes; quotes are escaped as `''` in the
/// SQL text and must come back unescaped in the param.
fn str_lit() -> impl Strategy<Value = Lit> {
    proptest::collection::vec(
        prop_oneof![
            Just('a'),
            Just('B'),
            Just('7'),
            Just(' '),
            Just('\\'),
            Just('\''),
            Just('-'),
        ],
        0..8,
    )
    .prop_map(|chars| {
        let raw = format!("{MARKER}{}", chars.into_iter().collect::<String>());
        Lit {
            sql: format!("'{}'", raw.replace('\'', "''")),
            param: raw,
        }
    })
}

fn any_lit() -> impl Strategy<Value = Lit> {
    prop_oneof![int_lit(), float_lit(), str_lit()]
}

/// Cosmetic dimensions of a statement (keyword casing, whitespace,
/// table name). Held constant when comparing two literal sets: the
/// tokenizer preserves casing and whitespace verbatim, so only the
/// literals may vary between the compared statements.
#[derive(Debug, Clone)]
struct Shape {
    select: &'static str,
    r#in: &'static str,
    ws: &'static str,
    table: &'static str,
}

fn shape() -> impl Strategy<Value = Shape> {
    (
        prop_oneof![Just("SELECT"), Just("select"), Just("Select")],
        prop_oneof![Just("IN"), Just("in")],
        prop_oneof![Just(" "), Just("  "), Just("\t")],
        prop_oneof![Just("users"), Just("order_items")],
    )
        .prop_map(|(select, r#in, ws, table)| Shape {
            select,
            r#in,
            ws,
            table,
        })
}

/// Render one statement:
/// `SELECT * FROM t WHERE id = <lit> AND name IN (<list>) AND note = <str>`.
fn render(shape: &Shape, eq_lit: &Lit, in_list: &[Lit], note: &Lit) -> String {
    let list = in_list
        .iter()
        .map(|l| l.sql.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let w = shape.ws;
    format!(
        "{sel}{w}*{w}FROM{w}{table}{w}WHERE{w}id{w}={w}{eq}{w}AND{w}name{w}{inkw}{w}({list}){w}AND{w}note = {note}",
        sel = shape.select,
        table = shape.table,
        eq = eq_lit.sql,
        inkw = shape.r#in,
        note = note.sql,
    )
}

type Statement = (Shape, Lit, Vec<Lit>, Lit);

fn statement() -> impl Strategy<Value = Statement> {
    (
        shape(),
        any_lit(),
        prop::collection::vec(any_lit(), 1..5),
        str_lit(),
    )
}

proptest! {
    /// Idempotence: normalizing a template is a no-op. Every literal was
    /// already collapsed to `?` (and `IN` lists to `IN (?)`), so a second
    /// pass must return the same template and extract nothing.
    #[test]
    fn normalization_is_idempotent((shape, eq, list, note) in statement()) {
        let first = normalize_sql(&render(&shape, &eq, &list, &note));
        let second = normalize_sql(&first.template);
        prop_assert_eq!(&second.template, &first.template);
        prop_assert!(
            second.params.is_empty(),
            "template re-extracted params: {:?}",
            second.params
        );
    }

    /// Literal invariance: two statements sharing a skeleton but with
    /// different literal values, types, and IN-list arities produce the
    /// same template. This is what makes N+1 grouping by template work.
    #[test]
    fn literals_never_change_the_template(
        (shape, eq_a, list_a, note_a) in statement(),
        (eq_b, list_b, note_b) in (any_lit(), prop::collection::vec(any_lit(), 1..5), str_lit()),
    ) {
        let a = normalize_sql(&render(&shape, &eq_a, &list_a, &note_a));
        let b = normalize_sql(&render(&shape, &eq_b, &list_b, &note_b));
        prop_assert_eq!(a.template, b.template);
    }

    /// Containment: params come back exactly and in encounter order, and
    /// the template retains no literal content: no string marker, no
    /// quote, no digit (the skeleton is digit-free, so any digit in the
    /// template is a leaked literal).
    #[test]
    fn params_never_leak_into_the_template((shape, eq, list, note) in statement()) {
        let normalized = normalize_sql(&render(&shape, &eq, &list, &note));

        let expected: Vec<&str> = std::iter::once(&eq)
            .chain(&list)
            .chain(std::iter::once(&note))
            .map(|l| l.param.as_str())
            .collect();
        prop_assert_eq!(&normalized.params, &expected);

        prop_assert!(!normalized.template.contains(MARKER), "{}", normalized.template);
        prop_assert!(!normalized.template.contains('\''), "{}", normalized.template);
        prop_assert!(
            !normalized.template.bytes().any(|b| b.is_ascii_digit()),
            "digit leaked into template: {}",
            normalized.template
        );
    }
}
