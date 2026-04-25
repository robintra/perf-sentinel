//! `perf-sentinel query` subcommand: HTTP client for the daemon's
//! `/api/*` endpoints, with colored terminal renderers for each action.
//!
//! Only compiled when the `daemon` feature is enabled. The `inspect`
//! sub-action additionally requires the `tui` feature.

#![cfg(feature = "daemon")]

use crate::QueryAction;
use crate::QueryOutputFormat;
use crate::render::{AnsiColors, ansi_colors, print_findings};

/// Entry point for the `query` subcommand. Validates the daemon URL,
/// dispatches to the per-action handler and exits with a clear error if
/// the daemon is unreachable.
pub(crate) async fn cmd_query(daemon_url: &str, action: QueryAction) {
    let client = sentinel_core::http_client::build_client();
    let timeout = std::time::Duration::from_secs(10);

    // Validate the daemon URL upfront so misconfigurations fail with a
    // clear error before the first request goes out.
    let trimmed = daemon_url.trim_end_matches('/');
    let base_uri: sentinel_core::http_client::Uri = trimmed.parse().unwrap_or_else(|e| {
        eprintln!("Invalid daemon URL `{daemon_url}`: {e}");
        std::process::exit(1);
    });
    if !matches!(base_uri.scheme_str(), Some("http" | "https")) {
        eprintln!("Invalid daemon URL `{daemon_url}`: scheme must be http or https");
        std::process::exit(1);
    }

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
        } => {
            let path = build_findings_path(
                limit,
                service.as_deref(),
                finding_type.as_deref(),
                severity.as_deref(),
            );
            let body = fetch(&path).await;
            render_findings_response(&body, format, daemon_url);
        }
        QueryAction::Explain { trace_id, format } => {
            let body = fetch(&format!("/api/explain/{trace_id}")).await;
            render_explain_response(&body, format);
        }
        #[cfg(feature = "tui")]
        QueryAction::Inspect => {
            let body = fetch("/api/findings?limit=10000").await;
            run_inspect_action(&body, &client, trimmed, timeout).await;
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
    let mut params = vec![format!("limit={limit}")];
    if let Some(s) = service {
        params.push(format!("service={s}"));
    }
    if let Some(t) = finding_type {
        params.push(format!("type={t}"));
    }
    if let Some(s) = severity {
        params.push(format!("severity={s}"));
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

fn render_findings_response(body: &[u8], format: QueryOutputFormat, daemon_url: &str) {
    match format {
        QueryOutputFormat::Json => print_pretty_json(body),
        QueryOutputFormat::Text => print_findings_text(body, daemon_url),
    }
}

fn print_findings_text(body: &[u8], daemon_url: &str) {
    let stored: Vec<sentinel_core::daemon::findings_store::StoredFinding> =
        serde_json::from_slice(body).unwrap_or_default();
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
    println!();
    print_findings(&findings, false);
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
) {
    let stored: Vec<sentinel_core::daemon::findings_store::StoredFinding> =
        serde_json::from_slice(body).unwrap_or_default();
    let findings: Vec<sentinel_core::detect::Finding> =
        stored.into_iter().map(|sf| sf.finding).collect();
    if findings.is_empty() {
        let AnsiColors { green, reset, .. } = ansi_colors(false);
        println!("{green}No findings from daemon. Nothing to inspect.{reset}");
        return;
    }
    // Build minimal Trace stubs from distinct trace_ids. The TUI detail
    // panel needs span trees, but `/api/findings` does not ship them.
    // Fetch them upfront in parallel via `fetch_explain_trees` so the TUI
    // opens immediately instead of stalling on per-trace round-trips
    // (100 traces * 50ms RTT = 5s without concurrency; ~300ms with 16).
    let trace_ids: std::collections::BTreeSet<String> =
        findings.iter().map(|f| f.trace_id.clone()).collect();
    let pre_rendered_trees =
        fetch_explain_trees(client, base_url.to_string(), timeout, &trace_ids, 16).await;
    // Fetch correlations once (single endpoint, no per-trace fanout).
    // Graceful degrade to empty list if the daemon is older or the
    // endpoint is unreachable, the panel will then show its hint.
    let correlations = fetch_correlations(client, base_url, timeout).await;
    let traces: Vec<sentinel_core::correlate::Trace> = trace_ids
        .into_iter()
        .map(|tid| sentinel_core::correlate::Trace {
            trace_id: tid,
            spans: vec![],
        })
        .collect();
    let detect_config = sentinel_core::detect::DetectConfig {
        n_plus_one_threshold: 5,
        window_ms: 500,
        slow_threshold_ms: 500,
        slow_min_occurrences: 3,
        max_fanout: 20,
        chatty_service_min_calls: 15,
        pool_saturation_concurrent_threshold: 10,
        serialized_min_sequential: 3,
    };
    let mut app = crate::tui::App::new(findings, traces, detect_config)
        .with_pre_rendered_trees(pre_rendered_trees)
        .with_correlations(correlations);
    if let Err(e) = crate::tui::run(&mut app) {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}

#[cfg(feature = "tui")]
async fn fetch_correlations(
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
    timeout: std::time::Duration,
) -> Vec<sentinel_core::detect::correlate_cross::CrossTraceCorrelation> {
    let Ok(uri) = format!("{base_url}/api/correlations").parse::<sentinel_core::http_client::Uri>()
    else {
        return Vec::new();
    };
    let Ok(body) =
        sentinel_core::http_client::fetch_get(client, &uri, "perf-sentinel-query", timeout, None)
            .await
    else {
        return Vec::new();
    };
    serde_json::from_slice(&body).unwrap_or_default()
}
