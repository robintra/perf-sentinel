//! `perf-sentinel disclose` subcommand.
//!
//! Loads an org-config TOML, aggregates archived per-window `Report`
//! NDJSON files inside the requested period, applies the official-intent
//! validator when needed, computes the deterministic content hash, and
//! writes the resulting `perf-sentinel-report.json`.

use std::path::{Path, PathBuf};

use chrono::{NaiveDate, Utc};
use sentinel_core::report::periodic::aggregator::{
    AggregateInputs, AntiPatternAccumulator, ServiceAccumulator, UNATTRIBUTED_SERVICE,
    aggregate_from_paths,
};
use sentinel_core::report::periodic::org_config::{self, OrgConfig};
use sentinel_core::report::periodic::schema::{
    AntiPatternDetail, Application, ApplicationG1, ApplicationG2, CalibrationInputs,
    Confidentiality, DisabledPattern, ExcludedApp, ExcludedEnv, Integrity, IntegrityLevel,
    Methodology, Notes, OrgIdentifiers, Organisation, Period, PeriodType, PeriodicReport,
    ReportIntent, ReportMetadata, SCHEMA_VERSION, ScopeManifest, core_patterns_required,
};
use sentinel_core::report::periodic::{
    MIN_PERIOD_COVERAGE_FOR_OFFICIAL, binary_hash, compute_content_hash, validate_official,
};
use sentinel_core::text_safety::sanitize_for_terminal;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ReportIntentCli {
    Internal,
    Official,
    Audited,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ConfidentialityCli {
    Internal,
    Public,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum PeriodTypeCli {
    #[value(name = "calendar-quarter")]
    CalendarQuarter,
    #[value(name = "calendar-month")]
    CalendarMonth,
    #[value(name = "calendar-year")]
    CalendarYear,
    Custom,
}

impl From<ReportIntentCli> for ReportIntent {
    fn from(value: ReportIntentCli) -> Self {
        match value {
            ReportIntentCli::Internal => Self::Internal,
            ReportIntentCli::Official => Self::Official,
            ReportIntentCli::Audited => Self::Audited,
        }
    }
}

impl From<ConfidentialityCli> for Confidentiality {
    fn from(value: ConfidentialityCli) -> Self {
        match value {
            ConfidentialityCli::Internal => Self::Internal,
            ConfidentialityCli::Public => Self::Public,
        }
    }
}

impl From<PeriodTypeCli> for PeriodType {
    fn from(value: PeriodTypeCli) -> Self {
        match value {
            PeriodTypeCli::CalendarQuarter => Self::CalendarQuarter,
            PeriodTypeCli::CalendarMonth => Self::CalendarMonth,
            PeriodTypeCli::CalendarYear => Self::CalendarYear,
            PeriodTypeCli::Custom => Self::Custom,
        }
    }
}

/// Read-only `disclose --tui` preview: a calendar stepper over the period,
/// live intent and confidentiality toggles, an aggregated summary, and the
/// equivalent `disclose` command. Never hashes or writes the report — the
/// canonical artefact stays on the reproducible CLI/CI path (`cmd_disclose`).
/// Compiled only with the `tui` feature; the canonical `disclose` path does
/// not depend on any of it.
#[cfg(feature = "tui")]
pub(crate) mod preview {
    use std::path::{Path, PathBuf};

    use chrono::{DateTime, Datelike, Days, Months, NaiveDate, Utc};
    use sentinel_core::report::periodic::AggregationError;
    use sentinel_core::report::periodic::aggregator::aggregate_from_paths;
    use sentinel_core::report::periodic::org_config::OrgConfig;
    use sentinel_core::report::periodic::schema::{
        Confidentiality, Period, PeriodType, ReportIntent,
    };
    use sentinel_core::report::periodic::{MIN_PERIOD_COVERAGE_FOR_OFFICIAL, validate_official};
    use sentinel_core::text_safety::sanitize_for_terminal;

    use super::build_report;

    /// Calendar granularity for the preview stepper.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Granularity {
        Month,
        Quarter,
        Year,
        Custom,
    }

    impl Granularity {
        /// Cycle to the next granularity (Month, Quarter, Year, Custom, back to Month).
        pub(crate) fn next(self) -> Self {
            match self {
                Self::Month => Self::Quarter,
                Self::Quarter => Self::Year,
                Self::Year => Self::Custom,
                Self::Custom => Self::Month,
            }
        }

        /// Short label for the settings bar.
        pub(crate) fn label(self) -> &'static str {
            match self {
                Self::Month => "Month",
                Self::Quarter => "Quarter",
                Self::Year => "Year",
                Self::Custom => "Custom",
            }
        }

        /// The frozen [`PeriodType`] this granularity maps onto.
        pub(crate) fn period_type(self) -> PeriodType {
            match self {
                Self::Month => PeriodType::CalendarMonth,
                Self::Quarter => PeriodType::CalendarQuarter,
                Self::Year => PeriodType::CalendarYear,
                Self::Custom => PeriodType::Custom,
            }
        }
    }

    /// Resolve a granularity and anchor date into calendar-aligned `[from, to]`
    /// bounds. `Custom` returns the caller-supplied dates verbatim.
    pub(crate) fn resolve_period(
        granularity: Granularity,
        anchor: NaiveDate,
        custom_from: NaiveDate,
        custom_to: NaiveDate,
    ) -> (NaiveDate, NaiveDate) {
        match granularity {
            Granularity::Month => month_bounds(anchor),
            Granularity::Quarter => quarter_bounds(anchor),
            Granularity::Year => year_bounds(anchor),
            Granularity::Custom => (custom_from, custom_to),
        }
    }

    /// Step the anchor one granularity-unit forward (`forward`) or backward.
    /// `Custom` leaves the anchor unchanged (its bounds are edited directly).
    pub(crate) fn step_anchor(
        granularity: Granularity,
        anchor: NaiveDate,
        forward: bool,
    ) -> NaiveDate {
        let months = match granularity {
            Granularity::Month => 1,
            Granularity::Quarter => 3,
            Granularity::Year => 12,
            Granularity::Custom => return anchor,
        };
        let shifted = if forward {
            anchor.checked_add_months(Months::new(months))
        } else {
            anchor.checked_sub_months(Months::new(months))
        };
        shifted.unwrap_or(anchor)
    }

    fn month_bounds(anchor: NaiveDate) -> (NaiveDate, NaiveDate) {
        let first = first_of_month(anchor.year(), anchor.month());
        (first, last_day_of_span(first, 1))
    }

    fn quarter_bounds(anchor: NaiveDate) -> (NaiveDate, NaiveDate) {
        let first_month = (anchor.month0() / 3) * 3 + 1;
        let first = first_of_month(anchor.year(), first_month);
        (first, last_day_of_span(first, 3))
    }

    fn year_bounds(anchor: NaiveDate) -> (NaiveDate, NaiveDate) {
        let first = first_of_month(anchor.year(), 1);
        (first, last_day_of_span(first, 12))
    }

    fn first_of_month(year: i32, month: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, 1)
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch date is valid"))
    }

    /// Last calendar day of a span of `months` starting at `first` (a first-of-month date).
    fn last_day_of_span(first: NaiveDate, months: u32) -> NaiveDate {
        first
            .checked_add_months(Months::new(months))
            .and_then(|next| next.pred_opt())
            .unwrap_or(first)
    }

    /// Toggle between the two preview-relevant intents (Audited is reserved).
    pub(crate) fn cycle_intent(intent: ReportIntent) -> ReportIntent {
        match intent {
            ReportIntent::Internal => ReportIntent::Official,
            _ => ReportIntent::Internal,
        }
    }

    /// Toggle confidentiality between the public G2 aggregate and internal G1 detail.
    pub(crate) fn cycle_confidentiality(confidentiality: Confidentiality) -> Confidentiality {
        match confidentiality {
            Confidentiality::Internal => Confidentiality::Public,
            Confidentiality::Public => Confidentiality::Internal,
        }
    }

    fn intent_cli_value(intent: ReportIntent) -> &'static str {
        match intent {
            ReportIntent::Internal => "internal",
            ReportIntent::Official => "official",
            ReportIntent::Audited => "audited",
        }
    }

    fn confidentiality_cli_value(confidentiality: Confidentiality) -> &'static str {
        match confidentiality {
            Confidentiality::Internal => "internal",
            Confidentiality::Public => "public",
        }
    }

    fn period_type_cli_value(period_type: PeriodType) -> &'static str {
        match period_type {
            PeriodType::CalendarMonth => "calendar-month",
            PeriodType::CalendarQuarter => "calendar-quarter",
            PeriodType::CalendarYear => "calendar-year",
            PeriodType::Custom => "custom",
        }
    }

    /// Render the `disclose` CLI command equivalent to the current preview
    /// settings, for the operator to copy into a reproducible run.
    pub(crate) fn equivalent_command(
        intent: ReportIntent,
        confidentiality: Confidentiality,
        period_type: PeriodType,
        from: NaiveDate,
        to: NaiveDate,
        input: &[PathBuf],
        org_config_path: &Path,
    ) -> String {
        use std::fmt::Write as _;
        let mut cmd = format!(
            "perf-sentinel disclose --intent {} --confidentiality {} --period-type {} --from {from} --to {to}",
            intent_cli_value(intent),
            confidentiality_cli_value(confidentiality),
            period_type_cli_value(period_type),
        );
        for path in input {
            let _ = write!(cmd, " --input {}", path.display());
        }
        let _ = write!(cmd, " --org-config {}", org_config_path.display());
        cmd
    }

    /// Official-validator outcome for the preview.
    pub(crate) enum ValidatorStatus {
        /// Only enforced for official intent.
        NotApplicable,
        Pass,
        Fail(Vec<String>),
    }

    /// Aggregated, redacted summary of a previewed period (never hashed or written).
    pub(crate) struct PreviewSummary {
        pub windows: u64,
        pub days_covered: u32,
        pub period_coverage: f64,
        pub applications_measured: u32,
        pub applications_excluded: usize,
        pub total_requests: u64,
        pub total_carbon_kgco2eq: f64,
        pub total_energy_kwh: f64,
        pub waste_ratio: f64,
        pub anti_patterns: u64,
        pub runtime_windows: u64,
        pub fallback_windows: u64,
        pub malformed_lines: u64,
        pub validator: ValidatorStatus,
    }

    /// Read-only outcome of a preview re-aggregation.
    pub(crate) enum Preview {
        /// No windows fell inside the resolved period.
        Empty,
        /// Aggregation failed (I/O, path resolution). The message is sanitized.
        Error(String),
        /// A summary ready to render.
        Ready(Box<PreviewSummary>),
    }

    /// Re-aggregate the archive over `period` and build the unwritten, unhashed
    /// report for preview. Mirrors `cmd_disclose` minus hashing and file output.
    pub(crate) fn compute_preview(
        input: &[PathBuf],
        org: &OrgConfig,
        period: &Period,
        intent: ReportIntent,
        confidentiality: Confidentiality,
        strict_attribution: bool,
    ) -> Preview {
        let aggregate = match aggregate_from_paths(input, period, strict_attribution) {
            Ok(a) => a,
            Err(AggregationError::NoWindowsInPeriod) => return Preview::Empty,
            Err(err) => {
                return Preview::Error(sanitize_for_terminal(&err.to_string()).into_owned());
            }
        };

        let windows = aggregate.windows_aggregated;
        let malformed_lines = aggregate.malformed_lines_skipped;
        let runtime_windows = aggregate.runtime_windows;
        let fallback_windows = aggregate.fallback_windows;

        let report = build_report(
            org,
            period.clone(),
            intent,
            confidentiality,
            "preview".to_string(),
            aggregate,
        );

        let validator = if matches!(intent, ReportIntent::Official) {
            match validate_official(&report) {
                Ok(()) => ValidatorStatus::Pass,
                Err(errors) => ValidatorStatus::Fail(
                    errors
                        .iter()
                        .map(|e| sanitize_for_terminal(&e.to_string()).into_owned())
                        .collect(),
                ),
            }
        } else {
            ValidatorStatus::NotApplicable
        };

        Preview::Ready(Box::new(PreviewSummary {
            windows,
            days_covered: period.days_covered,
            period_coverage: report.aggregate.period_coverage,
            applications_measured: report.scope_manifest.applications_measured,
            applications_excluded: report.scope_manifest.applications_excluded.len(),
            total_requests: report.aggregate.total_requests,
            total_carbon_kgco2eq: report.aggregate.total_carbon_kgco2eq,
            total_energy_kwh: report.aggregate.total_energy_kwh,
            waste_ratio: report.aggregate.aggregate_waste_ratio,
            anti_patterns: report.aggregate.anti_patterns_detected_count,
            runtime_windows,
            fallback_windows,
            malformed_lines,
            validator,
        }))
    }

    /// Which custom-period edge `step`/`step_month` move while editing a
    /// `Custom` range.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum CustomField {
        From,
        To,
    }

    #[derive(Debug, Clone, Copy)]
    enum AdjustBy {
        Day,
        Month,
    }

    /// Visual tone for a preview summary line, mapped to a terminal style by
    /// the TUI. Keeps all summary content (and its colouring intent) in this
    /// module, so the renderer stays a thin style map and the lines are
    /// testable without ratatui.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Tone {
        Header,
        Normal,
        Dim,
        Good,
        Warn,
        Bad,
    }

    /// One rendered line of the preview summary.
    pub(crate) struct PreviewLine {
        pub text: String,
        pub tone: Tone,
    }

    impl PreviewLine {
        fn new(tone: Tone, text: impl Into<String>) -> Self {
            Self {
                text: text.into(),
                tone,
            }
        }
    }

    /// State for the read-only `disclose --tui` preview tab. Holds the archive
    /// *paths* (never a parsed in-memory copy) and re-runs `aggregate_from_paths`
    /// against the cold NDJSON on every settings change, exactly as the
    /// canonical `cmd_disclose` does.
    pub(crate) struct DiscloseState {
        input: Vec<PathBuf>,
        org: OrgConfig,
        org_config_path: PathBuf,
        strict_attribution: bool,
        /// Earliest and latest window timestamp in the archive, for default
        /// anchoring and the range hint. `None` for an empty archive.
        archive_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
        granularity: Granularity,
        anchor: NaiveDate,
        custom_from: NaiveDate,
        custom_to: NaiveDate,
        custom_field: CustomField,
        intent: ReportIntent,
        confidentiality: Confidentiality,
        preview: Preview,
        scroll_offset: u16,
    }

    impl DiscloseState {
        /// Build the preview state. Anchors on the last day the archive covers
        /// (falling back to `fallback_anchor` for an empty archive) and runs the
        /// first cold aggregation.
        pub(crate) fn new(
            input: Vec<PathBuf>,
            org: OrgConfig,
            org_config_path: PathBuf,
            strict_attribution: bool,
            archive_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
            fallback_anchor: NaiveDate,
        ) -> Self {
            let anchor = archive_range.map_or(fallback_anchor, |(_, max)| max.date_naive());
            let (custom_from, custom_to) = archive_range.map_or((anchor, anchor), |(min, max)| {
                (min.date_naive(), max.date_naive())
            });
            let mut state = Self {
                input,
                org,
                org_config_path,
                strict_attribution,
                archive_range,
                granularity: Granularity::Month,
                anchor,
                custom_from,
                custom_to,
                custom_field: CustomField::From,
                intent: ReportIntent::Internal,
                confidentiality: Confidentiality::Public,
                preview: Preview::Empty,
                scroll_offset: 0,
            };
            state.recompute();
            state
        }

        pub(crate) fn granularity(&self) -> Granularity {
            self.granularity
        }

        pub(crate) fn intent(&self) -> ReportIntent {
            self.intent
        }

        pub(crate) fn confidentiality(&self) -> Confidentiality {
            self.confidentiality
        }

        pub(crate) fn custom_field(&self) -> CustomField {
            self.custom_field
        }

        pub(crate) fn archive_range(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
            self.archive_range
        }

        pub(crate) fn scroll_offset(&self) -> u16 {
            self.scroll_offset
        }

        pub(crate) fn resolved_dates(&self) -> (NaiveDate, NaiveDate) {
            resolve_period(
                self.granularity,
                self.anchor,
                self.custom_from,
                self.custom_to,
            )
        }

        fn period(&self) -> Period {
            let (from, to) = self.resolved_dates();
            let days_covered = match (to - from).num_days() {
                n if n < 0 => 0,
                n => u32::try_from(n).map_or(u32::MAX, |d| d.saturating_add(1)),
            };
            Period {
                from_date: from,
                to_date: to,
                period_type: self.granularity.period_type(),
                days_covered,
            }
        }

        pub(crate) fn days_covered(&self) -> u32 {
            self.period().days_covered
        }

        /// Re-read the cold archive and rebuild the (unwritten) preview for the
        /// current settings. Called on every period/intent/confidentiality edit.
        fn recompute(&mut self) {
            let period = self.period();
            self.preview = compute_preview(
                &self.input,
                &self.org,
                &period,
                self.intent,
                self.confidentiality,
                self.strict_attribution,
            );
            self.scroll_offset = 0;
        }

        pub(crate) fn cycle_granularity(&mut self) {
            self.granularity = self.granularity.next();
            self.recompute();
        }

        /// Coarse step: move the anchor one granularity-unit in calendar modes,
        /// or the active edge by one day in `Custom`.
        pub(crate) fn step(&mut self, forward: bool) {
            if self.granularity == Granularity::Custom {
                self.adjust_custom(forward, AdjustBy::Day);
            } else {
                self.anchor = step_anchor(self.granularity, self.anchor, forward);
            }
            self.recompute();
        }

        /// Fine step: only meaningful in `Custom`, moves the active edge by one
        /// month. A no-op (no recompute) in calendar modes.
        pub(crate) fn step_month(&mut self, forward: bool) {
            if self.granularity == Granularity::Custom {
                self.adjust_custom(forward, AdjustBy::Month);
                self.recompute();
            }
        }

        fn adjust_custom(&mut self, forward: bool, by: AdjustBy) {
            let target = match self.custom_field {
                CustomField::From => &mut self.custom_from,
                CustomField::To => &mut self.custom_to,
            };
            let next = match (by, forward) {
                (AdjustBy::Day, true) => target.checked_add_days(Days::new(1)),
                (AdjustBy::Day, false) => target.checked_sub_days(Days::new(1)),
                (AdjustBy::Month, true) => target.checked_add_months(Months::new(1)),
                (AdjustBy::Month, false) => target.checked_sub_months(Months::new(1)),
            };
            if let Some(next) = next {
                *target = next;
            }
            // Keep the range ordered: the just-moved edge wins.
            if self.custom_from > self.custom_to {
                match self.custom_field {
                    CustomField::From => self.custom_to = self.custom_from,
                    CustomField::To => self.custom_from = self.custom_to,
                }
            }
        }

        /// Toggle which custom edge `step`/`step_month` move. No-op outside `Custom`.
        pub(crate) fn toggle_custom_field(&mut self) {
            if self.granularity == Granularity::Custom {
                self.custom_field = match self.custom_field {
                    CustomField::From => CustomField::To,
                    CustomField::To => CustomField::From,
                };
            }
        }

        pub(crate) fn toggle_intent(&mut self) {
            self.intent = cycle_intent(self.intent);
            // Re-runs the official validator for the new intent.
            self.recompute();
        }

        pub(crate) fn toggle_confidentiality(&mut self) {
            self.confidentiality = cycle_confidentiality(self.confidentiality);
            // Re-redacts at the new confidentiality.
            self.recompute();
        }

        pub(crate) fn scroll(&mut self, forward: bool) {
            if forward {
                let max = u16::try_from(self.summary_lines().len())
                    .unwrap_or(u16::MAX)
                    .saturating_sub(1);
                self.scroll_offset = self.scroll_offset.saturating_add(1).min(max);
            } else {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
        }

        /// The `disclose` command equivalent to the current settings.
        pub(crate) fn equivalent_command(&self) -> String {
            let (from, to) = self.resolved_dates();
            equivalent_command(
                self.intent,
                self.confidentiality,
                self.granularity.period_type(),
                from,
                to,
                &self.input,
                &self.org_config_path,
            )
        }

        /// The scrollable summary, as styled lines. Drives both the renderer and
        /// the scroll clamp, so the two never disagree on line count.
        pub(crate) fn summary_lines(&self) -> Vec<PreviewLine> {
            match &self.preview {
                Preview::Empty => vec![
                    PreviewLine::new(Tone::Warn, "No archived windows fall in this period."),
                    PreviewLine::new(Tone::Dim, "Step or widen the period to include windows."),
                ],
                Preview::Error(msg) => vec![
                    PreviewLine::new(Tone::Bad, "Aggregation failed:"),
                    PreviewLine::new(Tone::Bad, msg.clone()),
                ],
                Preview::Ready(s) => Self::ready_lines(s),
            }
        }

        fn ready_lines(s: &PreviewSummary) -> Vec<PreviewLine> {
            let mut lines = Vec::new();
            lines.push(PreviewLine::new(Tone::Header, "Coverage"));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!("  Windows aggregated:  {}", s.windows),
            ));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!("  Days covered:        {}", s.days_covered),
            ));
            let coverage_ok = s.period_coverage >= MIN_PERIOD_COVERAGE_FOR_OFFICIAL;
            lines.push(PreviewLine::new(
                if coverage_ok { Tone::Good } else { Tone::Warn },
                format!(
                    "  Period coverage:     {:.1}% (official needs >= {:.0}%)",
                    s.period_coverage * 100.0,
                    MIN_PERIOD_COVERAGE_FOR_OFFICIAL * 100.0,
                ),
            ));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!(
                    "  Runtime / fallback:  {} / {} windows",
                    s.runtime_windows, s.fallback_windows
                ),
            ));
            lines.push(PreviewLine::new(
                if s.malformed_lines == 0 {
                    Tone::Dim
                } else {
                    Tone::Warn
                },
                format!("  Malformed skipped:   {}", s.malformed_lines),
            ));

            lines.push(PreviewLine::new(Tone::Header, "Scope"));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!("  Applications measured: {}", s.applications_measured),
            ));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!("  Applications excluded: {}", s.applications_excluded),
            ));

            lines.push(PreviewLine::new(Tone::Header, "Totals"));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!("  Requests:            {}", s.total_requests),
            ));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!(
                    "  Carbon:              {:.4} kgCO2eq",
                    s.total_carbon_kgco2eq
                ),
            ));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!("  Energy:              {:.4} kWh", s.total_energy_kwh),
            ));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!("  Waste ratio:         {:.1}%", s.waste_ratio * 100.0),
            ));
            lines.push(PreviewLine::new(
                Tone::Normal,
                format!("  Anti-patterns:       {}", s.anti_patterns),
            ));

            lines.push(PreviewLine::new(Tone::Header, "Official validator"));
            match &s.validator {
                ValidatorStatus::NotApplicable => lines.push(PreviewLine::new(
                    Tone::Dim,
                    "  Not enforced (intent = internal)",
                )),
                ValidatorStatus::Pass => {
                    lines.push(PreviewLine::new(Tone::Good, "  Pass"));
                }
                ValidatorStatus::Fail(errors) => {
                    lines.push(PreviewLine::new(Tone::Bad, "  Fail:"));
                    for e in errors {
                        lines.push(PreviewLine::new(Tone::Bad, format!("    - {e}")));
                    }
                }
            }
            lines
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn d(year: i32, month: u32, day: u32) -> NaiveDate {
            NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
        }

        fn sample_org() -> OrgConfig {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/examples/perf-sentinel-org.toml");
            sentinel_core::report::periodic::org_config::load_from_path(path)
                .expect("load example org config")
        }

        /// State over an empty archive (`Preview::Empty`). The stepper and
        /// toggle transitions under test don't depend on archived data.
        fn empty_state(anchor: NaiveDate) -> DiscloseState {
            DiscloseState::new(
                Vec::new(),
                sample_org(),
                std::path::PathBuf::from("org.toml"),
                false,
                None,
                anchor,
            )
        }

        #[test]
        fn granularity_cycles_in_order() {
            assert_eq!(Granularity::Month.next(), Granularity::Quarter);
            assert_eq!(Granularity::Quarter.next(), Granularity::Year);
            assert_eq!(Granularity::Year.next(), Granularity::Custom);
            assert_eq!(Granularity::Custom.next(), Granularity::Month);
        }

        #[test]
        fn month_bounds_snap_to_calendar() {
            let (from, to) = resolve_period(
                Granularity::Month,
                d(2026, 2, 15),
                d(2000, 1, 1),
                d(2000, 1, 1),
            );
            assert_eq!(from, d(2026, 2, 1));
            assert_eq!(to, d(2026, 2, 28));
        }

        #[test]
        fn month_bounds_handle_leap_february() {
            let (from, to) = resolve_period(
                Granularity::Month,
                d(2024, 2, 10),
                d(2000, 1, 1),
                d(2000, 1, 1),
            );
            assert_eq!(from, d(2024, 2, 1));
            assert_eq!(to, d(2024, 2, 29));
        }

        #[test]
        fn quarter_bounds_snap_to_calendar() {
            let (from, to) = resolve_period(
                Granularity::Quarter,
                d(2026, 5, 15),
                d(2000, 1, 1),
                d(2000, 1, 1),
            );
            assert_eq!(from, d(2026, 4, 1));
            assert_eq!(to, d(2026, 6, 30));
            let (from, to) = resolve_period(
                Granularity::Quarter,
                d(2026, 12, 31),
                d(2000, 1, 1),
                d(2000, 1, 1),
            );
            assert_eq!(from, d(2026, 10, 1));
            assert_eq!(to, d(2026, 12, 31));
        }

        #[test]
        fn year_bounds_snap_to_calendar() {
            let (from, to) = resolve_period(
                Granularity::Year,
                d(2026, 7, 4),
                d(2000, 1, 1),
                d(2000, 1, 1),
            );
            assert_eq!(from, d(2026, 1, 1));
            assert_eq!(to, d(2026, 12, 31));
        }

        #[test]
        fn custom_returns_supplied_dates() {
            let (from, to) = resolve_period(
                Granularity::Custom,
                d(2026, 1, 1),
                d(2026, 3, 10),
                d(2026, 9, 20),
            );
            assert_eq!(from, d(2026, 3, 10));
            assert_eq!(to, d(2026, 9, 20));
        }

        #[test]
        fn step_anchor_moves_by_unit() {
            assert_eq!(
                step_anchor(Granularity::Month, d(2026, 12, 15), true),
                d(2027, 1, 15)
            );
            assert_eq!(
                step_anchor(Granularity::Month, d(2026, 1, 15), false),
                d(2025, 12, 15)
            );
            assert_eq!(
                step_anchor(Granularity::Quarter, d(2026, 5, 15), true),
                d(2026, 8, 15)
            );
            assert_eq!(
                step_anchor(Granularity::Year, d(2026, 5, 15), true),
                d(2027, 5, 15)
            );
            // Custom is a no-op (edges are edited directly).
            assert_eq!(
                step_anchor(Granularity::Custom, d(2026, 5, 15), true),
                d(2026, 5, 15)
            );
        }

        #[test]
        fn default_anchors_on_archive_max() {
            let min = "2026-01-05T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
            let max = "2026-03-20T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
            let state = DiscloseState::new(
                Vec::new(),
                sample_org(),
                std::path::PathBuf::from("org.toml"),
                false,
                Some((min, max)),
                d(2000, 1, 1),
            );
            let (from, to) = state.resolved_dates();
            assert_eq!(from, d(2026, 3, 1));
            assert_eq!(to, d(2026, 3, 31));
        }

        #[test]
        fn cycle_granularity_changes_resolution() {
            let mut state = empty_state(d(2026, 5, 15));
            assert_eq!(state.granularity(), Granularity::Month);
            state.cycle_granularity();
            assert_eq!(state.granularity(), Granularity::Quarter);
            let (from, to) = state.resolved_dates();
            assert_eq!(from, d(2026, 4, 1));
            assert_eq!(to, d(2026, 6, 30));
        }

        #[test]
        fn step_shifts_month_period() {
            let mut state = empty_state(d(2026, 5, 15));
            state.step(true);
            let (from, to) = state.resolved_dates();
            assert_eq!(from, d(2026, 6, 1));
            assert_eq!(to, d(2026, 6, 30));
        }

        #[test]
        fn toggle_intent_flips_internal_official() {
            let mut state = empty_state(d(2026, 5, 15));
            assert_eq!(state.intent(), ReportIntent::Internal);
            state.toggle_intent();
            assert_eq!(state.intent(), ReportIntent::Official);
            state.toggle_intent();
            assert_eq!(state.intent(), ReportIntent::Internal);
        }

        #[test]
        fn toggle_confidentiality_flips_public_internal() {
            let mut state = empty_state(d(2026, 5, 15));
            assert_eq!(state.confidentiality(), Confidentiality::Public);
            state.toggle_confidentiality();
            assert_eq!(state.confidentiality(), Confidentiality::Internal);
            state.toggle_confidentiality();
            assert_eq!(state.confidentiality(), Confidentiality::Public);
        }

        #[test]
        fn custom_field_toggle_only_in_custom() {
            let mut state = empty_state(d(2026, 5, 15));
            // Month mode: the toggle is a no-op.
            state.toggle_custom_field();
            assert_eq!(state.custom_field(), CustomField::From);
            state.cycle_granularity();
            state.cycle_granularity();
            state.cycle_granularity();
            assert_eq!(state.granularity(), Granularity::Custom);
            state.toggle_custom_field();
            assert_eq!(state.custom_field(), CustomField::To);
        }

        #[test]
        fn custom_day_step_keeps_range_ordered() {
            let min = "2026-03-10T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
            let max = "2026-03-12T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
            let mut state = DiscloseState::new(
                Vec::new(),
                sample_org(),
                std::path::PathBuf::from("org.toml"),
                false,
                Some((min, max)),
                d(2000, 1, 1),
            );
            state.cycle_granularity();
            state.cycle_granularity();
            state.cycle_granularity();
            assert_eq!(state.granularity(), Granularity::Custom);
            assert_eq!(state.resolved_dates(), (d(2026, 3, 10), d(2026, 3, 12)));
            // Push the From edge past To; To follows so the range stays ordered.
            state.step(true);
            state.step(true);
            state.step(true);
            assert_eq!(state.resolved_dates(), (d(2026, 3, 13), d(2026, 3, 13)));
        }

        #[test]
        fn equivalent_command_includes_all_flags() {
            let cmd = equivalent_command(
                ReportIntent::Official,
                Confidentiality::Public,
                PeriodType::CalendarMonth,
                d(2026, 3, 1),
                d(2026, 3, 31),
                &[std::path::PathBuf::from("archive.ndjson")],
                std::path::Path::new("org.toml"),
            );
            assert!(cmd.contains("--intent official"));
            assert!(cmd.contains("--confidentiality public"));
            assert!(cmd.contains("--period-type calendar-month"));
            assert!(cmd.contains("--from 2026-03-01"));
            assert!(cmd.contains("--to 2026-03-31"));
            assert!(cmd.contains("--input archive.ndjson"));
            assert!(cmd.contains("--org-config org.toml"));
        }

        #[test]
        fn empty_archive_reports_no_windows() {
            let state = empty_state(d(2026, 5, 15));
            let lines = state.summary_lines();
            assert!(lines.iter().any(|l| l.text.contains("No archived windows")));
        }
    }
}

#[cfg(feature = "tui")]
pub(crate) use preview::{CustomField, DiscloseState, Granularity, Tone};

#[allow(clippy::too_many_arguments)]
pub fn cmd_disclose(
    intent: ReportIntentCli,
    confidentiality: ConfidentialityCli,
    period_type: PeriodTypeCli,
    from: NaiveDate,
    to: NaiveDate,
    input: &[PathBuf],
    output: &Path,
    org_config_path: &Path,
    strict_attribution: bool,
    emit_attestation: Option<&Path>,
) -> i32 {
    if matches!(intent, ReportIntentCli::Audited) {
        eprintln!(
            "Error: audited intent is reserved for a future release, use 'internal' or 'official' instead"
        );
        return 2;
    }

    let org = match org_config::load_from_path(org_config_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("Error: {}", sanitize_for_terminal(&err.to_string()));
            return 1;
        }
    };

    let days_covered = match (to - from).num_days() {
        n if n < 0 => {
            eprintln!("Error: to_date precedes from_date");
            return 2;
        }
        n => u32::try_from(n).map_or(u32::MAX, |d| d.saturating_add(1)),
    };

    let period = Period {
        from_date: from,
        to_date: to,
        period_type: period_type.into(),
        days_covered,
    };

    let aggregate = match aggregate_from_paths(input, &period, strict_attribution) {
        Ok(a) => a,
        Err(err) => {
            eprintln!("Error: {}", sanitize_for_terminal(&err.to_string()));
            return 1;
        }
    };

    let intent_schema: ReportIntent = intent.into();
    let confidentiality_schema: Confidentiality = confidentiality.into();
    let generated_by = if std::env::var("CI").is_ok_and(|v| !v.is_empty()) {
        "ci".to_string()
    } else {
        "cli-batch".to_string()
    };

    let windows = aggregate.windows_aggregated;
    let mut report = build_report(
        &org,
        period,
        intent_schema,
        confidentiality_schema,
        generated_by,
        aggregate,
    );

    report.integrity.binary_hash = binary_hash().ok();
    report.report_metadata.integrity_level = IntegrityLevel::HashOnly;

    if matches!(intent_schema, ReportIntent::Official)
        && let Err(errors) = validate_official(&report)
    {
        eprintln!("Error: report validation failed");
        for e in &errors {
            eprintln!("  - {}", sanitize_for_terminal(&e.to_string()));
        }
        return 2;
    }

    match compute_content_hash(&report) {
        Ok(hash) => {
            report.integrity.content_hash = hash;
        }
        Err(err) => {
            eprintln!("Error: failed to hash report: {err}");
            return 1;
        }
    }

    if let Err(err) = write_pretty_json(&report, output) {
        eprintln!("Error: failed to write {}: {err}", output.display());
        return 1;
    }

    if let Some(att_path) = emit_attestation {
        let subject_name = output
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("perf-sentinel-report.json");
        if let Err(err) = write_attestation(&report, output, att_path, subject_name) {
            eprintln!(
                "Error: failed to write attestation {}: {err}",
                att_path.display()
            );
            return 1;
        }
        eprintln!("Wrote attestation {}", att_path.display());
    }

    eprintln!(
        "Wrote {} ({} windows aggregated, {} services)",
        output.display(),
        windows,
        report.applications.len()
    );
    0
}

fn write_attestation(
    report: &PeriodicReport,
    report_path: &Path,
    attestation_path: &Path,
    subject_name: &str,
) -> std::io::Result<()> {
    use sentinel_core::report::periodic::attestation::build_in_toto_statement_named;
    use sentinel_core::report::periodic::compute_file_sha256_hex;

    // Refuse to truncate a symlink, same posture as write_pretty_json.
    if let Ok(meta) = std::fs::symlink_metadata(attestation_path)
        && meta.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "attestation output {} is a symlink, refusing to overwrite",
                attestation_path.display()
            ),
        ));
    }
    let digest = compute_file_sha256_hex(report_path)?;
    let statement = build_in_toto_statement_named(report, &digest, subject_name);
    // Compact single-line JSON matches the `.intoto.jsonl` convention
    // (one self-contained JSON value per line) used by cosign tooling,
    // with a trailing newline so concatenating multiple statements
    // stays valid JSONL.
    let mut json = serde_json::to_string(&statement)
        .map_err(|e| std::io::Error::other(format!("serialise attestation: {e}")))?;
    json.push('\n');
    std::fs::write(attestation_path, json)
}

pub(crate) fn build_report(
    org: &OrgConfig,
    period: Period,
    intent: ReportIntent,
    confidentiality: Confidentiality,
    generated_by: String,
    aggregate: AggregateInputs,
) -> PeriodicReport {
    let methodology = Methodology {
        sci_specification: org.methodology.sci_specification.clone(),
        perf_sentinel_version: env!("CARGO_PKG_VERSION").to_string(),
        enabled_patterns: org.methodology.enabled_patterns.clone(),
        disabled_patterns: org
            .methodology
            .disabled_patterns
            .iter()
            .map(|d| DisabledPattern {
                name: d.name.clone(),
                reason: d.reason.clone(),
            })
            .collect(),
        core_patterns_required: core_patterns_required(),
        conformance: org.methodology.conformance,
        calibration_inputs: CalibrationInputs {
            cloud_regions: org.methodology.calibration.cloud_regions.clone(),
            carbon_intensity_source: org.methodology.calibration.carbon_intensity_source.clone(),
            specpower_table_version: org.methodology.calibration.specpower_table_version.clone(),
            binary_specpower_vintage: Some(
                sentinel_core::score::cloud_energy::embedded_specpower_vintage().to_string(),
            ),
            scaphandre_used: org.methodology.calibration.scaphandre_used,
            energy_source_models: aggregate.energy_source_models.clone(),
            calibration_applied: aggregate.calibration_applied,
        },
    };

    let measured_services_count = aggregate
        .per_service
        .keys()
        .filter(|k| k.as_str() != UNATTRIBUTED_SERVICE)
        .count();
    let scope_manifest = ScopeManifest {
        total_applications_declared: org.scope_manifest.total_applications_declared,
        applications_measured: u32::try_from(measured_services_count).unwrap_or(u32::MAX),
        applications_excluded: org
            .scope_manifest
            .applications_excluded
            .iter()
            .map(|a| ExcludedApp {
                service_name: a.service_name.clone(),
                reason: a.reason.clone(),
            })
            .collect(),
        environments_measured: org.scope_manifest.environments_measured.clone(),
        environments_excluded: org
            .scope_manifest
            .environments_excluded
            .iter()
            .map(|e| ExcludedEnv {
                name: e.name.clone(),
                reason: e.reason.clone(),
            })
            .collect(),
        total_requests_in_period: org.scope_manifest.total_requests_in_period,
        requests_measured: aggregate.aggregate.total_requests,
        coverage_percentage: org.scope_manifest.total_requests_in_period.map(|total| {
            if total == 0 {
                0.0
            } else {
                100.0 * (aggregate.aggregate.total_requests as f64) / (total as f64)
            }
        }),
    };

    let applications = build_applications(
        &aggregate.per_service,
        &aggregate.first_seen,
        &aggregate.last_seen,
        confidentiality,
    );

    let base_disclaimers = if org.notes.disclaimers.is_empty() {
        default_disclaimers()
    } else {
        org.notes.disclaimers.clone()
    };
    let disclaimers = augment_disclaimers_for_coverage(
        base_disclaimers,
        intent,
        aggregate.aggregate.period_coverage,
    );
    let disclaimers =
        augment_disclaimers_for_binary_versions(disclaimers, &aggregate.aggregate.binary_versions);
    let disclaimers =
        augment_disclaimers_for_calibration(disclaimers, aggregate.calibration_applied);

    PeriodicReport {
        schema_version: SCHEMA_VERSION.to_string(),
        report_metadata: ReportMetadata {
            intent,
            confidentiality_level: confidentiality,
            integrity_level: IntegrityLevel::None,
            generated_at: Utc::now(),
            generated_by,
            perf_sentinel_version: env!("CARGO_PKG_VERSION").to_string(),
            report_uuid: Uuid::new_v4(),
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        organisation: Organisation {
            name: org.organisation.name.clone(),
            country: org.organisation.country.clone(),
            identifiers: OrgIdentifiers {
                siren: org.organisation.identifiers.siren.clone(),
                vat: org.organisation.identifiers.vat.clone(),
                lei: org.organisation.identifiers.lei.clone(),
                opencorporates_url: org.organisation.identifiers.opencorporates_url.clone(),
                domain: org.organisation.identifiers.domain.clone(),
            },
            sector: org.organisation.sector.clone(),
        },
        period,
        scope_manifest,
        methodology,
        aggregate: aggregate.aggregate,
        applications,
        integrity: Integrity {
            content_hash: String::new(),
            binary_hash: None,
            binary_verification_url: None,
            trace_integrity_chain: serde_json::Value::Null,
            signature: None,
            binary_attestation: None,
        },
        notes: Notes {
            disclaimers,
            reference_urls: org.notes.reference_urls.clone(),
        },
    }
}

/// Append a disclaimer when the period had at least one window with
/// operator-supplied calibration coefficients applied.
fn augment_disclaimers_for_calibration(
    mut disclaimers: Vec<String>,
    calibration_applied: bool,
) -> Vec<String> {
    if calibration_applied {
        disclaimers.push(
            "Calibration applied: per-service energy coefficients from the operator \
             calibration file were used for at least one scoring window in this period. \
             Inspect methodology.calibration_inputs.calibration_applied for the binary fact."
                .to_string(),
        );
    }
    disclaimers
}

/// Append a disclaimer when the period spans more than one
/// perf-sentinel binary version. Single-version periods emit nothing.
fn augment_disclaimers_for_binary_versions(
    mut disclaimers: Vec<String>,
    binary_versions: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    if binary_versions.len() > 1 {
        let list = binary_versions
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        disclaimers.push(format!(
            "This period spans multiple perf-sentinel binary versions ({list}). \
             Verify version compatibility if comparing this report against \
             historical baselines."
        ));
    }
    disclaimers
}

/// Append the runtime-calibration coverage disclaimer when an internal
/// report falls below the official-grade threshold. Official reports are
/// rejected by `validate_official` upstream, so they never reach this
/// branch.
fn augment_disclaimers_for_coverage(
    mut disclaimers: Vec<String>,
    intent: ReportIntent,
    period_coverage: f64,
) -> Vec<String> {
    if matches!(intent, ReportIntent::Internal)
        && period_coverage < MIN_PERIOD_COVERAGE_FOR_OFFICIAL
    {
        disclaimers.push(format!(
            "Runtime-calibration coverage for this period is {:.1}%, below the \
             {:.0}% threshold. Aggregate energy and per-service attribution rely \
             on proxy fallback for the remaining windows. Not suitable for \
             official disclosure.",
            period_coverage * 100.0,
            MIN_PERIOD_COVERAGE_FOR_OFFICIAL * 100.0,
        ));
    }
    disclaimers
}

fn build_applications(
    per_service: &BTreeMap<String, ServiceAccumulator>,
    first_seen: &BTreeMap<(String, String), chrono::DateTime<Utc>>,
    last_seen: &BTreeMap<(String, String), chrono::DateTime<Utc>>,
    confidentiality: Confidentiality,
) -> Vec<Application> {
    let mut out = Vec::with_capacity(per_service.len());
    for (service, accum) in per_service {
        // The `_unattributed` bucket contributes to aggregate totals
        // but is not a "measured application" in the wire output.
        // Keeping it would desync applications_measured from applications.len().
        if service == UNATTRIBUTED_SERVICE {
            continue;
        }
        let avoidable: u64 = accum
            .anti_patterns
            .values()
            .map(|ap| ap.avoidable_io_ops)
            .sum();
        let any_anti_pattern: u64 = accum.anti_patterns.values().map(|ap| ap.occurrences).sum();
        let efficiency_score = if accum.total_io_ops == 0 {
            // Zero I/O recorded but findings present: cannot publish 100%.
            if any_anti_pattern == 0 { 100.0 } else { 0.0 }
        } else {
            // Efficiency = 100 - 100 * avoidable / total_io_ops (clamped).
            (100.0 - 100.0 * (avoidable as f64) / (accum.total_io_ops as f64)).clamp(0.0, 100.0)
        };
        let endpoints_observed = u32::try_from(accum.endpoints_seen.len()).unwrap_or(u32::MAX);
        match confidentiality {
            Confidentiality::Internal => out.push(Application::G1(ApplicationG1 {
                service_name: service.clone(),
                display_name: None,
                service_version: None,
                endpoints_observed,
                total_requests: accum.total_requests,
                energy_kwh: accum.energy_kwh,
                carbon_kgco2eq: accum.carbon_kgco2eq,
                efficiency_score,
                anti_patterns: build_anti_pattern_details(
                    service,
                    &accum.anti_patterns,
                    first_seen,
                    last_seen,
                    service_carbon_ratio(accum),
                ),
            })),
            Confidentiality::Public => {
                let count: u64 = accum.anti_patterns.values().map(|ap| ap.occurrences).sum();
                out.push(Application::G2(ApplicationG2 {
                    service_name: service.clone(),
                    display_name: None,
                    service_version: None,
                    endpoints_observed,
                    total_requests: accum.total_requests,
                    energy_kwh: accum.energy_kwh,
                    carbon_kgco2eq: accum.carbon_kgco2eq,
                    efficiency_score,
                    anti_patterns_detected_count: count,
                }));
            }
        }
    }
    out
}

fn service_carbon_ratio(accum: &ServiceAccumulator) -> f64 {
    if accum.energy_kwh > 0.0 {
        accum.carbon_kgco2eq / accum.energy_kwh
    } else {
        0.0
    }
}

fn build_anti_pattern_details(
    service: &str,
    anti_patterns: &BTreeMap<String, AntiPatternAccumulator>,
    first_seen: &BTreeMap<(String, String), chrono::DateTime<Utc>>,
    last_seen: &BTreeMap<(String, String), chrono::DateTime<Utc>>,
    service_carbon_kwh_ratio: f64,
) -> Vec<AntiPatternDetail> {
    // Proxy coefficient lifted from the carbon module so the per-pattern
    // waste line up with the aggregate proxy energy. Region-blind, see
    // design doc 08.
    const ENERGY_PER_IO_OP_KWH: f64 = 0.000_000_1;
    let now = Utc::now();
    let mut out = Vec::with_capacity(anti_patterns.len());
    for (pattern, accum) in anti_patterns {
        let key = (service.to_string(), pattern.clone());
        let first = first_seen.get(&key).copied().unwrap_or(now);
        let last = last_seen.get(&key).copied().unwrap_or(now);
        let waste_kwh = (accum.avoidable_io_ops as f64) * ENERGY_PER_IO_OP_KWH;
        let waste_kgco2eq = waste_kwh * service_carbon_kwh_ratio;
        out.push(AntiPatternDetail {
            kind: pattern.clone(),
            occurrences: accum.occurrences,
            estimated_waste_kwh: waste_kwh,
            estimated_waste_kgco2eq: waste_kgco2eq,
            first_seen: first,
            last_seen: last,
        });
    }
    out
}

fn default_disclaimers() -> Vec<String> {
    vec![
        "Directional estimate, not regulatory-grade.".to_string(),
        "Approximate uncertainty bracket: ~2x multiplicative.".to_string(),
        "Optimization potential excludes embodied hardware emissions (SCI M term).".to_string(),
        "Per-service carbon includes operational emissions only; embodied carbon (SCI M term) is reported in the aggregate total but not attributed per service.".to_string(),
        "Energy and carbon attribution per service is runtime-calibrated when the window's energy_model is non-empty; archives written before this feature shipped fall back to proportional I/O share.".to_string(),
        "Not suitable for CSRD or GHG Protocol Scope 3 reporting.".to_string(),
        "Methodology: ISO/IEC 21031:2024 (SCI).".to_string(),
    ]
}

fn write_pretty_json(report: &PeriodicReport, output: &Path) -> std::io::Result<()> {
    // Refuse to truncate a symlink. Residual TOCTOU between the check
    // and the open is accepted given the CLI is operator-driven.
    if let Ok(meta) = std::fs::symlink_metadata(output)
        && meta.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "output {} is a symlink; refusing to overwrite",
                output.display()
            ),
        ));
    }
    let file = std::fs::File::create(output)?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, report)?;
    use std::io::Write as _;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_disclaimer_added_for_internal_below_threshold() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_coverage(base, ReportIntent::Internal, 0.5);
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("50.0%"));
        let threshold_text = format!("{:.0}%", MIN_PERIOD_COVERAGE_FOR_OFFICIAL * 100.0);
        assert!(out[1].contains(&threshold_text));
        assert!(out[1].contains("Not suitable for official disclosure"));
    }

    #[test]
    fn coverage_disclaimer_omitted_for_internal_at_full_coverage() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_coverage(base.clone(), ReportIntent::Internal, 1.0);
        assert_eq!(out, base);
    }

    #[test]
    fn coverage_disclaimer_omitted_for_internal_exactly_at_threshold() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_coverage(
            base.clone(),
            ReportIntent::Internal,
            MIN_PERIOD_COVERAGE_FOR_OFFICIAL,
        );
        assert_eq!(out, base);
    }

    #[test]
    fn coverage_disclaimer_omitted_for_official_intent() {
        // Official below threshold is refused by the validator upstream,
        // but if we ever build the report (e.g. validator bypassed), this
        // branch must not add the internal-only disclaimer.
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_coverage(base.clone(), ReportIntent::Official, 0.5);
        assert_eq!(out, base);
    }

    #[test]
    fn binary_versions_disclaimer_omitted_for_single_version() {
        let base = vec!["existing".to_string()];
        let mut versions = std::collections::BTreeSet::new();
        versions.insert("0.6.2".to_string());
        let out = augment_disclaimers_for_binary_versions(base.clone(), &versions);
        assert_eq!(out, base);
    }

    #[test]
    fn binary_versions_disclaimer_omitted_for_empty_set() {
        let base = vec!["existing".to_string()];
        let versions = std::collections::BTreeSet::new();
        let out = augment_disclaimers_for_binary_versions(base.clone(), &versions);
        assert_eq!(out, base);
    }

    #[test]
    fn calibration_disclaimer_omitted_when_not_applied() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_calibration(base.clone(), false);
        assert_eq!(out, base);
    }

    #[test]
    fn calibration_disclaimer_added_when_applied() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_calibration(base, true);
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("Calibration applied"));
        assert!(out[1].contains("calibration_inputs.calibration_applied"));
    }

    #[test]
    fn binary_versions_disclaimer_added_for_multiple_versions() {
        let base = vec!["existing".to_string()];
        let mut versions = std::collections::BTreeSet::new();
        versions.insert("0.6.2".to_string());
        versions.insert("0.6.3".to_string());
        let out = augment_disclaimers_for_binary_versions(base, &versions);
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("0.6.2"));
        assert!(out[1].contains("0.6.3"));
        assert!(out[1].contains("multiple perf-sentinel binary versions"));
    }

    #[test]
    fn emit_attestation_produces_statement_with_matching_digest() {
        use sentinel_core::report::periodic::attestation::{
            IN_TOTO_STATEMENT_TYPE, InTotoStatement, PERF_SENTINEL_PREDICATE_TYPE,
        };
        use sentinel_core::report::periodic::compute_file_sha256_hex;

        let example = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("docs/schemas/examples/example-official-public-G2.json");
        let report: PeriodicReport =
            serde_json::from_str(&std::fs::read_to_string(&example).unwrap()).unwrap();

        let tmp = std::env::temp_dir().join(format!(
            "perf-sentinel-attestation-test-{}.json",
            std::process::id()
        ));
        let att = tmp.with_extension("intoto.jsonl");
        std::fs::write(&tmp, std::fs::read(&example).unwrap()).unwrap();

        write_attestation(&report, &tmp, &att, "subject.json").expect("write attestation");

        let statement_json = std::fs::read_to_string(&att).unwrap();
        let statement: InTotoStatement = serde_json::from_str(&statement_json).unwrap();
        assert_eq!(statement.statement_type, IN_TOTO_STATEMENT_TYPE);
        assert_eq!(statement.predicate_type, PERF_SENTINEL_PREDICATE_TYPE);
        assert_eq!(statement.subject[0].name, "subject.json");
        let expected_digest = compute_file_sha256_hex(&tmp).unwrap();
        assert_eq!(
            statement.subject[0].digest.get("sha256").unwrap(),
            &expected_digest
        );

        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&att);
    }
}
