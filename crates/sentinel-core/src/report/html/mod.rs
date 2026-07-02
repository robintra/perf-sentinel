//! HTML dashboard sink (single-file output, vanilla JS, `textContent`-only).
//!
//! Emits a self-contained HTML file that renders a completed [`Report`]
//! as an interactive dashboard with Findings, Explain and (when green
//! scoring is enabled) `GreenOps` tabs.
//!
//! # Security model
//!
//! All user-controlled data is injected inside a
//! `<script id="report-data" type="application/json">` block and read
//! once at load time via `Element.textContent`. The bundled JS uses
//! `textContent` and `document.createElement()` exclusively and never
//! calls `innerHTML`, `insertAdjacentHTML`, `document.write`, `eval()`
//! or `new Function()`. The unit test
//! [`tests::no_forbidden_apis_in_template`] greps the template on every
//! build to enforce the rule.
//!
//! Additional defense: [`inject`] escapes the substring `</` in the
//! serialized JSON payload to `<\/` so a user-controlled value
//! (SQL template, HTTP URL, service name) cannot close the `<script>`
//! block early. `\/` is a permitted JSON string escape, so
//! `JSON.parse` recovers the original value unchanged.
//!
//! # Trace embedding
//!
//! Only traces that contain at least one finding are embedded (the empty
//! state in the Explain tab makes free navigation pointless). When
//! `max_traces_embedded` is `None`, the sink targets a ~5 MB HTML file
//! size by trimming the lowest-IIS traces first (top-waste fallback
//! reusing the `top_offenders` ordering). When the user sets
//! `max_traces_embedded` explicitly, that cap is honored exactly,
//! regardless of the size target.
//!
//! See `docs/design/07-CLI-CONFIG-RELEASE.md` for the full design
//! rationale.

use crate::correlate::Trace;
use crate::diff::DiffReport;
use crate::event::EventType;
use crate::ingest::pg_stat::PgStatReport;
use crate::normalize::NormalizedEvent;
use crate::report::Report;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const TEMPLATE: &str = include_str!("html_template.html");
const JSON_PLACEHOLDER: &str = "{{REPORT_JSON}}";
const TITLE_PLACEHOLDER: &str = "{{PAGE_TITLE}}";
const CSP_PLACEHOLDER: &str = "{{CONTENT_SECURITY_POLICY}}";
const BRAND_LOGO_PLACEHOLDER: &str = "{{BRAND_LOGO}}";
const FONT_FACES_PLACEHOLDER: &str = "{{FONT_FACES}}";
const DEFAULT_TITLE: &str = "perf-sentinel report";
// Geist + Geist Mono (OFL-1.1) Latin subset, embedded as base64 woff2 so the
// self-contained report renders the brand typeface offline, with no network
// fetch. Generated from the @fontsource woff2 subsets; the license text lives
// in `fonts-LICENSE.txt` beside it. Base64 alphabet contains no `{` so the
// double-brace guard below holds.
const FONT_FACES: &str = include_str!("fonts.css");
// Brand wordmark (horizontal lockup), embedded so the self-contained report
// needs no network fetch. `logo-horiz-light.svg` is the dark wordmark for
// light backgrounds; `logo-horiz-dark.svg` is the light wordmark for dark
// backgrounds. The template swaps them by `data-theme` in pure CSS. Kept
// inside this crate (not referenced from the repo-root `logo/`) so
// `cargo publish` packages them; an out-of-package `include_str!` would break
// the published crate's compile.
const BRAND_LOGO_LIGHT_SVG: &str = include_str!("logo-horiz-light.svg");
const BRAND_LOGO_DARK_SVG: &str = include_str!("logo-horiz-dark.svg");
const DEFAULT_SIZE_TARGET_BYTES: usize = 5 * 1024 * 1024;
/// Static-mode Content-Security-Policy. See `docs/design/07-CLI-CONFIG-RELEASE.md`
/// § "`STATIC_CSP` compile-time invariant" for the substitution-shadowing
/// guarantee enforced by the const block below.
const STATIC_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; \
                          style-src 'unsafe-inline'; img-src data:; \
                          font-src data:; base-uri 'none'; form-action 'none'";

/// Compile-time guard: a value substituted into the document before the JSON
/// marker (the CSP, the brand SVGs) must not contain `{{`, which would shadow
/// a later `{{...}}` placeholder during [`inject`].
const fn assert_no_double_brace(s: &str) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        assert!(
            !(bytes[i] == b'{' && bytes[i + 1] == b'{'),
            "embedded asset must not contain `{{{{`, it would shadow placeholder substitution"
        );
        i += 1;
    }
}
const _: () = {
    assert_no_double_brace(STATIC_CSP);
    assert_no_double_brace(BRAND_LOGO_LIGHT_SVG);
    assert_no_double_brace(BRAND_LOGO_DARK_SVG);
    // FONT_FACES is substituted first in `inject`, ahead of the JSON/CSP/
    // TITLE markers, so a stray `{{` in the embedded font CSS would shadow a
    // real placeholder. base64 has no `{`, but guard it like the siblings.
    assert_no_double_brace(FONT_FACES);
};
/// Embedded in every payload as the `version` field. Extracted from the
/// environment at compile time via `env!`, kept as a single constant so
/// the size-trim pass and the final build path cannot drift.
const PAYLOAD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Options controlling HTML rendering.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Label shown in the top bar (filename, `-` for stdin, etc.).
    pub input_label: String,
    /// Explicit cap on embedded traces. When `None`, the sink trims to
    /// fit [`DEFAULT_SIZE_TARGET_BYTES`] using the top-waste fallback.
    pub max_traces_embedded: Option<usize>,
    /// Optional `pg_stat_statements` report embedded alongside the
    /// analysis. When `Some`, the HTML dashboard exposes a `pg_stat` tab
    /// plus the Explain-to-`pg_stat` cross-navigation for matching SQL
    /// templates.
    pub pg_stat: Option<PgStatReport>,
    /// Optional diff against a baseline run embedded alongside the
    /// analysis. When `Some`, the HTML dashboard exposes a Diff tab
    /// with new/resolved findings, severity changes, and per-endpoint
    /// deltas.
    pub diff: Option<DiffReport>,
    /// When `Some`, the generated HTML enables live mode: the in-page
    /// JavaScript connects to the daemon at this URL for ack/revoke
    /// interactions, fetches the daemon-side acks listing, and shows a
    /// connection-status indicator. Reveals the auth-key prompt modal
    /// on a 401 response. The daemon must have CORS configured (see
    /// `[daemon.cors]` in CONFIGURATION.md) and the document origin
    /// allowed.
    ///
    /// When `None`, the HTML is purely static: no badge, no
    /// ack/revoke buttons, no acknowledgments panel, strict CSP with
    /// no `connect-src` directive.
    ///
    /// The URL is expected to have been validated by the caller. The
    /// renderer trusts it as-is and concatenates it into the
    /// Content-Security-Policy `connect-src` directive. Validation
    /// rejects userinfo, paths, query strings and ASCII control
    /// characters via `crates/sentinel-cli/src/ack.rs::validate_url`.
    /// The browser-side handlers (auth-key prompt, ack/revoke modal,
    /// fetch retry) live in the live-mode IIFE block at the bottom
    /// of `crates/sentinel-core/src/report/html_template.html`.
    pub daemon_url: Option<String>,
}

/// Counters describing how many candidate traces ended up embedded in
/// the rendered HTML. Returned by [`render`] so callers can surface a
/// trim notice to the user when `kept < total`. Field naming mirrors
/// the private `TrimSummary` struct used inside the JSON payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderStats {
    /// Number of traces actually embedded in the rendered HTML.
    pub kept: usize,
    /// Total candidate traces before the trace-level size or cap trim.
    /// Candidates come from the findings kept in the embed, so when the
    /// findings trim fires this is already conservative versus the full
    /// JSON report (deliberate: every embedded trace has its finding
    /// visible in the dashboard).
    pub total: usize,
}

/// Render a report to a self-contained HTML string and return how many
/// traces were embedded vs. how many were candidates.
///
/// # Panics
///
/// Panics if `serde_json` fails to serialize the payload. The payload
/// is built from `Serialize` types with only string and number keys,
/// so this can only happen on serde internal errors (out-of-memory and
/// similar system-level failures), not on user input.
///
/// # Examples
///
/// ```no_run
/// use sentinel_core::report::html::{render, RenderOptions};
/// use sentinel_core::pipeline::analyze_with_traces;
/// # fn load_events() -> Vec<sentinel_core::event::SpanEvent> { vec![] }
/// let events = load_events();
/// let cfg = sentinel_core::config::Config::default();
/// let (report, traces) = analyze_with_traces(events, &cfg);
/// let (html, _stats) = render(&report, &traces, &RenderOptions {
///     input_label: "traces.json".to_string(),
///     max_traces_embedded: None,
///     pg_stat: None,
///     diff: None,
///     daemon_url: None,
/// });
/// assert!(html.starts_with("<!DOCTYPE html>"));
/// ```
#[must_use]
pub fn render(report: &Report, traces: &[Trace], options: &RenderOptions) -> (String, RenderStats) {
    // Mixed-content guard: an http:// daemon URL on a non-loopback host
    // breaks ack/revoke fetches if the report is later served over https.
    if let Some(url) = options.daemon_url.as_deref()
        && let Some(rest) = url.strip_prefix("http://")
    {
        let host_only = rest.split(['/', ':']).next().unwrap_or("");
        let is_loopback =
            host_only == "localhost" || host_only == "127.0.0.1" || host_only == "[::1]";
        if !is_loopback {
            tracing::warn!(
                daemon_url = url,
                "http:// daemon URL on a non-loopback host: ack/revoke fetches will be blocked when the report is served over https://"
            );
        }
    }
    let sanitized_label = sanitize_input_label(&options.input_label);
    let (report_embed, trimmed_findings) = slim_report_for_embed(report, options);
    // Trace ranking reads the un-slimmed `top_offenders` so the ordering
    // is accurate even past the embed cap; the payload serializes the
    // slim report.
    let payload = build_payload_with_label(
        &report_embed,
        &report.green_summary.top_offenders,
        traces,
        options,
        &sanitized_label,
        trimmed_findings,
    );
    let kept = payload.embedded_traces.len();
    let total = payload.trimmed_traces.as_ref().map_or(kept, |s| s.total);
    // Serialization of our fixed-shape payload cannot fail: all nested
    // types are `Serialize`, every map key is `&'static str`, and there
    // are no non-string map keys anywhere in the tree. If a future
    // refactor introduces a `HashMap<NonStringKey, _>` anywhere under
    // `Payload`, `serde_json` will fail here at runtime. Keep the
    // payload's map keys `&'static str` or `String` only.
    let json = serde_json::to_string(&payload).expect("payload always serializes");
    let title = derive_page_title(&sanitized_label);
    let csp = build_csp(options.daemon_url.as_deref());
    let html = inject(&json, &title, &csp);
    (html, RenderStats { kept, total })
}

/// Render and write a rendered HTML dashboard to `output`.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] if the file cannot be
/// created or written.
///
/// # Panics
///
/// Panics if serialization fails for the same reason as [`render`].
pub fn write(
    report: &Report,
    traces: &[Trace],
    options: &RenderOptions,
    output: &Path,
) -> std::io::Result<()> {
    let (html, _stats) = render(report, traces, options);
    std::fs::write(output, html)
}

// --- internal ---

#[derive(Debug, Serialize)]
struct Payload<'a> {
    version: &'static str,
    input_label: &'a str,
    report: &'a Report,
    embedded_traces: Vec<EmbeddedTrace<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trimmed_traces: Option<TrimSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trimmed_findings: Option<TrimSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pg_stat: Option<&'a PgStatReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<&'a DiffReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon: Option<DaemonHandle<'a>>,
}

/// Live-mode handle embedded in the JSON payload. Presence flips the JS
/// boot path from "static" to "live": fetch ack data, reveal the
/// daemon-status badge, attach Ack/Revoke handlers. Field naming kept
/// short on purpose, the JSON is read at boot every time.
#[derive(Debug, Serialize)]
struct DaemonHandle<'a> {
    url: &'a str,
}

#[derive(Debug, Serialize)]
struct EmbeddedTrace<'a> {
    trace_id: &'a str,
    spans: Vec<EmbeddedSpan<'a>>,
}

#[derive(Debug, Serialize)]
struct EmbeddedSpan<'a> {
    span_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_span_id: Option<&'a str>,
    service: &'a str,
    endpoint: &'a str,
    event_type: &'static str,
    operation: &'a str,
    template: &'a str,
    duration_us: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_code: Option<u16>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct TrimSummary {
    kept: usize,
    total: usize,
}

/// Inject the CSP, page title and JSON payload into the template.
///
/// Escapes `</` to `<\/` in the JSON payload so a user-controlled
/// string cannot close the `<script>` block early. `\/` is a permitted
/// JSON string escape, so round-tripping through `JSON.parse` recovers
/// the original value. The title is already HTML-escaped by
/// [`derive_page_title`]. The CSP string is built by [`build_csp`] from
/// a static prefix and the validated daemon URL, no untrusted bytes
/// reach the meta tag.
///
/// Substitution order is critical and verified by
/// `hostile_input_label_with_json_placeholder_does_not_double_substitute`
/// and friends:
/// - the brand SVG is substituted first; it is trusted compile-time content
///   guaranteed `{{`-free (see [`assert_no_double_brace`]), so it cannot lay
///   down a fake placeholder for the later passes to match;
/// - the JSON payload is substituted before the title, so a hostile
///   `input_label` carrying `{{REPORT_JSON}}` (injected only at the title
///   pass) cannot trigger a second JSON substitution;
/// - the CSP and title markers sit in `<head>`, ahead of both the JSON block
///   and the brand marker, so a hostile title or JSON payload cannot shadow
///   the static `replacen(..., 1)` matches.
fn inject(json: &str, title: &str, csp: &str) -> String {
    // Defense-in-depth: a `{{` byte sequence in the CSP would shadow a
    // template placeholder during the title substitution. `validate_url`
    // rejects bytes `hyper::Uri` does not accept in a host so the check
    // holds today; plain `assert!` keeps the safety net in release.
    assert!(
        !csp.contains("{{"),
        "CSP must not contain `{{{{` placeholder bytes, got: {csp}"
    );
    let safe = json.replace("</", "<\\/");
    // Brand wordmark as raw inline SVG (light + dark variants), substituted
    // into the static <span> in the topbar. Server-side substitution, not a
    // runtime `innerHTML`, so the template keeps its textContent-only XSS
    // invariant. The SVG is trusted compile-time content and lives in the
    // body (not a <script>), so no `</` escaping is needed. Substituted
    // before the report JSON so a hostile `{{BRAND_LOGO}}` inside report
    // content cannot shadow this one.
    let brand_logo = format!(
        "<span class=\"ps-logo ps-logo-light\">{BRAND_LOGO_LIGHT_SVG}</span>\
         <span class=\"ps-logo ps-logo-dark\">{BRAND_LOGO_DARK_SVG}</span>"
    );
    TEMPLATE
        // Font faces first: trusted base64 in the <head> <style>, substituted
        // before the report JSON so a hostile `{{FONT_FACES}}` inside report
        // content cannot shadow it. The base64 alphabet has no `{`.
        .replacen(FONT_FACES_PLACEHOLDER, FONT_FACES, 1)
        .replacen(BRAND_LOGO_PLACEHOLDER, &brand_logo, 1)
        .replacen(JSON_PLACEHOLDER, &safe, 1)
        .replacen(CSP_PLACEHOLDER, csp, 1)
        .replacen(TITLE_PLACEHOLDER, title, 1)
}

/// Build the Content-Security-Policy string for a render call. In
/// static mode, returns the historical strict policy verbatim. In live
/// mode, appends `connect-src 'self' <daemon_url>` so the in-page
/// JavaScript can `fetch()` the daemon AND any same-origin asset (a
/// future template change adding a same-origin fetch will not silently
/// break under the strict CSP). The caller validates the URL upstream
/// (the CLI runs it through `validate_url` and rejects userinfo, paths,
/// query strings, ASCII control characters), so no CSP-breaking byte
/// (single quote, semicolon, whitespace, curly braces) can land in the
/// directive value. The `inject` `debug_assert!(!csp.contains("{{"))`
/// is the load-bearing fallback in case `validate_url` is ever
/// relaxed.
#[must_use]
fn build_csp(daemon_url: Option<&str>) -> String {
    match daemon_url {
        Some(url) => format!("{STATIC_CSP}; connect-src 'self' {url}"),
        None => STATIC_CSP.to_string(),
    }
}

/// Derive the `<title>` text from the user-supplied `input_label`.
///
/// Strips any path components, HTML-escapes the filename, and formats
/// as `perf-sentinel: <filename>`. Falls back to a fixed string when
/// the label is empty or `-` (stdin).
fn derive_page_title(input_label: &str) -> String {
    let trimmed = input_label.trim();
    if trimmed.is_empty() || trimmed == "-" {
        return DEFAULT_TITLE.to_string();
    }
    let filename = Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(trimmed);
    format!("perf-sentinel: {}", html_escape_text(filename))
}

/// Strip control and unsafe-format characters from `input_label`
/// before it lands in the JSON payload. The topbar renders the value
/// via `textContent`, so there is no XSS risk, but a leaked `BiDi`
/// override would still flip the visible order of surrounding text.
fn sanitize_input_label(input_label: &str) -> String {
    input_label
        .chars()
        .filter(|c| !c.is_control() && !is_unsafe_format_char(*c))
        .collect()
}

/// Minimal HTML escape for the title text. `<title>` is a raw-text
/// element, so only `&` and `<` strictly need escaping, but we also
/// escape `>` and the two quote characters for belt-and-braces safety.
/// Control characters (Unicode Cc, plus the known `BiDi` and
/// line/paragraph-separator format codes that some terminals and
/// browsers honor) are dropped so a hostile filename cannot inject
/// cosmetic payloads into the browser tab.
fn html_escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() || is_unsafe_format_char(c) => {}
            _ => out.push(c),
        }
    }
    out
}

/// Unicode format characters that carry cosmetic payloads: `BiDi`
/// override and isolate marks, line/paragraph separators, and the
/// byte-order mark. `char::is_control` only catches the `Cc` category,
/// so we filter these `Cf` entries by hand.
fn is_unsafe_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200E}' // LEFT-TO-RIGHT MARK
        | '\u{200F}' // RIGHT-TO-LEFT MARK
        | '\u{2028}' // LINE SEPARATOR
        | '\u{2029}' // PARAGRAPH SEPARATOR
        | '\u{202A}'..='\u{202E}' // LRE / RLE / PDF / LRO / RLO
        | '\u{2066}'..='\u{2069}' // LRI / RLI / FSI / PDI
        | '\u{FEFF}' // BYTE ORDER MARK
    )
}

fn build_payload_with_label<'a>(
    report: &'a Report,
    full_top_offenders: &[crate::report::TopOffender],
    traces: &'a [Trace],
    options: &'a RenderOptions,
    input_label: &'a str,
    trimmed_findings: Option<TrimSummary>,
) -> Payload<'a> {
    // Candidate set from the embedded findings (so every embedded trace
    // has its finding shown), ranked by the full offender list (so the
    // ordering does not degrade past the embed cap).
    let ordered = order_candidates_by_iis(&report.findings, full_top_offenders, traces);
    let total = ordered.len();

    let (kept_refs, trimmed) = if let Some(cap) = options.max_traces_embedded {
        let take = cap.min(total);
        let summary = if take < total {
            Some(TrimSummary { kept: take, total })
        } else {
            None
        };
        (ordered.into_iter().take(take).collect::<Vec<_>>(), summary)
    } else {
        trim_to_size_target(
            ordered,
            report,
            options,
            input_label,
            trimmed_findings.clone(),
        )
    };

    let embedded_traces = kept_refs.iter().copied().map(embed_trace).collect();

    Payload {
        version: PAYLOAD_VERSION,
        input_label,
        report,
        embedded_traces,
        trimmed_traces: trimmed,
        trimmed_findings,
        pg_stat: options.pg_stat.as_ref(),
        diff: options.diff.as_ref(),
        daemon: options
            .daemon_url
            .as_deref()
            .map(|url| DaemonHandle { url }),
    }
}

/// Filter traces to those referenced by a finding and sort by
/// per-trace IIS (highest first). Lower `top_offenders` index means
/// higher IIS. Traces whose `(service, endpoint)` pairs are absent from
/// `top_offenders` rank as `usize::MAX` and sort last.
fn order_candidates_by_iis<'a>(
    findings: &[crate::detect::Finding],
    top_offenders: &[crate::report::TopOffender],
    traces: &'a [Trace],
) -> Vec<&'a Trace> {
    let finding_trace_ids: HashSet<&str> = findings.iter().map(|f| f.trace_id.as_str()).collect();

    let mut rank: HashMap<(&str, &str), usize> = HashMap::new();
    for (i, off) in top_offenders.iter().enumerate() {
        rank.insert((off.service.as_str(), off.endpoint.as_str()), i);
    }

    let mut scored: Vec<(usize, &'a Trace)> = traces
        .iter()
        .filter(|t| finding_trace_ids.contains(t.trace_id.as_str()))
        .map(|t| (trace_rank(t, &rank), t))
        .collect();
    scored.sort_by_key(|(score, _)| *score);
    scored.into_iter().map(|(_, t)| t).collect()
}

fn trace_rank(trace: &Trace, rank: &HashMap<(&str, &str), usize>) -> usize {
    trace
        .spans
        .iter()
        .map(|s| {
            rank.get(&(s.event.service.as_ref(), s.event.source.endpoint.as_str()))
                .copied()
                .unwrap_or(usize::MAX)
        })
        .min()
        .unwrap_or(usize::MAX)
}

/// Findings share of the JSON budget when the sink targets a file size.
/// Traces get whatever remains; without this bound a large batch (tens of
/// thousands of findings) ships a multi-MB envelope no matter how many
/// traces are trimmed.
const FINDINGS_BUDGET_SHARE_PCT: usize = 70;

/// Cap on `green_summary.top_offenders` embedded in the HTML payload. The
/// dashboard only ever reads `top_offenders[0]` (the "Top offender" card),
/// so a high-endpoint-cardinality report would otherwise embed thousands
/// of rows nothing renders. The full ranking still drives trace ordering
/// (from the un-slimmed report) and stays in `analyze --format json`. The
/// cap leaves headroom for a future top-N table without re-bloating.
const TOP_OFFENDERS_EMBED_CAP: usize = 25;

/// Build the slimmed `Report` embedded in the HTML payload. Three sections
/// the dashboard does not fully render are bounded so a high-volume report
/// does not bloat the self-contained file, while `analyze --format json`
/// keeps every one of them in full:
///   - `findings`: trimmed critical-first when over the size budget
///     (surfaced as a banner), full otherwise;
///   - `per_endpoint_io_ops`: dropped entirely (no dashboard view reads it);
///   - `green_summary.top_offenders`: capped to [`TOP_OFFENDERS_EMBED_CAP`].
fn slim_report_for_embed(
    report: &Report,
    options: &RenderOptions,
) -> (Report, Option<TrimSummary>) {
    let (findings, trimmed_findings) = select_embedded_findings(report, options);
    // Clone-then-truncate: the transient full clone is freed immediately,
    // and a one-shot HTML render is not a hot path. What matters is that
    // the serialized payload carries at most the cap.
    let mut green_summary = report.green_summary.clone();
    green_summary
        .top_offenders
        .truncate(TOP_OFFENDERS_EMBED_CAP);
    // Exhaustive literal, not `report.clone()`: it makes the dropped
    // `per_endpoint_io_ops` explicit (never cloning the big vec) and turns
    // a future new `Report` field into a compile error here.
    let embed = Report {
        analysis: report.analysis.clone(),
        findings,
        green_summary,
        quality_gate: report.quality_gate.clone(),
        per_endpoint_io_ops: Vec::new(),
        correlations: report.correlations.clone(),
        warnings: report.warnings.clone(),
        warning_details: report.warning_details.clone(),
        acknowledged_findings: report.acknowledged_findings.clone(),
        binary_version: report.binary_version.clone(),
        disclosure_waste: report.disclosure_waste.clone(),
    };
    (embed, trimmed_findings)
}

/// Select the findings to embed. Critical findings are kept first, then
/// warning, then info, preserving the canonical report order inside each
/// band. Returns the full set (and no summary) when `--max-traces-embedded`
/// opts out of size targeting or the set already fits; otherwise the
/// critical-first prefix that fits, with a [`TrimSummary`].
fn select_embedded_findings(
    report: &Report,
    options: &RenderOptions,
) -> (Vec<crate::detect::Finding>, Option<TrimSummary>) {
    if options.max_traces_embedded.is_some() {
        return (report.findings.clone(), None);
    }
    let json_budget = DEFAULT_SIZE_TARGET_BYTES.saturating_sub(TEMPLATE.len());
    let findings_budget = json_budget * FINDINGS_BUDGET_SHARE_PCT / 100;
    // Serialize each finding exactly once: the same sizes serve the
    // whole-array early exit (sum + commas + brackets) and the budget
    // loop below, instead of serializing the array a second time.
    let sizes: Vec<usize> = report
        .findings
        .iter()
        .map(|f| serde_json::to_string(f).map_or(usize::MAX, |s| s.len()))
        .collect();
    let total_len = sizes
        .iter()
        .fold(2usize, |acc, len| acc.saturating_add(len.saturating_add(1)));
    if total_len <= findings_budget {
        return (report.findings.clone(), None);
    }

    let mut order: Vec<usize> = (0..report.findings.len()).collect();
    // Stable sort on the derived Severity ordering (Critical < Warning
    // < Info): severity bands first, canonical order within a band.
    order.sort_by_key(|&i| &report.findings[i].severity);

    let mut running = 2usize; // the [] array brackets
    let mut keep: Vec<usize> = Vec::new();
    for &i in &order {
        let next = running.saturating_add(sizes[i].saturating_add(1));
        if next > findings_budget {
            break;
        }
        running = next;
        keep.push(i);
    }
    keep.sort_unstable();

    let summary = TrimSummary {
        kept: keep.len(),
        total: report.findings.len(),
    };
    let kept = keep
        .into_iter()
        .map(|i| report.findings[i].clone())
        .collect();
    (kept, Some(summary))
}

/// Greedy trim-to-size loop: serialize, measure, drop the lowest-ranked
/// trace if over budget. Bounded by the number of input traces. On
/// realistic inputs (few dozen traces, report JSON under ~200 KB) the
/// first iteration usually fits and no trimming happens.
fn trim_to_size_target<'a>(
    ordered: Vec<&'a Trace>,
    report: &Report,
    options: &'a RenderOptions,
    input_label: &'a str,
    trimmed_findings: Option<TrimSummary>,
) -> (Vec<&'a Trace>, Option<TrimSummary>) {
    let total = ordered.len();

    // Serialize each embedded trace once and the non-trace envelope
    // once, then prefix-sum scan for the longest trace prefix that
    // fits under the size target: O(N * avg_trace_size) total, unlike
    // re-serializing the whole payload per shed trace, which is O(N^2).

    // Step 1: per-trace JSON sizes. We account for the surrounding
    // comma and the 2 literal bracket bytes of the JSON array via
    // `separator_overhead` below.
    let per_trace_lens: Vec<usize> = ordered
        .iter()
        .copied()
        .map(|t| serde_json::to_string(&embed_trace(t)).map_or(usize::MAX, |s| s.len()))
        .collect();

    // Step 2: envelope size. Build a payload whose `embedded_traces`
    // is empty, serialize it once, and use its length as the fixed
    // overhead that every kept-trace count shares. `trimmed_traces`
    // is set to a placeholder with realistic digits so its JSON
    // length is not under-reported (the actual value is written back
    // in `build_payload_with_label` after trimming), and the real
    // `trimmed_findings` rides along for the same reason.
    let envelope = Payload {
        version: PAYLOAD_VERSION,
        input_label,
        report,
        embedded_traces: Vec::new(),
        trimmed_traces: Some(TrimSummary { kept: 0, total }),
        trimmed_findings,
        pg_stat: options.pg_stat.as_ref(),
        diff: options.diff.as_ref(),
        daemon: options
            .daemon_url
            .as_deref()
            .map(|url| DaemonHandle { url }),
    };
    let envelope_len = serde_json::to_string(&envelope).map_or(usize::MAX, |s| s.len());

    // Budget for the serialized JSON payload: the template is a fixed
    // cost on every output. If TEMPLATE.len() already exceeds the
    // target (implausible), we return an empty set rather than
    // underflow.
    let json_budget = DEFAULT_SIZE_TARGET_BYTES.saturating_sub(TEMPLATE.len());

    // Find the largest prefix of `ordered` whose combined size fits
    // under the budget. Each trace contributes `len + 1` (for the
    // comma separator); the two empty-array bytes `[]` are already
    // included in `envelope_len`.
    let mut running = envelope_len;
    let mut keep_count: usize = 0;
    for &len in &per_trace_lens {
        let delta = len.saturating_add(1);
        let next = running.saturating_add(delta);
        if next > json_budget {
            break;
        }
        running = next;
        keep_count += 1;
    }

    let kept: Vec<&'a Trace> = ordered.into_iter().take(keep_count).collect();
    let trimmed = if kept.len() < total {
        Some(TrimSummary {
            kept: kept.len(),
            total,
        })
    } else {
        None
    };
    (kept, trimmed)
}

fn embed_trace(t: &Trace) -> EmbeddedTrace<'_> {
    EmbeddedTrace {
        trace_id: t.trace_id.as_str(),
        spans: t.spans.iter().map(embed_span).collect(),
    }
}

fn embed_span(e: &NormalizedEvent) -> EmbeddedSpan<'_> {
    EmbeddedSpan {
        span_id: e.event.span_id.as_str(),
        parent_span_id: e.event.parent_span_id.as_deref(),
        service: e.event.service.as_ref(),
        endpoint: e.event.source.endpoint.as_str(),
        event_type: match e.event.event_type {
            EventType::Sql => "sql",
            EventType::HttpOut => "http_out",
        },
        operation: e.event.operation.as_str(),
        // Only the masked template is embedded. The raw event.target (the
        // original db.statement / URL) carries literals and must never reach
        // the HTML payload. The JS falls back template-first, so dropping it
        // loses nothing observable.
        template: e.template.as_ref(),
        duration_us: e.event.duration_us,
        status_code: e.event.status_code,
    }
}

#[cfg(test)]
mod tests;
