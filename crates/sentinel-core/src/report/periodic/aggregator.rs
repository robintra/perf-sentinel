//! Fold archived per-window [`Report`] envelopes into a
//! [`PeriodicReport`] builder. Wire format and per-service attribution
//! policy: `docs/design/08-PERIODIC-DISCLOSURE.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;

use crate::detect::Finding;
use crate::report::Report;
use crate::score::carbon::ENERGY_PER_IO_OP_KWH;

use super::errors::AggregationError;
use super::schema::{
    Aggregate, CarbonBreakdown, DatabaseWasteAggregate, Period, TemporalCoverage, WasteTier,
};

pub const UNATTRIBUTED_SERVICE: &str = "_unattributed";

/// Cardinality cap on services tracked by the aggregator. Caps the
/// `Builder.per_service` map so that a tampered archive carrying an
/// unbounded number of distinct service strings cannot exhaust memory.
/// Overflow is folded into `UNATTRIBUTED_SERVICE`.
const MAX_SERVICES: usize = 4096;

/// Cardinality cap on distinct `energy_model` strings tracked in
/// `Builder.energy_source_models`. Overflow entries are silently dropped.
const MAX_ENERGY_MODELS: usize = 64;

/// Per-string length cap for `energy_model` entries collected from
/// archive lines. Longer values are rejected (dropped, never inserted).
const MAX_ENERGY_MODEL_LEN: usize = super::schema::MODEL_TAG_MAX_LEN;

/// Cardinality cap on distinct `binary_version` strings tracked in
/// `Builder.binary_versions`. Overflow entries are silently dropped.
/// Sized for multi-team async-release environments where a quarter can
/// span more than a dozen patch versions; 256 × 64 bytes = 16 KB worst
/// case, negligible memory budget.
const MAX_BINARY_VERSIONS: usize = 256;

/// Per-string length cap on `binary_version` entries.
const MAX_BINARY_VERSION_LEN: usize = 64;

/// Matches the JSON Schema pattern `^[A-Za-z0-9._+-]+$` for `binary_version`
/// without pulling in a regex. Rejects empty input and any byte outside the
/// allowed alphabet so a tampered archive cannot inject control chars or
/// arbitrary UTF-8 into the periodic report.
fn is_valid_binary_version(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

#[derive(Debug, Default)]
pub struct AggregateInputs {
    pub aggregate: Aggregate,
    pub per_service: BTreeMap<String, ServiceAccumulator>,
    pub windows_aggregated: u64,
    pub source_files: Vec<String>,
    pub malformed_lines_skipped: u64,
    /// Windows carrying no `disclosure_waste`, archived before canonical
    /// disclosure. Their waste fed the operational tier only, so the
    /// canonical tier omits those windows.
    pub legacy_waste_windows: u64,
    pub first_seen: BTreeMap<(String, String), DateTime<Utc>>,
    pub last_seen: BTreeMap<(String, String), DateTime<Utc>>,
    /// Distinct `energy_model` tags (without `+cal` suffix) observed
    /// across the folded windows. Empty when every window predates
    /// per-service carbon attribution.
    pub energy_source_models: BTreeSet<String>,
    /// Number of windows that carried runtime-calibrated per-service
    /// data. Together with `fallback_windows`, surfaces the share of
    /// the period that benefits from runtime attribution vs. the proxy.
    pub runtime_windows: u64,
    /// Number of windows that fell back to the I/O proxy path. Each
    /// archive file emits at most one `tracing::warn!` when its first
    /// fallback window is folded.
    pub fallback_windows: u64,
    /// `true` if at least one folded window carried a `+cal` suffix on
    /// its `energy_model`. Surfaced via `CalibrationInputs.calibration_applied`.
    pub calibration_applied: bool,
    /// Archive integrity: windows whose chain verified, windows written
    /// before chaining existed, and detected breaks. Published so a
    /// reader sees which part of the period is still attestable.
    pub chain_verified: u64,
    pub chain_unchained: u64,
    pub chain_breaks: u64,
    /// Breaks in the same files but outside the period. One rolling
    /// archive can cover several periods, and this report only answers
    /// for its own, so they are counted apart rather than folded in.
    pub chain_breaks_outside: u64,
    /// Windows the daemon produced but could not archive, derived from
    /// the cumulative `drops` counter on the archive lines. `None` when
    /// no line carried the counter (pre-v1.7 archives).
    pub windows_dropped: Option<u64>,
    /// Times the drop counter went backwards (daemon restarts). Each
    /// makes `windows_dropped` a lower bound over the gap it spans.
    pub drop_counter_resets: Option<u64>,
    /// Coefficient sets observed over the period, as `key=value` strings.
    pub scoring_coefficients: BTreeSet<String>,
    /// SCI methodology tags observed. Current windows use the `+transport`
    /// variant. The legacy tag can remain when a period spans older windows.
    pub carbon_methodologies: BTreeSet<String>,
    /// The three terms whose sum is the published total, in gCO2eq.
    /// Only `operational` carries an avoidable share: embodied hardware
    /// and network transport are irreducible by fixing an anti-pattern.
    pub embodied_gco2_total: f64,
    pub operational_gco2_total: f64,
    pub transport_gco2_total: f64,
}

#[derive(Debug, Default, Clone)]
pub struct ServiceAccumulator {
    pub total_requests: u64,
    pub total_io_ops: u64,
    pub energy_kwh: f64,
    pub carbon_kgco2eq: f64,
    pub anti_patterns: BTreeMap<String, AntiPatternAccumulator>,
    pub endpoints_seen: BTreeSet<String>,
}

#[derive(Debug, Default, Clone)]
pub struct AntiPatternAccumulator {
    pub occurrences: u64,
    /// Estimated avoidable I/O ops attributed to this pattern. For
    /// avoidable types (`n_plus_one_*`, `redundant_*`), sums
    /// `pattern.occurrences - 1` across findings, zero for non-avoidable
    /// types. Drives both per-service efficiency and the per-pattern
    /// `estimated_waste_*` values surfaced by `disclose`.
    pub avoidable_io_ops: u64,
}

#[derive(Debug, Deserialize)]
struct ArchivedReport {
    ts: DateTime<Utc>,
    report: Report,
}

/// Walk `paths` (files and/or directories), fold every in-period
/// archived report into a single [`AggregateInputs`].
///
/// # Errors
///
/// - [`AggregationError::InvalidInput`] if a path is neither a file nor
///   a directory.
/// - [`AggregationError::Io`] on read errors.
/// - [`AggregationError::NoWindowsInPeriod`] if zero archived windows
///   fall inside `period`.
/// - [`AggregationError::UnattributedWindow`] when `strict_attribution`
///   is set and a window has no per-service offenders.
pub fn aggregate_from_paths(
    paths: &[PathBuf],
    period: &Period,
    strict_attribution: bool,
) -> Result<AggregateInputs, AggregationError> {
    let files = resolve_files(paths)?;
    let source_files: Vec<String> = files
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();

    let mut builder = Builder::default();
    for path in &files {
        builder.process_file(path, period, strict_attribution)?;
    }

    if builder.windows_aggregated == 0 {
        return Err(AggregationError::NoWindowsInPeriod);
    }

    Ok(builder.finalize(source_files, period))
}

/// Inclusive `(earliest, latest)` window timestamp covered by an archive.
pub type ArchiveTimeRange = (DateTime<Utc>, DateTime<Utc>);

/// Scan the archive `paths` for the earliest and latest window timestamp,
/// without folding the (heavy) report bodies. Each NDJSON line is parsed
/// for its `ts` field only. Returns `None` when no parseable window is
/// found. Used by the interactive `disclose --tui` preview to pick a
/// sensible default period and show the archive's covered range; the
/// canonical aggregation stays in [`aggregate_from_paths`].
///
/// # Errors
///
/// Same path-resolution and I/O errors as [`aggregate_from_paths`].
pub fn archive_time_range(paths: &[PathBuf]) -> Result<Option<ArchiveTimeRange>, AggregationError> {
    #[derive(Deserialize)]
    struct TsOnly {
        ts: DateTime<Utc>,
    }
    let mut range: Option<ArchiveTimeRange> = None;
    for path in &resolve_files(paths)? {
        let file = File::open(path).map_err(|source| AggregationError::Io {
            path: path.display().to_string(),
            source,
        })?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|source| AggregationError::Io {
                path: path.display().to_string(),
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Malformed lines are silently skipped here (diagnostics are
            // the aggregation path's job); we only need the time bounds.
            if let Ok(TsOnly { ts }) = serde_json::from_str::<TsOnly>(trimmed) {
                range = Some(match range {
                    None => (ts, ts),
                    Some((lo, hi)) => (lo.min(ts), hi.max(ts)),
                });
            }
        }
    }
    Ok(range)
}

/// Per-window scalars extracted up front so `process_window` and its
/// helpers can pass a single value around instead of re-reading the
/// `Report` everywhere. Fields are derived from `green_summary` and
/// `analysis.traces_analyzed` only, never mutated downstream.
struct WindowMetrics {
    carbon_kg: f64,
    avoidable_kg: f64,
    total_io: u64,
    avoidable_io: u64,
    traces: u64,
    energy_kwh: f64,
    runtime_attribution: bool,
}

/// Period-summed avoidable energy/carbon for one N+1 threshold tier.
/// `threshold` reconciled by `max` across windows. `avoidable_kg` in kg.
#[derive(Default)]
struct WasteTierAccumulator {
    n_plus_one_threshold: u32,
    avoidable_io_ops: u64,
    avoidable_kwh: f64,
    avoidable_kg: f64,
}

/// Fold one window's waste block into a running accumulator. An
/// out-of-spec provenance tag drops the whole block: a figure whose
/// provenance cannot be published must not reach the sums either.
fn fold_waste_block(acc: &mut DbWasteAccumulator, block: &crate::report::DisclosureDbWaste) {
    let crate::report::DisclosureDbWaste {
        model,
        energy_kwh,
        operational_waste_kwh: operational_kwh,
        operational_waste_gco2: operational_gco2,
        canonical_waste_kwh: canonical_kwh,
        canonical_waste_gco2: canonical_gco2,
        energy_gco2,
    } = block;
    let (energy_kwh, operational_kwh, canonical_kwh) =
        (*energy_kwh, *operational_kwh, *canonical_kwh);
    let (operational_gco2, canonical_gco2, energy_gco2) =
        (*operational_gco2, *canonical_gco2, *energy_gco2);
    if !super::schema::is_valid_model_tag(model) {
        return;
    }
    let energy = sanitize_f64(energy_kwh);
    acc.energy_kwh += energy;
    acc.operational_kwh += sanitize_f64(operational_kwh);
    acc.canonical_kwh += sanitize_f64(canonical_kwh);
    // Keep None-vs-zero: sums stay None until a window actually carried
    // a carbon conversion.
    if let Some(g) = operational_gco2 {
        acc.operational_g = Some(acc.operational_g.unwrap_or(0.0) + sanitize_f64(g));
    }
    if let Some(g) = canonical_gco2 {
        acc.canonical_g = Some(acc.canonical_g.unwrap_or(0.0) + sanitize_f64(g));
    }
    // Estimated subsystem carbon is already attributed inside the service
    // total. Only measured or declared external-scope figures sit beside it.
    if model != crate::report::DB_WASTE_MODEL_ESTIMATED
        && let Some(g) = energy_gco2
    {
        acc.energy_g = Some(acc.energy_g.unwrap_or(0.0) + sanitize_f64(g));
    }
    acc.windows = acc.windows.saturating_add(1);
    // Three provenance classes, three buckets, see
    // `docs/design/08-PERIODIC-DISCLOSURE.md`.
    if model == crate::report::DB_WASTE_MODEL_ESTIMATED {
        acc.estimated_windows = acc.estimated_windows.saturating_add(1);
    } else if model == crate::report::BROKER_WASTE_MODEL_SPECPOWER {
        acc.declared_windows = acc.declared_windows.saturating_add(1);
        acc.declared_energy_kwh += energy;
    } else {
        acc.measured_windows = acc.measured_windows.saturating_add(1);
        acc.measured_energy_kwh += energy;
    }
    if operational_gco2.is_some() || canonical_gco2.is_some() {
        acc.windows_with_carbon = acc.windows_with_carbon.saturating_add(1);
    }
    // Same cap as the sibling energy-model collector.
    if acc.models.len() < MAX_BINARY_VERSIONS || acc.models.contains(model) {
        acc.models.insert(model.clone());
    }
}

#[derive(Default)]
struct DbWasteAccumulator {
    energy_kwh: f64,
    measured_energy_kwh: f64,
    operational_kwh: f64,
    /// `None` until a window carried a carbon conversion, so an absent
    /// conversion is not published as an affirmative zero.
    operational_g: Option<f64>,
    canonical_kwh: f64,
    canonical_g: Option<f64>,
    /// Total carbon of the subsystem, beside the totals and never in them.
    energy_g: Option<f64>,
    models: BTreeSet<String>,
    windows: u64,
    measured_windows: u64,
    declared_energy_kwh: f64,
    declared_windows: u64,
    estimated_windows: u64,
    windows_with_carbon: u64,
}

#[derive(Default)]
struct Builder {
    per_service: BTreeMap<String, ServiceAccumulator>,
    windows_aggregated: u64,
    malformed_lines_skipped: u64,
    legacy_waste_windows: u64,
    first_seen: BTreeMap<(String, String), DateTime<Utc>>,
    last_seen: BTreeMap<(String, String), DateTime<Utc>>,
    total_requests: u64,
    total_io_ops: u64,
    total_carbon_kgco2eq: f64,
    /// Avoidable tiers from each window's `Report.disclosure_waste`.
    canonical_waste: WasteTierAccumulator,
    operational_waste: WasteTierAccumulator,
    /// Database-waste sums from each window's `disclosure_waste.database`.
    /// Windows predating the block are not folded (no canonical figure),
    /// so both tiers stay consistent.
    db_waste: DbWasteAccumulator,
    msg_waste: DbWasteAccumulator,
    /// Sum of runtime-calibrated `energy_kwh` for windows that carry it.
    runtime_energy_kwh: f64,
    /// Distinct energy model strings collected across all windows. The
    /// `+cal` suffix is stripped so consumers see the bare source tag.
    energy_source_models: BTreeSet<String>,
    /// Windows that carried `green_summary.energy_kwh > 0` or non-empty
    /// per-service runtime maps.
    runtime_windows: u64,
    /// Windows that fell back to the I/O proxy path. Used by tests and
    /// surfaced via [`AggregateInputs`] for operator diagnostics.
    fallback_windows: u64,
    /// Distinct `binary_version` values observed across the folded
    /// windows. Empty when every window predates the field.
    binary_versions: BTreeSet<String>,
    /// Set when at least one window's `energy_model` carried the `+cal`
    /// suffix, indicating operator calibration was active for that window.
    calibration_applied: bool,
    /// Archive lines whose hash chain checked out, carried no chain at
    /// all (pre-chaining archives), or failed to verify.
    chain_verified: u64,
    chain_unchained: u64,
    chain_breaks: u64,
    chain_breaks_outside: u64,
    /// Sum of in-period deltas of the cumulative `drops` counter, and
    /// whether any line carried it at all (pre-v1.7 archives carry none,
    /// and `0` must stay distinguishable from "not measured").
    windows_dropped: u64,
    drop_counter_resets: u64,
    drops_observed: bool,
    /// SCI methodology tags observed, bounded by `MAX_ENERGY_MODELS`.
    carbon_methodologies: BTreeSet<String>,
    /// Distinct coefficient sets observed, as `"key=value"` strings. A
    /// set that changed mid-period yields more than one entry.
    scoring_coefficients: BTreeSet<String>,
    /// Set once a window contributed transport under a coefficient that
    /// is not the fixed one, or under none this binary can read.
    transport_coefficient_uncertain: bool,
    /// Running sums of the three terms of the published total, in gCO2eq.
    embodied_gco2_total: f64,
    operational_gco2_total: f64,
    transport_gco2_total: f64,
    /// Per-service set of distinct energy model tags accumulated across
    /// the period's windows. The `+cal` suffix is stripped before
    /// insertion. Service cardinality is bounded by `MAX_SERVICES`,
    /// each inner set by `MAX_ENERGY_MODELS`.
    per_service_energy_models: BTreeMap<String, BTreeSet<String>>,
    /// Sum and count of per-window `per_service_measured_ratio` values,
    /// keyed by service. Finalized to a per-service mean in `finalize`.
    per_service_measured_ratio_sums: BTreeMap<String, (f64, u32)>,
    /// Distinct UTC calendar days that carried >= 1 folded window. Bounded
    /// by the period length (<= 366 for a calendar year), no cap needed.
    /// Drives the v1.2 temporal-coverage continuity signal.
    observed_days: BTreeSet<NaiveDate>,
}

/// The chain anchor: the previous line's hash and its sequence number.
type ChainAnchor = Option<(String, u64)>;

/// One line's worth of chain state, threaded through [`Builder::walk_chain_line`].
struct ChainStep<'a> {
    parsed: Option<&'a mut serde_json::Value>,
    expected: &'a mut ChainAnchor,
    chain_started: &'a mut bool,
    in_scope: bool,
    previous_in_scope: bool,
    next_seq: &'a dyn Fn(&ChainAnchor) -> u64,
    path: &'a Path,
    line_no: usize,
    warned_break: &'a mut bool,
}

impl Builder {
    /// Advance the integrity chain by one archive line. Split out of
    /// `process_file` so that loop stays under the complexity gate.
    fn walk_chain_line(&mut self, step: ChainStep<'_>) {
        // An unparseable line is a crash-truncated fragment, not an edit.
        // The anchor is kept, a destroyed window still surfaces as a break
        // through its successor's `prev`.
        let outcome = step.parsed.map_or(ChainOutcome::Malformed, |value| {
            verify_chain_value(value, step.expected.as_ref())
        });
        match outcome {
            // The typed fold counts it under malformed_lines_skipped.
            ChainOutcome::Malformed => {}
            ChainOutcome::Verified(hash) => {
                *step.chain_started = true;
                if step.in_scope {
                    self.chain_verified += 1;
                }
                let seq = (step.next_seq)(step.expected);
                *step.expected = Some((hash, seq));
            }
            // Unchained is benign only before the file's chain starts:
            // those lines predate chaining. Once a line has verified, a
            // later one without a `hash` is a field that was removed,
            // which is exactly the edit the chain exists to catch.
            ChainOutcome::Unchained if !*step.chain_started => {
                if step.in_scope {
                    self.chain_unchained += 1;
                }
            }
            ChainOutcome::Unchained => {
                self.count_break(step.in_scope || step.previous_in_scope);
                // No hash to chain onto, so the anchor is dropped and the
                // next chained line re-establishes it.
                *step.expected = None;
                warn_break(step.path, step.line_no, step.warned_break);
            }
            ChainOutcome::Break(hash) => {
                *step.chain_started = true;
                // The current line reveals a removed predecessor. If that
                // predecessor was the last in-period line, the break
                // affects this report even when the revealing line itself
                // is just outside the boundary.
                self.count_break(step.in_scope || step.previous_in_scope);
                // Resynchronise on this line's own hash and seq, so one
                // edit reports one break rather than poisoning the tail.
                let seq = (step.next_seq)(step.expected);
                *step.expected = Some((hash, seq));
                warn_break(step.path, step.line_no, step.warned_break);
            }
        }
    }
}

impl Builder {
    fn process_file(
        &mut self,
        path: &Path,
        period: &Period,
        strict: bool,
    ) -> Result<(), AggregationError> {
        let file = File::open(path).map_err(|source| AggregationError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut warned_fallback = false;
        // Chain state is per file: a rotated file restarts from the seed.
        // `None` means the walk lost its anchor and adopts whatever the
        // next chained line declares, so one damaged line costs one break.
        let mut expected: Option<(String, u64)> =
            Some((super::hasher::ARCHIVE_CHAIN_SEED.to_string(), 0));
        let mut warned_break = false;
        let mut chain_started = false;
        let mut previous_in_scope = false;
        let mut last_drops: Option<u64> = None;
        for (line_no, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| AggregationError::Io {
                path: path.display().to_string(),
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Parsed once: the chain check and the typed fold share this
            // value, instead of running two full JSON parses per line.
            let mut parsed: Option<serde_json::Value> = serde_json::from_str(trimmed).ok();
            let parsed_seq = parsed
                .as_ref()
                .and_then(|v| v.get("seq"))
                .and_then(serde_json::Value::as_u64);
            // Every line is walked, including those outside the period, or
            // an edit just outside the window would go unseen. What the
            // line's own timestamp decides is which counter it lands in:
            // one rolling archive can span several periods, and a 2024
            // edit must not be published as this quarter's break.
            let in_scope = line_in_period(parsed.as_ref(), period);
            if let Some(drops) = parsed
                .as_ref()
                .and_then(|v| v.get("drops"))
                .and_then(serde_json::Value::as_u64)
            {
                self.fold_drops(drops, &mut last_drops, in_scope);
            }
            let next_seq = |expected: &Option<(String, u64)>| {
                parsed_seq
                    .unwrap_or_else(|| expected.as_ref().map_or(0, |(_, seq)| *seq))
                    .saturating_add(1)
            };
            self.walk_chain_line(ChainStep {
                parsed: parsed.as_mut(),
                expected: &mut expected,
                chain_started: &mut chain_started,
                in_scope,
                previous_in_scope,
                next_seq: &next_seq,
                path,
                line_no,
                warned_break: &mut warned_break,
            });
            if parsed.is_some() {
                previous_in_scope = in_scope;
            }
            let typed = parsed.map_or_else(
                || serde_json::from_str::<ArchivedReport>(trimmed),
                serde_json::from_value::<ArchivedReport>,
            );
            match typed {
                Ok(envelope) => {
                    if !in_period(envelope.ts, period) {
                        continue;
                    }
                    let used_fallback = self.process_window(envelope, strict)?;
                    if used_fallback && !warned_fallback {
                        warned_fallback = true;
                        tracing::warn!(
                            path = %path.display(),
                            "archive predates per-service carbon attribution; \
                             falling back to I/O share proxy for this file",
                        );
                    }
                }
                Err(err) => {
                    self.malformed_lines_skipped += 1;
                    tracing::warn!(
                        path = %path.display(),
                        line = line_no + 1,
                        error = %err,
                        "skipping malformed archive line",
                    );
                }
            }
        }
        Ok(())
    }

    /// Fold one line's cumulative `drops` counter. The delta between two
    /// consecutive carrying lines is the loss between them, attributed to
    /// the later line's period. A decrease is a daemon restart: the count
    /// restarts from the new value (its drops happened after the restart)
    /// and the reset is surfaced so the figure reads as a lower bound.
    /// The first carrying line of a file sets the baseline without
    /// contributing: the counter is daemon-lifetime, not file-lifetime,
    /// so its absolute value says nothing about this file.
    fn fold_drops(&mut self, drops: u64, last: &mut Option<u64>, in_scope: bool) {
        self.drops_observed = true;
        match *last {
            Some(prev) if drops < prev && in_scope => {
                self.drop_counter_resets += 1;
                self.windows_dropped = self.windows_dropped.saturating_add(drops);
            }
            Some(prev) if drops >= prev && in_scope => {
                self.windows_dropped = self.windows_dropped.saturating_add(drops - prev);
            }
            Some(_) | None => {}
        }
        *last = Some(drops);
    }

    fn count_break(&mut self, in_scope: bool) {
        if in_scope {
            self.chain_breaks += 1;
        } else {
            self.chain_breaks_outside += 1;
        }
    }

    fn process_window(
        &mut self,
        envelope: ArchivedReport,
        strict: bool,
    ) -> Result<bool, AggregationError> {
        let ts = envelope.ts;
        let report = envelope.report;

        let Some(m) = self.compute_window_metrics(&report, ts) else {
            return Ok(false);
        };

        self.fold_global_counters(&m);
        // Count the day only once the window is committed (after the
        // non-finite-carbon guard), keeping observed_days aligned with
        // windows_aggregated.
        self.observed_days.insert(ts.date_naive());
        self.fold_disclosure_waste(&report, &m);
        self.fold_binary_version(&report.binary_version);
        self.fold_window_energy_model(&report.green_summary.energy_model);
        self.fold_carbon_methodology(report.green_summary.co2.as_ref());
        self.fold_transport_coefficient(
            report.green_summary.co2.as_ref(),
            report.green_summary.scoring_config.as_ref(),
        );
        self.fold_scoring_coefficients(report.green_summary.scoring_config.as_ref());
        self.fold_per_service_measured_ratio(&report.green_summary.per_service_measured_ratio);
        self.fold_per_service_energy_models(&report.green_summary.per_service_energy_model);

        let per_service_io = service_io_distribution(&report.per_endpoint_io_ops);
        let unattributed = per_service_io.is_empty() && !m.runtime_attribution;
        if unattributed && strict {
            return Err(AggregationError::UnattributedWindow {
                ts: ts.to_rfc3339(),
            });
        }

        self.attribute_window(&report, &m, &per_service_io, unattributed);
        self.route_findings(&report.findings, ts, unattributed);

        Ok(!m.runtime_attribution)
    }

    /// Validate, then capture the per-window scalars the rest of
    /// `process_window` needs. Returns `None` (and bumps the malformed
    /// counter) when the carbon fields are non-finite, signalling the
    /// caller to skip the window.
    fn compute_window_metrics(
        &mut self,
        report: &Report,
        ts: DateTime<Utc>,
    ) -> Option<WindowMetrics> {
        let carbon_kg = report
            .green_summary
            .co2
            .as_ref()
            .map_or(0.0, |c| c.total.mid / 1000.0);
        let avoidable_kg = report
            .green_summary
            .co2
            .as_ref()
            .map_or(0.0, |c| c.avoidable.mid / 1000.0);
        if !carbon_kg.is_finite() || !avoidable_kg.is_finite() {
            self.malformed_lines_skipped += 1;
            tracing::warn!(ts = %ts, "skipping window with non-finite carbon");
            return None;
        }
        // Sanitize against `+Inf` from tampered archives. NaN / -Inf /
        // negative inputs fall through the `> 0.0` check to the proxy
        // path; the post-clamp catches the remaining `+Inf` case.
        let raw_energy = if report.green_summary.energy_kwh > 0.0 {
            report.green_summary.energy_kwh
        } else {
            (report.green_summary.total_io_ops as f64) * ENERGY_PER_IO_OP_KWH
        };
        Some(WindowMetrics {
            carbon_kg,
            avoidable_kg,
            total_io: report.green_summary.total_io_ops as u64,
            avoidable_io: report.green_summary.avoidable_io_ops as u64,
            traces: report.analysis.traces_analyzed as u64,
            energy_kwh: sanitize_f64(raw_energy),
            runtime_attribution: !report.green_summary.per_service_carbon_kgco2eq.is_empty()
                && !report.green_summary.per_service_energy_kwh.is_empty(),
        })
    }

    fn fold_global_counters(&mut self, m: &WindowMetrics) {
        self.windows_aggregated += 1;
        self.total_requests = self.total_requests.saturating_add(m.traces);
        self.total_io_ops = self.total_io_ops.saturating_add(m.total_io);
        self.total_carbon_kgco2eq += m.carbon_kg;
        self.runtime_energy_kwh += m.energy_kwh;
    }

    /// Accumulate the canonical and operational avoidable tiers. A legacy
    /// archive (no `disclosure_waste`) has no canonical figure, so it feeds
    /// only the operational tier (best-effort from `green_summary`); the
    /// canonical tier is left untouched rather than contaminated with
    /// operator-threshold data, so an all-legacy period fails official
    /// validation honestly instead of presenting legacy data as canonical.
    fn fold_disclosure_waste(&mut self, report: &Report, m: &WindowMetrics) {
        if let Some(dw) = &report.disclosure_waste {
            fold_tier(&mut self.canonical_waste, &dw.canonical);
            fold_tier(&mut self.operational_waste, &dw.operational);
            if let Some(db) = &dw.database {
                self.fold_database_block(db);
            }
            if let Some(mw) = &dw.messaging {
                self.fold_messaging_block(mw);
            }
        } else {
            self.legacy_waste_windows += 1;
            // accounted_io_ops is not serialized, so the legacy energy share
            // uses total_io as the denominator (clamped). Threshold stays 0.
            let ratio = if m.total_io == 0 {
                0.0
            } else {
                (m.avoidable_io as f64 / m.total_io as f64).min(1.0)
            };
            self.operational_waste.avoidable_io_ops = self
                .operational_waste
                .avoidable_io_ops
                .saturating_add(m.avoidable_io);
            self.operational_waste.avoidable_kwh += m.energy_kwh * ratio;
            self.operational_waste.avoidable_kg += m.avoidable_kg;
        }
    }

    /// Fold one window's `disclosure_waste.database` block into the running
    /// database-waste sums. An out-of-spec provenance tag drops the whole
    /// block: a figure whose provenance cannot be published must not reach
    /// the sums either.
    fn fold_database_block(&mut self, db: &crate::report::DisclosureDbWaste) {
        fold_waste_block(&mut self.db_waste, db);
    }

    /// Same fold for the window's `disclosure_waste.messaging` block.
    fn fold_messaging_block(&mut self, mw: &crate::report::DisclosureMsgWaste) {
        fold_waste_block(&mut self.msg_waste, mw);
    }

    fn fold_binary_version(&mut self, bv: &str) {
        if bv.is_empty() || bv.len() > MAX_BINARY_VERSION_LEN || !is_valid_binary_version(bv) {
            return;
        }
        if self.binary_versions.len() < MAX_BINARY_VERSIONS || self.binary_versions.contains(bv) {
            self.binary_versions.insert(bv.to_string());
        }
    }

    /// Record the coefficients one window was scored with. They scale the
    /// published figures and appear nowhere else, so a period that changed
    /// them shows both values rather than one.
    /// Whether the low/high bracket can be published: it only frames the
    /// fixed coefficient, so any window carrying transport under another
    /// value, or under none we can read, disqualifies the whole period.
    /// Absent is disqualifying, not default: windows archived before
    /// 0.9.25 record no coefficient at all and could hold any value.
    fn fold_transport_coefficient(
        &mut self,
        co2: Option<&crate::score::carbon::CarbonReport>,
        cfg: Option<&crate::score::carbon::ScoringConfig>,
    ) {
        let contributed = co2.is_some_and(|c| c.transport_gco2.unwrap_or(0.0) > 0.0);
        if !contributed {
            return;
        }
        let applied = cfg.and_then(|c| c.network_energy_per_byte_kwh);
        let is_default = applied.is_some_and(|v| {
            (v - crate::score::carbon::DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH).abs() < f64::EPSILON
        });
        if !is_default {
            self.transport_coefficient_uncertain = true;
        }
    }

    fn fold_scoring_coefficients(&mut self, cfg: Option<&crate::score::carbon::ScoringConfig>) {
        let Some(cfg) = cfg else { return };
        let mut push = |entry: String| {
            if self.scoring_coefficients.len() < MAX_ENERGY_MODELS
                || self.scoring_coefficients.contains(&entry)
            {
                self.scoring_coefficients.insert(entry);
            }
        };
        if let Some(v) = cfg.embodied_per_request_gco2 {
            push(format!("embodied_gco2_per_request={v}"));
        }
        if let Some(v) = cfg.network_energy_per_byte_kwh {
            push(format!("network_kwh_per_byte={v}"));
        }
        if let Some(v) = cfg.per_operation_coefficients {
            push(format!("per_operation_coefficients={v}"));
        }
        if let Some(v) = cfg.use_hourly_profiles {
            push(format!("use_hourly_profiles={v}"));
        }
    }

    /// Collect the methodology tag and the three terms of one window's
    /// total: operational, embodied, transport. Only the first carries an
    /// avoidable share, so the split is what tells a reader how much of
    /// the published total is reducible at all.
    fn fold_carbon_methodology(&mut self, co2: Option<&crate::score::carbon::CarbonReport>) {
        let Some(co2) = co2 else { return };
        self.embodied_gco2_total += sanitize_f64(co2.embodied_gco2);
        self.operational_gco2_total += sanitize_f64(co2.operational_gco2);
        self.transport_gco2_total += sanitize_f64(co2.transport_gco2.unwrap_or(0.0));
        let tag = co2.total.methodology.as_str();
        if tag.is_empty() || tag.len() > MAX_ENERGY_MODEL_LEN {
            return;
        }
        if self.carbon_methodologies.len() < MAX_ENERGY_MODELS
            || self.carbon_methodologies.contains(tag)
        {
            self.carbon_methodologies.insert(tag.to_string());
        }
    }

    fn fold_window_energy_model(&mut self, model: &str) {
        if model.is_empty() || model.len() > MAX_ENERGY_MODEL_LEN {
            return;
        }
        self.record_energy_model_tag(model);
    }

    /// Strip the `+cal` suffix, flip the calibration flag if present,
    /// and insert the bare tag into `energy_source_models` subject to
    /// the model-set cap.
    fn record_energy_model_tag(&mut self, raw: &str) {
        let bare = raw.strip_suffix("+cal").unwrap_or(raw);
        if raw.len() != bare.len() {
            self.calibration_applied = true;
        }
        if self.energy_source_models.len() < MAX_ENERGY_MODELS
            || self.energy_source_models.contains(bare)
        {
            self.energy_source_models.insert(bare.to_string());
        }
    }

    fn fold_per_service_measured_ratio(&mut self, map: &BTreeMap<String, f64>) {
        for (service, ratio) in map {
            // Symmetric clamp: `sanitize_f64` maps NaN/Inf/negative to
            // 0.0, `.min(1.0)` maps overshoots to 1.0. Both are treated
            // as "out of spec" rather than dropped, so the period mean
            // stays defined.
            let ratio = sanitize_f64(*ratio).min(1.0);
            let entry =
                if let Some(existing) = self.per_service_measured_ratio_sums.get_mut(service) {
                    existing
                } else if self.per_service_measured_ratio_sums.len() >= MAX_SERVICES {
                    continue;
                } else {
                    self.per_service_measured_ratio_sums
                        .entry(service.clone())
                        .or_insert((0.0, 0))
                };
            entry.0 += ratio;
            entry.1 = entry.1.saturating_add(1);
        }
    }

    fn fold_per_service_energy_models(&mut self, map: &BTreeMap<String, String>) {
        for (service, raw_model) in map {
            if raw_model.is_empty() || raw_model.len() > MAX_ENERGY_MODEL_LEN {
                continue;
            }
            self.record_energy_model_tag(raw_model);
            let bare = raw_model.strip_suffix("+cal").unwrap_or(raw_model);
            let set = if let Some(existing) = self.per_service_energy_models.get_mut(service) {
                existing
            } else if self.per_service_energy_models.len() >= MAX_SERVICES {
                continue;
            } else {
                self.per_service_energy_models
                    .entry(service.clone())
                    .or_default()
            };
            if set.len() < MAX_ENERGY_MODELS || set.contains(bare) {
                set.insert(bare.to_string());
            }
        }
    }

    fn attribute_window(
        &mut self,
        report: &Report,
        m: &WindowMetrics,
        per_service_io: &BTreeMap<String, u64>,
        unattributed: bool,
    ) {
        if m.runtime_attribution {
            self.attribute_runtime(report, m, per_service_io);
        } else if unattributed {
            self.attribute_unattributed(m);
        } else {
            self.attribute_proxy_share(report, m, per_service_io);
        }
    }

    fn attribute_runtime(
        &mut self,
        report: &Report,
        m: &WindowMetrics,
        per_service_io: &BTreeMap<String, u64>,
    ) {
        self.runtime_windows += 1;
        for (service, carbon) in &report.green_summary.per_service_carbon_kgco2eq {
            let carbon = sanitize_f64(*carbon);
            let energy = sanitize_f64(
                report
                    .green_summary
                    .per_service_energy_kwh
                    .get(service)
                    .copied()
                    .unwrap_or(0.0),
            );
            let Some(bucket) = bounded_entry(&mut self.per_service, service) else {
                continue;
            };
            bucket.carbon_kgco2eq += carbon;
            bucket.energy_kwh += energy;
            if let Some(io) = per_service_io.get(service) {
                bucket.total_io_ops += *io;
                let share = if m.total_io == 0 {
                    0.0
                } else {
                    *io as f64 / m.total_io as f64
                };
                bucket.total_requests += scale_u64(m.traces, share);
            }
        }
        collect_endpoints_seen(&mut self.per_service, &report.per_endpoint_io_ops);
    }

    fn attribute_unattributed(&mut self, m: &WindowMetrics) {
        self.fallback_windows += 1;
        let bucket = self
            .per_service
            .entry(UNATTRIBUTED_SERVICE.to_string())
            .or_default();
        bucket.total_requests += m.traces;
        bucket.total_io_ops += m.total_io;
        bucket.energy_kwh += m.energy_kwh;
        bucket.carbon_kgco2eq += m.carbon_kg;
    }

    fn attribute_proxy_share(
        &mut self,
        report: &Report,
        m: &WindowMetrics,
        per_service_io: &BTreeMap<String, u64>,
    ) {
        self.fallback_windows += 1;
        let total_window_io: u64 = per_service_io.values().sum();
        for (service, io) in per_service_io {
            let share = if total_window_io == 0 {
                0.0
            } else {
                *io as f64 / total_window_io as f64
            };
            let Some(bucket) = bounded_entry(&mut self.per_service, service) else {
                continue;
            };
            bucket.total_io_ops += *io;
            bucket.total_requests += scale_u64(m.traces, share);
            bucket.energy_kwh += m.energy_kwh * share;
            bucket.carbon_kgco2eq += m.carbon_kg * share;
        }
        collect_endpoints_seen(&mut self.per_service, &report.per_endpoint_io_ops);
    }

    fn route_findings(&mut self, findings: &[Finding], ts: DateTime<Utc>, unattributed: bool) {
        for finding in findings {
            // Route findings to the unattributed bucket when the window
            // had no per-service offenders or runtime maps, so a service
            // never publishes efficiency=100 alongside non-zero
            // anti_patterns_detected_count.
            let service_key: &str = if unattributed {
                UNATTRIBUTED_SERVICE
            } else {
                finding.service.as_str()
            };
            let pattern: &'static str = finding.finding_type.as_str();
            let avoidable = if finding.finding_type.is_avoidable_io() {
                finding.pattern.occurrences.saturating_sub(1) as u64
            } else {
                0
            };

            let Some(bucket) = bounded_entry(&mut self.per_service, service_key) else {
                continue;
            };
            let ap = bucket.anti_patterns.entry(pattern.to_string()).or_default();
            ap.occurrences += 1;
            ap.avoidable_io_ops = ap.avoidable_io_ops.saturating_add(avoidable);
            self.update_seen_timestamps(service_key, pattern, ts);
        }
    }

    fn update_seen_timestamps(&mut self, service_key: &str, pattern: &str, ts: DateTime<Utc>) {
        let key = (service_key.to_string(), pattern.to_string());
        self.first_seen
            .entry(key.clone())
            .and_modify(|prev| {
                if ts < *prev {
                    *prev = ts;
                }
            })
            .or_insert(ts);
        self.last_seen
            .entry(key)
            .and_modify(|prev| {
                if ts > *prev {
                    *prev = ts;
                }
            })
            .or_insert(ts);
    }

    /// Prefer the sum of runtime-calibrated `energy_kwh` accumulated from
    /// each window. Falls back to per-service energy, already proxy when
    /// no runtime data exists.
    fn total_energy_kwh(&self) -> f64 {
        if self.runtime_energy_kwh > 0.0 {
            self.runtime_energy_kwh
        } else {
            self.per_service.values().map(|s| s.energy_kwh).sum()
        }
    }

    fn finalize(self, source_files: Vec<String>, period: &Period) -> AggregateInputs {
        let total_requests = self.total_requests;
        let total_energy_kwh = self.total_energy_kwh();
        let total_carbon = self.total_carbon_kgco2eq;
        // Flat avoidable fields alias the canonical (non-manipulable) tier.
        let canonical_waste = make_waste_tier(&self.canonical_waste, self.total_io_ops);
        let operational_waste = make_waste_tier(&self.operational_waste, self.total_io_ops);
        let anti_patterns_count: u64 = self
            .per_service
            .values()
            .flat_map(|s| s.anti_patterns.values())
            .map(|ap| ap.occurrences)
            .sum();

        let total_windows = self.runtime_windows + self.fallback_windows;
        let period_coverage = if total_windows == 0 {
            1.0
        } else {
            self.runtime_windows as f64 / total_windows as f64
        };

        let temporal_coverage = compute_temporal_coverage(&self.observed_days, period);

        AggregateInputs {
            aggregate: Aggregate {
                total_requests,
                total_energy_kwh,
                total_carbon_kgco2eq: total_carbon,
                carbon_breakdown: build_carbon_breakdown(
                    self.operational_gco2_total,
                    self.embodied_gco2_total,
                    self.transport_gco2_total,
                    self.db_waste.energy_g,
                    self.msg_waste.energy_g,
                    !self.transport_coefficient_uncertain,
                ),
                aggregate_efficiency_score: canonical_waste.efficiency_score,
                aggregate_waste_ratio: canonical_waste.waste_ratio,
                anti_patterns_detected_count: anti_patterns_count,
                estimated_optimization_potential_kgco2eq: canonical_waste.carbon_kgco2eq,
                canonical_waste,
                operational_waste,
                period_coverage,
                binary_versions: self.binary_versions,
                runtime_windows_count: self.runtime_windows,
                fallback_windows_count: self.fallback_windows,
                database_waste: (self.db_waste.windows > 0).then(|| DatabaseWasteAggregate {
                    energy_kwh: self.db_waste.energy_kwh,
                    measured_energy_kwh: self.db_waste.measured_energy_kwh,
                    declared_energy_kwh: self.db_waste.declared_energy_kwh,
                    models: self.db_waste.models,
                    windows_with_figure: self.db_waste.windows,
                    measured_windows: self.db_waste.measured_windows,
                    declared_windows: self.db_waste.declared_windows,
                    estimated_windows: self.db_waste.estimated_windows,
                    windows_with_carbon: self.db_waste.windows_with_carbon,
                    operational_waste_kwh: self.db_waste.operational_kwh,
                    operational_waste_kgco2eq: self.db_waste.operational_g.map(|g| g / 1000.0),
                    canonical_waste_kwh: self.db_waste.canonical_kwh,
                    canonical_waste_kgco2eq: self.db_waste.canonical_g.map(|g| g / 1000.0),
                }),
                messaging_waste: messaging_waste_aggregate(self.msg_waste),
                per_service_energy_models: self.per_service_energy_models,
                per_service_measured_ratio: self
                    .per_service_measured_ratio_sums
                    .into_iter()
                    .map(|(svc, (sum, count))| {
                        let mean = if count == 0 {
                            0.0
                        } else {
                            sum / f64::from(count)
                        };
                        (svc, mean)
                    })
                    .collect(),
                temporal_coverage,
            },
            per_service: self.per_service,
            windows_aggregated: self.windows_aggregated,
            source_files,
            malformed_lines_skipped: self.malformed_lines_skipped,
            legacy_waste_windows: self.legacy_waste_windows,
            first_seen: self.first_seen,
            last_seen: self.last_seen,
            energy_source_models: self.energy_source_models,
            runtime_windows: self.runtime_windows,
            fallback_windows: self.fallback_windows,
            calibration_applied: self.calibration_applied,
            windows_dropped: self.drops_observed.then_some(self.windows_dropped),
            drop_counter_resets: self.drops_observed.then_some(self.drop_counter_resets),
            chain_verified: self.chain_verified,
            chain_unchained: self.chain_unchained,
            chain_breaks: self.chain_breaks,
            chain_breaks_outside: self.chain_breaks_outside,
            carbon_methodologies: self.carbon_methodologies,
            scoring_coefficients: self.scoring_coefficients,
            embodied_gco2_total: self.embodied_gco2_total,
            operational_gco2_total: self.operational_gco2_total,
            transport_gco2_total: self.transport_gco2_total,
        }
    }
}

/// Broker-side waste block, emitted only once a window carried a figure.
fn messaging_waste_aggregate(
    w: DbWasteAccumulator,
) -> Option<super::schema::MessagingWasteAggregate> {
    (w.windows > 0).then(|| super::schema::MessagingWasteAggregate {
        energy_kwh: w.energy_kwh,
        measured_energy_kwh: w.measured_energy_kwh,
        declared_energy_kwh: w.declared_energy_kwh,
        models: w.models,
        windows_with_figure: w.windows,
        measured_windows: w.measured_windows,
        declared_windows: w.declared_windows,
        estimated_windows: w.estimated_windows,
        windows_with_carbon: w.windows_with_carbon,
        operational_waste_kwh: w.operational_kwh,
        operational_waste_kgco2eq: w.operational_g.map(|g| g / 1000.0),
        canonical_waste_kwh: w.canonical_kwh,
        canonical_waste_kgco2eq: w.canonical_g.map(|g| g / 1000.0),
    })
}

/// Split the period total into its three terms, in kgCO2eq. One rule for
/// every term: absent when zero, so unmeasured never reads as zero.
/// Transport is linear in its coefficient, so low/high are mid rescaled,
/// and omitted when a window used another coefficient than the fixed one.
fn build_carbon_breakdown(
    operational_gco2: f64,
    embodied_gco2: f64,
    transport_gco2: f64,
    database_gco2: Option<f64>,
    messaging_gco2: Option<f64>,
    fixed_coefficient: bool,
) -> Option<CarbonBreakdown> {
    use crate::score::carbon::{
        DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH, NETWORK_ENERGY_PER_BYTE_KWH_HIGH,
        NETWORK_ENERGY_PER_BYTE_KWH_LOW,
    };
    let transport = (transport_gco2 > 0.0).then_some(transport_gco2 / 1000.0);
    (operational_gco2 > 0.0
        || embodied_gco2 > 0.0
        || transport.is_some()
        || database_gco2.is_some()
        || messaging_gco2.is_some())
    .then(|| CarbonBreakdown {
        operational_kgco2eq: (operational_gco2 > 0.0).then_some(operational_gco2 / 1000.0),
        embodied_kgco2eq: (embodied_gco2 > 0.0).then_some(embodied_gco2 / 1000.0),
        transport_kgco2eq: transport,
        transport_kgco2eq_low: transport
            .filter(|_| fixed_coefficient)
            .map(|t| t * (NETWORK_ENERGY_PER_BYTE_KWH_LOW / DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH)),
        transport_kgco2eq_high: transport
            .filter(|_| fixed_coefficient)
            .map(|t| t * (NETWORK_ENERGY_PER_BYTE_KWH_HIGH / DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH)),
        database_kgco2eq_out_of_total: database_gco2.map(|g| g / 1000.0),
        messaging_kgco2eq_out_of_total: messaging_gco2.map(|g| g / 1000.0),
    })
}

/// One warning per file, whatever the number of broken lines.
fn warn_break(path: &Path, line_no: usize, warned: &mut bool) {
    if *warned {
        return;
    }
    *warned = true;
    tracing::warn!(
        path = %path.display(),
        line = line_no + 1,
        "archive integrity chain broken: a window was edited, removed or \
         reordered after it was written",
    );
}

/// Verdict for one archive line against the running chain.
enum ChainOutcome {
    /// Hash recomputes and `prev` points at the previous line. Carries the
    /// line's own hash, which the next line must reference.
    Verified(String),
    /// No `hash` field: written before archives were chained. Not a break,
    /// simply not attestable.
    Unchained,
    /// Edited, removed or reordered. Carries this line's own hash so the
    /// walk can resynchronise.
    Break(String),
    /// Not JSON at all: a crash-truncated fragment, never chained.
    Malformed,
}

/// Removes the line's `hash` field in place: the chain hashes the body
/// without it, and the typed fold that consumes the value ignores it.
fn verify_chain_value(
    value: &mut serde_json::Value,
    expected: Option<&(String, u64)>,
) -> ChainOutcome {
    let Some(stated) = value
        .as_object_mut()
        .and_then(|obj| obj.remove("hash"))
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .map(String::from)
    else {
        return ChainOutcome::Unchained;
    };
    let body = &*value;
    if super::hasher::archive_chain_hash(body).ok().as_deref() != Some(stated.as_str()) {
        return ChainOutcome::Break(stated);
    }
    // Without an anchor the walk cannot say where this line belongs, so it
    // adopts it: the break was already counted on the line that lost it.
    let Some((expected_prev, expected_seq)) = expected else {
        return ChainOutcome::Verified(stated);
    };
    let prev_ok = body
        .get("prev")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|p| p == expected_prev);
    // A `seq` that skips means lines are missing between this one and the
    // last. Absent on the first chained format, treated as in sequence.
    let seq_ok = body
        .get("seq")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|s| s == *expected_seq);
    if prev_ok && seq_ok {
        ChainOutcome::Verified(stated)
    } else {
        ChainOutcome::Break(stated)
    }
}

/// Whether an archive line's own timestamp falls inside the period.
/// A line the reader cannot parse counts as in-period: it was read from a
/// file the operator pointed at, and dropping it would hide a break.
fn line_in_period(value: Option<&serde_json::Value>, period: &Period) -> bool {
    value
        .and_then(|v| v.get("ts"))
        .and_then(serde_json::Value::as_str)
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .is_none_or(|ts| in_period(ts.with_timezone(&Utc), period))
}

fn service_io_distribution(
    per_endpoint: &[crate::report::PerEndpointIoOps],
) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for entry in per_endpoint {
        *out.entry(entry.service.clone()).or_insert(0) += entry.io_ops as u64;
    }
    out
}

/// Fold one window's avoidable tier into the period accumulator, sanitizing
/// the energy/carbon against tampered archives.
fn fold_tier(acc: &mut WasteTierAccumulator, tier: &crate::report::AvoidableTier) {
    // saturating_add: the counts come from untrusted archive JSON; a wrapping
    // sum would be a silent under-reporting primitive in a release binary.
    acc.avoidable_io_ops = acc
        .avoidable_io_ops
        .saturating_add(tier.avoidable_io_ops as u64);
    acc.avoidable_kwh += sanitize_f64(tier.avoidable_kwh);
    acc.avoidable_kg += sanitize_f64(tier.avoidable_gco2) / 1000.0;
    acc.n_plus_one_threshold = acc.n_plus_one_threshold.max(tier.n_plus_one_threshold);
}

/// Derive a [`WasteTier`] from a period accumulator. `waste_ratio` and
/// `efficiency_score` are computed against the period's total I/O ops.
fn make_waste_tier(acc: &WasteTierAccumulator, total_io_ops: u64) -> WasteTier {
    // An accumulator that received no data (threshold 0 and no avoidable ops,
    // i.e. an all-legacy canonical tier) is the all-zero default, not "100%
    // efficient". Returning the default lets `skip_serializing_if` omit it,
    // signalling "no data" rather than a misleading perfect score.
    if acc.n_plus_one_threshold == 0 && acc.avoidable_io_ops == 0 {
        return WasteTier::default();
    }
    let waste_ratio = if total_io_ops == 0 {
        0.0
    } else {
        acc.avoidable_io_ops as f64 / total_io_ops as f64
    };
    WasteTier {
        n_plus_one_threshold: acc.n_plus_one_threshold,
        energy_kwh: acc.avoidable_kwh,
        carbon_kgco2eq: acc.avoidable_kg,
        waste_ratio: waste_ratio.clamp(0.0, 1.0),
        efficiency_score: (100.0 - waste_ratio * 100.0).clamp(0.0, 100.0),
    }
}

/// Strip non-finite and negative values from any `f64` field read out
/// of archive JSON (top-level energy, per-service energy, per-service
/// carbon). Tampered or corrupted archives can carry `NaN`, `+Inf`, or
/// negative numbers which would otherwise poison every downstream sum.
fn sanitize_f64(value: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        0.0
    }
}

/// Record each `(service, endpoint)` pair into the matching service
/// bucket's `endpoints_seen` set. Services absent from the bucket map
/// (filtered out by the cap or never inserted) are skipped.
fn collect_endpoints_seen(
    per_service: &mut BTreeMap<String, ServiceAccumulator>,
    entries: &[crate::report::PerEndpointIoOps],
) {
    for entry in entries {
        if let Some(bucket) = per_service.get_mut(&entry.service) {
            bucket.endpoints_seen.insert(entry.endpoint.clone());
        }
    }
}

/// Bounded `entry()`-equivalent for the per-service map. Returns a
/// mutable handle to the bucket when the cap has room, `None` once the
/// cap is reached for a previously unseen service.
fn bounded_entry<'a>(
    per_service: &'a mut BTreeMap<String, ServiceAccumulator>,
    service: &str,
) -> Option<&'a mut ServiceAccumulator> {
    if per_service.contains_key(service) {
        return per_service.get_mut(service);
    }
    if per_service.len() >= MAX_SERVICES {
        return None;
    }
    Some(per_service.entry(service.to_string()).or_default())
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn scale_u64(value: u64, factor: f64) -> u64 {
    let scaled = value as f64 * factor;
    if scaled.is_finite() && scaled >= 0.0 {
        scaled.round() as u64
    } else {
        0
    }
}

fn in_period(ts: DateTime<Utc>, period: &Period) -> bool {
    // Half-open [from, to+1d) so that envelopes at any sub-second offset
    // inside `to_date` (e.g. `2026-03-31T23:59:59.500Z`) are included.
    let from = naive_to_utc_start(period.from_date);
    let to_exclusive = period
        .to_date
        .succ_opt()
        .map_or_else(|| naive_to_utc_start(period.to_date), naive_to_utc_start);
    ts >= from && ts < to_exclusive
}

fn naive_to_utc_start(d: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).expect("00:00:00 is valid"))
}

/// Build the v1.2 temporal-continuity signal from the set of distinct
/// observed days and the declared period. `observed_days` only ever holds
/// in-period days (the `in_period` filter runs before a window is folded),
/// so the ratio cannot exceed 1; it is clamped defensively anyway.
///
/// This measures days with OBSERVED TRAFFIC, not daemon uptime: archiving is
/// traffic-gated, so legitimately quiet days lower it. See
/// [`TemporalCoverage`].
fn compute_temporal_coverage(observed: &BTreeSet<NaiveDate>, period: &Period) -> TemporalCoverage {
    let days_in_period = period.days_covered;
    let observed_days = u32::try_from(observed.len()).unwrap_or(u32::MAX);
    let temporal_coverage = if days_in_period == 0 {
        0.0
    } else {
        (f64::from(observed_days) / f64::from(days_in_period)).clamp(0.0, 1.0)
    };
    TemporalCoverage {
        temporal_coverage,
        observed_days,
        days_in_period,
        largest_gap_days: largest_gap_days(observed, period),
    }
}

/// Longest run of consecutive in-period calendar days with zero windows.
///
/// Walks the sorted `observed` set (`O(observed_days)`) rather than every day in
/// the declared span, so the cost is bounded by archive content, not by an
/// operator-chosen `from`/`to` range. `observed` holds only in-period days, so
/// the leading/trailing edges and the between-day gaps cover the whole period.
fn largest_gap_days(observed: &BTreeSet<NaiveDate>, period: &Period) -> u32 {
    // Inclusive day-count between two dates as a saturating u32 (>= 0).
    let span = |a: NaiveDate, b: NaiveDate| -> u32 {
        u32::try_from((b - a).num_days().max(0)).unwrap_or(u32::MAX)
    };
    let Some(&first) = observed.iter().next() else {
        // No observed day: the whole period is one gap.
        return if period.to_date >= period.from_date {
            span(period.from_date, period.to_date).saturating_add(1)
        } else {
            0
        };
    };
    // Leading gap: days before the first observed day.
    let mut max = span(period.from_date, first);
    // Between consecutive observed days a and b: (b - a) - 1 empty days.
    let mut prev = first;
    for &day in observed.iter().skip(1) {
        max = max.max(span(prev, day).saturating_sub(1));
        prev = day;
    }
    // Trailing gap: days after the last observed day.
    max.max(span(prev, period.to_date))
}

fn resolve_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, AggregationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for path in paths {
        let meta = stat_no_follow(path)?;
        if meta.is_file() {
            push_unique(&mut out, &mut seen, path.clone());
        } else if meta.is_dir() {
            collect_dir_ndjson(path, &mut out, &mut seen)?;
        } else {
            return Err(AggregationError::InvalidInput(path.display().to_string()));
        }
    }
    out.sort();
    Ok(out)
}

/// `symlink_metadata` plus an explicit symlink rejection. The
/// `resolve_files` caller wants `is_file()` / `is_dir()` semantics
/// without following links.
fn stat_no_follow(path: &Path) -> Result<std::fs::Metadata, AggregationError> {
    let meta = std::fs::symlink_metadata(path).map_err(|source| AggregationError::Io {
        path: path.display().to_string(),
        source,
    })?;
    if meta.file_type().is_symlink() {
        return Err(AggregationError::SymlinkRefused {
            path: path.display().to_string(),
        });
    }
    Ok(meta)
}

fn collect_dir_ndjson(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<(), AggregationError> {
    let entries = std::fs::read_dir(dir).map_err(|source| AggregationError::Io {
        path: dir.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| AggregationError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        let p = entry.path();
        // Symlink rejection scoped to `.ndjson` candidates only. A
        // symlinked README or sibling file in the same archive
        // directory is not our concern.
        if p.extension().and_then(|e| e.to_str()) != Some("ndjson") {
            continue;
        }
        stat_no_follow(&p)?;
        push_unique(out, seen, p);
    }
    Ok(())
}

fn push_unique(out: &mut Vec<PathBuf>, seen: &mut BTreeSet<PathBuf>, path: PathBuf) {
    let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if seen.insert(canonical) {
        out.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Confidence, Finding, FindingType, Pattern, Severity};
    use crate::report::interpret::InterpretationLevel;
    use crate::report::{Analysis, GreenSummary, PerEndpointIoOps, QualityGate, Report};
    use crate::score::carbon::{CarbonEstimate, CarbonReport};
    use chrono::TimeZone;
    use core::assert_matches;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_finding(service: &str, ft: FindingType, template: &str) -> Finding {
        Finding {
            finding_type: ft,
            severity: Severity::Warning,
            trace_id: "abc".to_string(),
            service: service.to_string(),
            grouping: Vec::new(),
            source_endpoint: "/api/test".to_string(),
            pattern: Pattern {
                template: template.to_string(),
                occurrences: 5,
                window_ms: 100,
                distinct_params: 3,
                ..Default::default()
            },
            suggestion: String::new(),
            first_timestamp: "2026-01-01T00:00:00Z".to_string(),
            last_timestamp: "2026-01-01T00:00:10Z".to_string(),
            green_impact: None,
            confidence: Confidence::DaemonProduction,
            classification_method: None,
            code_location: None,
            instrumentation_scopes: vec![],
            suggested_fix: None,
            signature: String::new(),
        }
    }

    fn make_report(
        traces: usize,
        total_io: usize,
        avoidable_io: usize,
        services_io: &[(&str, &str, usize)],
        findings: Vec<Finding>,
    ) -> Report {
        let carbon = CarbonReport {
            total: CarbonEstimate {
                low: 0.5,
                mid: 1.0,
                high: 2.0,
                model: "io_proxy_v3".to_string(),
                methodology: "sci_numerator".to_string(),
            },
            avoidable: CarbonEstimate {
                low: 0.1,
                mid: 0.2,
                high: 0.4,
                model: "io_proxy_v3".to_string(),
                methodology: "operational_ratio".to_string(),
            },
            operational_gco2: 0.8,
            embodied_gco2: 0.2,
            transport_gco2: None,
            sci_per_trace: None,
            functional_unit: String::new(),
        };
        let waste_ratio = if total_io == 0 {
            0.0
        } else {
            avoidable_io as f64 / total_io as f64
        };
        let band = InterpretationLevel::for_waste_ratio(waste_ratio);
        Report {
            analysis: Analysis {
                duration_ms: 10,
                events_processed: traces,
                traces_analyzed: traces,
                ingest: None,
            },
            findings,
            green_summary: GreenSummary {
                total_io_ops: total_io,
                avoidable_io_ops: avoidable_io,
                io_waste_ratio: waste_ratio,
                io_waste_ratio_band: band,
                co2: Some(carbon),
                ..GreenSummary::disabled(0)
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: services_io
                .iter()
                .map(|(svc, ep, ops)| PerEndpointIoOps {
                    service: (*svc).to_string(),
                    endpoint: (*ep).to_string(),
                    io_ops: *ops,
                })
                .collect(),
            correlations: vec![],
            embedded_traces: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
            binary_version: String::new(),
            detection_config: None,
            disclosure_waste: None,
        }
    }

    fn write_archive(lines: &[(DateTime<Utc>, Report)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("archive.ndjson");
        let mut file = File::create(&path).unwrap();
        for (ts, report) in lines {
            let envelope = serde_json::json!({ "ts": ts, "report": report });
            writeln!(file, "{}", serde_json::to_string(&envelope).unwrap()).unwrap();
        }
        (dir, path)
    }

    /// Same shape the daemon writer produces, built with the shared chain
    /// primitives rather than a second implementation of them.
    fn write_chained_archive(lines: &[(DateTime<Utc>, Report)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("archive.ndjson");
        let mut file = File::create(&path).unwrap();
        let mut prev = super::super::hasher::ARCHIVE_CHAIN_SEED.to_string();
        for (seq, (ts, report)) in lines.iter().enumerate() {
            let body =
                serde_json::json!({ "ts": ts, "report": report, "prev": prev, "seq": seq as u64 });
            let hash = super::super::hasher::archive_chain_hash(&body).unwrap();
            let mut line = body;
            line.as_object_mut()
                .unwrap()
                .insert("hash".to_string(), serde_json::Value::String(hash.clone()));
            writeln!(file, "{}", serde_json::to_string(&line).unwrap()).unwrap();
            prev = hash;
        }
        (dir, path)
    }

    /// Chained archive whose lines carry the cumulative `drops` counter,
    /// the shape the daemon writes since v1.7.
    fn write_chained_archive_with_drops(
        lines: &[(DateTime<Utc>, Report, u64)],
    ) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("archive.ndjson");
        let mut file = File::create(&path).unwrap();
        let mut prev = super::super::hasher::ARCHIVE_CHAIN_SEED.to_string();
        for (seq, (ts, report, drops)) in lines.iter().enumerate() {
            let body = serde_json::json!({
                "ts": ts, "report": report, "prev": prev, "seq": seq as u64, "drops": drops,
            });
            let hash = super::super::hasher::archive_chain_hash(&body).unwrap();
            let mut line = body;
            line.as_object_mut()
                .unwrap()
                .insert("hash".to_string(), serde_json::Value::String(hash.clone()));
            writeln!(file, "{}", serde_json::to_string(&line).unwrap()).unwrap();
            prev = hash;
        }
        (dir, path)
    }

    fn q1_2026() -> Period {
        Period {
            from_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            to_date: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            period_type: crate::report::periodic::schema::PeriodType::CalendarQuarter,
            days_covered: 90,
        }
    }

    fn plain_window() -> Report {
        make_report(10, 100, 10, &[("svc-a", "/api", 100)], vec![])
    }

    /// The deltas of the cumulative counter are the period's losses. The
    /// first carrying line only sets the baseline (the counter is
    /// daemon-lifetime, its absolute value says nothing about this
    /// file), and a decrease is a daemon restart: counted as a reset,
    /// the delta restarts from the new value.
    #[test]
    fn drop_counter_deltas_become_windows_dropped() {
        let ts = |m, d| Utc.with_ymd_and_hms(2026, m, d, 0, 0, 0).unwrap();
        let windows = [
            (ts(1, 10), plain_window(), 2),
            (ts(1, 20), plain_window(), 5),
            (ts(2, 10), plain_window(), 5),
            (ts(2, 20), plain_window(), 1),
        ];
        let (_dir, path) = write_chained_archive_with_drops(&windows);
        let inputs = aggregate_from_paths(std::slice::from_ref(&path), &q1_2026(), false).unwrap();
        // Baseline 2, then +3, +0, reset (1 < 5) restarting at 1.
        assert_eq!(inputs.windows_dropped, Some(4));
        assert_eq!(inputs.drop_counter_resets, Some(1));
        assert_eq!(inputs.chain_verified, 4, "drops are covered by the hash");
    }

    /// A pre-v1.7 archive carries no counter: "not measured" must stay
    /// distinguishable from "zero drops".
    #[test]
    fn archives_without_the_counter_yield_no_drop_figures() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_archive(&[(ts1, plain_window())]);
        let inputs = aggregate_from_paths(std::slice::from_ref(&path), &q1_2026(), false).unwrap();
        assert_eq!(inputs.windows_dropped, None);
        assert_eq!(inputs.drop_counter_resets, None);
    }

    #[test]
    fn an_intact_chain_verifies_and_an_edited_window_breaks_it() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let windows = [
            (ts1, plain_window()),
            (ts2, plain_window()),
            (ts3, plain_window()),
        ];

        let (_dir, path) = write_chained_archive(&windows);
        let clean = aggregate_from_paths(std::slice::from_ref(&path), &q1_2026(), false).unwrap();
        assert_eq!(clean.chain_verified, 3);
        assert_eq!(clean.chain_breaks, 0);
        assert_eq!(clean.chain_unchained, 0);

        // Edit the middle window the way a hand-tuned archive would be.
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(ToString::to_string).collect();
        let mut middle: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        middle["report"]["green_summary"]["io_waste_ratio"] = serde_json::json!(0.01);
        lines[1] = serde_json::to_string(&middle).unwrap();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let tampered = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(tampered.chain_breaks, 1, "the edited window must show up");
        assert_eq!(
            tampered.chain_verified, 2,
            "the untouched windows stay attestable"
        );
    }

    #[test]
    fn a_crash_truncated_fragment_is_not_a_break() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_chained_archive(&[(ts1, plain_window()), (ts2, plain_window())]);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // A power loss mid-write leaves a newline-terminated partial line.
        std::fs::write(
            &path,
            format!("{}\n{{\"ts\":\"2026-02-01T\n{}\n", lines[0], lines[1]),
        )
        .unwrap();
        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.chain_breaks, 0, "a dropped window is not tampering");
        assert_eq!(out.chain_verified, 2);
        assert_eq!(out.malformed_lines_skipped, 1);
    }

    #[test]
    fn a_removed_window_breaks_the_chain_and_pre_chain_archives_do_not() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_chained_archive(&[
            (ts1, plain_window()),
            (ts2, plain_window()),
            (ts3, plain_window()),
        ]);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // Drop the middle line: the third no longer points at its predecessor.
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.chain_breaks, 1);

        // An archive written before chaining existed is not a break.
        let (_dir2, old) = write_archive(&[(ts1, plain_window()), (ts2, plain_window())]);
        let legacy = aggregate_from_paths(&[old], &q1_2026(), false).unwrap();
        assert_eq!(legacy.chain_unchained, 2);
        assert_eq!(legacy.chain_breaks, 0);
        assert_eq!(legacy.chain_verified, 0);
    }

    #[test]
    fn a_break_revealed_just_after_the_period_still_affects_the_period() {
        let in_period = Utc.with_ymd_and_hms(2026, 3, 30, 0, 0, 0).unwrap();
        let removed = Utc.with_ymd_and_hms(2026, 3, 31, 0, 0, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let (_dir, path) = write_chained_archive(&[
            (in_period, plain_window()),
            (removed, plain_window()),
            (after, plain_window()),
        ]);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.chain_breaks, 1);
        assert_eq!(out.chain_breaks_outside, 0);
    }

    #[test]
    fn carbon_methodology_and_embodied_are_folded_from_the_windows() {
        // Methodology and embodied values are read from archived windows,
        // not from the config of whoever runs `disclose`.
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let mut with_transport = plain_window();
        if let Some(co2) = with_transport.green_summary.co2.as_mut() {
            co2.total.methodology = "sci_v1_numerator+transport".to_string();
            co2.transport_gco2 = Some(0.05);
            // The real pipeline computes total as operational + embodied +
            // transport, so a window carrying transport carries it in its
            // total too. Adding the term without the total would test an
            // arithmetic the product never produces.
            co2.total.mid += 0.05;
        }
        let (_dir, path) = write_archive(&[(ts1, plain_window()), (ts2, with_transport)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(
            out.carbon_methodologies
                .contains("sci_v1_numerator+transport"),
            "a window counting transport must be visible: {:?}",
            out.carbon_methodologies
        );
        assert_eq!(
            out.carbon_methodologies.len(),
            2,
            "a period spanning legacy and current windows shows both tags"
        );
        // 0.2 gCO2eq of M per window, two windows.
        assert!((out.embodied_gco2_total - 0.4).abs() < 1e-9);

        // The split must add up to the published total, otherwise a reader
        // cannot tell the reducible part from the rest.
        let bd = out.aggregate.carbon_breakdown.expect("breakdown present");
        let sum = bd.operational_kgco2eq.unwrap_or(0.0)
            + bd.embodied_kgco2eq.unwrap_or(0.0)
            + bd.transport_kgco2eq.unwrap_or(0.0);
        assert!(
            (sum - out.aggregate.total_carbon_kgco2eq).abs() < 1e-9,
            "operational {:?} + embodied {:?} + transport {:?} must equal total {}",
            bd.operational_kgco2eq,
            bd.embodied_kgco2eq,
            bd.transport_kgco2eq,
            out.aggregate.total_carbon_kgco2eq
        );
        assert!(
            bd.transport_kgco2eq.is_some_and(|t| t > 0.0),
            "the window counting transport must show up in the split"
        );
    }

    #[test]
    fn stripping_the_hash_field_is_a_break_not_a_pre_chain_line() {
        // Deleting `hash` used to read as "written before chaining
        // existed", the benign bucket, which handed an editor a way to
        // rewrite a window and publish breaks: 0.
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_chained_archive(&[(ts1, plain_window()), (ts2, plain_window())]);
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        lines[1].as_object_mut().unwrap().remove("hash");
        lines[1]["report"]["green_summary"]["io_waste_ratio"] = serde_json::json!(0.01);
        let rewritten: Vec<String> = lines.iter().map(ToString::to_string).collect();
        std::fs::write(&path, rewritten.join("\n") + "\n").unwrap();

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.chain_breaks, 1, "a stripped hash after a chained line");
        assert_eq!(out.chain_unchained, 0, "and it is not filed as benign");
    }

    #[test]
    fn a_removed_run_that_ends_inside_the_file_is_a_break() {
        // `prev` alone cannot see a removed run: the line after it points
        // at a hash that is no longer there, and `seq` jumps too.
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_chained_archive(&[
            (ts1, plain_window()),
            (ts2, plain_window()),
            (ts3, plain_window()),
        ]);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.chain_breaks, 1, "the missing middle must surface");
    }

    #[test]
    fn truncating_the_tail_is_invisible_to_the_chain_alone() {
        // Pins the documented limit rather than a capability: what is left
        // after a clean tail cut is a shorter self-consistent chain, and
        // no field inside the file can contradict it. Only an anchor kept
        // outside it can, which `integrity.cross_period_log` reserves.
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_chained_archive(&[
            (ts1, plain_window()),
            (ts2, plain_window()),
            (ts3, plain_window()),
        ]);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(&path, format!("{}\n", lines[0])).unwrap();

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.chain_breaks, 0, "the remaining prefix is consistent");
        assert_eq!(out.chain_verified, 1);
    }

    #[test]
    fn one_stripped_hash_costs_one_break_not_two() {
        // The line after a damaged one chains onto a hash the walk never
        // saw. Counting that as a second break would report two edits for
        // one, and a reader cannot tell an inflated count from a real one.
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_chained_archive(&[
            (ts1, plain_window()),
            (ts2, plain_window()),
            (ts3, plain_window()),
        ]);
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        lines[1].as_object_mut().unwrap().remove("hash");
        let rewritten: Vec<String> = lines.iter().map(ToString::to_string).collect();
        std::fs::write(&path, rewritten.join("\n") + "\n").unwrap();

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.chain_breaks, 1, "one damaged line, one break");
        assert_eq!(out.chain_verified, 2, "the third line re-anchors");
    }

    #[test]
    fn a_break_outside_the_period_is_counted_apart() {
        // One rolling archive can cover years. A window edited in 2025
        // must not be published as a break in the 2026 Q1 disclosure, and
        // the verified count must match what the period actually folded.
        let old_ts = Utc.with_ymd_and_hms(2025, 6, 15, 0, 0, 0).unwrap();
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_chained_archive(&[
            (old_ts, plain_window()),
            (ts1, plain_window()),
            (ts2, plain_window()),
        ]);
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        lines[0]["report"]["green_summary"]["io_waste_ratio"] = serde_json::json!(0.01);
        let rewritten: Vec<String> = lines.iter().map(ToString::to_string).collect();
        std::fs::write(&path, rewritten.join("\n") + "\n").unwrap();

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.chain_breaks, 0, "the period itself is intact");
        assert_eq!(out.chain_breaks_outside, 1, "the 2025 edit still surfaces");
        assert_eq!(out.chain_verified, 2, "only the period's windows count");
    }

    #[test]
    fn the_applied_coefficients_are_published_and_a_change_shows_both() {
        // They scale every published figure and appear nowhere else, so a
        // period scored under two different coefficients must say so
        // rather than average them into one number.
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let mut first = plain_window();
        first.green_summary.scoring_config = Some(crate::score::carbon::ScoringConfig {
            embodied_per_request_gco2: Some(0.001),
            ..crate::score::carbon::ScoringConfig::default()
        });
        let mut second = plain_window();
        second.green_summary.scoring_config = Some(crate::score::carbon::ScoringConfig {
            embodied_per_request_gco2: Some(0.0001),
            ..crate::score::carbon::ScoringConfig::default()
        });
        let (_dir, path) = write_archive(&[(ts1, first), (ts2, second)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(
            out.scoring_coefficients
                .contains("embodied_gco2_per_request=0.001"),
            "{:?}",
            out.scoring_coefficients
        );
        assert!(
            out.scoring_coefficients
                .contains("embodied_gco2_per_request=0.0001"),
            "a coefficient lowered mid-period must stay visible"
        );
    }

    #[test]
    fn transport_is_omitted_rather_than_zeroed_when_nothing_counted_it() {
        // With `include_network_transport` off, and with it on but no
        // cross-region traffic, the windows are identical. Publishing 0.0
        // would assert a measurement neither case made.
        let ts = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_archive(&[(ts, plain_window())]);
        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        let bd = out.aggregate.carbon_breakdown.expect("breakdown present");
        assert!(bd.transport_kgco2eq.is_none());
        assert!(
            bd.embodied_kgco2eq.unwrap_or(0.0) > 0.0,
            "the other terms still publish"
        );
    }

    #[test]
    fn transport_bracket_needs_every_window_to_declare_the_fixed_coefficient() {
        use crate::score::carbon::{
            CarbonEstimate, CarbonReport, DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH, ScoringConfig,
        };
        let estimate = || CarbonEstimate {
            low: 0.0,
            mid: 0.0,
            high: 0.0,
            model: String::new(),
            methodology: String::new(),
        };
        let with_transport = |gco2: f64| CarbonReport {
            total: estimate(),
            avoidable: estimate(),
            operational_gco2: 0.0,
            embodied_gco2: 0.0,
            transport_gco2: Some(gco2),
            sci_per_trace: None,
            functional_unit: String::new(),
        };
        let coefficient = |v: Option<f64>| ScoringConfig {
            network_energy_per_byte_kwh: v,
            ..ScoringConfig::default()
        };

        // A window declaring the fixed coefficient keeps the bracket.
        let mut acc = Builder::default();
        acc.fold_transport_coefficient(
            Some(&with_transport(4.0)),
            Some(&coefficient(Some(DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH))),
        );
        assert!(!acc.transport_coefficient_uncertain);

        // A custom one disqualifies the period.
        let mut acc = Builder::default();
        acc.fold_transport_coefficient(Some(&with_transport(4.0)), Some(&coefficient(Some(5e-10))));
        assert!(acc.transport_coefficient_uncertain);

        // So does an absent one: pre-0.9.25 windows record no coefficient
        // and could have been scored with anything.
        let mut acc = Builder::default();
        acc.fold_transport_coefficient(Some(&with_transport(4.0)), Some(&coefficient(None)));
        assert!(acc.transport_coefficient_uncertain);
        let mut acc = Builder::default();
        acc.fold_transport_coefficient(Some(&with_transport(4.0)), None);
        assert!(acc.transport_coefficient_uncertain);

        // A window that contributed no transport says nothing either way.
        let mut acc = Builder::default();
        acc.fold_transport_coefficient(Some(&with_transport(0.0)), None);
        assert!(!acc.transport_coefficient_uncertain);

        let with_bracket = build_carbon_breakdown(10.0, 0.0, 4.0, None, None, true).unwrap();
        assert!(with_bracket.transport_kgco2eq_low.is_some());
        let without = build_carbon_breakdown(10.0, 0.0, 4.0, None, None, false).unwrap();
        assert!(
            without.transport_kgco2eq.is_some(),
            "the mid still publishes"
        );
        assert!(without.transport_kgco2eq_low.is_none());
        assert!(without.transport_kgco2eq_high.is_none());
    }

    #[test]
    fn standalone_subsystem_carbon_emits_a_breakdown() {
        for (database_gco2, messaging_gco2) in [(Some(2.0), None), (None, Some(3.0))] {
            let breakdown =
                build_carbon_breakdown(0.0, 0.0, 0.0, database_gco2, messaging_gco2, true)
                    .expect("subsystem carbon emits a breakdown");

            // Unmeasured terms are omitted, never published as 0.0.
            assert!(breakdown.operational_kgco2eq.is_none());
            assert!(breakdown.embodied_kgco2eq.is_none());

            assert_eq!(
                breakdown.database_kgco2eq_out_of_total,
                database_gco2.map(|g| g / 1000.0)
            );
            assert_eq!(
                breakdown.messaging_kgco2eq_out_of_total,
                messaging_gco2.map(|g| g / 1000.0)
            );
        }
    }

    #[test]
    fn temporal_coverage_counts_distinct_days() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let (_dir, path) = write_archive(&[
            (ts1, plain_window()),
            (ts2, plain_window()),
            (ts3, plain_window()),
        ]);
        let tc = aggregate_from_paths(&[path], &q1_2026(), false)
            .unwrap()
            .aggregate
            .temporal_coverage;
        assert_eq!(tc.observed_days, 3);
        assert_eq!(tc.days_in_period, 90);
        assert!((tc.temporal_coverage - 3.0 / 90.0).abs() < 1e-9);
        // The three days are a month apart, so the gap is large.
        assert!(tc.largest_gap_days > 25, "gap was {}", tc.largest_gap_days);
    }

    #[test]
    fn temporal_coverage_dedups_same_day_windows() {
        let morning = Utc.with_ymd_and_hms(2026, 1, 10, 1, 0, 0).unwrap();
        let evening = Utc.with_ymd_and_hms(2026, 1, 10, 23, 0, 0).unwrap();
        let (_dir, path) = write_archive(&[(morning, plain_window()), (evening, plain_window())]);
        let tc = aggregate_from_paths(&[path], &q1_2026(), false)
            .unwrap()
            .aggregate
            .temporal_coverage;
        assert_eq!(tc.observed_days, 1);
    }

    #[test]
    fn temporal_coverage_buckets_subsecond_near_midnight_by_utc_day() {
        // 23:59:59.500 on Jan 31 and 00:00:00.200 on Feb 1 are distinct days.
        let jan31 = Utc.with_ymd_and_hms(2026, 1, 31, 23, 59, 59).unwrap()
            + chrono::Duration::milliseconds(500);
        let feb1 = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()
            + chrono::Duration::milliseconds(200);
        let (_dir, path) = write_archive(&[(jan31, plain_window()), (feb1, plain_window())]);
        let tc = aggregate_from_paths(&[path], &q1_2026(), false)
            .unwrap()
            .aggregate
            .temporal_coverage;
        assert_eq!(tc.observed_days, 2);
    }

    #[test]
    fn aggregator_surfaces_both_waste_tiers() {
        let ts = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        // green_summary avoidable (50) differs from the canonical tier (200),
        // so the assertions prove the disclosure_waste tiers drive the output,
        // not the operational green_summary.
        let mut report = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);
        report.disclosure_waste = Some(crate::report::DisclosureWaste {
            database: None,
            messaging: None,
            canonical: crate::report::AvoidableTier {
                n_plus_one_threshold: 2,
                avoidable_io_ops: 200,
                avoidable_kwh: 0.5,
                avoidable_gco2: 300.0,
            },
            operational: crate::report::AvoidableTier {
                n_plus_one_threshold: 5,
                avoidable_io_ops: 50,
                avoidable_kwh: 0.1,
                avoidable_gco2: 80.0,
            },
        });

        let (_dir, path) = write_archive(&[(ts, report)]);
        let agg = aggregate_from_paths(&[path], &q1_2026(), false)
            .unwrap()
            .aggregate;

        assert_eq!(agg.canonical_waste.n_plus_one_threshold, 2);
        assert_eq!(agg.operational_waste.n_plus_one_threshold, 5);
        assert!((agg.canonical_waste.carbon_kgco2eq - 0.3).abs() < 1e-9);
        assert!((agg.operational_waste.carbon_kgco2eq - 0.08).abs() < 1e-9);
        assert!((agg.canonical_waste.energy_kwh - 0.5).abs() < 1e-9);
        assert!((agg.operational_waste.energy_kwh - 0.1).abs() < 1e-9);
        assert!((agg.canonical_waste.waste_ratio - 0.2).abs() < 1e-9);
        assert!((agg.operational_waste.waste_ratio - 0.05).abs() < 1e-9);
        // Flat fields alias the canonical tier.
        assert!(
            (agg.estimated_optimization_potential_kgco2eq - agg.canonical_waste.carbon_kgco2eq)
                .abs()
                < 1e-12
        );
        assert!((agg.aggregate_waste_ratio - agg.canonical_waste.waste_ratio).abs() < 1e-12);
        // No window carried a database block: the aggregate omits it.
        assert!(agg.database_waste.is_none());
    }

    #[test]
    fn aggregator_sums_messaging_waste_and_splits_provenance() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let msg_block = |energy: f64, model: &str| crate::report::DisclosureMsgWaste {
            energy_kwh: energy,
            model: model.to_string(),
            operational_waste_kwh: energy * 0.5,
            operational_waste_gco2: Some(energy * 50.0),
            canonical_waste_kwh: energy * 0.8,
            canonical_waste_gco2: Some(energy * 80.0),
            energy_gco2: Some(energy * 100.0),
        };
        let tier = crate::report::AvoidableTier {
            n_plus_one_threshold: 2,
            avoidable_io_ops: 10,
            avoidable_kwh: 0.1,
            avoidable_gco2: 1.0,
        };
        let mut r1 = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);
        r1.disclosure_waste = Some(crate::report::DisclosureWaste {
            canonical: tier.clone(),
            operational: tier.clone(),
            database: None,
            messaging: Some(msg_block(2.0, "broker_specpower")),
        });
        let mut r2 = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);
        r2.disclosure_waste = Some(crate::report::DisclosureWaste {
            canonical: tier.clone(),
            operational: tier,
            database: None,
            messaging: Some(msg_block(1.0, "estimated")),
        });

        let (_dir, path) = write_archive(&[(ts1, r1), (ts2, r2)]);
        let agg = aggregate_from_paths(&[path], &q1_2026(), false)
            .unwrap()
            .aggregate;
        assert_eq!(
            agg.carbon_breakdown
                .as_ref()
                .and_then(|b| b.messaging_kgco2eq_out_of_total),
            Some(0.2),
            "estimated fallback carbon is already inside the service total"
        );
        let mw = agg.messaging_waste.expect("messaging block emitted");

        assert!((mw.energy_kwh - 3.0).abs() < 1e-12);
        // A declared cluster is neither measured nor estimated: it is an
        // operator statement about provisioned hardware, and publishing
        // it under `measured_*` would read as a reading of the broker.
        assert!(
            (mw.measured_energy_kwh - 0.0).abs() < 1e-12,
            "no window was measured here"
        );
        assert!((mw.declared_energy_kwh - 2.0).abs() < 1e-12);
        assert_eq!(mw.windows_with_figure, 2);
        assert_eq!(mw.measured_windows, 0);
        assert_eq!(mw.declared_windows, 1);
        assert_eq!(mw.estimated_windows, 1);
        assert_eq!(
            mw.measured_windows + mw.declared_windows + mw.estimated_windows,
            mw.windows_with_figure
        );
        assert_eq!(
            mw.models,
            ["broker_specpower".to_string(), "estimated".to_string()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn aggregator_sums_database_waste_across_windows() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let db_block = |energy: f64, model: &str| crate::report::DisclosureDbWaste {
            energy_kwh: energy,
            model: model.to_string(),
            operational_waste_kwh: energy * 0.5,
            operational_waste_gco2: Some(energy * 50.0),
            canonical_waste_kwh: energy * 0.8,
            canonical_waste_gco2: Some(energy * 80.0),
            energy_gco2: Some(energy * 100.0),
        };
        let tier = crate::report::AvoidableTier {
            n_plus_one_threshold: 2,
            avoidable_io_ops: 10,
            avoidable_kwh: 0.1,
            avoidable_gco2: 1.0,
        };
        let mut r1 = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);
        r1.disclosure_waste = Some(crate::report::DisclosureWaste {
            canonical: tier.clone(),
            operational: tier.clone(),
            database: Some(db_block(1.0, "alumet_rapl")),
            messaging: None,
        });
        let mut r2 = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);
        r2.disclosure_waste = Some(crate::report::DisclosureWaste {
            canonical: tier.clone(),
            operational: tier.clone(),
            database: Some(db_block(0.5, "estimated")),
            messaging: None,
        });
        // Out-of-spec provenance tag: the whole block is dropped, none
        // of its figures reach the sums.
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let mut r3 = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);
        r3.disclosure_waste = Some(crate::report::DisclosureWaste {
            canonical: tier.clone(),
            operational: tier,
            database: Some(db_block(9.0, "bad tag!")),
            messaging: None,
        });

        let (_dir, path) = write_archive(&[(ts1, r1), (ts2, r2), (ts3, r3)]);
        let agg = aggregate_from_paths(&[path], &q1_2026(), false)
            .unwrap()
            .aggregate;

        assert_eq!(
            agg.carbon_breakdown
                .as_ref()
                .and_then(|b| b.database_kgco2eq_out_of_total),
            Some(0.1),
            "estimated fallback carbon is already inside the service total"
        );

        let db = agg.database_waste.expect("database aggregate");
        assert_eq!(db.windows_with_figure, 2);
        assert!((db.energy_kwh - 1.5).abs() < 1e-12);
        assert!((db.operational_waste_kwh - 0.75).abs() < 1e-12);
        // gCO2 sums are converted to kg: (50 + 25) / 1000.
        assert!((db.operational_waste_kgco2eq.unwrap() - 0.075).abs() < 1e-12);
        assert!((db.canonical_waste_kwh - 1.2).abs() < 1e-12);
        assert!((db.canonical_waste_kgco2eq.unwrap() - 0.12).abs() < 1e-12);
        let models: Vec<&str> = db.models.iter().map(String::as_str).collect();
        assert_eq!(models, vec!["alumet_rapl", "estimated"]);
        // Provenance split: one measured window, one estimated.
        assert!((db.measured_energy_kwh - 1.0).abs() < 1e-12);
        assert_eq!(db.measured_windows, 1);
        assert_eq!(db.estimated_windows, 1);
        assert_eq!(db.windows_with_carbon, 2);
    }

    #[test]
    fn aggregator_folds_three_windows() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();

        let r1 = make_report(
            100,
            1_000,
            100,
            &[("svc-a", "/api", 600), ("svc-b", "/api", 400)],
            vec![make_finding("svc-a", FindingType::NPlusOneSql, "SELECT *")],
        );
        let r2 = make_report(
            200,
            2_000,
            200,
            &[("svc-a", "/api", 1_200), ("svc-b", "/api", 800)],
            vec![
                make_finding("svc-a", FindingType::NPlusOneSql, "SELECT *"),
                make_finding("svc-b", FindingType::RedundantHttp, "GET /x"),
            ],
        );
        let r3 = make_report(150, 1_500, 150, &[("svc-a", "/other", 1_500)], vec![]);

        let (_dir, path) = write_archive(&[(ts1, r1), (ts2, r2), (ts3, r3)]);
        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();

        assert_eq!(out.windows_aggregated, 3);
        assert_eq!(out.aggregate.total_requests, 100 + 200 + 150);
        assert!(out.aggregate.total_energy_kwh > 0.0);
        // These windows are legacy (no disclosure_waste), so the avoidable
        // figures land only in the operational tier; the canonical tier stays
        // the all-zero default (omitted on the wire, not "100% efficient")
        // rather than being fed legacy data, and the flat aliases stay zero.
        assert!(out.aggregate.operational_waste.waste_ratio > 0.0);
        assert!(out.aggregate.operational_waste.efficiency_score < 100.0);
        assert_eq!(out.aggregate.canonical_waste, WasteTier::default());
        assert!(out.aggregate.aggregate_waste_ratio.abs() < 1e-12);
        assert_eq!(out.aggregate.anti_patterns_detected_count, 3);

        let svc_a = out.per_service.get("svc-a").expect("svc-a missing");
        let svc_b = out.per_service.get("svc-b").expect("svc-b missing");
        assert_eq!(
            svc_a
                .anti_patterns
                .get("n_plus_one_sql")
                .unwrap()
                .occurrences,
            2
        );
        assert_eq!(
            svc_b
                .anti_patterns
                .get("redundant_http")
                .unwrap()
                .occurrences,
            1
        );
        // svc-a saw two endpoints across the windows.
        assert!(svc_a.endpoints_seen.len() >= 2);
    }

    #[test]
    fn aggregate_request_total_does_not_sum_rounded_service_shares() {
        let ts = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let report = make_report(
            1,
            2,
            0,
            &[("svc-a", "/api", 1), ("svc-b", "/api", 1)],
            vec![],
        );
        let (_dir, path) = write_archive(&[(ts, report)]);
        let aggregate = aggregate_from_paths(&[path], &q1_2026(), false)
            .unwrap()
            .aggregate;
        assert_eq!(aggregate.total_requests, 1);
    }

    #[test]
    fn archive_time_range_reports_min_and_max() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 3, 20, 12, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 2, 10, 0, 0, 0).unwrap();
        let r = make_report(10, 100, 5, &[("svc", "/", 100)], vec![]);
        let (_dir, path) = write_archive(&[(ts1, r.clone()), (ts2, r.clone()), (ts3, r)]);

        let range = archive_time_range(&[path])
            .unwrap()
            .expect("non-empty archive");
        assert_eq!(range.0, ts1);
        assert_eq!(range.1, ts2);
    }

    #[test]
    fn archive_time_range_empty_for_no_paths() {
        assert_eq!(archive_time_range(&[]).unwrap(), None);
    }

    #[test]
    fn archive_time_range_skips_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("archive.ndjson");
        let mut file = File::create(&path).unwrap();
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_report(10, 100, 0, &[("svc", "/", 100)], vec![]);
        let envelope = serde_json::json!({ "ts": ts, "report": r });
        writeln!(file, "{{ not json").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "{}", serde_json::to_string(&envelope).unwrap()).unwrap();
        drop(file);

        let range = archive_time_range(&[path])
            .unwrap()
            .expect("one valid window");
        assert_eq!(range, (ts, ts));
    }

    #[test]
    fn aggregator_filters_outside_period() {
        let in_p = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let before = Utc.with_ymd_and_hms(2025, 12, 31, 0, 0, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 4, 1, 12, 0, 0).unwrap();

        let r = make_report(50, 100, 5, &[("svc", "/", 100)], vec![]);
        let (_dir, path) = write_archive(&[(before, r.clone()), (in_p, r.clone()), (after, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.windows_aggregated, 1);
    }

    #[test]
    fn aggregator_skips_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("archive.ndjson");
        let mut file = File::create(&path).unwrap();
        let r = make_report(10, 100, 0, &[("svc", "/", 100)], vec![]);
        let envelope = serde_json::json!({
            "ts": Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap(),
            "report": r,
        });
        writeln!(file, "{}", serde_json::to_string(&envelope).unwrap()).unwrap();
        writeln!(file, "{{ not json").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "{}", serde_json::to_string(&envelope).unwrap()).unwrap();

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.windows_aggregated, 2);
        assert_eq!(out.malformed_lines_skipped, 1);
    }

    #[test]
    fn aggregator_errors_when_no_windows_in_period() {
        let outside = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let r = make_report(10, 100, 0, &[("svc", "/", 100)], vec![]);
        let (_dir, path) = write_archive(&[(outside, r)]);

        let err = aggregate_from_paths(&[path], &q1_2026(), false).unwrap_err();
        assert_matches!(err, AggregationError::NoWindowsInPeriod);
    }

    #[test]
    fn aggregator_strict_attribution_errors_on_empty_io() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_report(10, 100, 0, &[], vec![]);
        let (_dir, path) = write_archive(&[(ts, r)]);

        let err = aggregate_from_paths(&[path], &q1_2026(), true).unwrap_err();
        assert_matches!(err, AggregationError::UnattributedWindow { .. });
    }

    #[test]
    fn aggregator_falls_back_to_unattributed_when_lax() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_report(20, 100, 5, &[], vec![]);
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(out.per_service.contains_key(UNATTRIBUTED_SERVICE));
    }

    #[test]
    fn aggregator_resolves_directory_of_ndjson() {
        let dir = TempDir::new().unwrap();
        let p1 = dir.path().join("a.ndjson");
        let p2 = dir.path().join("b.ndjson");
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_report(10, 100, 0, &[("svc", "/", 100)], vec![]);
        for p in [&p1, &p2] {
            let mut f = File::create(p).unwrap();
            let env = serde_json::json!({ "ts": ts, "report": r });
            writeln!(f, "{}", serde_json::to_string(&env).unwrap()).unwrap();
        }

        let out = aggregate_from_paths(&[dir.path().to_path_buf()], &q1_2026(), false).unwrap();
        assert_eq!(out.windows_aggregated, 2);
        assert_eq!(out.source_files.len(), 2);
    }

    #[test]
    fn aggregator_tracks_first_and_last_seen() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 5, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 3, 25, 0, 0, 0).unwrap();
        let r1 = make_report(
            10,
            100,
            10,
            &[("svc", "/", 100)],
            vec![make_finding("svc", FindingType::NPlusOneSql, "SELECT *")],
        );
        let r2 = r1.clone();
        let (_dir, path) = write_archive(&[(ts1, r1), (ts2, r2)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        let key = ("svc".to_string(), "n_plus_one_sql".to_string());
        assert_eq!(*out.first_seen.get(&key).unwrap(), ts1);
        assert_eq!(*out.last_seen.get(&key).unwrap(), ts2);
    }

    fn make_runtime_report(
        services: &[(&str, &str, usize)],
        per_service_carbon: &[(&str, f64)],
        per_service_energy: &[(&str, f64)],
        per_service_region: &[(&str, &str)],
        energy_kwh: f64,
        energy_model: &str,
    ) -> Report {
        let mut r = make_report(10, 100, 5, services, vec![]);
        r.green_summary.energy_kwh = energy_kwh;
        r.green_summary.energy_model = energy_model.to_string();
        r.green_summary.per_service_carbon_kgco2eq = per_service_carbon
            .iter()
            .map(|(s, v)| ((*s).to_string(), *v))
            .collect();
        r.green_summary.per_service_energy_kwh = per_service_energy
            .iter()
            .map(|(s, v)| ((*s).to_string(), *v))
            .collect();
        r.green_summary.per_service_region = per_service_region
            .iter()
            .map(|(s, r)| ((*s).to_string(), (*r).to_string()))
            .collect();
        r
    }

    #[test]
    fn aggregator_uses_runtime_attribution_when_present() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_runtime_report(
            &[("svc-low", "/api", 100), ("svc-high", "/api", 100)],
            &[("svc-low", 0.005), ("svc-high", 0.500)],
            &[("svc-low", 0.001), ("svc-high", 0.001)],
            &[("svc-low", "eu-west-3"), ("svc-high", "pl")],
            0.002,
            "scaphandre_rapl",
        );
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.runtime_windows, 1);
        assert_eq!(out.fallback_windows, 0);
        assert!(
            (out.aggregate.total_energy_kwh - 0.002).abs() < 1e-12,
            "runtime energy must replace the proxy"
        );
        assert!((out.aggregate.period_coverage - 1.0).abs() < f64::EPSILON);
        assert_eq!(out.aggregate.runtime_windows_count, 1);
        assert_eq!(out.aggregate.fallback_windows_count, 0);
        let low = out.per_service.get("svc-low").expect("svc-low");
        let high = out.per_service.get("svc-high").expect("svc-high");
        assert!((low.carbon_kgco2eq - 0.005).abs() < 1e-12);
        assert!((high.carbon_kgco2eq - 0.500).abs() < 1e-12);
        assert!(out.energy_source_models.contains("scaphandre_rapl"));
    }

    #[test]
    fn aggregator_falls_back_to_proxy_for_legacy_archives() {
        // make_report leaves the per-service maps empty and energy_kwh
        // at zero, mirroring an archive without runtime energy attribution.
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_report(10, 100, 5, &[("svc", "/", 100)], vec![]);
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.runtime_windows, 0);
        assert_eq!(out.fallback_windows, 1);
        assert!(out.energy_source_models.is_empty());
        // Proxy energy = 100 ops * 1e-7 kWh.
        assert!((out.aggregate.total_energy_kwh - 100.0 * 1e-7).abs() < 1e-12);
        assert!(out.aggregate.period_coverage.abs() < f64::EPSILON);
        assert_eq!(out.aggregate.runtime_windows_count, 0);
        assert_eq!(out.aggregate.fallback_windows_count, 1);
    }

    #[test]
    fn aggregator_mixed_archive_per_window_strategy() {
        let ts_legacy = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
        let ts_runtime = Utc.with_ymd_and_hms(2026, 2, 10, 0, 0, 0).unwrap();
        let legacy = make_report(10, 100, 5, &[("svc-a", "/", 100)], vec![]);
        let runtime = make_runtime_report(
            &[("svc-b", "/", 50)],
            &[("svc-b", 0.020)],
            &[("svc-b", 0.0005)],
            &[("svc-b", "eu-west-3")],
            0.0005,
            "cloud_specpower+cal",
        );
        let (_dir, path) = write_archive(&[(ts_legacy, legacy), (ts_runtime, runtime)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.runtime_windows, 1);
        assert_eq!(out.fallback_windows, 1);
        // `+cal` suffix is stripped in the collected set.
        assert!(out.energy_source_models.contains("cloud_specpower"));
        assert!(!out.energy_source_models.iter().any(|m| m.ends_with("+cal")));
        assert!((out.aggregate.period_coverage - 0.5).abs() < f64::EPSILON);
        assert_eq!(out.aggregate.runtime_windows_count, 1);
        assert_eq!(out.aggregate.fallback_windows_count, 1);
        // Invariant: coverage × total ≈ runtime count.
        let total = out.aggregate.runtime_windows_count + out.aggregate.fallback_windows_count;
        let derived = out.aggregate.period_coverage * total as f64;
        assert!(
            (derived - out.aggregate.runtime_windows_count as f64).abs() < f64::EPSILON,
            "period_coverage × total = {derived} should match runtime count {}",
            out.aggregate.runtime_windows_count
        );
    }

    #[test]
    fn aggregator_clamps_negative_energy_and_carbon_from_tampered_archive() {
        // JSON allows negative numbers; a tampered archive could carry
        // them to skew the period downward. Without the clamp, per-service
        // sums would go negative and propagate to `total_energy_kwh`.
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_runtime_report(
            &[("svc-a", "/", 100)],
            &[("svc-a", -1.0e10), ("svc-b", -0.5)],
            &[("svc-a", -1.0), ("svc-b", -2.0)],
            &[("svc-a", "eu-west-3"), ("svc-b", "pl")],
            -1.0e6,
            "scaphandre_rapl",
        );
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        // Per-service clamp exercised here: every negative input maps to 0.
        let svc_a = out.per_service.get("svc-a").expect("svc-a");
        assert!((svc_a.carbon_kgco2eq - 0.0).abs() < f64::EPSILON);
        assert!((svc_a.energy_kwh - 0.0).abs() < f64::EPSILON);
        let svc_b = out.per_service.get("svc-b").expect("svc-b");
        assert!((svc_b.carbon_kgco2eq - 0.0).abs() < f64::EPSILON);
        assert!((svc_b.energy_kwh - 0.0).abs() < f64::EPSILON);
        // Negative `energy_kwh` was rejected by the `> 0.0` check, so the
        // proxy fallback ran: 100 ops × 1e-7 kWh = 1e-5.
        assert!((out.aggregate.total_energy_kwh - 100.0 * 1e-7).abs() < 1e-12);
    }

    #[test]
    fn aggregator_caps_per_service_cardinality() {
        // A tampered archive carrying MAX_SERVICES + N distinct service
        // strings must not balloon `per_service`. Overflow services are
        // silently dropped, existing services keep accumulating.
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let overflow = 32_usize;
        let services_raw: Vec<(String, f64, f64, String)> = (0..(MAX_SERVICES + overflow))
            .map(|i| {
                (
                    format!("svc-{i:05}"),
                    0.001,
                    0.0001,
                    "eu-west-3".to_string(),
                )
            })
            .collect();
        let services: Vec<(&str, &str, usize)> = services_raw
            .iter()
            .map(|(s, _, _, _)| (s.as_str(), "/", 1))
            .collect();
        let carbon: Vec<(&str, f64)> = services_raw
            .iter()
            .map(|(s, c, _, _)| (s.as_str(), *c))
            .collect();
        let energy: Vec<(&str, f64)> = services_raw
            .iter()
            .map(|(s, _, e, _)| (s.as_str(), *e))
            .collect();
        let regions: Vec<(&str, &str)> = services_raw
            .iter()
            .map(|(s, _, _, r)| (s.as_str(), r.as_str()))
            .collect();
        let r = make_runtime_report(
            &services,
            &carbon,
            &energy,
            &regions,
            0.0001,
            "scaphandre_rapl",
        );
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(out.per_service.len() <= MAX_SERVICES);
        assert_eq!(out.windows_aggregated, 1);
    }

    #[test]
    fn aggregator_rejects_oversize_energy_model_strings() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let oversize = "x".repeat(1024);
        let r = make_runtime_report(
            &[("svc", "/", 10)],
            &[("svc", 0.001)],
            &[("svc", 0.0001)],
            &[("svc", "eu-west-3")],
            0.0001,
            &oversize,
        );
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(
            out.energy_source_models.is_empty(),
            "oversize energy_model strings must not enter the set"
        );
    }

    #[test]
    fn aggregator_caps_distinct_energy_models() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let mut reports = Vec::new();
        for i in 0..(MAX_ENERGY_MODELS + 20) {
            let model = format!("model_{i:04}");
            let r = make_runtime_report(
                &[("svc", "/", 10)],
                &[("svc", 0.001)],
                &[("svc", 0.0001)],
                &[("svc", "eu-west-3")],
                0.0001,
                &model,
            );
            let offset = i64::try_from(i).expect("test bound");
            reports.push((ts + chrono::Duration::seconds(offset), r));
        }
        let (_dir, path) = write_archive(&reports);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        // Fed 84 distinct models, cap is 64. Set must saturate at the cap.
        assert_eq!(out.energy_source_models.len(), MAX_ENERGY_MODELS);
    }

    #[test]
    fn aggregator_collects_single_binary_version() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let mut r = make_report(10, 100, 5, &[("svc", "/", 100)], vec![]);
        r.binary_version = "0.6.2".to_string();
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.aggregate.binary_versions.len(), 1);
        assert!(out.aggregate.binary_versions.contains("0.6.2"));
    }

    #[test]
    fn aggregator_collects_distinct_binary_versions_in_mixed_archive() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 10, 0, 0, 0).unwrap();
        let mut r1 = make_report(10, 100, 5, &[("svc-a", "/", 100)], vec![]);
        r1.binary_version = "0.6.2".to_string();
        let mut r2 = make_report(10, 100, 5, &[("svc-b", "/", 50)], vec![]);
        r2.binary_version = "0.6.3".to_string();
        let (_dir, path) = write_archive(&[(ts1, r1), (ts2, r2)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.aggregate.binary_versions.len(), 2);
        assert!(out.aggregate.binary_versions.contains("0.6.2"));
        assert!(out.aggregate.binary_versions.contains("0.6.3"));
    }

    #[test]
    fn aggregator_skips_empty_binary_version_from_legacy_archive() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_report(10, 100, 5, &[("svc", "/", 100)], vec![]);
        // make_report leaves binary_version as String::new()
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(out.aggregate.binary_versions.is_empty());
    }

    #[test]
    fn aggregator_rejects_oversize_binary_version_strings() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let mut r = make_report(10, 100, 5, &[("svc", "/", 100)], vec![]);
        r.binary_version = "x".repeat(MAX_BINARY_VERSION_LEN + 1);
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(out.aggregate.binary_versions.is_empty());
    }

    #[test]
    fn aggregator_detects_calibration_when_cal_suffix_present() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_runtime_report(
            &[("svc", "/", 10)],
            &[("svc", 0.001)],
            &[("svc", 0.0001)],
            &[("svc", "eu-west-3")],
            0.0001,
            "io_proxy_v3+cal",
        );
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(out.calibration_applied);
        // Bare model is collected without the +cal suffix.
        assert!(out.energy_source_models.contains("io_proxy_v3"));
    }

    #[test]
    fn aggregator_does_not_set_calibration_when_no_cal_suffix() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_runtime_report(
            &[("svc", "/", 10)],
            &[("svc", 0.001)],
            &[("svc", 0.0001)],
            &[("svc", "eu-west-3")],
            0.0001,
            "scaphandre_rapl",
        );
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(!out.calibration_applied);
    }

    #[test]
    fn aggregator_collects_per_service_energy_models_single_window() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let mut r = make_runtime_report(
            &[("svc-a", "/", 10), ("svc-b", "/", 10)],
            &[("svc-a", 0.001), ("svc-b", 0.001)],
            &[("svc-a", 0.0001), ("svc-b", 0.0001)],
            &[("svc-a", "eu-west-3"), ("svc-b", "eu-west-3")],
            0.0002,
            "scaphandre_rapl",
        );
        r.green_summary
            .per_service_energy_model
            .insert("svc-a".to_string(), "scaphandre_rapl".to_string());
        r.green_summary
            .per_service_energy_model
            .insert("svc-b".to_string(), "io_proxy_v3".to_string());
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        let map = &out.aggregate.per_service_energy_models;
        assert_eq!(map.len(), 2);
        assert!(map.get("svc-a").unwrap().contains("scaphandre_rapl"));
        assert!(map.get("svc-b").unwrap().contains("io_proxy_v3"));
    }

    #[test]
    fn aggregator_merges_per_service_energy_models_across_windows() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 10, 0, 0, 0).unwrap();
        let mut r1 = make_runtime_report(
            &[("svc", "/", 10)],
            &[("svc", 0.001)],
            &[("svc", 0.0001)],
            &[("svc", "eu-west-3")],
            0.0001,
            "io_proxy_v3",
        );
        r1.green_summary
            .per_service_energy_model
            .insert("svc".to_string(), "io_proxy_v3".to_string());
        let mut r2 = make_runtime_report(
            &[("svc", "/", 10)],
            &[("svc", 0.001)],
            &[("svc", 0.0001)],
            &[("svc", "eu-west-3")],
            0.0001,
            "scaphandre_rapl",
        );
        r2.green_summary
            .per_service_energy_model
            .insert("svc".to_string(), "scaphandre_rapl".to_string());
        let (_dir, path) = write_archive(&[(ts1, r1), (ts2, r2)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        let set = out.aggregate.per_service_energy_models.get("svc").unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains("io_proxy_v3"));
        assert!(set.contains("scaphandre_rapl"));
    }

    #[test]
    fn aggregator_strips_cal_suffix_from_per_service_energy_models() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let mut r = make_runtime_report(
            &[("svc", "/", 10)],
            &[("svc", 0.001)],
            &[("svc", 0.0001)],
            &[("svc", "eu-west-3")],
            0.0001,
            "io_proxy_v3+cal",
        );
        r.green_summary
            .per_service_energy_model
            .insert("svc".to_string(), "io_proxy_v3+cal".to_string());
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        let set = out.aggregate.per_service_energy_models.get("svc").unwrap();
        assert!(set.contains("io_proxy_v3"));
        assert!(!set.iter().any(|m| m.ends_with("+cal")));
    }

    #[test]
    fn aggregator_per_service_measured_ratio_means_across_windows() {
        // Three windows with the same service at ratios 0.5, 0.8, 0.3.
        // Period-level mean: (0.5 + 0.8 + 0.3) / 3 = 0.533...
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 10, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 10, 0, 0, 0).unwrap();
        let make = |ratio: f64| {
            let mut r = make_runtime_report(
                &[("svc", "/", 10)],
                &[("svc", 0.001)],
                &[("svc", 0.0001)],
                &[("svc", "eu-west-3")],
                0.0001,
                "scaphandre_rapl",
            );
            r.green_summary
                .per_service_measured_ratio
                .insert("svc".to_string(), ratio);
            r
        };
        let (_dir, path) = write_archive(&[(ts1, make(0.5)), (ts2, make(0.8)), (ts3, make(0.3))]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        let mean = out
            .aggregate
            .per_service_measured_ratio
            .get("svc")
            .copied()
            .expect("ratio entry");
        let expected = (0.5 + 0.8 + 0.3) / 3.0;
        assert!(
            (mean - expected).abs() < 1e-9,
            "expected mean {expected}, got {mean}"
        );
    }

    #[test]
    fn aggregator_per_service_measured_ratio_clamps_out_of_range_symmetrically() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let mut r = make_runtime_report(
            &[("svc", "/", 10)],
            &[("svc", 0.001)],
            &[("svc", 0.0001)],
            &[("svc", "eu-west-3")],
            0.0001,
            "scaphandre_rapl",
        );
        // Negative -> 0.0 (sanitize_f64), overshoot -> 1.0 (.min(1.0)).
        // Symmetric: both produce a mean entry instead of dropping.
        r.green_summary
            .per_service_measured_ratio
            .insert("svc-neg".to_string(), -0.5);
        r.green_summary
            .per_service_measured_ratio
            .insert("svc-over".to_string(), 1.5);
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(
            out.aggregate.per_service_measured_ratio.get("svc-neg"),
            Some(&0.0)
        );
        assert_eq!(
            out.aggregate.per_service_measured_ratio.get("svc-over"),
            Some(&1.0)
        );
    }

    #[test]
    fn aggregator_per_service_energy_models_empty_for_legacy_archive() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        // make_report leaves the per-service map empty.
        let r = make_report(10, 100, 5, &[("svc", "/", 100)], vec![]);
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(out.aggregate.per_service_energy_models.is_empty());
    }

    #[test]
    fn aggregator_calibration_sticky_when_only_one_window_has_cal() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 10, 0, 0, 0).unwrap();
        let r1 = make_runtime_report(
            &[("svc", "/", 10)],
            &[("svc", 0.001)],
            &[("svc", 0.0001)],
            &[("svc", "eu-west-3")],
            0.0001,
            "io_proxy_v3",
        );
        let r2 = make_runtime_report(
            &[("svc", "/", 10)],
            &[("svc", 0.001)],
            &[("svc", 0.0001)],
            &[("svc", "eu-west-3")],
            0.0001,
            "io_proxy_v3+cal",
        );
        let (_dir, path) = write_archive(&[(ts1, r1), (ts2, r2)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(out.calibration_applied);
    }

    #[test]
    fn aggregator_rejects_invalid_binary_version_pattern() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let mut r = make_report(10, 100, 5, &[("svc", "/", 100)], vec![]);
        // Control char + arbitrary UTF-8: must be rejected by the
        // boundary check, no entry in the period-level set.
        r.binary_version = "0.6.2\u{0001}\u{00e9}".to_string();
        let (_dir, path) = write_archive(&[(ts, r)]);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert!(out.aggregate.binary_versions.is_empty());
    }

    /// A mixed period is the unguarded case: `fold_tier` takes the max of
    /// the thresholds, so official validation passes while the canonical
    /// tier under-reports by the legacy windows' share.
    #[test]
    fn legacy_windows_are_counted_not_silently_folded() {
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let tier = crate::report::AvoidableTier {
            n_plus_one_threshold: crate::detect::n_plus_one::DISCLOSURE_N_PLUS_ONE_THRESHOLD,
            avoidable_io_ops: 10,
            avoidable_kwh: 0.1,
            avoidable_gco2: 1.0,
        };
        let mut canonical = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);
        canonical.disclosure_waste = Some(crate::report::DisclosureWaste {
            canonical: tier.clone(),
            operational: tier,
            database: None,
            messaging: None,
        });
        // Second window keeps `disclosure_waste: None`, the legacy shape.
        let legacy = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);

        let (_dir, path) = write_archive(&[(ts1, canonical), (ts2, legacy)]);
        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();

        assert_eq!(out.windows_aggregated, 2);
        assert_eq!(
            out.legacy_waste_windows, 1,
            "the window without a canonical figure must be counted"
        );
    }

    #[test]
    fn aggregator_caps_distinct_binary_versions() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let mut reports = Vec::new();
        for i in 0..(MAX_BINARY_VERSIONS + 5) {
            let mut r = make_report(10, 100, 5, &[("svc", "/", 100)], vec![]);
            r.binary_version = format!("0.6.{i}");
            let offset = i64::try_from(i).expect("test bound");
            reports.push((ts + chrono::Duration::seconds(offset), r));
        }
        let (_dir, path) = write_archive(&reports);

        let out = aggregate_from_paths(&[path], &q1_2026(), false).unwrap();
        assert_eq!(out.aggregate.binary_versions.len(), MAX_BINARY_VERSIONS);
    }
}
