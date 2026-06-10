//! Seeded synthetic trace generator for benchmarks and large-input fixtures.
//!
//! Deterministic: the same [`SynthSpec`] always yields the same events, so
//! criterion baselines and `bench --synthetic` runs are reproducible.
//! Hidden from the public API surface, the shapes mirror the demo dataset
//! and the detector fixtures (one anti-pattern per trace plus clean noise).

use std::sync::Arc;

use crate::event::{EventSource, EventType, SpanEvent};
use crate::time::millis_to_iso8601;

/// Relative weights of the per-trace patterns drawn by [`generate`].
#[derive(Debug, Clone)]
pub struct PatternMix {
    pub n_plus_one: u32,
    pub redundant: u32,
    pub chatty: u32,
    pub fanout: u32,
    pub slow: u32,
    pub clean: u32,
}

impl Default for PatternMix {
    /// Realistic fleet mix: mostly clean traffic, N+1 as the dominant
    /// anti-pattern, the rest as a long tail.
    fn default() -> Self {
        Self {
            n_plus_one: 30,
            redundant: 10,
            chatty: 10,
            fanout: 5,
            slow: 5,
            clean: 40,
        }
    }
}

impl PatternMix {
    fn total(&self) -> u32 {
        self.n_plus_one + self.redundant + self.chatty + self.fanout + self.slow + self.clean
    }
}

/// Specification for one deterministic synthetic dataset.
#[derive(Debug, Clone)]
pub struct SynthSpec {
    /// Number of distinct `service.name` values to spread traces over.
    pub services: usize,
    /// Number of traces to generate.
    pub traces: usize,
    /// Event count target for clean traces (patterns have their own shapes).
    pub spans_per_trace: usize,
    pub mix: PatternMix,
    pub seed: u64,
}

impl Default for SynthSpec {
    fn default() -> Self {
        Self {
            services: 8,
            traces: 100,
            spans_per_trace: 6,
            mix: PatternMix::default(),
            seed: 42,
        }
    }
}

/// xorshift64* PRNG: tiny, deterministic, no `rand` dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Avoid the all-zero fixed point.
        Self(seed.wrapping_mul(2_685_821_657_736_338_717).max(1))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(2_685_821_657_736_338_717)
    }

    /// Uniform pick in `[0, n)`. `n` must be nonzero.
    fn pick(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % n.max(1) as u64).unwrap_or(0)
    }
}

/// Base instant for all generated timestamps (2025-07-10T14:32:01Z).
const BASE_MS: u64 = 1_752_157_921_000;

const REGIONS: [&str; 3] = ["eu-west-3", "us-east-1", "eu-central-1"];

const SQL_TABLES: [&str; 8] = [
    "orders",
    "order_item",
    "users",
    "payments",
    "inventory",
    "audit_log",
    "sessions",
    "products",
];

const HTTP_ROUTES: [&str; 6] = [
    "http://user-svc:5000/api/users",
    "http://product-svc:5000/api/products",
    "http://stock-svc:5000/api/stock",
    "http://billing-svc:5000/api/invoices",
    "http://auth-svc:5000/api/tokens",
    "http://geo-svc:5000/api/locations",
];

const ENDPOINTS: [&str; 5] = [
    "POST /api/orders/{id}/submit",
    "GET /api/orders/{id}",
    "GET /api/users/{id}/profile",
    "POST /api/payments",
    "GET /api/catalog/search",
];

/// Generate the deterministic dataset described by `spec`.
#[must_use]
pub fn generate(spec: &SynthSpec) -> Vec<SpanEvent> {
    let mut rng = Rng::new(spec.seed);
    let services: Vec<Arc<str>> = (0..spec.services.max(1))
        .map(|i| Arc::from(format!("synth-svc-{i:04}")))
        .collect();
    let regions: Vec<Arc<str>> = REGIONS.iter().map(|r| Arc::from(*r)).collect();

    // Rough mean of the per-pattern event counts under the default mix,
    // good enough to avoid Vec regrowth.
    let mut events = Vec::with_capacity(spec.traces.saturating_mul(9));
    for trace_idx in 0..spec.traces {
        let svc_idx = rng.pick(services.len());
        let ctx = TraceCtx {
            trace_id: format!("synth-{}-{trace_idx}", spec.seed),
            service: Arc::clone(&services[svc_idx]),
            region: Arc::clone(&regions[svc_idx % regions.len()]),
            endpoint: ENDPOINTS[rng.pick(ENDPOINTS.len())],
            // Spread traces 15 ms apart so per-trace windows stay tight
            // while the dataset spans a realistic time range.
            base_ms: BASE_MS + (trace_idx as u64) * 15,
        };
        let draw = u32::try_from(rng.next_u64() % u64::from(spec.mix.total().max(1))).unwrap_or(0);
        let m = &spec.mix;
        if draw < m.n_plus_one {
            push_n_plus_one(&mut events, &ctx, &mut rng);
        } else if draw < m.n_plus_one + m.redundant {
            push_redundant(&mut events, &ctx, &mut rng);
        } else if draw < m.n_plus_one + m.redundant + m.chatty {
            push_chatty(&mut events, &ctx);
        } else if draw < m.n_plus_one + m.redundant + m.chatty + m.fanout {
            push_fanout(&mut events, &ctx, &mut rng);
        } else if draw < m.n_plus_one + m.redundant + m.chatty + m.fanout + m.slow {
            push_slow(&mut events, &ctx, &mut rng);
        } else {
            push_clean(&mut events, &ctx, spec.spans_per_trace, &mut rng);
        }
    }
    events
}

/// Generate at least `target_events` events by growing the trace count.
///
/// Used by `bench --synthetic` so callers think in event counts (the unit
/// of the published throughput numbers) rather than trace counts.
#[must_use]
pub fn generate_target_events(
    target_events: usize,
    services: usize,
    mix: &PatternMix,
    seed: u64,
) -> Vec<SpanEvent> {
    // Mean events per trace under the default mix is ~9; over-ask and
    // truncate so the output length is exact and deterministic.
    let spec = SynthSpec {
        services,
        traces: target_events / 6 + 1,
        spans_per_trace: 6,
        mix: mix.clone(),
        seed,
    };
    let mut events = generate(&spec);
    while events.len() < target_events {
        let extra = SynthSpec {
            traces: (target_events - events.len()) / 6 + 1,
            seed: seed.wrapping_add(events.len() as u64),
            ..spec.clone()
        };
        events.extend(generate(&extra));
    }
    events.truncate(target_events);
    events
}

/// Benchmark shim over the crate-private timestamp parser, so the
/// criterion suite can measure it without widening `time`'s visibility.
#[must_use]
pub fn parse_ts_ms(ts: &str) -> Option<u64> {
    crate::time::parse_iso8601_utc_to_ms(ts).ok()
}

/// Per-trace generation context shared by the pattern builders.
struct TraceCtx {
    trace_id: String,
    service: Arc<str>,
    region: Arc<str>,
    endpoint: &'static str,
    base_ms: u64,
}

impl TraceCtx {
    fn event(
        &self,
        idx: usize,
        offset_ms: u64,
        event_type: EventType,
        operation: &str,
        target: String,
        duration_us: u64,
    ) -> SpanEvent {
        SpanEvent {
            timestamp: millis_to_iso8601(self.base_ms + offset_ms),
            trace_id: self.trace_id.clone(),
            span_id: format!("{}-s{idx}", self.trace_id),
            parent_span_id: None,
            service: Arc::clone(&self.service),
            cloud_region: Some(Arc::clone(&self.region)),
            event_type,
            operation: operation.to_string(),
            target,
            duration_us,
            source: EventSource {
                endpoint: self.endpoint.to_string(),
                method: "Handler::handle".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
            instrumentation_scopes: Vec::new(),
        }
    }

    fn sql(&self, idx: usize, offset_ms: u64, target: String, duration_us: u64) -> SpanEvent {
        self.event(
            idx,
            offset_ms,
            EventType::Sql,
            "SELECT",
            target,
            duration_us,
        )
    }

    fn http(&self, idx: usize, offset_ms: u64, target: String, duration_us: u64) -> SpanEvent {
        let mut e = self.event(
            idx,
            offset_ms,
            EventType::HttpOut,
            "GET",
            target,
            duration_us,
        );
        e.status_code = Some(200);
        e.response_size_bytes = Some(2_048);
        e
    }
}

/// One lookup repeated with distinct literals: the classic N+1 loop.
fn push_n_plus_one(events: &mut Vec<SpanEvent>, ctx: &TraceCtx, rng: &mut Rng) {
    let table = SQL_TABLES[rng.pick(SQL_TABLES.len())];
    let count = 6 + rng.pick(4);
    for i in 0..count {
        let id = 1000 + rng.pick(9000);
        events.push(ctx.sql(
            i,
            (i as u64) * 5,
            format!("SELECT * FROM {table} WHERE parent_id = {id}"),
            700 + (rng.pick(400) as u64),
        ));
    }
}

/// The same query with the same literal repeated verbatim.
fn push_redundant(events: &mut Vec<SpanEvent>, ctx: &TraceCtx, rng: &mut Rng) {
    let table = SQL_TABLES[rng.pick(SQL_TABLES.len())];
    let id = 1000 + rng.pick(9000);
    let target = format!("SELECT * FROM {table} WHERE id = {id}");
    for i in 0..5 {
        events.push(ctx.sql(i, (i as u64) * 4, target.clone(), 600));
    }
}

/// Many outbound HTTP calls from one trace (chatty service).
fn push_chatty(events: &mut Vec<SpanEvent>, ctx: &TraceCtx) {
    for i in 0..16 {
        let route = HTTP_ROUTES[i % HTTP_ROUTES.len()];
        events.push(ctx.http(i, (i as u64) * 3, format!("{route}/{}", 100 + i), 8_000));
    }
}

/// One parent span with an excessive number of children.
fn push_fanout(events: &mut Vec<SpanEvent>, ctx: &TraceCtx, rng: &mut Rng) {
    let parent = ctx.http(0, 0, format!("{}/batch", HTTP_ROUTES[0]), 90_000);
    let parent_id = parent.span_id.clone();
    events.push(parent);
    let route = HTTP_ROUTES[rng.pick(HTTP_ROUTES.len())];
    for i in 1..=25 {
        let mut child = ctx.http(i, 1 + (i as u64), format!("{route}/{}", 200 + i), 6_000);
        child.parent_span_id = Some(parent_id.clone());
        events.push(child);
    }
}

/// A recurring query far above the slow threshold.
fn push_slow(events: &mut Vec<SpanEvent>, ctx: &TraceCtx, rng: &mut Rng) {
    let table = SQL_TABLES[rng.pick(SQL_TABLES.len())];
    for i in 0..3 {
        events.push(ctx.sql(
            i,
            (i as u64) * 10,
            format!("SELECT * FROM {table} ORDER BY created_at DESC LIMIT 50"),
            800_000 + (rng.pick(400_000) as u64),
        ));
    }
}

/// Varied, non-repeating I/O: no finding expected.
fn push_clean(events: &mut Vec<SpanEvent>, ctx: &TraceCtx, count: usize, rng: &mut Rng) {
    for i in 0..count.max(2) {
        let offset = (i as u64) * 7;
        if i % 3 == 2 {
            let route = HTTP_ROUTES[rng.pick(HTTP_ROUTES.len())];
            events.push(ctx.http(i, offset, format!("{route}/{}", 300 + i), 9_000));
        } else {
            let table = SQL_TABLES[(i + rng.pick(3)) % SQL_TABLES.len()];
            let id = 1000 + rng.pick(9000);
            events.push(ctx.sql(
                i,
                offset,
                format!("SELECT id, status FROM {table} WHERE id = {id} AND tenant = 'a{i}'"),
                900,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic() {
        let spec = SynthSpec::default();
        let a = generate(&spec);
        let b = generate(&spec);
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0].trace_id, b[0].trace_id);
        assert_eq!(a[a.len() - 1].target, b[b.len() - 1].target);
    }

    #[test]
    fn different_seeds_differ() {
        let a = generate(&SynthSpec::default());
        let b = generate(&SynthSpec {
            seed: 43,
            ..SynthSpec::default()
        });
        // Trace ids embed the seed, content draws differ.
        assert_ne!(a[0].trace_id, b[0].trace_id);
    }

    #[test]
    fn target_events_is_exact() {
        let events = generate_target_events(10_000, 16, &PatternMix::default(), 7);
        assert_eq!(events.len(), 10_000);
    }

    #[test]
    fn services_are_bounded_and_used() {
        let spec = SynthSpec {
            services: 4,
            traces: 200,
            ..SynthSpec::default()
        };
        let events = generate(&spec);
        let distinct: std::collections::HashSet<&str> =
            events.iter().map(|e| e.service.as_ref()).collect();
        assert!(distinct.len() <= 4);
        assert!(distinct.len() >= 2, "expected several services in use");
    }

    #[test]
    fn pipeline_detects_planted_patterns() {
        // End-to-end sanity: the default mix must produce findings of the
        // planted kinds when run through the real pipeline.
        let spec = SynthSpec {
            services: 4,
            traces: 300,
            ..SynthSpec::default()
        };
        let events = generate(&spec);
        let config = crate::config::Config::default();
        let report = crate::pipeline::analyze(events, &config);
        assert!(
            report.analysis.traces_analyzed >= 290,
            "traces should flow through, got {}",
            report.analysis.traces_analyzed
        );
        let types: std::collections::HashSet<&str> = report
            .findings
            .iter()
            .map(|f| f.finding_type.as_str())
            .collect();
        for expected in ["n_plus_one_sql", "redundant_sql", "slow_sql"] {
            assert!(
                types.contains(expected),
                "expected planted {expected} findings, got types {types:?}"
            );
        }
    }
}
