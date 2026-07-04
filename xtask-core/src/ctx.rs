use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::common::project::ProjectConfig;

pub struct Ctx {
    pub root: PathBuf,
    pub config: ProjectConfig,
}

impl Ctx {
    /// Loads the xtask context for the current Cargo workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if Cargo metadata cannot be resolved for the current
    /// working directory or the project config cannot be loaded.
    pub fn load() -> Result<Self> {
        let metadata = cargo_metadata::MetadataCommand::new()
            .no_deps()
            .exec()
            .context("resolving workspace root via cargo metadata")?;
        let root = PathBuf::from(metadata.workspace_root.as_std_path());
        let config_path = root.join(".config/xtask.toml");
        let config = ProjectConfig::load(&root)
            .with_context(|| format!("loading {}", config_path.display()))?;
        Ok(Self { root, config })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, clap::Subcommand)]
    enum Inner {
        Alpha {
            #[arg(long)]
            level: u8,
        },
    }

    #[derive(Debug, clap::Subcommand)]
    enum Outer {
        #[command(flatten)]
        Core(Inner),
        Custom,
    }

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Outer,
    }

    #[test]
    fn flattened_subcommand_parses_at_top_level() {
        let cli = Cli::try_parse_from(["x", "alpha", "--level", "3"]);
        let cli = cli.expect("flatten must expose inner variants at top level");
        match cli.command {
            Outer::Core(Inner::Alpha { level }) => assert_eq!(level, 3),
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn ctx_load_resolves_workspace_root() {
        let ctx = Ctx::load().expect("ctx loads inside the kithara workspace");
        assert!(ctx.root.join("Cargo.toml").is_file());
        assert!(ctx.root.join("xtask-core").is_dir());
        assert_eq!(ctx.config.project.name, "kithara");
    }
}
