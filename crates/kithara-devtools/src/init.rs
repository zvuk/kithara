use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use clap::Args;
use serde::Serialize;

use crate::{Ctx, common::baseline::Baseline, consts};

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Overwrite existing files instead of failing.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, derive_more::Error)]
#[error(ignore)]
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

    println!("{}", consts::MAIN_RS_SNIPPET);
    Ok(())
}

fn target_files(root: &Path) -> TargetFiles {
    let baselines = consts::BASELINE_CONFIG_DIRS
        .iter()
        .map(|dir| Baseline::path(&root.join(dir)))
        .collect();
    TargetFiles {
        baselines,
        config: root.join(consts::PROJECT_CONFIG_REL),
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
    text.push_str(consts::COMMENTED_CONFIG_TEMPLATE);
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
