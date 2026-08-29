//! Ingestion stage: reads raw events from various sources.

#[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
pub mod auth_header;
pub mod jaeger;
#[cfg(feature = "jaeger-query")]
pub mod jaeger_query;
pub mod json;
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
pub mod lookback;
pub mod mysql_stat;
pub mod otlp;
pub mod pg_stat;
#[cfg(any(feature = "daemon", feature = "tempo"))]
pub(crate) mod prometheus_scrape;
#[cfg(feature = "tempo")]
pub mod tempo;
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
pub(crate) mod url_enc;
pub mod zipkin;

use crate::event::SpanEvent;

/// Upper bound on `--max-traces` for the backend-query subcommands,
/// tempo and jaeger-query alike. Not a backend limit: it is the largest
/// search either client is sized to read back, and it lives here, off
/// every feature gate, so both clap declarations name the same number.
pub const MAX_SEARCH_TRACES: usize = 10_000;

/// Overrun remedies for [`MAX_SEARCH_TRACES`]-bounded clients, written
/// once: the search body shrinks with the flag, a single trace does not.
///
/// Gated with the two clients that read them, unlike `MAX_SEARCH_TRACES`
/// above, which the clap declarations name whether or not either feature is
/// on. Ungated they are dead code in the default core build, which is the one
/// CI checks for crates.io parity.
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
pub(crate) const SEARCH_OVERRUN_REMEDY: &str = "lower --max-traces";
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
pub(crate) const TRACE_OVERRUN_REMEDY: &str =
    "this single trace is larger than the cap, --max-traces cannot shrink it";

/// Give route templates the canonical path shape used by findings.
///
/// Some instrumentations emit `http.route` without its leading slash. Keep
/// legacy method-prefixed routes and full URLs intact; URL-like fallback
/// attributes are handled separately by each ingest adapter.
pub(crate) fn canonical_http_route(route: &str) -> String {
    if route.is_empty()
        || route.starts_with('/')
        || route.contains("://")
        || route.chars().any(char::is_whitespace)
    {
        route.to_string()
    } else {
        format!("/{route}")
    }
}

/// Resolve an `http.route`, preferring `url.path` only when the route is a
/// framework name rather than a path. A slash anywhere keeps slashless path
/// templates (for example Django's `api/orders/{id}`) authoritative.
pub(crate) fn http_route_endpoint(
    route: Option<&str>,
    url_path: Option<&str>,
    allow_url_path: bool,
) -> Option<String> {
    let route = route.filter(|route| !route.trim().is_empty())?;
    if allow_url_path
        && !route.contains('/')
        && let Some(path) = url_path.filter(|path| !path.trim().is_empty())
    {
        return Some(path.to_string());
    }
    Some(canonical_http_route(route))
}

/// `db.system` values for datastores whose `db.statement` is not
/// relational SQL and would be mangled by the SQL tokenizer (cache,
/// document, wide-column, graph, search, time-series stores).
///
/// Denylist by design: only values we are confident are non-SQL are
/// listed, so an unknown or absent `db.system` always stays SQL and no
/// SQL engine (postgresql, mysql, mssql, oracle, clickhouse, ...) is
/// ever dropped by mistake.
const NON_SQL_DB_SYSTEMS: &[&str] = &[
    "redis",
    "memcached",
    "mongodb",
    "cassandra",
    "dynamodb",
    "couchbase",
    "couchdb",
    "elasticsearch",
    "opensearch",
    "neo4j",
    "hbase",
    "geode",
    "influxdb",
];

/// What the tag-based gates (Jaeger, Zipkin) can classify a span as.
///
/// Deliberately narrower than [`EventType`]: those gates admit a span on
/// `db.statement` or on an HTTP target, never on `messaging.system`.
/// Matching on this instead of `EventType` makes adding a third case a
/// compile error in every downstream match, rather than a publish
/// silently inheriting the HTTP arm and being labelled `GET`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagIoKind {
    Sql,
    HttpOut,
}

impl TagIoKind {
    pub(crate) const fn event_type(self) -> crate::event::EventType {
        match self {
            Self::Sql => crate::event::EventType::Sql,
            Self::HttpOut => crate::event::EventType::HttpOut,
        }
    }
}

/// True when `db.system` names a known non-SQL datastore. Such spans
/// carry a `db.statement` that is not SQL, so they are dropped at
/// ingestion rather than fed to the SQL tokenizer (perf-sentinel does
/// not model non-SQL datastores). Case-insensitive, no allocation.
#[must_use]
pub(crate) fn is_non_sql_db_system(system: &str) -> bool {
    NON_SQL_DB_SYSTEMS
        .iter()
        .any(|s| system.eq_ignore_ascii_case(s))
}

/// Relational SQL systems perf-sentinel tokenizes. The dd-trace resource
/// fallback fires only for these, so an unrecognized `db.type` never has its
/// command string fed to the SQL tokenizer (fail closed against phantom SQL
/// findings and PII leakage from cache or document keys).
const SQL_DB_SYSTEMS: &[&str] = &[
    "postgresql",
    "mysql",
    "mariadb",
    "mssql",
    "oracle",
    "db2",
    "sqlite",
    "h2",
    "hsqldb",
    "derby",
    "cockroachdb",
    "clickhouse",
    "spanner",
    "redshift",
    "snowflake",
    "bigquery",
    "trino",
    "presto",
    "vertica",
    "teradata",
    "hive",
    "sql",
];

/// True when `system` names a relational SQL datastore. Case-insensitive.
#[must_use]
pub(crate) fn is_sql_db_system(system: &str) -> bool {
    SQL_DB_SYSTEMS
        .iter()
        .any(|s| system.eq_ignore_ascii_case(s))
}

/// One canonical token per database engine, so every ingestion path labels and
/// gates the same engine identically regardless of which vocabulary the upstream
/// used: dd-trace `db.type`, the stable `OTel` 1.27+ `db.system.name` (often
/// namespaced, e.g. `aws.dynamodb`), or the older experimental `db.system`.
/// Unknown values pass through unchanged.
#[must_use]
pub(crate) fn canonical_db_system(system: &str) -> &str {
    const ALIASES: &[(&str, &str)] = &[
        ("postgres", "postgresql"),
        ("sqlserver", "mssql"),
        ("sql server", "mssql"),
        ("microsoft.sql_server", "mssql"),
        ("oracle.db", "oracle"),
        ("ibm.db2", "db2"),
        ("gcp.spanner", "spanner"),
        ("aws.redshift", "redshift"),
        ("aws.dynamodb", "dynamodb"),
    ];
    for &(alias, canonical) in ALIASES {
        if system.eq_ignore_ascii_case(alias) {
            return canonical;
        }
    }
    system
}

/// Maximum parent hops for any ancestor walk, shared by the OTLP, Jaeger and
/// Zipkin paths. Java auto-instrumented stacks chain up to 8 layers (HTTP
/// server, Filter, `DispatcherServlet`, Controller, Service, Repository,
/// Hibernate, JDBC).
pub(crate) const ANCESTOR_WALK_MAX_DEPTH: usize = 8;

/// Qualification separators across the languages `code.*` covers: `.` (Java,
/// Python), `\` (PHP), `::` (Rust, C++), `#` (Ruby, javadoc).
const CODE_FRAME_SEPARATORS: [char; 4] = ['.', '\\', ':', '#'];

/// Namespace half of a fully-qualified `code.function.name`, shared by the
/// OTLP, Jaeger and Zipkin paths so they derive it identically.
///
/// The `\` fallback fires only when no `.` is present: PHP namespaces
/// (`Doctrine\DBAL\Driver\Connection::query`) carry no dot, dot-based
/// languages always do, and Rust `::`-only names have neither, so other
/// languages are unchanged.
#[must_use]
pub(crate) fn namespace_from_qualified_name(fq: &str) -> Option<&str> {
    fq.rsplit_once('.')
        .or_else(|| fq.rsplit_once('\\'))
        .map(|(ns, _)| ns)
}

/// Reject what must not become an endpoint: blanks, and control characters,
/// which `sanitize_span_event` already drops from the `code_*` fields and
/// which `source.endpoint` does not filter on its own.
fn usable_code_frame_part(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty() && !s.contains(char::is_control)).then_some(s)
}

/// Framework namespaces that never name an origin: on PHP the outermost
/// `code.*` frame is the framework's HTTP kernel, which collides in the ack
/// signature exactly as `"unknown"` did while looking resolved. Prefix match.
const FRAMEWORK_FRAME_PREFIXES: &[&str] = &[
    // PHP (lab-observed on symfony-svc, laravel-svc and the OTel demo)
    "Symfony\\",
    "Illuminate\\",
    "Laravel\\",
    "Doctrine\\",
    "Slim\\",
    "DI\\",
    "PDO::",
    "PDOStatement::",
    // Java servlet containers and DI/ORM infrastructure
    "org.springframework.",
    "org.apache.",
    "org.hibernate.",
    "org.eclipse.",
    "jakarta.",
    "javax.",
    "java.",
    "io.quarkus.",
    "io.vertx.",
    "io.helidon.",
    "io.netty.",
    "com.zaxxer.",
    // Ruby framework internals
    "ActiveRecord::",
    "ActionController::",
    "ActiveSupport::",
    "Rack::",
];

/// Endpoint fallback for entry points carrying no HTTP attribute: scheduled
/// jobs, message consumers. Without it they all report `"unknown"`, which
/// names no origin and collides in the ack signature.
///
/// `None` when the frame is unusable, is a bare function name, or belongs to
/// a framework kernel ([`FRAMEWORK_FRAME_PREFIXES`]): all collide like
/// `"unknown"`. `#` becomes `.` and `?`/`@` frames are rejected, because
/// `strip_endpoint_secrets` would truncate them into a colliding spelling.
#[must_use]
pub(crate) fn code_frame_endpoint(
    namespace: Option<&str>,
    function: Option<&str>,
) -> Option<String> {
    let frame = match (
        namespace.and_then(usable_code_frame_part),
        function.and_then(usable_code_frame_part),
    ) {
        // Stable `code.function.name` is already qualified and the namespace
        // was derived from it, so concatenating would repeat the prefix.
        (Some(ns), Some(f))
            if f.len() > ns.len()
                && f.starts_with(ns)
                && f[ns.len()..].starts_with(CODE_FRAME_SEPARATORS) =>
        {
            f.to_string()
        }
        // Join with the separator that attaches a function to this namespace,
        // so the legacy pair and the stable name spell one origin the same way.
        (Some(ns), Some(f)) => format!("{ns}{}{f}", frame_separator(ns)),
        (Some(ns), None) => ns.to_string(),
        (None, Some(f)) if f.contains(CODE_FRAME_SEPARATORS) => f.to_string(),
        _ => return None,
    };
    if FRAMEWORK_FRAME_PREFIXES
        .iter()
        .any(|p| frame.starts_with(p))
    {
        return None;
    }
    let frame = if frame.contains('#') {
        frame.replace('#', ".")
    } else {
        frame
    };
    (!frame.contains(['?', '@'])).then_some(frame)
}

/// Separator that attaches a function to this namespace: `::` for PHP and
/// Rust/C++, `.` otherwise. PHP qualifies namespaces with `\` but attaches
/// methods with `::` (`Slim\App::handle`), so a `\` namespace joins with `::`.
fn frame_separator(namespace: &str) -> &'static str {
    if namespace.contains('\\') || namespace.contains("::") {
        "::"
    } else {
        "."
    }
}

/// Trait for event ingestion sources.
/// Resolve the configured grouping attributes against one span, in config
/// order, skipping the absent ones. `keys` empty means the caller never
/// configured any and the built-in default applies, an explicitly empty
/// `[detection] grouping_attributes` turns grouping off instead.
///
/// `lookup` is the per-format attribute reader (OTLP resource + span
/// attributes, Jaeger process + span tags, Zipkin span tags).
pub(crate) fn collect_grouping(
    keys: Option<&[std::sync::Arc<str>]>,
    lookup: impl Fn(&str) -> Option<std::sync::Arc<str>>,
) -> Vec<crate::event::GroupingAttribute> {
    let mut out = Vec::new();
    match keys {
        Some(keys) => {
            for key in keys {
                if let Some(value) = lookup(key) {
                    out.push(crate::event::GroupingAttribute {
                        key: std::sync::Arc::clone(key),
                        value,
                    });
                }
            }
        }
        None => {
            for key in crate::config::DEFAULT_GROUPING_ATTRIBUTES {
                if let Some(value) = lookup(key) {
                    out.push(crate::event::GroupingAttribute {
                        key: std::sync::Arc::from(key),
                        value,
                    });
                }
            }
        }
    }
    out
}

pub trait IngestSource {
    /// Error type for this source.
    type Error: std::error::Error;

    /// Ingest events from the source and return them.
    ///
    /// # Errors
    ///
    /// Returns an error if the raw input cannot be parsed or exceeds size limits.
    fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::{
        NON_SQL_DB_SYSTEMS, SQL_DB_SYSTEMS, canonical_db_system, canonical_http_route,
        code_frame_endpoint, http_route_endpoint, is_non_sql_db_system, is_sql_db_system,
    };

    #[test]
    fn canonical_http_route_only_prefixes_slashless_templates() {
        assert_eq!(canonical_http_route("api/orders/{id}"), "/api/orders/{id}");
        for route in [
            "/api/orders/{id}",
            "POST /api/orders/{id}",
            "https://example.test/api/orders/{id}",
        ] {
            assert_eq!(canonical_http_route(route), route);
        }
    }

    #[test]
    fn named_http_route_uses_url_path_without_reclassifying_path_routes() {
        assert_eq!(
            http_route_endpoint(
                Some("app_fault_nplusonesql"),
                Some("/api/fault/n-plus-one-sql"),
                true,
            )
            .as_deref(),
            Some("/api/fault/n-plus-one-sql")
        );
        for route in [
            "api/fault/n-plus-one-sql",
            "/api/fault/{kind}",
            "POST /api/fault/{kind}",
            "https://example.test/api/fault/{kind}",
        ] {
            assert_eq!(
                http_route_endpoint(Some(route), Some("/wrong"), true),
                Some(canonical_http_route(route))
            );
        }
        assert_eq!(
            http_route_endpoint(Some("app_fault_nplusonesql"), None, true).as_deref(),
            Some("/app_fault_nplusonesql")
        );
        assert_eq!(
            http_route_endpoint(Some("outbound_payment"), Some("/v1/pay"), false,).as_deref(),
            Some("/outbound_payment")
        );
    }

    #[test]
    fn namespace_derivation_matches_each_language() {
        use super::namespace_from_qualified_name as ns;
        // Dot wins wherever one exists, `\` only serves PHP, and a name that
        // qualifies with neither has no namespace to derive. The PHP result is
        // a prefix of the qualified name, which `code_frame_endpoint` then
        // keeps whole rather than concatenating.
        assert_eq!(ns("com.foo.PurgeJob.execute"), Some("com.foo.PurgeJob"));
        assert_eq!(ns("Slim\\App::handle"), Some("Slim"));
        assert_eq!(
            code_frame_endpoint(ns("Slim\\App::handle"), Some("Slim\\App::handle")),
            None,
            "framework frame, denied whichever way the namespace derives"
        );
        assert_eq!(ns("myapp::worker::run"), None);
        assert_eq!(ns("execute"), None);
    }

    #[test]
    fn code_frame_endpoint_joins_legacy_pair() {
        assert_eq!(
            code_frame_endpoint(Some("com.foo.PurgeJob"), Some("execute")).as_deref(),
            Some("com.foo.PurgeJob.execute")
        );
    }

    #[test]
    fn code_frame_endpoint_does_not_repeat_a_qualified_name() {
        // The namespace is derived from the stable `code.function.name`, so it
        // is a prefix of it. Concatenating would print it twice. Covers every
        // separator, not just the dot: `::` and `#` reach here too.
        for (ns, f) in [
            ("com.foo.OrderService", "com.foo.OrderService.findItems"),
            ("App\\Jobs\\PurgeJob", "App\\Jobs\\PurgeJob::handle"),
            ("myapp::worker", "myapp::worker::run"),
        ] {
            assert_eq!(code_frame_endpoint(Some(ns), Some(f)).as_deref(), Some(f));
        }
    }

    #[test]
    fn code_frame_endpoint_joins_with_the_namespace_separator() {
        // The legacy pair and the stable qualified name describe one origin.
        // PHP attaches methods with `::`, never `\` (lab-refuted spelling).
        for (ns, f, expected) in [
            (
                "App\\Jobs\\PurgeJob",
                "handle",
                "App\\Jobs\\PurgeJob::handle",
            ),
            ("myapp::worker", "run", "myapp::worker::run"),
            ("com.foo.PurgeJob", "execute", "com.foo.PurgeJob.execute"),
        ] {
            assert_eq!(
                code_frame_endpoint(Some(ns), Some(f)).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn code_frame_endpoint_rejects_framework_kernel_frames() {
        // Real frames from the lab run: the outermost PHP frame is the HTTP
        // kernel, which collides like "unknown" while looking resolved.
        let cases = [
            (
                None,
                Some("Symfony\\Component\\HttpKernel\\HttpKernel::handle"),
            ),
            (None, Some("Illuminate\\Foundation\\Http\\Kernel::handle")),
            (None, Some("PDOStatement::execute")),
            (Some("Slim\\App"), Some("handle")),
            (
                Some("org.apache.catalina.core.StandardWrapper"),
                Some("invoke"),
            ),
        ];
        for (ns, f) in cases {
            assert_eq!(code_frame_endpoint(ns, f), None, "{ns:?} {f:?}");
        }
        // An application frame in the same languages still resolves.
        assert_eq!(
            code_frame_endpoint(Some("App\\Jobs\\PurgeJob"), Some("handle")).as_deref(),
            Some("App\\Jobs\\PurgeJob::handle")
        );
    }

    #[test]
    fn code_frame_endpoint_rejects_what_the_sanitizer_would_truncate() {
        // `strip_endpoint_secrets` truncates at '?' and strips userinfo before
        // the first '/', so these would reach the ack signature mangled and
        // collide with the plain spelling.
        for (ns, f) in [
            (Some("Order"), Some("valid?")),
            (Some("App\\Jobs"), Some("class@anonymous")),
        ] {
            assert_eq!(code_frame_endpoint(ns, f), None, "{ns:?} {f:?}");
        }
    }

    #[test]
    fn code_frame_endpoint_rewrites_hash_to_dot() {
        // `strip_endpoint_secrets` truncates at '#', which would drop the
        // method half of a Ruby-style or javadoc-style name.
        assert_eq!(
            code_frame_endpoint(None, Some("MyClass#method")).as_deref(),
            Some("MyClass.method")
        );
    }

    #[test]
    fn code_frame_endpoint_rejects_unusable_input() {
        // Each of these used to yield a misleading endpoint that collided in
        // the ack signature exactly as `"unknown"` did, or a malformed one.
        let cases = [
            (None, None, "nothing at all"),
            (Some(""), None, "blank namespace"),
            (Some(""), Some("execute"), "blank namespace, leading dot"),
            (Some("  "), Some(""), "whitespace only"),
            (None, Some("execute"), "bare function name, too generic"),
            (None, Some("run\u{1b}[2J"), "control characters"),
            (
                Some("com.foo\u{7}"),
                Some("run"),
                "control char in namespace",
            ),
        ];
        for (ns, f, desc) in cases {
            assert_eq!(code_frame_endpoint(ns, f), None, "{desc}");
        }
    }

    #[test]
    fn sql_and_non_sql_lists_are_disjoint() {
        // A db system cannot be both relational and non-relational. Overlap
        // would make classification order-dependent.
        for s in SQL_DB_SYSTEMS {
            assert!(!is_non_sql_db_system(s), "{s} is in both lists");
        }
        for s in NON_SQL_DB_SYSTEMS {
            assert!(!is_sql_db_system(s), "{s} is in both lists");
        }
    }

    #[test]
    fn canonical_aliases_resolve_to_a_classified_system() {
        // Every alias target must land in exactly one membership list, or the
        // alias silently breaks classification (zero findings, or a non-SQL
        // command tokenized as SQL with its key embedded in a finding).
        let alias_inputs = [
            "postgres",
            "sqlserver",
            "sql server",
            "microsoft.sql_server",
            "oracle.db",
            "ibm.db2",
            "gcp.spanner",
            "aws.redshift",
            "aws.dynamodb",
        ];
        for input in alias_inputs {
            let canonical = canonical_db_system(input);
            let sql = is_sql_db_system(canonical);
            let non_sql = is_non_sql_db_system(canonical);
            assert!(
                sql ^ non_sql,
                "{input} -> {canonical} classified as neither or both (sql={sql}, non_sql={non_sql})"
            );
        }
    }
}
