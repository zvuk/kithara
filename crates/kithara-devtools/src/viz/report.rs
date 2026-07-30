use std::collections::{BTreeMap, BTreeSet};

use super::{
    graph::{EdgeKind, NodeId, NodeKind},
    mermaid,
    metrics::ArchitectureMetrics,
    view::{DiagramModel, EvidenceStyle},
};

const DEGREE_FINDING_THRESHOLD: usize = 4;
const LONG_CHAIN_THRESHOLD: usize = 6;
const MAX_FINDINGS: usize = 12;
const MAX_RELATIONS: usize = 20;

pub(crate) fn render_metrics(metrics: &ArchitectureMetrics) -> String {
    let confirmed = &metrics.confirmed;
    let candidates = &metrics.including_candidates;
    let index = metrics
        .architecture_complexity_index
        .map_or_else(|| "n/a".to_string(), |value| format!("{value:.2} / 100"));
    let candidate_index = metrics
        .including_candidates_complexity_index
        .map_or_else(|| "n/a".to_string(), |value| format!("{value:.2} / 100"));
    let mut output = format!(
        "## Architectural complexity\n\n\
         Experimental ACI: **{index}** resolved-static, **{candidate_index}** including \
         syntax-derived candidates. Runtime observations cannot change either static score.\n\n\
         | Metric | Confirmed | Including candidates |\n\
         | --- | ---: | ---: |\n\
         | Contours | {} | {} |\n\
         | Internal relations | {} | {} |\n\
         | Incoming relations | {} | {} |\n\
         | Outgoing relations | {} | {} |\n\
         | Propagation cost | {:.3} | {:.3} |\n\
         | Normalized propagation | {:.3} | {:.3} |\n\
         | Cyclic contours | {} | {} |\n\
         | Largest cycle | {} | {} |\n\
         | Normalized depth | {:.3} | {:.3} |\n\
         | Instability | {} | {} |\n\
         | Cohesion | {} | {} |\n\
         | Boundary alignment | {} | {} |\n\
         | External coupling | {} | {} |\n\
         | Boundary load | {} | {} |\n\
         | Shared ownership | {} | {} |\n\n",
        confirmed.node_count,
        candidates.node_count,
        confirmed.internal_relations,
        candidates.internal_relations,
        confirmed.incoming_relations,
        candidates.incoming_relations,
        confirmed.outgoing_relations,
        candidates.outgoing_relations,
        confirmed.propagation_cost,
        candidates.propagation_cost,
        confirmed.normalized_propagation,
        candidates.normalized_propagation,
        confirmed.cyclic_nodes,
        candidates.cyclic_nodes,
        confirmed.largest_cycle,
        candidates.largest_cycle,
        confirmed.normalized_depth,
        candidates.normalized_depth,
        metric_value(confirmed.instability),
        metric_value(candidates.instability),
        metric_value(confirmed.cohesion),
        metric_value(candidates.cohesion),
        metric_value(confirmed.boundary_alignment),
        metric_value(candidates.boundary_alignment),
        metric_value(confirmed.external_coupling_ratio),
        metric_value(candidates.external_coupling_ratio),
        metric_value(confirmed.boundary_load),
        metric_value(candidates.boundary_load),
        metric_value(confirmed.shared_resource_ratio),
        metric_value(candidates.shared_resource_ratio),
    );
    output.push_str(&format!(
        "Type balance: abstractness {}, main-sequence distance {}.\n\n",
        metric_value(confirmed.abstractness),
        metric_value(confirmed.main_sequence_distance),
    ));
    output.push_str("ACI contributions:\n\n");
    for (name, value) in &metrics.contributions {
        output.push_str(&format!("- `{name}`: {value:.3}\n"));
    }
    if metrics.candidate_contributions != metrics.contributions {
        output.push_str("\nCandidate-inclusive contributions:\n\n");
        for (name, value) in &metrics.candidate_contributions {
            output.push_str(&format!("- `{name}`: {value:.3}\n"));
        }
    }
    output.push_str("\n### Metric findings\n\n");
    if let Some((name, value)) = metrics
        .contributions
        .iter()
        .max_by(|left, right| left.1.total_cmp(right.1))
    {
        output.push_str(&format!(
            "- Largest confirmed ACI contribution: `{name}` at {value:.3}.\n"
        ));
    }
    if confirmed.cyclic_nodes > 0 {
        output.push_str(&format!(
            "- {} contours participate in {} cyclic components; the largest contains {} contours.\n",
            confirmed.cyclic_nodes, confirmed.cyclic_scc_count, confirmed.largest_cycle
        ));
    }
    if confirmed.multi_owner_resources > 0 {
        output.push_str(&format!(
            "- {} resources have more than one visible owner.\n",
            confirmed.multi_owner_resources
        ));
    }
    let confirmed_relations =
        confirmed.internal_relations + confirmed.incoming_relations + confirmed.outgoing_relations;
    let candidate_relations = candidates.internal_relations
        + candidates.incoming_relations
        + candidates.outgoing_relations;
    let additional_relations = candidate_relations.saturating_sub(confirmed_relations);
    if additional_relations > 0 {
        output.push_str(&format!(
            "- Syntax-derived candidates add {additional_relations} cross-contour relations; \
             resolve them semantically before treating their topology as confirmed.\n"
        ));
    }
    if confirmed.cyclic_nodes == 0
        && confirmed.multi_owner_resources == 0
        && additional_relations == 0
    {
        output.push_str("- No cycle, multi-owner, or candidate-delta diagnostic is present.\n");
    }
    output.push_str(&format!(
        "\nRuntime overlay: {} observed and {} manual cross-contour relations.\n\n",
        metrics.runtime.observed_relations, metrics.runtime.manual_relations
    ));
    output
}

fn metric_value(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_string(), |value| format!("{value:.3}"))
}

pub(crate) fn render(model: &DiagramModel) -> String {
    let analysis = analyze(model);
    let mut output = String::from("## Diagram analysis\n\n");
    output.push_str(&format!(
        "This projection contains {} nodes and {} edges. Evidence styles: {} resolved, \
         {} conditional, {} runtime-observed, {} manual, {} candidate, {} unresolved, and {} \
         conflicting.\n",
        model.nodes.len(),
        model.edges.len(),
        analysis.styles[&EvidenceStyle::Resolved],
        analysis.styles[&EvidenceStyle::Conditional],
        analysis.styles[&EvidenceStyle::Observed],
        analysis.styles[&EvidenceStyle::Manual],
        analysis.styles[&EvidenceStyle::Candidate],
        analysis.styles[&EvidenceStyle::Unresolved],
        analysis.styles[&EvidenceStyle::Conflicting],
    ));
    let crates = model
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Package)
        .count();
    let modules = model
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Module)
        .count();
    let abstractions = model
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::ConcreteType | NodeKind::Trait | NodeKind::ModuleFunctions
            )
        })
        .count();
    let boundary_nodes = model
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::Constructor
                    | NodeKind::PublicFunction
                    | NodeKind::Task
                    | NodeKind::Resource
            )
        })
        .count();
    output.push_str(&format!(
        "Architecture contours: {crates} crates, {modules} modules, {abstractions} abstractions, \
         and {boundary_nodes} explicit boundary/resource nodes.\n"
    ));

    if analysis.entries.is_empty() {
        output.push_str("No connected entry flow is visible in this projection.\n\n");
    } else {
        output.push_str("Visible entry flow starts at ");
        for (index, node) in analysis.entries.iter().enumerate() {
            if index > 0 {
                output.push_str(", ");
            }
            output.push_str(&node_reference(model, node));
        }
        output.push_str(".\n\n");
    }

    output.push_str("### Visible relations\n\n");
    if model.edges.is_empty() {
        output.push_str("- No relation crosses the visible contours.\n\n");
    } else {
        for edge in model.edges.iter().take(MAX_RELATIONS) {
            let count = if edge.count > 1 {
                format!(" x{}", edge.count)
            } else {
                String::new()
            };
            let details = edge
                .details
                .iter()
                .take(3)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("; ");
            output.push_str(&format!(
                "- {} --{}{} [{}]--> {}",
                node_reference(model, &edge.source),
                relation_name(edge.kind),
                count,
                style_name(edge.style),
                node_reference(model, &edge.target),
            ));
            if !details.is_empty() {
                output.push_str(&format!(": {details}"));
            }
            output.push('\n');
        }
        if model.edges.len() > MAX_RELATIONS {
            output.push_str(&format!(
                "- {} more visible relations are retained in `projection.json`.\n",
                model.edges.len() - MAX_RELATIONS
            ));
        }
        output.push('\n');
    }

    output.push_str("### Findings\n\n");
    if analysis.findings.is_empty() {
        output.push_str("- No branching, convergence, cycle, ownership, or long-chain finding crossed the configured visible thresholds.\n");
    } else {
        for finding in analysis.findings.iter().take(MAX_FINDINGS) {
            output.push_str(&format!(
                "- **{}** at {}: {}",
                finding.kind,
                node_reference(model, &finding.node),
                finding.message
            ));
            output.push('\n');
        }
    }
    if model.hidden_nodes > 0 {
        output.push_str(&format!(
            "\n### Navigation\n\n{} nodes are outside this selected projection. Narrow the same graph with \
             `--crate`, `--module`, or `--scenario`; no finding above describes a hidden node.",
            model.hidden_nodes
        ));
        output.push('\n');
    }
    output
}

fn relation_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "contains",
        EdgeKind::DependsOn => "depends on",
        EdgeKind::Implements => "implements",
        EdgeKind::Starts => "starts",
        EdgeKind::Calls => "calls",
        EdgeKind::Spawns => "spawns",
        EdgeKind::Constructs => "constructs",
        EdgeKind::Clones => "clones",
        EdgeKind::Stores => "stores",
        EdgeKind::Transfers => "transfers",
        EdgeKind::Sends => "sends",
        EdgeKind::Drops => "drops",
    }
}

fn style_name(style: EvidenceStyle) -> &'static str {
    match style {
        EvidenceStyle::Resolved => "resolved",
        EvidenceStyle::Conditional => "conditional",
        EvidenceStyle::Observed => "observed",
        EvidenceStyle::Manual => "manual",
        EvidenceStyle::Candidate => "candidate",
        EvidenceStyle::Unresolved => "unresolved",
        EvidenceStyle::Conflicting => "conflicting",
    }
}

struct Analysis {
    styles: BTreeMap<EvidenceStyle, usize>,
    entries: Vec<NodeId>,
    findings: Vec<Finding>,
}

struct Finding {
    kind: &'static str,
    node: NodeId,
    message: String,
}

fn analyze(model: &DiagramModel) -> Analysis {
    let mut styles = [
        EvidenceStyle::Resolved,
        EvidenceStyle::Conditional,
        EvidenceStyle::Observed,
        EvidenceStyle::Manual,
        EvidenceStyle::Candidate,
        EvidenceStyle::Unresolved,
        EvidenceStyle::Conflicting,
    ]
    .into_iter()
    .map(|style| (style, 0))
    .collect::<BTreeMap<_, _>>();
    for node in &model.nodes {
        *styles.entry(node.style).or_default() += 1;
    }
    for edge in &model.edges {
        *styles.entry(edge.style).or_default() += 1;
    }

    let mut outgoing = BTreeMap::<NodeId, Vec<NodeId>>::new();
    let mut incoming = BTreeMap::<NodeId, Vec<NodeId>>::new();
    for node in &model.nodes {
        outgoing.entry(node.id.clone()).or_default();
        incoming.entry(node.id.clone()).or_default();
    }
    for edge in &model.edges {
        outgoing
            .entry(edge.source.clone())
            .or_default()
            .push(edge.target.clone());
        incoming
            .entry(edge.target.clone())
            .or_default()
            .push(edge.source.clone());
    }
    for targets in outgoing.values_mut() {
        targets.sort();
        targets.dedup();
    }
    for sources in incoming.values_mut() {
        sources.sort();
        sources.dedup();
    }

    let entries = model
        .nodes
        .iter()
        .filter(|node| {
            incoming.get(&node.id).is_none_or(Vec::is_empty)
                && outgoing
                    .get(&node.id)
                    .is_some_and(|targets| !targets.is_empty())
        })
        .map(|node| node.id.clone())
        .take(5)
        .collect();
    let mut findings = Vec::new();
    for node in &model.nodes {
        let fan_out = outgoing.get(&node.id).map_or(0, Vec::len);
        if fan_out >= DEGREE_FINDING_THRESHOLD {
            findings.push(Finding {
                kind: "High fan-out",
                node: node.id.clone(),
                message: format!(
                    "{fan_out} visible downstream branches increase orchestration breadth"
                ),
            });
        }
        let fan_in = incoming.get(&node.id).map_or(0, Vec::len);
        if fan_in >= DEGREE_FINDING_THRESHOLD {
            findings.push(Finding {
                kind: "High fan-in",
                node: node.id.clone(),
                message: format!("{fan_in} visible upstream paths converge on one abstraction"),
            });
        }
        if node.kind == NodeKind::Resource
            && !model.edges.iter().any(|edge| {
                edge.target == node.id
                    && matches!(
                        edge.kind,
                        EdgeKind::Constructs
                            | EdgeKind::Clones
                            | EdgeKind::Stores
                            | EdgeKind::Transfers
                            | EdgeKind::Sends
                    )
            })
        {
            findings.push(Finding {
                kind: "Unclear owner",
                node: node.id.clone(),
                message: "the visible diagram has no constructor, transfer, store, send, or clone edge for this resource"
                    .to_string(),
            });
        }
    }
    for group in &model.groups {
        findings.push(Finding {
            kind: if group.members.len() >= 5 {
                "Large cycle"
            } else {
                "Cycle"
            },
            node: group.node.clone(),
            message: format!(
                "{} mutually reachable call nodes are collapsed here",
                group.members.len()
            ),
        });
    }
    if let Some(chain) = longest_chain(&outgoing)
        && chain.len() >= LONG_CHAIN_THRESHOLD
    {
        findings.push(Finding {
            kind: "Long chain",
            node: chain[0].clone(),
            message: format!(
                "{} visible nodes form the longest acyclic path in this projection",
                chain.len()
            ),
        });
    }
    findings.sort_by(|left, right| {
        left.node
            .cmp(&right.node)
            .then_with(|| left.kind.cmp(right.kind))
    });
    Analysis {
        styles,
        entries,
        findings,
    }
}

fn longest_chain(adjacency: &BTreeMap<NodeId, Vec<NodeId>>) -> Option<Vec<NodeId>> {
    let mut longest = Vec::new();
    for start in adjacency.keys() {
        let mut visiting = BTreeSet::new();
        let mut path = Vec::new();
        longest_from(start, adjacency, &mut visiting, &mut path, &mut longest);
    }
    (!longest.is_empty()).then_some(longest)
}

fn longest_from(
    node: &NodeId,
    adjacency: &BTreeMap<NodeId, Vec<NodeId>>,
    visiting: &mut BTreeSet<NodeId>,
    path: &mut Vec<NodeId>,
    longest: &mut Vec<NodeId>,
) {
    if !visiting.insert(node.clone()) {
        return;
    }
    path.push(node.clone());
    if path.len() > longest.len() {
        longest.clone_from(path);
    }
    if path.len() < LONG_CHAIN_THRESHOLD * 3
        && let Some(targets) = adjacency.get(node)
    {
        for target in targets {
            longest_from(target, adjacency, visiting, path, longest);
        }
    }
    path.pop();
    visiting.remove(node);
}

fn node_reference(model: &DiagramModel, id: &NodeId) -> String {
    let label = model
        .nodes
        .iter()
        .find(|node| node.id == *id)
        .map_or("unknown", |node| node.label.as_str())
        .replace('`', "'");
    format!("`{}` (`{label}`)", mermaid::node_id(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz::{
        graph::NodeKind,
        view::{DiagramEdge, DiagramNode, ViewKind},
    };

    fn node(symbol: &str) -> DiagramNode {
        DiagramNode {
            id: NodeId::symbol("demo", "demo", "crate", symbol),
            kind: NodeKind::Function,
            label: symbol.to_string(),
            location: None,
            style: EvidenceStyle::Resolved,
            parent: None,
        }
    }

    #[test]
    fn every_finding_references_a_visible_node() {
        let nodes = ["root", "a", "b", "c", "d"]
            .into_iter()
            .map(node)
            .collect::<Vec<_>>();
        let edges = nodes[1..]
            .iter()
            .map(|target| DiagramEdge {
                source: nodes[0].id.clone(),
                target: target.id.clone(),
                kind: EdgeKind::Calls,
                style: EvidenceStyle::Resolved,
                count: 1,
                details: vec!["root -> target".to_string()],
                origins: vec!["test".to_string()],
            })
            .collect();
        let model = DiagramModel {
            kind: ViewKind::Overview,
            lod: crate::viz::view::DetailLevel::Full,
            nodes,
            edges,
            groups: Vec::new(),
            hidden_nodes: 0,
        };

        let report = render(&model);

        assert!(report.contains("High fan-out"));
        assert!(report.contains(&mermaid::node_id(&model.nodes[0].id)));
    }

    #[test]
    fn hidden_nodes_are_only_described_as_navigation_scope() {
        let model = DiagramModel {
            kind: ViewKind::Overview,
            lod: crate::viz::view::DetailLevel::Full,
            nodes: vec![node("visible")],
            edges: Vec::new(),
            groups: Vec::new(),
            hidden_nodes: 7,
        };

        let report = render(&model);

        assert!(report.contains("7 nodes are outside this selected projection"));
        assert!(report.contains("no finding above describes a hidden node"));
    }
}
