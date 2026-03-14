//! sentinel-core: core library for perf-sentinel.
//!
//! Provides the analysis pipeline for detecting performance anti-patterns
//! in runtime traces (SQL queries, HTTP calls).

pub mod config;
pub mod correlate;
pub mod detect;
pub mod event;
pub mod ingest;
pub mod normalize;
pub mod pipeline;
pub mod report;
pub mod score;
