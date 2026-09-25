use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, ensure};
use clap::Subcommand;
use kithara_devtools::Ctx;
use serde::Serialize;
use tracing::info;

use super::{
    discover::{Declaration, Registration, discover, registrations},
    project,
};

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Emit syntactic candidates; this does not certify SDK coverage.
    Discover {
        /// JSON output, relative to the workspace unless absolute.
        #[arg(long, default_value = "target/config-protocol/discovery.json")]
        output: PathBuf,
    },
    /// Emit registered retained values and delegated SDK operations.
    Manifest {
        /// JSON output, relative to the workspace unless absolute.
        #[arg(long, default_value = "target/config-protocol/manifest.json")]
        output: PathBuf,
        /// Compilation surface whose cfg availability must be verified later.
        #[arg(long, value_enum, default_value_t)]
        target_profile: TargetProfile,
    },
    /// Generate FFI records and converters for registered SDK operations.
    Project {
        /// Rust source output, relative to the workspace unless absolute.
        #[arg(
            long,
            default_value = "crates/kithara-ffi/src/core/config/config_generated.rs"
        )]
        output: PathBuf,
        /// Generated host record source, relative to the workspace unless absolute.
        #[arg(
            long,
            default_value = "crates/kithara-ffi/src/core/config/config_host_generated.rs"
        )]
        host_output: PathBuf,
        /// Generated per-item source settings, relative to the workspace unless absolute.
        #[arg(
            long,
            default_value = "crates/kithara-ffi/src/core/config/config_source_generated.rs"
        )]
        source_output: PathBuf,
        /// Verify the existing projection instead of writing it.
        #[arg(long)]
        check: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TargetProfile {
    #[default]
    Native,
    Apple,
    Android,
    Web,
}

#[derive(Serialize)]
struct Inventory {
    schema_version: u32,
    rust_files: usize,
    declarations: Vec<Declaration>,
}

#[derive(Serialize)]
struct Manifest {
    schema_version: u32,
    target_profile: TargetProfile,
    rust_files: usize,
    registrations: Vec<Registration>,
}

pub(crate) fn run(command: ConfigCommand, ctx: &Ctx) -> Result<()> {
    if let ConfigCommand::Project {
        output,
        host_output,
        source_output,
        check,
    } = &command
    {
        let manifest = manifest(&ctx.root, TargetProfile::Native)?;
        let generated = project::render(&manifest.registrations)?;
        let generated_host = project::render_host(&manifest.registrations)?;
        let generated_source = project::render_source(&manifest.registrations)?;
        let output = ctx.root.join(output);
        let host_output = ctx.root.join(host_output);
        let source_output = ctx.root.join(source_output);
        if *check {
            verify_projection(&output, &generated)?;
            verify_projection(&host_output, &generated_host)?;
            verify_projection(&source_output, &generated_source)?;
            info!(path = %output.display(), "configuration FFI projection is current");
            return Ok(());
        }
        write_projection(&output, generated)?;
        write_projection(&host_output, generated_host)?;
        write_projection(&source_output, generated_source)?;
        info!(path = %output.display(), "configuration FFI projection complete");
        return Ok(());
    }
    let (output, contents, files, entries, message) = match command {
        ConfigCommand::Discover { output } => {
            let inventory = inventory(&ctx.root)?;
            let entries = inventory.declarations.len();
            let files = inventory.rust_files;
            (
                output,
                serde_json::to_string_pretty(&inventory)?,
                files,
                entries,
                "configuration discovery complete; semantic classification is not checked",
            )
        }
        ConfigCommand::Manifest {
            output,
            target_profile,
        } => {
            let manifest = manifest(&ctx.root, target_profile)?;
            let entries = manifest.registrations.len();
            let files = manifest.rust_files;
            (
                output,
                serde_json::to_string_pretty(&manifest)?,
                files,
                entries,
                "configuration registration manifest complete",
            )
        }
        ConfigCommand::Project { .. } => unreachable!("handled above"),
    };
    let output = ctx.root.join(output);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut json = contents;
    json.push('\n');
    fs::write(&output, json).with_context(|| format!("write {}", output.display()))?;
    info!(files, entries, path = %output.display(), message);
    Ok(())
}

fn write_projection(output: &Path, generated: String) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(output, generated).with_context(|| format!("write {}", output.display()))
}

fn verify_projection(output: &Path, generated: &str) -> Result<()> {
    let existing = fs::read_to_string(output)
        .with_context(|| format!("read configuration FFI projection {}", output.display()))?;
    ensure!(
        existing == generated,
        "configuration FFI projection is stale: {}",
        output.display()
    );
    Ok(())
}

fn source_paths(root: &Path) -> Result<Vec<String>> {
    let files = Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .context("list configuration discovery inputs")?;
    ensure!(
        files.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&files.stderr)
    );
    let paths = String::from_utf8(files.stdout).context("non-UTF-8 discovery path")?;
    let mut paths: Vec<_> = paths
        .split('\0')
        .filter(|path| path.ends_with(".rs"))
        .map(str::to_owned)
        .collect();
    paths.sort_unstable();
    paths.dedup();
    ensure!(!paths.is_empty(), "no Rust discovery inputs");
    Ok(paths)
}

fn manifest(root: &Path, target_profile: TargetProfile) -> Result<Manifest> {
    let paths: Vec<_> = source_paths(root)?
        .into_iter()
        .filter(|path| !path.starts_with("tests/"))
        .collect();
    let mut manifest = Manifest {
        schema_version: 1,
        target_profile,
        rust_files: paths.len(),
        registrations: Vec::new(),
    };
    for path in paths {
        let source = fs::read_to_string(root.join(&path))
            .with_context(|| format!("read configuration registration input {path}"))?;
        manifest
            .registrations
            .extend(registrations(&path, &source)?);
    }
    Ok(manifest)
}

fn inventory(root: &Path) -> Result<Inventory> {
    // Include new source files without scanning ignored build outputs.
    let paths = source_paths(root)?;
    let mut inventory = Inventory {
        schema_version: 1,
        rust_files: paths.len(),
        declarations: Vec::new(),
    };
    for path in paths {
        let source = fs::read_to_string(root.join(&path))
            .with_context(|| format!("read configuration discovery input {path}"))?;
        inventory.declarations.extend(discover(&path, &source)?);
    }
    Ok(inventory)
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::{TargetProfile, inventory, manifest, verify_projection};

    #[test]
    fn projection_check_rejects_stale_output() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("config_generated.rs");
        fs::write(&output, "old").unwrap();
        assert!(verify_projection(&output, "new").is_err());
        fs::write(&output, "new").unwrap();
        verify_projection(&output, "new").unwrap();
    }

    #[test]
    fn discovery_includes_untracked_source_excludes_artifacts_and_rejects_bad_source() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .arg(root.path())
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.path().join(".gitignore"), "target/\n").unwrap();
        fs::create_dir(root.path().join("target")).unwrap();
        fs::write(root.path().join("target/generated.rs"), "not valid Rust").unwrap();
        fs::write(
            root.path().join("lib.rs"),
            "struct NewConfig { value: u64 }",
        )
        .unwrap();
        let first = inventory(root.path()).unwrap();
        assert_eq!(first.rust_files, 1);
        assert_eq!(first.declarations.len(), 1);
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&inventory(root.path()).unwrap()).unwrap()
        );
        fs::write(root.path().join("broken.rs"), "struct MissingConfig {").unwrap();
        assert!(inventory(root.path()).is_err());
    }

    #[test]
    fn manifest_is_deterministic_and_contains_only_explicit_registrations() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .arg(root.path())
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            root.path().join("lib.rs"),
            "#[kithara_config::config] struct Registered { #[config(value)] value: u64 } struct IgnoredConfig { value: u64 }",
        )
        .unwrap();
        fs::create_dir(root.path().join("tests")).unwrap();
        fs::write(
            root.path().join("tests/fixture.rs"),
            "#[kithara_config::config] struct Fixture { #[config(value)] value: u64 }",
        )
        .unwrap();
        let first = manifest(root.path(), TargetProfile::Native).unwrap();
        assert_eq!(first.rust_files, 1);
        assert_eq!(first.registrations.len(), 1);
        assert_eq!(first.registrations[0].owner, "Registered");
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&manifest(root.path(), TargetProfile::Native).unwrap()).unwrap()
        );
    }
}
