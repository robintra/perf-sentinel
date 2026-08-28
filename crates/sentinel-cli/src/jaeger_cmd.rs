//! `perf-sentinel jaeger-query` subcommand: fetch traces from the
//! Jaeger query HTTP API and run them through the batch pipeline.

#![cfg(feature = "jaeger-query")]

use sentinel_core::pipeline;
use tracing::info;

use crate::render::emit_report_and_gate;
use crate::{OutputFormat, apply_acknowledgments_or_exit, grouping_keys, load_config};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn cmd_jaeger_query(
    endpoint: &str,
    trace_id: Option<&str>,
    service: Option<&str>,
    lookback: &str,
    from: Option<&str>,
    to: Option<&str>,
    max_traces: usize,
    auth_header: Option<&str>,
    config_path: Option<&std::path::Path>,
    sort: Option<crate::render::FindingsSort>,
    format: Option<OutputFormat>,
    ci: bool,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
    show_acknowledged: bool,
) {
    if trace_id.is_none() && service.is_none() {
        eprintln!("Error: either --trace-id or --service is required");
        std::process::exit(crate::EXIT_TOOLING_ERROR);
    }
    if trace_id.is_some() && service.is_some() {
        eprintln!("Error: --trace-id and --service are mutually exclusive");
        std::process::exit(crate::EXIT_TOOLING_ERROR);
    }

    let window = crate::resolve_search_window_or_exit(lookback, from, to);

    let config = load_config(config_path);

    let events = match sentinel_core::ingest::jaeger_query::ingest_from_jaeger_query_with_grouping(
        endpoint,
        service,
        trace_id,
        window,
        max_traces,
        auth_header,
        grouping_keys(&config),
    )
    .await
    {
        Ok(events) => events,
        Err(e) => {
            eprintln!("Error fetching traces from Jaeger query API: {e}");
            std::process::exit(crate::EXIT_TOOLING_ERROR);
        }
    };

    info!(
        events = events.len(),
        "Ingested events from Jaeger query API, running analysis"
    );

    let (mut report, traces) = pipeline::analyze_with_traces(events, &config, None);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
        sentinel_core::acknowledgments::ReportOrigin::FreshAnalysis,
    );
    // This JSON is rendered later by `report --input`, which sees no raw
    // traces, so carry the masked spans the way the daemon export does.
    // After the acks, so a suppressed finding does not drag its tree along.
    sentinel_core::report::embedded::embed_finding_traces(&mut report, &traces);
    // After the acks so a masked finding does not weigh in the aggregate,
    // and before the sinks so `--format json` comes out ranked too.
    if let Some(mode) = sort {
        crate::render::sort_findings(&mut report.findings, mode);
    }
    emit_report_and_gate(&mut report, format, ci, "jaeger-query", show_acknowledged);
}
