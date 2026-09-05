use anyhow::Result;
use cargo_metadata::MetadataCommand;
use clap::Args;

use crate::common::{
    project::ProjectConfig,
    scope::{Scope, Tool},
};

#[derive(Debug, Args)]
pub struct ScopeArgs {
    /// Downstream tool whose flag dialect should be emitted.
    #[arg(long = "for", value_enum)]
    pub for_tool: Tool,
    /// Zero or more scope tokens. Empty = whole workspace.
    pub tokens: Vec<String>,
}

pub(crate) fn run(args: &ScopeArgs) -> Result<()> {
    let metadata = build_metadata()?;
    let workspace_root = metadata.workspace_root.as_std_path();
    let project = ProjectConfig::load(workspace_root)?;
    let scope = Scope::resolve(
        &args.tokens,
        workspace_root,
        &project.workspace_scan.top_level_dirs,
    )?;
    let flags = scope.flags_for(args.for_tool);
    println!("{}", flags.join(" "));
    Ok(())
}

fn build_metadata() -> Result<cargo_metadata::Metadata> {
    let metadata = MetadataCommand::new().no_deps().exec()?;
    Ok(metadata)
}
