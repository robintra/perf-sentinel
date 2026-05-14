//! Error types for the periodic disclosure report pipeline.

use thiserror::Error;

#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum ValidationError {
    #[error("organisation.{field}: {reason}")]
    Organisation { field: &'static str, reason: String },
    #[error("scope_manifest.{field}: {reason}")]
    ScopeManifest { field: &'static str, reason: String },
    #[error("methodology.{field}: {reason}")]
    Methodology { field: &'static str, reason: String },
    #[error("aggregate.{field}: {reason}")]
    Aggregate { field: &'static str, reason: String },
    #[error("period: {0}")]
    Period(String),
    #[error("applications: {0}")]
    Applications(String),
    #[error("intent 'audited' is not yet implemented")]
    AuditedNotImplemented,
    #[error("integrity.content_hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HashError {
    #[error("failed to serialize report for hashing: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AggregationError {
    #[error("input path is not a file or directory: {0}")]
    InvalidInput(String),
    #[error("failed to read input file {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("no archived reports fell within the requested period")]
    NoWindowsInPeriod,
    #[error("strict attribution requested but window at {ts} has no per-service offenders")]
    UnattributedWindow { ts: String },
}
