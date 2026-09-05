use std::{
    io::{self, Write as _},
    process::Command,
    time::Duration,
};

use anyhow::{Result, bail};

use super::{
    cli::{Lod, RuntimeMode, SemanticMode, ViewName, VizArgs},
    filter::ArchitectureFilter,
    hierarchy,
    manifest::{self, ArtifactRequest},
    mermaid,
    metrics::MetricsAnalyzer,
    scenario::{self, RunRequest, RuntimeSummary, ScenarioSummary},
    semantic::{self, EnrichRequest, SemanticState, SemanticSummary},
    source,
    trace::TraceState,
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
            metadata,
            config: &ctx.config.architecture,
            root: &ctx.root,
            output: &output,
            selected: args.scenario.as_deref(),
            manual_trace: args.trace.as_deref(),
            run_configured: args.runtime == RuntimeMode::Auto,
        },
        &mut graph,
    )?;
    let semantic = if args.semantic == SemanticMode::Off {
        SemanticSummary::static_only("semantic resolution disabled")
    } else {
        semantic::enrich(
            &mut graph,
            &EnrichRequest {
                filter: &filter,
                root: &ctx.root,
                program: ctx.config.tools.program("rust-analyzer"),
                timeout: Duration::from_secs(ctx.config.architecture.runtime.semantic_timeout_secs),
                module: args.module.as_deref(),
                package: args.krate.as_deref(),
                scenario: args.scenario.as_deref(),
            },
        )
    };
    let request = ViewRequest {
        kind: view_kind(args.view),
        package: args.krate.clone(),
        module: args.module.clone(),
        scenario: projection_scenario(args.scenario.as_deref(), args.trace.is_some(), &runtime),
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
        args,
        budgets: &ctx.config.architecture.render,
        root: &ctx.root,
        revision: &revision,
        project: &ctx.config.project.name,
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
    let warnings = degradation_warnings(args.semantic, &semantic, &runtime);
    if !warnings.is_empty() {
        let mut stderr = io::stderr().lock();
        for warning in warnings {
            writeln!(stderr, "warning: {warning}")?;
        }
    }
    if requested_evidence_degraded(
        args.semantic,
        semantic.state,
        args.scenario.as_deref(),
        args.trace.is_some(),
        &runtime,
    ) {
        bail!("explicitly requested architecture evidence degraded; artifacts were preserved");
    }
    Ok(())
}

fn degradation_warnings(
    semantic_mode: SemanticMode,
    semantic: &SemanticSummary,
    runtime: &RuntimeSummary,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if semantic_mode != SemanticMode::Off
        && matches!(
            semantic.state,
            SemanticState::Unavailable | SemanticState::TimedOut | SemanticState::Failed
        )
    {
        let diagnostics = semantic.diagnostics.join("; ");
        let warning = format!("semantic overlay degraded (`{}`)", semantic.state.as_str());
        warnings.push(if diagnostics.is_empty() {
            warning
        } else {
            format!("{warning}: {diagnostics}")
        });
    }
    for scenario in runtime
        .scenarios
        .iter()
        .filter(|scenario| scenario.is_degraded())
    {
        let mut diagnostics = scenario.trace.diagnostics.clone();
        if let Some(exit_code) = scenario.exit_code {
            diagnostics.push(format!("exit code {exit_code}"));
        }
        if let Some(stderr) = &scenario.stderr {
            diagnostics.push(format!("stderr log: {stderr}"));
        }
        let warning = format!(
            "runtime overlay `{}` degraded (scenario `{}`, trace `{}`)",
            scenario.name,
            scenario.state.as_str(),
            trace_state_name(scenario.trace.state),
        );
        warnings.push(if diagnostics.is_empty() {
            warning
        } else {
            format!("{warning}: {}", diagnostics.join("; "))
        });
    }
    warnings
}

fn requested_evidence_degraded(
    semantic_mode: SemanticMode,
    semantic_state: SemanticState,
    selected_scenario: Option<&str>,
    manual_trace_selected: bool,
    runtime: &RuntimeSummary,
) -> bool {
    let semantic_degraded = semantic_mode == SemanticMode::Required
        && matches!(
            semantic_state,
            SemanticState::Unavailable | SemanticState::TimedOut | SemanticState::Failed
        );
    let selected_scenario_degraded = selected_scenario.is_some_and(|selected| {
        runtime
            .scenarios
            .iter()
            .any(|scenario| scenario.name == selected && scenario.is_degraded())
    });
    let manual_trace_degraded = manual_trace_selected
        && runtime
            .scenarios
            .last()
            .is_some_and(ScenarioSummary::is_degraded);
    semantic_degraded || selected_scenario_degraded || manual_trace_degraded
}

fn projection_scenario(
    selected_scenario: Option<&str>,
    manual_trace_selected: bool,
    runtime: &RuntimeSummary,
) -> Option<String> {
    if let Some(selected) = selected_scenario {
        return runtime
            .scenarios
            .iter()
            .find(|scenario| scenario.name == selected)
            .filter(|scenario| !scenario.is_degraded())
            .map(|_| selected.to_string());
    }
    if manual_trace_selected {
        return runtime
            .scenarios
            .last()
            .filter(|scenario| !scenario.is_degraded())
            .map(|_| "manual".to_string());
    }
    None
}

fn trace_state_name(state: TraceState) -> &'static str {
    match state {
        TraceState::Complete => "complete",
        TraceState::Empty => "empty",
        TraceState::Truncated => "truncated",
        TraceState::Failed => "failed",
    }
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

const fn view_kind(view: ViewName) -> ViewKind {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz::{scenario::ScenarioState, trace::TraceSummary};

    #[test]
    fn warnings_name_every_degraded_overlay_and_its_diagnostic() {
        let runtime = runtime(vec![scenario(
            "queue-playback",
            ScenarioState::TimedOut,
            TraceState::Empty,
        )]);

        let warnings = degradation_warnings(
            SemanticMode::Auto,
            &semantic(
                SemanticState::TimedOut,
                "rust-analyzer timed out during workspace loading",
            ),
            &runtime,
        );

        assert_eq!(
            warnings,
            [
                "semantic overlay degraded (`timed_out`): rust-analyzer timed out during workspace loading",
                "runtime overlay `queue-playback` degraded (scenario `timed_out`, trace `empty`): diagnostic",
            ]
        );
    }

    #[test]
    fn semantic_off_does_not_warn_about_missing_semantic_evidence() {
        let warnings = degradation_warnings(
            SemanticMode::Off,
            &semantic(SemanticState::Unavailable, "semantic resolution disabled"),
            &RuntimeSummary::default(),
        );

        assert!(warnings.is_empty());
    }

    #[test]
    fn required_semantic_mode_fails_for_every_missing_state() {
        for state in [
            SemanticState::Unavailable,
            SemanticState::TimedOut,
            SemanticState::Failed,
        ] {
            assert!(requested_evidence_degraded(
                SemanticMode::Required,
                state,
                None,
                false,
                &RuntimeSummary::default(),
            ));
            assert!(!requested_evidence_degraded(
                SemanticMode::Auto,
                state,
                None,
                false,
                &RuntimeSummary::default(),
            ));
        }
    }

    #[test]
    fn only_an_explicitly_selected_degraded_scenario_is_fatal() {
        let runtime = runtime(vec![scenario(
            "queue-playback",
            ScenarioState::TimedOut,
            TraceState::Empty,
        )]);

        assert!(requested_evidence_degraded(
            SemanticMode::Auto,
            SemanticState::Complete,
            Some("queue-playback"),
            false,
            &runtime,
        ));
        assert!(!requested_evidence_degraded(
            SemanticMode::Auto,
            SemanticState::Complete,
            None,
            false,
            &runtime,
        ));
        assert_eq!(
            projection_scenario(Some("queue-playback"), false, &runtime),
            None
        );
    }

    #[test]
    fn an_explicitly_supplied_empty_trace_is_fatal() {
        let runtime = runtime(vec![scenario(
            "manual",
            ScenarioState::Manual,
            TraceState::Empty,
        )]);

        assert!(requested_evidence_degraded(
            SemanticMode::Auto,
            SemanticState::Complete,
            None,
            true,
            &runtime,
        ));
        assert!(!requested_evidence_degraded(
            SemanticMode::Auto,
            SemanticState::Complete,
            None,
            false,
            &runtime,
        ));
        assert_eq!(projection_scenario(None, true, &runtime), None);
    }

    #[test]
    fn a_configured_manual_scenario_does_not_alias_the_explicit_trace() {
        let runtime = runtime(vec![
            scenario("manual", ScenarioState::Failed, TraceState::Empty),
            scenario("manual", ScenarioState::Manual, TraceState::Complete),
        ]);

        assert!(!requested_evidence_degraded(
            SemanticMode::Auto,
            SemanticState::Complete,
            None,
            true,
            &runtime,
        ));
        assert_eq!(
            projection_scenario(None, true, &runtime).as_deref(),
            Some("manual")
        );
    }

    fn runtime(scenarios: Vec<ScenarioSummary>) -> RuntimeSummary {
        RuntimeSummary { scenarios }
    }

    fn semantic(state: SemanticState, diagnostic: &str) -> SemanticSummary {
        SemanticSummary {
            state,
            diagnostics: vec![diagnostic.to_string()],
            outgoing_calls: 0,
            prepared_symbols: 0,
            requested_symbols: 0,
            resolved_edges: 0,
            skipped_symbols: 0,
            unmatched_targets: 0,
        }
    }

    fn scenario(name: &str, state: ScenarioState, trace_state: TraceState) -> ScenarioSummary {
        ScenarioSummary {
            state,
            exit_code: None,
            stderr: None,
            stdout: None,
            name: name.to_string(),
            trace: TraceSummary {
                state: trace_state,
                diagnostics: vec!["diagnostic".to_string()],
                matched_records: 0,
                records: 0,
                unmatched_records: 0,
            },
        }
    }
}
