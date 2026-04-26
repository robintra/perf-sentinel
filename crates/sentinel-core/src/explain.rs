//! Explain mode: builds a tree view of a trace with findings annotated inline.

use crate::correlate::Trace;
use crate::detect::Finding;
use crate::normalize::NormalizedEvent;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A node in the explain tree, representing a single span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanNode {
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub service: String,
    pub template: String,
    pub operation: String,
    pub duration_us: u64,
    pub timestamp: String,
    pub children: Vec<SpanNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<InlineFinding>,
}

/// A finding annotated on a span node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineFinding {
    #[serde(rename = "type")]
    pub finding_type: String,
    pub severity: String,
    pub occurrences: usize,
    pub suggestion: String,
    /// Framework-specific actionable fix carried over from the source
    /// finding so the tree view can render it inline. Absent when the
    /// finding had no inferred framework.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<crate::detect::suggestions::SuggestedFix>,
    /// Source code location carried over from the source finding.
    /// Absent when the instrumentation agent did not emit `code.*`
    /// span attributes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_location: Option<crate::event::CodeLocation>,
}

impl InlineFinding {
    /// Convert a full [`Finding`] into a compact inline representation.
    fn from_finding(f: &Finding) -> Self {
        Self {
            finding_type: f.finding_type.as_str().to_string(),
            severity: f.severity.as_str().to_string(),
            occurrences: f.pattern.occurrences,
            suggestion: f.suggestion.clone(),
            suggested_fix: f.suggested_fix.clone(),
            code_location: f.code_location.clone(),
        }
    }
}

/// The complete explain tree for a trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainTree {
    pub trace_id: String,
    /// Trace-level findings that cannot be anchored to a single span.
    ///
    /// The span-template-based annotation path attaches findings to spans
    /// whose normalized template matches `finding.pattern.template`. Some
    /// detectors (chatty service, pool saturation, serialized calls) emit
    /// findings whose `pattern.template` is a service name or an entry
    /// endpoint rather than a span template, so the match finds nothing.
    /// Those findings land here instead of being silently dropped from the
    /// tree view.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_level_findings: Vec<InlineFinding>,
    pub roots: Vec<SpanNode>,
}

/// Build an explain tree from a trace and its findings.
#[must_use]
pub fn build_tree(trace: &Trace, findings: &[Finding]) -> ExplainTree {
    // Collect the set of span templates that actually appear in the trace.
    // Used both to index span-anchored findings and to decide which findings
    // fall through to the trace-level bucket.
    let span_templates: HashSet<&str> = trace.spans.iter().map(|s| s.template.as_str()).collect();

    // Index findings by template for quick lookup (span-anchored path).
    // Any finding whose template does not match a span template is collected
    // separately into `trace_level_findings` so the tree view can still
    // surface it at the top instead of dropping it silently.
    let mut findings_by_template: HashMap<&str, Vec<&Finding>> = HashMap::new();
    let mut trace_level_findings: Vec<InlineFinding> = Vec::new();
    for f in findings {
        if f.trace_id != trace.trace_id {
            continue;
        }
        if span_templates.contains(f.pattern.template.as_str()) {
            findings_by_template
                .entry(f.pattern.template.as_str())
                .or_default()
                .push(f);
        } else {
            trace_level_findings.push(InlineFinding::from_finding(f));
        }
    }

    // Build span nodes
    let nodes: Vec<SpanNode> = trace
        .spans
        .iter()
        .map(|span| make_node(span, &findings_by_template))
        .collect();

    // Index by span_id
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        by_id.insert(node.span_id.as_str(), i);
    }

    // Build tree: collect children, identify roots
    let mut children_map: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();

    for (i, span) in trace.spans.iter().enumerate() {
        if let Some(ref parent_id) = span.event.parent_span_id
            && let Some(&parent_idx) = by_id.get(parent_id.as_str())
        {
            children_map.entry(parent_idx).or_default().push(i);
            continue;
        }
        roots.push(i);
    }

    // Recursive tree assembly (with depth guard against stack overflow)
    let mut nodes: Vec<Option<SpanNode>> = nodes.into_iter().map(Some).collect();
    let root_nodes = roots
        .iter()
        .map(|&idx| assemble_node(idx, &mut nodes, &children_map, 0))
        .collect();

    ExplainTree {
        trace_id: trace.trace_id.clone(),
        trace_level_findings,
        roots: root_nodes,
    }
}

fn make_node(
    span: &NormalizedEvent,
    findings_by_template: &HashMap<&str, Vec<&Finding>>,
) -> SpanNode {
    let inline_findings = findings_by_template
        .get(span.template.as_str())
        .map(|fs| fs.iter().map(|f| InlineFinding::from_finding(f)).collect())
        .unwrap_or_default();

    SpanNode {
        span_id: span.event.span_id.clone(),
        parent_span_id: span.event.parent_span_id.clone(),
        service: span.event.service.clone(),
        template: span.template.clone(),
        operation: span.event.operation.clone(),
        duration_us: span.event.duration_us,
        timestamp: span.event.timestamp.clone(),
        children: Vec::new(),
        findings: inline_findings,
    }
}

/// Maximum tree depth to prevent stack overflow on deeply nested traces.
const MAX_TREE_DEPTH: usize = 256;

fn assemble_node(
    idx: usize,
    nodes: &mut [Option<SpanNode>],
    children_map: &HashMap<usize, Vec<usize>>,
    depth: usize,
) -> SpanNode {
    // Guard against cyclic parent references: if the node was already taken,
    // return a minimal placeholder instead of panicking.
    let Some(mut node) = nodes[idx].take() else {
        return SpanNode {
            span_id: String::new(),
            parent_span_id: None,
            service: String::new(),
            template: "(cycle detected)".to_string(),
            operation: String::new(),
            duration_us: 0,
            timestamp: String::new(),
            children: Vec::new(),
            findings: Vec::new(),
        };
    };
    if depth < MAX_TREE_DEPTH
        && let Some(child_indices) = children_map.get(&idx)
    {
        node.children = child_indices
            .iter()
            .map(|&ci| assemble_node(ci, nodes, children_map, depth + 1))
            .collect();
        node.children.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    }
    node
}

/// ANSI color codes for tree formatting.
struct TreeColors {
    bold: &'static str,
    cyan: &'static str,
    red: &'static str,
    yellow: &'static str,
    dim: &'static str,
    reset: &'static str,
}

/// Format the explain tree as colored terminal text.
#[must_use]
pub fn format_tree_text(tree: &ExplainTree, use_color: bool) -> String {
    use std::fmt::Write;

    let colors = if use_color {
        TreeColors {
            bold: "\x1b[1m",
            cyan: "\x1b[36m",
            red: "\x1b[31m",
            yellow: "\x1b[33m",
            dim: "\x1b[2m",
            reset: "\x1b[0m",
        }
    } else {
        TreeColors {
            bold: "",
            cyan: "",
            red: "",
            yellow: "",
            dim: "",
            reset: "",
        }
    };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}{cyan}Trace {}{}",
        colors.bold,
        tree.trace_id,
        colors.reset,
        cyan = colors.cyan
    );

    format_trace_level_findings(&mut out, &tree.trace_level_findings, &colors);

    for (i, root) in tree.roots.iter().enumerate() {
        let is_last = i == tree.roots.len() - 1;
        format_node(&mut out, root, "", is_last, &colors, 0);
    }

    out
}

/// Render the trace-level findings header section, if any.
///
/// Emitted between the `Trace <id>` header and the span tree, so the reader
/// sees whole-trace issues (chatty service, pool saturation, serialized
/// calls) before drilling into individual spans.
fn format_trace_level_findings(out: &mut String, findings: &[InlineFinding], c: &TreeColors) {
    use crate::text_safety::sanitize_for_terminal;
    use std::fmt::Write;

    if findings.is_empty() {
        return;
    }

    let _ = writeln!(
        out,
        "{}{}\u{26a0} Trace-level findings:{}",
        c.bold, c.yellow, c.reset,
    );
    for f in findings {
        let severity_color = match f.severity.as_str() {
            "critical" => c.red,
            "warning" => c.yellow,
            _ => c.dim,
        };
        let _ = writeln!(
            out,
            "  {severity_color}\u{2022} {} {} (\u{00d7}{}){}",
            sanitize_for_terminal(&f.finding_type.replace('_', " ")),
            sanitize_for_terminal(&f.severity),
            f.occurrences,
            c.reset,
        );
        write_finding_details(out, "      ", f, c);
    }
    let _ = writeln!(out);
}

/// Indented sub-lines under an annotated finding (suggestion, optional
/// fix and code location), using `├─` / `└─` connectors.
fn write_finding_details(out: &mut String, prefix: &str, f: &InlineFinding, c: &TreeColors) {
    use crate::text_safety::{safe_url, sanitize_for_terminal};
    use std::fmt::Write;

    let mut lines: Vec<String> = Vec::with_capacity(3);
    lines.push(format!(
        "suggestion: {}",
        sanitize_for_terminal(&f.suggestion)
    ));
    if let Some(ref fix) = f.suggested_fix {
        let url_part = match fix.reference_url.as_deref().and_then(safe_url) {
            Some(u) => format!(" ({u})"),
            None => String::new(),
        };
        lines.push(format!(
            "fix [{}]: {}{url_part}",
            sanitize_for_terminal(&fix.framework),
            sanitize_for_terminal(&fix.recommendation),
        ));
    }
    if let Some(ref loc) = f.code_location {
        let s = loc.display_string();
        if !s.is_empty() {
            lines.push(format!("location: {}", sanitize_for_terminal(&s)));
        }
    }

    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.iter().enumerate() {
        let connector = if i == last {
            "\u{2514}\u{2500}"
        } else {
            "\u{251c}\u{2500}"
        };
        let _ = writeln!(out, "{prefix}{}{connector} {line}{}", c.dim, c.reset);
    }
}

fn format_node(
    out: &mut String,
    node: &SpanNode,
    prefix: &str,
    is_last: bool,
    c: &TreeColors,
    depth: usize,
) {
    use crate::text_safety::sanitize_for_terminal;
    use std::fmt::Write;

    let connector = if is_last {
        "\u{2514}\u{2500} "
    } else {
        "\u{251c}\u{2500} "
    };
    let duration_str = format_duration(node.duration_us);

    let _ = write!(
        out,
        "{prefix}{connector}{}{}{} {}({duration_str}){}",
        c.bold,
        sanitize_for_terminal(&node.template),
        c.reset,
        c.dim,
        c.reset,
    );

    // Annotate findings inline
    for f in &node.findings {
        let severity_color = match f.severity.as_str() {
            "critical" => c.red,
            "warning" => c.yellow,
            _ => c.dim,
        };
        let _ = write!(
            out,
            " {severity_color}\u{2190} {} {} (\u{00d7}{}){}",
            sanitize_for_terminal(&f.finding_type.replace('_', " ")),
            sanitize_for_terminal(&f.severity),
            f.occurrences,
            c.reset,
        );
    }
    out.push('\n');

    // Compute child prefix once for suggestions and child recursion
    let child_prefix = if is_last {
        format!("{prefix}   ")
    } else {
        format!("{prefix}\u{2502}  ")
    };

    let detail_prefix = format!("{child_prefix}  ");
    for f in &node.findings {
        write_finding_details(out, &detail_prefix, f, c);
    }

    if depth < MAX_TREE_DEPTH {
        for (i, child) in node.children.iter().enumerate() {
            let child_is_last = i == node.children.len() - 1;
            format_node(out, child, &child_prefix, child_is_last, c, depth + 1);
        }
    }
}

fn format_duration(us: u64) -> String {
    if us < 1000 {
        format!("{us}\u{00b5}s")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1000.0)
    } else {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    }
}

/// Format the explain tree as a JSON value.
///
/// # Errors
///
/// Returns an error if the tree cannot be serialized.
pub fn format_tree_json(tree: &ExplainTree) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Confidence, FindingType, Pattern, Severity};
    use crate::test_helpers::{make_sql_event, make_trace};

    fn make_finding_for(trace_id: &str, template: &str) -> Finding {
        Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Critical,
            trace_id: trace_id.to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/{id}/submit".to_string(),
            pattern: Pattern {
                template: template.to_string(),
                occurrences: 6,
                window_ms: 200,
                distinct_params: 6,
            },
            suggestion: "Use WHERE order_id IN (?)".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
            code_location: None,
            instrumentation_scopes: Vec::new(),
            suggested_fix: None,
        }
    }

    #[test]
    fn build_tree_single_root() {
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT * FROM order_item WHERE order_id = 42",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);
        let tree = build_tree(&trace, &[]);

        assert_eq!(tree.trace_id, "trace-1");
        assert_eq!(tree.roots.len(), 1);
        assert!(tree.roots[0].children.is_empty());
    }

    #[test]
    fn build_tree_with_children() {
        let mut events = Vec::new();
        let mut root = make_sql_event("trace-1", "root", "SELECT 1", "2025-07-10T14:32:01.000Z");
        root.parent_span_id = None;
        events.push(root);

        for i in 1..=3 {
            let mut child = make_sql_event(
                "trace-1",
                &format!("child-{i}"),
                &format!("SELECT * FROM t WHERE id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
            );
            child.parent_span_id = Some("root".to_string());
            events.push(child);
        }

        let trace = make_trace(events);
        let tree = build_tree(&trace, &[]);

        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.roots[0].children.len(), 3);
    }

    #[test]
    fn build_tree_with_findings() {
        let events = crate::test_helpers::make_sql_series_events(5);
        let trace = make_trace(events);

        let finding = make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        let tree = build_tree(&trace, &[finding]);

        // All spans share the same template, so all should have the finding
        let has_finding = tree.roots.iter().any(|r| !r.findings.is_empty());
        assert!(
            has_finding,
            "at least one span should have findings attached"
        );
    }

    #[test]
    fn format_tree_text_no_panic() {
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT * FROM order_item WHERE order_id = 42",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);
        let tree = build_tree(&trace, &[]);

        let text = format_tree_text(&tree, false);
        assert!(text.contains("trace-1"));
        assert!(text.contains("SELECT * FROM order_item WHERE order_id = ?"));
    }

    #[test]
    fn format_tree_json_roundtrip() {
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT 1",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);
        let tree = build_tree(&trace, &[]);

        let json = format_tree_json(&tree).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["trace_id"], "trace-1");
    }

    #[test]
    fn format_duration_microseconds() {
        assert_eq!(format_duration(500), "500\u{00b5}s");
        assert_eq!(format_duration(1200), "1.2ms");
        assert_eq!(format_duration(2_500_000), "2.50s");
    }

    #[test]
    fn format_tree_text_with_color() {
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT * FROM order_item WHERE order_id = 42",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);
        let finding = make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        let tree = build_tree(&trace, &[finding]);

        let text = format_tree_text(&tree, true);
        // Should contain ANSI escape codes
        assert!(text.contains("\x1b[1m"), "should contain bold ANSI code");
        assert!(text.contains("\x1b[36m"), "should contain cyan ANSI code");
    }

    /// Build a chatty-service-style finding whose `pattern.template` is the
    /// entry endpoint (as real detectors emit it), not a span template.
    fn make_chatty_finding(trace_id: &str) -> Finding {
        Finding {
            finding_type: FindingType::ChattyService,
            severity: Severity::Warning,
            trace_id: trace_id.to_string(),
            service: "gateway-svc".to_string(),
            source_endpoint: "GET /api/dashboard/home".to_string(),
            pattern: Pattern {
                template: "GET /api/dashboard/home".to_string(),
                occurrences: 16,
                window_ms: 300,
                distinct_params: 16,
            },
            suggestion: "Consider aggregating calls with a BFF layer".to_string(),
            first_timestamp: "2025-07-10T14:32:00.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:00.300Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
            code_location: None,
            instrumentation_scopes: Vec::new(),
            suggested_fix: None,
        }
    }

    #[test]
    fn trace_level_finding_routed_to_header() {
        // A single SQL span with a template that does NOT match the chatty
        // finding's entry-endpoint template.
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT * FROM users WHERE id = 42",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);

        let finding = make_chatty_finding("trace-1");
        let tree = build_tree(&trace, &[finding]);

        assert!(
            tree.roots[0].findings.is_empty(),
            "chatty finding should not land on the SQL span"
        );
        assert_eq!(
            tree.trace_level_findings.len(),
            1,
            "chatty finding should land in trace_level_findings"
        );
        assert_eq!(tree.trace_level_findings[0].finding_type, "chatty_service");
        assert_eq!(tree.trace_level_findings[0].severity, "warning");
    }

    #[test]
    fn span_anchored_and_trace_level_coexist() {
        // Five matching SQL spans trigger a span-anchored N+1 SQL finding,
        // alongside a trace-level chatty finding. Both should be captured.
        let events = crate::test_helpers::make_sql_series_events(5);
        let trace = make_trace(events);

        let nplus = make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        let chatty = make_chatty_finding("trace-1");
        let tree = build_tree(&trace, &[nplus, chatty]);

        let has_inline = tree.roots.iter().any(|r| !r.findings.is_empty());
        assert!(has_inline, "N+1 finding should annotate the SQL spans");
        assert_eq!(
            tree.trace_level_findings.len(),
            1,
            "chatty finding should still land in trace_level_findings"
        );
    }

    #[test]
    fn format_tree_text_renders_trace_level_section() {
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT 1",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);
        let finding = make_chatty_finding("trace-1");
        let tree = build_tree(&trace, &[finding]);

        let text = format_tree_text(&tree, false);
        assert!(
            text.contains("Trace-level findings:"),
            "should render the trace-level header: {text}"
        );
        assert!(
            text.contains("chatty service"),
            "should mention the finding type: {text}"
        );
        assert!(text.contains("BFF"), "should render the suggestion: {text}");
    }

    #[test]
    fn cyclic_parent_reference_handled() {
        use crate::event::{EventSource, EventType, SpanEvent};
        use crate::normalize::NormalizedEvent;

        // Create two spans that reference each other as parents
        let span_a = NormalizedEvent {
            event: SpanEvent {
                timestamp: "2025-07-10T14:32:01.000Z".to_string(),
                trace_id: "trace-cycle".to_string(),
                span_id: "span-a".to_string(),
                parent_span_id: Some("span-b".to_string()),
                service: "svc".to_string(),
                cloud_region: None,
                event_type: EventType::Sql,
                operation: "SELECT".to_string(),
                target: "SELECT 1".to_string(),
                duration_us: 100,
                source: EventSource {
                    endpoint: "GET /test".to_string(),
                    method: "test".to_string(),
                },
                status_code: None,
                response_size_bytes: None,
                code_function: None,
                code_filepath: None,
                code_lineno: None,
                code_namespace: None,
                instrumentation_scopes: Vec::new(),
            },
            template: "SELECT ?".to_string(),
            params: vec!["1".to_string()],
        };
        let span_b = NormalizedEvent {
            event: SpanEvent {
                timestamp: "2025-07-10T14:32:01.001Z".to_string(),
                trace_id: "trace-cycle".to_string(),
                span_id: "span-b".to_string(),
                parent_span_id: Some("span-a".to_string()),
                service: "svc".to_string(),
                cloud_region: None,
                event_type: EventType::Sql,
                operation: "SELECT".to_string(),
                target: "SELECT 2".to_string(),
                duration_us: 100,
                source: EventSource {
                    endpoint: "GET /test".to_string(),
                    method: "test".to_string(),
                },
                status_code: None,
                response_size_bytes: None,
                code_function: None,
                code_filepath: None,
                code_lineno: None,
                code_namespace: None,
                instrumentation_scopes: Vec::new(),
            },
            template: "SELECT ?".to_string(),
            params: vec!["2".to_string()],
        };
        let trace = Trace {
            trace_id: "trace-cycle".to_string(),
            spans: vec![span_a, span_b],
        };

        // Should not panic despite cyclic parent references
        let tree = build_tree(&trace, &[]);
        let text = format_tree_text(&tree, false);
        assert!(!text.is_empty());
    }

    #[test]
    fn findings_from_other_trace_ignored() {
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT * FROM order_item WHERE order_id = 42",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);

        // Finding for a different trace
        let finding =
            make_finding_for("trace-other", "SELECT * FROM order_item WHERE order_id = ?");
        let tree = build_tree(&trace, &[finding]);

        assert!(tree.roots[0].findings.is_empty());
    }

    #[test]
    fn fix_and_location_render_inline_under_a_finding() {
        use crate::detect::suggestions::SuggestedFix;
        use crate::event::CodeLocation;

        let events = crate::test_helpers::make_sql_series_events(5);
        let trace = make_trace(events);
        let mut finding =
            make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        finding.suggested_fix = Some(SuggestedFix {
            pattern: "n_plus_one_sql".to_string(),
            framework: "java_jpa".to_string(),
            recommendation: "Use @BatchSize on the lazy collection".to_string(),
            reference_url: Some("https://docs.example.com/batch".to_string()),
        });
        finding.code_location = Some(CodeLocation {
            function: Some("findItems".to_string()),
            filepath: Some("src/main/java/orders/OrderService.java".to_string()),
            lineno: Some(118),
            namespace: Some("com.foo.orders.OrderService".to_string()),
        });

        let tree = build_tree(&trace, &[finding]);
        let text = format_tree_text(&tree, false);

        assert!(
            text.contains("suggestion: Use WHERE order_id IN (?)"),
            "missing suggestion line, got:\n{text}"
        );
        assert!(
            text.contains("fix [java_jpa]: Use @BatchSize on the lazy collection (https://docs.example.com/batch)"),
            "missing fix line, got:\n{text}"
        );
        assert!(
            text.contains("location:")
                && text.contains("src/main/java/orders/OrderService.java:118"),
            "missing location line, got:\n{text}"
        );
    }

    #[test]
    fn finding_without_fix_or_location_keeps_only_suggestion() {
        let events = crate::test_helpers::make_sql_series_events(5);
        let trace = make_trace(events);
        let finding = make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        let tree = build_tree(&trace, &[finding]);
        let text = format_tree_text(&tree, false);

        assert!(text.contains("suggestion:"), "got:\n{text}");
        assert!(!text.contains("fix ["), "fix line leaked, got:\n{text}");
        assert!(
            !text.contains("location:"),
            "location line leaked, got:\n{text}"
        );
    }

    #[test]
    fn fix_without_location_renders_without_location_line() {
        use crate::detect::suggestions::SuggestedFix;

        let events = crate::test_helpers::make_sql_series_events(5);
        let trace = make_trace(events);
        let mut finding =
            make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        finding.suggested_fix = Some(SuggestedFix {
            pattern: "n_plus_one_sql".to_string(),
            framework: "rust_diesel".to_string(),
            recommendation: "Use belonging_to + grouped_by".to_string(),
            reference_url: None,
        });

        let tree = build_tree(&trace, &[finding]);
        let text = format_tree_text(&tree, false);

        assert!(
            text.contains("fix [rust_diesel]: Use belonging_to + grouped_by"),
            "missing fix line, got:\n{text}"
        );
        assert!(
            !text.contains("location:"),
            "location must be omitted, got:\n{text}"
        );
    }

    #[test]
    fn ansi_escape_in_template_is_stripped_from_tree() {
        let mut events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT 1",
            "2025-07-10T14:32:01.000Z",
        )];
        // Override the normalized template with attacker-controlled bytes.
        events[0].target = "evil\x1b[2J\x1b[H wipe".to_string();
        let trace = make_trace(events);
        let tree = build_tree(&trace, &[]);
        let text = format_tree_text(&tree, false);
        assert!(
            !text.as_bytes().contains(&0x1b),
            "ESC byte from template leaked, got:\n{text}"
        );
    }

    #[test]
    fn ansi_escape_in_suggestion_is_stripped() {
        let events = crate::test_helpers::make_sql_series_events(5);
        let trace = make_trace(events);
        let mut finding =
            make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        finding.suggestion = "click \x1b]8;;https://attacker/\x07here\x1b]8;;\x07".to_string();
        let tree = build_tree(&trace, &[finding]);
        let text = format_tree_text(&tree, false);
        assert!(
            !text.as_bytes().contains(&0x1b),
            "ESC leaked from suggestion, got:\n{text}"
        );
        assert!(
            !text.as_bytes().contains(&0x07),
            "BEL leaked from suggestion, got:\n{text}"
        );
    }

    #[test]
    fn ansi_escape_in_code_location_is_stripped() {
        use crate::event::CodeLocation;

        let events = crate::test_helpers::make_sql_series_events(5);
        let trace = make_trace(events);
        let mut finding =
            make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        finding.code_location = Some(CodeLocation {
            function: Some("findItems\x1b[31m".to_string()),
            filepath: Some("src/Foo.java".to_string()),
            lineno: Some(10),
            namespace: Some("com.foo".to_string()),
        });
        let tree = build_tree(&trace, &[finding]);
        let text = format_tree_text(&tree, false);
        assert!(
            !text.as_bytes().contains(&0x1b),
            "ESC leaked from code_location, got:\n{text}"
        );
    }

    #[test]
    fn non_https_reference_url_is_omitted_in_tree() {
        use crate::detect::suggestions::SuggestedFix;

        let events = crate::test_helpers::make_sql_series_events(5);
        let trace = make_trace(events);
        let mut finding =
            make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        finding.suggested_fix = Some(SuggestedFix {
            pattern: "n_plus_one_sql".to_string(),
            framework: "java_jpa".to_string(),
            recommendation: "Use @BatchSize".to_string(),
            reference_url: Some("javascript:alert(1)".to_string()),
        });

        let tree = build_tree(&trace, &[finding]);
        let text = format_tree_text(&tree, false);
        assert!(
            !text.contains("javascript:"),
            "javascript: URL leaked, got:\n{text}"
        );
        assert!(
            text.contains("fix [java_jpa]: Use @BatchSize"),
            "recommendation must still render, got:\n{text}"
        );
    }

    #[test]
    fn trace_level_finding_renders_fix_inline() {
        use crate::detect::suggestions::SuggestedFix;

        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT 1",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);
        let mut finding = make_chatty_finding("trace-1");
        finding.suggested_fix = Some(SuggestedFix {
            pattern: "chatty_service".to_string(),
            framework: "java_generic".to_string(),
            recommendation: "Aggregate calls behind a BFF".to_string(),
            reference_url: None,
        });

        let tree = build_tree(&trace, &[finding]);
        let text = format_tree_text(&tree, false);

        assert!(
            text.contains("Trace-level findings:"),
            "missing header, got:\n{text}"
        );
        assert!(
            text.contains("fix [java_generic]: Aggregate calls behind a BFF"),
            "trace-level finding must render fix line, got:\n{text}"
        );
    }

    #[test]
    fn explain_tree_serde_roundtrip() {
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT * FROM order_item WHERE order_id = 1",
            "2025-07-10T14:32:01.050Z",
        )];
        let trace = make_trace(events);
        let finding = make_finding_for("trace-1", "SELECT * FROM order_item WHERE order_id = ?");
        let tree = build_tree(&trace, &[finding]);
        let json_str = format_tree_json(&tree).unwrap();
        let back: ExplainTree = serde_json::from_str(&json_str).unwrap();
        assert_eq!(back.trace_id, "trace-1");
        assert!(!back.roots.is_empty());
    }
}
