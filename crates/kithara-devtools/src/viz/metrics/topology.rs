use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::{NodeId, Relation, ratio_or_zero};

pub(super) struct Topology {
    pub(super) propagation_cost: f64,
    pub(super) cyclic_scc_count: usize,
    pub(super) cyclic_nodes: usize,
    pub(super) largest_cycle: usize,
    pub(super) normalized_depth: f64,
}

pub(super) fn analyze(subjects: &BTreeSet<NodeId>, relations: &BTreeSet<Relation>) -> Topology {
    if subjects.is_empty() {
        return Topology {
            propagation_cost: 0.0,
            cyclic_scc_count: 0,
            cyclic_nodes: 0,
            largest_cycle: 0,
            normalized_depth: 0.0,
        };
    }
    let ids = subjects.iter().cloned().collect::<Vec<_>>();
    let indices = ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut adjacency = vec![Vec::new(); ids.len()];
    let mut reverse = vec![Vec::new(); ids.len()];
    let mut self_loops = BTreeSet::new();
    for relation in relations {
        let source = indices[&relation.source];
        let target = indices[&relation.target];
        if source == target {
            self_loops.insert(source);
        } else {
            adjacency[source].push(target);
            reverse[target].push(source);
        }
    }
    for edges in adjacency.iter_mut().chain(&mut reverse) {
        edges.sort_unstable();
        edges.dedup();
    }
    let order = finish_order(&adjacency);
    let components = components(&reverse, &order);
    let mut component_of = vec![0; ids.len()];
    for (component, members) in components.iter().enumerate() {
        for member in members {
            component_of[*member] = component;
        }
    }
    let mut dag = vec![BTreeSet::new(); components.len()];
    for (source, targets) in adjacency.iter().enumerate() {
        for target in targets {
            let source_component = component_of[source];
            let target_component = component_of[*target];
            if source_component != target_component {
                dag[source_component].insert(target_component);
            }
        }
    }
    let topological = topological_order(&dag);
    let component_sizes = components.iter().map(Vec::len).collect::<Vec<_>>();
    let propagation_cost = propagation_cost(&dag, &topological, &component_sizes, ids.len());
    let cyclic_sizes = components
        .iter()
        .filter_map(|members| {
            (members.len() > 1 || members.iter().any(|member| self_loops.contains(member)))
                .then_some(members.len())
        })
        .collect::<Vec<_>>();
    let depth = dag_depth(&dag, &topological);

    Topology {
        propagation_cost,
        cyclic_scc_count: cyclic_sizes.len(),
        cyclic_nodes: cyclic_sizes.iter().sum(),
        largest_cycle: cyclic_sizes.into_iter().max().unwrap_or_default(),
        normalized_depth: ratio_or_zero(depth, ids.len().saturating_sub(1).max(1)),
    }
}

fn finish_order(adjacency: &[Vec<usize>]) -> Vec<usize> {
    let mut visited = vec![false; adjacency.len()];
    let mut order = Vec::with_capacity(adjacency.len());
    for start in 0..adjacency.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut stack = vec![(start, 0)];
        while let Some((node, edge_index)) = stack.last_mut() {
            if let Some(next) = adjacency[*node].get(*edge_index).copied() {
                *edge_index += 1;
                if !visited[next] {
                    visited[next] = true;
                    stack.push((next, 0));
                }
            } else {
                order.push(*node);
                stack.pop();
            }
        }
    }
    order
}

fn components(reverse: &[Vec<usize>], order: &[usize]) -> Vec<Vec<usize>> {
    let mut visited = vec![false; reverse.len()];
    let mut components = Vec::new();
    for start in order.iter().rev().copied() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut members = Vec::new();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            members.push(node);
            for next in reverse[node].iter().rev().copied() {
                if !visited[next] {
                    visited[next] = true;
                    stack.push(next);
                }
            }
        }
        members.sort_unstable();
        components.push(members);
    }
    components
}

fn topological_order(dag: &[BTreeSet<usize>]) -> Vec<usize> {
    let mut indegree = vec![0; dag.len()];
    for targets in dag {
        for target in targets {
            indegree[*target] += 1;
        }
    }
    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(node, degree)| (*degree == 0).then_some(node))
        .collect::<VecDeque<_>>();
    let mut order = Vec::with_capacity(dag.len());
    while let Some(node) = ready.pop_front() {
        order.push(node);
        for target in &dag[node] {
            indegree[*target] -= 1;
            if indegree[*target] == 0 {
                ready.push_back(*target);
            }
        }
    }
    order
}

fn propagation_cost(
    dag: &[BTreeSet<usize>],
    topological: &[usize],
    component_sizes: &[usize],
    node_count: usize,
) -> f64 {
    let words = dag.len().div_ceil(u64::BITS as usize);
    let mut reachable = vec![vec![0_u64; words]; dag.len()];
    for component in topological.iter().rev().copied() {
        set_bit(&mut reachable[component], component);
        for target in &dag[component] {
            union_reachable(&mut reachable, component, *target);
        }
    }
    let pairs = reachable
        .iter()
        .enumerate()
        .map(|(component, bits)| {
            let reachable_nodes = component_sizes
                .iter()
                .enumerate()
                .filter(|(target, _)| bit_is_set(bits, *target))
                .map(|(_, size)| size)
                .sum::<usize>();
            component_sizes[component].saturating_mul(reachable_nodes)
        })
        .fold(0_usize, usize::saturating_add);
    ratio_or_zero(pairs, node_count.saturating_mul(node_count))
}

fn set_bit(bits: &mut [u64], bit: usize) {
    bits[bit / u64::BITS as usize] |= 1 << (bit % u64::BITS as usize);
}

fn union_reachable(reachable: &mut [Vec<u64>], destination: usize, source: usize) {
    if destination == source {
        return;
    }
    let (destination, source) = if destination < source {
        let (left, right) = reachable.split_at_mut(source);
        (&mut left[destination], &right[0])
    } else {
        let (left, right) = reachable.split_at_mut(destination);
        (&mut right[0], &left[source])
    };
    for (destination, source) in destination.iter_mut().zip(source) {
        *destination |= source;
    }
}

fn bit_is_set(bits: &[u64], bit: usize) -> bool {
    bits[bit / u64::BITS as usize] & (1 << (bit % u64::BITS as usize)) != 0
}

fn dag_depth(dag: &[BTreeSet<usize>], topological: &[usize]) -> usize {
    let mut depth = vec![0; dag.len()];
    for source in topological {
        for target in &dag[*source] {
            depth[*target] = depth[*target].max(depth[*source] + 1);
        }
    }
    depth.into_iter().max().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz::{
        graph::EdgeKind,
        metrics::{Relation, normalized_propagation},
    };

    fn id(name: &str) -> NodeId {
        NodeId::module("demo", "lib", name)
    }

    fn subjects(names: &[&str]) -> BTreeSet<NodeId> {
        names.iter().map(|name| id(name)).collect()
    }

    fn relation(source: &str, target: &str) -> Relation {
        Relation {
            source: id(source),
            target: id(target),
            kind: EdgeKind::Calls,
        }
    }

    #[test]
    fn independent_contours_have_only_self_propagation() {
        let subjects = subjects(&["a", "b", "c", "d"]);
        let topology = analyze(&subjects, &BTreeSet::new());

        assert_eq!(topology.propagation_cost, 0.25);
        assert_eq!(
            normalized_propagation(topology.propagation_cost, subjects.len()),
            0.0
        );
        assert_eq!(topology.cyclic_nodes, 0);
        assert_eq!(topology.normalized_depth, 0.0);
    }

    #[test]
    fn adding_a_cycle_increases_cyclicity_and_propagation() {
        let subjects = subjects(&["a", "b", "c"]);
        let acyclic = BTreeSet::from([relation("a", "b"), relation("b", "c")]);
        let cyclic = BTreeSet::from([relation("a", "b"), relation("b", "c"), relation("c", "a")]);

        let acyclic = analyze(&subjects, &acyclic);
        let cyclic = analyze(&subjects, &cyclic);

        assert!(cyclic.propagation_cost > acyclic.propagation_cost);
        assert!(
            normalized_propagation(cyclic.propagation_cost, subjects.len())
                > normalized_propagation(acyclic.propagation_cost, subjects.len())
        );
        assert_eq!(cyclic.cyclic_nodes, 3);
        assert_eq!(cyclic.cyclic_scc_count, 1);
        assert_eq!(cyclic.largest_cycle, 3);
    }

    #[test]
    fn chain_depth_is_normalized_by_contour_count() {
        let subjects = subjects(&["a", "b", "c"]);
        let relations = BTreeSet::from([relation("a", "b"), relation("b", "c")]);

        assert_eq!(analyze(&subjects, &relations).normalized_depth, 1.0);
    }
}
