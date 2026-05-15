//! Operational thresholds shared by multiple CLI subcommands.

/// Hard cap on a local `--report` file read. Legitimate periodic
/// disclosures are well under 10 MB; 64 MiB leaves room for outliers
/// (deep G1 archives, large per-service breakdowns) while bounding a
/// single `Vec<u8>` allocation against a poisoned mirror or a runaway
/// artefact feeding the binary multi-GB JSON.
pub const MAX_LOCAL_REPORT_BYTES: u64 = 64 * 1024 * 1024;
