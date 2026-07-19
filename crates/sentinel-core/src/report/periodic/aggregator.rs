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
use super::schema::{Aggregate, DatabaseWasteAggregate, Period, TemporalCoverage, WasteTier};

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
    models: BTreeSet<String>,
    windows: u64,
    measured_windows: u64,
    estimated_windows: u64,
    windows_with_carbon: u64,
}

#[derive(Default)]
struct Builder {
    per_service: BTreeMap<String, ServiceAccumulator>,
    windows_aggregated: u64,
    malformed_lines_skipped: u64,
    first_seen: BTreeMap<(String, String), DateTime<Utc>>,
    last_seen: BTreeMap<(String, String), DateTime<Utc>>,
    total_io_ops: u64,
    total_carbon_kgco2eq: f64,
    /// Avoidable tiers from each window's `Report.disclosure_waste`.
    canonical_waste: WasteTierAccumulator,
    operational_waste: WasteTierAccumulator,
    /// Database-waste sums from each window's `disclosure_waste.database`.
    /// Windows predating the block are not folded (no canonical figure),
    /// so both tiers stay consistent.
    db_waste: DbWasteAccumulator,
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
        for (line_no, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| AggregationError::Io {
                path: path.display().to_string(),
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<ArchivedReport>(trimmed) {
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
        } else {
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
        if !super::schema::is_valid_model_tag(&db.model) {
            return;
        }
        let acc = &mut self.db_waste;
        let energy = sanitize_f64(db.energy_kwh);
        acc.energy_kwh += energy;
        acc.operational_kwh += sanitize_f64(db.operational_waste_kwh);
        acc.canonical_kwh += sanitize_f64(db.canonical_waste_kwh);
        // Keep None-vs-zero: sums stay None until a window actually carried
        // a carbon conversion.
        if let Some(g) = db.operational_waste_gco2 {
            acc.operational_g = Some(acc.operational_g.unwrap_or(0.0) + sanitize_f64(g));
        }
        if let Some(g) = db.canonical_waste_gco2 {
            acc.canonical_g = Some(acc.canonical_g.unwrap_or(0.0) + sanitize_f64(g));
        }
        acc.windows = acc.windows.saturating_add(1);
        if db.model == crate::report::DB_WASTE_MODEL_ESTIMATED {
            acc.estimated_windows = acc.estimated_windows.saturating_add(1);
        } else {
            acc.measured_windows = acc.measured_windows.saturating_add(1);
            acc.measured_energy_kwh += energy;
        }
        if db.operational_waste_gco2.is_some() || db.canonical_waste_gco2.is_some() {
            acc.windows_with_carbon = acc.windows_with_carbon.saturating_add(1);
        }
        // Same cap as the sibling energy-model collector.
        if acc.models.len() < MAX_BINARY_VERSIONS || acc.models.contains(&db.model) {
            acc.models.insert(db.model.clone());
        }
    }

    fn fold_binary_version(&mut self, bv: &str) {
        if bv.is_empty() || bv.len() > MAX_BINARY_VERSION_LEN || !is_valid_binary_version(bv) {
            return;
        }
        if self.binary_versions.len() < MAX_BINARY_VERSIONS || self.binary_versions.contains(bv) {
            self.binary_versions.insert(bv.to_string());
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

    fn finalize(self, source_files: Vec<String>, period: &Period) -> AggregateInputs {
        let total_requests: u64 = self.per_service.values().map(|s| s.total_requests).sum();
        // Prefer the sum of runtime-calibrated `energy_kwh` accumulated
        // from each window. Falls back to per-service energy (which is
        // already proxy when no runtime data exists).
        let total_energy_kwh: f64 = if self.runtime_energy_kwh > 0.0 {
            self.runtime_energy_kwh
        } else {
            self.per_service.values().map(|s| s.energy_kwh).sum()
        };
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
                    models: self.db_waste.models,
                    windows_with_figure: self.db_waste.windows,
                    measured_windows: self.db_waste.measured_windows,
                    estimated_windows: self.db_waste.estimated_windows,
                    windows_with_carbon: self.db_waste.windows_with_carbon,
                    operational_waste_kwh: self.db_waste.operational_kwh,
                    operational_waste_kgco2eq: self.db_waste.operational_g.map(|g| g / 1000.0),
                    canonical_waste_kwh: self.db_waste.canonical_kwh,
                    canonical_waste_kgco2eq: self.db_waste.canonical_g.map(|g| g / 1000.0),
                }),
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
            first_seen: self.first_seen,
            last_seen: self.last_seen,
            energy_source_models: self.energy_source_models,
            runtime_windows: self.runtime_windows,
            fallback_windows: self.fallback_windows,
            calibration_applied: self.calibration_applied,
        }
    }
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
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
            binary_version: String::new(),
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
        });
        let mut r2 = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);
        r2.disclosure_waste = Some(crate::report::DisclosureWaste {
            canonical: tier.clone(),
            operational: tier.clone(),
            database: Some(db_block(0.5, "estimated")),
        });
        // Out-of-spec provenance tag: the whole block is dropped, none
        // of its figures reach the sums.
        let ts3 = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        let mut r3 = make_report(100, 1_000, 50, &[("svc-a", "/api", 1_000)], vec![]);
        r3.disclosure_waste = Some(crate::report::DisclosureWaste {
            canonical: tier.clone(),
            operational: tier,
            database: Some(db_block(9.0, "bad tag!")),
        });

        let (_dir, path) = write_archive(&[(ts1, r1), (ts2, r2), (ts3, r3)]);
        let agg = aggregate_from_paths(&[path], &q1_2026(), false)
            .unwrap()
            .aggregate;

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
