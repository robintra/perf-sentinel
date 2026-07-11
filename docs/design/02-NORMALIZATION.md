# Normalization: SQL and HTTP

Normalization is the second pipeline stage. It transforms raw `SpanEvent`s into `NormalizedEvent`s by extracting a template (parameterized query or URL pattern) and the concrete parameter values.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/ingestion_dark.svg">
  <img alt="Auto-format detection" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/ingestion.svg">
</picture>

## Why not use `sqlparser`?

The [sqlparser](https://docs.rs/sqlparser/) crate is a full SQL parser that builds an AST. We deliberately chose a homemade tokenizer instead:

- **Binary size:** sqlparser adds ~300KB to the release binary. perf-sentinel targets < 15 MB total.
- **Dependency weight:** sqlparser pulls in additional crates and increases compile time.
- **Dialect-agnostic:** sqlparser requires specifying a SQL dialect (PostgreSQL, MySQL, etc.). Our tokenizer works across all dialects because it only replaces literals, it never needs to understand query structure.
- **Performance:** a full parser builds an AST we would immediately discard. Our single-pass tokenizer processes input in O(n) with no intermediate data structure.
- **Simplicity:** 120 lines of code vs a 50,000+ line dependency.

The trade-off is documented in [LIMITATIONS.md](../LIMITATIONS.md): the tokenizer handles ASCII SQL only and does not perform semantic analysis. It supports CTEs, double-quoted identifiers, PostgreSQL dollar-quoted strings and `CALL` statements.

This "never understands query structure" property is sufficient for the whole pipeline, not just this stage. Every detector (`n_plus_one`, `redundant`, `fanout`, `sanitizer_aware`, …) reasons over *trace shape* (occurrence counts, timing variance, span ordering, ORM instrumentation scope) and the query *fingerprint*, never the SQL's internal grammar. Structural SQL analysis would only pay off if perf-sentinel pivoted into single-query static analysis (a different product category), and even then an `EXPLAIN` plan beats re-parsing the logged text.

A quieter benefit: the tokenizer is *total*. It always emits a best-effort template, even on truncated or unknown-dialect SQL (see the unterminated-literal handling), whereas a real parser rejects input it cannot parse. Since trace SQL is whatever a driver happened to log, a strict parser would need this tokenizer as a fallback anyway.

## SQL tokenizer: single-pass state machine

`normalize_sql()` processes the query byte-by-byte through five states:

| State             | Trigger (enter)          | Action                                             | Trigger (exit)                              |
|-------------------|--------------------------|----------------------------------------------------|---------------------------------------------|
| **Normal**        | Default / end of literal | Accumulate into template                           | `'`, `"`, `$$`/`$tag$`, or standalone digit |
| **InString**      | Opening `'`              | Accumulate into `current_value`                    | Closing `'` (not `''`)                      |
| **InNumber**      | Standalone digit         | Accumulate digits/dot                              | Non-digit or second dot                     |
| **InDoubleQuote** | Opening `"`              | Pass through into template (identifier preserved)  | Closing `"`                                 |
| **InDollarQuote** | `$$` or `$tag$`          | Accumulate body into `current_value`, emit one `?` | Matching `$$` / `$tag$`                     |

### Batch `push_str` optimization

Instead of pushing characters one at a time with `template.push(b as char)`, the tokenizer tracks a `normal_start` index:

```rust
// On entering InString or InNumber:
if i > normal_start {
    template.push_str(&query[normal_start..i]);
}
// On returning to Normal:
normal_start = i;
// At end of input (still in Normal):
template.push_str(&query[normal_start..len]);
```

This batches contiguous Normal-state runs into a single `push_str` call. For a typical query like `SELECT * FROM player WHERE game_id = 42`, the `SELECT * FROM player WHERE game_id = ` prefix is flushed in one call instead of 38 individual `push` calls.

The [Rust `String::push_str` implementation](https://doc.rust-lang.org/src/alloc/string.rs.html) copies bytes with `memcpy`, which is significantly faster than repeated `push` calls that each check capacity and potentially reallocate.

### IN-list regex skip

Most SQL queries do not contain `IN (...)` clauses. The tokenizer tracks whether the `IN` keyword appears:

```rust
if !has_in_list
    && (b == b'I' || b == b'i')
    && i + 1 < len
    && (bytes[i + 1] == b'N' || bytes[i + 1] == b'n')
    && (i == 0 || bytes[i - 1].is_ascii_whitespace())
    && (i + 2 >= len || !bytes[i + 2].is_ascii_alphanumeric())
{
    has_in_list = true;
}
```

If `has_in_list` is false after the main loop, the regex post-pass (`IN_LIST_RE.replace_all`) is skipped entirely. This avoids ~2us of regex overhead on the ~80% of queries that have no IN clause.

### `Cow::Borrowed` optimization

When the regex does run but makes no replacements (e.g., `IN (?)` is already collapsed), `Regex::replace_all` returns `Cow::Borrowed`. The code checks for this:

```rust
let template = if has_in_list {
    match IN_LIST_RE.replace_all(&template, "IN (?)") {
        Cow::Borrowed(_) => template,    // no allocation
        Cow::Owned(s) => s,              // one allocation
    }
} else {
    template                              // no regex at all
};
```

This three-tier approach ensures zero unnecessary allocations.

### `LazyLock` for regex

The `IN_LIST_RE` regex is compiled once via [`std::sync::LazyLock`](https://doc.rust-lang.org/std/sync/struct.LazyLock.html) (stable since Rust 1.80):

```rust
static IN_LIST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)IN\s*\(\s*\?(?:\s*,\s*\?)*\s*\)").unwrap()
});
```

`LazyLock` is preferred over the `lazy_static!` macro because it is in `std`, no external dependency needed.

### Other micro-optimizations

- **`String::with_capacity(query.len())`**: pre-allocates the template to avoid reallocation in the common case where the template is slightly shorter than the input.
- **`std::mem::take(&mut current_value)`**: moves the accumulated literal value into `params` without cloning, replacing `current_value` with an empty `String` in place. This is a zero-cost ownership transfer.
- **`is_identifier_byte_before()`**: checks whether the byte before a digit is alphanumeric or underscore, preventing digits within identifiers (`player2`, `col_1`) from being misinterpreted as numeric literals.

## HTTP normalizer

### Hand-coded UUID check

The HTTP normalizer replaces UUID path segments with `{uuid}`. Instead of using a regex, the check is hand-coded:

```rust
fn is_uuid(s: &str) -> bool {
    if s.len() != 36 { return false; }
    let b = s.as_bytes();
    b[8] == b'-' && b[13] == b'-' && b[18] == b'-' && b[23] == b'-'
        && b.iter().enumerate().all(|(i, &c)| {
            matches!(i, 8 | 13 | 18 | 23) || c.is_ascii_hexdigit()
        })
}
```

**Why hand-coded?** This function is called on every path segment of every HTTP URL in the pipeline. A compiled regex (`Regex::is_match`) takes ~150ns per call due to the regex engine overhead. The hand-coded check takes ~3ns, a length check (fast rejection for >99% of segments), four byte comparisons for dash positions and a single pass for hex digits.

At 100,000 events/sec with an average of 4 path segments per URL, this saves ~60ms/sec of regex overhead.

### `split_origin` and host-aware grouping

```rust
fn split_origin(target: &str) -> (Option<&str>, &str) {
    match target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
    {
        Some(rest) => match rest.find('/') {
            Some(idx) => (Some(&rest[..idx]), &rest[idx..]),
            None => (Some(rest), "/"),
        },
        None => (None, target),
    }
}
```

This splits the authority from the path without pulling in the [url](https://docs.rs/url/) crate (~50KB binary overhead). It handles `http://`, `https://` and bare paths (`/api/foo`, which have no authority).

The callee host is then kept in the template for DNS-addressed calls (`GET user-svc/api/foo`) and dropped for IP-literal authorities. `host_group_prefix` classifies the authority: an IPv4 dotted-decimal or a bracketed IPv6 literal is a load-balanced replica address and is dropped, so pods behind one service keep deduping into one template, while a DNS hostname is lowercased, stripped of RFC 3986 userinfo and port, and prepended to the path.

Keeping the host stops two calls to the same path on different backends (`http://ms-a/x`, `http://ms-b/x`) from collapsing into one template and raising a false `redundant_http`. Because the finding signature hashes the template (`acknowledgments::compute_signature`), an outbound-HTTP finding's ack signature therefore depends on the callee host.

### Query parameter limit

Query parameters are stripped from the URL template and collected into `params`. The collection is capped at 100 parameters via `.take(100)` to prevent unbounded memory allocation from URLs with adversarially large query strings. Since query parameters are not part of the normalized template, excess parameters beyond 100 are simply not extracted.

### Pre-allocation

```rust
let mut result = String::with_capacity(path.len() + 8);
```

The `+ 8` accounts for the longest replacement (`{uuid}` = 6 chars, replacing a 36-char UUID). This avoids reallocation in the common case where replacements make the path shorter.

## Normalization dispatcher

The `normalize()` function dispatches to the SQL or HTTP normalizer based on `event_type`:

```rust
pub fn normalize(event: SpanEvent) -> NormalizedEvent {
    match event.event_type {
        EventType::Sql => { /* sql::normalize_sql(...) */ }
        EventType::HttpOut => { /* http::normalize_http(...) */ }
    }
}
```

`normalize_all()` is a simple `events.into_iter().map(normalize).collect()`. The `into_iter()` consumes the input vector and each `SpanEvent` is moved (not cloned) into the normalizer.

## Defense-in-depth

**Query truncation.** `normalize_sql` truncates input at `MAX_QUERY_LEN` (64 KB) before processing to prevent the state-machine tokenizer from running on adversarially large inputs. Truncation uses `floor_char_boundary` to avoid splitting multi-byte UTF-8 characters. This is a second layer after the `sanitize_span_event` field caps applied at the ingestion boundary.
