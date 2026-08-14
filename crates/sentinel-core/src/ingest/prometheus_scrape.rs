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

/// Build the `topk` instant query over a validated series name.
///
/// Only the comma needs encoding: the parentheses and underscores are safe
/// in a URL query string, and [`validate_series_name`] keeps the series
/// inside that same safe set.
pub(crate) fn build_topk_query(top_n: usize, series: &str) -> String {
    format!("topk({top_n}%2C%20{series})")
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

/// Index a counter series by an identity label.
///
/// Both scrapes need a call count that the exporter publishes as a series of
/// its own rather than a label, keyed by `queryid` on `PostgreSQL` and by
/// `digest` on `MySQL`. Rows without that label are dropped rather than
/// matched on anything else: the statement text is often truncated by the
/// exporter, so two distinct statements can share it and a wrong join is
/// worse than a missing count.
pub(crate) fn counter_by_label(
    body: &[u8],
    label: &str,
) -> Result<std::collections::HashMap<String, u64>, String> {
    let results = instant_query_results(body)?;
    let mut counts = std::collections::HashMap::with_capacity(results.len());
    for result in &results {
        let Some(id) = result
            .get("metric")
            .and_then(|m| m.get(label))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = sample_value(result).max(0.0) as u64;
        counts.insert(id.to_string(), count);
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn topk_query_encodes_only_the_comma() {
        assert_eq!(
            build_topk_query(10, "mysql_perf_schema_events_statements_seconds_total"),
            "topk(10%2C%20mysql_perf_schema_events_statements_seconds_total)"
        );
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
