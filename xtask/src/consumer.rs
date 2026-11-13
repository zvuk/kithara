//! Public UI contracts checked from a generated external consumer.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use clap::Args;
use kithara_devtools::Ctx;
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::child;

#[derive(Debug, Args)]
pub(crate) struct ConsumerArgs {
    /// Caller-owned font with Cyrillic glyphs, distinct from the embedded Sans face.
    #[arg(long)]
    font: PathBuf,
    /// Receipt directory; each invocation retains a separate run and reuses its target.
    #[arg(long)]
    artifacts: PathBuf,
}

struct Consumer<'a> {
    directory: &'a Path,
    capture: &'a Path,
    target: &'a Path,
    font: &'a Path,
    cancel: &'a child::Cancel,
}

pub(crate) fn run(ctx: &Ctx, args: &ConsumerArgs) -> Result<()> {
    let font = args.font.canonicalize().context("caller font file")?;
    let artifacts = ctx.root.join(&args.artifacts);
    fs::create_dir_all(&artifacts)?;
    let artifacts = artifacts.canonicalize()?;
    let capture = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&artifacts)?
        .keep();
    let source = tempfile::Builder::new()
        .prefix("kithara-ui-consumer-")
        .tempdir()?;
    ensure!(
        !source
            .path()
            .canonicalize()?
            .starts_with(ctx.root.canonicalize()?),
        "consumer must be outside the checkout"
    );
    let target = artifacts.join("target");
    let cancel = child::Cancel::install()?;
    let consumer = Consumer {
        directory: source.path(),
        capture: &capture,
        target: &target,
        font: &font,
        cancel: &cancel,
    };
    info!(receipts = %capture.display(), "external UI consumer");
    consumer.prepare(&ctx.root)?;
    consumer.checks()
}

impl Consumer<'_> {
    fn prepare(&self, root: &Path) -> Result<()> {
        fs::create_dir(self.directory.join("src"))?;
        fs::create_dir(self.capture.join("src"))?;
        for name in ["consumer.rs", "host.rs"] {
            let template = root.join("xtask/tests/fixtures/ui").join(name);
            fs::copy(&template, self.directory.join("src").join(name))?;
            fs::copy(template, self.capture.join("src").join(name))?;
        }
        let manifest = manifest(root)?;
        fs::write(self.directory.join("Cargo.toml"), &manifest)?;
        fs::write(self.capture.join("Cargo.toml"), manifest)?;
        fs::write(
            self.capture.join("font.sha256"),
            format!("{}\n", hex::encode(Sha256::digest(fs::read(self.font)?))),
        )?;
        self.checked(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["rev-parse", "HEAD"]),
            "source-revision",
            Some(30),
        )?;
        self.checked(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["status", "--porcelain"]),
            "source-status",
            Some(30),
        )?;
        self.checked(Command::new("rustc").arg("-vV"), "toolchain", Some(30))?;
        self.checked(
            &mut self.cargo(&["generate-lockfile", "--offline"]),
            "lock",
            Some(300),
        )?;
        fs::copy(
            self.directory.join("Cargo.lock"),
            self.capture.join("Cargo.lock"),
        )?;
        Ok(())
    }

    fn checks(&self) -> Result<()> {
        let mut matrix = Vec::new();
        for feature in ["render", "vello", "masonry"] {
            self.graphs(feature)?;
            self.checked(
                &mut self.cargo(&[
                    "run",
                    "--offline",
                    "--locked",
                    "--features",
                    feature,
                    "--bin",
                    "consumer",
                ]),
                &format!("native-{feature}"),
                None,
            )?;
            if feature == "masonry" {
                self.checked(
                    &mut self.cargo(&[
                        "run",
                        "--offline",
                        "--locked",
                        "--features",
                        feature,
                        "--bin",
                        "host",
                    ]),
                    "native-host",
                    None,
                )?;
            }
            let label = format!("wasm-{feature}");
            let status = self.execute(
                &mut self.cargo(&[
                    "check",
                    "--offline",
                    "--locked",
                    "--features",
                    feature,
                    "--target",
                    "wasm32-unknown-unknown",
                    "--bins",
                ]),
                &label,
                None,
            )?;
            let availability = if status.success() {
                "compile-only"
            } else {
                "unavailable"
            };
            matrix.push(json!({"feature": feature, "native": "passed", "wasm": availability}));
            fs::write(
                self.capture.join("matrix.json"),
                serde_json::to_vec_pretty(&matrix)?,
            )?;
            ensure!(
                status.success() || feature == "masonry",
                "{label} failed; see {}",
                self.capture.display()
            );
            if !status.success() {
                warn!(feature, "wasm route unavailable; compiler receipt retained");
            }
        }
        Ok(())
    }

    fn graphs(&self, feature: &str) -> Result<()> {
        for (platform, target) in [
            ("apple", "aarch64-apple-darwin"),
            ("wasm", "wasm32-unknown-unknown"),
        ] {
            for (kind, edges) in [("normal-build", "normal,build"), ("features", "features")] {
                let label = format!("{platform}-{feature}-{kind}");
                self.checked(
                    &mut self.cargo(&[
                        "tree",
                        "--offline",
                        "--locked",
                        "--target",
                        target,
                        "--features",
                        feature,
                        "--edges",
                        edges,
                        "--prefix",
                        "none",
                    ]),
                    &label,
                    Some(300),
                )?;
                if platform == "apple" && kind == "normal-build" {
                    let tree = fs::read_to_string(self.capture.join(format!("{label}.stdout")))?;
                    ensure!(!tree.trim().is_empty(), "empty normal/build graph");
                    let audio = audio_packages(&tree);
                    ensure!(
                        audio.is_empty(),
                        "{label} contains product audio union: {audio:?}"
                    );
                }
            }
        }
        Ok(())
    }

    fn cargo(&self, args: &[&str]) -> Command {
        let mut command = Command::new("cargo");
        command
            .current_dir(self.directory)
            .env("CARGO_TARGET_DIR", self.target)
            .env("EXTERNAL_UI_FONT", self.font)
            .args(args);
        for name in [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTDOCFLAGS",
            "RUSTC_WORKSPACE_WRAPPER",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn checked(&self, command: &mut Command, label: &str, seconds: Option<u64>) -> Result<()> {
        ensure!(
            self.execute(command, label, seconds)?.success(),
            "{label} failed; see {}",
            self.capture.display()
        );
        Ok(())
    }

    fn execute(
        &self,
        command: &mut Command,
        label: &str,
        seconds: Option<u64>,
    ) -> Result<ExitStatus> {
        let argv: Vec<_> = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        fs::write(
            self.capture.join(format!("{label}.command.json")),
            serde_json::to_vec_pretty(&argv)?,
        )?;
        info!(label, "external UI check");
        command
            .current_dir(self.directory)
            .stdin(Stdio::null())
            .stdout(File::create(self.capture.join(format!("{label}.stdout")))?)
            .stderr(File::create(self.capture.join(format!("{label}.stderr")))?);
        let mut process = child::spawn(command)?;
        let status = child::supervise(
            &mut process,
            Some(self.cancel),
            seconds.map(Duration::from_secs),
        )
        .with_context(|| format!("{label}; receipts at {}", self.capture.display()))?;
        fs::write(
            self.capture.join(format!("{label}.status.json")),
            serde_json::to_vec(&json!({"success":status.success(), "exit":status.code()}))?,
        )?;
        Ok(status)
    }
}

fn manifest(root: &Path) -> Result<String> {
    let workspace: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let dependencies = workspace
        .get("workspace")
        .and_then(|value| value.get("dependencies"))
        .context("workspace dependencies")?;
    let mut manifest: toml::Value = toml::from_str(
        r#"
[workspace]
resolver = "2"
[workspace.dependencies]
[package]
name = "external-ui-consumer"
version = "0.0.0"
edition = "2024"
publish = false
[features]
render = ["dep:kithara-ui", "kithara-ui/render"]
vello = ["render", "kithara-ui/vello", "dep:vello"]
masonry = ["vello", "kithara-ui/masonry", "dep:masonry"]
[dependencies]
kithara-platform = { workspace = true }
kithara-ui = { workspace = true, optional = true }
kithara-ui-draw = { workspace = true, features = ["list"] }
kithara-ui-shaping = { workspace = true, features = ["shape"] }
skrifa = { workspace = true }
masonry = { workspace = true, optional = true }
vello = { workspace = true, optional = true }
[[bin]]
name = "consumer"
path = "src/consumer.rs"
[[bin]]
name = "host"
path = "src/host.rs"
required-features = ["masonry"]
"#,
    )?;
    let registry = manifest["workspace"]["dependencies"]
        .as_table_mut()
        .context("consumer dependency registry")?;
    for name in [
        "kithara-platform",
        "kithara-ui",
        "kithara-ui-draw",
        "kithara-ui-shaping",
    ] {
        let relative = dependencies
            .get(name)
            .and_then(|value| value.get("path"))
            .and_then(toml::Value::as_str)
            .with_context(|| format!("workspace path for {name}"))?;
        let path = root.join(relative).canonicalize()?;
        let mut dependency = toml::Table::new();
        dependency.insert(
            "path".into(),
            path.to_str().context("UTF-8 dependency path")?.into(),
        );
        dependency.insert("default-features".into(), false.into());
        registry.insert(name.into(), dependency.into());
    }
    for name in ["skrifa", "masonry", "vello"] {
        registry.insert(
            name.into(),
            dependencies
                .get(name)
                .context("workspace dependency")?
                .clone(),
        );
    }
    toml::to_string(&manifest).context("serialize external UI manifest")
}

fn audio_packages(tree: &str) -> BTreeSet<&str> {
    tree.lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| {
            name.starts_with("firewheel") || matches!(*name, "cpal" | "kithara-workspace-hack")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_consumer_uses_checkout_paths_and_workspace_versions() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let generated: toml::Value = toml::from_str(&manifest(root).unwrap()).unwrap();
        let original: toml::Value =
            toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
        assert_eq!(
            generated["workspace"]["dependencies"]["skrifa"],
            original["workspace"]["dependencies"]["skrifa"]
        );
        assert_eq!(
            generated["dependencies"]["kithara-ui"]["workspace"].as_bool(),
            Some(true)
        );
        for name in [
            "kithara-platform",
            "kithara-ui",
            "kithara-ui-draw",
            "kithara-ui-shaping",
        ] {
            let dependency = &generated["workspace"]["dependencies"][name];
            assert!(
                Path::new(dependency["path"].as_str().unwrap())
                    .starts_with(root.canonicalize().unwrap())
            );
            assert_eq!(dependency["default-features"].as_bool(), Some(false));
        }
    }

    #[test]
    fn audio_union_includes_host_macro_dependencies() {
        assert_eq!(
            audio_packages(
                "kithara-ui v1\nfirewheel-macros v1\ncpal v1\nkithara-workspace-hack v1\n"
            ),
            BTreeSet::from(["cpal", "firewheel-macros", "kithara-workspace-hack"])
        );
        assert!(audio_packages("external-ui-consumer v1\nkithara-ui v1\n").is_empty());
    }
}
