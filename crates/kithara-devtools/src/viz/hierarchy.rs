use std::collections::{BTreeMap, BTreeSet};

use super::{
    graph::{NodeId, NodeKind},
    mermaid::{self, DetailDiagram},
    view::{DetailLevel, DiagramModel, Projector, ViewRequest},
};

pub(crate) fn plan(
    projector: &Projector<'_>,
    request: &ViewRequest,
    model: &DiagramModel,
) -> Vec<DetailDiagram> {
    match model.lod {
        DetailLevel::Crates => workspace_details(projector, request, model),
        DetailLevel::Modules => subsystem_details(projector, request, model, None, None),
        DetailLevel::Abstractions | DetailLevel::Methods | DetailLevel::Full => Vec::new(),
    }
}

fn workspace_details(
    projector: &Projector<'_>,
    request: &ViewRequest,
    model: &DiagramModel,
) -> Vec<DetailDiagram> {
    let mut details = Vec::new();
    for node in model
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Package)
    {
        let detail = projector.project(&ViewRequest {
            kind: request.kind,
            package: Some(node.id.package.clone()),
            module: None,
            scenario: request.scenario.clone(),
            lod: DetailLevel::Modules,
            filter: request.filter.clone(),
        });
        if detail.nodes.is_empty() {
            continue;
        }
        let path = format!("crates/{}", node.id.package);
        let hotspots = hotspot_modules(&detail);
        details.extend(subsystem_details(
            projector,
            request,
            &detail,
            Some(&path),
            Some(&hotspots),
        ));
        details.push(DetailDiagram {
            label: node.label.clone(),
            path,
            parent: None,
            package: node.id.package.clone(),
            module: None,
            model: detail,
        });
    }
    details
}

fn subsystem_details(
    projector: &Projector<'_>,
    request: &ViewRequest,
    model: &DiagramModel,
    parent: Option<&str>,
    selected: Option<&BTreeSet<NodeId>>,
) -> Vec<DetailDiagram> {
    model
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Module)
        .filter(|node| {
            request
                .package
                .as_ref()
                .is_none_or(|package| node.id.package == *package)
        })
        .filter(|node| selected.is_none_or(|selected| selected.contains(&node.id)))
        .filter_map(|node| {
            let detail = projector.project(&ViewRequest {
                kind: request.kind,
                package: Some(node.id.package.clone()),
                module: Some(node.id.module.clone()),
                scenario: request.scenario.clone(),
                lod: DetailLevel::Abstractions,
                filter: request.filter.clone(),
            });
            (!detail.nodes.is_empty()).then(|| {
                let hash = mermaid::node_id(&node.id);
                let suffix = hash.trim_start_matches("n_");
                DetailDiagram {
                    label: format!("{}::{}", node.id.package, node.label),
                    path: format!(
                        "crates/{}/{}-{suffix}",
                        node.id.package,
                        artifact_segment(&node.id.module)
                    ),
                    parent: parent.map(str::to_string),
                    package: node.id.package.clone(),
                    module: Some(node.id.module.clone()),
                    model: detail,
                }
            })
        })
        .collect()
}

fn hotspot_modules(model: &DiagramModel) -> BTreeSet<NodeId> {
    const HOTSPOT_DEGREE: usize = 4;

    let modules = model
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Module)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let mut degrees = modules
        .iter()
        .cloned()
        .map(|module| (module, 0_usize))
        .collect::<BTreeMap<_, _>>();
    for edge in &model.edges {
        if let Some(degree) = degrees.get_mut(&edge.source) {
            *degree += 1;
        }
        if let Some(degree) = degrees.get_mut(&edge.target) {
            *degree += 1;
        }
    }
    let maximum = degrees.values().copied().max().unwrap_or_default();
    degrees
        .into_iter()
        .filter_map(|(module, degree)| {
            (degree >= HOTSPOT_DEGREE || maximum > 0 && degree == maximum).then_some(module)
        })
        .collect()
}

fn artifact_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect()
}
