use super::*;
use crate::event::EventType;

fn sample_jaeger_json() -> &'static str {
    r#"{
            "data": [{
                "traceID": "abc123",
                "spans": [
                    {
                        "spanID": "span-1",
                        "operationName": "OrderService::create_order",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 1200,
                        "processID": "p1",
                        "tags": [
                            { "key": "db.statement", "value": "SELECT * FROM order_item WHERE order_id = 42" },
                            { "key": "db.system", "value": "postgresql" }
                        ]
                    },
                    {
                        "spanID": "span-2",
                        "operationName": "http-call",
                        "references": [{ "refType": "CHILD_OF", "spanID": "span-1" }],
                        "startTime": 1720621921200000,
                        "duration": 15000,
                        "processID": "p1",
                        "tags": [
                            { "key": "http.url", "value": "http://user-svc:5000/api/users/123" },
                            { "key": "http.method", "value": "GET" },
                            { "key": "http.status_code", "value": "200" }
                        ]
                    },
                    {
                        "spanID": "span-3",
                        "operationName": "internal-op",
                        "references": [],
                        "startTime": 1720621921300000,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "internal.type", "value": "processing" }
                        ]
                    }
                ],
                "processes": {
                    "p1": { "serviceName": "order-svc" }
                }
            }]
        }"#
}

#[test]
fn namespaces_are_extracted_from_process_tags() {
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "query",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 1200,
                    "processID": "p1",
                    "tags": [{ "key": "db.statement", "value": "SELECT 1" }]
                }],
                "processes": { "p1": {
                    "serviceName": "svc",
                    "tags": [
                        { "key": "service.namespace", "value": "payments" },
                        { "key": "k8s.namespace.name", "value": "prod-eu" }
                    ]
                }}
            }]
        }"#;

    let events = JaegerIngest::new(64 * 1024)
        .ingest(json.as_bytes())
        .unwrap();

    let captured = crate::test_helpers::grouping_pairs(&events[0].grouping);
    assert_eq!(
        captured,
        vec![
            ("k8s.namespace.name", "prod-eu"),
            ("service.namespace", "payments"),
        ],
        "both values are kept, config order, Kubernetes first"
    );
    assert_eq!(events[0].grouping_value(), Some("prod-eu"));
}

#[test]
fn parses_jaeger_export() {
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(sample_jaeger_json().as_bytes()).unwrap();
    assert_eq!(events.len(), 2, "non-IO span should be skipped");
}

#[test]
fn non_sql_datastore_span_is_dropped() {
    // A Redis span carries a db.statement that is not relational SQL.
    // It must be dropped, never tokenized as SQL.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "s1", "operationName": "redis-get",
                        "references": [], "startTime": 1, "duration": 10, "processID": "p1",
                        "tags": [
                            { "key": "db.system", "value": "redis" },
                            { "key": "db.statement", "value": "GET user:123" }
                        ]
                    },
                    {
                        "spanID": "s2", "operationName": "sql",
                        "references": [], "startTime": 1, "duration": 10, "processID": "p1",
                        "tags": [
                            { "key": "db.system", "value": "postgresql" },
                            { "key": "db.statement", "value": "SELECT 1" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(events[0].operation, "postgresql");
}

#[test]
fn db_system_alias_is_canonicalized() {
    // A Jaeger trace tagging db.system="postgres" must label the operation
    // "postgresql", same as the OTLP and Zipkin paths.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1", "operationName": "q",
                    "startTime": 0, "duration": 100, "processID": "p1",
                    "tags": [
                        { "key": "db.system", "value": "postgres" },
                        { "key": "db.statement", "value": "SELECT 1" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(events[0].operation, "postgresql");
}

#[test]
fn stable_db_system_name_non_sql_is_dropped() {
    // A non-SQL store reported only under the stable db.system.name key
    // ("aws.dynamodb") must be dropped, not tokenized as SQL: its statement
    // can carry a key/document value.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1", "operationName": "q",
                    "startTime": 0, "duration": 100, "processID": "p1",
                    "tags": [
                        { "key": "db.system.name", "value": "aws.dynamodb" },
                        { "key": "db.statement", "value": "SELECT * FROM Orders WHERE Id = 'secret'" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events, []);
}

#[test]
fn sql_span_maps_correctly() {
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(sample_jaeger_json().as_bytes()).unwrap();
    let sql = events
        .iter()
        .find(|e| e.event_type == EventType::Sql)
        .unwrap();

    assert_eq!(sql.trace_id, "abc123");
    assert_eq!(sql.span_id, "span-1");
    assert_eq!(&*sql.service, "order-svc");
    assert_eq!(sql.operation, "postgresql");
    assert_eq!(sql.target, "SELECT * FROM order_item WHERE order_id = 42");
    assert_eq!(sql.duration_us, 1200);
    assert!(sql.parent_span_id.is_none());
    assert!(sql.status_code.is_none());
    assert_eq!(sql.timestamp, "2024-07-10T14:32:01.123Z");
}

#[test]
fn http_span_maps_correctly() {
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(sample_jaeger_json().as_bytes()).unwrap();
    let http = events
        .iter()
        .find(|e| e.event_type == EventType::HttpOut)
        .unwrap();

    assert_eq!(http.trace_id, "abc123");
    assert_eq!(http.span_id, "span-2");
    assert_eq!(http.operation, "GET");
    assert_eq!(http.target, "http://user-svc:5000/api/users/123");
    assert_eq!(http.duration_us, 15000);
    assert_eq!(http.status_code, Some(200));
    assert_eq!(http.parent_span_id.as_deref(), Some("span-1"));
}

#[test]
fn rejects_oversized_payload() {
    let ingest = JaegerIngest::new(10);
    let result = ingest.ingest(sample_jaeger_json().as_bytes());
    assert!(result.is_err());
}

#[test]
fn malformed_json_missing_data_key() {
    let json = r#"{"traces": []}"#;
    let ingest = JaegerIngest::new(1_048_576);
    assert!(ingest.ingest(json.as_bytes()).is_err());
}

#[test]
fn malformed_json_missing_trace_id() {
    let json = r#"{"data": [{"spans": [], "processes": {}}]}"#;
    let ingest = JaegerIngest::new(1_048_576);
    assert!(ingest.ingest(json.as_bytes()).is_err());
}

#[test]
fn malformed_json_missing_spans() {
    let json = r#"{"data": [{"traceID": "t1", "processes": {}}]}"#;
    let ingest = JaegerIngest::new(1_048_576);
    assert!(ingest.ingest(json.as_bytes()).is_err());
}

#[test]
fn malformed_json_missing_span_id() {
    let json = r#"{"data": [{"traceID": "t1", "spans": [{"operationName": "op", "startTime": 0, "duration": 0, "processID": "p1", "tags": []}], "processes": {"p1": {"serviceName": "svc"}}}]}"#;
    let ingest = JaegerIngest::new(1_048_576);
    assert!(ingest.ingest(json.as_bytes()).is_err());
}

#[test]
fn empty_data_array_produces_no_events() {
    let json = r#"{"data": []}"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events, []);
}

#[test]
fn empty_spans_array_produces_no_events() {
    let json = r#"{"data": [{"traceID": "t1", "spans": [], "processes": {"p1": {"serviceName": "svc"}}}]}"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events, []);
}

#[test]
fn unknown_process_id_falls_back_to_unknown_service() {
    let json = r#"{"data": [{"traceID": "t1", "spans": [{"spanID": "s1", "operationName": "op", "startTime": 0, "duration": 100, "processID": "unknown", "tags": [{"key": "db.statement", "value": "SELECT 1"}]}], "processes": {"p1": {"serviceName": "svc"}}}]}"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(&*events[0].service, crate::event::UNKNOWN_SERVICE);
}

#[test]
fn numeric_tag_value_converted_to_string() {
    let json = r#"{"data": [{"traceID": "t1", "spans": [{"spanID": "s1", "operationName": "op", "startTime": 0, "duration": 100, "processID": "p1", "tags": [{"key": "http.url", "value": "http://svc/api"}, {"key": "http.status_code", "value": 200}]}], "processes": {"p1": {"serviceName": "svc"}}}]}"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].status_code, Some(200));
}

#[test]
fn slashless_http_route_is_canonicalized_before_http_target() {
    // Jaeger reads endpoint tags from the current span (not the parent
    // like OTLP). When both http.route and http.target are present,
    // route must win so the ack signature stays stable.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "query",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" },
                        { "key": "http.route", "value": "api/orders/{id}" },
                        { "key": "http.target", "value": "/api/orders/42" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source.endpoint, "/api/orders/{id}");
}

#[test]
fn named_route_uses_url_path_for_own_and_ancestor_endpoints() {
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "root",
                        "operationName": "request",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 5000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "server" },
                            { "key": "http.route", "value": "app_fault_nplusonesql" },
                            { "key": "url.path", "value": "/api/fault/n-plus-one-sql" }
                        ]
                    },
                    {
                        "spanID": "child",
                        "operationName": "query",
                        "references": [{ "refType": "CHILD_OF", "spanID": "root" }],
                        "startTime": 1720621921123100,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "db.statement", "value": "SELECT 1" },
                            { "key": "db.system", "value": "postgresql" }
                        ]
                    },
                    {
                        "traceID": "t2",
                        "spanID": "own",
                        "operationName": "query",
                        "references": [],
                        "startTime": 1720621921123200,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "server" },
                            { "key": "db.statement", "value": "SELECT 2" },
                            { "key": "db.system", "value": "postgresql" },
                            { "key": "http.route", "value": "app_fault_nplusonesql" },
                            { "key": "url.path", "value": "/api/fault/n-plus-one-sql" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "symfony-svc" } }
            }]
        }"#;

    let events = JaegerIngest::new(1_048_576)
        .ingest(json.as_bytes())
        .unwrap();

    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| event.source.endpoint == "/api/fault/n-plus-one-sql")
    );
}

#[test]
fn server_route_and_legacy_url_is_context_not_http_out() {
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "POST /api/orders/{id}",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "span.kind", "value": "server" },
                        { "key": "http.route", "value": "api/orders/{id}" },
                        { "key": "http.url", "value": "http://order-svc/api/orders/42" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;

    let events = JaegerIngest::new(1_048_576)
        .ingest(json.as_bytes())
        .unwrap();

    assert_eq!(events, []);
}

#[test]
fn server_url_full_without_route_is_context_not_http_out() {
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "POST /api/orders/42",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "span.kind", "value": "server" },
                        { "key": "url.full", "value": "http://order-svc/api/orders/42" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;

    let events = JaegerIngest::new(1_048_576)
        .ingest(json.as_bytes())
        .unwrap();

    assert_eq!(events, []);
}

#[test]
fn unspecified_outgoing_url_uses_its_parent_server_route() {
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "s1",
                        "operationName": "POST /api/orders",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 5000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "server" },
                            { "key": "http.route", "value": "api/orders" }
                        ]
                    },
                    {
                        "spanID": "s2",
                        "operationName": "GET",
                        "references": [{ "refType": "CHILD_OF", "spanID": "s1" }],
                        "startTime": 1720621921123200,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "http.url", "value": "https://partner.example/v1/pay" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;

    let events = JaegerIngest::new(1_048_576)
        .ingest(json.as_bytes())
        .unwrap();
    let outgoing = events
        .iter()
        .find(|event| event.event_type == EventType::HttpOut)
        .expect("outgoing event present");

    assert_eq!(outgoing.source.endpoint, "/api/orders");
}

#[test]
fn unspecified_root_url_does_not_self_source() {
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "GET",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "http.url", "value": "https://partner.example/v1/pay" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;

    let events = JaegerIngest::new(1_048_576)
        .ingest(json.as_bytes())
        .unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::HttpOut);
    assert_eq!(events[0].source.endpoint, "unknown");
}

#[test]
fn http_target_used_only_when_route_absent() {
    // Documented Jaeger fallback: instrumentation that omits
    // http.route falls back to http.target. The endpoint string is
    // less stable but still useful.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "query",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" },
                        { "key": "http.target", "value": "/api/orders/42" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source.endpoint, "/api/orders/42");
}

#[test]
fn code_frame_used_when_no_http_tag() {
    // Non-HTTP entry point: without the code frame the endpoint would be
    // empty, which names no origin and collides in the ack signature.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "query",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" },
                        { "key": "code.function", "value": "execute" },
                        { "key": "code.namespace", "value": "com.foo.PurgeJob" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source.endpoint, "com.foo.PurgeJob.execute");
}

#[test]
fn endpoint_falls_back_to_consumer_destination() {
    // A boolean tag keeps its JSON type on Jaeger.
    let trace = |trace_id: &str, destination_tags: &str| {
        format!(
            r#"{{
                    "traceID": "{trace_id}",
                    "spans": [
                        {{
                            "spanID": "c1",
                            "operationName": "process",
                            "references": [],
                            "startTime": 1720621921123000,
                            "duration": 5000,
                            "processID": "p1",
                            "tags": [
                                {{ "key": "span.kind", "value": "consumer" }},
                                {{ "key": "messaging.system", "value": "rabbitmq" }},
                                {destination_tags}
                            ]
                        }},
                        {{
                            "spanID": "q1",
                            "operationName": "query",
                            "references": [{{ "refType": "CHILD_OF", "spanID": "c1" }}],
                            "startTime": 1720621921123100,
                            "duration": 500,
                            "processID": "p1",
                            "tags": [
                                {{ "key": "db.statement", "value": "SELECT 1" }},
                                {{ "key": "db.system", "value": "postgresql" }}
                            ]
                        }}
                    ],
                    "processes": {{ "p1": {{ "serviceName": "web-crm" }} }}
                }}"#
        )
    };
    let json = format!(
        r#"{{ "data": [{}, {}] }}"#,
        trace(
            "named",
            r#"{ "key": "messaging.destination.name", "value": "crm.dossiers" }"#
        ),
        trace(
            "anonymous",
            r#"{ "key": "messaging.destination.name", "value": "crm.dossiers" },
                   { "key": "messaging.destination.anonymous", "value": true }"#
        ),
    );
    let events = JaegerIngest::new(1_048_576)
        .ingest(json.as_bytes())
        .unwrap();
    let endpoint_for = |trace_id: &str| {
        events
            .iter()
            .find(|e| e.trace_id == trace_id && e.event_type == EventType::Sql)
            .expect("sql leaf present")
            .source
            .endpoint
            .clone()
    };
    assert_eq!(endpoint_for("named"), "rabbitmq crm.dossiers");
    assert_eq!(endpoint_for("anonymous"), "unknown");
}

#[test]
fn endpoint_resolves_through_ancestors() {
    // The Spring shape the lab measured 0/43 on: the route sits on the
    // SERVER span two levels above the JDBC leaf, and the intermediate
    // CLIENT span's URL must not win over it.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "s1",
                        "operationName": "POST /api/orders",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 5000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "server" },
                            { "key": "http.route", "value": "api/orders" }
                        ]
                    },
                    {
                        "spanID": "s2",
                        "operationName": "GET",
                        "references": [{ "refType": "CHILD_OF", "spanID": "s1" }],
                        "startTime": 1720621921123100,
                        "duration": 3000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "client" },
                            { "key": "http.url", "value": "https://partner.example/v1/pay" },
                            { "key": "url.path", "value": "/v1/pay" }
                        ]
                    },
                    {
                        "spanID": "s3",
                        "operationName": "query",
                        "references": [{ "refType": "CHILD_OF", "spanID": "s2" }],
                        "startTime": 1720621921123200,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "db.statement", "value": "SELECT 1" },
                            { "key": "db.system", "value": "postgresql" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    let outbound = events
        .iter()
        .find(|event| event.event_type == EventType::HttpOut)
        .expect("client outbound event present");
    assert_eq!(outbound.target, "https://partner.example/v1/pay");
    let sql = events
        .iter()
        .find(|e| e.event_type == EventType::Sql)
        .expect("sql leaf present");
    assert_eq!(sql.source.endpoint, "/api/orders");
}

#[test]
#[allow(clippy::too_many_lines)] // One fixture proves both sides of the service boundary.
fn outermost_route_stops_at_the_service_boundary() {
    let json = r#"{
            "data": [
                {
                    "traceID": "same-service",
                    "spans": [
                        {
                            "spanID": "outer",
                            "operationName": "POST /api/fault/pool-saturation",
                            "references": [],
                            "startTime": 1720621921123000,
                            "duration": 5000,
                            "processID": "laravel",
                            "tags": [
                                { "key": "span.kind", "value": "server" },
                                { "key": "http.route", "value": "/api/fault/pool-saturation" }
                            ]
                        },
                        {
                            "spanID": "nested",
                            "operationName": "GET /api/payments/history",
                            "references": [{ "refType": "CHILD_OF", "spanID": "outer" }],
                            "startTime": 1720621921123100,
                            "duration": 3000,
                            "processID": "laravel",
                            "tags": [
                                { "key": "span.kind", "value": "server" },
                                { "key": "http.route", "value": "/api/payments/history" }
                            ]
                        },
                        {
                            "spanID": "sql",
                            "operationName": "query",
                            "references": [{ "refType": "CHILD_OF", "spanID": "nested" }],
                            "startTime": 1720621921123200,
                            "duration": 500,
                            "processID": "laravel",
                            "tags": [
                                { "key": "db.statement", "value": "SELECT * FROM payments" },
                                { "key": "db.system", "value": "postgresql" }
                            ]
                        }
                    ],
                    "processes": { "laravel": { "serviceName": "laravel-svc" } }
                },
                {
                    "traceID": "cross-service",
                    "spans": [
                        {
                            "spanID": "caller",
                            "operationName": "POST /api/orders",
                            "references": [],
                            "startTime": 1720621921123000,
                            "duration": 5000,
                            "processID": "orders",
                            "tags": [
                                { "key": "span.kind", "value": "server" },
                                { "key": "http.route", "value": "/api/orders" }
                            ]
                        },
                        {
                            "spanID": "callee",
                            "operationName": "GET /api/payments/history",
                            "references": [{ "refType": "CHILD_OF", "spanID": "caller" }],
                            "startTime": 1720621921123100,
                            "duration": 3000,
                            "processID": "payments",
                            "tags": [
                                { "key": "span.kind", "value": "server" },
                                { "key": "http.route", "value": "/api/payments/history" }
                            ]
                        },
                        {
                            "spanID": "sql",
                            "operationName": "query",
                            "references": [{ "refType": "CHILD_OF", "spanID": "callee" }],
                            "startTime": 1720621921123200,
                            "duration": 500,
                            "processID": "payments",
                            "tags": [
                                { "key": "db.statement", "value": "SELECT * FROM payments" },
                                { "key": "db.system", "value": "postgresql" }
                            ]
                        }
                    ],
                    "processes": {
                        "orders": { "serviceName": "orders-svc" },
                        "payments": { "serviceName": "payments-svc" }
                    }
                }
            ]
        }"#;

    let events = JaegerIngest::new(1_048_576)
        .ingest(json.as_bytes())
        .unwrap();
    let same_service = events
        .iter()
        .find(|event| event.trace_id == "same-service")
        .expect("same-service SQL event present");
    let cross_service = events
        .iter()
        .find(|event| event.trace_id == "cross-service")
        .expect("cross-service SQL event present");

    assert_eq!(same_service.source.endpoint, "/api/fault/pool-saturation");
    assert_eq!(cross_service.source.endpoint, "/api/payments/history");
    assert_eq!(&*cross_service.service, "payments-svc");
}

#[test]
fn route_at_hop_nine_is_outside_the_shared_depth_limit() {
    let mut spans = vec![serde_json::json!({
        "spanID": "p9", "operationName": "server", "references": [],
        "startTime": 1, "duration": 1, "processID": "svc",
        "tags": [{ "key": "http.route", "value": "/too-deep" }]
    })];
    for id in (1_u8..9).rev() {
        spans.push(serde_json::json!({
            "spanID": format!("p{id}"), "operationName": "internal",
            "references": [{ "refType": "CHILD_OF", "spanID": format!("p{}", id + 1) }],
            "startTime": 1, "duration": 1, "processID": "svc",
            "tags": if id == 8 {
                serde_json::json!([{ "key": "http.route", "value": "/at-limit" }])
            } else {
                serde_json::json!([])
            }
        }));
    }
    spans.push(serde_json::json!({
        "spanID": "sql", "operationName": "query",
        "references": [{ "refType": "CHILD_OF", "spanID": "p1" }],
        "startTime": 1, "duration": 1, "processID": "svc",
        "tags": [
            { "key": "db.statement", "value": "SELECT 1" },
            { "key": "db.system", "value": "postgresql" }
        ]
    }));
    let payload = serde_json::json!({
        "data": [{
            "traceID": "trace", "spans": spans,
            "processes": { "svc": { "serviceName": "svc" } }
        }]
    })
    .to_string();

    let events = JaegerIngest::new(1_048_576)
        .ingest(payload.as_bytes())
        .unwrap();
    assert_eq!(events[0].source.endpoint, "/at-limit");
}

#[test]
fn walk_accepts_http_target_on_an_ancestor() {
    // An SDK older than semconv 1.23 records http.target and no
    // http.route. The leaf check accepts it and the walk must too.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "s1",
                        "operationName": "POST /api/orders",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 5000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "server" },
                            { "key": "http.target", "value": "/api/orders/42" }
                        ]
                    },
                    {
                        "spanID": "s2",
                        "operationName": "query",
                        "references": [{ "refType": "CHILD_OF", "spanID": "s1" }],
                        "startTime": 1720621921123200,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "db.statement", "value": "SELECT 1" },
                            { "key": "db.system", "value": "postgresql" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    let sql = events
        .iter()
        .find(|e| e.event_type == EventType::Sql)
        .expect("sql leaf present");
    assert_eq!(sql.source.endpoint, "/api/orders/42");
}

#[test]
fn parent_stable_url_path_provides_source_endpoint() {
    // Stable SERVER spans use url.path when no route template is available.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "s1",
                        "operationName": "POST /api/fault/pool-saturation",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 5000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "server" },
                            { "key": "url.path", "value": "/api/fault/pool-saturation" }
                        ]
                    },
                    {
                        "spanID": "s2",
                        "operationName": "query",
                        "references": [{ "refType": "CHILD_OF", "spanID": "s1" }],
                        "startTime": 1720621921123200,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "db.statement", "value": "SELECT 1" },
                            { "key": "db.system", "value": "postgresql" },
                            { "key": "code.function.name", "value": "com.foo.FaultPool.query" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source.endpoint, "/api/fault/pool-saturation");
}

#[test]
fn empty_http_fallback_does_not_block_url_path() {
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "s1",
                        "operationName": "POST /api/fault/pool-saturation",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 5000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "server" },
                            { "key": "http.target", "value": "" },
                            { "key": "http.url", "value": "" },
                            { "key": "url.full", "value": "" },
                            { "key": "url.path", "value": "/api/fault/pool-saturation" }
                        ]
                    },
                    {
                        "spanID": "s2",
                        "operationName": "query",
                        "references": [{ "refType": "CHILD_OF", "spanID": "s1" }],
                        "startTime": 1720621921123200,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "db.statement", "value": "SELECT 1" },
                            { "key": "db.system", "value": "postgresql" },
                            { "key": "code.function.name", "value": "com.foo.FaultPool.query" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    let sql = events
        .iter()
        .find(|event| event.event_type == EventType::Sql)
        .expect("sql child event present");

    assert_eq!(sql.source.endpoint, "/api/fault/pool-saturation");
}

#[test]
fn endpoint_falls_back_to_unknown_not_empty() {
    // An empty endpoint would put an empty component in the ack signature.
    // The documented fallback is "unknown" on every ingestion path.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "query",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events[0].source.endpoint, "unknown");
}

#[test]
fn code_frame_endpoint_reads_stable_semconv() {
    // An OTel 1.27+ agent emits only `code.function.name`. Reading only
    // the legacy spelling would leave the endpoint empty here while the
    // same trace over OTLP resolves, so one ack could not cover both paths.
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "query",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" },
                        { "key": "code.function.name", "value": "com.foo.PurgeJob.execute" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source.endpoint, "com.foo.PurgeJob.execute");
    assert_eq!(
        events[0].code_namespace.as_deref(),
        Some("com.foo.PurgeJob"),
        "namespace must be derived from the FQ name, as the OTLP path does"
    );
}

#[test]
fn stable_semconv_tags() {
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "query",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "db.query.text", "value": "SELECT 1" },
                        { "key": "db.system", "value": "mysql" }
                    ]
                }, {
                    "spanID": "s2",
                    "operationName": "fetch",
                    "references": [],
                    "startTime": 1720621921200000,
                    "duration": 1000,
                    "processID": "p1",
                    "tags": [
                        { "key": "url.full", "value": "http://api/items" },
                        { "key": "http.request.method", "value": "POST" },
                        { "key": "http.response.status_code", "value": "201" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let ingest = JaegerIngest::new(1_048_576);
    let events = ingest.ingest(json.as_bytes()).unwrap();
    assert_eq!(events.len(), 2);

    let sql = &events[0];
    assert_eq!(sql.target, "SELECT 1");
    assert_eq!(sql.operation, "mysql");

    let http = &events[1];
    assert_eq!(http.target, "http://api/items");
    assert_eq!(http.operation, "POST");
    assert_eq!(http.status_code, Some(201));
}

#[test]
fn micrometer_method_and_status_tags_are_read() {
    let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "http post",
                    "references": [],
                    "startTime": 1720621921200000,
                    "duration": 1000,
                    "processID": "p1",
                    "tags": [
                        { "key": "http.url", "value": "http://api/items" },
                        { "key": "method", "value": "POST" },
                        { "key": "status", "value": "201" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
    let events = JaegerIngest::new(1_048_576)
        .ingest(json.as_bytes())
        .unwrap();

    assert_eq!(events[0].operation, "POST");
    assert_eq!(events[0].status_code, Some(201));
}
