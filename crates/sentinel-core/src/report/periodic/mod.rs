//! Periodic disclosure report (v1.0).
//!
//! A separate JSON document type, distinct from the per-batch [`Report`]
//! tree, aggregated over a calendar period (default quarter). Designed
//! for public transparency: structured, sourced, hash-verifiable.
//!
//! See `docs/REPORTING.md` for the operator-facing guide and
//! `docs/schemas/perf-sentinel-report-v1.json` for the wire schema.
//!
//! [`Report`]: crate::report::Report

pub mod aggregator;
pub mod errors;
pub mod hasher;
pub mod org_config;
pub mod schema;
pub mod validator;

pub use aggregator::{
    AggregateInputs, AntiPatternAccumulator, ServiceAccumulator, UNATTRIBUTED_SERVICE,
    aggregate_from_paths,
};
pub use errors::{AggregationError, HashError, ValidationError};
pub use hasher::{binary_hash, compute_content_hash};
pub use schema::{
    Aggregate, AntiPatternDetail, Application, ApplicationG1, ApplicationG2, CalibrationInputs,
    Confidentiality, Conformance, DisabledPattern, ExcludedApp, ExcludedEnv, Integrity,
    IntegrityLevel, Methodology, Notes, OrgIdentifiers, Organisation, Period, PeriodType,
    PeriodicReport, ReportIntent, ReportMetadata, SCHEMA_VERSION, ScopeManifest,
    core_patterns_required,
};
pub use validator::{validate_content_hash, validate_official};
