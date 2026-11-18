use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::{Ctx, common::tools::ToolsConfig};

use super::{process::Process, run::PipelineKind};
use crate::{
    android,
    config::{KitharaExt, PublishStep},
    consts, publish,
    release::{self, sha256},
    wasm,
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
    kind: PipelineKind,
) -> Result<()> {
    process.require_os(&["macos"], "Apple release")?;
    let profile = ext.release.package(package)?;
    // Every crate has to carry the version before hours go into building it.
    if let Some(version) = version_variable(kind).map(required_env).transpose()? {
        publish::release_crates(&version)?;
    }

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

/// The variable naming the version this pipeline publishes, or `None` when it
/// publishes none. A release is a version: someone names it, every crate
/// carries it, and the tag step stamps the Swift manifest with it.
/// The rolling nightly channel names no version by design, and neither does
/// the door that asks only whether the build lanes still work - one job builds
/// for all three, so a gate the job carries unconditionally fails the two that
/// have no value to give it.
const fn version_variable(kind: PipelineKind) -> Option<&'static str> {
    match kind {
        PipelineKind::Release => Some("KITHARA_RELEASE_VERSION"),
        _ => None,
    }
}

pub(crate) fn docs(process: &Process, ctx: &Ctx, ext: &KitharaExt) -> Result<()> {
    process.require_os(&["macos"], "Apple documentation release")?;
    // The documentation is generated against the local Swift package, and the
    // package resolves its binary target from the debug build tree. Without
    // it the manifest itself refuses to load, long before anything is
    // documented, so this job builds the framework it reads.
    process.run(
        ctx.config.tools.program("just"),
        &["platform", "apple", "xcframework", "--profile", "debug"],
        "Apple XCFramework",
    )?;
    // DocC writes its links for the path the site serves the archive under.
    let hosting = release::hosting_base(&ext.release, "apple")?;
    process.run(
        ctx.config.tools.program("just"),
        &["platform", "apple", "doc", &hosting],
        "Apple documentation",
    )?;
    package_docs(process, ctx, ext, "apple")
}

/// Zip one rendered documentation directory into the release asset the channel
/// names, and record its checksum beside it.
fn package_docs(process: &Process, ctx: &Ctx, ext: &KitharaExt, channel: &str) -> Result<()> {
    let docs = ext.release.docs_channel(channel)?;
    zip_directory(
        process,
        &ctx.config.tools,
        &ctx.root.join(&docs.archive),
        &ctx.root.join(&docs.asset),
    )?;
    write_checksum(&ctx.root.join(&docs.asset))
}

pub(crate) fn wasm(process: &Process, ctx: &Ctx, ext: &KitharaExt) -> Result<()> {
    process.require_os(&["macos"], "WASM release")?;
    build_wasm_bundle(
        process,
        ctx.config.tools.program("just"),
        &required_env("CI_COMMIT_SHA")?,
    )?;
    zip_directory(
        process,
        &ctx.config.tools,
        &ctx.root.join(&ext.release.wasm_dist),
        &ctx.root.join(&ext.release.wasm_asset),
    )?;
    write_checksum(&ctx.root.join(&ext.release.wasm_asset))?;
    wasm::render_docs(&ctx.root.join(&ext.release.docs_channel("web")?.archive))?;
    package_docs(process, ctx, ext, "web")
}

/// Builds the web bundle for the commit it ships, which is the only build
/// that tells the FFI crate what to embed.
fn build_wasm_bundle(process: &Process, just: &str, revision: &str) -> Result<()> {
    let mut command = process.command(just);
    command
        .args(["platform", "wasm", "build", "--profile", "release"])
        .env(consts::BUILD_REVISION_ENV, revision);
    process.run_command(&mut command, "release WASM bundle")
}

pub(crate) fn build_android(process: &Process, ctx: &Ctx, ext: &KitharaExt) -> Result<()> {
    process.require_os(&["macos"], "Android release")?;
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
    // Dokka reads the Kotlin the archive above generated, so the documentation
    // is rendered here rather than in a job that would have to build it again.
    android::render_docs(process)?;
    package_docs(process, ctx, ext, "android")
}

pub(crate) fn publish(process: &Process, ctx: &Ctx, ext: &KitharaExt, channel: &str) -> Result<()> {
    let profile = ext.release.channel(channel)?;
    // The jobs this one waits for take hours, and these tools are reached deep
    // inside the publish steps. Ask for them first, so a host that lacks one
    // says so before a release is built rather than after.
    let tools = &ctx.config.tools;
    let mut required = vec!["cargo", "gh", "git", "curl", tools.program("unzip")];
    if profile.steps.contains(&PublishStep::Tag) {
        required.push(tools.program("git-cliff"));
    }
    process.require_tools(&required)?;
    for variable in &profile.tokens {
        required_env(variable)?;
    }
    let source = required_env("CI_COMMIT_SHA")?;
    let version = profile
        .requires_version
        .then(|| required_env("KITHARA_RELEASE_VERSION"))
        .transpose()?;
    let crates = version
        .as_deref()
        .map(publish::release_crates)
        .transpose()?
        .unwrap_or_default();

    // A channel that carries no version replaces one rolling tag with whatever
    // the branch built today, so an asset it never built is absent rather than
    // wrong.
    verify_assets(&ctx.root, profile.require_all_assets, ext.release.assets())?;

    let mut tagged = None;
    for step in &profile.steps {
        match step {
            PublishStep::Tag => {
                tagged = Some(release::tag_release(
                    ctx,
                    &source,
                    versioned(version.as_deref(), *step)?,
                    &ctx.root,
                    &crates,
                )?);
            }
            PublishStep::Retained => {
                let commit = tagged
                    .as_deref()
                    .context("the retained step publishes the commit the tag step tagged")?;
                release::publish_release(
                    ctx,
                    commit,
                    versioned(version.as_deref(), *step)?,
                    &ctx.root,
                )?;
            }
            PublishStep::NightlyRetained => release::publish_nightly(ctx, &source, &ctx.root)?,
            PublishStep::Pages => {
                release::publish_pages(
                    ctx,
                    versioned(version.as_deref(), *step)?,
                    &crates,
                    &ctx.root,
                )?;
            }
            PublishStep::Crates => publish::publish_release(ctx)?,
        }
    }
    Ok(())
}

/// The version a step publishes: only a channel that names one runs it.
fn versioned(version: Option<&str>, step: PublishStep) -> Result<&str> {
    version.with_context(|| {
        format!("the {step:?} step publishes a version, and this channel names none")
    })
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

fn verify_assets<'a>(
    root: &Path,
    require_all: bool,
    assets: impl Iterator<Item = &'a str>,
) -> Result<()> {
    for name in assets {
        // A checksum sidecar ships as an asset of its own alongside the
        // archive it checks; it has no sidecar of its own to verify against.
        if name.ends_with(".sha256") {
            continue;
        }
        let path = root.join(name);
        if require_all || path.is_file() {
            verify_checksum(&path)?;
        }
    }
    Ok(())
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

fn required_env(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{name} is required"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::process::Recording;

    /// The web bundle embeds the commit it describes only when packaging
    /// names it, so the release names the commit it is built from.
    #[test]
    fn the_release_bundle_is_told_the_commit_it_is_built_from() {
        let process = Process::recording(Path::new("/checkout"), Recording::default());

        build_wasm_bundle(&process, "just", "0123abcd").unwrap();

        let recording = process.recorded().unwrap();
        let steps = recording.steps();
        let [step] = steps else {
            panic!("one step: {steps:?}");
        };
        assert_eq!(
            step.args,
            ["platform", "wasm", "build", "--profile", "release"]
        );
        assert_eq!(
            step.env.get(consts::BUILD_REVISION_ENV).map(String::as_str),
            Some("0123abcd")
        );
    }

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
    fn a_release_pipeline_publishes_the_version_it_was_given() {
        assert_eq!(
            version_variable(PipelineKind::Release),
            Some("KITHARA_RELEASE_VERSION")
        );
    }

    #[test]
    fn the_rolling_nightly_publishes_no_version() {
        assert_eq!(version_variable(PipelineKind::Nightly), None);
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
    fn checksum_sidecars_ship_without_being_verified_themselves() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("Kithara-macos-universal.zip");
        fs::write(&artifact, b"app").unwrap();
        write_checksum(&artifact).unwrap();

        // The sidecar is an asset of its own, so publication uploads it; the
        // loop must not ask for a checksum of the checksum.
        verify_assets(
            directory.path(),
            true,
            [
                "Kithara-macos-universal.zip",
                "Kithara-macos-universal.zip.sha256",
            ]
            .into_iter(),
        )
        .unwrap();
    }

    #[test]
    fn a_present_artifact_without_its_sidecar_still_stops_the_channel() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("Kithara-macos-universal.zip");
        fs::write(&artifact, b"app").unwrap();

        // The sidecar ships as its own asset, but its presence in the list
        // must not exempt the archive it belongs to from verification.
        let error = verify_assets(
            directory.path(),
            false,
            [
                "Kithara-macos-universal.zip",
                "Kithara-macos-universal.zip.sha256",
            ]
            .into_iter(),
        )
        .expect_err("an artifact without its sidecar cannot publish");
        assert!(error.to_string().contains("reading checksum"), "{error:#}");
    }

    #[test]
    fn a_channel_that_requires_every_asset_still_rejects_an_absent_one() {
        let directory = tempfile::tempdir().unwrap();
        let error = verify_assets(directory.path(), true, ["kithara.aar"].into_iter())
            .expect_err("a required asset the build never made stops the channel");
        assert!(error.to_string().contains("is missing"), "{error:#}");
    }

    #[test]
    fn a_rolling_channel_skips_an_asset_the_build_never_made() {
        let directory = tempfile::tempdir().unwrap();
        verify_assets(directory.path(), false, ["kithara.aar"].into_iter()).unwrap();
    }
}
