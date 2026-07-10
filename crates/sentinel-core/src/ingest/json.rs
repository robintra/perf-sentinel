//! JSON ingestion with auto-format detection.
//!
//! Detects the input format (native, OTLP/JSON, Jaeger, Zipkin) and dispatches
//! to the appropriate parser. Format detection peeks at the JSON structure,
//! in this order:
//! - Has `"data"` key with trace objects containing `"spans"` -> Jaeger
//! - Has `"resourceSpans"` (or `"resource_spans"`) key -> OTLP/JSON
//! - Is array where items have `"traceId"` + `"localEndpoint"` -> Zipkin
//! - Otherwise -> native perf-sentinel format

use crate::event::SpanEvent;
use crate::ingest::IngestSource;

/// Defense-in-depth nesting cap for the native ingest path. The native
/// span-event format is flat (top-level array of objects, each with at
/// most a `source` and a few scalar fields), so depth 32 is well above
/// what valid input ever needs. We pre-scan the bytes BEFORE handing
/// them to `serde_json::from_slice` because `serde_json` has a built-in
/// recursion limit of 128 (its compile-time default, no public setter
/// to tighten it). The pre-scan is O(N) in payload bytes, negligible
/// next to the JSON parse cost.
pub const MAX_JSON_DEPTH: usize = 32;

/// Reject the payload when its bracket nesting exceeds [`MAX_JSON_DEPTH`].
///
/// This is a byte-level pre-scan, not a full JSON parse: it counts `[`
/// and `{` opens against `]` and `}` closes, ignoring any character that
/// appears inside a `"..."` string (with `\"` escape support). False
/// positives (rejecting valid input) are impossible because we never
/// inflate the depth on string contents. False negatives (accepting an
/// over-deep payload) are impossible because every structural open
/// increments depth.
///
/// `pub` so CLI subcommands that accept user-supplied JSON through paths
/// that bypass `JsonIngest` (e.g. `report --input` in Report mode,
/// `report --before`) can enforce the same defense-in-depth cap.
#[must_use]
pub fn exceeds_max_depth(raw: &[u8]) -> bool {
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escape = false;
    for &b in raw {
        if in_string {
            advance_string_state(b, &mut in_string, &mut escape);
            continue;
        }
        if bump_depth(b, &mut depth, &mut in_string) {
            return true;
        }
    }
    false
}

/// Advance the string-scanning state machine by one byte while inside a
/// `"..."` literal. Handles `\"` escapes and the closing `"`. Pulled out
/// of [`exceeds_max_depth`] to keep its cognitive complexity under the
/// S3776 threshold.
#[inline]
fn advance_string_state(b: u8, in_string: &mut bool, escape: &mut bool) {
    if *escape {
        *escape = false;
    } else if b == b'\\' {
        *escape = true;
    } else if b == b'"' {
        *in_string = false;
    }
}

/// Apply a structural byte to the bracket-depth counter. Returns `true`
/// iff the depth rose above [`MAX_JSON_DEPTH`] (the caller short-circuits
/// and rejects the payload).
#[inline]
fn bump_depth(b: u8, depth: &mut usize, in_string: &mut bool) -> bool {
    match b {
        b'"' => *in_string = true,
        b'[' | b'{' => {
            *depth += 1;
            if *depth > MAX_JSON_DEPTH {
                return true;
            }
        }
        b']' | b'}' => *depth = depth.saturating_sub(1),
        _ => {}
    }
    false
}

/// The detected input format.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions (0.9.5 added
/// `Otlp`; external matchers must carry a wildcard arm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputFormat {
    /// Native perf-sentinel JSON array of `SpanEvent`.
    Native,
    /// OTLP/JSON (`ExportTraceServiceRequest`), single object or NDJSON.
    Otlp,
    /// Jaeger JSON export format.
    Jaeger,
    /// Zipkin JSON v2 format.
    Zipkin,
}

/// Ingests span events from JSON input with auto-format detection.
pub struct JsonIngest {
    max_size: usize,
}

impl JsonIngest {
    #[must_use]
    pub const fn new(max_size: usize) -> Self {
        Self { max_size }
    }
}

impl IngestSource for JsonIngest {
    type Error = JsonIngestError;

    fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error> {
        if raw.len() > self.max_size {
            return Err(JsonIngestError::PayloadTooLarge {
                size: raw.len(),
                max: self.max_size,
            });
        }

        // Apply the project-wide nesting cap before dispatching to a
        // format-specific parser. Pre-0.5.15 only the Native arm enforced
        // it, leaving Jaeger and Zipkin paths on serde_json's looser
        // 128-frame default.
        if exceeds_max_depth(raw) {
            return Err(JsonIngestError::PayloadTooDeep {
                max_depth: MAX_JSON_DEPTH,
            });
        }

        match detect_format(raw) {
            InputFormat::Otlp => {
                // Deserialize each document (a single pretty-printed request or
                // the Collector file exporter's NDJSON, one request per line)
                // straight into the typed ExportTraceServiceRequest: that keeps
                // strict duplicate-key rejection, positioned parse errors, and
                // streaming memory. Only a document that trips protojson's
                // omitted-`values` case (empty arrayValue/kvlistValue) is
                // re-parsed through a normalized Value, so the common case pays
                // nothing. convert_otlp_request sanitizes each event, same code
                // path as the daemon listeners.
                type OtlpRequest =
                    opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
                let mut events = Vec::new();
                let mut parsed_any = false;
                let mut offset = 0;
                while offset < raw.len() {
                    let mut stream = serde_json::Deserializer::from_slice(&raw[offset..])
                        .into_iter::<OtlpRequest>();
                    match stream.next() {
                        None => break,
                        Some(Ok(request)) => {
                            parsed_any = true;
                            events.extend(crate::ingest::otlp::convert_otlp_request(&request));
                            offset += stream.byte_offset();
                        }
                        // Canonical protojson omits empty repeated fields, so an
                        // empty-list attribute serializes as `{"arrayValue":{}}`
                        // and fails with `missing field values`. Backfill the
                        // empty list via normalize_otlp_json and retry just this
                        // one document.
                        Some(Err(e)) if is_missing_values(&e) => {
                            let mut retry = serde_json::Deserializer::from_slice(&raw[offset..])
                                .into_iter::<serde_json::Value>();
                            let Some(Ok(mut value)) = retry.next() else {
                                return Err(JsonIngestError::Parse(e));
                            };
                            normalize_otlp_json(&mut value);
                            let request: OtlpRequest =
                                serde_json::from_value(value).map_err(JsonIngestError::Parse)?;
                            parsed_any = true;
                            events.extend(crate::ingest::otlp::convert_otlp_request(&request));
                            offset += retry.byte_offset();
                        }
                        // A truncated trailing document is routine on a live or
                        // rotated Collector file-exporter dump (exporter still
                        // writing, file rotated mid-line). Tolerate it once at
                        // least one request parsed; mid-stream garbage (non-EOF
                        // errors) and a truncated-only payload still fail.
                        Some(Err(e)) if e.is_eof() && parsed_any => {
                            tracing::warn!(
                                "ignoring truncated trailing OTLP JSON document \
                                 (live or rotated file-exporter dump?)"
                            );
                            break;
                        }
                        Some(Err(e)) => return Err(JsonIngestError::Parse(e)),
                    }
                }
                Ok(events)
            }
            InputFormat::Jaeger => {
                let ingest = crate::ingest::jaeger::JaegerIngest::new(self.max_size);
                ingest
                    .ingest(raw)
                    .map_err(|e| JsonIngestError::Format(e.to_string()))
            }
            InputFormat::Zipkin => {
                let ingest = crate::ingest::zipkin::ZipkinIngest::new(self.max_size);
                ingest
                    .ingest(raw)
                    .map_err(|e| JsonIngestError::Format(e.to_string()))
            }
            InputFormat::Native => {
                let mut events: Vec<SpanEvent> =
                    serde_json::from_slice(raw).map_err(JsonIngestError::Parse)?;
                // Sanitize cloud.region at the JSON ingest boundary, symmetric
                // with the OTLP path. Invalid values (empty, > 64 bytes, non-ASCII
                // alphanumeric plus `-`/`_`) are replaced with None to prevent
                // log-forging through downstream tracing::debug! format strings.
                for event in &mut events {
                    if let Some(region) = event.cloud_region.as_deref()
                        && !crate::score::carbon::is_valid_region_id(region)
                    {
                        event.cloud_region = None;
                    }
                    crate::event::sanitize_span_event(event);
                }
                Ok(events)
            }
        }
    }
}

/// True for the serde error opentelemetry-proto raises on an empty-list
/// attribute serialized the protojson way, `{"arrayValue":{}}` or
/// `{"kvlistValue":{}}`: the derived Deserialize marks `values` required. The
/// only proto fields named `values` are ArrayValue/KeyValueList, so this match
/// is unambiguous and never fires on a well-formed request.
fn is_missing_values(e: &serde_json::Error) -> bool {
    e.classify() == serde_json::error::Category::Data
        && e.to_string().contains("missing field `values`")
}

/// Backfill the `values` field on empty `arrayValue`/`kvlistValue` attribute
/// values. Canonical protojson omits empty repeated fields, so `{"arrayValue":{}}`
/// is a valid empty list, but opentelemetry-proto's derived Deserialize marks
/// `values` required. Walks the parsed document and inserts an empty array where
/// missing. Recursion depth is bounded by the pre-dispatch `MAX_JSON_DEPTH` cap.
fn normalize_otlp_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for key in ["arrayValue", "kvlistValue"] {
                if let Some(serde_json::Value::Object(inner)) = map.get_mut(key)
                    && !inner.contains_key("values")
                {
                    inner.insert("values".to_string(), serde_json::Value::Array(Vec::new()));
                }
            }
            for v in map.values_mut() {
                normalize_otlp_json(v);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(normalize_otlp_json),
        _ => {}
    }
}

/// Detect the format of the JSON input using lightweight byte-level heuristics.
///
/// Peeks at the first few kilobytes to identify the format without parsing the full
/// payload into a `serde_json::Value`, avoiding a 2x parse cost.
#[must_use]
pub fn detect_format(raw: &[u8]) -> InputFormat {
    let peek = std::str::from_utf8(&raw[..raw.len().min(1024)]).unwrap_or("");

    // `{`-rooted formats are told apart STRUCTURALLY, on top-level keys
    // only, never on whole-buffer substrings: a Jaeger export can carry
    // the literal "resourceSpans" inside a span name or tag value (a
    // trace OF an OTel Collector), and an OTLP request can carry "data"
    // as an attribute key or value while always containing a nested
    // "spans" key inside scopeSpans, so substring sniffs misroute in
    // BOTH directions.
    if peek.trim_start().starts_with('{') {
        let mut saw_data_key = false;
        for key in TopLevelKeys::new(peek) {
            match key {
                // OTLP/JSON: { "resourceSpans": [...] } (camelCase per the
                // protobuf JSON mapping; the snake_case spelling routes here
                // too so it fails with a clear serde error instead of
                // Native's confusing "expected array").
                "resourceSpans" | "resource_spans" => return InputFormat::Otlp,
                // Jaeger: { "data": [{ ..., "spans": [...] }] }; the nested
                // "spans" key is confirmed on a deeper window below.
                "data" => saw_data_key = true,
                _ => {}
            }
        }
        if saw_data_key {
            let deeper = std::str::from_utf8(&raw[..raw.len().min(4096)]).unwrap_or("");
            if deeper.contains("\"spans\"") {
                return InputFormat::Jaeger;
            }
        }
    }

    // Zipkin: [{ "traceId": "...", "localEndpoint": {...} }]
    if peek.trim_start().starts_with('[')
        && peek.contains("\"traceId\"")
        && peek.contains("\"localEndpoint\"")
    {
        return InputFormat::Zipkin;
    }

    InputFormat::Native
}

/// Iterator over the keys of the ROOT JSON object inside a (possibly
/// truncated) prefix: depth-1 strings whose next non-whitespace byte is
/// `:`. Strings at any other depth (nested keys, attribute names) and
/// string VALUES never qualify, which is what makes the format sniff
/// immune to payload content. Escape-aware, stops silently when the
/// prefix ends mid-string.
struct TopLevelKeys<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> TopLevelKeys<'a> {
    fn new(peek: &'a str) -> Self {
        Self {
            bytes: peek.as_bytes(),
            pos: 0,
            depth: 0,
        }
    }

    /// Scan a quoted string whose opening quote is at `self.pos`, advancing
    /// `self.pos` past the closing quote. Returns the byte range of the
    /// content, or `None` if the string is truncated at the end of the peek
    /// window (escape-aware).
    fn scan_string(&mut self) -> Option<(usize, usize)> {
        let start = self.pos + 1;
        let mut i = start;
        let mut escape = false;
        while i < self.bytes.len() {
            let c = self.bytes[i];
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                break;
            }
            i += 1;
        }
        if i >= self.bytes.len() {
            return None; // truncated mid-string at the end of the peek window
        }
        self.pos = i + 1;
        Some((start, i))
    }

    /// True if the next non-whitespace byte at or after `self.pos` is `:`,
    /// i.e. the string just scanned is an object key rather than a value.
    fn colon_follows(&self) -> bool {
        let mut j = self.pos;
        while j < self.bytes.len() && self.bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        j < self.bytes.len() && self.bytes[j] == b':'
    }
}

impl<'a> Iterator for TopLevelKeys<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'{' | b'[' => {
                    self.depth += 1;
                    self.pos += 1;
                }
                b'}' | b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    self.pos += 1;
                }
                b'"' => {
                    let (start, end) = self.scan_string()?;
                    if self.depth == 1
                        && self.colon_follows()
                        && let Ok(key) = std::str::from_utf8(&self.bytes[start..end])
                    {
                        return Some(key);
                    }
                }
                _ => self.pos += 1,
            }
        }
        None
    }
}

/// Errors that can occur during JSON ingestion.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JsonIngestError {
    #[error("payload too large: {size} bytes exceeds maximum of {max} bytes")]
    PayloadTooLarge { size: usize, max: usize },
    #[error(
        "payload nesting exceeds maximum depth of {max_depth} (defense against deeply-nested attacker payloads)"
    )]
    PayloadTooDeep { max_depth: usize },
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("format detection error: {0}")]
    Format(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_matches;

    #[test]
    fn rejects_oversized_payload() {
        let ingest = JsonIngest::new(10);
        let result = ingest.ingest(&[0u8; 100]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_empty_array() {
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(b"[]").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn detect_native_format() {
        let json = r#"[{"type": "sql", "target": "SELECT 1"}]"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Native);
    }

    #[test]
    fn detect_jaeger_format() {
        let json = r#"{"data": [{"traceID": "abc", "spans": [], "processes": {}}]}"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Jaeger);
    }

    #[test]
    fn detect_zipkin_format() {
        let json = r#"[{"traceId": "abc", "id": "s1", "localEndpoint": {"serviceName": "svc"}}]"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Zipkin);
    }

    #[test]
    fn detect_empty_array_is_native() {
        assert_eq!(detect_format(b"[]"), InputFormat::Native);
    }

    #[test]
    fn detect_invalid_json_falls_to_native() {
        assert_eq!(detect_format(b"not json"), InputFormat::Native);
    }

    #[test]
    fn auto_ingest_jaeger() {
        let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "op",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        {"key": "db.statement", "value": "SELECT 1"},
                        {"key": "db.system", "value": "pg"}
                    ]
                }],
                "processes": {"p1": {"serviceName": "svc"}}
            }]
        }"#;
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
    }

    #[test]
    fn auto_ingest_zipkin() {
        let json = r#"[{
            "traceId": "t1",
            "id": "s1",
            "name": "query",
            "timestamp": 1720621921123000,
            "duration": 500,
            "localEndpoint": {"serviceName": "svc"},
            "tags": {"db.statement": "SELECT 1", "db.system": "pg"}
        }]"#;
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
    }

    // ----- OTLP/JSON -----

    /// Compact single-request OTLP/JSON body with one SQL CLIENT span.
    fn otlp_request_json(trace_id: &str, statement: &str) -> String {
        otlp_request_json_with_attrs(trace_id, statement, "")
    }

    /// Same span as `otlp_request_json`, with `extra_attrs` spliced into the
    /// attributes array after db.statement/db.system. Each element must carry a
    /// leading comma (e.g. `,{"key":..,"value":..}`); pass "" for none.
    fn otlp_request_json_with_attrs(trace_id: &str, statement: &str, extra_attrs: &str) -> String {
        format!(
            r#"{{"resourceSpans":[{{"resource":{{"attributes":[{{"key":"service.name","value":{{"stringValue":"svc"}}}}]}},"scopeSpans":[{{"spans":[{{"traceId":"{trace_id}","spanId":"eee19b7ec3c1b174","name":"db-query","kind":3,"startTimeUnixNano":"1720621921000000000","endTimeUnixNano":"1720621921000500000","attributes":[{{"key":"db.statement","value":{{"stringValue":"{statement}"}}}},{{"key":"db.system","value":{{"stringValue":"postgresql"}}}}{extra_attrs}]}}]}}]}}]}}"#
        )
    }

    #[test]
    fn detect_otlp_format() {
        let json = r#"{"resourceSpans": [{"scopeSpans": []}]}"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Otlp);
    }

    #[test]
    fn detect_jaeger_wins_over_stray_resource_spans_literal() {
        // Regression: a Jaeger export can mention "resourceSpans" inside a
        // span name or tag (e.g. a trace OF an OTel Collector). The Jaeger
        // rule must keep winning, as it did before the OTLP sniff existed.
        let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "export resourceSpans",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [{"key": "note", "value": "handles \"resourceSpans\" batches"}]
                }],
                "processes": {"p1": {"serviceName": "collector"}}
            }]
        }"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Jaeger);
    }

    #[test]
    fn detect_otlp_wins_over_stray_data_literal() {
        // Regression (reverse direction): an OTLP dump whose first spans
        // carry a "data" attribute key or value must NOT be misrouted to
        // Jaeger. OTLP always contains a nested "spans" key (scopeSpans),
        // so a substring rule on "data" would have flipped it.
        let json = r#"{"resourceSpans":[{"resource":{"attributes":[{"key":"data","value":{"stringValue":"data"}}]},"scopeSpans":[{"spans":[]}]}]}"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Otlp);
    }

    #[test]
    fn top_level_keys_ignores_nested_keys_and_string_values() {
        let json = r#"{"a": {"nested": 1}, "b": ["data", {"c": 2}], "d": "resourceSpans"}"#;
        let keys: Vec<&str> = TopLevelKeys::new(json).collect();
        assert_eq!(keys, ["a", "b", "d"]);
    }

    #[test]
    fn detect_otlp_snake_case_routes_to_otlp() {
        // snake_case is not a valid OTLP/JSON spelling (with-serde is
        // camelCase-only), but routing it to the OTLP arm yields a clear
        // serde error instead of Native's "expected array".
        let json = r#"{"resource_spans": []}"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Otlp);
        let ingest = JsonIngest::new(1_048_576);
        assert_matches!(
            ingest.ingest(json.as_bytes()),
            Err(JsonIngestError::Parse(_))
        );
    }

    #[test]
    fn auto_ingest_otlp() {
        let json = otlp_request_json("5b8efff798038103d269b633813fc60c", "SELECT 1");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
        assert_eq!(events[0].service.as_ref(), "svc");
        assert_eq!(events[0].trace_id, "5b8efff798038103d269b633813fc60c");
    }

    #[test]
    fn otlp_empty_array_value_ingests() {
        // Canonical protojson omits empty repeated fields, so `{"arrayValue":{}}`
        // is a valid empty-list attribute. It must not poison the batch (#81).
        let json = otlp_request_json_with_attrs(
            "5b8efff798038103d269b633813fc60c",
            "SELECT 1",
            r#",{"key":"tags","value":{"arrayValue":{}}}"#,
        );
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
    }

    #[test]
    fn otlp_empty_kvlist_value_ingests() {
        // `{"kvlistValue":{}}` omits `values` identically and must also parse.
        let json = otlp_request_json_with_attrs(
            "5b8efff798038103d269b633813fc60c",
            "SELECT 1",
            r#",{"key":"meta","value":{"kvlistValue":{}}}"#,
        );
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
    }

    #[test]
    fn normalize_fills_missing_array_values() {
        // The issue's "attribute reads as an empty list" assertion. It is not
        // observable on SpanEvent (list attributes are not lifted), so assert it
        // at the value level: after normalization, `{"arrayValue":{}}` becomes a
        // valid AnyValue holding an empty ArrayValue.
        use opentelemetry_proto::tonic::common::v1::{AnyValue, any_value};
        let mut value: serde_json::Value = serde_json::from_str(r#"{"arrayValue":{}}"#).unwrap();
        normalize_otlp_json(&mut value);
        let any: AnyValue = serde_json::from_value(value).unwrap();
        let Some(any_value::Value::ArrayValue(av)) = any.value else {
            panic!("expected ArrayValue variant");
        };
        assert!(av.values.is_empty());
    }

    #[test]
    fn otlp_issue_81_repro_line_ingests() {
        // The exact repro from #81: a lone SERVER span whose only attribute is
        // an empty arrayValue. The span is filtered for lacking http.url, so no
        // events are produced, but ingest must not error on the empty arrayValue.
        let json = r#"{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"svc-a"}}]},"scopeSpans":[{"scope":{"name":"repro"},"spans":[{"traceId":"5b8efff798038103d269b633813fc60c","spanId":"eee19b7ec3c1b174","name":"GET /x","kind":2,"startTimeUnixNano":"1783678644000000000","endTimeUnixNano":"1783678644100000000","attributes":[{"key":"empty.list","value":{"arrayValue":{}}}]}]}]}]}"#;
        let ingest = JsonIngest::new(1_048_576);
        assert!(ingest.ingest(json.as_bytes()).is_ok());
    }

    #[test]
    fn otlp_empty_array_value_mid_ndjson_continues() {
        // The lenient retry must advance past the patched document and keep
        // parsing the rest of the stream, not stop at the first empty list.
        let line1 = otlp_request_json_with_attrs(
            "0af7651916cd43dd8448eb211c80319c",
            "SELECT 1",
            r#",{"key":"tags","value":{"arrayValue":{}}}"#,
        );
        let line2 = otlp_request_json("1bf7651916cd43dd8448eb211c80319d", "SELECT 2");
        let json = format!("{line1}\n{line2}\n");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].target, "SELECT 1");
        assert_eq!(events[1].target, "SELECT 2");
    }

    #[test]
    fn otlp_type_wrong_truncated_tail_still_fails() {
        // A trailing document that is both truncated and type-wrong is not the
        // benign truncated-tail case: the strict typed parser rejects the wrong
        // type before EOF, so the batch must fail rather than silently drop it.
        let full = otlp_request_json("0af7651916cd43dd8448eb211c80319c", "SELECT 1");
        let json = format!("{full}\n{{\"resourceSpans\":123");
        let ingest = JsonIngest::new(1_048_576);
        assert_matches!(
            ingest.ingest(json.as_bytes()),
            Err(JsonIngestError::Parse(_))
        );
    }

    #[test]
    fn auto_ingest_otlp_ndjson() {
        // Collector file-exporter shape: one request per line.
        let json = format!(
            "{}\n{}\n",
            otlp_request_json("0af7651916cd43dd8448eb211c80319c", "SELECT 1"),
            otlp_request_json("1bf7651916cd43dd8448eb211c80319d", "SELECT 2"),
        );
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].target, "SELECT 1");
        assert_eq!(events[1].target, "SELECT 2");
    }

    #[test]
    fn auto_ingest_otlp_ndjson_tolerates_truncated_tail() {
        // A live or rotated Collector file-exporter dump routinely ends on
        // a partially-written line: keep the parsed requests, warn, and do
        // not fail the whole batch.
        let full = otlp_request_json("0af7651916cd43dd8448eb211c80319c", "SELECT 1");
        let truncated = &full[..full.len() / 2];
        let json = format!("{full}\n{truncated}");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
    }

    #[test]
    fn auto_ingest_otlp_truncated_only_payload_still_fails() {
        // With zero complete requests there is nothing to salvage: the
        // parse error must surface, not an empty success.
        let full = otlp_request_json("0af7651916cd43dd8448eb211c80319c", "SELECT 1");
        let truncated = &full[..full.len() / 2];
        let ingest = JsonIngest::new(1_048_576);
        assert_matches!(
            ingest.ingest(truncated.as_bytes()),
            Err(JsonIngestError::Parse(_))
        );
    }

    #[test]
    fn auto_ingest_otlp_mid_stream_garbage_still_fails() {
        // Non-EOF errors (malformed document between valid ones) are not
        // the truncated-tail case and must abort the ingest.
        let full = otlp_request_json("0af7651916cd43dd8448eb211c80319c", "SELECT 1");
        let json = format!("{full}\n{{\"resourceSpans\": 42}}\n{full}");
        let ingest = JsonIngest::new(1_048_576);
        assert_matches!(
            ingest.ingest(json.as_bytes()),
            Err(JsonIngestError::Parse(_))
        );
    }

    #[test]
    fn auto_ingest_otlp_pretty_printed_single_object() {
        // A pretty-printed request spans many lines; the stream
        // deserializer must not treat it as broken NDJSON.
        let compact = otlp_request_json("5b8efff798038103d269b633813fc60c", "SELECT 1");
        let value: serde_json::Value = serde_json::from_str(&compact).unwrap();
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(pretty.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
    }

    #[test]
    fn deeply_nested_otlp_payload_is_rejected() {
        // Same guard as Jaeger/Zipkin: nesting via attribute arrayValue
        // must trip the pre-dispatch depth cap, not serde's 128 default.
        let depth = MAX_JSON_DEPTH + 4;
        let mut payload = String::from(
            r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{"attributes":[{"key":"a","value":{"arrayValue":{"values":["#,
        );
        for _ in 0..depth {
            payload.push('[');
        }
        for _ in 0..depth {
            payload.push(']');
        }
        payload.push_str("]}}}]}]}]}]}");
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert!(
            matches!(result, Err(JsonIngestError::PayloadTooDeep { .. })),
            "deeply-nested OTLP input must be rejected: {result:?}"
        );
    }

    // ----- Sanitize cloud_region on native JSON path -----

    fn native_event_with_cloud_region(cloud_region: &str) -> String {
        format!(
            r#"[{{
                "timestamp": "2025-07-10T14:32:01.123Z",
                "trace_id": "trace-1",
                "span_id": "span-1",
                "service": "order-svc",
                "cloud_region": {cr},
                "type": "sql",
                "operation": "SELECT",
                "target": "SELECT 1",
                "duration_us": 1000,
                "source": {{
                    "endpoint": "POST /api/orders/42/submit",
                    "method": "OrderService::create_order"
                }}
            }}]"#,
            cr = serde_json::to_string(cloud_region).unwrap()
        )
    }

    #[test]
    fn native_json_valid_cloud_region_preserved() {
        // Valid region names round-trip intact.
        let json = native_event_with_cloud_region("eu-west-3");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cloud_region.as_deref(), Some("eu-west-3"));
    }

    #[test]
    fn native_json_invalid_cloud_region_is_sanitized_to_none() {
        // A malicious client on the JSON socket trying to log-forge via
        // a newline in cloud_region must have the value replaced with None,
        // symmetric with the OTLP boundary sanitization.
        let json = native_event_with_cloud_region("eu-west-3\n2026 WARN fake alert");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            events[0].cloud_region.is_none(),
            "cloud_region with control char must be sanitized"
        );
    }

    #[test]
    fn native_json_oversized_cloud_region_sanitized() {
        // 65 chars exceeds the 64-byte cap.
        let long_region = "a".repeat(65);
        let json = native_event_with_cloud_region(&long_region);
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events[0].cloud_region.is_none());
    }

    #[test]
    fn native_json_cloud_region_with_space_sanitized() {
        let json = native_event_with_cloud_region("eu west 3");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events[0].cloud_region.is_none());
    }

    #[test]
    fn native_json_cloud_region_with_dot_sanitized() {
        // Dot is not in the allowlist (prevents path-traversal-style tricks).
        let json = native_event_with_cloud_region("eu.west.3");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events[0].cloud_region.is_none());
    }

    #[test]
    fn deeply_nested_native_payload_is_rejected_below_stack_overflow() {
        // Build `[[[[...]]]]` with depth above `MAX_JSON_DEPTH`. The
        // pre-scan guard must reject before serde_json walks the tree.
        let depth = MAX_JSON_DEPTH + 4;
        let mut payload = String::with_capacity(depth * 2);
        for _ in 0..depth {
            payload.push('[');
        }
        for _ in 0..depth {
            payload.push(']');
        }
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert_matches!(result, Err(JsonIngestError::PayloadTooDeep { .. }));
    }

    #[test]
    fn deeply_nested_jaeger_payload_is_rejected() {
        // Pre-0.5.15 only the Native arm enforced MAX_JSON_DEPTH. A Jaeger
        // payload with 33+ frames of nesting would slip through to
        // JaegerIngest and rely on serde_json's looser 128-frame default.
        let depth = MAX_JSON_DEPTH + 4;
        let mut payload = String::from(r#"{"data":[{"spans":[{"tags":["#);
        for _ in 0..depth {
            payload.push('[');
        }
        for _ in 0..depth {
            payload.push(']');
        }
        payload.push_str("]}]}]}");
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert!(
            matches!(result, Err(JsonIngestError::PayloadTooDeep { .. })),
            "deeply-nested Jaeger input must be rejected: {result:?}"
        );
    }

    #[test]
    fn deeply_nested_zipkin_payload_is_rejected() {
        // Symmetric guard for the Zipkin v2 path.
        let depth = MAX_JSON_DEPTH + 4;
        let mut payload = String::from(
            r#"[{"traceId":"abc","localEndpoint":{"serviceName":"s"},"annotations":["#,
        );
        for _ in 0..depth {
            payload.push('[');
        }
        for _ in 0..depth {
            payload.push(']');
        }
        payload.push_str("]}]");
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert!(
            matches!(result, Err(JsonIngestError::PayloadTooDeep { .. })),
            "deeply-nested Zipkin input must be rejected: {result:?}"
        );
    }

    // Boundary tests for the 32-frame depth cap. The cap rejects when
    // peak nesting strictly exceeds 32 (`*depth > MAX_JSON_DEPTH`), so
    // peak = 32 is OK and peak = 33 fails. The depth-31 / depth-33 pair
    // skips the ambiguous boundary at peak = 32 to keep the assertions
    // robust if the cap is ever adjusted by one frame.

    #[test]
    fn native_ingest_accepts_input_at_depth_31() {
        // Native: array-of-arrays, peak depth = number of `[` brackets.
        let mut payload = String::with_capacity(64);
        for _ in 0..31 {
            payload.push('[');
        }
        for _ in 0..31 {
            payload.push(']');
        }
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert!(
            !matches!(result, Err(JsonIngestError::PayloadTooDeep { .. })),
            "depth 31 must not be rejected by the depth guard, got: {result:?}"
        );
    }

    #[test]
    fn native_ingest_rejects_input_at_depth_33() {
        let mut payload = String::with_capacity(68);
        for _ in 0..33 {
            payload.push('[');
        }
        for _ in 0..33 {
            payload.push(']');
        }
        let ingest = JsonIngest::new(1_048_576);
        assert_matches!(
            ingest.ingest(payload.as_bytes()),
            Err(JsonIngestError::PayloadTooDeep { .. })
        );
    }

    #[test]
    fn jaeger_ingest_accepts_input_at_depth_31() {
        // Jaeger wrapper `{"data":[{"spans":[{"tags":[ ... ]}]}]}` reaches
        // peak 6 before the inner brackets. Inner depth 25 yields peak 31.
        let inner = 25;
        let mut payload = String::from(r#"{"data":[{"spans":[{"tags":["#);
        for _ in 0..inner {
            payload.push('[');
        }
        for _ in 0..inner {
            payload.push(']');
        }
        payload.push_str("]}]}]}");
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert!(
            !matches!(result, Err(JsonIngestError::PayloadTooDeep { .. })),
            "Jaeger depth 31 must not be rejected by the depth guard, got: {result:?}"
        );
    }

    #[test]
    fn jaeger_ingest_rejects_input_at_depth_33() {
        // Inner depth 27 yields peak 33 (6 wrapper + 27 inner).
        let inner = 27;
        let mut payload = String::from(r#"{"data":[{"spans":[{"tags":["#);
        for _ in 0..inner {
            payload.push('[');
        }
        for _ in 0..inner {
            payload.push(']');
        }
        payload.push_str("]}]}]}");
        let ingest = JsonIngest::new(1_048_576);
        assert_matches!(
            ingest.ingest(payload.as_bytes()),
            Err(JsonIngestError::PayloadTooDeep { .. })
        );
    }

    #[test]
    fn zipkin_ingest_accepts_input_at_depth_31() {
        // Zipkin wrapper `[{"traceId":...,"localEndpoint":{...},"annotations":[...]}]`
        // reaches peak 3 before the inner brackets. Inner depth 28 yields peak 31.
        let inner = 28;
        let mut payload = String::from(
            r#"[{"traceId":"abc","localEndpoint":{"serviceName":"s"},"annotations":["#,
        );
        for _ in 0..inner {
            payload.push('[');
        }
        for _ in 0..inner {
            payload.push(']');
        }
        payload.push_str("]}]");
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert!(
            !matches!(result, Err(JsonIngestError::PayloadTooDeep { .. })),
            "Zipkin depth 31 must not be rejected by the depth guard, got: {result:?}"
        );
    }

    #[test]
    fn zipkin_ingest_rejects_input_at_depth_33() {
        // Inner depth 30 yields peak 33 (3 wrapper + 30 inner).
        let inner = 30;
        let mut payload = String::from(
            r#"[{"traceId":"abc","localEndpoint":{"serviceName":"s"},"annotations":["#,
        );
        for _ in 0..inner {
            payload.push('[');
        }
        for _ in 0..inner {
            payload.push(']');
        }
        payload.push_str("]}]");
        let ingest = JsonIngest::new(1_048_576);
        assert_matches!(
            ingest.ingest(payload.as_bytes()),
            Err(JsonIngestError::PayloadTooDeep { .. })
        );
    }

    #[test]
    fn depth_scan_ignores_brackets_inside_strings() {
        // A valid native event whose `target` field contains `[[[...`.
        // The pre-scan must not count those brackets, otherwise it
        // would falsely reject SQL queries like `WHERE id IN (...)` or
        // template strings.
        let json = native_event_with_cloud_region("eu-west-3").replace(
            "\"SELECT 1\"",
            "\"SELECT * FROM t WHERE col = '[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]'\"",
        );
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest
            .ingest(json.as_bytes())
            .expect("string-internal brackets must not trigger the depth guard");
        assert_eq!(events.len(), 1);
    }
}
