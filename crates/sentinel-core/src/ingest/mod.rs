//! Ingestion stage: reads raw events from various sources.

#[cfg(any(feature = "daemon", feature = "tempo", feature = "jaeger-query"))]
pub mod auth_header;
pub mod jaeger;
#[cfg(feature = "jaeger-query")]
pub mod jaeger_query;
pub mod json;
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
pub mod lookback;
pub mod otlp;
pub mod pg_stat;
#[cfg(feature = "tempo")]
pub mod tempo;
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
pub(crate) mod url_enc;
pub mod zipkin;

use crate::event::SpanEvent;

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

/// Trait for event ingestion sources.
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
