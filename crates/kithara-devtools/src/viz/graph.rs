use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::Serialize;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct NodeId {
    pub(crate) package: String,
    pub(crate) target: String,
    pub(crate) module: String,
    pub(crate) symbol: String,
}

impl NodeId {
    pub(crate) fn symbol(
        package: impl Into<String>,
        target: impl Into<String>,
        module: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        Self {
            package: package.into(),
            target: target.into(),
            module: module.into(),
            symbol: symbol.into(),
        }
    }

    pub(crate) fn package(package: &str) -> Self {
        Self::symbol(package, "<workspace>", "<package>", "<package>")
    }

    pub(crate) fn module(package: &str, target: &str, module: &str) -> Self {
        Self::symbol(package, target, module, "<module>")
    }

    pub(crate) fn site(package: &str, target: &str, module: &str, site: &str) -> Self {
        Self::symbol(package, target, module, site)
    }

    pub(crate) fn abstraction(
        package: &str,
        target: &str,
        module: &str,
        kind: &str,
        name: &str,
    ) -> Self {
        Self::symbol(
            package,
            target,
            module,
            format!("<abstraction:{kind}:{name}>"),
        )
    }

    pub(crate) fn module_functions(package: &str, target: &str, module: &str) -> Self {
        Self::abstraction(package, target, module, "functions", module)
    }

    pub(crate) fn resource(package: &str, target: &str, resource: &str) -> Self {
        Self::symbol(package, target, "<resource>", resource)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/{}/{}/{}",
            self.package, self.target, self.module, self.symbol
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NodeKind {
    Package,
    Module,
    ConcreteType,
    Trait,
    ModuleFunctions,
    PublicItem,
    OwnershipSite,
    Resource,
    Function,
    PublicFunction,
    Constructor,
    TraitMethod,
    Task,
    Scenario,
    RuntimeEvent,
    UnresolvedCall,
    CycleGroup,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EdgeKind {
    Contains,
    DependsOn,
    Implements,
    Starts,
    Calls,
    Spawns,
    Constructs,
    Clones,
    Stores,
    Transfers,
    Sends,
    Drops,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceClass {
    Static,
    Conditional,
    Resolved,
    Observed,
    Manual,
    Inferred,
    Unresolved,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Certainty {
    Resolved,
    Candidate,
    Unresolved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergedCertainty {
    Resolved,
    Candidate,
    Unresolved,
    Conflicting,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct Evidence {
    pub(crate) class: EvidenceClass,
    pub(crate) certainty: Certainty,
    pub(crate) origin: String,
}

impl Evidence {
    pub(crate) fn static_fact(origin: impl Into<String>) -> Self {
        Self::new(EvidenceClass::Static, Certainty::Resolved, origin)
    }

    pub(crate) fn resolved(origin: impl Into<String>) -> Self {
        Self::new(EvidenceClass::Resolved, Certainty::Resolved, origin)
    }

    pub(crate) fn conditional(origin: impl Into<String>) -> Self {
        Self::new(EvidenceClass::Conditional, Certainty::Resolved, origin)
    }

    pub(crate) fn inferred(origin: impl Into<String>) -> Self {
        Self::new(EvidenceClass::Inferred, Certainty::Candidate, origin)
    }

    pub(crate) fn observed(origin: impl Into<String>) -> Self {
        Self::new(EvidenceClass::Observed, Certainty::Resolved, origin)
    }

    pub(crate) fn manual(origin: impl Into<String>) -> Self {
        Self::new(EvidenceClass::Manual, Certainty::Resolved, origin)
    }

    pub(crate) fn unresolved(origin: impl Into<String>) -> Self {
        Self::new(EvidenceClass::Unresolved, Certainty::Unresolved, origin)
    }

    fn new(class: EvidenceClass, certainty: Certainty, origin: impl Into<String>) -> Self {
        Self {
            class,
            certainty,
            origin: origin.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct SourceLocation {
    pub(crate) path: String,
    pub(crate) line: usize,
    pub(crate) column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Node {
    pub(crate) id: NodeId,
    pub(crate) kind: NodeKind,
    pub(crate) label: String,
    pub(crate) location: Option<SourceLocation>,
    pub(crate) evidence: BTreeSet<Evidence>,
    pub(crate) diagnostics: BTreeSet<String>,
}

impl Node {
    pub(crate) fn new(
        id: NodeId,
        kind: NodeKind,
        label: impl Into<String>,
        evidence: Evidence,
    ) -> Self {
        Self {
            id,
            kind,
            label: label.into(),
            location: None,
            evidence: BTreeSet::from([evidence]),
            diagnostics: BTreeSet::new(),
        }
    }

    pub(crate) fn at(self, path: impl Into<String>, line: usize) -> Self {
        self.at_position(path, line, 0)
    }

    pub(crate) fn at_position(
        mut self,
        path: impl Into<String>,
        line: usize,
        column: usize,
    ) -> Self {
        self.location = Some(SourceLocation {
            path: path.into(),
            line,
            column,
        });
        self
    }

    pub(crate) fn certainty(&self) -> MergedCertainty {
        merge_certainty(self.evidence.iter().map(|evidence| evidence.certainty))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Edge {
    pub(crate) source: NodeId,
    pub(crate) target: NodeId,
    pub(crate) kind: EdgeKind,
    pub(crate) evidence: BTreeSet<Evidence>,
}

impl Edge {
    pub(crate) fn new(source: NodeId, target: NodeId, kind: EdgeKind, evidence: Evidence) -> Self {
        Self {
            source,
            target,
            kind,
            evidence: BTreeSet::from([evidence]),
        }
    }

    pub(crate) fn certainty(&self) -> MergedCertainty {
        merge_certainty(self.evidence.iter().map(|evidence| evidence.certainty))
    }
}

fn merge_certainty(certainties: impl Iterator<Item = Certainty>) -> MergedCertainty {
    let certainties = certainties.collect::<BTreeSet<_>>();
    if certainties.contains(&Certainty::Unresolved) && certainties.len() > 1 {
        return MergedCertainty::Conflicting;
    }
    if certainties.contains(&Certainty::Resolved) {
        MergedCertainty::Resolved
    } else if certainties.contains(&Certainty::Candidate) {
        MergedCertainty::Candidate
    } else {
        MergedCertainty::Unresolved
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct EdgeKey {
    source: NodeId,
    target: NodeId,
    kind: EdgeKind,
}

#[derive(Debug, Default)]
pub(crate) struct EvidenceGraph {
    nodes: BTreeMap<NodeId, Node>,
    edges: BTreeMap<EdgeKey, Edge>,
}

impl EvidenceGraph {
    pub(crate) fn merge_node(&mut self, node: Node) {
        if let Some(current) = self.nodes.get_mut(&node.id) {
            if current.kind != node.kind || current.label != node.label {
                current.diagnostics.insert(format!(
                    "identity collision: {:?} `{}` versus {:?} `{}`",
                    current.kind, current.label, node.kind, node.label
                ));
            }
            current.evidence.extend(node.evidence);
            if current.location.is_none() {
                current.location = node.location;
            }
            current.diagnostics.extend(node.diagnostics);
        } else {
            self.nodes.insert(node.id.clone(), node);
        }
    }

    pub(crate) fn merge_edge(&mut self, edge: Edge) {
        let key = EdgeKey {
            source: edge.source.clone(),
            target: edge.target.clone(),
            kind: edge.kind,
        };
        if let Some(current) = self.edges.get_mut(&key) {
            current.evidence.extend(edge.evidence);
        } else {
            self.edges.insert(key, edge);
        }
    }

    pub(crate) fn add_node_evidence(&mut self, id: &NodeId, evidence: Evidence) -> bool {
        let Some(node) = self.nodes.get_mut(id) else {
            return false;
        };
        node.evidence.insert(evidence);
        true
    }

    pub(crate) fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    #[cfg(test)]
    fn edge(&self, source: &NodeId, target: &NodeId, kind: EdgeKind) -> Option<&Edge> {
        self.edges.get(&EdgeKey {
            source: source.clone(),
            target: target.clone(),
            kind,
        })
    }

    pub(crate) fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    pub(crate) fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.edges.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(certainty: Certainty, origin: &str) -> Evidence {
        Evidence {
            class: EvidenceClass::Static,
            certainty,
            origin: origin.to_string(),
        }
    }

    #[test]
    fn symbol_identity_is_stable_and_structural() {
        let first = NodeId::symbol("demo", "lib", "runtime::worker", "Worker::run");
        let same = NodeId::symbol("demo", "lib", "runtime::worker", "Worker::run");
        let other_target = NodeId::symbol("demo", "cli", "runtime::worker", "Worker::run");

        assert_eq!(first, same);
        assert_ne!(first, other_target);
        assert_eq!(first.to_string(), "demo/lib/runtime::worker/Worker::run");
    }

    #[test]
    fn repeated_nodes_and_edges_merge_evidence() {
        let source = NodeId::symbol("demo", "lib", "runtime", "start");
        let target = NodeId::symbol("demo", "lib", "runtime", "worker");
        let mut graph = EvidenceGraph::default();
        graph.merge_node(Node::new(
            source.clone(),
            NodeKind::PublicItem,
            "start",
            evidence(Certainty::Resolved, "first"),
        ));
        graph.merge_node(Node::new(
            source.clone(),
            NodeKind::PublicItem,
            "start",
            evidence(Certainty::Resolved, "second"),
        ));
        graph.merge_node(Node::new(
            target.clone(),
            NodeKind::PublicItem,
            "worker",
            evidence(Certainty::Resolved, "target"),
        ));
        graph.merge_edge(Edge::new(
            source.clone(),
            target.clone(),
            EdgeKind::Constructs,
            evidence(Certainty::Candidate, "syntax"),
        ));
        graph.merge_edge(Edge::new(
            source.clone(),
            target.clone(),
            EdgeKind::Constructs,
            evidence(Certainty::Resolved, "semantic"),
        ));

        assert_eq!(graph.node(&source).expect("source").evidence.len(), 2);
        let edge = graph
            .edge(&source, &target, EdgeKind::Constructs)
            .expect("merged edge");
        assert_eq!(edge.evidence.len(), 2);
        assert_eq!(edge.certainty(), MergedCertainty::Resolved);
    }

    #[test]
    fn conflicting_certainty_remains_visible() {
        let source = NodeId::symbol("demo", "lib", "runtime", "start");
        let target = NodeId::symbol("demo", "lib", "runtime", "worker");
        let mut graph = EvidenceGraph::default();
        graph.merge_edge(Edge::new(
            source.clone(),
            target.clone(),
            EdgeKind::Constructs,
            evidence(Certainty::Resolved, "semantic"),
        ));
        graph.merge_edge(Edge::new(
            source.clone(),
            target.clone(),
            EdgeKind::Constructs,
            evidence(Certainty::Unresolved, "trace"),
        ));

        assert_eq!(
            graph
                .edge(&source, &target, EdgeKind::Constructs)
                .expect("edge")
                .certainty(),
            MergedCertainty::Conflicting
        );
    }

    #[test]
    fn graph_iteration_is_deterministic() {
        let mut graph = EvidenceGraph::default();
        for label in ["zeta", "alpha", "middle"] {
            let id = NodeId::symbol("demo", "lib", "runtime", label);
            graph.merge_node(Node::new(
                id,
                NodeKind::PublicItem,
                label,
                evidence(Certainty::Resolved, label),
            ));
        }

        let labels = graph
            .nodes()
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["alpha", "middle", "zeta"]);
    }
}
