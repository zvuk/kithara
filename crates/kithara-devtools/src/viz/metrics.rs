use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{
    contour::ContourIndex,
    filter::ArchitectureFilter,
    graph::{Certainty, Edge, EdgeKind, EvidenceClass, EvidenceGraph, NodeId, NodeKind},
    view::{DiagramModel, DiagramNode},
};

mod topology;

pub(crate) const METRICS_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricsScopeKind {
    Workspace,
    Crate,
    Module,
}

#[derive(Debug, Serialize)]
pub(crate) struct MetricsScope {
    pub(crate) kind: MetricsScopeKind,
    pub(crate) package: Option<String>,
    pub(crate) module: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct MetricsProfile {
    pub(crate) node_count: usize,
    pub(crate) internal_relations: usize,
    pub(crate) incoming_relations: usize,
    pub(crate) outgoing_relations: usize,
    pub(crate) afferent_coupling: usize,
    pub(crate) efferent_coupling: usize,
    pub(crate) instability: Option<f64>,
    pub(crate) cohesion: Option<f64>,
    pub(crate) boundary_alignment: Option<f64>,
    pub(crate) propagation_cost: f64,
    pub(crate) normalized_propagation: f64,
    pub(crate) cyclic_scc_count: usize,
    pub(crate) cyclic_nodes: usize,
    pub(crate) cyclicity: f64,
    pub(crate) largest_cycle: usize,
    pub(crate) normalized_depth: f64,
    pub(crate) bottleneck_concentration: Option<f64>,
    pub(crate) boundary_load: Option<f64>,
    pub(crate) shared_resource_ratio: Option<f64>,
    pub(crate) multi_owner_resources: usize,
    pub(crate) external_coupling_ratio: Option<f64>,
    pub(crate) abstractness: Option<f64>,
    pub(crate) main_sequence_distance: Option<f64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RuntimeMetrics {
    pub(crate) observed_relations: usize,
    pub(crate) manual_relations: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct ArchitectureMetrics {
    pub(crate) schema_version: u32,
    pub(crate) scope: MetricsScope,
    pub(crate) confirmed: MetricsProfile,
    pub(crate) including_candidates: MetricsProfile,
    pub(crate) runtime: RuntimeMetrics,
    pub(crate) architecture_complexity_index: Option<f64>,
    pub(crate) including_candidates_complexity_index: Option<f64>,
    pub(crate) contributions: BTreeMap<&'static str, f64>,
    pub(crate) candidate_contributions: BTreeMap<&'static str, f64>,
    pub(crate) limitations: Vec<&'static str>,
    pub(crate) contours: BTreeMap<String, Box<Self>>,
}

pub(crate) struct MetricsAnalyzer<'a> {
    graph: &'a EvidenceGraph,
    filter: &'a ArchitectureFilter,
    contours: &'a ContourIndex,
}

struct MetricsRequest<'a> {
    graph: &'a EvidenceGraph,
    model: &'a DiagramModel,
    filter: &'a ArchitectureFilter,
    package: Option<&'a str>,
    module: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Relation {
    source: NodeId,
    target: NodeId,
    kind: EdgeKind,
}

struct ContractedGraph {
    subjects: BTreeSet<NodeId>,
    selected: BTreeSet<NodeId>,
    relations: BTreeMap<Relation, Certainty>,
}

impl<'a> MetricsAnalyzer<'a> {
    pub(crate) fn new(
        graph: &'a EvidenceGraph,
        filter: &'a ArchitectureFilter,
        contours: &'a ContourIndex,
    ) -> Self {
        Self {
            graph,
            filter,
            contours,
        }
    }

    pub(crate) fn analyze(
        &self,
        model: &DiagramModel,
        package: Option<&str>,
        module: Option<&str>,
    ) -> ArchitectureMetrics {
        analyze(
            &MetricsRequest {
                graph: self.graph,
                model,
                filter: self.filter,
                package,
                module,
            },
            self.contours,
        )
    }
}

fn analyze(request: &MetricsRequest<'_>, contours: &ContourIndex) -> ArchitectureMetrics {
    let subjects = primary_contours(request);
    let external = external_contours(request, &subjects);
    let selected = subjects
        .iter()
        .chain(&external)
        .cloned()
        .collect::<BTreeSet<_>>();
    let crate_surface = request
        .package
        .and_then(|package| crate_surface(&subjects, package));
    let relations = collect_relations(request, contours, &selected, crate_surface.as_ref());
    let contracted = ContractedGraph {
        subjects,
        selected,
        relations,
    };
    let confirmed_relations = contracted
        .relations
        .iter()
        .filter(|(_, certainty)| **certainty == Certainty::Resolved)
        .map(|(relation, _)| relation.clone())
        .collect::<BTreeSet<_>>();
    let candidate_relations = contracted
        .relations
        .iter()
        .filter(|(_, certainty)| matches!(certainty, Certainty::Resolved | Certainty::Candidate))
        .map(|(relation, _)| relation.clone())
        .collect::<BTreeSet<_>>();
    let confirmed_ownership = ownership_profile(request, contours, &contracted.subjects, false);
    let candidate_ownership = ownership_profile(request, contours, &contracted.subjects, true);
    let abstraction = abstraction_profile(request, contours, &contracted.subjects);
    let confirmed = profile(
        &contracted.subjects,
        &confirmed_relations,
        confirmed_ownership,
        abstraction,
    );
    let including_candidates = profile(
        &contracted.subjects,
        &candidate_relations,
        candidate_ownership,
        abstraction,
    );
    let (architecture_complexity_index, contributions) = complexity_index(&confirmed);
    let (including_candidates_complexity_index, candidate_contributions) =
        complexity_index(&including_candidates);
    let runtime = runtime_metrics(
        request,
        contours,
        &contracted.selected,
        crate_surface.as_ref(),
    );

    ArchitectureMetrics {
        schema_version: METRICS_SCHEMA_VERSION,
        scope: MetricsScope {
            kind: match (request.package, request.module) {
                (_, Some(_)) => MetricsScopeKind::Module,
                (Some(_), None) => MetricsScopeKind::Crate,
                (None, None) => MetricsScopeKind::Workspace,
            },
            package: request.package.map(str::to_string),
            module: request.module.map(str::to_string),
        },
        confirmed,
        including_candidates,
        runtime,
        architecture_complexity_index,
        including_candidates_complexity_index,
        contributions,
        candidate_contributions,
        limitations: vec![
            "candidate-only relations are excluded from the stable index",
            "runtime evidence is reported separately from static complexity",
            "inferred-community alignment is not scored yet",
        ],
        contours: BTreeMap::new(),
    }
}

impl ArchitectureMetrics {
    pub(crate) fn insert_contour(&mut self, path: String, metrics: Self) {
        self.contours.insert(path, Box::new(metrics));
    }
}

fn primary_contours(request: &MetricsRequest<'_>) -> BTreeSet<NodeId> {
    let scoped = request
        .model
        .nodes
        .iter()
        .filter(|node| node_is_static(request.graph, node))
        .filter(|node| node_in_scope(node, request.package, request.module))
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let parents_with_children = request
        .model
        .nodes
        .iter()
        .filter_map(|node| {
            node.parent
                .as_ref()
                .filter(|parent| scoped.contains(*parent))
                .cloned()
        })
        .collect::<BTreeSet<_>>();
    let mut primary = scoped
        .difference(&parents_with_children)
        .cloned()
        .collect::<BTreeSet<_>>();
    if request.package.is_some()
        && request.module.is_none()
        && let Some(surface) = scoped.iter().find(|id| id.module == "crate")
    {
        primary.insert(surface.clone());
    }
    primary
}

fn external_contours(
    request: &MetricsRequest<'_>,
    subjects: &BTreeSet<NodeId>,
) -> BTreeSet<NodeId> {
    let parents_with_children = request
        .model
        .nodes
        .iter()
        .filter_map(|node| node.parent.clone())
        .collect::<BTreeSet<_>>();
    request
        .model
        .nodes
        .iter()
        .filter(|node| node_is_static(request.graph, node))
        .filter(|node| !subjects.contains(&node.id))
        .filter(|node| !node_in_scope(node, request.package, request.module))
        .filter(|node| !parents_with_children.contains(&node.id))
        .map(|node| node.id.clone())
        .collect()
}

fn node_in_scope(node: &DiagramNode, package: Option<&str>, module: Option<&str>) -> bool {
    package.is_none_or(|package| node.id.package == package)
        && module.is_none_or(|module| module_contains(module, &node.id.module))
}

fn node_is_static(graph: &EvidenceGraph, node: &DiagramNode) -> bool {
    graph.node(&node.id).is_some_and(|source| {
        source
            .evidence
            .iter()
            .any(|evidence| is_static_class(evidence.class))
    })
}

fn module_contains(parent: &str, candidate: &str) -> bool {
    candidate == parent
        || candidate
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with("::"))
}

fn edge_touches_scope(edge: &Edge, request: &MetricsRequest<'_>) -> bool {
    if request.package.is_some() && request.module.is_none() {
        return id_in_source_scope(&edge.source, request);
    }
    request.package.is_none()
        || id_in_source_scope(&edge.source, request)
        || id_in_source_scope(&edge.target, request)
}

fn id_in_source_scope(id: &NodeId, request: &MetricsRequest<'_>) -> bool {
    request.package.is_none_or(|package| id.package == package)
        && request
            .module
            .is_none_or(|module| module_contains(module, &id.module))
}

fn crate_surface(subjects: &BTreeSet<NodeId>, package: &str) -> Option<NodeId> {
    subjects
        .iter()
        .find(|id| id.package == package && id.module == "crate")
        .cloned()
}

fn collect_relations(
    request: &MetricsRequest<'_>,
    contours: &ContourIndex,
    selected: &BTreeSet<NodeId>,
    crate_surface: Option<&NodeId>,
) -> BTreeMap<Relation, Certainty> {
    let mut relations = BTreeMap::new();
    for edge in request
        .graph
        .edges()
        .filter(|edge| edge.kind != EdgeKind::Contains)
        .filter(|edge| edge_touches_scope(edge, request))
        .filter(|edge| {
            !request.filter.excludes(&edge.source) && !request.filter.excludes(&edge.target)
        })
    {
        let Some(certainty) = static_certainty(edge) else {
            continue;
        };
        let Some(source) = lift_endpoint(
            &edge.source,
            request.package,
            contours,
            selected,
            crate_surface,
        ) else {
            continue;
        };
        let Some(target) = lift_endpoint(
            &edge.target,
            request.package,
            contours,
            selected,
            crate_surface,
        ) else {
            continue;
        };
        if source == target {
            continue;
        }
        relations
            .entry(Relation {
                source,
                target,
                kind: edge.kind,
            })
            .and_modify(|current| *current = merge_certainty(*current, certainty))
            .or_insert(certainty);
    }
    relations
}

fn lift_endpoint(
    id: &NodeId,
    package: Option<&str>,
    contours: &ContourIndex,
    selected: &BTreeSet<NodeId>,
    crate_surface: Option<&NodeId>,
) -> Option<NodeId> {
    if package.is_some_and(|package| id == &NodeId::package(package)) {
        return crate_surface.cloned();
    }
    contours.nearest_selected(id, selected)
}

fn static_certainty(edge: &Edge) -> Option<Certainty> {
    let mut candidate = false;
    for evidence in edge
        .evidence
        .iter()
        .filter(|evidence| is_static_class(evidence.class))
    {
        match evidence.certainty {
            Certainty::Resolved => return Some(Certainty::Resolved),
            Certainty::Candidate => candidate = true,
            Certainty::Unresolved => {}
        }
    }
    candidate.then_some(Certainty::Candidate)
}

fn merge_certainty(left: Certainty, right: Certainty) -> Certainty {
    if left == Certainty::Resolved || right == Certainty::Resolved {
        Certainty::Resolved
    } else {
        Certainty::Candidate
    }
}

fn is_static_class(class: EvidenceClass) -> bool {
    !matches!(class, EvidenceClass::Observed | EvidenceClass::Manual)
}

fn profile(
    subjects: &BTreeSet<NodeId>,
    relations: &BTreeSet<Relation>,
    ownership: OwnershipProfile,
    abstraction: AbstractionProfile,
) -> MetricsProfile {
    let internal = relations
        .iter()
        .filter(|relation| {
            subjects.contains(&relation.source) && subjects.contains(&relation.target)
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    let incoming = relations
        .iter()
        .filter(|relation| {
            !subjects.contains(&relation.source) && subjects.contains(&relation.target)
        })
        .collect::<Vec<_>>();
    let outgoing = relations
        .iter()
        .filter(|relation| {
            subjects.contains(&relation.source) && !subjects.contains(&relation.target)
        })
        .collect::<Vec<_>>();
    let afferent = incoming
        .iter()
        .map(|relation| &relation.source)
        .collect::<BTreeSet<_>>()
        .len();
    let efferent = outgoing
        .iter()
        .map(|relation| &relation.target)
        .collect::<BTreeSet<_>>()
        .len();
    let incident_relations = internal.len() + incoming.len() + outgoing.len();
    let topology = topology::analyze(subjects, &internal);
    let degrees = boundary_degrees(subjects, relations);
    let degree_sum = degrees.iter().sum::<usize>();
    let boundary_relations = incoming.len() + outgoing.len();
    let boundary_load = ratio(
        boundary_relations,
        boundary_relations.saturating_add(subjects.len()),
    );
    let external_coupling_ratio = ratio(boundary_relations, incident_relations);
    let instability = ratio(efferent, afferent + efferent);
    let main_sequence_distance = instability
        .zip(abstraction.abstractness)
        .map(|(instability, abstractness)| round((instability + abstractness - 1.0).abs()));

    MetricsProfile {
        node_count: subjects.len(),
        internal_relations: internal.len(),
        incoming_relations: incoming.len(),
        outgoing_relations: outgoing.len(),
        afferent_coupling: afferent,
        efferent_coupling: efferent,
        instability,
        cohesion: ratio(internal.len(), incident_relations),
        boundary_alignment: external_coupling_ratio.map(|ratio| round(1.0 - ratio)),
        propagation_cost: topology.propagation_cost,
        normalized_propagation: normalized_propagation(topology.propagation_cost, subjects.len()),
        cyclic_scc_count: topology.cyclic_scc_count,
        cyclic_nodes: topology.cyclic_nodes,
        cyclicity: ratio_or_zero(topology.cyclic_nodes, subjects.len()),
        largest_cycle: topology.largest_cycle,
        normalized_depth: topology.normalized_depth,
        bottleneck_concentration: degrees
            .iter()
            .max()
            .and_then(|maximum| ratio(*maximum, degree_sum)),
        boundary_load,
        shared_resource_ratio: ownership.shared_resource_ratio,
        multi_owner_resources: ownership.multi_owner_resources,
        external_coupling_ratio,
        abstractness: abstraction.abstractness,
        main_sequence_distance,
    }
}

fn boundary_degrees(subjects: &BTreeSet<NodeId>, relations: &BTreeSet<Relation>) -> Vec<usize> {
    subjects
        .iter()
        .map(|subject| {
            relations
                .iter()
                .filter(|relation| {
                    let source_is_subject = subjects.contains(&relation.source);
                    let target_is_subject = subjects.contains(&relation.target);
                    source_is_subject != target_is_subject
                        && (relation.source == *subject || relation.target == *subject)
                })
                .count()
        })
        .collect()
}

#[derive(Clone, Copy)]
struct AbstractionProfile {
    abstractness: Option<f64>,
}

fn abstraction_profile(
    request: &MetricsRequest<'_>,
    contours: &ContourIndex,
    subjects: &BTreeSet<NodeId>,
) -> AbstractionProfile {
    let mut traits = 0;
    let mut concrete_types = 0;
    for node in request
        .graph
        .nodes()
        .filter(|node| !request.filter.excludes(&node.id))
        .filter(|node| id_in_source_scope(&node.id, request))
    {
        if contours.nearest_selected(&node.id, subjects).is_none() {
            continue;
        }
        match node.kind {
            NodeKind::Trait => traits += 1,
            NodeKind::ConcreteType => concrete_types += 1,
            _ => {}
        }
    }
    AbstractionProfile {
        abstractness: ratio(traits, traits + concrete_types),
    }
}

#[derive(Clone, Copy)]
struct OwnershipProfile {
    shared_resource_ratio: Option<f64>,
    multi_owner_resources: usize,
}

fn ownership_profile(
    request: &MetricsRequest<'_>,
    contours: &ContourIndex,
    subjects: &BTreeSet<NodeId>,
    include_candidates: bool,
) -> OwnershipProfile {
    let resources = request
        .graph
        .nodes()
        .filter(|node| node.kind == NodeKind::Resource)
        .filter(|node| id_in_source_scope(&node.id, request))
        .filter(|node| !request.filter.excludes(&node.id))
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let mut owners = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();
    for edge in request
        .graph
        .edges()
        .filter(|edge| is_ownership_edge(edge.kind))
        .filter(|edge| {
            static_certainty(edge)
                .is_some_and(|certainty| certainty == Certainty::Resolved || include_candidates)
        })
        .filter(|edge| edge_touches_scope(edge, request))
        .filter(|edge| {
            !request.filter.excludes(&edge.source) && !request.filter.excludes(&edge.target)
        })
    {
        let (resource, endpoint) = if resources.contains(&edge.target) {
            (&edge.target, &edge.source)
        } else if resources.contains(&edge.source) {
            (&edge.source, &edge.target)
        } else {
            continue;
        };
        if let Some(owner) = contours.nearest_selected(endpoint, subjects) {
            owners.entry(resource.clone()).or_default().insert(owner);
        }
    }
    let owned_resources = owners.len();
    let multi_owner_resources = owners.values().filter(|owners| owners.len() > 1).count();
    OwnershipProfile {
        shared_resource_ratio: ratio(multi_owner_resources, owned_resources),
        multi_owner_resources,
    }
}

fn is_ownership_edge(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Constructs
            | EdgeKind::Clones
            | EdgeKind::Stores
            | EdgeKind::Transfers
            | EdgeKind::Sends
            | EdgeKind::Drops
    )
}

fn runtime_metrics(
    request: &MetricsRequest<'_>,
    contours: &ContourIndex,
    selected: &BTreeSet<NodeId>,
    crate_surface: Option<&NodeId>,
) -> RuntimeMetrics {
    let mut observed_relations = BTreeSet::new();
    let mut manual_relations = BTreeSet::new();
    for edge in request
        .graph
        .edges()
        .filter(|edge| edge_touches_scope(edge, request))
        .filter(|edge| {
            !request.filter.excludes(&edge.source) && !request.filter.excludes(&edge.target)
        })
    {
        let observed = edge
            .evidence
            .iter()
            .any(|evidence| evidence.class == EvidenceClass::Observed);
        let manual = edge
            .evidence
            .iter()
            .any(|evidence| evidence.class == EvidenceClass::Manual);
        if !observed && !manual {
            continue;
        }
        let Some(source) = lift_endpoint(
            &edge.source,
            request.package,
            contours,
            selected,
            crate_surface,
        ) else {
            continue;
        };
        let Some(target) = lift_endpoint(
            &edge.target,
            request.package,
            contours,
            selected,
            crate_surface,
        ) else {
            continue;
        };
        if source == target {
            continue;
        }
        let relation = Relation {
            source,
            target,
            kind: edge.kind,
        };
        if observed {
            observed_relations.insert(relation.clone());
        }
        if manual {
            manual_relations.insert(relation);
        }
    }
    RuntimeMetrics {
        observed_relations: observed_relations.len(),
        manual_relations: manual_relations.len(),
    }
}

fn complexity_index(profile: &MetricsProfile) -> (Option<f64>, BTreeMap<&'static str, f64>) {
    let boundary_pressure = |ratio: Option<f64>| {
        ratio
            .zip(profile.boundary_load)
            .map(|(ratio, load)| round(ratio * load))
    };
    let mut contributions = BTreeMap::from([
        (
            "bottleneck_pressure",
            boundary_pressure(profile.bottleneck_concentration),
        ),
        ("cyclicity", Some(profile.cyclicity)),
        (
            "external_coupling_pressure",
            boundary_pressure(profile.external_coupling_ratio),
        ),
        ("normalized_depth", Some(profile.normalized_depth)),
        ("propagation", Some(profile.normalized_propagation)),
        ("shared_ownership", profile.shared_resource_ratio),
    ]);
    contributions.retain(|_, value| value.is_some());
    let contributions = contributions
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, round(value))))
        .collect::<BTreeMap<_, _>>();
    let divisor = u32::try_from(contributions.len()).ok().map(f64::from);
    let index = divisor
        .filter(|divisor| *divisor > 0.0)
        .map(|divisor| round(100.0 * contributions.values().sum::<f64>() / divisor));
    (index, contributions)
}

fn normalized_propagation(propagation_cost: f64, node_count: usize) -> f64 {
    let Some(node_count) = u32::try_from(node_count).ok().map(f64::from) else {
        return propagation_cost;
    };
    if node_count <= 1.0 {
        return 0.0;
    }
    let self_baseline = 1.0 / node_count;
    round(((propagation_cost - self_baseline) / (1.0 - self_baseline)).clamp(0.0, 1.0))
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    let numerator = u32::try_from(numerator).ok().map(f64::from)?;
    let denominator = u32::try_from(denominator)
        .ok()
        .filter(|denominator| *denominator > 0)
        .map(f64::from)?;
    Some(round(numerator / denominator))
}

fn ratio_or_zero(numerator: usize, denominator: usize) -> f64 {
    ratio(numerator, denominator).unwrap_or(0.0)
}

fn round(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::{MetricsProfile, complexity_index};

    #[test]
    fn one_boundary_relation_does_not_dominate_complexity() {
        let profile = MetricsProfile {
            node_count: 20,
            incoming_relations: 1,
            afferent_coupling: 1,
            bottleneck_concentration: Some(1.0),
            boundary_load: Some(0.047_619),
            external_coupling_ratio: Some(1.0),
            ..MetricsProfile::default()
        };

        let (index, contributions) = complexity_index(&profile);

        assert_eq!(index, Some(1.904_76));
        assert_eq!(
            contributions.get("external_coupling_pressure"),
            Some(&0.047_619)
        );
        assert_eq!(contributions.get("bottleneck_pressure"), Some(&0.047_619));
    }
}
