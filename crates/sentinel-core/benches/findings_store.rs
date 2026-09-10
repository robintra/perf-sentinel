//! Criterion benchmark over the findings store's folded read path, the one
//! `GET /api/findings` takes on every dashboard refresh and every page.
//!
//! The store is filled the way a production ring looks: 100 000 retained
//! instances folding into a few thousand distinct signatures, each carrying
//! a real template and suggestion from the detectors. The three cases are
//! the first page, a deep page, and a one-row read, which is the shape of
//! the daemon view's status tick. The store lives behind the `daemon`
//! feature, which the core crate does not enable on its own, so compare
//! with
//! `cargo bench -p perf-sentinel-core --features daemon --bench findings_store -- --save-baseline main`
//! before a change and the same command with `-- --baseline main` after.

use std::collections::HashSet;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use sentinel_core::acknowledgments::enrich_with_signatures;
use sentinel_core::config::Config;
use sentinel_core::correlate;
use sentinel_core::daemon::findings_store::{FindingsFilter, FindingsStore};
use sentinel_core::detect::{self, DetectConfig, Finding, Severity};
use sentinel_core::event::GroupingAttribute;
use sentinel_core::normalize;
use sentinel_core::synth::{self, PatternMix};

/// How many retained instances the daemon holds at the production setting
/// this benchmark mirrors (`max_retained_findings = 100000`).
const RETAINED: usize = 100_000;
/// Distinct signatures the ring folds into. Measured floor on that
/// production daemon: 4331 across ten tenants and nine services.
const DISTINCT: usize = 4_300;

/// Production string sizes, measured on that daemon: templates average
/// 770 bytes and Hibernate's wide selects reach 18 KB, one row in twenty
/// or so; suggestions sit at a few hundred bytes now that
/// `serialized_calls` no longer lists every call. The synthetic detectors
/// emit far shorter strings, and the clone cost of a fold is proportional
/// to them, so the rows are padded to those sizes.
const TEMPLATE_BYTES: usize = 800;
const WIDE_TEMPLATE_BYTES: usize = 16_384;
const WIDE_EVERY: usize = 20;
const SUGGESTION_BYTES: usize = 600;

fn padded(prefix: &str, bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 16);
    s.push_str(prefix);
    let mut col = 0usize;
    while s.len() < bytes {
        s.push_str(&format!(", t.col_{col:04}"));
        col += 1;
    }
    s
}

/// Real findings from the detectors, one per signature, then multiplied
/// into `DISTINCT` signatures by varying the endpoint, which the signature
/// hashes, given the grouping production rows carry, and padded to
/// production sizes so the clone cost this benchmark exists to measure is
/// the one production pays.
fn distinct_findings() -> Vec<Finding> {
    let raw = synth::generate_target_events(20_000, 16, &PatternMix::default(), 42);
    let traces = correlate::correlate(normalize::normalize_all(raw));
    let mut base = detect::run_full_detection(&traces, &DetectConfig::from(&Config::default()));
    assert!(!base.is_empty(), "the synthetic corpus must yield findings");
    // Detection is per trace, so the same problem shows up once per trace
    // that has it: keep one instance per signature, or the endpoint
    // variants below would collide within a variant.
    enrich_with_signatures(&mut base);
    let mut seen = HashSet::new();
    base.retain(|f| seen.insert(f.signature.clone()));
    let grouping = vec![GroupingAttribute {
        key: "k8s.namespace.name".into(),
        value: "tenant-a".into(),
    }];
    let mut out = Vec::with_capacity(DISTINCT);
    'fill: for variant in 0.. {
        for finding in &base {
            if out.len() == DISTINCT {
                break 'fill;
            }
            let mut f = finding.clone();
            f.source_endpoint = format!("{} /v{variant}", f.source_endpoint);
            f.grouping.clone_from(&grouping);
            let template_bytes = if out.len() % WIDE_EVERY == 0 {
                WIDE_TEMPLATE_BYTES
            } else {
                TEMPLATE_BYTES
            };
            f.pattern.template = padded(&f.pattern.template, template_bytes);
            f.suggestion = padded(&f.suggestion, SUGGESTION_BYTES);
            out.push(f);
        }
    }
    enrich_with_signatures(&mut out);
    out
}

/// One instance per signature per batch, batches stamped a second apart,
/// until the ring is full. Every fifth batch is critical so the fold's
/// representative swap runs, as it does on a fleet where a pattern's
/// severity moves with the traffic.
fn filled_store(rt: &tokio::runtime::Runtime) -> FindingsStore {
    let warning = distinct_findings();
    let critical: Vec<Finding> = warning
        .iter()
        .cloned()
        .map(|mut f| {
            f.severity = Severity::Critical;
            f
        })
        .collect();
    let store = FindingsStore::new(RETAINED);
    rt.block_on(async {
        let mut batch = 0u64;
        while store.len().await < RETAINED {
            let instances = if batch.is_multiple_of(5) {
                &critical
            } else {
                &warning
            };
            store.push_batch(instances, 1_000 + batch * 1_000).await;
            batch += 1;
        }
        let folded = store.query_coalesced(&filter(usize::MAX, 0)).await.len();
        assert_eq!(folded, DISTINCT, "the ring must fold into DISTINCT rows");
    });
    store
}

fn filter(limit: usize, offset: usize) -> FindingsFilter {
    let mut f = FindingsFilter::default();
    f.limit = limit;
    f.offset = offset;
    f
}

fn bench_query_coalesced(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let store = filled_store(&rt);
    let mut group = c.benchmark_group("query_coalesced_100k");
    group.sample_size(20);
    for (name, limit, offset) in [
        ("first_page_1000", 1_000usize, 0usize),
        ("deep_page_1000_offset_3000", 1_000, 3_000),
        ("one_row", 1, 0),
    ] {
        let f = filter(limit, offset);
        group.bench_function(name, |b| {
            // The page's drop is the reader's cost, not the store's: keep
            // it out of the measurement so the fold alone is compared.
            b.iter_with_large_drop(|| {
                rt.block_on(black_box(&store).query_coalesced(black_box(&f)))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_query_coalesced);
criterion_main!(benches);
