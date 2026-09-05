use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::{Ctx, common::tools::ToolsConfig};
use sha2::{Digest, Sha256};

use super::process::Process;
use crate::{
    config::{KitharaExt, PackageProfile, PublishStep, ReleaseConfig},
    publish, release,
};

/// The retained framework, the documentation archive, and the WASM bundle are
/// built by three jobs. They share nothing but the checkout, so a failure in
/// one is retried without rebuilding the other two, and the release pipeline
/// shows which artifact is missing by name.
pub(crate) fn xcframework(
    process: &Process,
    ctx: &Ctx,
    ext: &KitharaExt,
    temp: &Path,
    package: &str,
) -> Result<()> {
    process.require_os("macos", "Apple release")?;
    let profile = ext.release.package(package)?;
    let expected = expected_checksum(profile, &ctx.root.join(&ext.release.manifest))?;

    process.run(
        ctx.config.tools.program("just"),
        &["platform", "apple", "release"],
        "Apple release artifacts",
    )?;
    let names: Vec<&str> = profile
        .assets
        .iter()
        .map(|key| ext.release.asset_name(*key))
        .filter(|name| !name.is_empty())
        .collect();
    for name in &names {
        copy_required(&temp.join(name), &ctx.root.join(name))?;
    }

    if let Some(manifest_checksum) = expected {
        let primary = ctx.root.join(&ext.release.core_asset);
        let actual_checksum = swift_checksum(process, &ctx.config.tools, &primary)?;
        if actual_checksum != manifest_checksum {
            bail!(
                "{} checksum does not match the retained XCFramework: expected \
                 {manifest_checksum}, found {actual_checksum}",
                ext.release.manifest
            );
        }
    }

    for name in &names {
        write_checksum(&ctx.root.join(name))?;
    }
    write_provenance(&ctx.root, package, &Provenance::from_env(), &names)
}

/// What a downloaded archive has to say about itself: the commit it was built
/// from, the ref it was built on, and the profile that built it. A reviewer
/// holding several archives at once cannot tell them apart otherwise, and the
/// job that produced one is not always still at hand.
struct Provenance {
    sha: String,
    branch: String,
    merge_request: Option<String>,
}

impl Provenance {
    fn from_env() -> Self {
        Self {
            sha: env::var("CI_COMMIT_SHA").unwrap_or_default(),
            branch: env::var("CI_COMMIT_REF_NAME").unwrap_or_default(),
            merge_request: env::var("CI_MERGE_REQUEST_IID").ok(),
        }
    }
}

fn write_provenance(
    root: &Path,
    package: &str,
    provenance: &Provenance,
    assets: &[&str],
) -> Result<()> {
    let mut entries = String::new();
    for name in assets {
        let digest = sha256(&root.join(name))?;
        if !entries.is_empty() {
            entries.push_str(",\n");
        }
        entries.push_str(&format!(
            "    {{ \"name\": \"{name}\", \"sha256\": \"{digest}\" }}"
        ));
    }
    let merge_request = provenance
        .merge_request
        .as_deref()
        .map_or_else(|| "null".to_string(), |iid| format!("\"{iid}\""));
    let body = format!(
        "{{\n  \"package\": \"{package}\",\n  \"sha\": \"{sha}\",\n  \"branch\": \"{branch}\",\n  \
         \"merge_request\": {merge_request},\n  \"assets\": [\n{entries}\n  ]\n}}\n",
        sha = provenance.sha,
        branch = provenance.branch,
    );
    let path = root.join("provenance.json");
    fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
}

/// The checksum the built framework has to match, or `None` when the profile
/// carries no version. The manifest pins what a released version resolves to,
/// so the two must agree before that version is published; packaging a commit
/// for someone to install by hand answers a different question and names no
/// version to check against.
fn expected_checksum(profile: &PackageProfile, manifest_path: &Path) -> Result<Option<String>> {
    if !profile.version_gate {
        return Ok(None);
    }
    let version = required_env("KITHARA_RELEASE_VERSION")?;
    let manifest = fs::read_to_string(manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest_version = manifest_field(&manifest, "version")?;
    if manifest_version != version {
        bail!(
            "{} version is {manifest_version}, requested {version}",
            manifest_path.display()
        );
    }
    Ok(Some(manifest_field(&manifest, "checksum")?))
}

pub(crate) fn docs(process: &Process, ctx: &Ctx, ext: &KitharaExt) -> Result<()> {
    process.require_os("macos", "Apple documentation release")?;
    // The documentation is generated against the local Swift package, and the
    // package resolves its binary target from the debug build tree. Without
    // it the manifest itself refuses to load, long before anything is
    // documented, so this job builds the framework it reads.
    process.run(
        ctx.config.tools.program("just"),
        &["platform", "apple", "xcframework", "--profile", "debug"],
        "Apple XCFramework",
    )?;
    process.run(
        ctx.config.tools.program("just"),
        &["platform", "apple", "doc"],
        "Apple documentation",
    )?;
    zip_directory(
        process,
        &ctx.config.tools,
        &ctx.root.join(&ext.release.docs_archive),
        &ctx.root.join(&ext.release.docs_asset),
    )?;
    write_checksum(&ctx.root.join(&ext.release.docs_asset))
}

pub(crate) fn wasm(process: &Process, ctx: &Ctx, ext: &KitharaExt) -> Result<()> {
    process.require_os("macos", "WASM release")?;
    process.run(
        ctx.config.tools.program("just"),
        &["platform", "wasm", "build", "--profile", "release"],
        "release WASM bundle",
    )?;
    zip_directory(
        process,
        &ctx.config.tools,
        &ctx.root.join(&ext.release.wasm_dist),
        &ctx.root.join(&ext.release.wasm_asset),
    )?;
    write_checksum(&ctx.root.join(&ext.release.wasm_asset))
}

pub(crate) fn build_android(process: &Process, ctx: &Ctx, ext: &KitharaExt) -> Result<()> {
    process.require_os("macos", "Android release")?;
    // The archive builds the libraries and generates the bindings itself, for
    // the release profile the artifact ships. A native build before it took
    // thirteen minutes for both ABIs in the debug profile, and the archive
    // then recreated the directories it had written and did the work again.
    process.run(
        ctx.config.tools.program("just"),
        &["platform", "android", "aar"],
        "Android AAR release",
    )?;
    for name in &ext.android.aars {
        let source = ctx.root.join("android/lib/build/outputs/aar").join(name);
        let destination = ctx.root.join(name);
        copy_required(&source, &destination)?;
        write_checksum(&destination)?;
    }
    Ok(())
}

pub(crate) fn publish(process: &Process, ctx: &Ctx, ext: &KitharaExt, channel: &str) -> Result<()> {
    // The jobs this one waits for take hours, and these tools are reached deep
    // inside the publish steps. Ask for them first, so a host that lacks one
    // says so before a release is built rather than after.
    process.require_tools(&["cargo", "gh", "git", ctx.config.tools.program("unzip")])?;
    let profile = ext.release.channel(channel)?;
    for variable in &profile.tokens {
        required_env(variable)?;
    }
    let source_sha = required_env("CI_COMMIT_SHA")?;

    if profile.requires_version {
        let version = required_env("KITHARA_RELEASE_VERSION")?;
        let manifest = git_show(ctx, &source_sha, &ext.release.manifest)?;
        let manifest_version = manifest_field(&manifest, "version")?;
        if manifest_version != version {
            bail!("source manifest version is {manifest_version}, requested {version}");
        }
    }

    // A channel that carries no version replaces one rolling tag with whatever
    // the branch built today, so an asset it never built is absent rather than
    // wrong.
    for name in retained_assets(&ext.release) {
        let path = ctx.root.join(name);
        if profile.require_all_assets || path.is_file() {
            verify_checksum(&path)?;
        }
    }

    for step in &profile.steps {
        match step {
            PublishStep::Retained => release::publish_retained(ctx, &source_sha, &ctx.root)?,
            PublishStep::NightlyRetained => {
                release::publish_nightly_retained(ctx, &source_sha, &ctx.root)?;
            }
            PublishStep::Pages => release::publish_pages_retained(ctx, &source_sha, &ctx.root)?,
            PublishStep::Crates => publish::publish_release(ctx)?,
        }
    }
    Ok(())
}

fn retained_assets(config: &ReleaseConfig) -> impl Iterator<Item = &str> {
    [
        config.core_asset.as_str(),
        config.merged_asset.as_str(),
        config.docs_asset.as_str(),
        config.wasm_asset.as_str(),
    ]
    .into_iter()
    .chain(config.platform_assets.iter().map(String::as_str))
    .filter(|name| !name.is_empty())
}

fn zip_directory(
    process: &Process,
    tools: &ToolsConfig,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    if !source.is_dir() {
        bail!("release directory not found: {}", source.display());
    }
    if destination.exists() {
        fs::remove_file(destination)
            .with_context(|| format!("removing {}", destination.display()))?;
    }
    let parent = source
        .parent()
        .with_context(|| format!("{} has no parent", source.display()))?;
    let name = source
        .file_name()
        .with_context(|| format!("{} has no final component", source.display()))?;
    let mut command = process.command(tools.program("zip"));
    command
        .current_dir(parent)
        .args(["-r", "-y", "-q"])
        .arg(destination)
        .arg(name);
    process.run_command(&mut command, "zip retained release directory")
}

fn copy_required(source: &Path, destination: &Path) -> Result<()> {
    if !source.is_file() {
        bail!("release build did not produce {}", source.display());
    }
    fs::copy(source, destination).with_context(|| {
        format!(
            "copying retained artifact {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn swift_checksum(process: &Process, tools: &ToolsConfig, path: &Path) -> Result<String> {
    let output = process
        .command(tools.program("swift"))
        .args(["package", "compute-checksum"])
        .arg(path)
        .output()
        .context("running swift package compute-checksum")?;
    if !output.status.success() {
        bail!(
            "swift package compute-checksum failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout)
        .context("swift checksum was not UTF-8")
        .map(|value| value.trim().to_string())
}

fn write_checksum(path: &Path) -> Result<()> {
    let checksum = sha256(path)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("artifact has no UTF-8 file name: {}", path.display()))?;
    fs::write(
        checksum_path(path),
        format!("{checksum}  {name}\n").as_bytes(),
    )
    .with_context(|| format!("writing checksum for {}", path.display()))
}

fn verify_checksum(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("retained release artifact is missing: {}", path.display());
    }
    let expected = fs::read_to_string(checksum_path(path))
        .with_context(|| format!("reading checksum for {}", path.display()))?
        .split_whitespace()
        .next()
        .map(str::to_string)
        .context("checksum file is empty")?;
    let actual = sha256(path)?;
    if actual != expected {
        bail!(
            "retained artifact {} has sha256 {actual}, expected {expected}",
            path.display()
        );
    }
    Ok(())
}

fn checksum_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".sha256");
    PathBuf::from(value)
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("opening retained artifact {}", path.display()))?;
    let mut digest = Sha256::new();
    io::copy(&mut file, &mut DigestWriter(&mut digest))
        .with_context(|| format!("hashing retained artifact {}", path.display()))?;
    Ok(hex::encode(digest.finalize()))
}

struct DigestWriter<'a, D>(&'a mut D);

impl<D: Digest> io::Write for DigestWriter<'_, D> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn manifest_field(manifest: &str, key: &str) -> Result<String> {
    let prefix = format!("let {key} = \"");
    manifest
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|rest| rest.strip_suffix('"'))
        .map(str::to_string)
        .with_context(|| format!("`let {key} = \"...\"` not found in release manifest"))
}

fn git_show(ctx: &Ctx, revision: &str, path: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .current_dir(&ctx.root)
        .args(["show", &format!("{revision}:{path}")])
        .output()
        .context("reading release manifest from Git")?;
    if !output.status.success() {
        bail!(
            "git show failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("release manifest from Git was not UTF-8")
}

fn required_env(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{name} is required"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AssetKey;

    #[test]
    fn provenance_names_the_commit_and_every_asset() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("Kithara.xcframework.zip"), b"bytes").unwrap();

        write_provenance(
            root,
            "snapshot",
            &Provenance {
                sha: "1575b93875fe".into(),
                branch: "laba/420-repeat-one-behavior".into(),
                merge_request: Some("14".into()),
            },
            &["Kithara.xcframework.zip"],
        )
        .unwrap();

        let written = fs::read_to_string(root.join("provenance.json")).unwrap();
        assert!(written.contains("1575b93875fe"));
        assert!(written.contains("laba/420-repeat-one-behavior"));
        assert!(written.contains("Kithara.xcframework.zip"));
        assert!(written.contains("\"merge_request\": \"14\""));
    }

    #[test]
    fn provenance_outside_a_pipeline_names_no_merge_request() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("Kithara.xcframework.zip"), b"bytes").unwrap();

        write_provenance(
            root,
            "snapshot",
            &Provenance {
                sha: String::new(),
                branch: String::new(),
                merge_request: None,
            },
            &["Kithara.xcframework.zip"],
        )
        .unwrap();

        let written = fs::read_to_string(root.join("provenance.json")).unwrap();
        assert!(written.contains("\"merge_request\": null"));
    }

    #[test]
    fn a_gate_free_profile_asks_for_no_version() {
        let profile = PackageProfile {
            version_gate: false,
            assets: vec![AssetKey::Merged],
        };

        let expected = expected_checksum(&profile, Path::new("Package.swift"))
            .expect("a gate-free profile reads no manifest");

        assert!(expected.is_none());
    }

    #[test]
    fn checksum_round_trip_detects_changed_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("artifact.zip");
        fs::write(&artifact, b"first").unwrap();
        write_checksum(&artifact).unwrap();
        verify_checksum(&artifact).unwrap();
        fs::write(&artifact, b"second").unwrap();
        assert!(verify_checksum(&artifact).is_err());
    }

    #[test]
    fn manifest_field_requires_exact_release_declaration() {
        let manifest = "let version = \"1.2.3\"\nlet checksum = \"abc\"\n";
        assert_eq!(manifest_field(manifest, "version").unwrap(), "1.2.3");
        assert!(manifest_field("let other = \"1.2.3\"\n", "version").is_err());
    }
}
