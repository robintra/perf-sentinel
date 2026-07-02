//! `perf-sentinel tempo` subcommand: fetch traces from the Grafana
//! Tempo HTTP API and run them through the batch pipeline.

#![cfg(feature = "tempo")]

use sentinel_core::pipeline;
use tracing::info;

use crate::render::emit_report_and_gate;
use crate::{OutputFormat, apply_acknowledgments_or_exit, load_config};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn cmd_tempo(
    endpoint: &str,
    trace_id: Option<&str>,
    service: Option<&str>,
    lookback: &str,
    max_traces: usize,
    auth_header: Option<&str>,
    config_path: Option<&std::path::Path>,
    format: Option<OutputFormat>,
    ci: bool,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
    show_acknowledged: bool,
) {
    if trace_id.is_none() && service.is_none() {
        eprintln!("Error: either --trace-id or --service is required");
        std::process::exit(1);
    }
    if trace_id.is_some() && service.is_some() {
        eprintln!("Error: --trace-id and --service are mutually exclusive");
        std::process::exit(1);
    }

    let lookback_duration = match sentinel_core::ingest::tempo::parse_lookback(lookback) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error parsing lookback: {e}");
            std::process::exit(1);
        }
    };

    let config = load_config(config_path);

    let events = match sentinel_core::ingest::tempo::ingest_from_tempo(
        endpoint,
        service,
        trace_id,
        lookback_duration,
        max_traces,
        auth_header,
    )
    .await
    {
        Ok(events) => events,
        Err(e) => {
            eprintln!("Error fetching traces from Tempo: {e}");
            std::process::exit(1);
        }
    };

    info!(
        events = events.len(),
        "Ingested events from Tempo, running analysis"
    );

    let mut report = pipeline::analyze(events, &config);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );
    emit_report_and_gate(&mut report, format, ci, "tempo", show_acknowledged);
}
