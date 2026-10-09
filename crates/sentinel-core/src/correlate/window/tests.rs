use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::*;
use crate::event::{EventSource, EventType, SpanEvent};
use crate::normalize;

fn make_event(trace_id: &str, target: &str) -> NormalizedEvent {
    let event = SpanEvent {
        timestamp: "2025-07-10T14:32:01.123Z".to_string(),
        trace_id: trace_id.to_string(),
        span_id: "span-1".to_string(),
        parent_span_id: None,
        link_trace_id: None,
        service: Arc::from("test"),
        grouping: Vec::new(),
        cloud_region: None,
        event_type: EventType::Sql,
        operation: "SELECT".to_string(),
        target: target.to_string(),
        duration_us: 100,
        source: EventSource {
            endpoint: "GET /test".to_string(),
            method: "Test::test".to_string(),
        },
        status_code: None,
        response_size_bytes: None,
        code_function: None,
        code_filepath: None,
        code_lineno: None,
        code_namespace: None,
        instrumentation_scopes: Vec::new(),
    };
    normalize::normalize(event)
}

fn make_child(
    trace_id: &str,
    service: &str,
    span_id: &str,
    parent_span_id: &str,
    target: &str,
    endpoint: &str,
) -> NormalizedEvent {
    let mut event = make_event(trace_id, target);
    event.event.service = Arc::from(service);
    event.event.span_id = span_id.to_string();
    event.event.parent_span_id = Some(parent_span_id.to_string());
    event.event.source.endpoint = endpoint.to_string();
    event
}

/// Endpoint attributed to the span carrying `target` in a snapshot.
/// Panics when the target is absent, which is a broken fixture.
fn endpoint_for<'a>(trace: &'a [NormalizedEvent], target: &str) -> &'a str {
    trace
        .iter()
        .find(|event| event.event.target == target)
        .expect("event present")
        .event
        .source
        .endpoint
        .as_str()
}

/// Two roots on `svc-a`, the shape the retain tests share.
fn two_root_groups() -> HashMap<Arc<str>, HashMap<String, String>> {
    HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([
            ("root-a".to_string(), "/api/a".to_string()),
            ("root-b".to_string(), "/api/b".to_string()),
        ]),
    )])
}

fn push_unknown_chain(
    window: &mut TraceWindow,
    prefix: &str,
    root_span_id: &str,
    intermediate_count: usize,
    leaf_target: &str,
) {
    let mut parent = root_span_id.to_string();
    for depth in 1..=intermediate_count {
        let span_id = format!("{prefix}-{depth}");
        window.push(
            make_child("t1", "svc-a", &span_id, &parent, &span_id, "unknown"),
            0,
        );
        parent = span_id;
    }
    window.push(
        make_child(
            "t1",
            "svc-a",
            &format!("{prefix}-leaf"),
            &parent,
            leaf_target,
            "unknown",
        ),
        0,
    );
}

#[test]
fn accumulates_events_by_trace() {
    let mut w = TraceWindow::new(WindowConfig::default());
    w.push(make_event("t1", "SELECT 1"), 0);
    w.push(make_event("t1", "SELECT 2"), 10);
    w.push(make_event("t2", "SELECT 3"), 20);

    assert_eq!(w.active_traces(), 2);
    let drained = w.drain_all();
    let t1 = drained.iter().find(|(id, _)| id == "t1").unwrap();
    assert_eq!(t1.1.len(), 2);
}

#[test]
fn ring_buffer_overflow() {
    let config = WindowConfig {
        max_events_per_trace: 3,
        ..Default::default()
    };
    let mut w = TraceWindow::new(config);
    for i in 0..5 {
        w.push(
            make_event("t1", &format!("SELECT {i}")),
            u64::try_from(i).unwrap(),
        );
    }

    let drained = w.drain_all();
    let t1 = drained.iter().find(|(id, _)| id == "t1").unwrap();
    assert_eq!(t1.1.len(), 3);
    // Should have the last 3 events (2, 3, 4)
    assert_eq!(t1.1[0].event.target, "SELECT 2");
    assert_eq!(t1.1[2].event.target, "SELECT 4");
}

#[test]
fn ttl_eviction() {
    let config = WindowConfig {
        trace_ttl_ms: 100,
        ..Default::default()
    };
    let mut w = TraceWindow::new(config);
    w.push(make_event("t1", "SELECT 1"), 0);
    w.push(make_event("t2", "SELECT 2"), 50);

    w.evict(150);
    // t1 last_seen=0, now=150, diff=150 > 100, so evicted
    // t2 last_seen=50, now=150, diff=100, so NOT evicted (100 <= 100)
    assert_eq!(w.active_traces(), 1);
    let drained = w.drain_all();
    assert_eq!(drained[0].0, "t2");
}

#[test]
fn lru_eviction() {
    let config = WindowConfig {
        max_active_traces: NonZeroUsize::new(2).unwrap(),
        ..Default::default()
    };
    let mut w = TraceWindow::new(config);
    w.push(make_event("t1", "SELECT 1"), 0);
    w.push(make_event("t2", "SELECT 2"), 10);
    // This should evict t1 (LRU: oldest access)
    let evicted = w.push(make_event("t3", "SELECT 3"), 20);

    assert!(evicted.is_some());
    assert_eq!(evicted.unwrap().0, "t1");
    assert_eq!(w.active_traces(), 2);
    assert!(w.traces.peek(&"t2".to_string()).is_some());
    assert!(w.traces.peek(&"t3".to_string()).is_some());
    assert!(w.traces.peek(&"t1".to_string()).is_none());
}

#[test]
fn drain_empties_window() {
    let mut w = TraceWindow::new(WindowConfig::default());
    w.push(make_event("t1", "SELECT 1"), 0);
    let drained = w.drain_all();
    assert_eq!(drained.len(), 1);
    assert_eq!(w.active_traces(), 0);
}

#[test]
fn lru_touch_prevents_eviction() {
    let config = WindowConfig {
        max_active_traces: NonZeroUsize::new(2).unwrap(),
        ..Default::default()
    };
    let mut w = TraceWindow::new(config);
    w.push(make_event("t1", "SELECT 1"), 0);
    w.push(make_event("t2", "SELECT 2"), 10);
    // Touch t1 so it becomes more recent than t2 (get_mut promotes to MRU)
    w.push(make_event("t1", "SELECT 1b"), 20);
    // Insert t3: should evict t2 (LRU), not t1 (MRU)
    let evicted = w.push(make_event("t3", "SELECT 3"), 30);

    assert!(evicted.is_some());
    assert_eq!(evicted.unwrap().0, "t2");
    assert_eq!(w.active_traces(), 2);
    assert!(w.traces.peek(&"t1".to_string()).is_some());
    assert!(w.traces.peek(&"t3".to_string()).is_some());
    assert!(w.traces.peek(&"t2".to_string()).is_none());
}

#[test]
fn evict_on_empty_window() {
    let mut w = TraceWindow::new(WindowConfig::default());
    w.evict(1000);
    assert_eq!(w.active_traces(), 0);
}

#[test]
fn ttl_evicts_all_expired() {
    let config = WindowConfig {
        trace_ttl_ms: 50,
        ..Default::default()
    };
    let mut w = TraceWindow::new(config);
    w.push(make_event("t1", "SELECT 1"), 0);
    w.push(make_event("t2", "SELECT 2"), 10);
    // Both expired at now=200
    w.evict(200);
    assert_eq!(w.active_traces(), 0);
}

#[test]
fn drain_empty_window() {
    let mut w = TraceWindow::new(WindowConfig::default());
    let drained = w.drain_all();
    assert_eq!(drained, []);
}

#[test]
fn lru_eviction_chain() {
    let config = WindowConfig {
        max_active_traces: NonZeroUsize::new(1).unwrap(),
        ..Default::default()
    };
    let mut w = TraceWindow::new(config);

    let evicted1 = w.push(make_event("t1", "SELECT 1"), 0);
    assert!(evicted1.is_none()); // first insert, no eviction

    let evicted2 = w.push(make_event("t2", "SELECT 2"), 10);
    // t1 evicted, only t2 remains
    assert!(evicted2.is_some());
    assert_eq!(evicted2.unwrap().0, "t1");
    assert_eq!(w.active_traces(), 1);
    assert!(w.traces.peek(&"t2".to_string()).is_some());

    let evicted3 = w.push(make_event("t3", "SELECT 3"), 20);
    // t2 evicted, only t3 remains
    assert!(evicted3.is_some());
    assert_eq!(evicted3.unwrap().0, "t2");
    assert_eq!(w.active_traces(), 1);
    assert!(w.traces.peek(&"t3".to_string()).is_some());
}

#[test]
fn evict_expired_returns_traces() {
    let config = WindowConfig {
        trace_ttl_ms: 100,
        ..Default::default()
    };
    let mut w = TraceWindow::new(config);
    w.push(make_event("t1", "SELECT 1"), 0);
    w.push(make_event("t2", "SELECT 2"), 50);

    // Not yet expired
    let expired = w.evict_expired(50);
    assert_eq!(expired, []);
    assert_eq!(w.active_traces(), 2);

    // t1 expired (150 - 0 = 150 > 100), t2 not (150 - 50 = 100 <= 100)
    let expired = w.evict_expired(150);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].0, "t1");
    assert_eq!(w.active_traces(), 1);
}

#[test]
fn push_returns_evicted_events() {
    let config = WindowConfig {
        max_active_traces: NonZeroUsize::new(1).unwrap(),
        ..Default::default()
    };
    let mut w = TraceWindow::new(config);
    w.push(make_event("t1", "SELECT 1"), 0);
    w.push(make_event("t1", "SELECT 2"), 5);

    let evicted = w.push(make_event("t2", "SELECT 3"), 10);
    assert!(evicted.is_some());
    let (id, events) = evicted.unwrap();
    assert_eq!(id, "t1");
    assert_eq!(events.len(), 2); // both events from t1
}

#[test]
fn reconciles_only_unknown_endpoints_in_the_same_trace_and_service() {
    let mut w = TraceWindow::new(WindowConfig::default());
    for (trace_id, service, endpoint, target) in [
        ("t1", "svc-a", "unknown", "SELECT 1"),
        ("t1", "svc-a", "  ", "SELECT 2"),
        ("t1", "svc-a", "/already-known", "SELECT 3"),
        ("t1", "svc-b", "unknown", "SELECT 4"),
        ("t2", "svc-a", "unknown", "SELECT 5"),
    ] {
        let mut event = make_event(trace_id, target);
        event.event.service = Arc::from(service);
        event.event.source.endpoint = endpoint.to_string();
        event.event.parent_span_id = Some("root-a".to_string());
        w.push(event, 0);
    }

    let root_endpoints = HashMap::from([(
        "root-a".to_string(),
        "/api/fault/slow-messaging".to_string(),
    )]);
    let service_root_endpoints = HashMap::from([(Arc::from("svc-a"), root_endpoints)]);
    assert_eq!(
        w.reconcile_source_endpoint_groups("t1", &service_root_endpoints),
        2
    );
    let t1 = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(endpoint_for(&t1, "SELECT 1"), "/api/fault/slow-messaging");
    assert_eq!(endpoint_for(&t1, "SELECT 2"), "/api/fault/slow-messaging");
    assert_eq!(endpoint_for(&t1, "SELECT 3"), "/already-known");
    assert_eq!(endpoint_for(&t1, "SELECT 4"), "unknown");
    assert_eq!(
        w.peek_clone("t2").expect("other trace remains")[0]
            .event
            .source
            .endpoint,
        "unknown"
    );
}

#[test]
fn consumer_context_resolves_only_what_nothing_else_does() {
    let svc = || Arc::<str>::from("svc-a");
    let context = |edges: &[(&str, Option<&str>)], consumers: &[(&str, &str)]| {
        let parents: SourceEndpointParentGroups = HashMap::from([(
            svc(),
            edges
                .iter()
                .map(|(span, parent)| (span.to_string(), parent.map(str::to_string)))
                .collect(),
        )]);
        let consumers: SourceEndpointGroups = HashMap::from([(
            svc(),
            consumers
                .iter()
                .map(|(span, endpoint)| (span.to_string(), endpoint.to_string()))
                .collect(),
        )]);
        (parents, consumers)
    };
    let mut w = TraceWindow::new(WindowConfig::default());

    // The consumer spans end after their children, so their context lands last.
    w.push(
        make_child("t1", "svc-a", "io-1", "listener", "unknown-io", "unknown"),
        0,
    );
    w.push(
        make_child(
            "t1",
            "svc-a",
            "io-2",
            "listener",
            "framed-io",
            "com.foo.DossierListener.onDossier",
        ),
        0,
    );
    let (parents, consumers) = context(
        &[("delivery", None), ("listener", Some("delivery"))],
        &[
            ("delivery", "rabbitmq orders.topic"),
            ("listener", "rabbitmq crm.dossiers"),
        ],
    );
    w.retain_source_endpoint_context_groups("t1", &HashMap::new(), &parents, &consumers, 0);
    let t1 = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(endpoint_for(&t1, "unknown-io"), "rabbitmq crm.dossiers");
    assert_eq!(
        endpoint_for(&t1, "framed-io"),
        "com.foo.DossierListener.onDossier"
    );

    // A route above the consumer outranks it, even where the conversion
    // already wrote the destination.
    w.push(
        make_child("t2", "svc-a", "io-3", "consumer", "late-io", "unknown"),
        0,
    );
    w.push(
        make_child(
            "t2",
            "svc-a",
            "io-4",
            "consumer",
            "provisional-io",
            "rabbitmq crm.dossiers",
        ),
        0,
    );
    let (parents, consumers) = context(
        &[("route", None), ("consumer", Some("route"))],
        &[("consumer", "rabbitmq crm.dossiers")],
    );
    let roots = HashMap::from([(
        svc(),
        HashMap::from([("route".to_string(), "/api/orders".to_string())]),
    )]);
    w.retain_source_endpoint_context_groups("t2", &roots, &parents, &consumers, 0);
    let t2 = w.peek_clone("t2").expect("trace remains active");
    assert_eq!(endpoint_for(&t2, "late-io"), "/api/orders");
    assert_eq!(endpoint_for(&t2, "provisional-io"), "/api/orders");
}

#[test]
fn consumer_destination_never_outranks_a_route_below_it() {
    // A listener that calls its own API: CONSUMER C -> HTTP client H (an
    // I/O event under C) -> SERVER X (route) -> SQL E, plus SQL spans
    // directly under C. H and the direct children take the destination,
    // E keeps the route, and neither the destination cached on H nor the
    // route found from E may leak onto the other branch.
    let svc = Arc::<str>::from("svc-a");
    let mut w = TraceWindow::new(WindowConfig::default());
    w.push(
        make_child(
            "t1",
            "svc-a",
            "http-out",
            "consumer",
            "self-call",
            "unknown",
        ),
        0,
    );
    w.push(
        make_child(
            "t1",
            "svc-a",
            "sql-early",
            "consumer",
            "SELECT 1",
            "unknown",
        ),
        0,
    );
    w.push(
        make_child("t1", "svc-a", "sql-routed", "route", "SELECT 2", "/api/x"),
        0,
    );
    let parents = consumer_http_route_parents(Arc::clone(&svc));
    let roots = HashMap::from([(
        Arc::clone(&svc),
        HashMap::from([("route".to_string(), "/api/x".to_string())]),
    )]);
    let consumers: SourceEndpointGroups = HashMap::from([(
        Arc::clone(&svc),
        HashMap::from([("consumer".to_string(), "rabbitmq crm.orders".to_string())]),
    )]);
    w.retain_source_endpoint_context_groups("t1", &roots, &parents, &consumers, 0);
    // A sibling pushed after the walk from E cached its path.
    w.push(
        make_child("t1", "svc-a", "sql-late", "consumer", "SELECT 3", "unknown"),
        1,
    );
    let t1 = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(endpoint_for(&t1, "self-call"), "rabbitmq crm.orders");
    assert_eq!(endpoint_for(&t1, "SELECT 1"), "rabbitmq crm.orders");
    assert_eq!(endpoint_for(&t1, "SELECT 2"), "/api/x");
    assert_eq!(endpoint_for(&t1, "SELECT 3"), "rabbitmq crm.orders");
}

#[test]
fn unproven_endpoint_never_outranks_a_nearer_route() {
    // A listener calling its own API through a framed self-call: the
    // frame the converter spelled on the client span is cached unproven
    // and names nothing under the SERVER span's own route.
    let svc = Arc::<str>::from("svc-a");
    let mut w = TraceWindow::new(WindowConfig::default());
    let roots = HashMap::from([(
        Arc::clone(&svc),
        HashMap::from([("route".to_string(), "/api/x".to_string())]),
    )]);
    let parents: SourceEndpointParentGroups = HashMap::from([(
        svc,
        HashMap::from([
            ("listener".to_string(), None),
            ("http-out".to_string(), Some("listener".to_string())),
            ("route".to_string(), Some("http-out".to_string())),
        ]),
    )]);
    w.retain_source_endpoint_context_groups("t1", &roots, &parents, &HashMap::new(), 0);
    w.push(
        make_child(
            "t1",
            "svc-a",
            "http-out",
            "listener",
            "self-call",
            "com.foo.DossierListener.onDossier",
        ),
        0,
    );
    w.push(
        make_child("t1", "svc-a", "sql-routed", "route", "SELECT 2", "/api/x"),
        0,
    );
    let t1 = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(
        endpoint_for(&t1, "self-call"),
        "com.foo.DossierListener.onDossier"
    );
    assert_eq!(endpoint_for(&t1, "SELECT 2"), "/api/x");
}

#[test]
fn unretained_consumer_destination_never_outranks_a_nearer_route() {
    // The consumer cap was reached before the listener's span arrived:
    // its self-call still carries the destination the converter spelled,
    // unproven, so the route of the SERVER span below it wins.
    let svc = || Arc::<str>::from("svc-a");
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 2,
        ..WindowConfig::default()
    });
    let fillers: SourceEndpointGroups = HashMap::from([(
        svc(),
        HashMap::from([
            ("c-a".to_string(), "rabbitmq a".to_string()),
            ("c-b".to_string(), "rabbitmq b".to_string()),
        ]),
    )]);
    w.retain_source_endpoint_context_groups("t1", &HashMap::new(), &HashMap::new(), &fillers, 0);
    let roots = HashMap::from([(
        svc(),
        HashMap::from([("route".to_string(), "/api/x".to_string())]),
    )]);
    let parents = consumer_http_route_parents(svc());
    let consumers: SourceEndpointGroups = HashMap::from([(
        svc(),
        HashMap::from([("consumer".to_string(), "rabbitmq crm.orders".to_string())]),
    )]);
    w.retain_source_endpoint_context_groups("t1", &roots, &parents, &consumers, 0);
    w.push(
        make_child(
            "t1",
            "svc-a",
            "http-out",
            "consumer",
            "self-call",
            "rabbitmq crm.orders",
        ),
        0,
    );
    w.push(
        make_child("t1", "svc-a", "sql-routed", "route", "SELECT 2", "/api/x"),
        0,
    );
    let t1 = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(endpoint_for(&t1, "self-call"), "rabbitmq crm.orders");
    assert_eq!(endpoint_for(&t1, "SELECT 2"), "/api/x");
}

#[test]
fn kept_endpoint_is_cached_at_its_own_depth() {
    // A client span keeping its frame under a far consumer: its cached
    // depth counts from itself, not from the destination it did not
    // adopt, so a child under a route-less SERVER span still inherits it.
    let mut links: HashMap<String, Option<String>> = (1..=7)
        .map(|index| {
            let parent = if index == 7 {
                "consumer".to_string()
            } else {
                format!("p{}", index + 1)
            };
            (format!("p{index}"), Some(parent))
        })
        .collect();
    links.insert("consumer".to_string(), None);
    links.insert("http-out".to_string(), Some("p1".to_string()));
    links.insert("x".to_string(), Some("http-out".to_string()));
    let svc = || Arc::<str>::from("svc-a");
    let parents: SourceEndpointParentGroups = HashMap::from([(svc(), links)]);
    let consumers: SourceEndpointGroups = HashMap::from([(
        svc(),
        HashMap::from([("consumer".to_string(), "rabbitmq orders".to_string())]),
    )]);
    let mut w = TraceWindow::new(WindowConfig::default());
    w.retain_source_endpoint_context_groups("t1", &HashMap::new(), &parents, &consumers, 0);
    w.push(
        make_child(
            "t1",
            "svc-a",
            "http-out",
            "p1",
            "self-call",
            "com.foo.Listener.on",
        ),
        0,
    );
    w.push(
        make_child("t1", "svc-a", "sql", "x", "SELECT 1", "unknown"),
        0,
    );
    let t1 = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(endpoint_for(&t1, "self-call"), "com.foo.Listener.on");
    assert_eq!(endpoint_for(&t1, "SELECT 1"), "com.foo.Listener.on");
}

#[test]
fn consumer_link_outlives_the_ancestry_lru() {
    // Two SQL spans under one listener that sits under a route: the
    // second arrives after other spans' links evicted the listener's
    // ancestry entry, and follows the retained link to the same route.
    let svc = || Arc::<str>::from("svc-a");
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 4,
        ..WindowConfig::default()
    });
    let roots = HashMap::from([(
        svc(),
        HashMap::from([("route".to_string(), "/api/r".to_string())]),
    )]);
    let parents: SourceEndpointParentGroups = HashMap::from([(
        svc(),
        HashMap::from([
            ("route".to_string(), None),
            ("consumer".to_string(), Some("route".to_string())),
        ]),
    )]);
    let consumers: SourceEndpointGroups = HashMap::from([(
        svc(),
        HashMap::from([("consumer".to_string(), "rabbitmq x".to_string())]),
    )]);
    w.retain_source_endpoint_context_groups("t1", &roots, &parents, &consumers, 0);
    w.push(
        make_child("t1", "svc-a", "sql-1", "consumer", "SELECT 1", "unknown"),
        0,
    );
    let fillers: SourceEndpointParentGroups = HashMap::from([(
        Arc::from("svc-b"),
        HashMap::from([
            ("b1".to_string(), None),
            ("b2".to_string(), None),
            ("b3".to_string(), None),
        ]),
    )]);
    w.retain_source_endpoint_context_groups("t1", &HashMap::new(), &fillers, &HashMap::new(), 0);
    w.push(
        make_child("t1", "svc-a", "sql-2", "consumer", "SELECT 2", "unknown"),
        0,
    );
    let t1 = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(endpoint_for(&t1, "SELECT 1"), "/api/r");
    assert_eq!(endpoint_for(&t1, "SELECT 2"), "/api/r");
}

#[test]
fn root_first_context_resolves_the_event_with_the_same_span_id() {
    let mut w = TraceWindow::new(WindowConfig::default());
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/orders".to_string())]),
    )]);
    assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
    let mut root = make_event("t1", "http://orders-svc/api/orders");
    root.event.service = Arc::from("svc-a");
    root.event.span_id = "root".to_string();
    root.event.source.endpoint = "unknown".to_string();
    assert!(w.push(root, 1).is_none());

    assert_eq!(
        w.peek_clone("t1").expect("trace remains active")[0]
            .event
            .source
            .endpoint,
        "/api/orders"
    );
    assert_eq!(
        w.drain_all().pop().expect("one finished trace").1[0]
            .event
            .source
            .endpoint,
        "/api/orders"
    );
}

#[test]
fn detached_reconciliation_resolves_the_event_with_the_root_span_id() {
    let mut root = make_event("t1", "http://orders-svc/api/orders");
    root.event.service = Arc::from("svc-a");
    root.event.span_id = "root".to_string();
    root.event.source.endpoint = "unknown".to_string();
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/orders".to_string())]),
    )]);

    let context = SourceContext {
        roots: &roots,
        parents: &HashMap::new(),
        consumers: &HashMap::new(),
    };
    assert_eq!(
        reconcile_event_source_endpoint_groups(std::slice::from_mut(&mut root), context),
        1
    );
    assert_eq!(root.event.source.endpoint, "/api/orders");
}

#[test]
fn detached_reconciliation_falls_back_to_the_nearest_consumer() {
    // A consumer-only service: the in-slice chain reaches the
    // destination where no route answers, and a route on it still wins.
    let svc = || Arc::<str>::from("svc-a");
    let mut events = vec![
        make_child("t1", "svc-a", "p", "top", "SELECT 1", "unknown"),
        make_child("t1", "svc-a", "e", "p", "SELECT 2", "unknown"),
    ];
    let empty = HashMap::new();
    let no_parents = HashMap::new();
    let consumers: SourceEndpointGroups = HashMap::from([(
        svc(),
        HashMap::from([("top".to_string(), "rabbitmq orders".to_string())]),
    )]);
    let context = SourceContext {
        roots: &empty,
        parents: &no_parents,
        consumers: &consumers,
    };
    assert_eq!(
        reconcile_event_source_endpoint_groups(&mut events, context),
        2
    );
    assert_eq!(endpoint_for(&events, "SELECT 2"), "rabbitmq orders");

    for event in &mut events {
        event.event.source.endpoint = "unknown".to_string();
    }
    let roots = HashMap::from([(
        svc(),
        HashMap::from([("top".to_string(), "/api/x".to_string())]),
    )]);
    let consumers: SourceEndpointGroups = HashMap::from([(
        svc(),
        HashMap::from([("p".to_string(), "rabbitmq orders".to_string())]),
    )]);
    let context = SourceContext {
        roots: &roots,
        parents: &no_parents,
        consumers: &consumers,
    };
    reconcile_event_source_endpoint_groups(&mut events, context);
    assert_eq!(endpoint_for(&events, "SELECT 2"), "/api/x");
}

#[test]
fn root_aware_reconciliation_is_order_independent_and_requires_a_known_parent_chain() {
    for updates in [
        [("root-a", "/api/a"), ("root-b", "/api/b")],
        [("root-b", "/api/b"), ("root-a", "/api/a")],
    ] {
        let mut w = TraceWindow::new(WindowConfig::default());
        for event in [
            make_child("t1", "svc-a", "a-direct", "root-a", "a-direct", "unknown"),
            make_child("t1", "svc-a", "a-mid", "root-a", "a-mid", "unknown"),
            make_child("t1", "svc-a", "a-leaf", "a-mid", "a-leaf", "unknown"),
            make_child("t1", "svc-a", "b-direct", "root-b", "b-direct", "unknown"),
            make_child(
                "t1",
                "svc-a",
                "missing-chain",
                "filtered-middle",
                "missing-chain",
                "unknown",
            ),
            make_child("t1", "svc-a", "known", "root-a", "known", "/already-known"),
            make_child(
                "t1",
                "svc-b",
                "other-service",
                "root-a",
                "other-service",
                "unknown",
            ),
        ] {
            w.push(event, 0);
        }

        let root_endpoints = updates
            .into_iter()
            .map(|(root_span_id, endpoint)| (root_span_id.to_string(), endpoint.to_string()))
            .collect();
        let service_root_endpoints = HashMap::from([(Arc::from("svc-a"), root_endpoints)]);
        w.reconcile_source_endpoint_groups("t1", &service_root_endpoints);

        let trace = w.peek_clone("t1").expect("trace remains active");
        assert_eq!(endpoint_for(&trace, "a-direct"), "/api/a");
        assert_eq!(endpoint_for(&trace, "a-mid"), "/api/a");
        assert_eq!(endpoint_for(&trace, "a-leaf"), "/api/a");
        assert_eq!(endpoint_for(&trace, "b-direct"), "/api/b");
        assert_eq!(endpoint_for(&trace, "missing-chain"), "unknown");
        assert_eq!(endpoint_for(&trace, "known"), "/already-known");
        assert_eq!(endpoint_for(&trace, "other-service"), "unknown");
    }
}

#[test]
fn grouped_reconciliation_bounds_depth_and_cycles() {
    let mut w = TraceWindow::new(WindowConfig::default());
    push_unknown_chain(&mut w, "within", "root-at-limit", 7, "within-leaf");
    push_unknown_chain(&mut w, "deep", "root-too-deep", 8, "deep-leaf");
    for index in 0..9 {
        let span_id = format!("cycle-{index}");
        let parent_span_id = format!("cycle-{}", (index + 1) % 9);
        w.push(
            make_child(
                "t1",
                "svc-a",
                &span_id,
                &parent_span_id,
                &span_id,
                "unknown",
            ),
            0,
        );
    }

    let root_endpoints = HashMap::from([
        ("root-at-limit".to_string(), "/at-limit".to_string()),
        ("root-too-deep".to_string(), "/too-deep".to_string()),
    ]);
    let service_root_endpoints = HashMap::from([(Arc::from("svc-a"), root_endpoints)]);
    w.reconcile_source_endpoint_groups("t1", &service_root_endpoints);

    let trace = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(endpoint_for(&trace, "within-leaf"), "/at-limit");
    assert_eq!(endpoint_for(&trace, "deep-leaf"), "unknown");
    for index in 0..9 {
        assert_eq!(endpoint_for(&trace, &format!("cycle-{index}")), "unknown");
    }
}

#[test]
fn compressed_ancestry_keeps_every_hop_beyond_the_depth_limit_unknown() {
    let mut w = TraceWindow::new(WindowConfig::default());
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/orders".to_string())]),
    )]);
    assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());

    let mut parent = "root".to_string();
    for depth in 1..=ANCESTOR_WALK_MAX_DEPTH {
        let span_id = format!("within-{depth}");
        w.push(
            make_child("t1", "svc-a", &span_id, &parent, &span_id, "unknown"),
            depth as u64,
        );
        parent = span_id;
    }
    w.push(make_child("t1", "svc-a", "a", "b", "a", "unknown"), 9);
    w.push(make_child("t1", "svc-a", "b", &parent, "b", "unknown"), 10);
    w.push(make_child("t1", "svc-a", "c", "a", "c", "unknown"), 11);
    w.push(make_child("t1", "svc-a", "d", "b", "d", "unknown"), 12);

    let trace = w.peek_clone("t1").expect("trace remains active");
    for span_id in ["a", "b", "c", "d"] {
        assert_eq!(
            trace
                .iter()
                .find(|event| event.event.span_id == span_id)
                .expect("event retained")
                .event
                .source
                .endpoint,
            "unknown",
            "{span_id} is beyond the ancestor walk limit"
        );
    }
}

#[test]
fn grouped_reconciliation_stays_bounded_under_many_unmatched_roots() {
    const ADVERSARIAL_SIZE: usize = 500;

    let mut w = TraceWindow::new(WindowConfig::default());
    push_unknown_chain(
        &mut w,
        "adversarial",
        "missing-parent",
        ADVERSARIAL_SIZE - 1,
        "adversarial-leaf",
    );
    let root_endpoints = (0..ADVERSARIAL_SIZE)
        .map(|index| (format!("absent-root-{index}"), format!("/root/{index}")))
        .collect();
    let service_root_endpoints = HashMap::from([(Arc::from("svc-a"), root_endpoints)]);

    let started = Instant::now();
    let updated = w.reconcile_source_endpoint_groups("t1", &service_root_endpoints);

    assert_eq!(updated, 0);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "group reconciliation exceeded the bounded-work budget"
    );
    assert!(
        w.peek_clone("t1")
            .expect("trace remains active")
            .iter()
            .all(|event| event.event.source.endpoint == "unknown")
    );
}

#[test]
fn trace_group_reconciliation_keys_parents_and_roots_by_service() {
    let mut w = TraceWindow::new(WindowConfig::default());
    for event in [
        make_child(
            "t1",
            "svc-a",
            "shared-child",
            "shared-root",
            "a-child",
            "unknown",
        ),
        make_child(
            "t1",
            "svc-b",
            "shared-child",
            "shared-root",
            "b-child",
            "unknown",
        ),
        make_child(
            "t1",
            "svc-b",
            "known-child",
            "shared-root",
            "known-child",
            "/already-known",
        ),
    ] {
        w.push(event, 0);
    }
    let service_root_endpoints = HashMap::from([
        (
            Arc::from("svc-a"),
            HashMap::from([("shared-root".to_string(), "/api/a".to_string())]),
        ),
        (
            Arc::from("svc-b"),
            HashMap::from([("shared-root".to_string(), "/api/b".to_string())]),
        ),
    ]);

    assert_eq!(
        w.reconcile_source_endpoint_groups("t1", &service_root_endpoints),
        2
    );
    let trace = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(endpoint_for(&trace, "a-child"), "/api/a");
    assert_eq!(endpoint_for(&trace, "b-child"), "/api/b");
    assert_eq!(endpoint_for(&trace, "known-child"), "/already-known");
}

#[test]
fn trace_group_reconciliation_stays_bounded_across_many_services() {
    const SERVICE_COUNT: usize = 2_000;
    const EVENTS_PER_SERVICE: usize = 25;
    const EVENT_COUNT: usize = SERVICE_COUNT * EVENTS_PER_SERVICE;

    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: EVENT_COUNT,
        ..WindowConfig::default()
    });
    let mut service_root_endpoints = HashMap::with_capacity(SERVICE_COUNT);
    for service_index in 0..SERVICE_COUNT {
        let service = format!("service-{service_index}");
        service_root_endpoints.insert(
            Arc::from(service.as_str()),
            HashMap::from([(
                format!("absent-root-{service_index}"),
                format!("/api/{service_index}"),
            )]),
        );
        for event_index in 0..EVENTS_PER_SERVICE {
            w.push(
                make_child(
                    "t1",
                    &service,
                    &format!("span-{service_index}-{event_index}"),
                    &format!("missing-parent-{service_index}-{event_index}"),
                    &format!("target-{service_index}-{event_index}"),
                    "unknown",
                ),
                0,
            );
        }
    }

    let started = Instant::now();
    let updated = w.reconcile_source_endpoint_groups("t1", &service_root_endpoints);

    assert_eq!(updated, 0);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "per-trace reconciliation exceeded the bounded-work budget"
    );
}

#[test]
fn late_endpoint_does_not_resurrect_an_evicted_trace() {
    let mut w = TraceWindow::new(WindowConfig {
        max_active_traces: NonZeroUsize::new(1).expect("nonzero"),
        ..WindowConfig::default()
    });
    w.push(make_event("evicted", "SELECT 1"), 0);
    w.push(make_event("active", "SELECT 2"), 1);

    let root_endpoints = HashMap::from([("root".to_string(), "/api/late".to_string())]);
    let service_root_endpoints = HashMap::from([(Arc::from("test"), root_endpoints)]);
    assert_eq!(
        w.reconcile_source_endpoint_groups("evicted", &service_root_endpoints),
        0
    );
    assert_eq!(w.active_traces(), 1);
    assert!(w.peek_clone("evicted").is_none());
    assert!(w.peek_clone("active").is_some());
}

#[test]
fn early_root_context_reconciles_later_multi_service_events_and_drains_io() {
    let mut w = TraceWindow::new(WindowConfig::default());
    let source_endpoint_groups = HashMap::from([
        (
            Arc::from("svc-a"),
            HashMap::from([
                ("root-a".to_string(), "/api/a".to_string()),
                ("root-a2".to_string(), "/api/a2".to_string()),
            ]),
        ),
        (
            Arc::from("svc-b"),
            HashMap::from([("root-b".to_string(), "/api/b".to_string())]),
        ),
    ]);

    assert!(
        w.retain_source_endpoint_groups("t1", &source_endpoint_groups, 10)
            .is_none()
    );
    assert_eq!(w.active_traces(), 1);
    assert_eq!(w.peek_clone("t1").expect("context retained"), []);
    assert!(
        w.push(make_child("t1", "svc-a", "a", "a-mid", "a", "unknown"), 20,)
            .is_none()
    );
    assert!(
        w.push(
            make_child("t1", "svc-a", "a-mid", "root-a", "a-mid", "unknown"),
            20,
        )
        .is_none()
    );
    assert!(
        w.push(
            make_child("t1", "svc-a", "a2", "root-a2", "a2", "unknown"),
            20,
        )
        .is_none()
    );
    assert!(
        w.push(make_child("t1", "svc-b", "b", "root-b", "b", "unknown"), 20,)
            .is_none()
    );

    let mut drained = w.drain_all();
    assert_eq!(drained.len(), 1, "only non-empty traces are drained");
    let (_, trace) = drained.pop().expect("trace finished");
    assert_eq!(trace[0].event.source.endpoint, "/api/a");
    assert_eq!(trace[1].event.source.endpoint, "/api/a");
    assert_eq!(trace[2].event.source.endpoint, "/api/a2");
    assert_eq!(trace[3].event.source.endpoint, "/api/b");
    assert_eq!(w.active_traces(), 0);
}

#[test]
fn early_root_context_obeys_lru_without_resurrection_or_empty_eviction() {
    let mut w = TraceWindow::new(WindowConfig {
        max_active_traces: NonZeroUsize::new(2).expect("nonzero"),
        ..WindowConfig::default()
    });
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/root".to_string())]),
    )]);

    assert!(w.retain_source_endpoint_groups("a", &roots, 0).is_none());
    assert!(w.retain_source_endpoint_groups("b", &roots, 1).is_none());
    assert!(
        w.retain_source_endpoint_groups("a", &roots, 2).is_none(),
        "updating context neither evicts nor promotes"
    );
    assert!(
        w.retain_source_endpoint_groups("c", &roots, 3).is_none(),
        "evicting empty context produces no detection batch"
    );
    assert!(w.peek_clone("a").is_none(), "LRU context was evicted");
    assert!(w.peek_clone("b").is_some());
    assert!(w.peek_clone("c").is_some());

    assert!(
        w.push(
            make_child("a", "svc-a", "late", "root", "late", "unknown"),
            4,
        )
        .is_none()
    );
    assert_eq!(
        w.peek_clone("a").expect("new event trace retained")[0]
            .event
            .source
            .endpoint,
        "unknown",
        "evicted context must not resurrect"
    );
}

#[test]
fn early_root_context_expires_without_an_empty_detection_batch() {
    let mut w = TraceWindow::new(WindowConfig {
        trace_ttl_ms: 100,
        ..WindowConfig::default()
    });
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/root".to_string())]),
    )]);

    assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
    assert_eq!(w.evict_expired(101), []);
    assert_eq!(w.active_traces(), 0);
    w.push(
        make_child("t1", "svc-a", "late", "root", "late", "unknown"),
        102,
    );
    assert_eq!(
        w.peek_clone("t1").expect("new event trace retained")[0]
            .event
            .source
            .endpoint,
        "unknown"
    );
}

#[test]
fn resolved_ancestry_survives_ring_rotation_for_shared_children() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 3,
        ..WindowConfig::default()
    });
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/orders".to_string())]),
    )]);
    assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
    w.push(
        make_child("t1", "svc-a", "intermediate", "root", "orders", "unknown"),
        1,
    );
    for index in 0..10 {
        w.push(
            make_child(
                "t1",
                "svc-a",
                &format!("child-{index}"),
                "intermediate",
                "orders",
                "unknown",
            ),
            index + 2,
        );
    }

    let preview = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(preview.len(), 3, "ring retains only the newest children");
    assert!(
        preview
            .iter()
            .all(|event| event.event.source.endpoint == "/api/orders")
    );

    let (trace_id, spans) = w.drain_all().pop().expect("one finished trace");
    let findings =
        crate::detect::slow::detect_slow(&crate::correlate::Trace { trace_id, spans }, 0, 3);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].source_endpoint, "/api/orders");
}

#[test]
fn capacity_one_proven_consumer_outranks_the_guessed_sole_root() {
    // A handler root and a listener root in one trace, the usual Java agent
    // shape where the CONSUMER span is a root of its own. The sole retained
    // root is the handler, the SQL sits under the consumer, and the guess
    // must not override what the chain proves. When the listener instead
    // sits under the handler's PRODUCER span, cap 1 cannot walk that far
    // and reports the destination where a larger cap reports the route.
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let svc = || Arc::<str>::from("svc-a");
    let roots = HashMap::from([(
        svc(),
        HashMap::from([("handler".to_string(), "/api/publish".to_string())]),
    )]);
    let parents: SourceEndpointParentGroups = HashMap::from([(
        svc(),
        HashMap::from([
            ("handler".to_string(), None),
            ("consumer".to_string(), None),
        ]),
    )]);
    let consumers: SourceEndpointGroups = HashMap::from([(
        svc(),
        HashMap::from([("consumer".to_string(), "rabbitmq crm.orders".to_string())]),
    )]);
    assert!(
        w.retain_source_endpoint_context_groups("t1", &roots, &parents, &consumers, 0)
            .is_none()
    );
    w.push(
        make_child("t1", "svc-a", "sql", "consumer", "SELECT 1", "unknown"),
        1,
    );

    let preview = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(preview[0].event.source.endpoint, "rabbitmq crm.orders");
    let (_, events) = w.drain_all().pop().expect("one finished trace");
    assert_eq!(events[0].event.source.endpoint, "rabbitmq crm.orders");
}

#[test]
fn capacity_one_retained_consumer_blocks_the_sole_root_guess() {
    // The listener's SQL reaches it through a span nothing retained: at
    // cap 1 the chain breaks there, and the retained destination, a
    // second entry point, keeps the handler's route from being guessed.
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let svc = || Arc::<str>::from("svc-a");
    let roots = HashMap::from([(
        svc(),
        HashMap::from([("handler".to_string(), "/api/publish".to_string())]),
    )]);
    let parents: SourceEndpointParentGroups = HashMap::from([(
        svc(),
        HashMap::from([
            ("handler".to_string(), None),
            ("consumer".to_string(), None),
        ]),
    )]);
    let consumers: SourceEndpointGroups = HashMap::from([(
        svc(),
        HashMap::from([("consumer".to_string(), "rabbitmq crm.orders".to_string())]),
    )]);
    w.retain_source_endpoint_context_groups("t1", &roots, &parents, &consumers, 0);
    w.push(
        make_child("t1", "svc-a", "sql", "tx", "SELECT 1", "unknown"),
        1,
    );

    let preview = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(preview[0].event.source.endpoint, "unknown");
    let (_, events) = w.drain_all().pop().expect("one finished trace");
    assert_eq!(events[0].event.source.endpoint, "unknown");
}

#[test]
fn capacity_one_ancestry_keeps_the_referenced_parent_for_siblings() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/orders".to_string())]),
    )]);
    assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
    w.push(
        make_child("t1", "svc-a", "parent", "root", "parent", "unknown"),
        1,
    );
    w.push(
        make_child("t1", "svc-a", "child-1", "parent", "child-1", "unknown"),
        2,
    );
    w.push(
        make_child("t1", "svc-a", "child-2", "parent", "child-2", "unknown"),
        3,
    );

    let preview = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(preview.len(), 1);
    assert_eq!(preview[0].event.span_id, "child-2");
    assert_eq!(preview[0].event.source.endpoint, "/api/orders");

    let (_, events) = w.drain_all().pop().expect("one finished trace");
    assert_eq!(events[0].event.source.endpoint, "/api/orders");
}

#[test]
fn capacity_one_never_falls_back_across_truncated_same_service_roots() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let first_root = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root-a".to_string(), "/api/a".to_string())]),
    )]);
    let second_root = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root-b".to_string(), "/api/b".to_string())]),
    )]);
    assert!(
        w.retain_source_endpoint_groups("t1", &first_root, 0)
            .is_none()
    );
    assert!(
        w.retain_source_endpoint_groups("t1", &second_root, 1)
            .is_none()
    );
    w.push(
        make_child("t1", "svc-a", "parent", "root-b", "parent", "unknown"),
        2,
    );
    w.push(
        make_child("t1", "svc-a", "child", "parent", "child", "unknown"),
        3,
    );

    let preview = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(preview.len(), 1);
    assert_eq!(preview[0].event.source.endpoint, "unknown");

    let (trace_id, spans) = w.drain_all().pop().expect("one finished trace");
    assert_eq!(spans[0].event.source.endpoint, "unknown");
    let findings =
        crate::detect::slow::detect_slow(&crate::correlate::Trace { trace_id, spans }, 0, 1);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].source_endpoint, "unknown");
}

#[test]
fn late_second_root_retracts_capacity_one_preview_and_finished_fallback() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let first_root = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root-a".to_string(), "/api/a".to_string())]),
    )]);
    assert!(
        w.retain_source_endpoint_groups("t1", &first_root, 0)
            .is_none()
    );
    w.push(
        make_child("t1", "svc-a", "parent", "missing", "parent", "unknown"),
        1,
    );
    w.push(
        make_child("t1", "svc-a", "child", "parent", "child", "unknown"),
        2,
    );
    assert_eq!(
        w.peek_clone("t1").expect("trace remains active")[0]
            .event
            .source
            .endpoint,
        "/api/a",
        "one retained root permits a provisional preview fallback"
    );

    let second_root = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root-b".to_string(), "/api/b".to_string())]),
    )]);
    assert!(
        w.retain_source_endpoint_groups("t1", &second_root, 3)
            .is_none()
    );
    assert_eq!(
        w.peek_clone("t1").expect("trace remains active")[0]
            .event
            .source
            .endpoint,
        "unknown"
    );
    let (trace_id, spans) = w.drain_all().pop().expect("one finished trace");
    assert_eq!(spans[0].event.source.endpoint, "unknown");
    let findings =
        crate::detect::slow::detect_slow(&crate::correlate::Trace { trace_id, spans }, 0, 1);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].source_endpoint, "unknown");
}

#[test]
fn explicit_reconciliation_records_new_root_ambiguity_before_resolution() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let first_root = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root-a".to_string(), "/api/a".to_string())]),
    )]);
    let second_root = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root-b".to_string(), "/api/b".to_string())]),
    )]);
    assert!(
        w.retain_source_endpoint_groups("t1", &first_root, 0)
            .is_none()
    );
    assert_eq!(w.reconcile_source_endpoint_groups("t1", &second_root), 0);
    assert!(
        w.traces
            .peek("t1")
            .expect("trace remains active")
            .ambiguous_source_endpoint_services
            .contains("svc-a")
    );

    w.push(
        make_child("t1", "svc-a", "parent", "root-b", "parent", "unknown"),
        1,
    );
    w.push(
        make_child("t1", "svc-a", "child", "parent", "child", "unknown"),
        2,
    );
    assert_eq!(
        w.peek_clone("t1").expect("trace remains active")[0]
            .event
            .source
            .endpoint,
        "unknown"
    );
}

#[test]
fn duplicate_root_update_keeps_capacity_one_fallback_unambiguous() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let first_root = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/old".to_string())]),
    )]);
    let updated_root = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/current".to_string())]),
    )]);
    assert!(
        w.retain_source_endpoint_groups("t1", &first_root, 0)
            .is_none()
    );
    assert!(
        w.retain_source_endpoint_groups("t1", &updated_root, 1)
            .is_none()
    );
    assert!(
        w.traces
            .peek("t1")
            .expect("trace remains active")
            .ambiguous_source_endpoint_services
            .is_empty(),
        "updating the same root must not make its service ambiguous"
    );

    w.push(
        make_child("t1", "svc-a", "parent", "root", "parent", "unknown"),
        2,
    );
    w.push(
        make_child("t1", "svc-a", "child-1", "parent", "child-1", "unknown"),
        3,
    );
    w.push(
        make_child("t1", "svc-a", "child-2", "parent", "child-2", "unknown"),
        4,
    );

    let preview = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(preview[0].event.span_id, "child-2");
    assert_eq!(preview[0].event.source.endpoint, "/api/current");
}

#[test]
fn same_batch_truncated_root_marks_only_its_retained_service_ambiguous() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let roots = two_root_groups();
    assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
    let retained_root = w
        .traces
        .peek("t1")
        .expect("trace remains active")
        .source_endpoint_groups["svc-a"]
        .keys()
        .next()
        .expect("one root retained")
        .clone();
    let dropped_root = if retained_root == "root-a" {
        "root-b"
    } else {
        "root-a"
    };
    assert!(
        w.traces
            .peek("t1")
            .expect("trace remains active")
            .ambiguous_source_endpoint_services
            .contains("svc-a")
    );

    w.push(
        make_child("t1", "svc-a", "parent", dropped_root, "parent", "unknown"),
        1,
    );
    w.push(
        make_child("t1", "svc-a", "child", "parent", "child", "unknown"),
        2,
    );
    assert_eq!(
        w.peek_clone("t1").expect("trace remains active")[0]
            .event
            .source
            .endpoint,
        "unknown"
    );
}

#[test]
fn root_parent_context_shares_the_endpoint_cap() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([
            ("nested".to_string(), "/api/nested".to_string()),
            ("outer".to_string(), "/api/outer".to_string()),
        ]),
    )]);
    let parents = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([
            ("nested".to_string(), Some("outer".to_string())),
            ("outer".to_string(), None),
        ]),
    )]);

    assert!(
        w.retain_source_endpoint_context_groups("t1", &roots, &parents, &HashMap::new(), 0)
            .is_none()
    );
    let buffer = w.traces.peek("t1").expect("trace remains active");
    let retained_roots = &buffer.source_endpoint_groups["svc-a"];
    let retained_parents = &buffer.source_endpoint_parent_groups["svc-a"];
    assert_eq!(retained_roots.len(), 1);
    assert_eq!(retained_parents.len(), 1);
    assert!(
        retained_parents
            .keys()
            .all(|root_span_id| retained_roots.contains_key(root_span_id))
    );
}

/// Every incoming map draws fresh hash keys, so roots admitted in its
/// iteration order would differ across these runs.
#[test]
fn the_endpoint_cap_admits_roots_in_span_id_order() {
    for _ in 0..32 {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 2,
            ..WindowConfig::default()
        });
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            (1..=6)
                .map(|i| (format!("r{i}"), format!("/r{i}")))
                .collect::<HashMap<_, _>>(),
        )]);
        assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
        let buffer = w.traces.peek("t1").expect("trace remains active");
        let mut retained: Vec<_> = buffer.source_endpoint_groups["svc-a"]
            .keys()
            .map(String::as_str)
            .collect();
        retained.sort_unstable();
        assert_eq!(retained, ["r1", "r2"]);
    }
}

#[test]
fn parent_only_context_uses_the_existing_ancestry_cap() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let parents = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([
            ("internal-a".to_string(), Some("outer".to_string())),
            ("internal-b".to_string(), Some("outer".to_string())),
        ]),
    )]);

    assert!(
        w.retain_source_endpoint_context_groups(
            "t1",
            &HashMap::new(),
            &parents,
            &HashMap::new(),
            0
        )
        .is_none()
    );
    let buffer = w.traces.peek("t1").expect("context-only trace retained");
    assert_eq!(buffer.source_endpoint_count, 0);
    assert_eq!(
        buffer
            .resolved_ancestry
            .as_ref()
            .expect("positive cap creates ancestry cache")
            .len(),
        1
    );
}

#[test]
fn root_parent_cycle_stays_within_the_ancestor_bound() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 2,
        ..WindowConfig::default()
    });
    let roots = two_root_groups();
    let parents = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([
            ("root-a".to_string(), Some("root-b".to_string())),
            ("root-b".to_string(), Some("root-a".to_string())),
        ]),
    )]);
    w.retain_source_endpoint_context_groups("t1", &roots, &parents, &HashMap::new(), 0);
    w.push(
        make_child("t1", "svc-a", "sql", "root-a", "SELECT 1", "unknown"),
        1,
    );

    let endpoint = &w.peek_clone("t1").expect("trace remains active")[0]
        .event
        .source
        .endpoint;
    assert!(endpoint == "/api/a" || endpoint == "/api/b");
}

#[test]
fn rejected_other_service_does_not_consume_ambiguity_state() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 1,
        ..WindowConfig::default()
    });
    let first_root = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root-a".to_string(), "/api/a".to_string())]),
    )]);
    assert!(
        w.retain_source_endpoint_groups("t1", &first_root, 0)
            .is_none()
    );
    for index in 0..100 {
        let rejected_root = HashMap::from([(
            Arc::from(format!("svc-rejected-{index}")),
            HashMap::from([(format!("root-{index}"), format!("/api/{index}"))]),
        )]);
        assert!(
            w.retain_source_endpoint_groups("t1", &rejected_root, index + 1)
                .is_none()
        );
    }
    let buffer = w.traces.peek("t1").expect("trace remains active");
    assert!(buffer.ambiguous_source_endpoint_services.is_empty());
    assert_eq!(buffer.source_endpoint_groups.len(), 1);

    w.push(
        make_child(
            "t1",
            "svc-rejected-0",
            "rejected-parent",
            "root-0",
            "rejected-parent",
            "unknown",
        ),
        101,
    );
    w.push(
        make_child(
            "t1",
            "svc-rejected-0",
            "rejected-child",
            "rejected-parent",
            "rejected-child",
            "unknown",
        ),
        102,
    );
    assert_eq!(
        w.peek_clone("t1").expect("trace remains active")[0]
            .event
            .source
            .endpoint,
        "unknown",
        "a rejected service must not inherit the retained service's root"
    );

    w.push(
        make_child("t1", "svc-a", "parent", "root-a", "parent", "unknown"),
        103,
    );
    w.push(
        make_child("t1", "svc-a", "child-1", "parent", "child-1", "unknown"),
        104,
    );
    w.push(
        make_child("t1", "svc-a", "child-2", "parent", "child-2", "unknown"),
        105,
    );
    assert_eq!(
        w.peek_clone("t1").expect("trace remains active")[0]
            .event
            .source
            .endpoint,
        "/api/a"
    );
}

#[test]
fn resolved_ancestry_survives_out_of_order_parent_rotation() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 2,
        ..WindowConfig::default()
    });
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/orders".to_string())]),
    )]);
    assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
    w.push(make_child("t1", "svc-a", "a", "b", "a", "unknown"), 1);
    w.push(make_child("t1", "svc-a", "b", "root", "b", "unknown"), 2);
    w.push(make_child("t1", "svc-a", "c", "a", "c", "unknown"), 3);

    let preview = w.peek_clone("t1").expect("trace remains active");
    let c = preview
        .iter()
        .find(|event| event.event.span_id == "c")
        .expect("newest child remains in the ring");
    assert_eq!(c.event.source.endpoint, "/api/orders");

    let (_, events) = w.drain_all().pop().expect("one finished trace");
    let c = events
        .iter()
        .find(|event| event.event.span_id == "c")
        .expect("newest child is drained");
    assert_eq!(c.event.source.endpoint, "/api/orders");
}

#[test]
fn ancestry_cache_does_not_preallocate_the_per_trace_limit() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 100_000,
        ..WindowConfig::default()
    });
    w.push(make_event("t1", "SELECT 1"), 0);

    let buffer = w.traces.peek("t1").expect("trace remains active");
    let ancestry = buffer
        .resolved_ancestry
        .as_ref()
        .expect("positive cap enables ancestry retention");
    assert_eq!(ancestry.cap(), NonZeroUsize::MAX);
    assert_eq!(ancestry.len(), 1);
}

#[test]
fn resolved_ancestry_isolated_by_service_when_span_ids_collide() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 4,
        ..WindowConfig::default()
    });
    let roots = HashMap::from([
        (
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/a".to_string())]),
        ),
        (
            Arc::from("svc-b"),
            HashMap::from([("root".to_string(), "/api/b".to_string())]),
        ),
    ]);
    assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
    w.push(
        make_child("t1", "svc-a", "shared", "root", "a-parent", "unknown"),
        1,
    );
    w.push(
        make_child("t1", "svc-b", "shared", "root", "b-parent", "unknown"),
        1,
    );
    w.push(
        make_child("t1", "svc-a", "a-child", "shared", "a-child", "unknown"),
        2,
    );
    w.push(
        make_child("t1", "svc-b", "b-child", "shared", "b-child", "unknown"),
        2,
    );

    let preview = w.peek_clone("t1").expect("trace remains active");
    assert_eq!(endpoint_for(&preview, "a-child"), "/api/a");
    assert_eq!(endpoint_for(&preview, "b-child"), "/api/b");
}

#[test]
fn root_update_feeds_resolved_ancestry_before_parent_rotation() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 3,
        ..WindowConfig::default()
    });
    w.push(
        make_child("t1", "svc-a", "intermediate", "root", "orders", "unknown"),
        0,
    );
    w.push(
        make_child("t1", "svc-a", "early", "intermediate", "orders", "unknown"),
        1,
    );
    let roots = HashMap::from([(
        Arc::from("svc-a"),
        HashMap::from([("root".to_string(), "/api/orders".to_string())]),
    )]);
    assert!(w.retain_source_endpoint_groups("t1", &roots, 2).is_none());
    for index in 0..10 {
        w.push(
            make_child(
                "t1",
                "svc-a",
                &format!("late-{index}"),
                "intermediate",
                "orders",
                "unknown",
            ),
            index + 3,
        );
    }

    assert!(
        w.peek_clone("t1")
            .expect("trace remains active")
            .iter()
            .all(|event| event.event.source.endpoint == "/api/orders")
    );
}

#[test]
fn early_root_storm_is_bounded_by_trace_and_per_trace_caps() {
    let mut w = TraceWindow::new(WindowConfig {
        max_events_per_trace: 3,
        max_active_traces: NonZeroUsize::new(2).expect("nonzero"),
        ..WindowConfig::default()
    });
    for index in 0..100 {
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([(format!("root-{index}"), format!("/api/{index}"))]),
        )]);
        assert!(
            w.retain_source_endpoint_groups(&format!("trace-{index}"), &roots, index)
                .is_none()
        );
    }
    let many_roots = HashMap::from([(
        Arc::from("svc-a"),
        (0..100)
            .map(|index| (format!("extra-root-{index}"), format!("/extra/{index}")))
            .collect(),
    )]);
    assert!(
        w.retain_source_endpoint_groups("trace-99", &many_roots, 100)
            .is_none()
    );

    assert_eq!(w.active_traces(), 2);
    assert!(
        w.traces
            .iter()
            .all(|(_, buffer)| buffer.source_endpoint_count <= 3)
    );
    assert!(w.traces.iter().all(|(_, buffer)| {
        buffer
            .resolved_ancestry
            .as_ref()
            .is_none_or(|ancestry| ancestry.len() <= 3)
    }));
    assert!(w.traces.iter().all(|(_, buffer)| {
        buffer.ambiguous_source_endpoint_services.len() <= buffer.source_endpoint_groups.len()
            && buffer
                .ambiguous_source_endpoint_services
                .iter()
                .all(|service| buffer.source_endpoint_groups.contains_key(service))
    }));
    assert!(w.drain_all().is_empty(), "context-only drain stays empty");
    assert_eq!(w.active_traces(), 0);
}

/// Parent chain `route -> http-out -> consumer` of one service: a route
/// reached through an outbound HTTP span under a consumer.
fn consumer_http_route_parents(service: Arc<str>) -> SourceEndpointParentGroups {
    HashMap::from([(
        service,
        HashMap::from([
            ("consumer".to_string(), None),
            ("http-out".to_string(), Some("consumer".to_string())),
            ("route".to_string(), Some("http-out".to_string())),
        ]),
    )])
}
