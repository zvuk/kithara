use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use clap::Args;
use serde::Serialize;

use crate::{Ctx, common::baseline::Baseline};

struct Consts;
impl Consts {
    const CONFIG_REL: &'static str = ".config/xtask.toml";
    const BASELINE_CONFIG_DIRS: &'static [&'static str] =
        &[".config/arch", ".config/style", ".config/idioms"];
    const COMMENTED_CONFIG_TEMPLATE: &'static str = r#"
# Optional generic tooling config sections. Uncomment only the settings this workspace owns.
#
# [health]
# feature_powerset_exclude = []
# workspace_exclude = []
#
# [test]
# default_lane = ""
# default_backend = ""
# feature_arg = ""
#
# [test.flash]
# features = []
# default = true
#
# [test.lanes.default]
# program = ""
# prefix_args = []
# suffix_args = []
# default_features = []
# default_flash = true
# passthrough = ""
#
# [test.net_backends.default]
# features = []
#
# [perf]
# primary_lane = ""
# nextest_profile = "perf"
# frame_prefix = ""
#
# [[perf.lanes]]
# flash = true
# backend = ""
#
# [orphans]
# exclude_packages = []
#
# [quality]
# unimock_traits_dir = ""
#
# [lint_exclude]
# paths = []
# modules = []
# scan_all_rules = []
#
# [workspace-scan]
# exclude = []
"#;

    const MAIN_RS_SNIPPET: &'static str = r#"use clap::{Parser, Subcommand};
use kithara_devtools::{CoreCommand, Ctx};

#[derive(Debug, Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(flatten)]
    Core(CoreCommand),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let ctx = Ctx::load()?;
    match cli.command {
        Command::Core(cmd) => kithara_devtools::run(&cmd, &ctx),
    }
}
"#;
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Overwrite existing files instead of failing.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug)]
struct InitConflict {
    paths: Vec<PathBuf>,
}

impl fmt::Display for InitConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = if self.paths.len() == 1 {
            "init target already exists"
        } else {
            "init targets already exist"
        };
        write!(f, "{label}:")?;
        for path in &self.paths {
            write!(f, "\n  {}", path.display())?;
        }
        Ok(())
    }
}

impl Error for InitConflict {}

struct TargetFiles {
    config: PathBuf,
    baselines: Vec<PathBuf>,
}

#[derive(Serialize)]
struct ConfigTemplate<'a> {
    project: ProjectTemplate<'a>,
}

#[derive(Serialize)]
struct ProjectTemplate<'a> {
    name: &'a str,
}

pub(crate) fn run(args: &InitArgs, ctx: &Ctx) -> Result<()> {
    let targets = target_files(&ctx.root);
    let conflicts = existing_targets(&targets);
    if !args.force && !conflicts.is_empty() {
        return Err(InitConflict { paths: conflicts }.into());
    }

    let config = render_config(&ctx.config.project.name)?;
    write_file(&targets.config, config.as_bytes())?;
    for baseline in &targets.baselines {
        write_file(baseline, b"")?;
    }

    println!("{}", Consts::MAIN_RS_SNIPPET);
    Ok(())
}

fn target_files(root: &Path) -> TargetFiles {
    let baselines = Consts::BASELINE_CONFIG_DIRS
        .iter()
        .map(|dir| Baseline::path(&root.join(dir)))
        .collect();
    TargetFiles {
        config: root.join(Consts::CONFIG_REL),
        baselines,
    }
}

fn existing_targets(targets: &TargetFiles) -> Vec<PathBuf> {
    std::iter::once(&targets.config)
        .chain(targets.baselines.iter())
        .filter(|path| path.exists())
        .cloned()
        .collect()
}

fn render_config(project_name: &str) -> Result<String> {
    let template = ConfigTemplate {
        project: ProjectTemplate { name: project_name },
    };
    let mut text =
        toml::to_string_pretty(&template).context("serialize project config template")?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(Consts::COMMENTED_CONFIG_TEMPLATE);
    Ok(text)
}

fn write_file(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("resolve init target parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create init target dir: {}", parent.display()))?;
    fs::write(path, contents).with_context(|| format!("write init target: {}", path.display()))
}
