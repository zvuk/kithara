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
use crate::common::project::ArchitectureRenderBudgets;

const SCHEMA_VERSION: u32 = 5;

#[derive(Debug)]
pub(crate) struct ArtifactSet {
    pub(crate) document: PathBuf,
}

#[derive(Serialize)]
struct GraphSnapshot<'a> {
    edges: Vec<&'a Edge>,
    nodes: Vec<&'a Node>,
    schema_version: u32,
}

#[derive(Serialize)]
struct ArtifactManifest<'a> {
    filters: &'a FilterSummary<'a>,
    runtime: &'a RuntimeSummary,
    semantic: &'a SemanticSummary,
    revision: &'a str,
    status: &'static str,
    view: &'static str,
    files: BTreeMap<&'static str, &'static str>,
    module: Option<&'a str>,
    package: Option<&'a str>,
    partition: PartitionManifest<'a>,
    schema_version: u32,
    lod: u8,
    collapsed_groups: usize,
    hidden_nodes: usize,
    visible_edges: usize,
    visible_nodes: usize,
}

#[derive(Serialize)]
struct PartitionManifest<'a> {
    state: &'static str,
    pages: Vec<PartitionPage<'a>>,
    covered_nodes: usize,
}

#[derive(Serialize)]
struct PartitionPage<'a> {
    file: &'a str,
    label: &'a str,
    mermaid: &'a str,
    parent: Option<&'a str>,
    visible_nodes: usize,
}

pub(crate) struct ArtifactRequest<'a> {
    pub(crate) filter: &'a ArchitectureFilter,
    pub(crate) metrics: &'a ArchitectureMetrics,
    pub(crate) budgets: &'a ArchitectureRenderBudgets,
    pub(crate) model: &'a DiagramModel,
    pub(crate) diagrams: &'a DiagramSet,
    pub(crate) graph: &'a EvidenceGraph,
    pub(crate) filters: &'a FilterSummary<'a>,
    pub(crate) root: &'a Path,
    pub(crate) runtime: &'a RuntimeSummary,
    pub(crate) semantic: &'a SemanticSummary,
    pub(crate) args: &'a VizArgs,
    pub(crate) project: &'a str,
    pub(crate) revision: &'a str,
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
        files,
        schema_version: SCHEMA_VERSION,
        revision: request.revision,
        status: overall_status(request.semantic, request.runtime),
        view: request.args.view.as_str(),
        lod: request.args.lod.resolve(u8::from(request.model.lod)),
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
    let analysis = report::render(&page.model, request.budgets);
    format!(
        "# {}\n\n{}\n\n```mermaid\n{}```\n\n{}{}{}",
        page.label, navigation, page.mermaid, metrics, analysis, children
    )
}

fn architecture_document(request: &ArtifactRequest<'_>) -> String {
    let analysis = report::render(request.model, request.budgets);
    let metrics = report::render_metrics(request.metrics);
    let evidence = evidence_summary(request.semantic, request.runtime);
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

fn evidence_summary(semantic: &SemanticSummary, runtime: &RuntimeSummary) -> String {
    let mut output = format!(
        "## Evidence status\n\n- Semantic: `{}`; requested {}, prepared {}, resolved {} edges, skipped {}.\n",
        semantic.state.as_str(),
        semantic.requested_symbols,
        semantic.prepared_symbols,
        semantic.resolved_edges,
        semantic.skipped_symbols,
    );
    for diagnostic in &semantic.diagnostics {
        output.push_str(&format!("  - Semantic diagnostic: {diagnostic}\n"));
    }
    if runtime.scenarios.is_empty() {
        output.push_str("- Runtime: no configured or selected scenario evidence.\n\n");
    } else {
        for scenario in &runtime.scenarios {
            output.push_str(&format!(
                "- Runtime `{}`: `{}`; {} records, {} matched, {} unmatched.\n",
                scenario.name,
                scenario.state.as_str(),
                scenario.trace.records,
                scenario.trace.matched_records,
                scenario.trace.unmatched_records,
            ));
            for diagnostic in &scenario.trace.diagnostics {
                output.push_str(&format!(
                    "  - Runtime `{}` diagnostic: {diagnostic}\n",
                    scenario.name
                ));
            }
            if let Some(exit_code) = scenario.exit_code {
                output.push_str(&format!(
                    "  - Runtime `{}` exit code: {exit_code}\n",
                    scenario.name
                ));
            }
            if let Some(stderr) = &scenario.stderr {
                output.push_str(&format!(
                    "  - Runtime `{}` stderr log: `{stderr}`\n",
                    scenario.name
                ));
            }
        }
        output.push('\n');
    }
    output
}

fn overall_status(semantic: &SemanticSummary, runtime: &RuntimeSummary) -> &'static str {
    if runtime.is_degraded() {
        return "runtime-degraded";
    }
    if semantic.state == SemanticState::Truncated || runtime.is_truncated() {
        return "truncated";
    }
    if runtime.has_runtime()
        && matches!(
            semantic.state,
            SemanticState::Unavailable | SemanticState::TimedOut | SemanticState::Failed
        )
    {
        return "runtime-enriched";
    }
    match semantic.state {
        SemanticState::Complete => "complete",
        SemanticState::Truncated => "truncated",
        SemanticState::Unavailable | SemanticState::TimedOut | SemanticState::Failed => {
            "static-only"
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz::{
        scenario::{ScenarioState, ScenarioSummary},
        trace::{TraceState, TraceSummary},
    };

    #[test]
    fn missing_semantic_evidence_has_the_same_status_for_every_cause() {
        let absent = semantic(SemanticState::Unavailable);
        let timed_out = semantic(SemanticState::TimedOut);
        let failed = semantic(SemanticState::Failed);

        assert_eq!(
            overall_status(&absent, &RuntimeSummary::default()),
            "static-only"
        );
        assert_eq!(
            overall_status(&timed_out, &RuntimeSummary::default()),
            "static-only"
        );
        assert_eq!(
            overall_status(&failed, &RuntimeSummary::default()),
            "static-only"
        );
        assert_eq!(
            overall_status(&absent, &runtime(ScenarioState::Complete)),
            "runtime-enriched"
        );
        assert_eq!(
            overall_status(&timed_out, &runtime(ScenarioState::Complete)),
            "runtime-enriched"
        );
        assert_eq!(
            overall_status(&failed, &runtime(ScenarioState::Complete)),
            "runtime-enriched"
        );
    }

    #[test]
    fn degraded_runtime_evidence_has_its_own_status() {
        assert_eq!(
            overall_status(
                &semantic(SemanticState::Complete),
                &runtime(ScenarioState::TimedOut),
            ),
            "runtime-degraded"
        );
    }

    #[test]
    fn manual_trace_content_controls_the_runtime_status() {
        let mut empty = runtime(ScenarioState::Manual);
        empty.scenarios[0].trace.state = TraceState::Empty;
        let mut truncated = runtime(ScenarioState::Manual);
        truncated.scenarios[0].trace.state = TraceState::Truncated;

        assert_eq!(
            overall_status(&semantic(SemanticState::Complete), &empty),
            "runtime-degraded"
        );
        assert_eq!(
            overall_status(&semantic(SemanticState::Complete), &truncated),
            "truncated"
        );
    }

    #[test]
    fn evidence_summary_explains_why_each_overlay_degraded() {
        let summary = evidence_summary(
            &semantic(SemanticState::TimedOut),
            &runtime(ScenarioState::TimedOut),
        );

        assert_eq!(
            summary,
            concat!(
                "## Evidence status\n\n",
                "- Semantic: `timed_out`; requested 0, prepared 0, resolved 0 edges, skipped 0.\n",
                "  - Semantic diagnostic: semantic diagnostic\n",
                "- Runtime `scenario`: `timed_out`; 0 records, 0 matched, 0 unmatched.\n",
                "  - Runtime `scenario` diagnostic: runtime diagnostic\n\n",
            )
        );
    }

    #[test]
    fn evidence_summary_preserves_failed_process_details() {
        let mut semantic = semantic(SemanticState::Complete);
        semantic.diagnostics.clear();
        let mut runtime = runtime(ScenarioState::Failed);
        runtime.scenarios[0].exit_code = Some(7);
        runtime.scenarios[0].stderr = Some("logs/scenario.stderr.log".to_string());
        runtime.scenarios[0].trace.state = TraceState::Complete;
        runtime.scenarios[0].trace.diagnostics.clear();

        let summary = evidence_summary(&semantic, &runtime);

        assert_eq!(
            summary,
            concat!(
                "## Evidence status\n\n",
                "- Semantic: `complete`; requested 0, prepared 0, resolved 0 edges, skipped 0.\n",
                "- Runtime `scenario`: `failed`; 0 records, 0 matched, 0 unmatched.\n",
                "  - Runtime `scenario` exit code: 7\n",
                "  - Runtime `scenario` stderr log: `logs/scenario.stderr.log`\n\n",
            )
        );
    }

    #[test]
    fn no_evidence_combination_has_an_incomplete_status() {
        let semantic_states = [
            SemanticState::Complete,
            SemanticState::Truncated,
            SemanticState::Unavailable,
            SemanticState::TimedOut,
            SemanticState::Failed,
        ];
        let scenario_states = [
            ScenarioState::Complete,
            ScenarioState::Empty,
            ScenarioState::Truncated,
            ScenarioState::Failed,
            ScenarioState::TimedOut,
            ScenarioState::Manual,
        ];

        for semantic_state in semantic_states {
            assert_ne!(
                overall_status(&semantic(semantic_state), &RuntimeSummary::default()),
                "incomplete"
            );
            for scenario_state in scenario_states {
                assert_ne!(
                    overall_status(&semantic(semantic_state), &runtime(scenario_state)),
                    "incomplete"
                );
            }
        }
    }

    fn semantic(state: SemanticState) -> SemanticSummary {
        SemanticSummary {
            state,
            diagnostics: vec!["semantic diagnostic".to_string()],
            outgoing_calls: 0,
            prepared_symbols: 0,
            requested_symbols: 0,
            resolved_edges: 0,
            skipped_symbols: 0,
            unmatched_targets: 0,
        }
    }

    fn runtime(state: ScenarioState) -> RuntimeSummary {
        let trace_state = match state {
            ScenarioState::Complete | ScenarioState::Manual => TraceState::Complete,
            ScenarioState::Empty | ScenarioState::TimedOut => TraceState::Empty,
            ScenarioState::Truncated => TraceState::Truncated,
            ScenarioState::Failed => TraceState::Failed,
        };
        RuntimeSummary {
            scenarios: vec![ScenarioSummary {
                exit_code: None,
                stderr: None,
                stdout: None,
                state,
                name: "scenario".to_string(),
                trace: TraceSummary {
                    state: trace_state,
                    diagnostics: vec!["runtime diagnostic".to_string()],
                    matched_records: 0,
                    records: 0,
                    unmatched_records: 0,
                },
            }],
        }
    }
}
