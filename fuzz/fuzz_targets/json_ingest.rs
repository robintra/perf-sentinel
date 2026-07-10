//! Fuzz the JSON ingest path end to end: format auto-detection
//! (native / OTLP JSON+NDJSON / Jaeger / Zipkin), the depth and size
//! caps, and each format-specific parser. Any panic is a finding; the
//! lab's batch-otlp-file negatives cover imagined failures, this covers
//! the rest (truncation, giant attributes, deep nesting, invalid UTF-8).

#![no_main]

use libfuzzer_sys::fuzz_target;
use sentinel_core::ingest::IngestSource;
use sentinel_core::ingest::json::JsonIngest;

fuzz_target!(|data: &[u8]| {
    // 1 MiB cap keeps iterations fast while still exercising the
    // PayloadTooLarge branch on oversized generated inputs.
    let _ = JsonIngest::new(1 << 20).ingest(data);
});
