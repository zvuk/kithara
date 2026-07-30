use std::collections::{BTreeMap, BTreeSet};

use super::{
    graph::{EdgeKind, NodeId, NodeKind},
    view::{DiagramEdge, DiagramGroup, DiagramModel, DiagramNode, EvidenceStyle},
};

pub(super) fn collapse(model: &mut DiagramModel) {
    let adjacency = call_adjacency(model);
    if adjacency.is_empty() {
        return;
    }
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for id in adjacency.keys() {
        visit_order(id, &adjacency, &mut seen, &mut order);
    }
    let transposed = transpose(&adjacency);
    seen.clear();
    let mut components = Vec::new();
    while let Some(id) = order.pop() {
        if seen.contains(&id) {
            continue;
        }
        let mut component = Vec::new();
        visit_component(&id, &transposed, &mut seen, &mut component);
        component.sort();
        let self_cycle = component.len() == 1
            && adjacency
                .get(&component[0])
                .is_some_and(|targets| targets.contains(&component[0]));
        if component.len() > 1 || self_cycle {
            components.push(component);
        }
    }
    components.sort();
    if components.is_empty() {
        return;
    }

    let nodes_by_id = model
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let mut replacement = BTreeMap::new();
    let mut group_nodes = Vec::new();
    for members in components {
        let first = &members[0];
        let group_id = NodeId::symbol(
            &first.package,
            &first.target,
            &first.module,
            format!("<cycle:{}:{}>", members.len(), first.symbol),
        );
        for member in &members {
            replacement.insert(member.clone(), group_id.clone());
        }
        let labels = members
            .iter()
            .filter_map(|id| nodes_by_id.get(id).map(|node| node.label.as_str()))
            .take(3)
            .collect::<Vec<_>>();
        let style = members
            .iter()
            .filter_map(|id| nodes_by_id.get(id).map(|node| node.style))
            .fold(EvidenceStyle::Resolved, merge_style);
        let location = members
            .iter()
            .find_map(|id| nodes_by_id.get(id).and_then(|node| node.location.clone()));
        let parent = members
            .iter()
            .find_map(|id| nodes_by_id.get(id).and_then(|node| node.parent.clone()));
        group_nodes.push(DiagramNode {
            id: group_id.clone(),
            kind: NodeKind::CycleGroup,
            label: format!("cycle ({}): {}", members.len(), labels.join(", ")),
            location,
            style,
            parent,
        });
        model.groups.push(DiagramGroup {
            node: group_id,
            members,
        });
    }

    model
        .nodes
        .retain(|node| !replacement.contains_key(&node.id));
    model.nodes.extend(group_nodes);
    model.nodes.sort_by(|left, right| left.id.cmp(&right.id));

    let mut edges = BTreeMap::new();
    for edge in &model.edges {
        let source = replacement
            .get(&edge.source)
            .cloned()
            .unwrap_or_else(|| edge.source.clone());
        let target = replacement
            .get(&edge.target)
            .cloned()
            .unwrap_or_else(|| edge.target.clone());
        if source == target {
            continue;
        }
        let key = (source.clone(), target.clone(), edge.kind);
        edges
            .entry(key)
            .and_modify(|current: &mut DiagramEdge| {
                current.style = merge_style(current.style, edge.style);
                current.count += edge.count;
                current.details.extend(edge.details.iter().cloned());
                current.details.sort();
                current.details.dedup();
                current.origins.extend(edge.origins.iter().cloned());
                current.origins.sort();
                current.origins.dedup();
            })
            .or_insert_with(|| DiagramEdge {
                source,
                target,
                kind: edge.kind,
                style: edge.style,
                count: edge.count,
                details: edge.details.clone(),
                origins: edge.origins.clone(),
            });
    }
    model.edges = edges.into_values().collect();
}

fn call_adjacency(model: &DiagramModel) -> BTreeMap<NodeId, BTreeSet<NodeId>> {
    let mut adjacency = model
        .nodes
        .iter()
        .map(|node| (node.id.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for edge in &model.edges {
        if edge.kind == EdgeKind::Calls
            && adjacency.contains_key(&edge.target)
            && let Some(targets) = adjacency.get_mut(&edge.source)
        {
            targets.insert(edge.target.clone());
        }
    }
    adjacency
}

fn visit_order(
    id: &NodeId,
    adjacency: &BTreeMap<NodeId, BTreeSet<NodeId>>,
    seen: &mut BTreeSet<NodeId>,
    order: &mut Vec<NodeId>,
) {
    if !seen.insert(id.clone()) {
        return;
    }
    if let Some(targets) = adjacency.get(id) {
        for target in targets {
            visit_order(target, adjacency, seen, order);
        }
    }
    order.push(id.clone());
}

fn transpose(adjacency: &BTreeMap<NodeId, BTreeSet<NodeId>>) -> BTreeMap<NodeId, BTreeSet<NodeId>> {
    let mut transposed = adjacency
        .keys()
        .cloned()
        .map(|id| (id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for (source, targets) in adjacency {
        for target in targets {
            if let Some(sources) = transposed.get_mut(target) {
                sources.insert(source.clone());
            }
        }
    }
    transposed
}

fn visit_component(
    id: &NodeId,
    adjacency: &BTreeMap<NodeId, BTreeSet<NodeId>>,
    seen: &mut BTreeSet<NodeId>,
    component: &mut Vec<NodeId>,
) {
    if !seen.insert(id.clone()) {
        return;
    }
    component.push(id.clone());
    if let Some(targets) = adjacency.get(id) {
        for target in targets {
            visit_component(target, adjacency, seen, component);
        }
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
