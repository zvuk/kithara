use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;

use super::{
    cli::VizArgs,
    contour,
    filter::{ArchitectureFilter, FilterSummary},
    graph::{Edge, EvidenceGraph, Node},
    mermaid::{DiagramPage, DiagramSet},
    metrics::ArchitectureMetrics,
    report,
    scenario::RuntimeSummary,
    semantic::{SemanticState, SemanticSummary},
    view::DiagramModel,
};

const SCHEMA_VERSION: u32 = 4;

#[derive(Debug)]
pub(crate) struct ArtifactSet {
    pub(crate) document: PathBuf,
}

#[derive(Serialize)]
struct GraphSnapshot<'a> {
    schema_version: u32,
    nodes: Vec<&'a Node>,
    edges: Vec<&'a Edge>,
}

#[derive(Serialize)]
struct ArtifactManifest<'a> {
    schema_version: u32,
    revision: &'a str,
    status: &'static str,
    view: &'static str,
    lod: u8,
    package: Option<&'a str>,
    module: Option<&'a str>,
    visible_nodes: usize,
    visible_edges: usize,
    hidden_nodes: usize,
    collapsed_groups: usize,
    filters: &'a FilterSummary<'a>,
    partition: PartitionManifest<'a>,
    semantic: &'a SemanticSummary,
    runtime: &'a RuntimeSummary,
    files: BTreeMap<&'static str, &'static str>,
}

#[derive(Serialize)]
struct PartitionManifest<'a> {
    state: &'static str,
    covered_nodes: usize,
    pages: Vec<PartitionPage<'a>>,
}

#[derive(Serialize)]
struct PartitionPage<'a> {
    label: &'a str,
    file: &'a str,
    mermaid: &'a str,
    parent: Option<&'a str>,
    visible_nodes: usize,
}

pub(crate) struct ArtifactRequest<'a> {
    pub(crate) root: &'a Path,
    pub(crate) revision: &'a str,
    pub(crate) project: &'a str,
    pub(crate) args: &'a VizArgs,
    pub(crate) graph: &'a EvidenceGraph,
    pub(crate) model: &'a DiagramModel,
    pub(crate) diagrams: &'a DiagramSet,
    pub(crate) metrics: &'a ArchitectureMetrics,
    pub(crate) semantic: &'a SemanticSummary,
    pub(crate) runtime: &'a RuntimeSummary,
    pub(crate) filter: &'a ArchitectureFilter,
    pub(crate) filters: &'a FilterSummary<'a>,
}

pub(crate) fn write(request: &ArtifactRequest<'_>) -> Result<ArtifactSet> {
    let output = request
        .root
        .join("target/architecture")
        .join(request.revision);
    fs::create_dir_all(&output)
        .with_context(|| format!("create architecture output: {}", output.display()))?;
    prepare_page_output(&output)?;

    write_text(&output.join("architecture.mmd"), &request.diagrams.index)?;
    if request.args.krate.is_none() {
        write_text(&output.join("workspace.mmd"), &request.diagrams.index)?;
    }
    for page in &request.diagrams.pages {
        for file in [&page.mermaid_file, &page.document_file] {
            let path = output.join(file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "create architecture contour directory: {}",
                        parent.display()
                    )
                })?;
            }
        }
        write_text(&output.join(&page.mermaid_file), &page.mermaid)?;
        write_text(
            &output.join(&page.document_file),
            &page_document(request, page),
        )?;
    }
    let document = architecture_document(request);
    write_text(&output.join("architecture.md"), &document)?;

    let snapshot = GraphSnapshot {
        schema_version: SCHEMA_VERSION,
        nodes: request.graph.nodes().collect(),
        edges: request.graph.edges().collect(),
    };
    write_json(&output.join("graph.json"), &snapshot)?;
    write_json(
        &output.join("contours.json"),
        &contour::snapshot(request.graph, request.model, request.filter),
    )?;
    write_json(&output.join("metrics.json"), request.metrics)?;
    write_json(&output.join("projection.json"), request.model)?;

    let mut files = BTreeMap::from([
        ("contours", "contours.json"),
        ("document", "architecture.md"),
        ("graph", "graph.json"),
        ("manifest", "manifest.json"),
        ("mermaid", "architecture.mmd"),
        ("metrics", "metrics.json"),
        ("projection", "projection.json"),
    ]);
    if request.args.krate.is_none() {
        files.insert("workspace_mermaid", "workspace.mmd");
    }
    let manifest = ArtifactManifest {
        schema_version: SCHEMA_VERSION,
        revision: request.revision,
        status: overall_status(request.semantic, request.runtime),
        view: request.args.view.as_str(),
        lod: request.args.lod.resolve(request.model.lod.as_u8()),
        package: request.args.krate.as_deref(),
        module: request.args.module.as_deref(),
        visible_nodes: request.model.nodes.len(),
        visible_edges: request.model.edges.len(),
        hidden_nodes: request.model.hidden_nodes,
        collapsed_groups: request.model.groups.len(),
        filters: request.filters,
        partition: PartitionManifest {
            state: request.diagrams.state.as_str(),
            covered_nodes: request.diagrams.covered_nodes,
            pages: request
                .diagrams
                .pages
                .iter()
                .map(|page| PartitionPage {
                    label: &page.label,
                    file: &page.document_file,
                    mermaid: &page.mermaid_file,
                    parent: page.parent.as_deref(),
                    visible_nodes: page.visible_nodes,
                })
                .collect(),
        },
        semantic: request.semantic,
        runtime: request.runtime,
        files,
    };
    write_json(&output.join("manifest.json"), &manifest)?;

    Ok(ArtifactSet {
        document: output.join("architecture.md"),
    })
}

fn prepare_page_output(output: &Path) -> Result<()> {
    for directory in ["contours", "crates"] {
        let path = output.join(directory);
        if path.exists() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("remove stale architecture pages: {}", path.display()))?;
        }
    }
    let workspace = output.join("workspace.mmd");
    if workspace.exists() {
        fs::remove_file(&workspace)
            .with_context(|| format!("remove stale workspace diagram: {}", workspace.display()))?;
    }
    Ok(())
}

fn page_document(request: &ArtifactRequest<'_>, page: &DiagramPage) -> String {
    let root_prefix = "../".repeat(
        Path::new(&page.document_file)
            .parent()
            .map_or(0, |parent| parent.components().count()),
    );
    let mut navigation = format!("[Back to architecture index]({root_prefix}architecture.md)");
    if let Some(parent) = &page.parent {
        navigation.push_str(&format!(" | [Parent contour]({root_prefix}{parent}.md)"));
    }
    let mut children = String::new();
    for child in request
        .diagrams
        .pages
        .iter()
        .filter(|child| child.parent.as_deref() == Some(&page.path))
    {
        if children.is_empty() {
            children.push_str("## Child contours\n\n");
        }
        children.push_str(&format!(
            "- [{}]({root_prefix}{}) - {} visible nodes\n",
            child.label, child.document_file, child.visible_nodes
        ));
    }
    if !children.is_empty() {
        children.push('\n');
    }
    let metrics = request
        .metrics
        .contours
        .get(&page.path)
        .map_or_else(String::new, |metrics| report::render_metrics(metrics));
    let analysis = report::render(&page.model);
    format!(
        "# {}\n\n{}\n\n```mermaid\n{}```\n\n{}{}{}",
        page.label, navigation, page.mermaid, metrics, analysis, children
    )
}

fn architecture_document(request: &ArtifactRequest<'_>) -> String {
    let analysis = report::render(request.model);
    let metrics = report::render_metrics(request.metrics);
    let evidence = evidence_summary(request);
    format!(
        "# {} Architecture\n\n\
         Status: **{}**\n\n\
         ## Architecture\n\n\
         ```mermaid\n\
         {}```\n\n\
         Visible nodes: {}. Visible edges: {}. Outside the selected projection: {}. \
         Collapsed cycles: {}. Semantic edges resolved: {}.\n\n\
         {}\
         {}\
         {}\
         ## Limitations\n\n\
         - Call targets without semantic evidence remain syntax-derived candidates.\n\
         - Unknown dynamic calls remain recorded in `graph.json`; the overview omits them instead of guessing a target.\n\
         - Runtime evidence proves only the configured observation; absence from a trace does not prove a path is dead.\n",
        request.project,
        overall_status(request.semantic, request.runtime),
        request.diagrams.index,
        request.model.nodes.len(),
        request.model.edges.len(),
        request.model.hidden_nodes,
        request.model.groups.len(),
        request.semantic.resolved_edges,
        metrics,
        analysis,
        evidence,
    ) + &contour_links(request.diagrams)
}

fn contour_links(diagrams: &DiagramSet) -> String {
    if diagrams.pages.is_empty() {
        return String::new();
    }
    let mut output = String::from("\n## Contour diagrams\n\n");
    for page in diagrams.pages.iter().filter(|page| page.parent.is_none()) {
        output.push_str(&format!(
            "- [{}]({}) - {} visible nodes\n",
            page.label, page.document_file, page.visible_nodes
        ));
    }
    output
}

fn evidence_summary(request: &ArtifactRequest<'_>) -> String {
    let mut output = format!(
        "## Evidence status\n\n- Semantic: `{}`; requested {}, prepared {}, resolved {} edges, skipped {}.\n",
        request.semantic.state.as_str(),
        request.semantic.requested_symbols,
        request.semantic.prepared_symbols,
        request.semantic.resolved_edges,
        request.semantic.skipped_symbols,
    );
    if request.runtime.scenarios.is_empty() {
        output.push_str("- Runtime: no configured or selected scenario evidence.\n\n");
    } else {
        for scenario in &request.runtime.scenarios {
            output.push_str(&format!(
                "- Runtime `{}`: `{}`; {} records, {} matched, {} unmatched.\n",
                scenario.name,
                scenario.state.as_str(),
                scenario.trace.records,
                scenario.trace.matched_records,
                scenario.trace.unmatched_records,
            ));
        }
        output.push('\n');
    }
    output
}

fn overall_status(semantic: &SemanticSummary, runtime: &RuntimeSummary) -> &'static str {
    if semantic.is_incomplete() || runtime.is_incomplete() {
        return "incomplete";
    }
    if semantic.state == SemanticState::Truncated || runtime.is_truncated() {
        return "truncated";
    }
    if runtime.has_runtime() && semantic.state == SemanticState::Unavailable {
        return "runtime-enriched";
    }
    match semantic.state {
        SemanticState::Complete => "complete",
        SemanticState::Truncated => "truncated",
        SemanticState::Unavailable => "static-only",
        SemanticState::TimedOut | SemanticState::Failed => "incomplete",
    }
}

fn write_text(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content).with_context(|| format!("write {}", path.display()))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}
