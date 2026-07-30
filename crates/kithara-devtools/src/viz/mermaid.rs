use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use anyhow::{Result, bail};

use super::{
    graph::{EdgeKind, NodeId, NodeKind},
    view::{DetailLevel, DiagramEdge, DiagramModel, DiagramNode, EvidenceStyle},
};

pub(crate) struct DiagramSet {
    pub(crate) index: String,
    pub(crate) pages: Vec<DiagramPage>,
    pub(crate) covered_nodes: usize,
    pub(crate) state: DiagramSetState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiagramSetState {
    Single,
    Hierarchical,
    Partitioned,
}

impl DiagramSetState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Hierarchical => "hierarchical",
            Self::Partitioned => "partitioned",
        }
    }
}

pub(crate) struct DetailDiagram {
    pub(crate) label: String,
    pub(crate) path: String,
    pub(crate) parent: Option<String>,
    pub(crate) package: String,
    pub(crate) module: Option<String>,
    pub(crate) model: DiagramModel,
}

pub(crate) struct DiagramPage {
    pub(crate) label: String,
    pub(crate) path: String,
    pub(crate) parent: Option<String>,
    pub(crate) document_file: String,
    pub(crate) mermaid_file: String,
    pub(crate) mermaid: String,
    pub(crate) visible_nodes: usize,
    pub(crate) model: DiagramModel,
}

pub(crate) fn render_set(model: &DiagramModel, details: Vec<DetailDiagram>) -> Result<DiagramSet> {
    if model.lod != DetailLevel::Full {
        let mut covered = model
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        let mut pages = Vec::new();
        for detail in details {
            covered.extend(detail.model.nodes.iter().map(|node| node.id.clone()));
            let mermaid = render(&detail.model)?;
            pages.push(DiagramPage {
                label: detail.label,
                document_file: format!("{}.md", detail.path),
                mermaid_file: format!("{}.mmd", detail.path),
                path: detail.path,
                parent: detail.parent,
                mermaid,
                visible_nodes: detail.model.nodes.len(),
                model: detail.model,
            });
        }
        pages.sort_by(|left, right| left.label.cmp(&right.label));
        return Ok(DiagramSet {
            index: render(model)?,
            state: if pages.is_empty() {
                DiagramSetState::Single
            } else {
                DiagramSetState::Hierarchical
            },
            pages,
            covered_nodes: covered.len(),
        });
    }

    let children = model_children(model);
    let roots = model
        .nodes
        .iter()
        .filter(|node| {
            node.parent
                .as_ref()
                .is_none_or(|parent| !model.nodes.iter().any(|candidate| &candidate.id == parent))
        })
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let mut index_nodes = roots.clone();
    for root in &roots {
        if let Some(descendants) = children.get(root) {
            index_nodes.extend(descendants.iter().cloned());
        }
    }
    let index = render(&reproject(model, &index_nodes))?;

    let mut covered = index_nodes;
    let mut pages = Vec::new();
    for node in &model.nodes {
        let Some(descendants) = children.get(&node.id) else {
            continue;
        };
        let visible = descendants.iter().cloned().collect::<BTreeSet<_>>();
        covered.extend(visible.iter().cloned());
        let suffix = node_id(&node.id).trim_start_matches("n_").to_string();
        let page_model = reproject(model, &visible);
        pages.push(DiagramPage {
            label: contour_label(model, &node.id),
            path: format!("contours/{suffix}"),
            parent: None,
            document_file: format!("contours/{suffix}.md"),
            mermaid_file: format!("contours/{suffix}.mmd"),
            mermaid: render(&page_model)?,
            visible_nodes: visible.len(),
            model: page_model,
        });
    }
    pages.sort_by(|left, right| left.label.cmp(&right.label));

    Ok(DiagramSet {
        index,
        pages,
        covered_nodes: covered.len(),
        state: DiagramSetState::Partitioned,
    })
}

pub(crate) fn render(model: &DiagramModel) -> Result<String> {
    let mut output = String::from("flowchart LR\n");
    let mut ids = BTreeMap::new();
    let mut node_ids = BTreeMap::new();
    for node in &model.nodes {
        let rendered_id = node_id(&node.id);
        if let Some(previous) = ids.insert(rendered_id.clone(), node.id.clone())
            && previous != node.id
        {
            bail!(
                "Mermaid node ID collision between `{previous}` and `{}`",
                node.id
            );
        }
        node_ids.insert(node.id.clone(), rendered_id.clone());
    }
    let subgraphs = render_nodes(&mut output, model, &node_ids)?;
    for (index, edge) in model.edges.iter().enumerate() {
        let source = node_ids
            .get(&edge.source)
            .ok_or_else(|| anyhow::anyhow!("visible edge source is missing: {}", edge.source))?;
        let target = node_ids
            .get(&edge.target)
            .ok_or_else(|| anyhow::anyhow!("visible edge target is missing: {}", edge.target))?;
        writeln!(output, "    {source} -->|{}| {target}", edge_label(edge))?;
        match edge.style {
            EvidenceStyle::Resolved => {}
            EvidenceStyle::Conditional => {
                writeln!(
                    output,
                    "    linkStyle {index} stroke:#6b46c1,stroke-dasharray:6 3"
                )?;
            }
            EvidenceStyle::Observed => {
                writeln!(
                    output,
                    "    linkStyle {index} stroke:#2b6cb0,stroke-width:2px"
                )?;
            }
            EvidenceStyle::Manual => {
                writeln!(
                    output,
                    "    linkStyle {index} stroke:#805ad5,stroke-width:2px,stroke-dasharray:4 2"
                )?;
            }
            EvidenceStyle::Candidate => {
                writeln!(output, "    linkStyle {index} stroke-dasharray: 5 4")?;
            }
            EvidenceStyle::Unresolved => {
                writeln!(
                    output,
                    "    linkStyle {index} stroke:#b7791f,stroke-dasharray: 3 3"
                )?;
            }
            EvidenceStyle::Conflicting => {
                writeln!(
                    output,
                    "    linkStyle {index} stroke:#c53030,stroke-width:2px"
                )?;
            }
        }
    }
    writeln!(
        output,
        "    classDef resolved fill:#e6fffa,stroke:#2c7a7b,color:#1a202c"
    )?;
    writeln!(
        output,
        "    classDef conditional fill:#faf5ff,stroke:#6b46c1,stroke-dasharray:6 3,color:#1a202c"
    )?;
    writeln!(
        output,
        "    classDef observed fill:#ebf8ff,stroke:#2b6cb0,stroke-width:2px,color:#1a202c"
    )?;
    writeln!(
        output,
        "    classDef manual fill:#faf5ff,stroke:#805ad5,stroke-width:2px,stroke-dasharray:4 2,color:#1a202c"
    )?;
    writeln!(
        output,
        "    classDef candidate fill:#ebf8ff,stroke:#3182ce,stroke-dasharray:5 4,color:#1a202c"
    )?;
    writeln!(
        output,
        "    classDef unresolved fill:#fffaf0,stroke:#b7791f,stroke-dasharray:3 3,color:#1a202c"
    )?;
    writeln!(
        output,
        "    classDef conflicting fill:#fff5f5,stroke:#c53030,stroke-width:2px,color:#1a202c"
    )?;
    for node in &model.nodes {
        let rendered_id = &node_ids[&node.id];
        if subgraphs.contains(&node.id) {
            continue;
        }
        writeln!(output, "    class {rendered_id} {}", style_name(node.style))?;
        if let Some(location) = &node.location {
            writeln!(
                output,
                "    click {rendered_id} \"{}#L{}\" \"Open source\"",
                escape_link(&location.path),
                location.line
            )?;
        }
    }
    Ok(output)
}

fn model_children(model: &DiagramModel) -> BTreeMap<NodeId, Vec<NodeId>> {
    let visible = model
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let mut children = BTreeMap::<NodeId, Vec<NodeId>>::new();
    for node in &model.nodes {
        if let Some(parent) = node
            .parent
            .as_ref()
            .filter(|parent| visible.contains(*parent))
        {
            children
                .entry(parent.clone())
                .or_default()
                .push(node.id.clone());
        }
    }
    for descendants in children.values_mut() {
        descendants.sort();
    }
    children
}

fn reproject(model: &DiagramModel, visible: &BTreeSet<NodeId>) -> DiagramModel {
    let parents = model
        .nodes
        .iter()
        .filter_map(|node| {
            node.parent
                .as_ref()
                .map(|parent| (node.id.clone(), parent.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut edges = BTreeMap::new();
    for edge in &model.edges {
        let Some(source) = nearest_selected(&edge.source, visible, &parents) else {
            continue;
        };
        let Some(target) = nearest_selected(&edge.target, visible, &parents) else {
            continue;
        };
        if source == target {
            continue;
        }
        edges
            .entry((source.clone(), target.clone(), edge.kind))
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
    DiagramModel {
        kind: model.kind,
        lod: model.lod,
        nodes: model
            .nodes
            .iter()
            .filter(|node| visible.contains(&node.id))
            .cloned()
            .map(|mut node| {
                node.parent = node.parent.filter(|parent| visible.contains(parent));
                node
            })
            .collect(),
        edges: edges.into_values().collect(),
        groups: model
            .groups
            .iter()
            .filter(|group| visible.contains(&group.node))
            .cloned()
            .collect(),
        hidden_nodes: 0,
    }
}

fn nearest_selected(
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

fn contour_label(model: &DiagramModel, id: &NodeId) -> String {
    let nodes = model
        .nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect::<BTreeMap<_, _>>();
    let mut labels = Vec::new();
    let mut current = Some(id);
    while let Some(node_id) = current {
        let Some(node) = nodes.get(node_id) else {
            break;
        };
        labels.push(node.label.as_str());
        current = node.parent.as_ref();
    }
    labels.reverse();
    labels.join(" / ")
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

fn render_nodes(
    output: &mut String,
    model: &DiagramModel,
    node_ids: &BTreeMap<NodeId, String>,
) -> Result<BTreeSet<NodeId>> {
    let nodes = model
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let mut children = BTreeMap::<NodeId, Vec<NodeId>>::new();
    let mut roots = Vec::new();
    for node in &model.nodes {
        if let Some(parent) = node
            .parent
            .as_ref()
            .filter(|parent| nodes.contains_key(*parent))
        {
            children
                .entry(parent.clone())
                .or_default()
                .push(node.id.clone());
        } else {
            roots.push(node.id.clone());
        }
    }
    for ids in children.values_mut() {
        ids.sort();
    }
    roots.sort();

    let mut context = RenderContext {
        lod: model.lod,
        nodes,
        children,
        node_ids,
        rendered: BTreeSet::new(),
        subgraphs: BTreeSet::new(),
    };
    for root in roots {
        render_node(output, &root, 1, &mut context)?;
    }
    let remaining = context.nodes.keys().cloned().collect::<Vec<_>>();
    for id in remaining {
        if !context.rendered.contains(&id) {
            render_node(output, &id, 1, &mut context)?;
        }
    }
    Ok(context.subgraphs)
}

struct RenderContext<'a> {
    lod: DetailLevel,
    nodes: BTreeMap<NodeId, &'a DiagramNode>,
    children: BTreeMap<NodeId, Vec<NodeId>>,
    node_ids: &'a BTreeMap<NodeId, String>,
    rendered: BTreeSet<NodeId>,
    subgraphs: BTreeSet<NodeId>,
}

fn render_node(
    output: &mut String,
    id: &NodeId,
    depth: usize,
    context: &mut RenderContext<'_>,
) -> Result<()> {
    if !context.rendered.insert(id.clone()) {
        return Ok(());
    }
    let node = context
        .nodes
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("visible hierarchy node is missing: {id}"))?;
    let kind = node.kind;
    let label = node.label.clone();
    let descendants = context.children.get(id).cloned().unwrap_or_default();
    let indent = "    ".repeat(depth);
    let rendered_id = context.node_ids[id].clone();
    if is_contour(kind, context.lod, !descendants.is_empty()) {
        context.subgraphs.insert(id.clone());
        writeln!(
            output,
            "{indent}subgraph {rendered_id}[\"{}\"]",
            escape_label(&label)
        )?;
        writeln!(output, "{indent}    direction LR")?;
        for child in &descendants {
            render_node(output, child, depth + 1, context)?;
        }
        writeln!(output, "{indent}end")?;
    } else {
        writeln!(
            output,
            "{indent}{rendered_id}[\"{}\"]",
            escape_label(&label)
        )?;
        for child in &descendants {
            render_node(output, child, depth, context)?;
        }
    }
    Ok(())
}

fn is_contour(kind: NodeKind, lod: DetailLevel, has_children: bool) -> bool {
    has_children
        && match kind {
            NodeKind::Package => lod >= DetailLevel::Modules,
            NodeKind::Module => lod >= DetailLevel::Abstractions,
            NodeKind::ConcreteType | NodeKind::Trait | NodeKind::ModuleFunctions => {
                lod >= DetailLevel::Methods
            }
            NodeKind::PublicItem
            | NodeKind::OwnershipSite
            | NodeKind::Resource
            | NodeKind::Function
            | NodeKind::PublicFunction
            | NodeKind::Constructor
            | NodeKind::TraitMethod
            | NodeKind::Task
            | NodeKind::Scenario
            | NodeKind::RuntimeEvent
            | NodeKind::UnresolvedCall
            | NodeKind::CycleGroup => false,
        }
}

pub(super) fn node_id(id: &NodeId) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in id.to_string().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("n_{hash:016x}")
}

fn escape_label(label: &str) -> String {
    label
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace(['\n', '\r'], " ")
}

fn escape_link(link: &str) -> String {
    link.replace('\\', "/").replace('"', "%22")
}

fn edge_label(edge: &DiagramEdge) -> String {
    let label = match edge.kind {
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
    };
    if edge.count > 1 {
        format!("{label} x{}", edge.count)
    } else {
        label.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz::{
        graph::{NodeId, NodeKind, SourceLocation},
        view::{DiagramModel, DiagramNode, EvidenceStyle, ViewKind},
    };

    #[test]
    fn stable_ids_and_labels_are_mermaid_safe() {
        let model = DiagramModel {
            kind: ViewKind::Overview,
            lod: DetailLevel::Full,
            nodes: vec![DiagramNode {
                id: NodeId::symbol("demo", "lib", "runtime", "start"),
                kind: NodeKind::Function,
                label: "start(\"resource\")".to_string(),
                location: Some(SourceLocation {
                    path: "src/lib.rs".to_string(),
                    line: 4,
                    column: 0,
                }),
                style: EvidenceStyle::Candidate,
                parent: None,
            }],
            edges: Vec::new(),
            groups: Vec::new(),
            hidden_nodes: 0,
        };

        let first = render(&model).expect("render");
        let second = render(&model).expect("render");
        assert_eq!(first, second);
        assert!(first.contains("flowchart LR"));
        assert!(first.contains(r#"start(&quot;resource&quot;)"#));
        assert!(first.contains("classDef candidate"));
    }
}
