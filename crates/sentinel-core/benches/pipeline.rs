//! Criterion benchmarks over the batch pipeline stages.
//!
//! Every input comes from the seeded generator in `sentinel_core::synth`,
//! so runs are reproducible across machines and baselines. Compare with
//! `cargo bench -p perf-sentinel-core -- --save-baseline main` before a
//! change and `-- --baseline main` after.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use sentinel_core::config::Config;
use sentinel_core::correlate::{self, Trace};
use sentinel_core::detect::{self, DetectConfig};
use sentinel_core::event::SpanEvent;
use sentinel_core::normalize;
use sentinel_core::pipeline;
use sentinel_core::score;
use sentinel_core::score::carbon::CarbonContext;
use sentinel_core::synth::{self, PatternMix, SynthSpec};

fn events(n: usize) -> Vec<SpanEvent> {
    synth::generate_target_events(n, 16, &PatternMix::default(), 42)
}

fn traces_for(n_events: usize) -> Vec<Trace> {
    correlate::correlate(normalize::normalize_all(events(n_events)))
}

/// Pattern-pure dataset: isolates one detector's dominant cost while
/// still going through the public `run_full_detection` entry point.
fn pattern_traces(pattern: &str, n_traces: usize) -> Vec<Trace> {
    let mut mix = PatternMix {
        n_plus_one: 0,
        redundant: 0,
        chatty: 0,
        fanout: 0,
        slow: 0,
        clean: 0,
    };
    match pattern {
        "n_plus_one" => mix.n_plus_one = 1,
        "redundant" => mix.redundant = 1,
        "chatty" => mix.chatty = 1,
        "fanout" => mix.fanout = 1,
        "slow" => mix.slow = 1,
        _ => mix.clean = 1,
    }
    let spec = SynthSpec {
        services: 16,
        traces: n_traces,
        spans_per_trace: 8,
        mix,
        seed: 42,
    };
    correlate::correlate(normalize::normalize_all(synth::generate(&spec)))
}

fn bench_normalize_micro(c: &mut Criterion) {
    let sql = &events(64)[0];
    let mut group = c.benchmark_group("normalize_micro");
    group.bench_function("sql_with_literals", |b| {
        b.iter_batched(
            || sql.clone(),
            |e| black_box(normalize::normalize(e)),
            BatchSize::SmallInput,
        );
    });
    let mut http = sql.clone();
    http.event_type = sentinel_core::event::EventType::HttpOut;
    "http://user-svc:5000/api/users/0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9/orders?page=3&size=50"
        .clone_into(&mut http.target);
    group.bench_function("http_uuid_and_query", |b| {
        b.iter_batched(
            || http.clone(),
            |e| black_box(normalize::normalize(e)),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_normalize_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalize_all");
    for n in [10_000usize, 100_000] {
        let input = events(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("{n}_events"), |b| {
            b.iter_batched(
                || input.clone(),
                |ev| black_box(normalize::normalize_all(ev)),
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

fn bench_correlate(c: &mut Criterion) {
    let mut group = c.benchmark_group("correlate");
    for n in [10_000usize, 100_000] {
        let normalized = normalize::normalize_all(events(n));
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("{n}_events"), |b| {
            b.iter_batched(
                || normalized.clone(),
                |ev| black_box(correlate::correlate(ev)),
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

fn bench_detectors(c: &mut Criterion) {
    let config = DetectConfig::from(&Config::default());
    let mut group = c.benchmark_group("detect");
    for pattern in [
        "n_plus_one",
        "redundant",
        "chatty",
        "fanout",
        "slow",
        "clean",
    ] {
        let traces = pattern_traces(pattern, 1_000);
        let spans: usize = traces.iter().map(|t| t.spans.len()).sum();
        group.throughput(Throughput::Elements(spans as u64));
        group.bench_function(format!("{pattern}_1k_traces"), |b| {
            b.iter(|| black_box(detect::run_full_detection(black_box(&traces), &config)));
        });
    }
    group.finish();
}

fn bench_score_green(c: &mut Criterion) {
    let ctx = CarbonContext {
        default_region: Some("eu-west-3".to_string()),
        ..CarbonContext::default()
    };
    let mut group = c.benchmark_group("score_green");
    for n in [10_000usize, 100_000] {
        let traces = traces_for(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("{n}_events"), |b| {
            b.iter(|| black_box(score::score_green(black_box(&traces), vec![], Some(&ctx))));
        });
    }
    group.finish();
}

fn bench_full_pipeline(c: &mut Criterion) {
    let config = Config::default();
    let mut group = c.benchmark_group("pipeline_analyze");
    for n in [10_000usize, 100_000] {
        let input = events(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("{n}_events"), |b| {
            b.iter_batched(
                || input.clone(),
                |ev| black_box(pipeline::analyze(ev, &config)),
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();

    let mut big = c.benchmark_group("pipeline_analyze_1m");
    big.sample_size(10);
    let input = events(1_000_000);
    big.throughput(Throughput::Elements(1_000_000));
    big.bench_function("1000000_events", |b| {
        b.iter_batched(
            || input.clone(),
            |ev| black_box(pipeline::analyze(ev, &config)),
            BatchSize::LargeInput,
        );
    });
    big.finish();
}

fn bench_time_parse(c: &mut Criterion) {
    c.bench_function("time_parse_iso8601", |b| {
        b.iter(|| black_box(synth::parse_ts_ms(black_box("2025-07-10T14:32:01.123Z"))));
    });
}

fn bench_prometheus_counter(c: &mut Criterion) {
    // The ServiceMeter hot-path question: per-event label lookup vs a
    // pre-cached child handle (the pattern used for the OTLP counters).
    let vec = prometheus::CounterVec::new(
        prometheus::Opts::new("bench_counter", "bench"),
        &["service"],
    )
    .expect("counter creation");
    let services: Vec<String> = (0..64).map(|i| format!("synth-svc-{i:04}")).collect();
    let mut group = c.benchmark_group("service_counter");
    group.bench_function("with_label_values_per_event", |b| {
        let mut i = 0usize;
        b.iter(|| {
            vec.with_label_values(&[services[i % services.len()].as_str()])
                .inc();
            i += 1;
        });
    });
    group.bench_function("cached_child_per_event", |b| {
        let children: Vec<prometheus::Counter> = services
            .iter()
            .map(|s| vec.with_label_values(&[s.as_str()]))
            .collect();
        let mut i = 0usize;
        b.iter(|| {
            children[i % children.len()].inc();
            i += 1;
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_normalize_micro,
    bench_normalize_all,
    bench_correlate,
    bench_detectors,
    bench_score_green,
    bench_full_pipeline,
    bench_time_parse,
    bench_prometheus_counter
);
criterion_main!(benches);
