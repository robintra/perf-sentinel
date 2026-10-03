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
/// span more than a dozen patch versions. The worst case,
/// 256 × 64 bytes = 16 KB, is a negligible memory budget.
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
/// sensible default period and show the archive's covered range. The
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
            // the aggregation path's job). This scan only needs the time
            // bounds.
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
    // Keep None-vs-zero: sums stay None until a window carried a carbon
    // conversion.
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
    /// Previous cumulative `drops` value per archive family, keyed by
    /// the family stem. The counter is daemon-lifetime, so the baseline
    /// must survive a rotation, and it must NOT cross into an unrelated
    /// archive: `disclose` takes a list of paths, and diffing one host's
    /// counter against another's would invent resets.
    last_drops: BTreeMap<String, u64>,
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

/// One archive line handed to [`Builder::fold_window`]. Grouped like
/// [`ChainStep`]: the mutable `warned_fallback` has to travel with the
/// read-only per-line context, and seven loose arguments would sit on
/// clippy's ceiling.
struct WindowStep<'a> {
    parsed: Option<serde_json::Value>,
    trimmed: &'a str,
    period: &'a Period,
    strict: bool,
    path: &'a Path,
    line_no: usize,
    warned_fallback: &'a mut bool,
}

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
        // The anchor is kept, so a destroyed window still surfaces as a
        // break through its successor's `prev`.
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
            // later one without a `hash` had the field removed, an edit
            // the chain exists to catch.
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
        let family = archive_family(path);
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
            // an edit just outside the window would go unseen. The line's
            // own timestamp decides which counter it lands in: one
            // rolling archive can span several periods, and a 2024 edit
            // must not be published as this quarter's break.
            let in_scope = line_in_period(parsed.as_ref(), period);
            if let Some(drops) = parsed
                .as_ref()
                .and_then(|v| v.get("drops"))
                .and_then(serde_json::Value::as_u64)
            {
                self.fold_drops(&family, drops, in_scope);
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
            self.fold_window(WindowStep {
                parsed,
                trimmed,
                period,
                strict,
                path,
                line_no,
                warned_fallback: &mut warned_fallback,
            })?;
        }
        Ok(())
    }

    /// Fold one archive line into the period, or count it as malformed.
    /// Split out of `process_file` for the same reason `walk_chain_line`
    /// is: the loop sits on the cognitive-complexity gate, and this branch
    /// carried the largest share of it.
    fn fold_window(&mut self, step: WindowStep<'_>) -> Result<(), AggregationError> {
        // Reuse the value the chain walk already parsed when there is one,
        // instead of parsing the same line a second time.
        let typed = step.parsed.map_or_else(
            || serde_json::from_str::<ArchivedReport>(step.trimmed),
            serde_json::from_value::<ArchivedReport>,
        );
        let envelope = match typed {
            Ok(envelope) => envelope,
            Err(err) => {
                self.malformed_lines_skipped += 1;
                tracing::warn!(
                    path = %step.path.display(),
                    line = step.line_no + 1,
                    error = %err,
                    "skipping malformed archive line",
                );
                return Ok(());
            }
        };
        if !in_period(envelope.ts, step.period) {
            return Ok(());
        }
        let used_fallback = self.process_window(envelope, step.strict)?;
        if used_fallback && !*step.warned_fallback {
            *step.warned_fallback = true;
            tracing::warn!(
                path = %step.path.display(),
                "archive predates per-service carbon attribution; \
                 falling back to I/O share proxy for this file",
            );
        }
        Ok(())
    }

    /// Fold one line's cumulative `drops` counter. The delta between two
    /// consecutive carrying lines is the loss between them, attributed to
    /// the later line's period. A decrease is a daemon restart: the count
    /// restarts from the new value (its drops happened after the restart)
    /// and the reset is surfaced so the figure reads as a lower bound.
    /// The first carrying line of an archive family sets that family's
    /// baseline without contributing: pruned rotations may hide arbitrary
    /// history behind its absolute value. The baseline then persists
    /// across that family's rotations, so a rotation boundary keeps its
    /// delta, and never leaks into another family's counter.
    ///
    /// `drops_observed` moves only on in-period lines: an out-of-period
    /// carrying line must not turn "not measured" into "measured zero".
    fn fold_drops(&mut self, family: &str, drops: u64, in_scope: bool) {
        if in_scope {
            self.drops_observed = true;
        }
        let Some(slot) = self.last_drops.get_mut(family) else {
            // First carrying line of this family: baseline only.
            self.last_drops.insert(family.to_string(), drops);
            return;
        };
        let prev = std::mem::replace(slot, drops);
        if !in_scope {
            return;
        }
        if drops < prev {
            self.drop_counter_resets += 1;
            self.windows_dropped = self.windows_dropped.saturating_add(drops);
        } else {
            self.windows_dropped = self
                .windows_dropped
                .saturating_add(drops.saturating_sub(prev));
        }
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
        // path. The post-clamp catches the remaining `+Inf` case.
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
    /// only the operational tier (best-effort from `green_summary`). The
    /// canonical tier is left untouched rather than contaminated with
    /// operator-threshold data, so an all-legacy period fails official
    /// validation instead of presenting legacy data as canonical.
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

    /// Record the coefficients one window was scored with. They scale the
    /// published figures and appear nowhere else, so a period that changed
    /// them shows both values rather than one.
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
    /// avoidable share, so the split tells a reader how much of the
    /// published total is reducible at all.
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
        // Route findings to the unattributed bucket when the window had
        // no per-service offenders or runtime maps, so a service never
        // publishes efficiency=100 alongside non-zero
        // anti_patterns_detected_count.
        let fold = unattributed.then_some(UNATTRIBUTED_SERVICE);
        for finding in findings {
            let pattern: &'static str = finding.finding_type.as_str();
            if !finding.finding_type.is_avoidable_io() {
                self.credit(fold.unwrap_or(&finding.service), pattern, ts, 1, 0);
                continue;
            }
            // The finding counts once, on its owner. Its avoidable ops go
            // to the services whose spans they are, the owner first so a
            // refused owner drops the finding whole.
            for (service, ops) in finding.avoidable_by_service() {
                let counted = u64::from(service == finding.service);
                let admitted =
                    self.credit(fold.unwrap_or(service), pattern, ts, counted, ops as u64);
                if !admitted && counted == 1 {
                    break;
                }
            }
        }
    }

    /// Add to one `(service, pattern)` row. False when the service cap
    /// refuses a new service.
    fn credit(
        &mut self,
        service: &str,
        pattern: &str,
        ts: DateTime<Utc>,
        occurrences: u64,
        avoidable: u64,
    ) -> bool {
        let Some(bucket) = bounded_entry(&mut self.per_service, service) else {
            return false;
        };
        let ap = bucket.anti_patterns.entry(pattern.to_string()).or_default();
        ap.occurrences += occurrences;
        ap.avoidable_io_ops = ap.avoidable_io_ops.saturating_add(avoidable);
        self.update_seen_timestamps(service, pattern, ts);
        true
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
    /// though not attestable.
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
    // saturating_add: the counts come from untrusted archive JSON. A wrapping
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
    // signalling "no data".
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
/// so the ratio cannot exceed 1. It is clamped defensively anyway.
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

/// The archive family a file belongs to: its directory plus its stem
/// with the rotation stamp stripped, so `archive.ndjson` and
/// `archive-20260110T000000000Z.ndjson` in one directory share a key
/// while two hosts' `archive.ndjson` do not. The daemon writes the
/// stamp as `{stem}-%Y%m%dT%H%M%S%fZ` (see `daemon::archive::rotate`).
fn archive_family(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let base = match stem.rsplit_once('-') {
        Some((head, stamp)) if is_rotation_stamp(stamp) && !head.is_empty() => head,
        _ => stem.as_str(),
    };
    format!(
        "{}\u{1}{base}",
        path.parent().unwrap_or(Path::new("")).display()
    )
}

/// `%Y%m%dT%H%M%S%fZ`: eight digits, `T`, digits, `Z`.
fn is_rotation_stamp(candidate: &str) -> bool {
    let Some(body) = candidate.strip_suffix('Z') else {
        return false;
    };
    let Some((date, time)) = body.split_once('T') else {
        return false;
    };
    date.len() == 8
        && date.bytes().all(|b| b.is_ascii_digit())
        && !time.is_empty()
        && time.bytes().all(|b| b.is_ascii_digit())
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
        if p.extension().and_then(std::ffi::OsStr::to_str) != Some("ndjson") {
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
mod tests;
