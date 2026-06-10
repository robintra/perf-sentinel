//! Operational thresholds shared by multiple CLI subcommands.

/// Hard cap on a local `--report` file read. Legitimate periodic
/// disclosures are well under 10 MB; 64 MiB leaves room for outliers
/// (deep G1 archives, large per-service breakdowns) while bounding a
/// single `Vec<u8>` allocation against a poisoned mirror or a runaway
/// artefact feeding the binary multi-GB JSON.
pub const MAX_LOCAL_REPORT_BYTES: u64 = 64 * 1024 * 1024;

/// Hard cap on a local trace-file read for the batch subcommands
/// (`analyze`, `diff`, `report`, `explain`, `calibrate`, `bench`,
/// `pg-stat`). Local files the operator passes explicitly are a
/// different trust model from the daemon's network listeners, so this
/// is deliberately decoupled from `[daemon] max_payload_size` (whose
/// 100 MiB ceiling exists to bound unauthenticated OTLP requests).
/// 1 GiB bounds the single `Vec<u8>` allocation while letting real
/// large-fleet exports (hundreds of MB of traces) load. The whole
/// parsed dataset must fit in RAM anyway, batch correlation is
/// whole-set by design.
pub const MAX_BATCH_INPUT_BYTES: usize = 1024 * 1024 * 1024;
