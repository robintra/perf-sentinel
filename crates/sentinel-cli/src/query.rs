//! `perf-sentinel query` subcommand: HTTP client for the daemon's
//! `/api/*` endpoints, with colored terminal renderers for each action.
//!
//! Only compiled when the `daemon` feature is enabled. The `inspect`
//! sub-action additionally requires the `tui` feature.

#![cfg(feature = "daemon")]

use crate::QueryAction;
use crate::QueryOutputFormat;
use crate::render::{AnsiColors, ansi_colors};

/// Entry point for the `query` subcommand. Validates the daemon URL,
/// dispatches to the per-action handler and exits with a clear error if
/// the daemon is unreachable.
pub(crate) async fn cmd_query(daemon_url: &str, action: QueryAction) {
    let client = sentinel_core::http_client::build_client();
    let timeout = std::time::Duration::from_secs(10);

    // Validate the daemon URL upfront so misconfigurations fail with a
    // clear error before the first request goes out. Shared with
    // `perf-sentinel ack` and `perf-sentinel report --daemon-url` for
    // identical rejection of userinfo, paths, query strings and
    // trailing slashes.
    let trimmed = crate::ack::validate_url(daemon_url).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });

    let fetch = |path: &str| {
        let uri: sentinel_core::http_client::Uri =
            format!("{trimmed}{path}").parse().unwrap_or_else(|e| {
                eprintln!("Invalid daemon URL path `{path}`: {e}");
                std::process::exit(1);
            });
        let client = &client;
        async move {
            match sentinel_core::http_client::fetch_get(
                client,
                &uri,
                "perf-sentinel-query",
                timeout,
                None,
            )
            .await
            {
                Ok(body) => body,
                Err(e) => {
                    eprintln!(
                        "Failed to connect to daemon at {daemon_url}: {e}\n\
                         Is `perf-sentinel watch` running?"
                    );
                    std::process::exit(1);
                }
            }
        }
    };

    match action {
        QueryAction::Findings {
            service,
            finding_type,
            severity,
            limit,
            format,
            sort,
        } => {
            let path = build_findings_path(
                limit,
                service.as_deref(),
                finding_type.as_deref(),
                severity.as_deref(),
            );
            let body = fetch(&path).await;
            render_findings_response(&body, format, daemon_url, sort);
        }
        QueryAction::Explain { trace_id, format } => {
            let body = fetch(&format!("/api/explain/{trace_id}")).await;
            render_explain_response(&body, format);
        }
        #[cfg(feature = "tui")]
        QueryAction::Inspect { api_key_file, sort } => {
            let api_key = match crate::ack::resolve_api_key(api_key_file.as_deref()) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            // include_acked=true so FindingResponse carries
            // `acknowledged_by` per finding, which the TUI uses to
            // render the `[acked by ...]` indicator and to populate
            // `acks_by_signature` for the modal write path.
            let limit = crate::ack::FINDINGS_FETCH_LIMIT;
            let path = format!("/api/findings?limit={limit}&include_acked=true");
            let body = fetch(&path).await;
            run_inspect_action(&body, &client, &trimmed, timeout, api_key, sort).await;
        }
        #[cfg(feature = "tui")]
        QueryAction::Monitor { refresh } => {
            crate::monitor::cmd_monitor(&trimmed, refresh);
        }
        QueryAction::Correlations { format } => {
            let body = fetch("/api/correlations").await;
            render_correlations_response(&body, format);
        }
        QueryAction::Status { format } => {
            let body = fetch("/api/status").await;
            render_status_response(&body, format);
        }
    }
}

fn build_findings_path(
    limit: usize,
    service: Option<&str>,
    finding_type: Option<&str>,
    severity: Option<&str>,
) -> String {
    use crate::ack::percent_encode_signature_segment as enc;
    // Percent-encode each value so a `--service "foo&limit=99999"` cannot
    // break out of its parameter slot. Defense-in-depth on a loopback API.
    let mut params = vec![format!("limit={limit}")];
    if let Some(s) = service {
        params.push(format!("service={}", enc(s)));
    }
    if let Some(t) = finding_type {
        params.push(format!("type={}", enc(t)));
    }
    if let Some(s) = severity {
        params.push(format!("severity={}", enc(s)));
    }
    format!("/api/findings?{}", params.join("&"))
}

fn print_pretty_json(body: &[u8]) {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    println!(
        "{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
}

fn render_findings_response(
    body: &[u8],
    format: QueryOutputFormat,
    daemon_url: &str,
    sort: Option<crate::render::FindingsSort>,
) {
    match format {
        QueryOutputFormat::Json => print_pretty_json(body),
        QueryOutputFormat::Text => print_findings_text(body, daemon_url, sort),
    }
}

/// Estimated aggregate avoidable I/O of a folded row: the store keeps
/// one representative per signature, so the exact per-detection sum is
/// gone and `seen_count x` the representative's ops stands in for it.
fn stored_impact(sf: &sentinel_core::daemon::findings_store::StoredFinding) -> u64 {
    let ops = sf
        .finding
        .green_impact
        .as_ref()
        .map_or(0u64, |gi| gi.estimated_extra_io_ops as u64);
    sf.seen_count.saturating_mul(ops)
}

/// Same keys and directions as `analyze --sort` and the dashboard:
/// descending primary, the other axis as the tie-break.
fn sort_stored(
    stored: &mut [sentinel_core::daemon::findings_store::StoredFinding],
    mode: crate::render::FindingsSort,
) {
    stored.sort_by(|a, b| {
        crate::render::compare_severity_impact(
            mode,
            (&a.finding.severity, stored_impact(a)),
            (&b.finding.severity, stored_impact(b)),
        )
    });
}

fn print_findings_text(body: &[u8], daemon_url: &str, sort: Option<crate::render::FindingsSort>) {
    let mut stored: Vec<sentinel_core::daemon::findings_store::StoredFinding> =
        serde_json::from_slice(body).unwrap_or_default();
    if let Some(mode) = sort {
        sort_stored(&mut stored, mode);
    }
    // The store coalesces by signature, so a row can stand for many traces.
    let recurring: Vec<(String, u64)> = stored
        .iter()
        .filter(|sf| sf.seen_count > 1)
        .map(|sf| (sf.finding.signature.clone(), sf.seen_count))
        .collect();
    // The daemon folded these rows: seen_count is the per-problem trace
    // tally the shared renderer cannot recount from single rows.
    let recurrence = stored_recurrence_index(&stored);
    let findings: Vec<sentinel_core::detect::Finding> =
        stored.into_iter().map(|sf| sf.finding).collect();
    if findings.is_empty() {
        let AnsiColors { green, reset, .. } = ansi_colors(false);
        println!("{green}No findings from daemon.{reset}");
        return;
    }
    let AnsiColors {
        bold,
        cyan,
        dim,
        reset,
        ..
    } = ansi_colors(false);
    println!();
    println!(
        "{bold}{cyan}=== perf-sentinel daemon findings ({} results) ==={reset}",
        findings.len()
    );
    println!("{dim}Source: {daemon_url}{reset}");
    if !recurring.is_empty() {
        let total: u64 = recurring.iter().map(|(_, n)| n).sum();
        println!(
            "{dim}{} of them recur, coalesced from {total} detections across traces{reset}",
            recurring.len()
        );
    }
    println!();
    crate::render::print_findings_with_recurrence(&findings, false, Some(recurrence));
}

/// Per-signature tallies from folded rows: the count is the daemon's,
/// the ops total the same `seen_count x` representative estimate the
/// sort uses.
fn stored_recurrence_index(
    stored: &[sentinel_core::daemon::findings_store::StoredFinding],
) -> std::collections::HashMap<String, crate::render::RecurrenceStats> {
    let mut index: std::collections::HashMap<String, crate::render::RecurrenceStats> =
        std::collections::HashMap::new();
    for sf in stored {
        // Accumulate: two rows can share the fallback key when the daemon
        // predates signatures, and overwriting would print one row's tally
        // under the other's block.
        let entry = index
            .entry(crate::render::recurrence_key(&sf.finding))
            .or_insert(crate::render::RecurrenceStats {
                count: 0,
                total_ops: 0,
            });
        entry.count = entry
            .count
            .saturating_add(usize::try_from(sf.seen_count).unwrap_or(usize::MAX));
        entry.total_ops = entry
            .total_ops
            .saturating_add(usize::try_from(stored_impact(sf)).unwrap_or(usize::MAX));
    }
    index
}

fn render_explain_response(body: &[u8], format: QueryOutputFormat) {
    match format {
        QueryOutputFormat::Json => print_pretty_json(body),
        QueryOutputFormat::Text => print_explain_text(body),
    }
}

fn print_explain_text(body: &[u8]) {
    if let Ok(tree) = serde_json::from_slice::<sentinel_core::explain::ExplainTree>(body) {
        let text = sentinel_core::explain::format_tree_text(&tree, true);
        println!("{text}");
        return;
    }
    // Daemon returned an error response (or unparseable JSON).
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    if let Some(err) = json.get("error").and_then(|v| v.as_str()) {
        eprintln!("Error: {err}");
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        );
    }
}

fn render_correlations_response(body: &[u8], format: QueryOutputFormat) {
    match format {
        QueryOutputFormat::Json => print_pretty_json(body),
        QueryOutputFormat::Text => print_correlations_text(body),
    }
}

fn print_correlations_text(body: &[u8]) {
    let correlations: Vec<sentinel_core::detect::correlate_cross::CrossTraceCorrelation> =
        serde_json::from_slice(body).unwrap_or_default();
    if correlations.is_empty() {
        let AnsiColors { green, reset, .. } = ansi_colors(false);
        println!("{green}No active cross-trace correlations.{reset}");
        return;
    }
    let colors = ansi_colors(false);
    let AnsiColors {
        bold, cyan, reset, ..
    } = colors;
    println!();
    println!(
        "{bold}{cyan}=== Cross-trace correlations ({} active) ==={reset}",
        correlations.len()
    );
    println!();
    for (i, c) in correlations.iter().enumerate() {
        print_correlation_entry(i, c, colors);
    }
}

fn print_correlation_entry(
    index: usize,
    c: &sentinel_core::detect::correlate_cross::CrossTraceCorrelation,
    colors: AnsiColors,
) {
    use sentinel_core::text_safety::sanitize_for_terminal;
    let AnsiColors {
        bold,
        red,
        yellow,
        dim,
        reset,
        ..
    } = colors;
    let conf_color = if c.confidence >= 0.8 {
        red
    } else if c.confidence >= 0.5 {
        yellow
    } else {
        dim
    };
    println!(
        "  {bold}#{} {}{reset} in {}",
        index + 1,
        c.source.finding_type.as_str(),
        sanitize_for_terminal(&c.source.service)
    );
    println!(
        "    {dim}->{reset} {} in {}",
        c.target.finding_type.as_str(),
        sanitize_for_terminal(&c.target.service)
    );
    println!(
        "    {dim}Observed:{reset} {} times, \
         {dim}median lag:{reset} {:.1}ms, \
         {conf_color}confidence: {:.0}%{reset}",
        c.co_occurrence_count,
        c.median_lag_ms,
        c.confidence * 100.0
    );
    println!(
        "    {dim}Period:{reset} {} .. {}",
        sanitize_for_terminal(&c.first_seen),
        sanitize_for_terminal(&c.last_seen)
    );
    println!();
}

fn render_status_response(body: &[u8], format: QueryOutputFormat) {
    match format {
        QueryOutputFormat::Json => print_pretty_json(body),
        QueryOutputFormat::Text => print_status_text(body),
    }
}

fn print_status_text(body: &[u8]) {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let AnsiColors {
        bold,
        cyan,
        green,
        dim,
        reset,
        ..
    } = ansi_colors(false);
    println!();
    println!("{bold}{cyan}=== perf-sentinel daemon status ==={reset}");
    println!();
    if let Some(v) = json.get("version").and_then(serde_json::Value::as_str) {
        println!("  {dim}Version:{reset}          {green}{v}{reset}");
    }
    if let Some(u) = json
        .get("uptime_seconds")
        .and_then(serde_json::Value::as_u64)
    {
        let h = u / 3600;
        let m = (u % 3600) / 60;
        let s = u % 60;
        println!("  {dim}Uptime:{reset}           {h}h {m}m {s}s");
    }
    if let Some(t) = json
        .get("active_traces")
        .and_then(serde_json::Value::as_u64)
    {
        println!("  {dim}Active traces:{reset}    {t}");
    }
    if let Some(f) = json
        .get("stored_findings")
        .and_then(serde_json::Value::as_u64)
    {
        println!("  {dim}Stored findings:{reset}  {f}");
    }
    println!();
}

/// Fetch `/api/explain/{trace_id}` for each `trace_id` in parallel with
/// bounded concurrency. Returns a map of successfully-parsed trees keyed
/// by `trace_id`. Traces that return an error response (e.g. aged out of
/// the daemon window) are silently skipped.
///
/// Used by `query inspect` to pre-populate the TUI detail panel without
/// the multi-second startup latency a sequential loop would incur.
#[cfg(feature = "tui")]
async fn fetch_explain_trees(
    client: &sentinel_core::http_client::HttpClient,
    base_url: String,
    timeout: std::time::Duration,
    trace_ids: &std::collections::BTreeSet<String>,
    concurrency: usize,
) -> std::collections::HashMap<String, String> {
    use tokio::task::JoinSet;

    let mut results: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut set: JoinSet<(String, Option<String>)> = JoinSet::new();
    let mut iter = trace_ids.iter();

    // Prime the join set with up to `concurrency` in-flight fetches.
    // `by_ref().take(concurrency)` stops cleanly when either the budget
    // or the trace_ids iterator is exhausted, whichever comes first.
    for tid in iter.by_ref().take(concurrency) {
        spawn_explain_fetch(&mut set, client, &base_url, timeout, tid.clone());
    }

    while let Some(join_result) = set.join_next().await {
        if let Ok((tid, tree_text)) = join_result
            && let Some(text) = tree_text
        {
            results.insert(tid, text);
        }
        // Maintain the concurrency window by launching the next pending
        // fetch as soon as one completes.
        if let Some(tid) = iter.next() {
            spawn_explain_fetch(&mut set, client, &base_url, timeout, tid.clone());
        }
    }

    results
}

#[cfg(feature = "tui")]
fn spawn_explain_fetch(
    set: &mut tokio::task::JoinSet<(String, Option<String>)>,
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
    timeout: std::time::Duration,
    trace_id: String,
) {
    let client = client.clone();
    let base = base_url.to_string();
    set.spawn(async move {
        let Ok(uri) =
            format!("{base}/api/explain/{trace_id}").parse::<sentinel_core::http_client::Uri>()
        else {
            return (trace_id, None);
        };
        let Ok(body) = sentinel_core::http_client::fetch_get(
            &client,
            &uri,
            "perf-sentinel-query",
            timeout,
            None,
        )
        .await
        else {
            return (trace_id, None);
        };
        let text = serde_json::from_slice::<sentinel_core::explain::ExplainTree>(&body)
            .ok()
            .map(|tree| sentinel_core::explain::format_tree_text(&tree, false));
        (trace_id, text)
    });
}

#[cfg(feature = "tui")]
async fn run_inspect_action(
    body: &[u8],
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
    timeout: std::time::Duration,
    api_key: Option<String>,
    sort: Option<crate::render::FindingsSort>,
) {
    let responses: Vec<sentinel_core::daemon::query_api::FindingResponse> =
        serde_json::from_slice(body).unwrap_or_default();
    let acks_by_signature: std::collections::HashMap<
        String,
        sentinel_core::daemon::query_api::AckSource,
    > = responses
        .iter()
        .filter_map(|r| {
            r.acknowledged_by
                .clone()
                .map(|src| (r.stored.finding.signature.clone(), src))
        })
        .collect();
    let findings: Vec<sentinel_core::detect::Finding> =
        responses.into_iter().map(|r| r.stored.finding).collect();
    if findings.is_empty() {
        let AnsiColors { green, reset, .. } = ansi_colors(false);
        println!("{green}No findings from daemon. Nothing to inspect.{reset}");
        return;
    }
    // Build minimal Trace stubs from distinct trace_ids. The TUI detail
    // panel needs span trees, but `/api/findings` does not ship them.
    let trace_ids: std::collections::BTreeSet<String> =
        findings.iter().map(|f| f.trace_id.clone()).collect();
    // Fetch the three independent endpoints concurrently to minimise
    // time-to-first-paint: span trees (`fetch_explain_trees` fans out
    // internally, 100 traces * 50ms RTT = 5s serial vs ~300ms at 16),
    // cross-trace correlations, and the report snapshot backing the
    // Analyze view (GreenOps waste, top offenders, quality gate). Each
    // degrades gracefully (empty / None) on an older or unreachable
    // daemon, so the corresponding panel shows its hint.
    let (pre_rendered_trees, correlations, report) = tokio::join!(
        fetch_explain_trees(client, base_url.to_string(), timeout, &trace_ids, 16),
        fetch_correlations(client, base_url, timeout),
        fetch_report(client, base_url, timeout),
    );
    let traces: Vec<sentinel_core::correlate::Trace> = trace_ids
        .into_iter()
        .map(|tid| sentinel_core::correlate::Trace {
            trace_id: tid,
            spans: vec![],
        })
        .collect();
    let app = crate::tui::App::new(findings, traces)
        .with_pre_rendered_trees(pre_rendered_trees)
        .with_correlations(correlations)
        .with_initial_sort(sort);
    let app = match report {
        Some(report) => app.with_summary(crate::tui::AnalyzeSummary {
            green_summary: report.green_summary,
            quality_gate: report.quality_gate,
            analysis: report.analysis,
        }),
        None => app,
    };
    let mut app = app.with_daemon_handle(base_url.to_string(), api_key, acks_by_signature);
    // `block_in_place` lets the synchronous `run_loop` (crossterm's
    // `event::read` is blocking) call `Handle::current().block_on(...)`
    // from inside `submit_ack_modal` without panicking the multi-thread
    // tokio runtime. The UI freezes for the ~100-300ms duration of the
    // ack write. Acceptable scope-minimal tradeoff, an async event loop
    // is a candidate followup.
    let result = tokio::task::block_in_place(|| crate::tui::run(&mut app));
    if let Err(e) = result {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}

/// GET `{base_url}{path}` and deserialize the JSON body. Returns `None`
/// on any failure (bad URL, transport error, non-2xx, parse error), the
/// graceful-degrade contract every TUI fetch in this crate shares. The
/// canonical fetch idiom: `query monitor` reuses it for its polling.
#[cfg(feature = "tui")]
pub(crate) async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
    path: &str,
    timeout: std::time::Duration,
) -> Option<T> {
    fetch_json_reporting(client, base_url, path, timeout)
        .await
        .ok()
}

/// Sibling of [`fetch_json`] that keeps the failure reason instead of
/// flattening it to `None`.
///
/// Used for `/api/export/report`, the one payload whose size an operator
/// controls: a body over the client's limit there means the daemon's
/// export knobs were raised past what its own clients can read, and
/// reporting that as a plain unreachable daemon sends the reader looking
/// at the network for a configuration problem.
#[cfg(feature = "tui")]
pub(crate) async fn fetch_json_reporting<T: serde::de::DeserializeOwned>(
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
    path: &str,
    timeout: std::time::Duration,
) -> Result<T, String> {
    let uri = format!("{base_url}{path}")
        .parse::<sentinel_core::http_client::Uri>()
        .map_err(|e| format!("invalid URL: {e}"))?;
    let body =
        sentinel_core::http_client::fetch_get(client, &uri, "perf-sentinel-query", timeout, None)
            .await
            .map_err(|e| match e {
                // Kept under `monitor::HEADER_REASON_MAX_CHARS` so the
                // truncated header still carries the action: the fix is a
                // setting on the peer, and a reason cut before it names one
                // is no better than the bare [STALE] marker it replaces.
                sentinel_core::http_client::FetchError::BodyTooLarge(limit) => format!(
                    "{path} over the {} MiB read limit: lower max_export_findings \
                     or max_retained_traces",
                    limit / (1024 * 1024)
                ),
                other => other.to_string(),
            })?;
    serde_json::from_slice(&body).map_err(|e| format!("{path}: malformed response ({e})"))
}

#[cfg(feature = "tui")]
async fn fetch_correlations(
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
    timeout: std::time::Duration,
) -> Vec<sentinel_core::detect::correlate_cross::CrossTraceCorrelation> {
    fetch_json(client, base_url, "/api/correlations", timeout)
        .await
        .unwrap_or_default()
}

/// Fetch the daemon's report snapshot from `/api/export/report` to back
/// the TUI's Analyze view. Returns `None` (graceful degrade) when the
/// endpoint is unreachable or the payload does not parse, e.g. an older
/// daemon, in which case the Analyze view renders its unavailable hint.
#[cfg(feature = "tui")]
async fn fetch_report(
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
    timeout: std::time::Duration,
) -> Option<sentinel_core::report::Report> {
    fetch_json(client, base_url, "/api/export/report", timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_core::daemon::findings_store::StoredFinding;

    fn stored(severity: &str, seen: u64, ops: usize) -> StoredFinding {
        serde_json::from_value(serde_json::json!({
            "finding": {
                "type": "redundant_sql",
                "severity": severity,
                "trace_id": "t1",
                "service": "svc",
                "source_endpoint": "GET /x",
                "pattern": { "template": "select 1", "occurrences": 2, "window_ms": 100, "distinct_params": 1 },
                "suggestion": "dedupe",
                "first_timestamp": "2026-08-05T10:00:00Z",
                "last_timestamp": "2026-08-05T10:00:01Z",
                "green_impact": {
                    "estimated_extra_io_ops": ops,
                    "io_intensity_score": 1.0,
                    "io_intensity_band": "moderate"
                },
                "confidence": "daemon_staging"
            },
            "stored_at_ms": 1_000,
            "seen_count": seen,
        }))
        .expect("StoredFinding deserializes")
    }

    #[test]
    fn stored_recurrence_index_carries_the_daemon_tally() {
        let mut info_row = stored("info", 40, 2);
        info_row.finding.signature = "sig-info".to_string();
        let mut crit_row = stored("critical", 1, 9);
        crit_row.finding.signature = "sig-crit".to_string();
        let rows = vec![info_row, crit_row];
        let index = stored_recurrence_index(&rows);
        assert_eq!(index.len(), 2);
        let info = &index["sig-info"];
        assert_eq!(info.count, 40);
        assert_eq!(info.total_ops, 80, "seen_count x representative ops");
    }

    #[test]
    fn impact_sort_puts_the_frequent_info_first() {
        let mut rows = vec![stored("critical", 1, 9), stored("info", 40, 2)];
        sort_stored(&mut rows, crate::render::FindingsSort::Impact);
        assert_eq!(
            rows[0].finding.severity,
            sentinel_core::detect::Severity::Info
        );
        assert_eq!(stored_impact(&rows[0]), 80);
        sort_stored(&mut rows, crate::render::FindingsSort::Severity);
        assert_eq!(
            rows[0].finding.severity,
            sentinel_core::detect::Severity::Critical
        );
    }
}
