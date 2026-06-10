//! sentinel-core: core library for perf-sentinel.
//!
//! Provides the analysis pipeline for detecting performance anti-patterns
//! in runtime traces (SQL queries, HTTP calls).

// This crate contains no `unsafe` code and is meant to stay that way: the
// analysis pipeline is pure data processing. `forbid` (not `deny`) so the
// guarantee cannot be locally overridden by an inner `allow`. The single
// `unsafe` FFI call in the workspace (libc::getrusage) lives in the CLI
// crate, where it is isolated and documented with its SAFETY invariants.
#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::cast_possible_truncation)] // u128 -> u64 for elapsed_ms
#![allow(clippy::cast_precision_loss)] // usize -> f64 for ratios
#![allow(clippy::similar_names)] // min_ts/min_ms, max_ts/max_ms are clear

pub mod acknowledgments;
pub mod calibrate;
pub mod config;
pub mod correlate;
#[cfg(feature = "daemon")]
pub mod daemon;
pub mod detect;
pub mod diff;
pub mod event;
pub mod explain;
#[cfg(any(feature = "daemon", feature = "tempo"))]
pub mod http_client;
pub mod ingest;
pub mod normalize;
pub mod pipeline;
pub mod quality_gate;
pub mod report;
pub mod score;
#[cfg(any(feature = "daemon", feature = "tempo"))]
pub(crate) mod shutdown;
// Hidden: benchmark and fixture tooling, not a supported library API.
#[doc(hidden)]
pub mod synth;
pub mod text_safety;
pub(crate) mod time;

#[cfg(test)]
pub(crate) mod test_helpers;

// Re-export the interpretation helper so the CLI and downstream consumers
// can write `sentinel_core::InterpretationLevel::for_iis(...)` without
// having to know it lives under `report::interpret::`.
pub use report::interpret::InterpretationLevel;

// Re-export the daemon error types for consistency with `InterpretationLevel`.
// Downstream consumers (the CLI, any library user) can now write
// `sentinel_core::DaemonError` / `sentinel_core::TlsConfigError` without
// having to know the module structure. Gated on `daemon` since the daemon
// module itself is gated.
#[cfg(feature = "daemon")]
pub use daemon::{DaemonError, TlsConfigError};
