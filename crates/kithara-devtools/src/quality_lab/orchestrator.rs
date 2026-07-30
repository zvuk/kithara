use std::{
    fs,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};

use super::{
    adapter::AdapterOptions,
    cli::RunArgs,
    config::QualityLabConfig,
    execution::{ToolRunContext, error_manifest, finish_manifest, run_tool},
    manifest::{Profile, Status, Tool},
    report, workspace,
};
use crate::Ctx;

pub(super) fn run(args: &RunArgs, config: &QualityLabConfig, ctx: &Ctx) -> Result<()> {
    let (profile, tools) = selection(args, config)?;
    let revision = workspace::git_text(&ctx.root, &["rev-parse", "HEAD"], "resolve Git revision")?;
    let revision_dir = ctx.root.join(&config.output_dir).join(&revision);
    fs::create_dir_all(&revision_dir).with_context(|| {
        format!(
            "create Quality Lab revision output: {}",
            revision_dir.display()
        )
    })?;

    let profile_started = Instant::now();
    let profile_budget = Duration::from_secs(config.profiles.get(profile).timeout_secs);
    let options = AdapterOptions {
        lcov: args.lcov.as_deref(),
        baseline: args.baseline.as_deref(),
    };
    let run_context = ToolRunContext {
        workspace_root: &ctx.root,
        revision_dir: &revision_dir,
        revision: &revision,
        profile,
        config,
        options: &options,
    };
    let mut manifests = Vec::with_capacity(tools.len());
    for tool in tools {
        let remaining = profile_budget.saturating_sub(profile_started.elapsed());
        if remaining.is_zero() {
            let tool_dir = revision_dir.join(tool.to_string());
            workspace::reset_directory(&tool_dir)?;
            manifests.push(finish_manifest(
                &tool_dir,
                error_manifest(
                    &revision,
                    profile,
                    tool,
                    &config.tools.get(tool).version,
                    "profile time budget exhausted before tool started",
                ),
            )?);
            continue;
        }
        manifests.push(run_tool(&run_context, tool, remaining)?);
    }

    let paths = report::write(&revision_dir, &revision, profile, &manifests)?;
    for manifest in &manifests {
        println!("{}: {}", manifest.tool, manifest.status);
    }
    println!("Quality Lab JSON: {}", paths.json.display());
    println!("Quality Lab report: {}", paths.markdown.display());

    if should_fail(profile, manifests.iter().map(|manifest| manifest.status)) {
        bail!(
            "Quality Lab {profile} profile failed; see {}",
            paths.markdown.display()
        );
    }
    Ok(())
}

fn selection(args: &RunArgs, config: &QualityLabConfig) -> Result<(Profile, Vec<Tool>)> {
    if let Some(profile) = args.target.profile() {
        return Ok((profile, config.profiles.get(profile).tools.clone()));
    }
    let tool = args
        .target
        .tool()
        .ok_or_else(|| anyhow!("Quality Lab target has neither a profile nor a tool"))?;
    let profile = if tool == Tool::CargoCrap {
        Profile::Coverage
    } else {
        Profile::Manual
    };
    Ok((profile, vec![tool]))
}

fn should_fail(profile: Profile, statuses: impl IntoIterator<Item = Status>) -> bool {
    statuses.into_iter().any(|status| {
        status == Status::ToolError || (profile == Profile::Coverage && status == Status::Findings)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_findings_gate_but_scheduled_findings_are_advisory() {
        assert!(should_fail(Profile::Coverage, [Status::Findings]));
        assert!(!should_fail(Profile::Scheduled, [Status::Findings]));
        assert!(!should_fail(Profile::Manual, [Status::Skipped]));
        assert!(should_fail(Profile::Manual, [Status::ToolError]));
    }
}
