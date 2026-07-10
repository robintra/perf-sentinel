//! Fuzz the homemade SQL tokenizer. Beyond "no panic" (byte indexing
//! and char-boundary slices are the risk surface, see
//! `checked_query_slice`), idempotence doubles as a correctness oracle:
//! a produced template must be a fixed point, mirroring the
//! `normalize::metamorphic` property on arbitrary input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sentinel_core::normalize::sql::normalize_sql;

fuzz_target!(|data: &str| {
    let first = normalize_sql(data);
    let second = normalize_sql(&first.template);
    assert_eq!(second.template, first.template);
});
