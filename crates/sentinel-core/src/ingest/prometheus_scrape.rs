//! Shared validation and transport for the Prometheus instant-query scrapes.
//!
//! `pg_stat` and `mysql_stat` both rank statements from an exporter series,
//! and both let an operator name that series. The endpoint and series checks
//! guard what the caller supplies, so they live here rather than in each
//! ingester: two copies of an input guard drift, and the copy that stops
//! being updated is the one that lets a bad value through.
//!
//! Errors surface as plain strings. Each caller owns a `#[non_exhaustive]`
//! error enum of its own and maps these into the variant it already exposes,
//! which keeps this module free of their types.

#![cfg(any(feature = "daemon", feature = "tempo"))]

use crate::ingest::auth_header::AuthHeader;

/// Reject a series name that is not a bare `PromQL` metric name.
///
/// The name lands unencoded in the query string, so anything outside
/// `[a-zA-Z_:][a-zA-Z0-9_:]*` either breaks the URL (a space, a brace) or
/// smuggles a second parameter into it (`&`, `#`). Rejecting beats
/// encoding: a label selector or a whole expression here is a mistake, and
/// naming it is more useful than a downstream "invalid URL".
pub(crate) fn validate_series_name(series: &str) -> Result<(), String> {
    let head_ok = matches!(
        series.as_bytes().first(),
        Some(b'a'..=b'z' | b'A'..=b'Z' | b'_' | b':')
    );
    if head_ok
        && series
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b':')
    {
        return Ok(());
    }
    Err(format!(
        "series name must be a bare PromQL metric name \
         matching [a-zA-Z_:][a-zA-Z0-9_:]*, got `{series}`"
    ))
}

/// Validate a user-supplied Prometheus endpoint string.
///
/// Rejects URLs that:
/// - carry ASCII control characters
/// - fail to parse as a hyper `Uri`
/// - have a scheme other than `http` or `https`
/// - carry userinfo (credentials in the authority, e.g. `user:pass@host`)
///   since credentials must flow via env vars or a mounted file
pub(crate) fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    if endpoint.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err("endpoint must not contain ASCII control characters".to_string());
    }
    let uri: crate::http_client::Uri = endpoint
        .parse()
        .map_err(|e| format!("invalid endpoint URL: {e}"))?;

    match uri.scheme_str() {
        Some("http" | "https") => {}
        Some(other) => {
            return Err(format!(
                "unsupported scheme `{other}`, only http and https are accepted"
            ));
        }
        None => {
            return Err("endpoint URL must include a scheme (http:// or https://)".to_string());
        }
    }

    // Check for userinfo. `hyper::Uri::authority()` returns the full
    // `[user[:pass]@]host[:port]` string; if it contains `@`, credentials
    // are embedded.
    if let Some(authority) = uri.authority()
        && authority.as_str().contains('@')
    {
        return Err("credentials in the URL are not accepted; use env vars instead".to_string());
    }

    Ok(())
}

/// Reject a label name that is not a bare `PromQL` label.
///
/// Same reasoning as [`validate_series_name`], one grammar tighter: a label
/// carries no `:`, and the operator-supplied query label now lands inside a
/// `sum by (...)` clause rather than only being read back off the response.
pub(crate) fn validate_label_name(label: &str) -> Result<(), String> {
    let head_ok = matches!(
        label.as_bytes().first(),
        Some(b'a'..=b'z' | b'A'..=b'Z' | b'_')
    );
    if head_ok
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Ok(());
    }
    Err(format!(
        "label name must be a bare PromQL label matching \
         [a-zA-Z_][a-zA-Z0-9_]*, got `{label}`"
    ))
}

/// Encode a `by (...)` clause over validated label names, deduplicated so an
/// operator naming an identity label as the query label cannot repeat it.
fn by_clause(labels: &[&str]) -> String {
    let mut seen: Vec<&str> = Vec::with_capacity(labels.len());
    for l in labels {
        if !seen.contains(l) {
            seen.push(l);
        }
    }
    format!("by%20({})", seen.join("%2C%20"))
}

/// Build the `topk` instant query, aggregated over the identity labels.
///
/// The exporters label one statement once per database, user or schema, so
/// the raw series ranks the same statement several times and splits its time
/// across those rows. `sum by (...)` folds them into the row the report
/// shows, which is also the row the call counter is joined onto.
///
/// Only the comma and the spaces need encoding: parentheses and underscores
/// are safe in a URL query string, and the validators keep every name inside
/// that same safe set.
pub(crate) fn build_topk_query(top_n: usize, series: &str, group_by: &[&str]) -> String {
    let by = by_clause(group_by);
    format!("topk({top_n}%2C%20sum%20{by}%20({series}))")
}

/// Build the call-counter query: aggregated on the same identity, then
/// intersected with the ranked statements.
///
/// Unfiltered, a `pg_stat_statements.max = 10000` instance overruns the body
/// cap in [`crate::http_client`] and the counts fall back to zero, which is
/// the hole this second query exists to close. `join_label` is a caller
/// constant (`queryid`, `digest`), never an operator string.
pub(crate) fn build_counter_query(
    calls_series: &str,
    group_by: &[&str],
    join_label: &str,
    ranked_query: &str,
) -> String {
    let by = by_clause(group_by);
    format!("sum%20{by}%20({calls_series})%20and%20on({join_label})%20{ranked_query}")
}

/// Run an instant query against a Prometheus endpoint and return the body.
///
/// The endpoint and the query are the caller's to validate beforehand. The
/// transport error carries the redacted endpoint, so credentials embedded in
/// a URL never reach stdout or stderr.
pub(crate) async fn fetch_instant_query(
    endpoint: &str,
    query: &str,
    auth_header: Option<&str>,
    user_agent: &str,
) -> Result<bytes::Bytes, String> {
    let parsed_auth = auth_header
        .map(AuthHeader::parse)
        .transpose()
        .map_err(|msg| format!("invalid auth header: {msg}"))?;
    if parsed_auth.is_some() && endpoint.starts_with("http://") {
        tracing::warn!(
            "Sending auth header over cleartext HTTP, prefer https:// to avoid credential leak"
        );
    }

    let client = crate::http_client::build_client();
    let url = format!("{endpoint}/api/v1/query?query={query}");
    let uri: crate::http_client::Uri = url.parse().map_err(|e| format!("invalid URL: {e}"))?;

    let timeout = std::time::Duration::from_secs(30);
    crate::http_client::fetch_get(&client, &uri, user_agent, timeout, parsed_auth.as_ref())
        .await
        .map_err(|e| {
            format!(
                "{e} (endpoint: {})",
                crate::http_client::redact_endpoint(&uri)
            )
        })
}

/// Read `data.result` out of an instant-query response.
///
/// Returns the array so each caller can map its own labels into its own
/// entry type, which is the only part that differs between them.
pub(crate) fn instant_query_results(body: &[u8]) -> Result<Vec<serde_json::Value>, String> {
    let json: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;
    json.get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_array())
        .cloned()
        .ok_or_else(|| "missing data.result array".to_string())
}

/// Read the sample value of one instant-query result.
///
/// The value is `[timestamp, "string_value"]`, and Prometheus always encodes
/// the sample as a string.
pub(crate) fn sample_value(result: &serde_json::Value) -> f64 {
    result
        .get("value")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.get(1))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// Separator for a composite identity key. A `PromQL` label value is arbitrary
/// UTF-8, so nothing forbids it outright, but no exporter puts a control
/// character in a digest or a schema name and both sides of the join build the
/// key the same way.
const KEY_SEP: char = '\u{1}';

/// Join label values into the key both sides of the call-count join use.
///
/// The first label is the identifier and is required: without it the row is
/// dropped rather than matched on anything else, since the statement text is
/// often truncated and a wrong join is worse than a missing count. The rest
/// read as empty when absent, so an exporter that omits one still joins as
/// long as it omits it on both series.
pub(crate) fn identity_key(metric: &serde_json::Value, labels: &[&str]) -> Option<String> {
    let value = |label: &&str| metric.get(*label).and_then(serde_json::Value::as_str);
    let mut key = value(labels.first()?)?.to_string();
    for label in labels.iter().skip(1) {
        key.push(KEY_SEP);
        key.push_str(value(label).unwrap_or_default());
    }
    Some(key)
}

/// Index a counter series by its identity labels.
///
/// Both scrapes need a call count that the exporter publishes as a series of
/// its own rather than a label, keyed by `queryid` on `PostgreSQL` and by
/// `digest` plus `schema` on `MySQL`. The query aggregates on that same
/// identity, so the sum here only guards against an exporter that splits it
/// further.
pub(crate) fn counter_by_labels(
    body: &[u8],
    labels: &[&str],
) -> Result<std::collections::HashMap<String, u64>, String> {
    let results = instant_query_results(body)?;
    let mut counts: std::collections::HashMap<String, u64> =
        std::collections::HashMap::with_capacity(results.len());
    for result in &results {
        let Some(key) = result.get("metric").and_then(|m| identity_key(m, labels)) else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = sample_value(result).max(0.0) as u64;
        counts
            .entry(key)
            .and_modify(|total| *total = total.saturating_add(count))
            .or_insert(count);
    }
    // Not gated on a non-empty result set: the query is intersected with the
    // ranked statements, so a series name that does not exist comes back empty
    // rather than label-less, and that is the case an operator needs told.
    if counts.is_empty() {
        tracing::warn!(
            labels = ?labels,
            "call-count query yielded no usable row, either the series does not \
             exist or it carries none of the join labels; counts stay at zero"
        );
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_query_folds_on_the_identity_and_is_bounded_by_the_ranked_set() {
        // Nothing but the names, the encoded commas and the encoded spaces:
        // anything else in a URL query string is a bug.
        let ranked = build_topk_query(10, "pg_seconds_total", &["queryid", "query"]);
        let query = build_counter_query("pg_calls_total", &["queryid"], "queryid", &ranked);
        assert_eq!(
            query,
            "sum%20by%20(queryid)%20(pg_calls_total)%20and%20on(queryid)%20\
             topk(10%2C%20sum%20by%20(queryid%2C%20query)%20(pg_seconds_total))"
        );
        assert!(!query.contains(' '), "a raw space would break the URL");
    }

    #[test]
    fn the_by_clause_drops_a_repeated_label() {
        // `--query-label queryid` would otherwise emit `by (queryid, queryid)`.
        assert_eq!(
            build_topk_query(5, "s", &["queryid", "queryid"]),
            "topk(5%2C%20sum%20by%20(queryid)%20(s))"
        );
    }

    #[test]
    fn counter_rows_sharing_an_identity_are_summed() {
        // The query already folds on the identity; this only guards against an
        // exporter that splits it further, and against a row with no identifier.
        let body = br#"{"data":{"result":[
            {"metric":{"queryid":"42","datname":"app"},"value":[1,"7"]},
            {"metric":{"queryid":"42","datname":"reporting"},"value":[1,"3"]},
            {"metric":{"datname":"app"},"value":[1,"99"]}]}}"#;
        let counts = counter_by_labels(body, &["queryid"]).expect("parse");
        assert_eq!(counts.get("42").copied(), Some(10));
        assert_eq!(counts.len(), 1, "a row without the identifier is dropped");
    }

    #[test]
    fn a_composite_identity_keeps_the_schemas_apart() {
        // One digest in two schemas is two rows, and each must keep its own
        // count: summing them onto both would inflate every mean.
        let body = br#"{"data":{"result":[
            {"metric":{"digest":"a1","schema":"shop"},"value":[1,"7"]},
            {"metric":{"digest":"a1","schema":"crm"},"value":[1,"3"]}]}}"#;
        let counts = counter_by_labels(body, &["digest", "schema"]).expect("parse");
        assert_eq!(counts.len(), 2);
        let metric = serde_json::json!({"digest": "a1", "schema": "shop"});
        let key = identity_key(&metric, &["digest", "schema"]).expect("key");
        assert_eq!(counts.get(&key).copied(), Some(7));
    }

    #[test]
    fn an_absent_trailing_label_still_joins() {
        // An exporter omitting `schema` omits it on both series, so the two
        // sides still meet; only the identifier itself is mandatory.
        let body = br#"{"data":{"result":[{"metric":{"digest":"a1"},"value":[1,"4"]}]}}"#;
        let counts = counter_by_labels(body, &["digest", "schema"]).expect("parse");
        let metric = serde_json::json!({"digest": "a1"});
        let key = identity_key(&metric, &["digest", "schema"]).expect("key");
        assert_eq!(counts.get(&key).copied(), Some(4));
        assert!(identity_key(&serde_json::json!({"schema": "shop"}), &["digest"]).is_none());
    }

    #[test]
    fn series_name_accepts_the_exporter_defaults() {
        for series in [
            "pg_stat_statements_seconds_total",
            "mysql_perf_schema_events_statements_seconds_total",
            "_leading_underscore",
            "ns:recorded:rule",
        ] {
            assert!(validate_series_name(series).is_ok(), "{series}");
        }
    }

    #[test]
    fn series_name_rejects_anything_that_escapes_the_query_string() {
        // `&` and `#` smuggle a parameter or truncate the query, a space or a
        // brace breaks the URL parse, and a leading digit is not a metric name.
        for series in [
            "pg_stat&admin=1",
            "x#y",
            "has space",
            "pg_stat{job=\"db\"}",
            "9leading_digit",
            "",
        ] {
            assert!(validate_series_name(series).is_err(), "{series}");
        }
    }

    #[test]
    fn endpoint_rejects_credentials_control_characters_and_other_schemes() {
        for endpoint in [
            "http://user:pass@prom:9090",
            "ftp://prom:9090",
            "prom:9090",
            "http://prom:9090\n",
        ] {
            assert!(validate_endpoint(endpoint).is_err(), "{endpoint}");
        }
        assert!(validate_endpoint("https://prom.example:9090").is_ok());
    }

    #[test]
    fn topk_query_encodes_only_the_commas_and_spaces() {
        assert_eq!(
            build_topk_query(
                10,
                "mysql_perf_schema_events_statements_seconds_total",
                &["digest", "digest_text", "schema"]
            ),
            "topk(10%2C%20sum%20by%20(digest%2C%20digest_text%2C%20schema)%20\
             (mysql_perf_schema_events_statements_seconds_total))"
        );
    }

    #[test]
    fn label_name_rejects_what_a_series_name_would_allow() {
        assert!(validate_label_name("digest_text").is_ok());
        // A colon is legal in a metric name and never in a label.
        for label in ["ns:recorded", "has space", "x&y", "9lead", ""] {
            assert!(validate_label_name(label).is_err(), "{label}");
        }
    }

    #[test]
    fn instant_query_reads_results_and_string_samples() {
        let body =
            br#"{"data":{"result":[{"metric":{"digest_text":"SELECT 1"},"value":[1,"2.5"]}]}}"#;
        let results = instant_query_results(body).expect("well-formed response");
        assert_eq!(1, results.len());
        assert!((sample_value(&results[0]) - 2.5).abs() < f64::EPSILON);
        assert!(instant_query_results(b"{}").is_err());
    }
}
