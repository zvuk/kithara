use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::{Ctx, util::check_tool};
use serde_json::{Value, json};

use crate::config::{KitharaExt, ReleaseConfig};

/// Two-phase Apple release flow.
///
/// `prepare` runs before the release PR: it takes the built `XCFramework` zip,
/// computes the SPM checksum, stamps the configured Swift package manifest
/// and stashes the zip in a local cache so `publish` ships the exact bytes
/// the manifest was stamped against.
///
/// `publish` runs after the PR is merged: it reads version + checksum from the
/// configured manifest at the given ref and publishes the cached zip to GitHub
/// (tag + release + asset) and the `GitLab` mirror (tag + generic package +
/// release). Every step is idempotent, so a failed run can be retried.
#[derive(Debug, clap::Args)]
pub(crate) struct ReleaseArgs {
    #[command(subcommand)]
    command: ReleaseCommand,
}

#[derive(Debug, clap::Subcommand)]
enum ReleaseCommand {
    /// Stamp the release manifest with version + checksum and cache the zip.
    Prepare {
        /// Version to release, without the `v` prefix (e.g. 0.0.2).
        version: String,
        /// Pre-built `XCFramework` zip. When omitted, `cargo xtask apple
        /// release` is run to produce /tmp/KitharaFFIInternal.xcframework.zip.
        #[arg(long)]
        zip: Option<PathBuf>,
    },
    /// Publish the prepared release to GitHub and the `GitLab` mirror.
    Publish {
        /// Git ref whose release manifest defines the release (merge commit).
        #[arg(long, default_value = "HEAD")]
        r#ref: String,
    },
    /// Deploy the prepared wasm bundle to GitHub Pages classic (gh-pages
    /// branch, force-orphan).
    Pages {
        /// Git ref whose release manifest defines the release version.
        #[arg(long, default_value = "HEAD")]
        r#ref: String,
    },
}

pub(crate) fn run(args: &ReleaseArgs, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let cfg = &ext.release;
    match &args.command {
        ReleaseCommand::Prepare { version, zip } => prepare(cfg, version, zip.as_deref()),
        ReleaseCommand::Publish { r#ref } => publish(cfg, r#ref),
        ReleaseCommand::Pages { r#ref } => pages(cfg, r#ref),
    }
}

fn prepare(cfg: &ReleaseConfig, version: &str, zip: Option<&Path>) -> Result<()> {
    require_config(cfg)?;
    validate_version(version)?;

    let built;
    let zip = if let Some(path) = zip {
        path
    } else {
        check_tool("cargo", &["--version"], "https://rustup.rs")?;
        println!("Building XCFramework (cargo xtask apple release)...");
        run_step(
            Command::new("cargo").args(["xtask", "apple", "release"]),
            "cargo xtask apple release",
        )?;
        built = env::temp_dir().join(&cfg.asset);
        built.as_path()
    };
    if !zip.is_file() {
        bail!("zip not found: {}", zip.display());
    }

    let checksum = swift_checksum(zip)?;
    println!("Checksum: {checksum}");

    let manifest_path = cfg.manifest.as_str();
    let manifest =
        fs::read_to_string(manifest_path).with_context(|| format!("read {manifest_path}"))?;
    let stamped = stamp_manifest(&manifest, version, &checksum)?;
    fs::write(manifest_path, stamped).with_context(|| format!("write {manifest_path}"))?;
    println!("Stamped {manifest_path}: version={version}");

    let cached = cache_path(cfg, &tag_of(version))?;
    if let Some(dir) = cached.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    fs::copy(zip, &cached).with_context(|| format!("cache zip to {}", cached.display()))?;
    println!("Cached artifact: {}", cached.display());

    let tag = tag_of(version);
    if !cfg.single_asset.is_empty() {
        let built = env::temp_dir().join(&cfg.single_asset);
        if built.is_file() {
            let dest = cache_named(&tag, &cfg.single_asset)?;
            fs::copy(&built, &dest)
                .with_context(|| format!("cache {} to {}", cfg.single_asset, dest.display()))?;
            println!("Cached artifact: {}", dest.display());
            let checksum = sha256(&built)?;
            println!("Checksum {}: {}", cfg.single_asset, checksum);
        } else {
            println!(
                "Note: {} not at {} — single-framework channel skipped (omit --zip to build it)",
                cfg.single_asset,
                built.display()
            );
        }
    }

    stage_dir_asset(&tag, &cfg.docs_asset, &cfg.docs_archive, "documentation")?;
    stage_dir_asset(
        &tag,
        &cfg.wasm_asset,
        &cfg.wasm_dist,
        "wasm gh-pages bundle",
    )?;

    println!();
    println!("Next steps:");
    println!("  1. Commit {manifest_path} in the release PR.");
    println!("  2. After merge: cargo xtask release publish");
    if !cfg.pages_branch.is_empty() && !cfg.wasm_asset.is_empty() {
        println!("  3. Deploy wasm to Pages: cargo xtask release pages");
    }
    Ok(())
}

/// Zip a freshly-built artifact dir (docs archive / wasm dist) into the tagged
/// release cache. A disabled channel (empty `asset`) is a no-op; a missing
/// source dir prints a note and is skipped so a release without that artifact
/// still succeeds.
fn stage_dir_asset(tag: &str, asset: &str, source_rel: &str, label: &str) -> Result<()> {
    if asset.is_empty() {
        return Ok(());
    }
    let source = Path::new(source_rel);
    if !source.is_dir() {
        println!(
            "Note: {label} dir {source_rel} not found — {asset} channel skipped (build it first)"
        );
        return Ok(());
    }
    let parent = source
        .parent()
        .with_context(|| format!("{source_rel} has no parent dir"))?;
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{source_rel} has no final component"))?;
    let dest = cache_named(tag, asset)?;
    zip_dir(parent, name, &dest)?;
    println!("Cached artifact: {} ({label})", dest.display());
    Ok(())
}

fn zip_dir(parent: &Path, directory_name: &str, output: &Path) -> Result<()> {
    if output.exists() {
        fs::remove_file(output).with_context(|| format!("remove {}", output.display()))?;
    }
    if let Some(dir) = output.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    run_step(
        Command::new("zip")
            .args(["-r", "-y", "-q"])
            .arg(output)
            .arg(directory_name)
            .current_dir(parent),
        "zip artifact dir",
    )
}

fn publish(cfg: &ReleaseConfig, git_ref: &str) -> Result<()> {
    require_config(cfg)?;
    check_tool("gh", &["--version"], "brew install gh")?;

    let sha = git_capture(&["rev-parse", git_ref])?;
    let manifest = git_capture(&["show", &format!("{sha}:{}", cfg.manifest)])?;
    let version = manifest_field(&manifest, "version")?;
    let checksum = manifest_field(&manifest, "checksum")?;
    let tag = tag_of(&version);
    println!("Release {tag} from {git_ref} ({short})", short = &sha[..12]);

    let zip = cache_path(cfg, &tag)?;
    let zip = match zip.is_file() {
        true => {
            let actual = sha256(&zip)?;
            if actual != checksum {
                bail!(
                    "cached {} sha256 {actual} does not match {} checksum {checksum}; \
                     re-run `cargo xtask release prepare` and release a new version",
                    zip.display(),
                    cfg.manifest
                );
            }
            Some(zip)
        }
        false => None,
    };

    let notes = release_notes(cfg, &tag, &sha, &checksum)?;

    publish_github(cfg, &tag, &sha, &notes, zip.as_deref())?;
    publish_gitlab(cfg, &tag, &sha, &notes, &checksum, zip.as_deref())?;
    publish_extras(cfg, &tag)?;
    sync_gitlab_distribution_links(cfg, &tag)?;

    println!();
    println!("Release {tag} published:");
    println!(
        "  https://github.com/{}/releases/tag/{tag}",
        cfg.github_repo
    );
    println!(
        "  https://{}/{}/-/releases/{tag}",
        cfg.gitlab_host, cfg.gitlab_project
    );
    Ok(())
}

fn publish_github(
    cfg: &ReleaseConfig,
    tag: &str,
    sha: &str,
    notes: &str,
    zip: Option<&Path>,
) -> Result<()> {
    let repo = &cfg.github_repo;
    let view = Command::new("gh")
        .args(["release", "view", tag, "--repo", repo])
        .output()
        .context("run gh release view")?;
    let exists = if view.status.success() {
        true
    } else {
        let text = command_output_text(&view);
        if gh_release_missing(&text) {
            false
        } else {
            bail!("gh release view failed: {}", text.trim());
        }
    };

    if exists {
        println!("[github] release {tag} already exists");
        println!("[github] updating release notes for {tag}...");
        let title = release_title(cfg, tag);
        run_step(
            Command::new("gh").args([
                "release", "edit", tag, "--repo", repo, "--title", &title, "--notes", notes,
            ]),
            "gh release edit",
        )?;
    } else {
        let zip = zip_required(zip, cfg, tag)?;
        println!("[github] creating release {tag}...");
        let title = release_title(cfg, tag);
        run_step(
            Command::new("gh").args([
                "release",
                "create",
                tag,
                "--repo",
                repo,
                "--target",
                sha,
                "--title",
                &title,
                "--notes",
                notes,
                &zip.display().to_string(),
            ]),
            "gh release create",
        )?;
        return Ok(());
    }

    let assets = gh_json(&["api", &format!("repos/{repo}/releases/tags/{tag}")])?;
    let has_asset = assets["assets"]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x["name"].as_str() == Some(&cfg.asset)));
    if has_asset {
        println!("[github] asset {} already uploaded", cfg.asset);
    } else {
        let zip = zip_required(zip, cfg, tag)?;
        println!("[github] uploading {}...", cfg.asset);
        run_step(
            Command::new("gh").args([
                "release",
                "upload",
                tag,
                "--repo",
                repo,
                &zip.display().to_string(),
            ]),
            "gh release upload",
        )?;
    }
    Ok(())
}

fn publish_gitlab(
    cfg: &ReleaseConfig,
    tag: &str,
    sha: &str,
    notes: &str,
    checksum: &str,
    zip: Option<&Path>,
) -> Result<()> {
    let token = gitlab_token(&cfg.gitlab_host)?;
    let api = GitlabApi::new(cfg, &token)?;

    let (code, _) = api.get(&format!("repository/tags/{tag}"))?;
    match code {
        200 => println!("[gitlab] tag {tag} already exists"),
        404 => {
            println!(
                "[gitlab] creating tag {tag} at {short}...",
                short = &sha[..12]
            );
            let (code, body) =
                api.post(&format!("repository/tags?tag_name={tag}&ref={sha}"), None)?;
            if code != 201 {
                bail!(
                    "gitlab tag create failed (HTTP {code}): {body}\n\
                     Make sure the merge commit is pushed to GitLab first."
                );
            }
        }
        other => bail!("gitlab tag lookup failed (HTTP {other})"),
    }

    let pkg_path = gitlab_package_path(cfg, tag, &cfg.asset);
    match api.package_file_sha(tag, &cfg.asset)? {
        Some(sha256) if sha256 == checksum => {
            println!("[gitlab] package {tag} already in registry");
        }
        Some(sha256) => bail!(
            "registry file for {tag} has sha256 {sha256}, expected {checksum}; \
             delete the broken package in GitLab and re-run"
        ),
        None => {
            let zip = zip_required(zip, cfg, tag)?;
            println!("[gitlab] uploading {} to package registry...", cfg.asset);
            let (code, body) = api.upload(&pkg_path, zip)?;
            if code != 201 {
                bail!("gitlab package upload failed (HTTP {code}): {body}");
            }
        }
    }

    let links = vec![gitlab_release_link(cfg, tag, &cfg.asset)];
    let (code, _) = api.get(&format!("releases/{tag}"))?;
    match code {
        200 => {
            println!("[gitlab] release {tag} already exists");
            println!("[gitlab] updating release notes for {tag}...");
            let payload = json!({
                "name": release_title(cfg, tag),
                "description": notes,
            });
            let (code, body) = api.put(&format!("releases/{tag}"), Some(&payload.to_string()))?;
            if code != 200 {
                bail!("gitlab release update failed (HTTP {code}): {body}");
            }
            sync_gitlab_release_links(&api, tag, &links)?;
        }
        404 => {
            println!("[gitlab] creating release {tag}...");
            let payload = json!({
                "tag_name": tag,
                "name": release_title(cfg, tag),
                "description": notes,
                "assets": { "links": links.iter().map(GitlabReleaseLink::as_json).collect::<Vec<_>>() },
            });
            let (code, body) = api.post("releases", Some(&payload.to_string()))?;
            if code != 201 {
                bail!("gitlab release create failed (HTTP {code}): {body}");
            }
        }
        other => bail!("gitlab release lookup failed (HTTP {other})"),
    }
    Ok(())
}

/// Publish the secondary cached assets — the single self-contained framework
/// and the documentation archive — to the GitHub release and the `GitLab`
/// generic registry so manual integrations download the same bytes.
fn publish_extras(cfg: &ReleaseConfig, tag: &str) -> Result<()> {
    upload_extra_asset(cfg, tag, &cfg.single_asset, true)?;
    upload_extra_asset(cfg, tag, &cfg.docs_asset, false)?;
    Ok(())
}

/// Upload one optional cached asset to GitHub + `GitLab`. `required` bails when
/// the cached zip is missing (the single framework channel); otherwise a
/// missing artifact (docs) is skipped with a note so the core release still
/// publishes.
fn upload_extra_asset(cfg: &ReleaseConfig, tag: &str, name: &str, required: bool) -> Result<()> {
    if name.is_empty() {
        return Ok(());
    }
    let zip = cache_named(tag, name)?;
    if !zip.is_file() {
        if required {
            bail!(
                "cached {name} not found at {}; re-run `cargo xtask release prepare`",
                zip.display()
            );
        }
        println!("[release] {name} not cached — skipping (run prepare to include it)");
        return Ok(());
    }
    upload_github_asset(&cfg.github_repo, tag, &zip)?;
    upload_gitlab_asset(cfg, tag, name, &zip)?;
    Ok(())
}

/// Deploy the prepared wasm bundle to GitHub Pages classic: force-orphan a
/// single commit onto the configured branch and push it. Idempotent — every
/// deploy replaces the branch contents wholesale.
fn pages(cfg: &ReleaseConfig, git_ref: &str) -> Result<()> {
    if cfg.pages_branch.is_empty() || cfg.wasm_asset.is_empty() {
        bail!("release.pages_branch / release.wasm_asset must be set in .config/xtask.toml");
    }
    check_tool("git", &["--version"], "https://git-scm.com")?;
    check_tool("unzip", &["-v"], "unzip is part of the base system")?;

    let sha = git_capture(&["rev-parse", git_ref])?;
    let manifest = git_capture(&["show", &format!("{sha}:{}", cfg.manifest)])?;
    let version = manifest_field(&manifest, "version")?;
    let tag = tag_of(&version);

    let zip = cache_named(&tag, &cfg.wasm_asset)?;
    if !zip.is_file() {
        bail!(
            "cached {} not found at {}; run `just release-artifacts {version}` first",
            cfg.wasm_asset,
            zip.display()
        );
    }

    let staging = env::temp_dir().join(format!(
        "{}-pages-{tag}",
        kithara_devtools::util::project_name()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging).with_context(|| format!("clean {}", staging.display()))?;
    }
    fs::create_dir_all(&staging).with_context(|| format!("create {}", staging.display()))?;
    run_step(
        Command::new("unzip")
            .arg("-q")
            .arg(&zip)
            .arg("-d")
            .arg(&staging),
        "unzip wasm bundle",
    )?;

    deploy_pages_branch(&cfg.pages_branch, &single_subdir(&staging)?, &tag)?;
    println!();
    println!("Pages deployed to branch {} ({tag}).", cfg.pages_branch);
    Ok(())
}

/// The bundle zip contains the `dist` dir as its single top-level entry; deploy
/// that dir's contents at the branch root. Fall back to the staging dir itself.
fn single_subdir(dir: &Path) -> Result<PathBuf> {
    let mut subdirs = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    match subdirs.len() {
        1 => Ok(subdirs.remove(0)),
        _ => Ok(dir.to_path_buf()),
    }
}

fn deploy_pages_branch(branch: &str, content: &Path, tag: &str) -> Result<()> {
    let worktree = env::temp_dir().join(format!(
        "{}-{branch}-worktree",
        kithara_devtools::util::project_name()
    ));
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree)
        .status();
    if worktree.exists() {
        fs::remove_dir_all(&worktree).with_context(|| format!("clean {}", worktree.display()))?;
    }

    run_step(
        Command::new("git")
            .args(["worktree", "add", "--force", "--detach"])
            .arg(&worktree),
        "git worktree add",
    )?;
    let result = deploy_into_worktree(&worktree, branch, content, tag);
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree)
        .status();
    result
}

fn deploy_into_worktree(worktree: &Path, branch: &str, content: &Path, tag: &str) -> Result<()> {
    run_step(
        Command::new("git")
            .current_dir(worktree)
            .args(["switch", "--orphan", branch]),
        "git switch --orphan",
    )?;
    clear_worktree(worktree)?;
    copy_dir(content, worktree)?;
    fs::write(worktree.join(".nojekyll"), "").context("write .nojekyll")?;
    run_step(
        Command::new("git")
            .current_dir(worktree)
            .args(["add", "-A"]),
        "git add",
    )?;
    run_step(
        Command::new("git").current_dir(worktree).args([
            "commit",
            "-m",
            &format!("deploy: wasm Pages for {tag}"),
        ]),
        "git commit",
    )?;
    run_step(
        Command::new("git")
            .current_dir(worktree)
            .args(["push", "--force", "origin", branch]),
        "git push gh-pages",
    )
}

/// Remove every entry except the worktree's `.git` link.
fn clear_worktree(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.file_name().and_then(|n| n.to_str()) == Some(".git") {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path).with_context(|| format!("remove {}", path.display()))?;
        } else {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            fs::create_dir_all(&to).with_context(|| format!("create {}", to.display()))?;
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn sync_gitlab_distribution_links(cfg: &ReleaseConfig, tag: &str) -> Result<()> {
    let token = gitlab_token(&cfg.gitlab_host)?;
    let api = GitlabApi::new(cfg, &token)?;
    let links = gitlab_release_links(cfg, tag);
    sync_gitlab_release_links(&api, tag, &links)
}

/// Upload (or replace) one asset on an existing GitHub release.
fn upload_github_asset(repo: &str, tag: &str, file: &Path) -> Result<()> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    println!("[github] uploading {name}...");
    run_step(
        Command::new("gh")
            .args(["release", "upload", tag, "--repo", repo, "--clobber"])
            .arg(file),
        "gh release upload",
    )
}

/// Mirror one asset to the `GitLab` generic package registry under the tag.
fn upload_gitlab_asset(cfg: &ReleaseConfig, tag: &str, name: &str, file: &Path) -> Result<()> {
    let token = gitlab_token(&cfg.gitlab_host)?;
    let api = GitlabApi::new(cfg, &token)?;
    let actual_sha = sha256(file)?;
    match api.package_file_sha(tag, name)? {
        Some(existing_sha) if existing_sha == actual_sha => {
            println!("[gitlab] package asset {name} already uploaded");
            return Ok(());
        }
        Some(existing_sha) => bail!(
            "gitlab package asset {name} has sha256 {existing_sha}, expected {actual_sha}; \
             delete the broken package file and re-run"
        ),
        None => {}
    }

    let path = gitlab_package_path(cfg, tag, name);
    println!("[gitlab] uploading {name} to package registry...");
    let (code, body) = api.upload(&path, file)?;
    if code != 200 && code != 201 {
        bail!("gitlab upload of {name} failed (HTTP {code}): {body}");
    }
    Ok(())
}

fn resolve_http_timeout_secs(cfg: &ReleaseConfig) -> Result<u64> {
    cfg.http_timeout_secs.context(
        "ext.release.http_timeout_secs is not set; fill in the [ext.release] section of .config/xtask.toml",
    )
}

fn resolve_upload_timeout_secs(cfg: &ReleaseConfig) -> Result<u64> {
    cfg.upload_timeout_secs.context(
        "ext.release.upload_timeout_secs is not set; fill in the [ext.release] section of .config/xtask.toml",
    )
}

struct GitlabApi {
    base: String,
    noproxy: String,
    token: String,
    http_timeout_secs: u64,
    upload_timeout_secs: u64,
}

impl GitlabApi {
    fn new(cfg: &ReleaseConfig, token: &str) -> Result<Self> {
        Ok(Self {
            base: format!(
                "https://{}/api/v4/projects/{}",
                cfg.gitlab_host,
                cfg.gitlab_project.replace('/', "%2F")
            ),
            noproxy: cfg.gitlab_host.clone(),
            token: token.to_string(),
            http_timeout_secs: resolve_http_timeout_secs(cfg)?,
            upload_timeout_secs: resolve_upload_timeout_secs(cfg)?,
        })
    }

    fn get(&self, path: &str) -> Result<(u16, String)> {
        self.curl(&[], path)
    }

    fn post(&self, path: &str, body: Option<&str>) -> Result<(u16, String)> {
        let mut extra = vec!["--request", "POST"];
        if let Some(json) = body {
            extra.extend(["--header", "Content-Type: application/json", "--data", json]);
        }
        self.curl(&extra, path)
    }

    fn put(&self, path: &str, body: Option<&str>) -> Result<(u16, String)> {
        let mut extra = vec!["--request", "PUT"];
        if let Some(json) = body {
            extra.extend(["--header", "Content-Type: application/json", "--data", json]);
        }
        self.curl(&extra, path)
    }

    fn upload(&self, path: &str, file: &Path) -> Result<(u16, String)> {
        let file_arg = file.display().to_string();
        let timeout = self.upload_timeout_secs.to_string();
        self.curl(&["--upload-file", &file_arg, "--max-time", &timeout], path)
    }

    /// Internal hosts are not reachable through corporate/sandbox proxies,
    /// so the `GitLab` host is always taken off-proxy.
    fn curl(&self, extra: &[&str], path: &str) -> Result<(u16, String)> {
        let url = format!("{}/{path}", self.base);
        let timeout = self.http_timeout_secs.to_string();
        let private_token = format!("PRIVATE-TOKEN: {}", self.token);
        let output = Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                &timeout,
                "--noproxy",
                &self.noproxy,
                "--header",
                &private_token,
                "-w",
                "\n%{http_code}",
            ])
            .args(extra)
            .arg(&url)
            .output()
            .with_context(|| format!("run curl {url}"))?;
        if !output.status.success() {
            bail!(
                "curl {url} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let (body, code) = stdout
            .rsplit_once('\n')
            .with_context(|| format!("curl {url}: missing status line"))?;
        let code: u16 = code.trim().parse().context("parse http status")?;
        Ok((code, body.to_string()))
    }

    /// sha256 of the asset file in the generic registry for `tag`,
    /// or None when the package or file does not exist yet.
    fn package_file_sha(&self, tag: &str, name: &str) -> Result<Option<String>> {
        let (code, body) = self.get(&format!("packages?package_version={tag}"))?;
        if code != 200 {
            bail!("gitlab package lookup failed (HTTP {code}): {body}");
        }
        let packages: Value = serde_json::from_str(&body).context("parse packages json")?;
        let Some(pkg_id) = packages
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["version"].as_str() == Some(tag))
            .and_then(|p| p["id"].as_i64())
        else {
            return Ok(None);
        };

        let (code, body) = self.get(&format!("packages/{pkg_id}/package_files"))?;
        if code != 200 {
            bail!("gitlab package_files lookup failed (HTTP {code}): {body}");
        }
        let files: Value = serde_json::from_str(&body).context("parse package_files json")?;
        let sha = files
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| f["file_name"].as_str() == Some(name))
            .filter_map(|f| f["file_sha256"].as_str())
            .next_back()
            .map(str::to_string);
        Ok(sha)
    }
}

#[derive(Debug)]
struct GitlabReleaseLink {
    name: String,
    url: String,
}

impl GitlabReleaseLink {
    fn as_json(&self) -> Value {
        json!({
            "name": &self.name,
            "url": &self.url,
            "link_type": "package",
        })
    }
}

fn gitlab_release_links(cfg: &ReleaseConfig, tag: &str) -> Vec<GitlabReleaseLink> {
    let mut links = vec![gitlab_release_link(cfg, tag, &cfg.asset)];
    if !cfg.single_asset.is_empty() {
        links.push(gitlab_release_link(cfg, tag, &cfg.single_asset));
    }
    links
}

fn gitlab_release_link(cfg: &ReleaseConfig, tag: &str, name: &str) -> GitlabReleaseLink {
    GitlabReleaseLink {
        name: name.to_string(),
        url: gitlab_package_url(cfg, tag, name),
    }
}

fn sync_gitlab_release_links(
    api: &GitlabApi,
    tag: &str,
    links: &[GitlabReleaseLink],
) -> Result<()> {
    let (code, body) = api.get(&format!("releases/{tag}/assets/links"))?;
    if code != 200 {
        bail!("gitlab release links lookup failed (HTTP {code}): {body}");
    }
    let existing: Value = serde_json::from_str(&body).context("parse release links json")?;
    for link in links {
        let existing_link = existing
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["name"].as_str() == Some(link.name.as_str()));
        match existing_link {
            Some(item) if item["url"].as_str() == Some(link.url.as_str()) => {
                println!(
                    "[gitlab] release link {} already points to package",
                    link.name
                );
            }
            Some(item) => {
                let id = item["id"]
                    .as_i64()
                    .with_context(|| format!("gitlab release link {} missing id", link.name))?;
                println!("[gitlab] updating release link {}...", link.name);
                let (code, body) = api.put(
                    &format!("releases/{tag}/assets/links/{id}"),
                    Some(&link.as_json().to_string()),
                )?;
                if code != 200 {
                    bail!(
                        "gitlab release link update for {} failed (HTTP {code}): {body}",
                        link.name
                    );
                }
            }
            None => {
                println!("[gitlab] adding release link {}...", link.name);
                let (code, body) = api.post(
                    &format!("releases/{tag}/assets/links"),
                    Some(&link.as_json().to_string()),
                )?;
                if code != 201 {
                    bail!(
                        "gitlab release link create for {} failed (HTTP {code}): {body}",
                        link.name
                    );
                }
            }
        }
    }
    Ok(())
}

fn gitlab_package_path(cfg: &ReleaseConfig, tag: &str, name: &str) -> String {
    format!("packages/generic/{}/{tag}/{name}", cfg.gitlab_package)
}

fn gitlab_package_url(cfg: &ReleaseConfig, tag: &str, name: &str) -> String {
    format!(
        "https://{}/api/v4/projects/{}/{}",
        cfg.gitlab_host,
        cfg.gitlab_project.replace('/', "%2F"),
        gitlab_package_path(cfg, tag, name)
    )
}

/// `GITLAB_TOKEN` env wins; otherwise reuse the local `glab` session so the
/// release flow works without extra setup on a developer machine.
fn gitlab_token(host: &str) -> Result<String> {
    if let Ok(token) = env::var("GITLAB_TOKEN")
        && !token.trim().is_empty()
    {
        return Ok(token.trim().to_string());
    }
    let output = Command::new("glab")
        .args(["auth", "status", "--hostname", host, "--show-token"])
        .output()
        .with_context(|| {
            format!("run glab auth status for {host} (set GITLAB_TOKEN or install glab)")
        })?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let token = text
        .lines()
        .find_map(|line| line.split("Token found: ").nth(1))
        .map(str::trim)
        .filter(|t| !t.is_empty() && !t.starts_with('*'))
        .with_context(|| {
            format!(
                "no usable token from glab for {host}; set GITLAB_TOKEN or run `glab auth login`"
            )
        })?;
    Ok(token.to_string())
}

fn release_notes(cfg: &ReleaseConfig, tag: &str, sha: &str, checksum: &str) -> Result<String> {
    let version = tag.trim_start_matches('v');
    let body = changelog_section(sha, version)
        .or_else(|| generated_release_summary(sha, tag).ok())
        .unwrap_or_else(|| {
            "This release contains the changes merged since the previous published tag.".to_string()
        });
    let single_checksum = if !cfg.single_asset.is_empty() {
        let single = cache_named(tag, &cfg.single_asset)?;
        if single.is_file() {
            Some(sha256(&single)?)
        } else {
            None
        }
    } else {
        None
    };
    Ok(render_release_notes(
        cfg,
        tag,
        body.trim(),
        checksum,
        single_checksum.as_deref(),
    ))
}

fn render_release_notes(
    cfg: &ReleaseConfig,
    tag: &str,
    body: &str,
    checksum: &str,
    single_checksum: Option<&str>,
) -> String {
    let mut notes = format!("## {}\n\n{}", release_title(cfg, tag), body.trim());
    notes.push_str("\n\n## Artifacts\n\n");
    notes.push_str(&format!("- `{}` for Swift Package Manager.\n", cfg.asset));
    if !cfg.single_asset.is_empty() {
        notes.push_str(&format!(
            "- `{}` for manual Apple integration.\n",
            cfg.single_asset
        ));
    }
    notes.push_str("- Rust crates are versioned and published in dependency order.\n");

    notes.push_str("\n## Checksums\n\n```text\n");
    notes.push_str(&format!("{}\n{}\n", cfg.asset, checksum));
    if let Some(single_checksum) = single_checksum {
        notes.push_str(&format!("\n{}\n{}\n", cfg.single_asset, single_checksum));
    }
    notes.push_str("```\n");

    notes
}

fn release_title(cfg: &ReleaseConfig, tag: &str) -> String {
    format!("{} {}", cfg.title, tag.trim_start_matches('v'))
}

fn changelog_section(sha: &str, version: &str) -> Option<String> {
    let Ok(changelog) = git_capture(&["show", &format!("{sha}:CHANGELOG.md")]) else {
        return None;
    };
    let headings = [
        format!("## [{version}]"),
        format!("## [v{version}]"),
        format!("## {version}"),
        format!("## v{version}"),
    ];
    let mut out = Vec::new();
    let mut in_section = false;
    for line in changelog.lines() {
        if line.starts_with("## ") {
            if in_section {
                break;
            }
            in_section = headings.iter().any(|heading| line.starts_with(heading));
            continue;
        }
        if in_section {
            out.push(line);
        }
    }
    let section = out.join("\n").trim().to_string();
    (!section.is_empty()).then_some(section)
}

fn generated_release_summary(sha: &str, tag: &str) -> Result<String> {
    let previous = previous_release_tag(sha, tag).ok();
    let range = previous
        .as_ref()
        .map_or_else(|| sha.to_string(), |prev| format!("{prev}..{sha}"));
    let log = git_capture(&["log", "--first-parent", "--pretty=%s", &range])?;
    let subjects: Vec<_> = log
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("fix(release):"))
        .map(normalize_release_subject)
        .collect();
    if subjects.is_empty() {
        return Ok("This release contains packaging and release maintenance updates.".to_string());
    }

    let mut out = String::from(
        "This release includes the main product and SDK changes merged since the previous published tag.\n\n### Highlights\n",
    );
    for subject in subjects {
        out.push_str("- ");
        out.push_str(&subject);
        out.push('\n');
    }
    Ok(out)
}

fn previous_release_tag(sha: &str, tag: &str) -> Result<String> {
    git_capture(&["describe", "--tags", "--abbrev=0", &format!("{sha}^")]).and_then(|prev| {
        if prev == tag {
            bail!("previous tag resolved to current release tag");
        }
        Ok(prev)
    })
}

fn normalize_release_subject(subject: &str) -> String {
    subject
        .strip_prefix("feat:")
        .or_else(|| subject.strip_prefix("fix:"))
        .or_else(|| subject.strip_prefix("refactor:"))
        .or_else(|| subject.strip_prefix("chore:"))
        .map_or(subject, str::trim)
        .to_string()
}

fn gh_release_missing(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("release not found")
        || (text.contains("not found") && text.contains("404"))
        || text.contains("http 404")
}

fn command_output_text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn require_config(cfg: &ReleaseConfig) -> Result<()> {
    let fields = [
        ("manifest", &cfg.manifest),
        ("title", &cfg.title),
        ("github_repo", &cfg.github_repo),
        ("gitlab_host", &cfg.gitlab_host),
        ("gitlab_project", &cfg.gitlab_project),
        ("gitlab_package", &cfg.gitlab_package),
        ("asset", &cfg.asset),
    ];
    for (name, value) in fields {
        if value.trim().is_empty() {
            bail!(
                "ext.release.{name} is not set; fill in the [ext.release] section of .config/xtask.toml"
            );
        }
    }
    Ok(())
}

fn validate_version(version: &str) -> Result<()> {
    let ok = regex::Regex::new(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$")
        .context("compile version regex")?
        .is_match(version);
    if !ok {
        bail!("invalid version {version:?}; expected e.g. 0.0.2 or 0.0.2-alpha1 (no v prefix)");
    }
    Ok(())
}

fn tag_of(version: &str) -> String {
    format!("v{version}")
}

fn cache_path(cfg: &ReleaseConfig, tag: &str) -> Result<PathBuf> {
    cache_named(tag, &cfg.asset)
}

/// Cache path for an arbitrary release artifact (per tag + file name).
fn cache_named(tag: &str, name: &str) -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/Caches")
        .join(kithara_devtools::util::project_name())
        .join("releases")
        .join(tag)
        .join(name))
}

fn zip_required<'a>(zip: Option<&'a Path>, cfg: &ReleaseConfig, tag: &str) -> Result<&'a Path> {
    zip.with_context(|| {
        format!(
            "artifact for {tag} not found at {}; run `cargo xtask release prepare` \
             on the machine that built the release zip",
            cache_path(cfg, tag).map_or_else(|_| cfg.asset.clone(), |p| p.display().to_string()),
        )
    })
}

/// Replace the `let version = "..."` / `let checksum = "..."` lines that the
/// SPM binary target reads. Bails when either line is missing so a manifest
/// refactor cannot silently produce an unstamped release.
fn stamp_manifest(manifest: &str, version: &str, checksum: &str) -> Result<String> {
    let mut out = String::with_capacity(manifest.len());
    let mut seen_version = false;
    let mut seen_checksum = false;
    for line in manifest.split_inclusive('\n') {
        if line.starts_with("let version = ") {
            out.push_str(&format!("let version = \"{version}\"\n"));
            seen_version = true;
        } else if line.starts_with("let checksum = ") {
            out.push_str(&format!("let checksum = \"{checksum}\"\n"));
            seen_checksum = true;
        } else {
            out.push_str(line);
        }
    }
    if !seen_version || !seen_checksum {
        bail!("release manifest is missing the `let version` / `let checksum` lines");
    }
    Ok(out)
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

fn swift_checksum(zip: &Path) -> Result<String> {
    let output = Command::new("swift")
        .args(["package", "compute-checksum"])
        .arg(zip)
        .output()
        .context("run swift package compute-checksum")?;
    if !output.status.success() {
        bail!(
            "swift package compute-checksum failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn sha256(path: &Path) -> Result<String> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .context("run shasum -a 256")?;
    if !output.status.success() {
        bail!("shasum failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .next()
        .map(str::to_string)
        .context("empty shasum output")
}

fn git_capture(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("run git {args:?}"))?;
    if !output.status.success() {
        bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn gh_json(args: &[&str]) -> Result<Value> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .with_context(|| format!("run gh {args:?}"))?;
    if !output.status.success() {
        bail!(
            "gh {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("parse gh json output")
}

fn run_step(cmd: &mut Command, description: &str) -> Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("failed to run {description}"))?;
    if !status.success() {
        bail!(
            "{description} failed (exit code: {})",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST_SAMPLE: &str =
        "// header\nlet version = \"0.0.1-alpha3\"\nlet checksum = \"abc\"\nlet other = 1\n";

    #[test]
    fn stamp_replaces_version_and_checksum() {
        let out = stamp_manifest(MANIFEST_SAMPLE, "0.0.2", "deadbeef").unwrap();
        assert!(out.contains("let version = \"0.0.2\"\n"));
        assert!(out.contains("let checksum = \"deadbeef\"\n"));
        assert!(out.contains("// header\n"));
        assert!(out.contains("let other = 1\n"));
    }

    #[test]
    fn stamp_rejects_manifest_without_fields() {
        let err = stamp_manifest("let other = 1\n", "0.0.2", "deadbeef").unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");
    }

    #[test]
    fn manifest_field_reads_values() {
        assert_eq!(
            manifest_field(MANIFEST_SAMPLE, "version").unwrap(),
            "0.0.1-alpha3"
        );
        assert_eq!(manifest_field(MANIFEST_SAMPLE, "checksum").unwrap(), "abc");
        assert!(manifest_field(MANIFEST_SAMPLE, "missing").is_err());
    }

    #[test]
    fn version_validation() {
        assert!(validate_version("0.0.2").is_ok());
        assert!(validate_version("1.2.3-alpha1").is_ok());
        assert!(validate_version("v0.0.2").is_err());
        assert!(validate_version("0.0").is_err());
        assert!(validate_version("0.0.2 ; rm -rf").is_err());
    }

    #[test]
    fn release_notes_render_release_content() {
        let cfg = ReleaseConfig {
            manifest: "Package.swift".into(),
            title: "Kithara".into(),
            github_repo: "zvuk/kithara".into(),
            gitlab_host: "gitlab.zvq.me".into(),
            gitlab_project: "disrupt/kithara".into(),
            gitlab_package: "kithara".into(),
            asset: "KitharaFFIInternal.xcframework.zip".into(),
            single_asset: "Kithara.xcframework.zip".into(),
            docs_asset: "Kithara-docs.zip".into(),
            docs_archive: "docs-build/Kithara.doccarchive".into(),
            wasm_asset: "kithara-wasm-pages.zip".into(),
            wasm_dist: "crates/kithara-ffi/dist".into(),
            pages_branch: "gh-pages".into(),
            http_timeout_secs: Some(60),
            upload_timeout_secs: Some(600),
        };
        let notes = render_release_notes(
            &cfg,
            "v0.0.2",
            "### Highlights\n- Better release notes.",
            "internal-sha",
            Some("single-sha"),
        );
        assert!(notes.contains("## Kithara 0.0.2"), "{notes}");
        assert!(notes.contains("Better release notes"), "{notes}");
        assert!(
            notes.contains("KitharaFFIInternal.xcframework.zip\ninternal-sha"),
            "{notes}"
        );
        assert!(
            notes.contains("Kithara.xcframework.zip\nsingle-sha"),
            "{notes}"
        );
        assert!(notes.contains("manual Apple integration"), "{notes}");
        assert!(!notes.contains("CocoaPods"), "{notes}");
        assert!(!notes.contains("Kithara.podspec"), "{notes}");
        assert!(!notes.contains("gitlab.zvq.me"), "{notes}");
    }

    #[test]
    fn gitlab_release_links_include_active_distribution_assets() {
        let cfg = ReleaseConfig {
            manifest: "Package.swift".into(),
            title: "Kithara".into(),
            github_repo: "zvuk/kithara".into(),
            gitlab_host: "gitlab.zvq.me".into(),
            gitlab_project: "disrupt/kithara".into(),
            gitlab_package: "kithara".into(),
            asset: "KitharaFFIInternal.xcframework.zip".into(),
            single_asset: "Kithara.xcframework.zip".into(),
            docs_asset: "Kithara-docs.zip".into(),
            docs_archive: "docs-build/Kithara.doccarchive".into(),
            wasm_asset: "kithara-wasm-pages.zip".into(),
            wasm_dist: "crates/kithara-ffi/dist".into(),
            pages_branch: "gh-pages".into(),
            http_timeout_secs: Some(60),
            upload_timeout_secs: Some(600),
        };

        let links = gitlab_release_links(&cfg, "v0.0.2");
        let names: Vec<_> = links.iter().map(|link| link.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "KitharaFFIInternal.xcframework.zip",
                "Kithara.xcframework.zip",
            ]
        );
        assert!(
            links.iter().all(|link| link.url.contains(
                "https://gitlab.zvq.me/api/v4/projects/disrupt%2Fkithara/packages/generic/kithara/v0.0.2/"
            )),
            "{links:#?}"
        );
    }

    #[test]
    fn gh_release_missing_does_not_hide_auth_failures() {
        assert!(gh_release_missing("HTTP 404: release not found"));
        assert!(!gh_release_missing(
            "Get https://api.github.com/repos/zvuk/kithara/releases/tags/v0.0.1-alpha3: Forbidden"
        ));
        assert!(!gh_release_missing(
            "The token in keyring is invalid. To re-authenticate, run: gh auth refresh"
        ));
    }

    #[test]
    fn release_http_timeout_uses_config() {
        let cfg = ReleaseConfig {
            manifest: String::new(),
            title: String::new(),
            github_repo: String::new(),
            gitlab_host: String::new(),
            gitlab_project: String::new(),
            gitlab_package: String::new(),
            asset: String::new(),
            single_asset: String::new(),
            docs_asset: String::new(),
            docs_archive: String::new(),
            wasm_asset: String::new(),
            wasm_dist: String::new(),
            pages_branch: String::new(),
            http_timeout_secs: Some(60),
            upload_timeout_secs: Some(600),
        };

        assert_eq!(resolve_http_timeout_secs(&cfg).unwrap(), 60);
        assert_eq!(resolve_upload_timeout_secs(&cfg).unwrap(), 600);
    }

    #[test]
    fn release_http_timeout_requires_config() {
        let cfg = ReleaseConfig {
            manifest: String::new(),
            title: String::new(),
            github_repo: String::new(),
            gitlab_host: String::new(),
            gitlab_project: String::new(),
            gitlab_package: String::new(),
            asset: String::new(),
            single_asset: String::new(),
            docs_asset: String::new(),
            docs_archive: String::new(),
            wasm_asset: String::new(),
            wasm_dist: String::new(),
            pages_branch: String::new(),
            http_timeout_secs: None,
            upload_timeout_secs: Some(600),
        };

        let error = resolve_http_timeout_secs(&cfg).unwrap_err();
        assert!(error.to_string().contains("http_timeout_secs"), "{error}");
    }

    #[test]
    fn release_upload_timeout_requires_config() {
        let cfg = ReleaseConfig {
            manifest: String::new(),
            title: String::new(),
            github_repo: String::new(),
            gitlab_host: String::new(),
            gitlab_project: String::new(),
            gitlab_package: String::new(),
            asset: String::new(),
            single_asset: String::new(),
            docs_asset: String::new(),
            docs_archive: String::new(),
            wasm_asset: String::new(),
            wasm_dist: String::new(),
            pages_branch: String::new(),
            http_timeout_secs: Some(60),
            upload_timeout_secs: None,
        };

        let error = resolve_upload_timeout_secs(&cfg).unwrap_err();
        assert!(error.to_string().contains("upload_timeout_secs"), "{error}");
    }
}
