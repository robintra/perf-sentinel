//! Fold archived per-window [`Report`] envelopes into a
//! [`PeriodicReport`] builder. Wire format and per-service attribution
//! policy: `docs/design/08-PERIODIC-DISCLOSURE.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;

use crate::report::Report;
use crate::score::carbon::ENERGY_PER_IO_OP_KWH;

use super::errors::AggregationError;
use super::schema::{Aggregate, Period};

pub const UNATTRIBUTED_SERVICE: &str = "_unattributed";

#[derive(Debug, Default)]
pub struct AggregateInputs {
    pub aggregate: Aggregate,
    pub per_service: BTreeMap<String, ServiceAccumulator>,
    pub windows_aggregated: u64,
    pub source_files: Vec<String>,
    pub malformed_lines_skipped: u64,
    pub first_seen: BTreeMap<(String, String), DateTime<Utc>>,
    pub last_seen: BTreeMap<(String, String), DateTime<Utc>>,
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

    Ok(builder.finalize(source_files))
}

#[derive(Default)]
struct Builder {
    per_service: BTreeMap<String, ServiceAccumulator>,
    windows_aggregated: u64,
    malformed_lines_skipped: u64,
    first_seen: BTreeMap<(String, String), DateTime<Utc>>,
    last_seen: BTreeMap<(String, String), DateTime<Utc>>,
    total_io_ops: u64,
    avoidable_io_ops: u64,
    total_carbon_kgco2eq: f64,
    avoidable_carbon_kgco2eq: f64,
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
                    if in_period(envelope.ts, period) {
                        self.process_window(envelope, strict)?;
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
    ) -> Result<(), AggregationError> {
        let ts = envelope.ts;
        let report = envelope.report;

        let window_carbon_kg = report
            .green_summary
            .co2
            .as_ref()
            .map_or(0.0, |c| c.total.mid / 1000.0);
        let window_avoidable_kg = report
            .green_summary
            .co2
            .as_ref()
            .map_or(0.0, |c| c.avoidable.mid / 1000.0);
        if !window_carbon_kg.is_finite() || !window_avoidable_kg.is_finite() {
            self.malformed_lines_skipped += 1;
            tracing::warn!(ts = %ts, "skipping window with non-finite carbon");
            return Ok(());
        }
        let window_total_io = report.green_summary.total_io_ops as u64;
        let window_avoidable_io = report.green_summary.avoidable_io_ops as u64;
        let window_energy_kwh = report.green_summary.total_io_ops as f64 * ENERGY_PER_IO_OP_KWH;
        let window_traces = report.analysis.traces_analyzed as u64;

        self.windows_aggregated += 1;
        self.total_io_ops += window_total_io;
        self.avoidable_io_ops += window_avoidable_io;
        self.total_carbon_kgco2eq += window_carbon_kg;
        self.avoidable_carbon_kgco2eq += window_avoidable_kg;

        let per_service_io = service_io_distribution(&report.per_endpoint_io_ops);
        let unattributed = per_service_io.is_empty();

        if unattributed && strict {
            return Err(AggregationError::UnattributedWindow {
                ts: ts.to_rfc3339(),
            });
        }

        if unattributed {
            let bucket = self
                .per_service
                .entry(UNATTRIBUTED_SERVICE.to_string())
                .or_default();
            bucket.total_requests += window_traces;
            bucket.total_io_ops += window_total_io;
            bucket.energy_kwh += window_energy_kwh;
            bucket.carbon_kgco2eq += window_carbon_kg;
        } else {
            let total_window_io: u64 = per_service_io.values().sum();
            for (service, io) in &per_service_io {
                let share = if total_window_io == 0 {
                    0.0
                } else {
                    *io as f64 / total_window_io as f64
                };
                let bucket = self.per_service.entry(service.clone()).or_default();
                bucket.total_io_ops += *io;
                bucket.total_requests += scale_u64(window_traces, share);
                bucket.energy_kwh += window_energy_kwh * share;
                bucket.carbon_kgco2eq += window_carbon_kg * share;
            }
            for entry in &report.per_endpoint_io_ops {
                if let Some(bucket) = self.per_service.get_mut(&entry.service) {
                    bucket.endpoints_seen.insert(entry.endpoint.clone());
                }
            }
        }

        for finding in &report.findings {
            // Route findings to the unattributed bucket when the window
            // had no per-service offenders, otherwise the service shows
            // efficiency = 100 with anti_patterns_detected_count > 0.
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

            let bucket = self.per_service.entry(service_key.to_string()).or_default();
            let ap = bucket.anti_patterns.entry(pattern.to_string()).or_default();
            ap.occurrences += 1;
            ap.avoidable_io_ops = ap.avoidable_io_ops.saturating_add(avoidable);

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

        Ok(())
    }

    fn finalize(self, source_files: Vec<String>) -> AggregateInputs {
        let total_requests: u64 = self.per_service.values().map(|s| s.total_requests).sum();
        let total_energy_kwh: f64 = self.per_service.values().map(|s| s.energy_kwh).sum();
        let total_carbon = self.total_carbon_kgco2eq;
        let waste_ratio = if self.total_io_ops == 0 {
            0.0
        } else {
            self.avoidable_io_ops as f64 / self.total_io_ops as f64
        };
        let efficiency_score = (100.0 - waste_ratio * 100.0).clamp(0.0, 100.0);
        let anti_patterns_count: u64 = self
            .per_service
            .values()
            .flat_map(|s| s.anti_patterns.values())
            .map(|ap| ap.occurrences)
            .sum();

        AggregateInputs {
            aggregate: Aggregate {
                total_requests,
                total_energy_kwh,
                total_carbon_kgco2eq: total_carbon,
                aggregate_efficiency_score: efficiency_score,
                aggregate_waste_ratio: waste_ratio.clamp(0.0, 1.0),
                anti_patterns_detected_count: anti_patterns_count,
                estimated_optimization_potential_kgco2eq: self.avoidable_carbon_kgco2eq,
            },
            per_service: self.per_service,
            windows_aggregated: self.windows_aggregated,
            source_files,
            malformed_lines_skipped: self.malformed_lines_skipped,
            first_seen: self.first_seen,
            last_seen: self.last_seen,
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

fn resolve_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, AggregationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for path in paths {
        let meta = std::fs::symlink_metadata(path).map_err(|source| AggregationError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if meta.file_type().is_symlink() {
            return Err(AggregationError::SymlinkRefused {
                path: path.display().to_string(),
            });
        }
        if meta.is_file() {
            push_unique(&mut out, &mut seen, path.clone());
        } else if meta.is_dir() {
            let entries = std::fs::read_dir(path).map_err(|source| AggregationError::Io {
                path: path.display().to_string(),
                source,
            })?;
            for entry in entries {
                let entry = entry.map_err(|source| AggregationError::Io {
                    path: path.display().to_string(),
                    source,
                })?;
                let p = entry.path();
                // Symlink rejection scoped to `.ndjson` candidates only.
                // A symlinked README or sibling file in the same archive
                // directory is not our concern.
                if p.extension().and_then(|e| e.to_str()) != Some("ndjson") {
                    continue;
                }
                let entry_meta =
                    std::fs::symlink_metadata(&p).map_err(|source| AggregationError::Io {
                        path: p.display().to_string(),
                        source,
                    })?;
                if entry_meta.file_type().is_symlink() {
                    return Err(AggregationError::SymlinkRefused {
                        path: p.display().to_string(),
                    });
                }
                push_unique(&mut out, &mut seen, p);
            }
        } else {
            return Err(AggregationError::InvalidInput(path.display().to_string()));
        }
    }
    out.sort();
    Ok(out)
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
                top_offenders: vec![],
                co2: Some(carbon),
                regions: vec![],
                transport_gco2: None,
                scoring_config: None,
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
        assert!(out.aggregate.aggregate_waste_ratio > 0.0);
        assert!(out.aggregate.aggregate_efficiency_score < 100.0);
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
        assert!(matches!(err, AggregationError::NoWindowsInPeriod));
    }

    #[test]
    fn aggregator_strict_attribution_errors_on_empty_io() {
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let r = make_report(10, 100, 0, &[], vec![]);
        let (_dir, path) = write_archive(&[(ts, r)]);

        let err = aggregate_from_paths(&[path], &q1_2026(), true).unwrap_err();
        assert!(matches!(err, AggregationError::UnattributedWindow { .. }));
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
}
