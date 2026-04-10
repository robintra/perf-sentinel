//! sentinel-core: core library for perf-sentinel.
//!
//! Provides the analysis pipeline for detecting performance anti-patterns
//! in runtime traces (SQL queries, HTTP calls).

#![warn(clippy::pedantic)]
#![allow(clippy::cast_possible_truncation)] // u128 -> u64 for elapsed_ms
#![allow(clippy::cast_precision_loss)] // usize -> f64 for ratios
#![allow(clippy::similar_names)] // min_ts/min_ms, max_ts/max_ms are clear

pub mod config;
pub mod correlate;
#[cfg(feature = "daemon")]
pub mod daemon;
pub mod detect;
pub mod event;
pub mod explain;
pub mod ingest;
pub mod normalize;
pub mod pipeline;
pub mod quality_gate;
pub mod report;
pub mod score;
pub(crate) mod time;

#[cfg(test)]
pub(crate) mod test_helpers;
