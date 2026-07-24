//! End-to-end integration tests for the perf-sentinel CLI.
//!
//! Single test binary; tests live in per-topic modules and share the
//! helpers module below.

mod helpers;

mod ack;
mod analyze;
mod demo;
mod diff;
mod disclose;
mod explain;
mod jaeger_query;
mod mysql_stat;
mod pg_stat;
mod report;
mod tempo;
mod tui;
mod watch;
