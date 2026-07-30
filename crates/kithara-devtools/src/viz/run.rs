use std::{
    io::{self, Write as _},
    process::Command,
};

use anyhow::{Result, bail};

use super::{
    cli::{Lod, RuntimeMode, SemanticMode, ViewName, VizArgs},
    filter::ArchitectureFilter,
    hierarchy,
    manifest::{self, ArtifactRequest},
    mermaid,
    metrics::MetricsAnalyzer,
    scenario::{self, RunRequest},
    semantic, source,
    view::{DetailLevel, Projector, ViewKind, ViewRequest},
};
use crate::{Ctx, common::project::ArchitectureFilterConfig};

pub(crate) fn run(args: &VizArgs, ctx: &Ctx) -> Result<()> {
    let metadata = ctx.metadata()?;
    if let Some(package) = args.krate.as_deref()
        && !metadata
            .workspace_packages()
            .iter()
            .any(|candidate| candidate.name == package)
    {
        bail!("workspace package not found: {package}");
    }
    let complete_filters = ArchitectureFilterConfig::default();
    let configured_filters = if args.include_default_excluded {
        &complete_filters
    } else {
        &ctx.config.architecture.filters
    };
    let filter = ArchitectureFilter::new(
        configured_filters,
        &args.exclude_crates,
        &args.exclude_modules,
        metadata,
    )?;
    if let Some(package) = args.krate.as_deref()
        && filter.excludes_package(package)
    {
        bail!("selected workspace package is excluded: {package}");
    }
    let mut graph = source::collect(ctx)?;
    if graph.nodes().all(|node| filter.excludes(&node.id)) {
        bail!("architecture filters removed every node from the selected projection");
    }
    if let Some(module) = args.module.as_deref()
        && filter.scope_is_fully_excluded(&graph, args.krate.as_deref(), module)
    {
        bail!("selected module scope is excluded: {module}");
    }
    let revision = revision(&ctx.root);
    let output = ctx.root.join("target/architecture").join(&revision);
    let runtime = scenario::run(
        &RunRequest {
            config: &ctx.config.architecture,
            metadata,
            root: &ctx.root,
            output: &output,
            selected: args.scenario.as_deref(),
            manual_trace: args.trace.as_deref(),
            run_configured: args.runtime == RuntimeMode::Auto,
        },
        &mut graph,
    )?;
    let semantic = if args.semantic == SemanticMode::Off {
        semantic::SemanticSummary::static_only("semantic resolution disabled")
    } else {
        semantic::enrich(
            &mut graph,
            &ctx.root,
            args.krate.as_deref(),
            args.module.as_deref(),
            args.scenario.as_deref(),
            &filter,
        )
    };
    let request = ViewRequest {
        kind: view_kind(args.view),
        package: args.krate.clone(),
        module: args.module.clone(),
        scenario: args
            .scenario
            .clone()
            .or_else(|| args.trace.as_ref().map(|_| "manual".to_string())),
        lod: detail_level(args),
        filter: filter.clone(),
    };
    let projector = Projector::new(&graph);
    let model = projector.project(&request);
    if model.nodes.is_empty() {
        bail!("architecture filters removed every node from the selected projection");
    }
    let details = hierarchy::plan(&projector, &request, &model);
    let analyzer = MetricsAnalyzer::new(&graph, &filter, projector.contours());
    let mut metrics = analyzer.analyze(&model, args.krate.as_deref(), args.module.as_deref());
    for detail in &details {
        metrics.insert_contour(
            detail.path.clone(),
            analyzer.analyze(
                &detail.model,
                Some(&detail.package),
                detail.module.as_deref(),
            ),
        );
    }
    let diagrams = mermaid::render_set(&model, details)?;
    let filter_summary = filter.summary(&graph);
    let artifacts = manifest::write(&ArtifactRequest {
        root: &ctx.root,
        revision: &revision,
        project: &ctx.config.project.name,
        args,
        graph: &graph,
        model: &model,
        diagrams: &diagrams,
        metrics: &metrics,
        semantic: &semantic,
        runtime: &runtime,
        filter: &filter,
        filters: &filter_summary,
    })?;
    writeln!(io::stdout().lock(), "==> {}", artifacts.document.display())?;
    if semantic.is_incomplete()
        || runtime.is_incomplete()
        || args.semantic == SemanticMode::Required
            && semantic.state == semantic::SemanticState::Unavailable
    {
        bail!("architecture analysis is incomplete; partial artifacts were preserved");
    }
    Ok(())
}

fn detail_level(args: &VizArgs) -> DetailLevel {
    match args.lod {
        Lod::Auto if args.module.is_some() => DetailLevel::Abstractions,
        Lod::Auto if args.krate.is_some() => DetailLevel::Modules,
        Lod::Level1 if args.krate.is_none() => DetailLevel::Crates,
        Lod::Auto | Lod::Level0 => DetailLevel::Crates,
        Lod::Level1 => DetailLevel::Modules,
        Lod::Level2 => DetailLevel::Abstractions,
        Lod::Level3 => DetailLevel::Methods,
        Lod::Level4 => DetailLevel::Full,
    }
}

fn view_kind(view: ViewName) -> ViewKind {
    match view {
        ViewName::Overview => ViewKind::Overview,
        ViewName::Hierarchy => ViewKind::Hierarchy,
        ViewName::Ownership => ViewKind::Ownership,
    }
}

fn revision(root: &std::path::Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--short=12", "HEAD"])
        .output();
    output
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|revision| revision.trim().to_string())
        .filter(|revision| !revision.is_empty())
        .unwrap_or_else(|| "working-tree".to_string())
}
