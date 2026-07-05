use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cargo_metadata::Metadata;

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
        let mut config = ProjectConfig::load(&root)
            .with_context(|| format!("loading {}", config_path.display()))?;
        if config.project.name.is_empty() {
            config.project.name = derive_project_name(&metadata, &root);
        }
        Ok(Self { root, config })
    }
}

fn derive_project_name(metadata: &Metadata, workspace_root: &Path) -> String {
    package_name_from_metadata(metadata, workspace_root)
        .or_else(|| workspace_dir_name(workspace_root))
        .unwrap_or_else(|| "xtask".to_string())
}

fn package_name_from_metadata(metadata: &Metadata, workspace_root: &Path) -> Option<String> {
    let root_manifest = workspace_root.join("Cargo.toml");
    let workspace_packages = metadata.workspace_packages();
    if let Some(package) = workspace_packages
        .iter()
        .find(|package| package.manifest_path.as_std_path() == root_manifest)
    {
        return Some(package.name.to_string());
    }
    if workspace_packages.len() == 1 {
        return Some(workspace_packages[0].name.to_string());
    }
    None
}

fn workspace_dir_name(workspace_root: &Path) -> Option<String> {
    workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use cargo_metadata::MetadataCommand;
    use clap::Parser;
    use tempfile::tempdir;

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
        assert!(ctx.root.join("crates/kithara-xtask-core").is_dir());
        assert_eq!(ctx.config.project.name, "kithara");
    }

    #[test]
    fn derive_project_name_uses_root_package_name() {
        let temp = tempdir().expect("tempdir");
        let manifest = temp.path().join("Cargo.toml");
        fs::write(
            &manifest,
            r#"
[package]
name = "metadata-name"
version = "0.1.0"
edition = "2024"
"#,
        )
        .expect("write manifest");
        fs::create_dir(temp.path().join("src")).expect("create src");
        fs::write(temp.path().join("src/lib.rs"), "").expect("write lib");

        let metadata = MetadataCommand::new()
            .manifest_path(&manifest)
            .no_deps()
            .exec()
            .expect("cargo metadata");

        assert_eq!(derive_project_name(&metadata, temp.path()), "metadata-name");
    }

    #[test]
    fn derive_project_name_falls_back_to_workspace_dir() {
        let temp = tempdir().expect("tempdir");
        let workspace = temp.path().join("demo-workspace");
        fs::create_dir(&workspace).expect("create workspace dir");
        fs::write(
            workspace.join("Cargo.toml"),
            r#"
[workspace]
members = ["a", "b"]
resolver = "3"
"#,
        )
        .expect("write workspace manifest");
        for name in ["a", "b"] {
            let package = workspace.join(name);
            fs::create_dir_all(package.join("src")).expect("create package src");
            fs::write(
                package.join("Cargo.toml"),
                format!(
                    r#"
[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
"#
                ),
            )
            .expect("write package manifest");
            fs::write(package.join("src/lib.rs"), "").expect("write package lib");
        }

        let metadata = MetadataCommand::new()
            .manifest_path(workspace.join("Cargo.toml"))
            .no_deps()
            .exec()
            .expect("cargo metadata");

        assert_eq!(derive_project_name(&metadata, &workspace), "demo-workspace");
    }
}
