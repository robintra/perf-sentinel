//! `perf-sentinel demo` subcommand: run the embedded demo dataset
//! through the pipeline and render it as a terminal report, HTML
//! dashboard, or TUI.

use sentinel_core::config::Config;
use tracing::info;

use crate::render::print_colored_report;
#[cfg(feature = "tui")]
use crate::tui;
#[cfg(feature = "tui")]
use crate::tui_launch::{launch_unified_tui, require_terminal_or_exit};
use crate::{
    DEFAULT_PG_STAT_TOP, limits, load_config, load_report_from_input, write_file_no_follow,
};

pub(crate) fn cmd_demo(
    config_path: Option<&std::path::Path>,
    html: Option<&std::path::Path>,
    #[cfg(feature = "tui")] tui: bool,
) {
    const DEMO_DATA: &str = include_str!("demo_data.json");

    let mut config = load_config(config_path);
    // Default to eu-west-3 for demo CO2 display if no region configured
    if config.green.default_region.is_none() {
        config.green.default_region = Some("eu-west-3".to_string());
    }

    // The TUI and HTML paths both need the correlated traces, not just the
    // report, so go through the same loader the analyze/report commands use.
    let (mut report, traces) = load_report_from_input(DEMO_DATA.as_bytes(), &config);

    // Cross-trace correlations are a daemon-only signal; the batch pipeline
    // never produces them. Seed illustrative ones so the demo can show the
    // Correlations tab (HTML) and panel (TUI) without a running daemon.
    report.correlations = demo_correlations();

    // The offline io_proxy model leaves per-region measured/estimated
    // provenance unset. Tag the demo regions the way Electricity Maps would:
    // the larger regions are measured live, the smallest only has an estimate.
    seed_demo_region_provenance(&mut report.green_summary.regions);

    if let Some(path) = html {
        // Field-by-field on a Default: RenderOptions is #[non_exhaustive],
        // so cross-crate struct literals do not compile.
        let mut options = sentinel_core::report::html::RenderOptions::default();
        options.input_label = "demo dataset".to_string();
        // Showcase the pg_stat, mysql_stat and Diff tabs from embedded demo
        // fixtures so the dashboard is fully populated without external inputs.
        options.pg_stat = Some(demo_pg_stat());
        options.mysql_stat = Some(demo_mysql_stat());
        options.diff = Some(demo_diff(&report, &config));
        let (html_out, _stats) = sentinel_core::report::html::render(&report, &traces, &options);
        if let Err(e) = write_file_no_follow(path, html_out.as_bytes()) {
            eprintln!("Error writing HTML report to {}: {e}", path.display());
            std::process::exit(1);
        }
        info!("HTML report written to {}", path.display());
        return;
    }

    #[cfg(feature = "tui")]
    if tui {
        require_terminal_or_exit();
        let detect_config = sentinel_core::detect::DetectConfig::from(&config);
        launch_unified_tui(report, traces, detect_config, tui::View::Analyze, None);
        return;
    }

    print_colored_report(&report, "demo");
}

/// Surface a mix of provenance states on the demo regions (the offline
/// `io_proxy` path leaves these fields unset). To showcase all three states,
/// the largest region is measured live (`RealTime`), the smallest is estimated,
/// and any region in between keeps the unset/unknown state (rendered as "-").
fn seed_demo_region_provenance(regions: &mut [sentinel_core::score::carbon::RegionBreakdown]) {
    use sentinel_core::score::carbon::IntensitySource;
    let min_idx = regions
        .iter()
        .enumerate()
        .min_by_key(|(_, r)| r.io_ops)
        .map(|(i, _)| i);
    let max_idx = regions
        .iter()
        .enumerate()
        .max_by_key(|(_, r)| r.io_ops)
        .map(|(i, _)| i);
    for (i, r) in regions.iter_mut().enumerate() {
        if Some(i) == min_idx {
            // Estimated only ever comes from a live Electricity Maps query.
            r.intensity_source = IntensitySource::RealTime;
            r.intensity_estimated = Some(true);
            r.intensity_estimation_method = Some("time_slicer_average".to_string());
        } else if Some(i) == max_idx {
            r.intensity_source = IntensitySource::RealTime;
            r.intensity_estimated = Some(false);
        }
        // Regions in between keep intensity_estimated = None ("-").
    }
}

/// Rank the embedded demo `pg_stat_statements` snapshot for the dashboard's
/// `pg_stat` tab. The fixture deliberately overlaps the demo SQL templates so
/// the Explain-to-`pg_stat` cross-navigation lights up.
fn demo_pg_stat() -> sentinel_core::ingest::pg_stat::PgStatReport {
    const DEMO_PG_STAT: &str = include_str!("demo_pg_stat.json");
    let entries = sentinel_core::ingest::pg_stat::parse_pg_stat(
        DEMO_PG_STAT.as_bytes(),
        limits::MAX_BATCH_INPUT_BYTES,
    )
    .expect("embedded demo pg_stat fixture is valid");
    sentinel_core::ingest::pg_stat::rank_pg_stat(&entries, DEFAULT_PG_STAT_TOP)
}

/// Rank the embedded demo `performance_schema` digest snapshot for the
/// dashboard's `mysql_stat` tab, so the demo showcases every tab.
fn demo_mysql_stat() -> sentinel_core::ingest::mysql_stat::MySqlStatReport {
    const DEMO_MYSQL_STAT: &str = include_str!("demo_mysql_stat.json");
    let entries = sentinel_core::ingest::mysql_stat::parse_mysql_stat(
        DEMO_MYSQL_STAT.as_bytes(),
        limits::MAX_BATCH_INPUT_BYTES,
    )
    .expect("embedded demo mysql_stat fixture is valid");
    sentinel_core::ingest::mysql_stat::rank_mysql_stat(&entries, DEFAULT_PG_STAT_TOP)
}

/// Diff the demo run against an embedded "previous run" so the dashboard's
/// Diff tab shows resolved/new findings and per-endpoint deltas.
fn demo_diff(
    current: &sentinel_core::report::Report,
    config: &Config,
) -> sentinel_core::diff::DiffReport {
    const DEMO_BASELINE: &str = include_str!("demo_baseline_data.json");
    let (baseline, _) = load_report_from_input(DEMO_BASELINE.as_bytes(), config);
    sentinel_core::diff::diff_runs(&baseline, current)
}

/// Hand-built cross-trace correlations for the demo. Batch analysis never
/// emits these (the correlator is daemon-only), so they are illustrative
/// and coherent with the demo traces rather than computed.
fn demo_correlations() -> Vec<sentinel_core::detect::correlate_cross::CrossTraceCorrelation> {
    use sentinel_core::detect::FindingType;
    use sentinel_core::detect::correlate_cross::{CorrelationEndpoint, CrossTraceCorrelation};

    let pair = |source: CorrelationEndpoint,
                target: CorrelationEndpoint,
                co_occurrence_count: u32,
                source_total_occurrences: u32,
                median_lag_ms: f64,
                sample_trace_id: &str| CrossTraceCorrelation {
        confidence: f64::from(co_occurrence_count) / f64::from(source_total_occurrences),
        source,
        target,
        co_occurrence_count,
        source_total_occurrences,
        median_lag_ms,
        first_seen: "2025-07-10T14:00:00.000Z".to_string(),
        last_seen: "2025-07-10T14:32:00.000Z".to_string(),
        sample_trace_id: Some(sample_trace_id.to_string()),
    };
    let endpoint = |finding_type: FindingType, service: &str, template: &str| CorrelationEndpoint {
        finding_type,
        service: service.to_string(),
        template: template.to_string(),
    };

    vec![
        pair(
            endpoint(
                FindingType::NPlusOneSql,
                "order-svc",
                "SELECT * FROM order_item WHERE order_id = ?",
            ),
            endpoint(
                FindingType::ChattyService,
                "gateway",
                "POST /api/orders/99/submit",
            ),
            42,
            50,
            18.0,
            "trace-demo-chatty",
        ),
        pair(
            endpoint(FindingType::PoolSaturation, "payment-svc", "payment-svc"),
            endpoint(
                FindingType::SerializedCalls,
                "checkout-svc",
                "POST /api/checkout/finalize",
            ),
            32,
            40,
            55.0,
            "trace-demo-serial",
        ),
        pair(
            endpoint(
                FindingType::NPlusOneHttp,
                "inventory-svc",
                "GET /api/products/{id}",
            ),
            endpoint(
                FindingType::ExcessiveFanout,
                "catalog-svc",
                "GET /api/catalog/page",
            ),
            27,
            33,
            9.0,
            "trace-demo-fanout",
        ),
    ]
}
