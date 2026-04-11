//! Explain mode: builds a tree view of a trace with findings annotated inline.

use crate::correlate::Trace;
use crate::detect::Finding;
use crate::normalize::NormalizedEvent;
use serde::Serialize;
use std::collections::HashMap;

/// A node in the explain tree, representing a single span.
#[derive(Debug, Clone, Serialize)]
pub struct SpanNode {
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub service: String,
    pub template: String,
    pub operation: String,
    pub duration_us: u64,
    pub timestamp: String,
    pub children: Vec<SpanNode>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<InlineFinding>,
}

/// A finding annotated on a span node.
#[derive(Debug, Clone, Serialize)]
pub struct InlineFinding {
    #[serde(rename = "type")]
    pub finding_type: String,
    pub severity: String,
    pub occurrences: usize,
    pub suggestion: String,
}

/// The complete explain tree for a trace.
#[derive(Debug, Clone, Serialize)]
pub struct ExplainTree {
    pub trace_id: String,
    pub roots: Vec<SpanNode>,
}

/// Build an explain tree from a trace and its findings.
#[must_use]
pub fn build_tree(trace: &Trace, findings: &[Finding]) -> ExplainTree {
    // Index findings by template for quick lookup
    let mut findings_by_template: HashMap<&str, Vec<&Finding>> = HashMap::new();
    for f in findings {
        if f.trace_id == trace.trace_id {
            findings_by_template
                .entry(f.pattern.template.as_str())
                .or_default()
                .push(f);
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
        roots: root_nodes,
    }
}

fn make_node(
    span: &NormalizedEvent,
    findings_by_template: &HashMap<&str, Vec<&Finding>>,
) -> SpanNode {
    let inline_findings = findings_by_template
        .get(span.template.as_str())
        .map(|fs| {
            fs.iter()
                .map(|f| InlineFinding {
                    finding_type: f.finding_type.as_str().to_string(),
                    severity: f.severity.as_str().to_string(),
                    occurrences: f.pattern.occurrences,
                    suggestion: f.suggestion.clone(),
                })
                .collect()
        })
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

    for (i, root) in tree.roots.iter().enumerate() {
        let is_last = i == tree.roots.len() - 1;
        format_node(&mut out, root, "", is_last, &colors, 0);
    }

    out
}

fn format_node(
    out: &mut String,
    node: &SpanNode,
    prefix: &str,
    is_last: bool,
    c: &TreeColors,
    depth: usize,
) {
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
        c.bold, node.template, c.reset, c.dim, c.reset,
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
            f.finding_type.replace('_', " "),
            f.severity,
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

    for f in &node.findings {
        let _ = writeln!(
            out,
            "{child_prefix}  {}\u{2514}\u{2500} suggestion: {}{}",
            c.dim, f.suggestion, c.reset,
        );
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
}
