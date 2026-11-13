use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::{Ctx, common::tools::ToolsConfig};

use super::{
    artifact::{file_name, sha256},
    changelog,
    git::{Remote, git},
    github, gitlab, notes, site, tag,
};
use crate::{
    config::{KitharaExt, ReleaseConfig},
    publish,
};

/// What the release publishes, rendered locally. CI publishes through
/// `ci release publish`, which runs the channel's steps in order: tag,
/// releases, Pages, crates.
#[derive(Debug, clap::Args)]
pub(crate) struct ReleaseArgs {
    #[command(subcommand)]
    command: ReleaseCommand,
}

#[derive(Debug, clap::Subcommand)]
enum ReleaseCommand {
    /// Build and sign a self-contained universal macOS application.
    App {
        /// Keychain identity name or fingerprint; also `KITHARA_APP_SIGN_IDENTITY`.
        #[arg(long)]
        identity: Option<String>,
    },
    /// Render the changelog from the history and the release tags.
    Changelog {
        /// Name the commits after the last release tag as this version.
        #[arg(long)]
        version: Option<String>,
    },
    /// Lay out the Pages site from release artifacts, without deploying it.
    Site {
        /// The version the site presents, without the `v` prefix.
        #[arg(long)]
        version: String,
        /// Directory holding the release artifacts.
        #[arg(long)]
        artifacts: PathBuf,
        /// Empty or missing directory the site is laid out in.
        #[arg(long)]
        output: PathBuf,
    },
}

pub(crate) fn run(args: &ReleaseArgs, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let cfg = &ext.release;
    let tools = &ctx.config.tools;
    match &args.command {
        ReleaseCommand::App { identity } => super::app::build(ctx, identity.as_deref()),
        ReleaseCommand::Changelog { version } => {
            if let Some(version) = version {
                tag::validate_version(version)?;
            }
            // The release tags sit beside the branch, so no branch fetch
            // brought them, and without them every release folds into one.
            Remote::github(&cfg.github_repo, None)?.fetch_tags(&ctx.root)?;
            changelog::write(&ctx.root, &cfg.changelog, tools, version.as_deref())
        }
        ReleaseCommand::Site {
            version,
            artifacts,
            output,
        } => {
            require_config(cfg)?;
            tag::validate_version(version)?;
            fs::create_dir_all(output).with_context(|| format!("creating {}", output.display()))?;
            if fs::read_dir(output)?.next().is_some() {
                bail!("{} is not empty", output.display());
            }
            let crates = publish::release_crates(version)?;
            build_site(cfg, tools, version, &crates, artifacts, output)?;
            println!("[site] laid out in {}", output.display());
            Ok(())
        }
    }
}

/// Stamp `version` on top of `source` and tag the stamp on both remotes.
/// `crates` are the crates the release publishes: while crates.io holds none
/// of them at `version`, a tag another build made is replaced. Answers the
/// tagged commit.
pub(crate) fn tag_release(
    ctx: &Ctx,
    source: &str,
    version: &str,
    artifacts: &Path,
    crates: &[String],
) -> Result<String> {
    let ext = KitharaExt::from_ctx(ctx)?;
    require_config(&ext.release)?;
    tag::ensure(
        &ext.release,
        &ctx.config.tools,
        &ctx.root,
        source,
        version,
        artifacts,
        || publish::registered(ctx, crates, version),
    )
}

/// Publish the tagged `commit` as release `version` on both remotes, with its
/// changelog section as the notes.
pub(crate) fn publish_release(
    ctx: &Ctx,
    commit: &str,
    version: &str,
    artifacts: &Path,
) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let cfg = &ext.release;
    require_config(cfg)?;
    let tag = tag::tag_of(version);
    let changelog = git(
        &ctx.root,
        &["show", &format!("{commit}:{}", cfg.changelog.output)],
    )?;
    let assets = release_assets(cfg, artifacts)?;
    let notes = notes::release(
        &notes::changelog_section(&changelog, version)?,
        &site::pages_url(&cfg.github_repo)?,
        &checksums(&assets)?,
    );
    let title = format!("{} {version}", cfg.title);
    github::publish(&cfg.github_repo, &tag, &title, &notes, &assets)?;
    gitlab::publish(cfg, &tag, &title, &notes, &assets)?;
    println!(
        "[release] https://github.com/{}/releases/tag/{tag}",
        cfg.github_repo
    );
    Ok(())
}

/// Replace the Pages site with the one release `version` presents.
pub(crate) fn publish_pages(
    ctx: &Ctx,
    version: &str,
    crates: &[String],
    artifacts: &Path,
) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let cfg = &ext.release;
    require_config(cfg)?;
    let out = tempfile::tempdir().context("creating the Pages staging directory")?;
    build_site(
        cfg,
        &ctx.config.tools,
        version,
        crates,
        artifacts,
        out.path(),
    )?;
    site::deploy(cfg, out.path(), version)?;
    println!("[pages] {}", site::pages_url(&cfg.github_repo)?);
    Ok(())
}

/// The rolling build channel. A nightly is not a version: it carries no
/// manifest checksum, reaches no crate registry, and answers one question —
/// what does the head of the default branch build into today. One tag holds
/// it, and every run replaces that tag, its release, and its package files
/// wholesale, because an asset cannot take new bytes under a name a release
/// already uses. A consumer pins the tag once and follows the branch.
pub(crate) fn publish_nightly(ctx: &Ctx, source: &str, artifacts: &Path) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let cfg = &ext.release;
    require_config(cfg)?;
    if cfg.nightly_tag.is_empty() {
        bail!("ext.release.nightly_tag is not set in .config/xtask.toml");
    }
    let tag = cfg.nightly_tag.as_str();
    let root = &ctx.root;
    let sha = git(root, &["rev-parse", &format!("{source}^{{commit}}")])?;
    let date = git(root, &["show", "-s", "--format=%cI", &sha])?;
    let subject = git(root, &["show", "-s", "--format=%s", &sha])?;
    let assets = nightly_assets(cfg, artifacts)?;
    let notes = notes::nightly(&cfg.title, &sha, &date, &subject, &checksums(&assets)?);
    let title = format!("{} nightly", cfg.title);

    github::replace_nightly(&cfg.github_repo, tag, &title, &sha, &notes, &assets)?;
    gitlab::replace_nightly(cfg, tag, &title, &sha, &notes, &assets)?;
    println!(
        "[nightly] https://github.com/{}/releases/tag/{tag}",
        cfg.github_repo
    );
    Ok(())
}

fn build_site(
    cfg: &ReleaseConfig,
    tools: &ToolsConfig,
    version: &str,
    crates: &[String],
    artifacts: &Path,
    out: &Path,
) -> Result<()> {
    let listed = checksums(&release_assets(cfg, artifacts)?)?;
    let section = site::section(cfg, version, crates, &listed);
    site::assemble(cfg, tools, artifacts, &section, out)
}

/// Every configured artifact: a release ships all of them.
fn release_assets(cfg: &ReleaseConfig, artifacts: &Path) -> Result<Vec<PathBuf>> {
    cfg.assets()
        .map(|name| {
            let path = artifacts.join(name);
            if !path.is_file() {
                bail!(
                    "the release needs {name}, which the build jobs did not leave at {}",
                    path.display()
                );
            }
            Ok(path)
        })
        .collect()
}

/// Whatever the build jobs handed over. The primary framework has to be there —
/// without it the channel publishes nothing anyone asked for — and the rest
/// ships when it was built, so one lane failing does not withhold the others.
fn nightly_assets(cfg: &ReleaseConfig, artifacts: &Path) -> Result<Vec<PathBuf>> {
    let primary = artifacts.join(&cfg.core_asset);
    if !primary.is_file() {
        bail!(
            "the nightly channel needs {}, which the release build jobs did not leave at {}",
            cfg.core_asset,
            primary.display()
        );
    }
    let mut assets = Vec::new();
    for name in cfg.assets() {
        let path = artifacts.join(name);
        if path.is_file() {
            assets.push(path);
        } else {
            println!("[nightly] {name} was not built — skipping");
        }
    }
    Ok(assets)
}

fn checksums(assets: &[PathBuf]) -> Result<Vec<(String, String)>> {
    assets
        .iter()
        .map(|asset| Ok((file_name(asset)?, sha256(asset)?)))
        .collect()
}

fn require_config(cfg: &ReleaseConfig) -> Result<()> {
    let fields = [
        ("manifest", &cfg.manifest),
        ("title", &cfg.title),
        ("github_repo", &cfg.github_repo),
        ("gitlab_host", &cfg.gitlab_host),
        ("gitlab_project", &cfg.gitlab_project),
        ("gitlab_package", &cfg.gitlab_package),
        ("core_asset", &cfg.core_asset),
        ("wasm_dist", &cfg.wasm_dist),
        ("pages_branch", &cfg.pages_branch),
        ("changelog.config", &cfg.changelog.config),
        ("changelog.output", &cfg.changelog.output),
        ("changelog.base", &cfg.changelog.base),
    ];
    for (name, value) in fields {
        if value.trim().is_empty() {
            bail!(
                "ext.release.{name} is not set; fill in the [ext.release] section of .config/xtask.toml"
            );
        }
    }
    for name in cfg.assets() {
        let path = Path::new(name);
        if path.file_name().and_then(|part| part.to_str()) != Some(name)
            || path.components().count() != 1
        {
            bail!("release artifact names must not contain path components: {name}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{config::DocsChannel, consts};

    fn config() -> ReleaseConfig {
        ReleaseConfig {
            core_asset: "primary.zip".into(),
            merged_asset: "single.zip".into(),
            platform_assets: vec!["kithara.aar".into(), consts::MACOS_APP_ARCHIVE.into()],
            docs: BTreeMap::from([(
                "apple".to_string(),
                DocsChannel {
                    asset: "docs.zip".into(),
                    ..DocsChannel::default()
                },
            )]),
            ..ReleaseConfig::default()
        }
    }

    /// The nightly publishes whatever the lanes built, but the framework the
    /// channel exists to carry is not optional.
    #[test]
    fn the_nightly_channel_ships_what_was_built() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = config();

        let error = nightly_assets(&cfg, dir.path()).expect_err("no primary artifact");
        assert!(error.to_string().contains("primary.zip"), "{error}");

        for name in ["primary.zip", "kithara.aar"] {
            fs::write(dir.path().join(name), b"x").expect("write artifact");
        }
        let assets = nightly_assets(&cfg, dir.path()).expect("primary artifact is there");
        let names: Vec<_> = assets.iter().map(|p| file_name(p).unwrap()).collect();
        assert_eq!(names, ["primary.zip", "kithara.aar"]);
    }

    /// A release is every configured artifact or nothing.
    #[test]
    fn a_release_needs_every_artifact() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = config();
        for name in ["primary.zip", "single.zip", "kithara.aar"] {
            fs::write(dir.path().join(name), b"x").expect("write artifact");
        }
        fs::write(dir.path().join(consts::MACOS_APP_ARCHIVE), b"app")
            .expect("write application artifact");

        let error = release_assets(&cfg, dir.path()).expect_err("docs.zip is missing");
        assert!(error.to_string().contains("docs.zip"), "{error}");

        fs::write(dir.path().join("docs.zip"), b"x").expect("write artifact");
        fs::remove_file(dir.path().join(consts::MACOS_APP_ARCHIVE))
            .expect("remove application artifact");
        let error = release_assets(&cfg, dir.path()).expect_err("application is missing");
        assert!(
            error.to_string().contains(consts::MACOS_APP_ARCHIVE),
            "{error}"
        );
        fs::write(dir.path().join(consts::MACOS_APP_ARCHIVE), b"app")
            .expect("write application artifact");
        assert_eq!(release_assets(&cfg, dir.path()).unwrap().len(), 5);
    }
}
