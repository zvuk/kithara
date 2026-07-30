use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{
    contour::ContourIndex,
    filter::ArchitectureFilter,
    graph::{
        Edge, EdgeKind, EvidenceGraph, MergedCertainty, Node, NodeId, NodeKind, SourceLocation,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ViewKind {
    Overview,
    Hierarchy,
    Ownership,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DetailLevel {
    Crates,
    Modules,
    Abstractions,
    Methods,
    Full,
}

impl DetailLevel {
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Crates => 0,
            Self::Modules => 1,
            Self::Abstractions => 2,
            Self::Methods => 3,
            Self::Full => 4,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ViewRequest {
    pub(crate) kind: ViewKind,
    pub(crate) package: Option<String>,
    pub(crate) module: Option<String>,
    pub(crate) scenario: Option<String>,
    pub(crate) lod: DetailLevel,
    pub(crate) filter: ArchitectureFilter,
}

pub(crate) struct Projector<'a> {
    graph: &'a EvidenceGraph,
    contours: ContourIndex,
}

impl<'a> Projector<'a> {
    pub(crate) fn new(graph: &'a EvidenceGraph) -> Self {
        Self {
            graph,
            contours: ContourIndex::new(graph),
        }
    }

    pub(crate) fn project(&self, request: &ViewRequest) -> DiagramModel {
        project_with_contours(self.graph, request, &self.contours)
    }

    pub(crate) fn contours(&self) -> &ContourIndex {
        &self.contours
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceStyle {
    Resolved,
    Conditional,
    Observed,
    Manual,
    Candidate,
    Unresolved,
    Conflicting,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DiagramNode {
    pub(crate) id: NodeId,
    pub(crate) kind: NodeKind,
    pub(crate) label: String,
    pub(crate) location: Option<SourceLocation>,
    pub(crate) style: EvidenceStyle,
    pub(crate) parent: Option<NodeId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DiagramEdge {
    pub(crate) source: NodeId,
    pub(crate) target: NodeId,
    pub(crate) kind: EdgeKind,
    pub(crate) style: EvidenceStyle,
    pub(crate) count: usize,
    pub(crate) details: Vec<String>,
    pub(crate) origins: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DiagramGroup {
    pub(crate) node: NodeId,
    pub(crate) members: Vec<NodeId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DiagramModel {
    pub(crate) kind: ViewKind,
    pub(crate) lod: DetailLevel,
    pub(crate) nodes: Vec<DiagramNode>,
    pub(crate) edges: Vec<DiagramEdge>,
    pub(crate) groups: Vec<DiagramGroup>,
    pub(crate) hidden_nodes: usize,
}

#[cfg(test)]
pub(crate) fn project(graph: &EvidenceGraph, request: &ViewRequest) -> DiagramModel {
    Projector::new(graph).project(request)
}

fn project_with_contours(
    graph: &EvidenceGraph,
    request: &ViewRequest,
    contours: &ContourIndex,
) -> DiagramModel {
    if request.lod == DetailLevel::Crates {
        if request.package.is_some() {
            return project_focused_crate(graph, request, contours);
        }
        return project_crates(graph, request);
    }

    let parents = contours.parents();
    let boundary = if request.lod == DetailLevel::Methods {
        boundary_functions(graph, parents)
    } else {
        BTreeSet::new()
    };
    let eligible = graph
        .nodes()
        .filter(|node| {
            node_visible_at_lod(node, request.lod, &boundary)
                && (node_in_view(node, request.kind)
                    || matches!(node.kind, NodeKind::Package | NodeKind::Module))
        })
        .filter(|node| scenario_matches(node, request))
        .filter(|node| !request.filter.excludes(&node.id))
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let mut candidates = eligible
        .iter()
        .filter(|id| {
            graph
                .node(id)
                .is_some_and(|node| module_matches(node, request))
        })
        .filter(|id| {
            request
                .package
                .as_ref()
                .is_none_or(|package| id.package == *package)
        })
        .cloned()
        .collect::<BTreeSet<_>>();

    if request.module.is_some() {
        include_connected_resources(graph, request.kind, &mut candidates);
    }
    include_visible_ancestors(parents, &eligible, &mut candidates);
    let external_ports = outgoing_public_ports(graph, request, &candidates, parents);
    candidates.extend(external_ports.iter().cloned());

    let visible = candidates;
    let nodes = graph
        .nodes()
        .filter(|node| visible.contains(&node.id))
        .map(|node| {
            if external_ports.contains(&node.id) {
                external_port_node(node)
            } else {
                diagram_node(node, visible_parent(&node.id, &visible, parents))
            }
        })
        .collect();
    let edges = lifted_edges(graph, request, &visible, parents);

    let mut model = DiagramModel {
        kind: request.kind,
        lod: request.lod,
        nodes,
        edges,
        groups: Vec::new(),
        hidden_nodes: 0,
    };
    if request.lod == DetailLevel::Full {
        super::cycle::collapse(&mut model);
    }
    model
}

fn include_visible_ancestors(
    parents: &BTreeMap<NodeId, NodeId>,
    eligible: &BTreeSet<NodeId>,
    candidates: &mut BTreeSet<NodeId>,
) {
    let selected = candidates.clone();
    for id in selected {
        let mut current = &id;
        let mut seen = BTreeSet::new();
        while let Some(parent) = parents.get(current) {
            if !seen.insert(parent.clone()) {
                break;
            }
            if eligible.contains(parent) {
                candidates.insert(parent.clone());
            }
            current = parent;
        }
    }
}

fn lifted_edges(
    graph: &EvidenceGraph,
    request: &ViewRequest,
    visible: &BTreeSet<NodeId>,
    parents: &BTreeMap<NodeId, NodeId>,
) -> Vec<DiagramEdge> {
    let mut lifted = BTreeMap::new();

    for edge in graph
        .edges()
        .filter(|edge| edge_in_view(edge, request.kind))
        .filter(|edge| package_edge_matches(edge, request))
        .filter(|edge| focused_edge_matches(edge, request))
        .filter(|edge| module_edge_matches(edge, request))
        .filter(|edge| {
            !request.filter.excludes(&edge.source) && !request.filter.excludes(&edge.target)
        })
    {
        let Some(source) = nearest_visible(&edge.source, visible, parents) else {
            continue;
        };
        let Some(target) = nearest_visible(&edge.target, visible, parents) else {
            continue;
        };
        if source == target {
            continue;
        }
        let style = evidence_style(
            edge.certainty(),
            edge.evidence.iter().map(|evidence| evidence.class),
        );
        let count = edge
            .evidence
            .iter()
            .map(|evidence| evidence.origin.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            .max(1);
        let detail = edge_detail(graph, edge);
        let origins = edge
            .evidence
            .iter()
            .map(|evidence| evidence.origin.clone())
            .collect::<Vec<_>>();
        lifted
            .entry((source.clone(), target.clone(), edge.kind))
            .and_modify(|current: &mut DiagramEdge| {
                current.style = merge_style(current.style, style);
                current.count += count;
                if !current.details.contains(&detail) {
                    current.details.push(detail.clone());
                    current.details.sort();
                }
                current.origins.extend(origins.iter().cloned());
                current.origins.sort();
                current.origins.dedup();
            })
            .or_insert_with(|| DiagramEdge {
                source,
                target,
                kind: edge.kind,
                style,
                count,
                details: vec![detail],
                origins,
            });
    }
    lifted.into_values().collect()
}

fn boundary_functions(
    graph: &EvidenceGraph,
    parents: &BTreeMap<NodeId, NodeId>,
) -> BTreeSet<NodeId> {
    let mut boundary = BTreeSet::new();
    for edge in graph.edges().filter(|edge| {
        !matches!(
            edge.kind,
            EdgeKind::Contains | EdgeKind::DependsOn | EdgeKind::Implements
        )
    }) {
        let source_owner = abstraction_ancestor(graph, &edge.source, parents);
        let target_owner = abstraction_ancestor(graph, &edge.target, parents);
        if source_owner == target_owner && source_owner.is_some() {
            continue;
        }
        for id in [&edge.source, &edge.target] {
            if let Some(function) = function_ancestor(graph, id, parents) {
                boundary.insert(function);
            }
        }
    }
    boundary
}

fn function_ancestor(
    graph: &EvidenceGraph,
    id: &NodeId,
    parents: &BTreeMap<NodeId, NodeId>,
) -> Option<NodeId> {
    let mut current = id;
    let mut seen = BTreeSet::new();
    loop {
        if graph.node(current).is_some_and(|node| {
            matches!(
                node.kind,
                NodeKind::Function | NodeKind::PublicFunction | NodeKind::Constructor
            )
        }) {
            return Some(current.clone());
        }
        if !seen.insert(current.clone()) {
            return None;
        }
        current = parents.get(current)?;
    }
}

fn abstraction_ancestor(
    graph: &EvidenceGraph,
    id: &NodeId,
    parents: &BTreeMap<NodeId, NodeId>,
) -> Option<NodeId> {
    let mut current = id;
    let mut seen = BTreeSet::new();
    loop {
        if graph.node(current).is_some_and(|node| {
            matches!(
                node.kind,
                NodeKind::ConcreteType | NodeKind::Trait | NodeKind::ModuleFunctions
            )
        }) {
            return Some(current.clone());
        }
        if !seen.insert(current.clone()) {
            return None;
        }
        current = parents.get(current)?;
    }
}

fn edge_detail(graph: &EvidenceGraph, edge: &Edge) -> String {
    let source = graph
        .node(&edge.source)
        .map_or_else(|| edge.source.symbol.as_str(), |node| node.label.as_str());
    let target = graph
        .node(&edge.target)
        .map_or_else(|| edge.target.symbol.as_str(), |node| node.label.as_str());
    format!("{source} -> {target}")
}

fn nearest_visible(
    id: &NodeId,
    visible: &BTreeSet<NodeId>,
    parents: &BTreeMap<NodeId, NodeId>,
) -> Option<NodeId> {
    let mut current = id;
    let mut seen = BTreeSet::new();
    loop {
        if visible.contains(current) {
            return Some(current.clone());
        }
        if !seen.insert(current.clone()) {
            return None;
        }
        current = parents.get(current)?;
    }
}

fn visible_parent(
    id: &NodeId,
    visible: &BTreeSet<NodeId>,
    parents: &BTreeMap<NodeId, NodeId>,
) -> Option<NodeId> {
    let parent = parents.get(id)?;
    nearest_visible(parent, visible, parents)
}

fn module_edge_matches(edge: &Edge, request: &ViewRequest) -> bool {
    let Some(module) = request.module.as_deref() else {
        return true;
    };
    edge.kind == EdgeKind::DependsOn
        || id_module_matches(&edge.source, module)
        || id_module_matches(&edge.target, module)
}

fn id_module_matches(id: &NodeId, module: &str) -> bool {
    id.module == module
        || id
            .module
            .strip_prefix(module)
            .is_some_and(|suffix| suffix.starts_with("::"))
}

fn project_crates(graph: &EvidenceGraph, request: &ViewRequest) -> DiagramModel {
    let visible = graph
        .nodes()
        .filter(|node| node.kind == NodeKind::Package)
        .filter(|node| !request.filter.excludes(&node.id))
        .filter(|node| {
            request
                .package
                .as_ref()
                .is_none_or(|package| node.id.package == *package)
        })
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();

    DiagramModel {
        kind: request.kind,
        lod: request.lod,
        nodes: graph
            .nodes()
            .filter(|node| visible.contains(&node.id))
            .map(|node| diagram_node(node, None))
            .collect(),
        edges: graph
            .edges()
            .filter(|edge| edge.kind == EdgeKind::DependsOn)
            .filter(|edge| package_edge_matches(edge, request))
            .filter(|edge| {
                !request.filter.excludes(&edge.source) && !request.filter.excludes(&edge.target)
            })
            .filter(|edge| visible.contains(&edge.source) && visible.contains(&edge.target))
            .map(diagram_edge)
            .collect(),
        groups: Vec::new(),
        hidden_nodes: 0,
    }
}

fn project_focused_crate(
    graph: &EvidenceGraph,
    request: &ViewRequest,
    contours: &ContourIndex,
) -> DiagramModel {
    let parents = contours.parents();
    let mut visible = graph
        .nodes()
        .filter(|node| node.kind == NodeKind::Package)
        .filter(|node| !request.filter.excludes(&node.id))
        .filter(|node| {
            request
                .package
                .as_ref()
                .is_some_and(|package| node.id.package == *package)
        })
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let external_ports = outgoing_public_ports(graph, request, &visible, parents);
    visible.extend(external_ports.iter().cloned());

    DiagramModel {
        kind: request.kind,
        lod: request.lod,
        nodes: graph
            .nodes()
            .filter(|node| visible.contains(&node.id))
            .map(|node| {
                if external_ports.contains(&node.id) {
                    external_port_node(node)
                } else {
                    diagram_node(node, None)
                }
            })
            .collect(),
        edges: lifted_edges(graph, request, &visible, parents),
        groups: Vec::new(),
        hidden_nodes: 0,
    }
}

fn package_edge_matches(edge: &Edge, request: &ViewRequest) -> bool {
    request
        .package
        .as_ref()
        .is_none_or(|package| edge.source.package == *package || edge.target.package == *package)
}

fn focused_edge_matches(edge: &Edge, request: &ViewRequest) -> bool {
    request
        .package
        .as_ref()
        .is_none_or(|package| edge.source.package == *package)
}

fn outgoing_public_ports(
    graph: &EvidenceGraph,
    request: &ViewRequest,
    internal: &BTreeSet<NodeId>,
    parents: &BTreeMap<NodeId, NodeId>,
) -> BTreeSet<NodeId> {
    let Some(package) = request.package.as_deref() else {
        return BTreeSet::new();
    };
    graph
        .edges()
        .filter(|edge| edge_in_view(edge, request.kind))
        .filter(|edge| edge.kind != EdgeKind::DependsOn)
        .filter(|edge| edge.source.package == package && edge.target.package != package)
        .filter(|edge| {
            request
                .module
                .as_deref()
                .is_none_or(|module| id_module_matches(&edge.source, module))
        })
        .filter(|edge| {
            !request.filter.excludes(&edge.source) && !request.filter.excludes(&edge.target)
        })
        .filter(|edge| nearest_visible(&edge.source, internal, parents).is_some())
        .filter_map(|edge| {
            graph
                .node(&edge.target)
                .filter(|node| public_external_target(node, edge))
                .map(|node| node.id.clone())
        })
        .collect()
}

fn public_external_target(node: &Node, edge: &Edge) -> bool {
    matches!(
        node.kind,
        NodeKind::PublicItem | NodeKind::PublicFunction | NodeKind::TraitMethod
    ) || node.kind == NodeKind::Constructor && edge.certainty() == MergedCertainty::Resolved
}

fn external_port_node(node: &Node) -> DiagramNode {
    let mut projected = diagram_node(node, None);
    projected.label = format!("external: {}::{}", node.id.package, node.label);
    projected
}

fn node_in_view(node: &Node, kind: ViewKind) -> bool {
    if matches!(
        node.kind,
        NodeKind::ConcreteType | NodeKind::Trait | NodeKind::ModuleFunctions
    ) {
        return true;
    }
    match kind {
        ViewKind::Overview => matches!(
            node.kind,
            NodeKind::Function
                | NodeKind::PublicFunction
                | NodeKind::Constructor
                | NodeKind::TraitMethod
                | NodeKind::OwnershipSite
                | NodeKind::Resource
                | NodeKind::Task
                | NodeKind::Scenario
                | NodeKind::RuntimeEvent
                | NodeKind::CycleGroup
        ),
        ViewKind::Hierarchy => matches!(
            node.kind,
            NodeKind::Package
                | NodeKind::Module
                | NodeKind::PublicItem
                | NodeKind::PublicFunction
                | NodeKind::Constructor
                | NodeKind::TraitMethod
        ),
        ViewKind::Ownership => matches!(node.kind, NodeKind::OwnershipSite | NodeKind::Resource),
    }
}

fn node_visible_at_lod(node: &Node, lod: DetailLevel, boundary: &BTreeSet<NodeId>) -> bool {
    match lod {
        DetailLevel::Crates => node.kind == NodeKind::Package,
        DetailLevel::Modules => node.kind == NodeKind::Package || ContourIndex::is_subsystem(node),
        DetailLevel::Abstractions => matches!(
            node.kind,
            NodeKind::Package
                | NodeKind::Module
                | NodeKind::ConcreteType
                | NodeKind::Trait
                | NodeKind::ModuleFunctions
        ),
        DetailLevel::Methods => {
            matches!(
                node.kind,
                NodeKind::Package
                    | NodeKind::Module
                    | NodeKind::ConcreteType
                    | NodeKind::Trait
                    | NodeKind::ModuleFunctions
                    | NodeKind::Constructor
                    | NodeKind::TraitMethod
                    | NodeKind::PublicFunction
                    | NodeKind::Resource
                    | NodeKind::Task
            ) || boundary.contains(&node.id)
        }
        DetailLevel::Full => true,
    }
}

fn edge_in_view(edge: &Edge, kind: ViewKind) -> bool {
    match kind {
        ViewKind::Overview => edge.kind != EdgeKind::Contains,
        ViewKind::Hierarchy => matches!(edge.kind, EdgeKind::Contains | EdgeKind::DependsOn),
        ViewKind::Ownership => matches!(
            edge.kind,
            EdgeKind::Constructs
                | EdgeKind::Clones
                | EdgeKind::Stores
                | EdgeKind::Transfers
                | EdgeKind::Sends
                | EdgeKind::Drops
        ),
    }
}

fn module_matches(node: &Node, request: &ViewRequest) -> bool {
    let Some(module) = request.module.as_deref() else {
        return true;
    };
    node.kind == NodeKind::Package
        || node.id.module == module
        || node
            .id
            .module
            .strip_prefix(module)
            .is_some_and(|suffix| suffix.starts_with("::"))
}

fn scenario_matches(node: &Node, request: &ViewRequest) -> bool {
    let Some(scenario) = request.scenario.as_deref() else {
        return true;
    };
    let prefix = format!("trace:{scenario}");
    node.evidence
        .iter()
        .any(|evidence| evidence.origin.starts_with(&prefix))
}

fn include_connected_resources(
    graph: &EvidenceGraph,
    kind: ViewKind,
    candidates: &mut BTreeSet<NodeId>,
) {
    if kind == ViewKind::Hierarchy {
        return;
    }
    for edge in graph.edges().filter(|edge| edge_in_view(edge, kind)) {
        if candidates.contains(&edge.source)
            && graph
                .node(&edge.target)
                .is_some_and(|node| node.kind == NodeKind::Resource)
        {
            candidates.insert(edge.target.clone());
        }
        if candidates.contains(&edge.target)
            && graph
                .node(&edge.source)
                .is_some_and(|node| node.kind == NodeKind::Resource)
        {
            candidates.insert(edge.source.clone());
        }
    }
}

fn diagram_node(node: &Node, parent: Option<NodeId>) -> DiagramNode {
    DiagramNode {
        id: node.id.clone(),
        kind: node.kind,
        label: node.label.clone(),
        location: node.location.clone(),
        style: evidence_style(
            node.certainty(),
            node.evidence.iter().map(|evidence| evidence.class),
        ),
        parent,
    }
}

fn diagram_edge(edge: &Edge) -> DiagramEdge {
    DiagramEdge {
        source: edge.source.clone(),
        target: edge.target.clone(),
        kind: edge.kind,
        style: evidence_style(
            edge.certainty(),
            edge.evidence.iter().map(|evidence| evidence.class),
        ),
        count: edge
            .evidence
            .iter()
            .map(|evidence| evidence.origin.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            .max(1),
        details: vec![format!("{} -> {}", edge.source.symbol, edge.target.symbol)],
        origins: edge
            .evidence
            .iter()
            .map(|evidence| evidence.origin.clone())
            .collect(),
    }
}

fn evidence_style(
    certainty: MergedCertainty,
    classes: impl Iterator<Item = super::graph::EvidenceClass>,
) -> EvidenceStyle {
    let classes = classes.collect::<BTreeSet<_>>();
    if classes.contains(&super::graph::EvidenceClass::Manual) {
        return EvidenceStyle::Manual;
    }
    if classes.contains(&super::graph::EvidenceClass::Observed) {
        return EvidenceStyle::Observed;
    }
    if classes.contains(&super::graph::EvidenceClass::Conditional) {
        return EvidenceStyle::Conditional;
    }
    match certainty {
        MergedCertainty::Resolved => EvidenceStyle::Resolved,
        MergedCertainty::Candidate => EvidenceStyle::Candidate,
        MergedCertainty::Unresolved => EvidenceStyle::Unresolved,
        MergedCertainty::Conflicting => EvidenceStyle::Conflicting,
    }
}

fn merge_style(left: EvidenceStyle, right: EvidenceStyle) -> EvidenceStyle {
    use EvidenceStyle::{
        Candidate, Conditional, Conflicting, Manual, Observed, Resolved, Unresolved,
    };

    match (left, right) {
        (Conflicting, _) | (_, Conflicting) => Conflicting,
        (Unresolved, _) | (_, Unresolved) => Unresolved,
        (Manual, _) | (_, Manual) => Manual,
        (Observed, _) | (_, Observed) => Observed,
        (Candidate, _) | (_, Candidate) => Candidate,
        (Conditional, _) | (_, Conditional) => Conditional,
        (Resolved, Resolved) => Resolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz::graph::{
        Certainty, Edge, EdgeKind, Evidence, EvidenceClass, EvidenceGraph, Node, NodeId, NodeKind,
    };

    fn evidence(origin: &str) -> Evidence {
        Evidence {
            class: EvidenceClass::Static,
            certainty: Certainty::Resolved,
            origin: origin.to_string(),
        }
    }

    #[test]
    fn projection_is_unbounded_and_deterministic() {
        let mut graph = EvidenceGraph::default();
        for label in ["zeta", "alpha", "middle"] {
            graph.merge_node(Node::new(
                NodeId::symbol("demo", "lib", "runtime", label),
                NodeKind::Function,
                label,
                evidence(label),
            ));
        }

        let request = ViewRequest {
            kind: ViewKind::Overview,
            package: None,
            module: None,
            scenario: None,
            lod: DetailLevel::Full,
            filter: ArchitectureFilter::default(),
        };
        let model = project(&graph, &request);

        assert_eq!(model.hidden_nodes, 0);
        assert_eq!(
            model
                .nodes
                .iter()
                .map(|node| node.label.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "middle", "zeta"]
        );
    }

    #[test]
    fn ownership_view_keeps_module_contour_around_ownership_nodes() {
        let mut graph = EvidenceGraph::default();
        let module = NodeId::module("demo", "lib", "runtime");
        let site = NodeId::site("demo", "lib", "runtime", "arc-new@10");
        let resource = NodeId::resource("demo", "lib", "SharedState");
        graph.merge_node(Node::new(
            module.clone(),
            NodeKind::Module,
            "runtime",
            evidence("module"),
        ));
        graph.merge_node(Node::new(
            site.clone(),
            NodeKind::OwnershipSite,
            "Arc::new(...)",
            evidence("site"),
        ));
        graph.merge_node(Node::new(
            resource.clone(),
            NodeKind::Resource,
            "Arc<SharedState>",
            evidence("resource"),
        ));
        graph.merge_edge(Edge::new(
            site,
            resource,
            EdgeKind::Constructs,
            evidence("constructs"),
        ));

        let model = project(
            &graph,
            &ViewRequest {
                kind: ViewKind::Ownership,
                package: None,
                module: None,
                scenario: None,
                lod: DetailLevel::Full,
                filter: ArchitectureFilter::default(),
            },
        );

        assert_eq!(model.nodes.len(), 3);
        assert!(model.nodes.iter().any(|node| node.id == module));
        assert_eq!(model.edges.len(), 1);
    }

    #[test]
    fn recursive_call_component_collapses_into_one_group() {
        let mut graph = EvidenceGraph::default();
        let first = NodeId::symbol("demo", "lib", "runtime", "first");
        let second = NodeId::symbol("demo", "lib", "runtime", "second");
        for (id, label) in [(&first, "first"), (&second, "second")] {
            graph.merge_node(Node::new(
                id.clone(),
                NodeKind::Function,
                label,
                evidence(label),
            ));
        }
        graph.merge_edge(Edge::new(
            first.clone(),
            second.clone(),
            EdgeKind::Calls,
            evidence("first-second"),
        ));
        graph.merge_edge(Edge::new(
            second,
            first,
            EdgeKind::Calls,
            evidence("second-first"),
        ));

        let model = project(
            &graph,
            &ViewRequest {
                kind: ViewKind::Overview,
                package: None,
                module: None,
                scenario: None,
                lod: DetailLevel::Full,
                filter: ArchitectureFilter::default(),
            },
        );

        assert_eq!(model.groups.len(), 1);
        assert_eq!(model.nodes.len(), 1);
        assert_eq!(model.nodes[0].kind, NodeKind::CycleGroup);
        assert!(model.edges.is_empty());
    }

    #[test]
    fn package_projection_keeps_only_outgoing_public_ports() {
        let mut graph = EvidenceGraph::default();
        let core_package = NodeId::package("core");
        let core_module = NodeId::module("core", "core", "runtime");
        let core = NodeId::symbol("core", "core", "runtime", "start");
        let dependency_package = NodeId::package("dependency");
        let dependency = NodeId::symbol("dependency", "dependency", "api", "serve");
        let private_dependency = NodeId::symbol("dependency", "dependency", "internal", "helper");
        let caller = NodeId::symbol("caller", "caller", "api", "run");
        for (id, kind) in [
            (&core_package, NodeKind::Package),
            (&core_module, NodeKind::Module),
            (&core, NodeKind::PublicFunction),
            (&dependency_package, NodeKind::Package),
            (&dependency, NodeKind::PublicFunction),
            (&private_dependency, NodeKind::Function),
            (&caller, NodeKind::PublicFunction),
        ] {
            graph.merge_node(Node::new(
                id.clone(),
                kind,
                &id.symbol,
                evidence(&id.symbol),
            ));
        }
        for (source, target) in [(&core_package, &core_module), (&core_module, &core)] {
            graph.merge_edge(Edge::new(
                source.clone(),
                target.clone(),
                EdgeKind::Contains,
                evidence("contains"),
            ));
        }
        graph.merge_edge(Edge::new(
            core_package.clone(),
            dependency_package.clone(),
            EdgeKind::DependsOn,
            evidence("cargo"),
        ));
        for origin in ["outgoing-a", "outgoing-b"] {
            graph.merge_edge(Edge::new(
                core.clone(),
                dependency.clone(),
                EdgeKind::Calls,
                evidence(origin),
            ));
        }
        graph.merge_edge(Edge::new(
            core.clone(),
            private_dependency.clone(),
            EdgeKind::Calls,
            evidence("private-candidate"),
        ));
        graph.merge_edge(Edge::new(
            caller.clone(),
            core.clone(),
            EdgeKind::Calls,
            evidence("incoming"),
        ));

        let model = project(
            &graph,
            &ViewRequest {
                kind: ViewKind::Overview,
                package: Some("core".to_string()),
                module: None,
                scenario: None,
                lod: DetailLevel::Modules,
                filter: ArchitectureFilter::default(),
            },
        );
        let visible = model
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            visible,
            BTreeSet::from([core_package, core_module.clone(), dependency.clone()])
        );
        assert!(!visible.contains(&dependency_package));
        assert!(!visible.contains(&private_dependency));
        assert!(!visible.contains(&caller));
        let port = model
            .nodes
            .iter()
            .find(|node| node.id == dependency)
            .expect("outgoing public port");
        assert_eq!(port.label, "external: dependency::serve");
        assert_eq!(port.parent, None);
        let outgoing = model
            .edges
            .iter()
            .find(|edge| edge.source == core_module && edge.target == port.id)
            .expect("outgoing public call");
        assert_eq!(outgoing.kind, EdgeKind::Calls);
        assert_eq!(outgoing.count, 2);
        assert!(
            model
                .edges
                .iter()
                .all(|edge| edge.kind != EdgeKind::DependsOn)
        );
    }

    #[test]
    fn package_lod_zero_uses_public_ports_instead_of_dependency_neighbors() {
        let mut graph = EvidenceGraph::default();
        let core_package = NodeId::package("core");
        let dependency_package = NodeId::package("dependency");
        let start = NodeId::symbol("core", "core", "crate", "start");
        let serve = NodeId::symbol("dependency", "dependency", "crate", "serve");
        for (id, label) in [(&core_package, "core"), (&dependency_package, "dependency")] {
            graph.merge_node(Node::new(
                id.clone(),
                NodeKind::Package,
                label,
                evidence(label),
            ));
        }
        for (id, label) in [(&start, "start"), (&serve, "serve")] {
            graph.merge_node(Node::new(
                id.clone(),
                NodeKind::PublicFunction,
                label,
                evidence(label),
            ));
        }
        graph.merge_edge(Edge::new(
            core_package.clone(),
            dependency_package.clone(),
            EdgeKind::DependsOn,
            evidence("cargo"),
        ));
        graph.merge_edge(Edge::new(
            core_package.clone(),
            start.clone(),
            EdgeKind::Contains,
            evidence("core-contains"),
        ));
        graph.merge_edge(Edge::new(
            start,
            serve.clone(),
            EdgeKind::Calls,
            evidence("outgoing"),
        ));

        let model = project(
            &graph,
            &ViewRequest {
                kind: ViewKind::Overview,
                package: Some("core".to_string()),
                module: None,
                scenario: None,
                lod: DetailLevel::Crates,
                filter: ArchitectureFilter::default(),
            },
        );
        let visible = model
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            visible,
            BTreeSet::from([core_package.clone(), serve.clone()])
        );
        assert!(!visible.contains(&dependency_package));
        assert_eq!(
            model
                .nodes
                .iter()
                .find(|node| node.id == serve)
                .expect("public port")
                .label,
            "external: dependency::serve"
        );
        assert_eq!(model.edges.len(), 1);
        assert_eq!(model.edges[0].source, core_package);
        assert_eq!(model.edges[0].target, serve);
        assert_eq!(model.edges[0].kind, EdgeKind::Calls);
    }
}
