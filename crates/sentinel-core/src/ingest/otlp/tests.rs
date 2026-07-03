use super::*;
#[cfg(feature = "daemon")]
use crate::report::metrics::MetricsState;
use opentelemetry_proto::tonic::common::v1::AnyValue;
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans};

/// Build a metrics sink from a fresh `MetricsState`, coerced to the
/// trait object the OTLP module expects. Co-locates the
/// `Arc<MetricsState>` -> `Arc<dyn MetricsSink>` cast so the four
/// HTTP-handler tests below stay readable.
#[cfg(feature = "daemon")]
fn fresh_metrics_sink() -> (Arc<MetricsState>, Arc<dyn MetricsSink>) {
    let state = Arc::new(MetricsState::new());
    let sink: Arc<dyn MetricsSink> = state.clone();
    (state, sink)
}

fn make_kv(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
        ..Default::default()
    }
}

fn make_int_kv(key: &str, value: i64) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::IntValue(value)),
        }),
        ..Default::default()
    }
}

fn make_sql_span(
    trace_id: &[u8],
    span_id: &[u8],
    parent_span_id: &[u8],
    statement: &str,
    start_ns: u64,
    end_ns: u64,
) -> Span {
    Span {
        trace_id: trace_id.to_vec(),
        span_id: span_id.to_vec(),
        parent_span_id: parent_span_id.to_vec(),
        name: "db.query".to_string(),
        start_time_unix_nano: start_ns,
        end_time_unix_nano: end_ns,
        attributes: vec![
            make_kv("db.statement", statement),
            make_kv("db.system", "postgresql"),
        ],
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)] // test helper builds a full OTLP Span with all required fields
fn make_http_span(
    trace_id: &[u8],
    span_id: &[u8],
    parent_span_id: &[u8],
    url: &str,
    method: &str,
    status: i64,
    start_ns: u64,
    end_ns: u64,
) -> Span {
    Span {
        trace_id: trace_id.to_vec(),
        span_id: span_id.to_vec(),
        parent_span_id: parent_span_id.to_vec(),
        name: "http.request".to_string(),
        start_time_unix_nano: start_ns,
        end_time_unix_nano: end_ns,
        attributes: vec![
            make_kv("http.url", url),
            make_kv("http.method", method),
            make_int_kv("http.status_code", status),
        ],
        ..Default::default()
    }
}

fn make_parent_span(span_id: &[u8], route: &str) -> Span {
    Span {
        trace_id: vec![1; 16],
        span_id: span_id.to_vec(),
        parent_span_id: vec![],
        name: "HandleRequest".to_string(),
        start_time_unix_nano: 0,
        end_time_unix_nano: 1_000_000_000,
        attributes: vec![
            make_kv("http.route", route),
            make_kv("code.function", "OrderService::create_order"),
        ],
        ..Default::default()
    }
}

fn make_request(service: &str, spans: Vec<Span>) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![make_kv("service.name", service)],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

#[test]
fn empty_request_returns_empty() {
    let req = ExportTraceServiceRequest {
        resource_spans: vec![],
    };
    assert!(convert_otlp_request(&req).is_empty());
}

#[test]
fn sql_span_maps_correctly() {
    let span = make_sql_span(
        &[1; 16],
        &[2; 8],
        &[],
        "SELECT * FROM order_item WHERE order_id = 42",
        1_720_621_921_000_000_000, // 2024-07-10T14:32:01.000Z
        1_720_621_921_001_200_000, // +1.2ms
    );
    let req = make_request("order-svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(events[0].operation, "postgresql");
    assert_eq!(
        events[0].target,
        "SELECT * FROM order_item WHERE order_id = 42"
    );
    assert_eq!(&*events[0].service, "order-svc");
    assert_eq!(events[0].duration_us, 1200);
    assert!(events[0].status_code.is_none());
}

#[test]
fn http_span_maps_correctly() {
    let span = make_http_span(
        &[1; 16],
        &[3; 8],
        &[],
        "http://user-svc:5000/api/users/123",
        "GET",
        200,
        1_720_621_921_000_000_000,
        1_720_621_921_015_000_000, // +15ms
    );
    let req = make_request("order-svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::HttpOut);
    assert_eq!(events[0].operation, "GET");
    assert_eq!(events[0].target, "http://user-svc:5000/api/users/123");
    assert_eq!(events[0].status_code, Some(200));
    assert_eq!(events[0].duration_us, 15000);
}

#[test]
fn non_io_span_skipped() {
    let span = Span {
        trace_id: vec![1; 16],
        span_id: vec![4; 8],
        name: "internal.processing".to_string(),
        start_time_unix_nano: 1_720_621_921_000_000_000,
        end_time_unix_nano: 1_720_619_521_000_500_000,
        attributes: vec![make_kv("custom.attr", "value")],
        ..Default::default()
    };
    let req = make_request("order-svc", vec![span]);
    assert!(convert_otlp_request(&req).is_empty());
}

/// Bare span with only the given attributes (filter-reason fixtures).
fn make_bare_span(span_id: &[u8], attributes: Vec<KeyValue>) -> Span {
    Span {
        trace_id: vec![1; 16],
        span_id: span_id.to_vec(),
        name: "fixture".to_string(),
        start_time_unix_nano: 1_720_621_921_000_000_000,
        end_time_unix_nano: 1_720_621_921_000_500_000,
        attributes,
        ..Default::default()
    }
}

#[test]
fn counted_conversion_classifies_filtered_spans() {
    // Four spans: internal, db.system without statement, HTTP method
    // without url, valid SQL. One filtered per reason, one retained.
    let internal = make_bare_span(&[4; 8], vec![make_kv("custom.attr", "value")]);
    let db_no_statement = make_bare_span(&[5; 8], vec![make_kv("db.system", "postgresql")]);
    let http_no_url = make_bare_span(&[6; 8], vec![make_kv("http.method", "GET")]);
    let sql = make_sql_span(&[1; 16], &[7; 8], &[], "SELECT 1", 0, 1000);
    let req = make_request(
        "order-svc",
        vec![internal, db_no_statement, http_no_url, sql],
    );

    let (events, stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(
        stats,
        SpanConversionStats {
            received: 4,
            filtered_not_io: 1,
            filtered_missing_db_statement: 1,
            filtered_missing_http_url: 1,
            filtered_non_sql_datastore: 0,
        }
    );
}

#[test]
fn non_sql_datastore_span_is_dropped() {
    // A Redis span carries a db.statement that is not relational SQL;
    // it must be dropped under the dedicated reason, never tokenized.
    let redis = make_bare_span(
        &[8; 8],
        vec![
            make_kv("db.system", "redis"),
            make_kv("db.statement", "GET user:123"),
        ],
    );
    let sql = make_sql_span(&[1; 16], &[7; 8], &[], "SELECT 1", 0, 1000);
    let req = make_request("order-svc", vec![redis, sql]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(stats.received, 2);
    assert_eq!(stats.filtered_non_sql_datastore, 1);
    assert_eq!(stats.filtered_not_io, 0);
}

#[test]
fn non_sql_datastore_span_with_url_is_dropped_not_http() {
    // An ES/OpenSearch span over an HTTP transport may carry both a
    // statement and url.full; the db.system gate must still drop it
    // rather than reclassify it as an HTTP outbound call.
    let es = make_bare_span(
        &[8; 8],
        vec![
            make_kv("db.system", "elasticsearch"),
            make_kv("db.statement", "GET /index/_search"),
            make_kv("url.full", "http://es:9200/index/_search"),
        ],
    );
    let req = make_request("search-svc", vec![es]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(stats.filtered_non_sql_datastore, 1);
}

#[test]
fn non_sql_datastore_span_without_statement_is_not_an_instrumentation_gap() {
    // A Redis span with db.system but no db.statement must count as a
    // deliberate non-SQL drop, not a MissingDbStatement instrumentation gap.
    let redis = make_bare_span(&[8; 8], vec![make_kv("db.system", "redis")]);
    let req = make_request("cache-svc", vec![redis]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(stats.filtered_non_sql_datastore, 1);
    assert_eq!(stats.filtered_missing_db_statement, 0);
}

#[test]
fn datadog_resource_with_db_type_classifies_as_sql() {
    // dd-trace leaves the obfuscated SQL in the Datadog resource, which
    // the OTel datadogreceiver surfaces as dd.span.Resource, and sets the
    // db.type meta key. No OTel db.statement is present. perf-sentinel must
    // still extract the SQL via the Datadog fallback.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "SELECT * FROM orders WHERE id = ?"),
            make_kv("db.type", "postgres"),
        ],
    );
    let req = make_request("order-svc", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(events[0].target, "SELECT * FROM orders WHERE id = ?");
    // dd-trace "postgres" is normalized to the OTel "postgresql" spelling
    // so both ingestion paths label the same engine identically.
    assert_eq!(events[0].operation, "postgresql");
    assert_eq!(stats.filtered_missing_db_statement, 0);
}

#[test]
fn datadog_resource_with_unknown_db_type_is_not_tokenized_as_sql() {
    // A non-SQL store whose dd-trace db.type escapes the non-SQL denylist
    // (here a fictional "aerospike") must NOT have its command tokenized as
    // SQL. The resource fallback is fail closed: only recognized SQL engines
    // reach the tokenizer. The span still carries a db signal, so it counts
    // as a missing-statement gap, never an emitted SQL event.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "GET namespace:set:key"),
            make_kv("db.type", "aerospike"),
        ],
    );
    let req = make_request("cache-svc", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(stats.filtered_missing_db_statement, 1);
}

#[test]
fn datadog_empty_resource_is_not_an_empty_sql_event() {
    // A dd-trace DB span whose resource the collector left empty must not
    // produce an empty-target SQL event. It is a SQL engine missing its
    // statement, so it counts as a missing-db-statement gap, not SQL.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "   "),
            make_kv("db.type", "postgres"),
        ],
    );
    let req = make_request("order-svc", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(stats.filtered_missing_db_statement, 1);
}

#[test]
fn datadog_resource_with_otel_db_system_classifies_as_sql() {
    // Newer datadogreceiver versions map the db system to the OTel
    // db.system key but still keep the statement only in dd.span.Resource.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "SELECT 1"),
            make_kv("db.system", "postgresql"),
        ],
    );
    let req = make_request("order-svc", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(events[0].target, "SELECT 1");
    assert_eq!(events[0].operation, "postgresql");
    assert_eq!(stats.filtered_missing_db_statement, 0);
}

#[test]
fn datadog_receiver_stable_semconv_db_system_name_classifies_as_sql() {
    // The current OTel datadogreceiver (v0.155+) emits the DB system under
    // the stable OTel 1.27+ key db.system.name (value "postgres"), not the
    // older db.system or the dd-trace db.type, and leaves the obfuscated SQL
    // in dd.span.Resource. perf-sentinel must recognize the stable key or
    // the whole dd-trace bridge yields zero SQL findings.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv(
                "dd.span.Resource",
                "SELECT * FROM order_item WHERE order_id = ?",
            ),
            make_kv("db.system.name", "postgres"),
        ],
    );
    let req = make_request("dd-shop", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(
        events[0].target,
        "SELECT * FROM order_item WHERE order_id = ?"
    );
    assert_eq!(events[0].operation, "postgresql");
    assert_eq!(stats.filtered_missing_db_statement, 0);
}

#[test]
fn datadog_stable_namespaced_non_sql_is_dropped() {
    // Stable OTel db.system.name uses namespaced spellings: DynamoDB is
    // "aws.dynamodb". It must canonicalize to "dynamodb" and be dropped as a
    // non-SQL datastore, never tokenized as SQL (the statement can carry PII).
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("db.system.name", "aws.dynamodb"),
            make_kv(
                "db.statement",
                "SELECT * FROM Orders WHERE Id = 'secret-key'",
            ),
        ],
    );
    let req = make_request("shop", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(stats.filtered_non_sql_datastore, 1);
}

#[test]
fn datadog_stable_unknown_db_system_name_without_statement_is_a_gap() {
    // A DB span reported under the stable key with no statement counts as a
    // missing-statement gap whether or not the engine is in the SQL
    // allowlist, so gaps on engines like Snowflake are still reported. The
    // stable key and db.type classify the same store identically.
    let span = make_bare_span(&[9; 8], vec![make_kv("db.system.name", "aerospike")]);
    let req = make_request("cache", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(stats.filtered_missing_db_statement, 1);
}

#[test]
fn datadog_http_resource_with_db_tag_is_not_tokenized_as_sql() {
    // A mis-tagged dd-trace span carrying an HTTP route in dd.span.Resource
    // plus a SQL db signal and an http.url must NOT have the route (which can
    // carry query-string secrets) tokenized as SQL; it is an HTTP call.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "GET /api/users?token=SECRET"),
            make_kv("db.type", "postgres"),
            make_kv("http.url", "https://svc/api/users?token=SECRET"),
        ],
    );
    let req = make_request("svc", vec![span]);

    let (events, _stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::HttpOut);
}

#[test]
fn datadog_stable_namespaced_sql_server_classifies_as_sql() {
    // SQL Server's stable db.system.name is "microsoft.sql_server"; it must
    // canonicalize to "mssql" so the dd.span.Resource fallback fires.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "SELECT * FROM orders WHERE id = ?"),
            make_kv("db.system.name", "microsoft.sql_server"),
        ],
    );
    let req = make_request("shop", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(events[0].operation, "mssql");
    assert_eq!(stats.filtered_missing_db_statement, 0);
}

#[test]
fn datadog_empty_db_system_name_does_not_shadow_db_type() {
    // An empty db.system.name must not short-circuit the effective-system
    // precedence and suppress a valid db.type, which would lose the SQL.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("db.system.name", ""),
            make_kv("db.type", "postgres"),
            make_kv("dd.span.Resource", "SELECT 1"),
        ],
    );
    let req = make_request("shop", vec![span]);

    let (events, _stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(events[0].operation, "postgresql");
}

#[test]
fn datadog_stable_http_method_blocks_resource_sql_fallback() {
    // A mis-tagged span with an HTTP route in dd.span.Resource, a SQL db
    // signal, and only the stable http.request.method (no http.url) must not
    // leak the route (with its query-string secret) through the SQL path.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "GET /api/users?token=SECRET"),
            make_kv("db.type", "postgres"),
            make_kv("http.request.method", "GET"),
        ],
    );
    let req = make_request("svc", vec![span]);

    let (events, _stats) = convert_otlp_request_counted(&req);

    assert!(!events.iter().any(|e| e.target.contains("SECRET")));
}

#[test]
fn datadog_whitespace_db_system_name_does_not_shadow_db_type() {
    // A whitespace-only db.system.name must not shadow a valid db.type,
    // which would drop the SQL.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("db.system.name", "   "),
            make_kv("db.type", "postgres"),
            make_kv("dd.span.Resource", "SELECT 1"),
        ],
    );
    let req = make_request("shop", vec![span]);

    let (events, _stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(events[0].operation, "postgresql");
}

#[test]
fn datadog_cloud_sql_engine_classifies_as_sql() {
    // Cloud SQL engines like Snowflake must be recognized so dd-trace users
    // on them get findings, per the integration doc's promise.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "SELECT * FROM orders WHERE id = ?"),
            make_kv("db.type", "snowflake"),
        ],
    );
    let req = make_request("shop", vec![span]);

    let (events, _stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Sql);
    assert_eq!(events[0].operation, "snowflake");
}

#[test]
fn datadog_resource_whitespace_is_trimmed_in_target() {
    // Stray collector whitespace around the resource must be trimmed so
    // repeated queries do not fragment into separate N+1 groups.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "  SELECT 1\n"),
            make_kv("db.type", "postgres"),
        ],
    );
    let req = make_request("shop", vec![span]);

    let (events, _stats) = convert_otlp_request_counted(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].target, "SELECT 1");
}

#[test]
fn datadog_resource_without_db_signal_is_not_sql() {
    // dd.span.Resource is set on every dd-trace span, including HTTP ones
    // (resource = "GET /api/orders"). Without a DB signal it must never be
    // read as a SQL statement.
    let span = make_bare_span(
        &[9; 8],
        vec![make_kv("dd.span.Resource", "GET /api/orders")],
    );
    let req = make_request("order-svc", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(stats.filtered_not_io, 1);
}

#[test]
fn datadog_redis_resource_is_dropped_non_sql() {
    // A dd-trace Redis span carries dd.span.Resource plus db.type=redis.
    // The effective-db-system gate must drop it as a non-SQL datastore,
    // never tokenize the resource as SQL.
    let span = make_bare_span(
        &[9; 8],
        vec![
            make_kv("dd.span.Resource", "GET user:123"),
            make_kv("db.type", "redis"),
        ],
    );
    let req = make_request("cache-svc", vec![span]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(stats.filtered_non_sql_datastore, 1);
    assert_eq!(stats.filtered_missing_db_statement, 0);
}

#[test]
fn server_span_without_url_counts_not_io_not_missing_url() {
    // Stable semconv: SERVER spans carry http.request.method +
    // url.path, url.full is CLIENT-only. A server span without a
    // full URL is inbound work, not stripped instrumentation.
    use opentelemetry_proto::tonic::trace::v1::span::SpanKind;

    let mut server = make_bare_span(&[5; 8], vec![make_kv("http.request.method", "GET")]);
    server.kind = SpanKind::Server as i32;
    let mut client = make_bare_span(&[6; 8], vec![make_kv("http.request.method", "GET")]);
    client.kind = SpanKind::Client as i32;
    let req = make_request("order-svc", vec![server, client]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(
        stats,
        SpanConversionStats {
            received: 2,
            filtered_not_io: 1,
            filtered_missing_db_statement: 0,
            filtered_missing_http_url: 1,
            filtered_non_sql_datastore: 0,
        }
    );
}

#[test]
fn counted_conversion_all_filtered_yields_zero_events() {
    // The "received but 0 retained" case: the request succeeds, the
    // tally is the only signal that nothing was analyzable.
    let internal = make_bare_span(&[4; 8], vec![make_kv("custom.attr", "value")]);
    let req = make_request("order-svc", vec![internal]);

    let (events, stats) = convert_otlp_request_counted(&req);

    assert!(events.is_empty());
    assert_eq!(stats.received, 1);
    assert_eq!(stats.filtered_not_io, 1);
}

#[cfg(feature = "daemon")]
#[test]
fn record_otlp_spans_moves_received_and_filtered_counters() {
    let (state, sink) = fresh_metrics_sink();
    sink.record_otlp_spans(SpanConversionStats {
        received: 5,
        filtered_not_io: 2,
        filtered_missing_db_statement: 1,
        filtered_missing_http_url: 0,
        filtered_non_sql_datastore: 3,
    });

    assert_eq!(state.otlp_spans_received_total.get(), 5);
    let filtered = |reason: &str| {
        state
            .otlp_spans_filtered_total
            .with_label_values(&[reason])
            .get()
    };
    assert_eq!(filtered("not_io"), 2);
    assert_eq!(filtered("missing_db_statement"), 1);
    assert_eq!(filtered("missing_http_url"), 0);
    assert_eq!(filtered("non_sql_datastore"), 3);
}

#[test]
fn parent_span_provides_source_endpoint() {
    let parent = make_parent_span(&[10; 8], "POST /api/orders/{id}/submit");
    let child = make_sql_span(
        &[1; 16],
        &[20; 8],
        &[10; 8], // parent_span_id
        "SELECT * FROM order_item WHERE order_id = 42",
        1_720_621_921_000_000_000,
        1_720_621_921_001_200_000,
    );
    let req = make_request("order-svc", vec![parent, child]);
    let events = convert_otlp_request(&req);

    // Only the child (SQL) should produce an event
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source.endpoint, "POST /api/orders/{id}/submit");
    assert_eq!(events[0].source.method, "OrderService::create_order");
}

#[test]
fn parent_span_http_route_takes_precedence_over_http_url() {
    // Critical for ack stability: when the parent emits both
    // http.route (template) and http.url (instantiated), the route
    // must win. Otherwise every distinct request id forks the ack
    // signature.
    let parent = Span {
        trace_id: vec![1; 16],
        span_id: vec![10; 8],
        parent_span_id: vec![],
        name: "HandleRequest".to_string(),
        start_time_unix_nano: 0,
        end_time_unix_nano: 1_000_000_000,
        attributes: vec![
            make_kv("http.route", "POST /api/orders/{id}/submit"),
            make_kv("http.url", "http://order-svc/api/orders/42/submit"),
            make_kv("code.function", "OrderService::create_order"),
        ],
        ..Default::default()
    };
    let child = make_sql_span(
        &[1; 16],
        &[20; 8],
        &[10; 8],
        "SELECT * FROM order_item WHERE order_id = 42",
        1_720_621_921_000_000_000,
        1_720_621_921_001_200_000,
    );
    let req = make_request("order-svc", vec![parent, child]);
    let events = convert_otlp_request(&req);

    let sql = events
        .iter()
        .find(|e| e.event_type == EventType::Sql)
        .expect("sql child event present");
    assert_eq!(sql.source.endpoint, "POST /api/orders/{id}/submit");
}

#[test]
fn parent_span_http_url_used_only_when_route_absent() {
    // Documented fallback: instrumentation that omits http.route
    // (legacy SDK, manual instrumentation) loses signature stability
    // but still produces a usable endpoint string.
    let parent = Span {
        trace_id: vec![1; 16],
        span_id: vec![10; 8],
        parent_span_id: vec![],
        name: "HandleRequest".to_string(),
        start_time_unix_nano: 0,
        end_time_unix_nano: 1_000_000_000,
        attributes: vec![make_kv("http.url", "http://order-svc/api/orders/42/submit")],
        ..Default::default()
    };
    let child = make_sql_span(&[1; 16], &[20; 8], &[10; 8], "SELECT 1", 0, 1_000_000);
    let req = make_request("order-svc", vec![parent, child]);
    let events = convert_otlp_request(&req);

    let sql = events
        .iter()
        .find(|e| e.event_type == EventType::Sql)
        .expect("sql child event present");
    assert_eq!(sql.source.endpoint, "http://order-svc/api/orders/42/submit");
}

#[test]
fn parent_span_url_full_used_when_neither_route_nor_url_present() {
    // OTel stable v1.21+ replaces http.url with url.full. Last-resort
    // fallback once http.route and http.url are both absent.
    let parent = Span {
        trace_id: vec![1; 16],
        span_id: vec![10; 8],
        parent_span_id: vec![],
        name: "HandleRequest".to_string(),
        start_time_unix_nano: 0,
        end_time_unix_nano: 1_000_000_000,
        attributes: vec![make_kv("url.full", "http://order-svc/api/orders/42")],
        ..Default::default()
    };
    let child = make_sql_span(&[1; 16], &[20; 8], &[10; 8], "SELECT 1", 0, 1_000_000);
    let req = make_request("order-svc", vec![parent, child]);
    let events = convert_otlp_request(&req);

    let sql = events
        .iter()
        .find(|e| e.event_type == EventType::Sql)
        .expect("sql child event present");
    assert_eq!(sql.source.endpoint, "http://order-svc/api/orders/42");
}

#[test]
fn missing_parent_falls_back() {
    let child = make_sql_span(
        &[1; 16],
        &[20; 8],
        &[99; 8], // parent not in batch
        "SELECT * FROM order_item WHERE order_id = 42",
        1_720_621_921_000_000_000,
        1_720_621_921_001_200_000,
    );
    let req = make_request("order-svc", vec![child]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source.endpoint, "unknown");
    assert_eq!(events[0].source.method, "db.query");
}

#[test]
fn trace_id_hex_encoding() {
    let trace_bytes: Vec<u8> = (0..16).collect();
    assert_eq!(
        bytes_to_hex(&trace_bytes),
        "000102030405060708090a0b0c0d0e0f"
    );
}

#[test]
fn timestamp_nanos_to_iso8601() {
    // 2024-07-10T14:32:01.123Z UTC
    let nanos: u64 = 1_720_621_921_123_000_000;
    let iso = nanos_to_iso8601(nanos);
    assert_eq!(iso, "2024-07-10T14:32:01.123Z");
}

#[test]
fn timestamp_epoch_zero() {
    assert_eq!(nanos_to_iso8601(0), "1970-01-01T00:00:00.000Z");
}

#[test]
fn duration_calculation() {
    let span = make_sql_span(
        &[1; 16],
        &[2; 8],
        &[],
        "SELECT 1",
        1_000_000_000, // 1 second
        1_002_500_000, // +2.5ms = 2500us
    );
    let req = make_request("test", vec![span]);
    let events = convert_otlp_request(&req);
    assert_eq!(events[0].duration_us, 2500);
}

#[test]
fn status_code_extraction() {
    let span = make_http_span(
        &[1; 16],
        &[3; 8],
        &[],
        "http://svc/api/health",
        "GET",
        404,
        1_000_000_000,
        1_001_000_000,
    );
    let req = make_request("test", vec![span]);
    let events = convert_otlp_request(&req);
    assert_eq!(events[0].status_code, Some(404));
}

#[test]
fn service_name_from_resource() {
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    let req = make_request("my-service", vec![span]);
    let events = convert_otlp_request(&req);
    assert_eq!(&*events[0].service, "my-service");
}

#[test]
fn span_with_both_db_and_http_prefers_sql() {
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
    let mut span = make_sql_span(
        &[1; 16],
        &[2; 8],
        &[],
        "SELECT 1",
        1_000_000_000,
        1_001_000_000,
    );
    span.attributes.push(KeyValue {
        key: "http.url".to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue("http://svc/api".to_string())),
        }),
        ..Default::default()
    });
    let req = make_request("test", vec![span]);
    let events = convert_otlp_request(&req);
    // db.statement takes precedence
    assert_eq!(events[0].event_type, EventType::Sql);
}

#[test]
fn clock_skew_duration_is_zero() {
    // end < start -> saturating_sub gives 0
    let span = make_sql_span(
        &[1; 16],
        &[2; 8],
        &[],
        "SELECT 1",
        2_000_000_000, // start = 2s
        1_000_000_000, // end = 1s (before start)
    );
    let req = make_request("test", vec![span]);
    let events = convert_otlp_request(&req);
    assert_eq!(events[0].duration_us, 0);
}

#[test]
fn bytes_to_hex_empty() {
    assert_eq!(bytes_to_hex(&[]), "");
}

#[test]
fn bytes_to_hex_all_values() {
    assert_eq!(bytes_to_hex(&[0x00, 0xff, 0xab]), "00ffab");
}

#[test]
fn nanos_to_iso8601_leap_year() {
    // 2024-02-29T00:00:00.000Z (2024 is a leap year)
    let nanos: u64 = 1_709_164_800_000_000_000;
    let iso = nanos_to_iso8601(nanos);
    assert_eq!(iso, "2024-02-29T00:00:00.000Z");
}

#[test]
fn empty_trace_id_produces_empty_hex() {
    assert_eq!(bytes_to_hex(&[]), "");
}

#[test]
fn short_span_id_produces_short_hex() {
    assert_eq!(bytes_to_hex(&[0xab]), "ab");
}

#[test]
fn missing_service_name_defaults_to_unknown() {
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    let req = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![], // no service.name
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans: vec![span],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let events = convert_otlp_request(&req);
    assert_eq!(&*events[0].service, "unknown");
}

#[test]
fn no_resource_defaults_to_unknown_service() {
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    let req = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: None,
            scope_spans: vec![ScopeSpans {
                spans: vec![span],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let events = convert_otlp_request(&req);
    assert_eq!(&*events[0].service, "unknown");
}

// ----- cloud.region extraction tests -----

fn make_request_with_resource_attrs(
    attrs: Vec<KeyValue>,
    spans: Vec<Span>,
) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: attrs,
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

#[test]
fn cloud_region_extracted_from_resource_attributes() {
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    let req = make_request_with_resource_attrs(
        vec![
            make_kv("service.name", "order-svc"),
            make_kv("cloud.region", "eu-west-3"),
        ],
        vec![span],
    );
    let events = convert_otlp_request(&req);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].cloud_region.as_deref(), Some("eu-west-3"));
}

#[test]
fn cloud_region_falls_back_to_span_attribute() {
    // Resource has no cloud.region; span itself carries it.
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    span.attributes.push(make_kv("cloud.region", "us-east-1"));
    let req =
        make_request_with_resource_attrs(vec![make_kv("service.name", "order-svc")], vec![span]);
    let events = convert_otlp_request(&req);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].cloud_region.as_deref(), Some("us-east-1"));
}

#[test]
fn cloud_region_resource_wins_over_span() {
    // When both are present, the resource value takes precedence
    // (canonical OTel location, single source per service).
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    span.attributes.push(make_kv("cloud.region", "us-east-1"));
    let req = make_request_with_resource_attrs(
        vec![
            make_kv("service.name", "order-svc"),
            make_kv("cloud.region", "eu-west-3"),
        ],
        vec![span],
    );
    let events = convert_otlp_request(&req);
    assert_eq!(events[0].cloud_region.as_deref(), Some("eu-west-3"));
}

#[test]
fn no_cloud_region_yields_none() {
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    let req = make_request("order-svc", vec![span]);
    let events = convert_otlp_request(&req);
    assert!(events[0].cloud_region.is_none());
}

// ----- cloud.region sanitization at OTLP boundary -----

#[test]
fn cloud_region_with_space_is_sanitized_to_none() {
    // Invalid character (space) at the resource level → silently dropped.
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    let req = make_request_with_resource_attrs(
        vec![
            make_kv("service.name", "order-svc"),
            make_kv("cloud.region", "eu west 3"),
        ],
        vec![span],
    );
    let events = convert_otlp_request(&req);
    assert_eq!(events.len(), 1);
    assert!(
        events[0].cloud_region.is_none(),
        "region with space must be sanitized to None"
    );
}

#[test]
fn oversized_cloud_region_is_sanitized_to_none() {
    // 65 chars, exceeds the 64-char cap.
    let long_region = "a".repeat(65);
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    let req = make_request_with_resource_attrs(
        vec![
            make_kv("service.name", "order-svc"),
            make_kv("cloud.region", &long_region),
        ],
        vec![span],
    );
    let events = convert_otlp_request(&req);
    assert!(events[0].cloud_region.is_none());
}

#[test]
fn cloud_region_with_control_char_is_sanitized_to_none() {
    // Log-forging payload: newline + fake log line.
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    let req = make_request_with_resource_attrs(
        vec![
            make_kv("service.name", "order-svc"),
            make_kv("cloud.region", "eu-west-3\n2026-04-07 ERROR fake"),
        ],
        vec![span],
    );
    let events = convert_otlp_request(&req);
    assert!(events[0].cloud_region.is_none());
}

#[test]
fn cloud_region_span_level_fallback_also_sanitized() {
    // Invalid cloud.region at the span level (resource has none) →
    // silently dropped too.
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    span.attributes.push(make_kv("cloud.region", "bad region!"));
    let req = make_request("order-svc", vec![span]);
    let events = convert_otlp_request(&req);
    assert!(events[0].cloud_region.is_none());
}

// ── instrumentation_scopes capture from OTLP ──────────────────

fn scoped_request(service: &str, scoped: Vec<(&str, Vec<Span>)>) -> ExportTraceServiceRequest {
    use opentelemetry_proto::tonic::common::v1::InstrumentationScope;
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![make_kv("service.name", service)],
                ..Default::default()
            }),
            scope_spans: scoped
                .into_iter()
                .map(|(name, spans)| ScopeSpans {
                    scope: Some(InstrumentationScope {
                        name: name.to_string(),
                        ..Default::default()
                    }),
                    spans,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }],
    }
}

#[test]
fn instrumentation_scope_captured_from_leaf_only() {
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    let req = scoped_request("svc", vec![("io.opentelemetry.jdbc", vec![span])]);
    let events = convert_otlp_request(&req);
    assert_eq!(events.len(), 1);
    let scopes: Vec<&str> = events[0]
        .instrumentation_scopes
        .iter()
        .map(AsRef::as_ref)
        .collect();
    assert_eq!(scopes, vec!["io.opentelemetry.jdbc"]);
}

#[test]
fn instrumentation_scopes_walk_parent_chain_deduped() {
    // Lab-shaped trace: HTTP server (spring-webmvc) -> Spring
    // Data span (spring-data-3.0) -> Hibernate (hibernate-6.0)
    // -> JDBC leaf (jdbc). Walker collects unique scopes leaf
    // to root.
    let http = make_span_with_code_attrs(
        &[10; 8],
        &[],
        "GET /api/orders",
        vec![make_kv("http.route", "GET /api/orders")],
    );
    let spring_data =
        make_span_with_code_attrs(&[11; 8], &[10; 8], "OrderRepository.findById", vec![]);
    let hibernate = make_span_with_code_attrs(&[12; 8], &[11; 8], "Session.find", vec![]);
    let jdbc = make_sql_span(&[1; 16], &[13; 8], &[12; 8], "SELECT 1", 0, 1_000_000);
    let req = scoped_request(
        "svc",
        vec![
            ("io.opentelemetry.spring-webmvc-6.0", vec![http]),
            ("io.opentelemetry.spring-data-3.0", vec![spring_data]),
            ("io.opentelemetry.hibernate-6.0", vec![hibernate]),
            ("io.opentelemetry.jdbc", vec![jdbc]),
        ],
    );
    let events = convert_otlp_request(&req);
    assert_eq!(events.len(), 1, "only the JDBC span yields a SpanEvent");
    let scopes: Vec<&str> = events[0]
        .instrumentation_scopes
        .iter()
        .map(AsRef::as_ref)
        .collect();
    assert_eq!(
        scopes,
        vec![
            "io.opentelemetry.jdbc",
            "io.opentelemetry.hibernate-6.0",
            "io.opentelemetry.spring-data-3.0",
            "io.opentelemetry.spring-webmvc-6.0",
        ],
        "leaf-to-root order, deduplicated"
    );
}

#[test]
fn instrumentation_scopes_empty_when_scope_absent() {
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    let req = make_request("svc", vec![span]);
    let events = convert_otlp_request(&req);
    assert_eq!(events.len(), 1);
    assert!(events[0].instrumentation_scopes.is_empty());
}

#[test]
fn cloud_region_empty_string_is_sanitized_to_none() {
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
    let req = make_request_with_resource_attrs(
        vec![
            make_kv("service.name", "order-svc"),
            make_kv("cloud.region", ""),
        ],
        vec![span],
    );
    let events = convert_otlp_request(&req);
    assert!(events[0].cloud_region.is_none());
}

// ── code.* parent walk and stable convention support ────────────

fn make_span_with_code_attrs(
    span_id: &[u8],
    parent_span_id: &[u8],
    name: &str,
    code_attrs: Vec<KeyValue>,
) -> Span {
    Span {
        trace_id: vec![1; 16],
        span_id: span_id.to_vec(),
        parent_span_id: parent_span_id.to_vec(),
        name: name.to_string(),
        start_time_unix_nano: 0,
        end_time_unix_nano: 1_000_000,
        attributes: code_attrs,
        ..Default::default()
    }
}

#[test]
fn code_attrs_inherited_from_immediate_parent() {
    // HTTP server parent carries code.namespace; JDBC child has none.
    // Walker must surface the parent namespace on the child SpanEvent.
    let parent = make_span_with_code_attrs(
        &[10; 8],
        &[],
        "GET /api/orders",
        vec![
            make_kv("http.route", "GET /api/orders"),
            make_kv("code.namespace", "com.foo.OrderController"),
            make_kv("code.function", "list"),
        ],
    );
    let child = make_sql_span(
        &[1; 16],
        &[20; 8],
        &[10; 8],
        "SELECT * FROM orders",
        0,
        1_000_000,
    );
    let req = make_request("order-svc", vec![parent, child]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].code_namespace.as_deref(),
        Some("com.foo.OrderController")
    );
    assert_eq!(events[0].code_function.as_deref(), Some("list"));
}

#[test]
fn code_attrs_inherited_from_grandparent() {
    // HTTP -> Service (carries code.*) -> Hibernate -> JDBC (no code.*).
    // Walker must traverse multiple levels.
    let http = make_span_with_code_attrs(
        &[10; 8],
        &[],
        "GET /api/orders",
        vec![make_kv("http.route", "GET /api/orders")],
    );
    let service = make_span_with_code_attrs(
        &[11; 8],
        &[10; 8],
        "OrderService.list",
        vec![
            make_kv("code.namespace", "com.foo.OrderService"),
            make_kv("code.function", "list"),
        ],
    );
    let hibernate = make_span_with_code_attrs(&[12; 8], &[11; 8], "Hibernate.query", vec![]);
    let jdbc = make_sql_span(&[1; 16], &[13; 8], &[12; 8], "SELECT 1", 0, 1_000_000);
    let req = make_request("order-svc", vec![http, service, hibernate, jdbc]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].code_namespace.as_deref(),
        Some("com.foo.OrderService")
    );
}

#[test]
fn code_attrs_orphan_span_returns_none() {
    // Span with no parent and no code.* must yield code_namespace = None.
    let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    let req = make_request("svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert!(events[0].code_namespace.is_none());
    assert!(events[0].code_function.is_none());
}

#[test]
fn code_attrs_max_depth_safety() {
    // Chain depth larger than CODE_ATTRS_MAX_DEPTH, none carry code.*.
    // Walker terminates at the cap and returns None without looping.
    let depth = u8::try_from(CODE_ATTRS_MAX_DEPTH * 2 + 4).unwrap();
    let mut spans = Vec::new();
    for i in 0..depth {
        let id = [i + 1; 8];
        let parent = if i == 0 { vec![] } else { vec![i; 8] };
        spans.push(make_span_with_code_attrs(
            &id,
            &parent,
            &format!("level.{i}"),
            vec![],
        ));
    }
    let leaf = make_sql_span(&[1; 16], &[100; 8], &[depth; 8], "SELECT 1", 0, 1_000_000);
    spans.push(leaf);
    let req = make_request("svc", spans);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert!(events[0].code_namespace.is_none());
}

#[test]
fn code_attrs_self_takes_precedence() {
    // Span has its own code.namespace; parent has a different one.
    // The span's own attrs must win; the walker only triggers when the
    // span itself has nothing.
    let parent = make_span_with_code_attrs(
        &[10; 8],
        &[],
        "GET /api/x",
        vec![make_kv("code.namespace", "com.parent")],
    );
    let mut child = make_sql_span(&[1; 16], &[20; 8], &[10; 8], "SELECT 1", 0, 1_000_000);
    child
        .attributes
        .push(make_kv("code.namespace", "com.child"));
    let req = make_request("svc", vec![parent, child]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].code_namespace.as_deref(), Some("com.child"));
}

// ── Stable code.* conventions (semconv v1.33.0) ─────────────────

#[test]
fn code_attrs_stable_conventions() {
    // Stable names only. Namespace must be derived from the FQ
    // function name by splitting on the last dot.
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    span.attributes.extend(vec![
        make_kv("code.function.name", "com.foo.OrderService.findItems"),
        make_kv("code.file.path", "src/main/java/com/foo/OrderService.java"),
        make_int_kv("code.line.number", 42),
    ]);
    let req = make_request("svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].code_function.as_deref(),
        Some("com.foo.OrderService.findItems")
    );
    assert_eq!(
        events[0].code_filepath.as_deref(),
        Some("src/main/java/com/foo/OrderService.java")
    );
    assert_eq!(events[0].code_lineno, Some(42));
    assert_eq!(
        events[0].code_namespace.as_deref(),
        Some("com.foo.OrderService")
    );
}

#[test]
fn code_attrs_php_backslash_namespace_derivation() {
    // PHP `code.function.name` is `Namespace\Class::method` with no dot.
    // The namespace must be derived by splitting on the last `\`, so it
    // still contains the `Doctrine\DBAL` segment the suggestion layer
    // matches on.
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    span.attributes.push(make_kv(
        "code.function.name",
        "Doctrine\\DBAL\\Driver\\Connection::query",
    ));
    let req = make_request("svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].code_namespace.as_deref(),
        Some("Doctrine\\DBAL\\Driver")
    );
}

#[test]
fn code_attrs_legacy_conventions_still_work() {
    // Legacy names only. No regression from the stable-name addition.
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    span.attributes.extend(vec![
        make_kv("code.function", "findItems"),
        make_kv("code.namespace", "com.foo.OrderService"),
        make_kv("code.filepath", "src/OrderService.java"),
        make_int_kv("code.lineno", 99),
    ]);
    let req = make_request("svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].code_function.as_deref(), Some("findItems"));
    assert_eq!(
        events[0].code_namespace.as_deref(),
        Some("com.foo.OrderService")
    );
    assert_eq!(events[0].code_lineno, Some(99));
}

#[test]
fn code_attrs_legacy_namespace_wins_over_derivation() {
    // Pathological agent emits both a stable FQ function name AND an
    // explicit legacy code.namespace. The explicit value must win,
    // not the derivation from rsplit_once.
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    span.attributes.extend(vec![
        make_kv("code.function.name", "com.foo.X.y"),
        make_kv("code.namespace", "com.bar.X"),
    ]);
    let req = make_request("svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].code_namespace.as_deref(), Some("com.bar.X"));
}

#[test]
fn code_attrs_legacy_function_does_not_derive_namespace() {
    // Legacy `code.function` is documented as a bare function name, even
    // when an agent technically packs a dotted value into it. We must
    // NOT derive a namespace from it; doing so would surface false
    // positives in JAVA_RULES on agents that emit `code.function = "X.y"`.
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    span.attributes
        .push(make_kv("code.function", "OrderService.findItems"));
    let req = make_request("svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].code_function.as_deref(),
        Some("OrderService.findItems")
    );
    assert!(events[0].code_namespace.is_none());
}

#[test]
fn code_attrs_no_dot_in_fq_name() {
    // Bare function name (Rust, C, JS callbacks) has no dot to split on.
    // Namespace stays None.
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    span.attributes.push(make_kv("code.function.name", "main"));
    let req = make_request("svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].code_function.as_deref(), Some("main"));
    assert!(events[0].code_namespace.is_none());
}

#[test]
fn java_rules_match_via_derived_namespace() {
    // End-to-end: a stable-convention FQ name on a JPA repository span
    // must produce a SpanEvent whose code_namespace triggers JAVA_RULES
    // (via the JPA prefix). Verifies that namespace derivation feeds
    // the suggestion engine correctly.
    let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
    span.attributes.push(make_kv(
        "code.function.name",
        "org.springframework.data.jpa.repository.support.SimpleJpaRepository.findAll",
    ));
    let req = make_request("svc", vec![span]);
    let events = convert_otlp_request(&req);

    assert_eq!(events.len(), 1);
    let ns = events[0]
        .code_namespace
        .as_deref()
        .expect("namespace derived from FQ name");
    assert_eq!(
        ns,
        "org.springframework.data.jpa.repository.support.SimpleJpaRepository"
    );
    assert!(ns.contains("org.springframework.data.jpa"));
}

// ── OTLP/HTTP handler with gzip decompression ───────────────────

#[cfg(feature = "daemon")]
mod http_handler {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use prost::Message;
    use std::io::Write;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    fn build_minimal_request_bytes() -> Vec<u8> {
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        let req = make_request("svc", vec![span]);
        req.encode_to_vec()
    }

    fn gzip(body: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body).expect("gzip encode");
        encoder.finish().expect("gzip finish")
    }

    /// Build a POST `/v1/traces` request with `Content-Type:
    /// application/json` and an empty body, used to exercise the
    /// 415 path in tests that focus on the rejection metric (the
    /// body content does not matter, only the wrong content type).
    fn unsupported_media_type_request() -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(Vec::<u8>::new()))
            .expect("build request")
    }

    #[tokio::test]
    async fn otlp_http_accepts_gzip_request() {
        let (tx, mut rx) = mpsc::channel(8);
        let router = otlp_http_router(tx, 1_048_576, None);

        let body = build_minimal_request_bytes();
        let gzipped = gzip(&body);
        let req = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(gzipped))
            .expect("build request");

        let response = router.oneshot(req).await.expect("router runs");
        assert_eq!(response.status(), StatusCode::OK);

        let events = rx.recv().await.expect("event batch sent");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
    }

    #[tokio::test]
    async fn otlp_http_accepts_uncompressed_request() {
        let (tx, mut rx) = mpsc::channel(8);
        let router = otlp_http_router(tx, 1_048_576, None);

        let body = build_minimal_request_bytes();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .body(Body::from(body))
            .expect("build request");

        let response = router.oneshot(req).await.expect("router runs");
        assert_eq!(response.status(), StatusCode::OK);

        let events = rx.recv().await.expect("event batch sent");
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn otlp_http_rejects_unsupported_encoding() {
        // Brotli is not enabled; tower-http surfaces this as 415.
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
        let router = otlp_http_router(tx, 1_048_576, None);

        let body = build_minimal_request_bytes();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .header(header::CONTENT_ENCODING, "br")
            .body(Body::from(body))
            .expect("build request");

        let response = router.oneshot(req).await.expect("router runs");
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn otlp_http_rejects_oversize_compressed_body() {
        // Compressed body above max_payload_size must be rejected before
        // decompression, bounding attacker decompression CPU work even
        // when operators raise the cap. Realistic clients always send
        // Content-Length, so the layer rejects pre-decompression.
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
        let cap = 256_usize;
        let router = otlp_http_router(tx, cap, None);

        let payload: Vec<u8> = vec![0u8; 4096];
        let req = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .header(header::CONTENT_LENGTH, payload.len())
            .body(Body::from(payload))
            .expect("build request");

        let response = router.oneshot(req).await.expect("router runs");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn otlp_http_content_type_check_with_gzip() {
        // Gzip body but JSON Content-Type. The Content-Type guard runs
        // after decompression and must still reject this with 415.
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
        let router = otlp_http_router(tx, 1_048_576, None);

        let body = build_minimal_request_bytes();
        let gzipped = gzip(&body);
        let req = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(gzipped))
            .expect("build request");

        let response = router.oneshot(req).await.expect("router runs");
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn http_handler_records_unsupported_media_type() {
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
        let (metrics, sink) = fresh_metrics_sink();
        let router = otlp_http_router(tx, 1_048_576, Some(sink));

        let response = router
            .oneshot(unsupported_media_type_request())
            .await
            .expect("router runs");
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            metrics
                .otlp_rejected_total
                .with_label_values(&["unsupported_media_type"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn http_handler_records_parse_error() {
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
        let (metrics, sink) = fresh_metrics_sink();
        let router = otlp_http_router(tx, 1_048_576, Some(sink));

        // Random bytes are extremely unlikely to be a valid OTLP
        // ExportTraceServiceRequest protobuf. prost decode returns
        // an error and the handler must reject with 400.
        let req = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .body(Body::from(vec![0xff_u8, 0xff, 0xff, 0xff, 0xff, 0xff]))
            .expect("build request");
        let response = router.oneshot(req).await.expect("router runs");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            metrics
                .otlp_rejected_total
                .with_label_values(&["parse_error"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn http_handler_records_channel_full() {
        // Drop the receiver so any send fails immediately. The
        // handler must reject with 503 and bump the channel_full
        // counter.
        let (tx, rx) = mpsc::channel::<Vec<SpanEvent>>(1);
        drop(rx);
        let (metrics, sink) = fresh_metrics_sink();
        let router = otlp_http_router(tx, 1_048_576, Some(sink));

        let body = build_minimal_request_bytes();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .body(Body::from(body))
            .expect("build request");
        let response = router.oneshot(req).await.expect("router runs");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            metrics
                .otlp_rejected_total
                .with_label_values(&["channel_full"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn http_handler_rejects_under_memory_pressure() {
        // Flag set: reject before decode with 503 and bump the
        // memory_pressure counter, without enqueueing anything. The
        // receiver stays open, so a rejection can only come from the guard.
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(8);
        let (metrics, sink) = fresh_metrics_sink();
        metrics.set_memory_high_water(true);
        let router = otlp_http_router(tx, 1_048_576, Some(sink));

        let body = build_minimal_request_bytes();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .body(Body::from(body))
            .expect("build request");
        let response = router.oneshot(req).await.expect("router runs");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            metrics
                .otlp_rejected_total
                .with_label_values(&["memory_pressure"])
                .get(),
            1
        );
        assert!(rx.try_recv().is_err(), "ingest must be halted at the door");
    }

    #[tokio::test(start_paused = true)]
    async fn http_handler_rejects_when_channel_full_but_open() {
        // Saturation, not closure: the receiver stays alive but the
        // queue is full. The enqueue must time out, reject with 503
        // and bump channel_full instead of parking forever (paused
        // time auto-advances past INGEST_ENQUEUE_TIMEOUT).
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(1);
        tx.try_send(vec![]).expect("fill the only slot");
        let (metrics, sink) = fresh_metrics_sink();
        let router = otlp_http_router(tx, 1_048_576, Some(sink));

        let body = build_minimal_request_bytes();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .body(Body::from(body))
            .expect("build request");
        let response = router.oneshot(req).await.expect("router runs");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            metrics
                .otlp_rejected_total
                .with_label_values(&["channel_full"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn http_handler_no_metrics_state_does_not_panic() {
        // None metrics state must not panic at any rejection site.
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
        let router = otlp_http_router(tx, 1_048_576, None);

        let response = router
            .oneshot(unsupported_media_type_request())
            .await
            .expect("router runs");
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn grpc_handler_records_channel_full() {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService;

        let (tx, rx) = mpsc::channel::<Vec<SpanEvent>>(1);
        drop(rx);
        let metrics = Arc::new(MetricsState::new());
        let svc = OtlpGrpcService::new(tx, Some(metrics.clone()));

        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        // Fully-qualified to avoid the `axum::http::Request` shadow
        // pulled in by the surrounding axum test module.
        let req = tonic::Request::new(make_request("svc", vec![span]));
        let result = svc.export(req).await;
        assert_eq!(
            result.expect_err("closed channel rejects").code(),
            tonic::Code::Internal,
            "shutdown (closed channel) is a genuine internal state"
        );
        assert_eq!(
            metrics
                .otlp_rejected_total
                .with_label_values(&["channel_full"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn grpc_handler_rejects_under_memory_pressure() {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService;

        // Flag set: reject with UNAVAILABLE (retryable) and bump the
        // memory_pressure counter before conversion. The receiver stays
        // open, so the rejection can only come from the guard.
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(8);
        let metrics = Arc::new(MetricsState::new());
        metrics.set_memory_high_water(true);
        let svc = OtlpGrpcService::new(tx, Some(metrics.clone()));

        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        let req = tonic::Request::new(make_request("svc", vec![span]));
        assert_eq!(
            svc.export(req)
                .await
                .expect_err("memory pressure rejects")
                .code(),
            tonic::Code::Unavailable,
        );
        assert_eq!(
            metrics
                .otlp_rejected_total
                .with_label_values(&["memory_pressure"])
                .get(),
            1
        );
        assert!(rx.try_recv().is_err(), "ingest must be halted at the door");
    }

    #[tokio::test(start_paused = true)]
    async fn grpc_handler_returns_unavailable_when_channel_full_but_open() {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService;

        // Saturation, not shutdown: the OTLP spec lists UNAVAILABLE
        // as retryable, INTERNAL is not. A saturated daemon must not
        // make compliant exporters drop the batch permanently.
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(1);
        tx.try_send(vec![]).expect("fill the only slot");
        let metrics = Arc::new(MetricsState::new());
        let svc = OtlpGrpcService::new(tx, Some(metrics.clone()));

        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        let req = tonic::Request::new(make_request("svc", vec![span]));
        let result = svc.export(req).await;
        assert_eq!(
            result.expect_err("full channel rejects").code(),
            tonic::Code::Unavailable
        );
        assert_eq!(
            metrics
                .otlp_rejected_total
                .with_label_values(&["channel_full"])
                .get(),
            1
        );
    }
}
