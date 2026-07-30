use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{
    filter::ArchitectureFilter,
    graph::{EdgeKind, EvidenceGraph, Node, NodeId, NodeKind},
    view::DiagramModel,
};

const CONTOUR_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub(crate) struct ContourSnapshot {
    schema_version: u32,
    contours: Vec<ContourRecord>,
}

#[derive(Debug, Serialize)]
struct ContourRecord {
    id: NodeId,
    kind: NodeKind,
    label: String,
    parent: Option<NodeId>,
    primary_visible: bool,
}

#[derive(Debug)]
pub(crate) struct ContourIndex {
    parents: BTreeMap<NodeId, NodeId>,
}

impl ContourIndex {
    pub(crate) fn new(graph: &EvidenceGraph) -> Self {
        let parents = graph
            .edges()
            .filter(|edge| edge.kind == EdgeKind::Contains)
            .map(|edge| (edge.target.clone(), edge.source.clone()))
            .collect();
        Self { parents }
    }

    pub(crate) fn parents(&self) -> &BTreeMap<NodeId, NodeId> {
        &self.parents
    }

    pub(crate) fn nearest_selected(
        &self,
        id: &NodeId,
        selected: &BTreeSet<NodeId>,
    ) -> Option<NodeId> {
        let mut current = id;
        let mut seen = BTreeSet::new();
        loop {
            if selected.contains(current) {
                return Some(current.clone());
            }
            if !seen.insert(current.clone()) {
                return None;
            }
            current = self.parents.get(current)?;
        }
    }

    pub(crate) fn is_subsystem(node: &Node) -> bool {
        node.kind == NodeKind::Module
            && (node.id.module == "crate" || !node.id.module.contains("::"))
    }
}

pub(crate) fn snapshot(
    graph: &EvidenceGraph,
    model: &DiagramModel,
    filter: &ArchitectureFilter,
) -> ContourSnapshot {
    let index = ContourIndex::new(graph);
    let visible = model
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let contours = graph
        .nodes()
        .filter(|node| !filter.excludes(&node.id))
        .map(|node| ContourRecord {
            id: node.id.clone(),
            kind: node.kind,
            label: node.label.clone(),
            parent: index
                .parents
                .get(&node.id)
                .filter(|parent| !filter.excludes(parent))
                .cloned(),
            primary_visible: visible.contains(&node.id),
        })
        .collect();
    ContourSnapshot {
        schema_version: CONTOUR_SCHEMA_VERSION,
        contours,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz::graph::{Edge, Evidence, Node};

    #[test]
    fn subsystem_is_a_crate_surface_or_top_level_module() {
        let root = Node::new(
            NodeId::module("demo", "lib", "crate"),
            NodeKind::Module,
            "crate",
            Evidence::static_fact("lib.rs"),
        );
        let top = Node::new(
            NodeId::module("demo", "lib", "runtime"),
            NodeKind::Module,
            "runtime",
            Evidence::static_fact("runtime.rs"),
        );
        let nested = Node::new(
            NodeId::module("demo", "lib", "runtime::worker"),
            NodeKind::Module,
            "worker",
            Evidence::static_fact("runtime/worker.rs"),
        );

        assert!(ContourIndex::is_subsystem(&root));
        assert!(ContourIndex::is_subsystem(&top));
        assert!(!ContourIndex::is_subsystem(&nested));
    }

    #[test]
    fn nearest_selected_walks_the_containment_chain() {
        let module = NodeId::module("demo", "lib", "runtime");
        let abstraction = NodeId::abstraction("demo", "lib", "runtime", "type", "Worker");
        let method = NodeId::symbol("demo", "lib", "runtime", "Worker::run");
        let mut graph = EvidenceGraph::default();
        for (id, kind, label) in [
            (module.clone(), NodeKind::Module, "runtime"),
            (abstraction.clone(), NodeKind::ConcreteType, "Worker"),
            (method.clone(), NodeKind::Function, "run"),
        ] {
            graph.merge_node(Node::new(id, kind, label, Evidence::static_fact(label)));
        }
        graph.merge_edge(Edge::new(
            module.clone(),
            abstraction.clone(),
            EdgeKind::Contains,
            Evidence::static_fact("type"),
        ));
        graph.merge_edge(Edge::new(
            abstraction,
            method.clone(),
            EdgeKind::Contains,
            Evidence::static_fact("method"),
        ));

        let index = ContourIndex::new(&graph);
        assert_eq!(
            index.nearest_selected(&method, &BTreeSet::from([module.clone()])),
            Some(module)
        );
    }
}
