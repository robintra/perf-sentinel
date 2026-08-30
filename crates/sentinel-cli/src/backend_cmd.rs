//! `perf-sentinel tempo` and `perf-sentinel jaeger-query`: fetch traces
//! from a tracing backend's HTTP API and run them through the batch
//! pipeline.
//!
//! The two subcommands are one command over two backends. They differ by
//! the ingest they call and by how the backend is named to the operator,
//! and they were two files that a diff put seven tokens apart.

#![cfg(any(feature = "tempo", feature = "jaeger-query"))]

use sentinel_core::pipeline;
use tracing::info;

use crate::render::emit_report_and_gate;
use crate::{OutputFormat, apply_acknowledgments_or_exit, grouping_keys, load_config};

/// Which backend the query runs against.
#[derive(Clone, Copy)]
pub(crate) enum QueryBackend {
    #[cfg(feature = "tempo")]
    Tempo,
    #[cfg(feature = "jaeger-query")]
    JaegerQuery,
}

impl QueryBackend {
    /// The value that reaches the report as its source.
    fn label(self) -> &'static str {
        match self {
            #[cfg(feature = "tempo")]
            Self::Tempo => "tempo",
            #[cfg(feature = "jaeger-query")]
            Self::JaegerQuery => "jaeger-query",
        }
    }

    /// How the backend is named in the two operator-facing messages.
    fn display(self) -> &'static str {
        match self {
            #[cfg(feature = "tempo")]
            Self::Tempo => "Tempo",
            #[cfg(feature = "jaeger-query")]
            Self::JaegerQuery => "Jaeger query API",
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn cmd_backend_query(
    backend: QueryBackend,
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
    let grouping = grouping_keys(&config);

    // Each backend carries its own error type and neither is inspected
    // past its message, so they meet as strings rather than as a third
    // enum nothing would match on.
    let fetched = match backend {
        #[cfg(feature = "tempo")]
        QueryBackend::Tempo => sentinel_core::ingest::tempo::ingest_from_tempo_with_grouping(
            endpoint,
            service,
            trace_id,
            window,
            max_traces,
            auth_header,
            grouping,
        )
        .await
        .map_err(|e| e.to_string()),
        #[cfg(feature = "jaeger-query")]
        QueryBackend::JaegerQuery => {
            sentinel_core::ingest::jaeger_query::ingest_from_jaeger_query_with_grouping(
                endpoint,
                service,
                trace_id,
                window,
                max_traces,
                auth_header,
                grouping,
            )
            .await
            .map_err(|e| e.to_string())
        }
    };

    let events = match fetched {
        Ok(events) => events,
        Err(e) => {
            eprintln!("Error fetching traces from {}: {e}", backend.display());
            std::process::exit(crate::EXIT_TOOLING_ERROR);
        }
    };

    info!(
        events = events.len(),
        backend = backend.label(),
        "Ingested events from the backend, running analysis"
    );

    let (mut report, traces) = pipeline::analyze_with_traces(events, &config, None);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
        sentinel_core::acknowledgments::ReportOrigin::FreshAnalysis,
    );
    // The seam embeds the findings' masked spans into the JSON sink, so
    // this JSON still draws span trees when `report --input` renders it
    // later without its input, the way the daemon export does.
    emit_report_and_gate(
        &mut report,
        format,
        ci,
        backend.label(),
        sort,
        Some(traces),
        show_acknowledged,
    );
}
