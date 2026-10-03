use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, Utc};
use sentinel_core::report::periodic::AggregationError;
use sentinel_core::report::periodic::aggregator::aggregate_from_paths;
use sentinel_core::report::periodic::org_config::OrgConfig;
use sentinel_core::report::periodic::schema::{Confidentiality, Period, PeriodType, ReportIntent};
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
pub(crate) fn step_anchor(granularity: Granularity, anchor: NaiveDate, forward: bool) -> NaiveDate {
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
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
            PathBuf::from("org.toml"),
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
            PathBuf::from("org.toml"),
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
            PathBuf::from("org.toml"),
            false,
            Some((min, max)),
            d(2000, 1, 1),
        );
        state.cycle_granularity();
        state.cycle_granularity();
        state.cycle_granularity();
        assert_eq!(state.granularity(), Granularity::Custom);
        assert_eq!(state.resolved_dates(), (d(2026, 3, 10), d(2026, 3, 12)));
        // Push the From edge past To, and To follows so the range stays ordered.
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
            &[PathBuf::from("archive.ndjson")],
            Path::new("org.toml"),
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

    fn summary(coverage: f64, validator: ValidatorStatus) -> PreviewSummary {
        PreviewSummary {
            windows: 6,
            days_covered: 90,
            period_coverage: coverage,
            applications_measured: 8,
            applications_excluded: 1,
            total_requests: 54,
            total_carbon_kgco2eq: 0.0001,
            total_energy_kwh: 0.0,
            waste_ratio: 0.218,
            anti_patterns: 60,
            runtime_windows: 0,
            fallback_windows: 6,
            malformed_lines: 0,
            validator,
        }
    }

    #[test]
    fn ready_lines_render_all_validator_states() {
        // Official passes, coverage above threshold (Good branch).
        let pass = DiscloseState::ready_lines(&summary(0.82, ValidatorStatus::Pass));
        assert!(
            pass.iter()
                .any(|l| l.text.contains("Windows aggregated:  6"))
        );
        assert!(
            pass.iter()
                .any(|l| l.text.contains("Requests:") && l.text.contains("54"))
        );
        assert!(pass.iter().any(|l| l.text.contains("Pass")));
        // Official fails, coverage below threshold (Warn branch + errors).
        let fail = DiscloseState::ready_lines(&summary(
            0.4,
            ValidatorStatus::Fail(vec!["period coverage too low".to_string()]),
        ));
        assert!(fail.iter().any(|l| l.text.contains("Fail")));
        assert!(
            fail.iter()
                .any(|l| l.text.contains("period coverage too low"))
        );
        // Internal intent: validator not enforced.
        let na = DiscloseState::ready_lines(&summary(0.4, ValidatorStatus::NotApplicable));
        assert!(na.iter().any(|l| l.text.contains("Not enforced")));
    }

    #[test]
    fn scroll_clamps_within_summary() {
        let mut state = empty_state(d(2026, 5, 15));
        let max = u16::try_from(state.summary_lines().len().saturating_sub(1)).unwrap();
        state.scroll(true);
        state.scroll(true);
        state.scroll(true);
        assert_eq!(state.scroll_offset(), max);
        state.scroll(false);
        state.scroll(false);
        state.scroll(false);
        assert_eq!(state.scroll_offset(), 0);
    }

    #[test]
    fn step_month_moves_custom_edge_then_noops_elsewhere() {
        let min = "2026-03-10T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let max = "2026-03-20T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let mut state = DiscloseState::new(
            Vec::new(),
            sample_org(),
            PathBuf::from("org.toml"),
            false,
            Some((min, max)),
            d(2000, 1, 1),
        );
        state.cycle_granularity();
        state.cycle_granularity();
        state.cycle_granularity();
        assert_eq!(state.granularity(), Granularity::Custom);
        // Move the "to" edge one month forward. "from" stays unchanged.
        state.toggle_custom_field();
        state.step_month(true);
        assert_eq!(state.resolved_dates(), (d(2026, 3, 10), d(2026, 4, 20)));
        // step_month is a no-op outside Custom.
        state.cycle_granularity();
        let before = state.resolved_dates();
        state.step_month(true);
        assert_eq!(state.resolved_dates(), before);
    }

    #[test]
    fn equivalent_command_covers_all_value_arms() {
        let internal = equivalent_command(
            ReportIntent::Internal,
            Confidentiality::Internal,
            PeriodType::CalendarQuarter,
            d(2026, 1, 1),
            d(2026, 3, 31),
            &[PathBuf::from("a.ndjson")],
            Path::new("o.toml"),
        );
        assert!(internal.contains("--intent internal"));
        assert!(internal.contains("--confidentiality internal"));
        assert!(internal.contains("--period-type calendar-quarter"));

        let audited = equivalent_command(
            ReportIntent::Audited,
            Confidentiality::Public,
            PeriodType::CalendarYear,
            d(2026, 1, 1),
            d(2026, 12, 31),
            &[PathBuf::from("a.ndjson")],
            Path::new("o.toml"),
        );
        assert!(audited.contains("--intent audited"));
        assert!(audited.contains("--period-type calendar-year"));

        let custom = equivalent_command(
            ReportIntent::Official,
            Confidentiality::Public,
            PeriodType::Custom,
            d(2026, 1, 1),
            d(2026, 2, 15),
            &[PathBuf::from("a.ndjson")],
            Path::new("o.toml"),
        );
        assert!(custom.contains("--period-type custom"));
    }
}
