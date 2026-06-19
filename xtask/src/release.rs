use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    common::project::{ProjectConfig, ReleaseConfig},
    util::check_tool,
};

const MANIFEST: &str = "Package.swift";

/// Two-phase Apple release flow.
///
/// `prepare` runs before the release PR: it takes the built `XCFramework` zip,
/// computes the SPM checksum, stamps `Package.swift` (committed in the PR)
/// and stashes the zip in a local cache so `publish` ships the exact bytes
/// the manifest was stamped against.
///
/// `publish` runs after the PR is merged: it reads version + checksum from
/// `Package.swift` at the given ref and publishes the cached zip to GitHub
/// (tag + release + asset) and the `GitLab` mirror (tag + generic package +
/// release). Every step is idempotent, so a failed run can be retried.
#[derive(Debug, clap::Args)]
pub(crate) struct ReleaseArgs {
    #[command(subcommand)]
    command: ReleaseCommand,
}

#[derive(Debug, clap::Subcommand)]
enum ReleaseCommand {
    /// Stamp Package.swift with version + checksum and cache the zip.
    Prepare {
        /// Version to release, without the `v` prefix (e.g. 0.0.2).
        version: String,
        /// Pre-built `XCFramework` zip. When omitted, `just apple release`
        /// is run to produce /tmp/KitharaFFIInternal.xcframework.zip.
        #[arg(long)]
        zip: Option<PathBuf>,
    },
    /// Publish the prepared release to GitHub and the `GitLab` mirror.
    Publish {
        /// Git ref whose Package.swift defines the release (merge commit).
        #[arg(long, default_value = "HEAD")]
        r#ref: String,
    },
}

pub(crate) fn run(args: &ReleaseArgs) -> Result<()> {
    let project = ProjectConfig::load(Path::new("."))?;
    let cfg = &project.release;
    match &args.command {
        ReleaseCommand::Prepare { version, zip } => prepare(cfg, version, zip.as_deref()),
        ReleaseCommand::Publish { r#ref } => publish(cfg, r#ref),
    }
}

fn prepare(cfg: &ReleaseConfig, version: &str, zip: Option<&Path>) -> Result<()> {
    require_config(cfg)?;
    validate_version(version)?;

    let built;
    let zip = if let Some(path) = zip {
        path
    } else {
        check_tool("just", &["--version"], "cargo install just")?;
        println!("Building XCFramework (just apple release)...");
        run_step(
            Command::new("just").args(["apple", "release"]),
            "just apple release",
        )?;
        built = env::temp_dir().join(&cfg.asset);
        built.as_path()
    };
    if !zip.is_file() {
        bail!("zip not found: {}", zip.display());
    }

    let checksum = swift_checksum(zip)?;
    println!("Checksum: {checksum}");

    let manifest = fs::read_to_string(MANIFEST).context("read Package.swift")?;
    let stamped = stamp_manifest(&manifest, version, &checksum)?;
    fs::write(MANIFEST, stamped).context("write Package.swift")?;
    println!("Stamped {MANIFEST}: version={version}");

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
        } else {
            println!(
                "Note: {} not at {} — single-framework channel skipped (omit --zip to build it)",
                cfg.single_asset,
                built.display()
            );
        }
    }
    if !cfg.podspec.is_empty() {
        let content =
            fs::read_to_string(&cfg.podspec).with_context(|| format!("read {}", cfg.podspec))?;
        let stamped = stamp_podspec(&content, version)?;
        fs::write(&cfg.podspec, stamped).with_context(|| format!("write {}", cfg.podspec))?;
        println!("Stamped {}: version={version}", cfg.podspec);
    }

    println!();
    println!("Next steps:");
    println!("  1. Commit {MANIFEST} in the release PR.");
    println!("  2. After merge: cargo xtask release publish");
    Ok(())
}

fn publish(cfg: &ReleaseConfig, git_ref: &str) -> Result<()> {
    require_config(cfg)?;
    check_tool("gh", &["--version"], "brew install gh")?;

    let sha = git_capture(&["rev-parse", git_ref])?;
    let manifest = git_capture(&["show", &format!("{sha}:{MANIFEST}")])?;
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
                    "cached {} sha256 {actual} does not match Package.swift checksum {checksum}; \
                     re-run `cargo xtask release prepare` and release a new version",
                    zip.display()
                );
            }
            Some(zip)
        }
        false => None,
    };

    publish_github(cfg, &tag, &sha, &checksum, zip.as_deref())?;
    publish_gitlab(cfg, &tag, &sha, &checksum, zip.as_deref())?;
    publish_extras(cfg, &tag, &sha)?;

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
    checksum: &str,
    zip: Option<&Path>,
) -> Result<()> {
    let repo = &cfg.github_repo;
    let exists = Command::new("gh")
        .args(["release", "view", tag, "--repo", repo])
        .output()
        .context("run gh release view")?
        .status
        .success();

    if exists {
        println!("[github] release {tag} already exists");
    } else {
        let zip = zip_required(zip, cfg, tag)?;
        let notes = release_notes(cfg, tag, checksum);
        println!("[github] creating release {tag}...");
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
                tag,
                "--notes",
                &notes,
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
    checksum: &str,
    zip: Option<&Path>,
) -> Result<()> {
    let token = gitlab_token()?;
    let api = GitlabApi::new(cfg, &token);

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

    let pkg_url = format!(
        "packages/generic/{}/{tag}/{}",
        cfg.gitlab_package, cfg.asset
    );
    match api.package_file_sha(tag)? {
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
            let (code, body) = api.upload(&pkg_url, zip)?;
            if code != 201 {
                bail!("gitlab package upload failed (HTTP {code}): {body}");
            }
        }
    }

    let (code, _) = api.get(&format!("releases/{tag}"))?;
    match code {
        200 => println!("[gitlab] release {tag} already exists"),
        404 => {
            println!("[gitlab] creating release {tag}...");
            let link_url = format!("{}/{pkg_url}", api.project_base());
            let payload = json!({
                "tag_name": tag,
                "name": tag,
                "description": release_notes(cfg, tag, checksum),
                "assets": { "links": [{
                    "name": cfg.asset,
                    "url": link_url,
                    "link_type": "package",
                }]},
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

/// Publish the secondary channels: the single self-contained framework zip
/// (`CocoaPods` + manual drag-in) and the `CocoaPods` podspec. Both are uploaded
/// to the GitHub release and mirrored to the `GitLab` generic registry (so
/// internal builds resolve them via `KITHARA_BINARY_BASE_URL`); the podspec
/// is also pushed to the public `CocoaPods` trunk.
fn publish_extras(cfg: &ReleaseConfig, tag: &str, sha: &str) -> Result<()> {
    if !cfg.single_asset.is_empty() {
        let zip = cache_named(tag, &cfg.single_asset)?;
        if !zip.is_file() {
            bail!(
                "cached {} not found at {}; re-run `cargo xtask release prepare`",
                cfg.single_asset,
                zip.display()
            );
        }
        upload_github_asset(&cfg.github_repo, tag, &zip)?;
        upload_gitlab_asset(cfg, tag, &cfg.single_asset, &zip)?;
    }
    if !cfg.podspec.is_empty() {
        let content = git_capture(&["show", &format!("{sha}:{}", cfg.podspec)])?;
        let tmp = env::temp_dir().join(&cfg.podspec);
        fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
        upload_github_asset(&cfg.github_repo, tag, &tmp)?;
        upload_gitlab_asset(cfg, tag, &cfg.podspec, &tmp)?;
        cocoapods_push(&tmp)?;
    }
    Ok(())
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
    let token = gitlab_token()?;
    let api = GitlabApi::new(cfg, &token);
    let path = format!("packages/generic/{}/{tag}/{name}", cfg.gitlab_package);
    println!("[gitlab] uploading {name} to package registry...");
    let (code, body) = api.upload(&path, file)?;
    if code != 200 && code != 201 {
        bail!("gitlab upload of {name} failed (HTTP {code}): {body}");
    }
    Ok(())
}

/// Push the podspec to the public `CocoaPods` trunk.
fn cocoapods_push(podspec: &Path) -> Result<()> {
    check_tool("pod", &["--version"], "sudo gem install cocoapods")?;
    println!("[cocoapods] pod trunk push {}...", podspec.display());
    run_step(
        Command::new("pod")
            .args(["trunk", "push", "--allow-warnings"])
            .arg(podspec),
        "pod trunk push",
    )
}

struct GitlabApi {
    base: String,
    noproxy: String,
    token: String,
}

impl GitlabApi {
    fn new(cfg: &ReleaseConfig, token: &str) -> Self {
        Self {
            base: format!(
                "https://{}/api/v4/projects/{}",
                cfg.gitlab_host,
                cfg.gitlab_project.replace('/', "%2F")
            ),
            noproxy: cfg.gitlab_host.clone(),
            token: token.to_string(),
        }
    }

    fn project_base(&self) -> &str {
        &self.base
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

    fn upload(&self, path: &str, file: &Path) -> Result<(u16, String)> {
        self.curl(
            &[
                "--upload-file",
                &file.display().to_string(),
                "--max-time",
                "600",
            ],
            path,
        )
    }

    /// Internal hosts are not reachable through corporate/sandbox proxies,
    /// so the `GitLab` host is always taken off-proxy.
    fn curl(&self, extra: &[&str], path: &str) -> Result<(u16, String)> {
        let url = format!("{}/{path}", self.base);
        let output = Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "60",
                "--noproxy",
                &self.noproxy,
                "--header",
                &format!("PRIVATE-TOKEN: {}", self.token),
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
    fn package_file_sha(&self, tag: &str) -> Result<Option<String>> {
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
            .filter_map(|f| f["file_sha256"].as_str())
            .next_back()
            .map(str::to_string);
        Ok(sha)
    }
}

/// `GITLAB_TOKEN` env wins; otherwise reuse the local `glab` session so the
/// release flow works without extra setup on a developer machine.
fn gitlab_token() -> Result<String> {
    if let Ok(token) = env::var("GITLAB_TOKEN")
        && !token.trim().is_empty()
    {
        return Ok(token.trim().to_string());
    }
    let output = Command::new("glab")
        .args(["auth", "status", "--show-token"])
        .output()
        .context("run glab auth status (set GITLAB_TOKEN or install glab)")?;
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
        .context("no usable token from glab; set GITLAB_TOKEN or run `glab auth login`")?;
    Ok(token.to_string())
}

fn release_notes(cfg: &ReleaseConfig, tag: &str, checksum: &str) -> String {
    format!(
        "## XCFramework checksum\n\n```\n{checksum}\n```\n\n\
         ## Swift Package Manager\n\n\
         GitHub: `.package(url: \"https://github.com/{repo}\", from: \"{version}\")`\n\n\
         GitLab mirror: add the package from `https://{host}/{project}.git` and resolve with\n\n\
         ```\nKITHARA_BINARY_BASE_URL=https://{host}/api/v4/projects/{project_enc}/packages/generic/{package}\n```\n",
        repo = cfg.github_repo,
        version = tag.trim_start_matches('v'),
        host = cfg.gitlab_host,
        project = cfg.gitlab_project,
        project_enc = cfg.gitlab_project.replace('/', "%2F"),
        package = cfg.gitlab_package,
    )
}

fn require_config(cfg: &ReleaseConfig) -> Result<()> {
    let fields = [
        ("github_repo", &cfg.github_repo),
        ("gitlab_host", &cfg.gitlab_host),
        ("gitlab_project", &cfg.gitlab_project),
        ("gitlab_package", &cfg.gitlab_package),
        ("asset", &cfg.asset),
    ];
    for (name, value) in fields {
        if value.trim().is_empty() {
            bail!("release.{name} is not set; fill in the [release] section of .config/xtask.toml");
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
        .join("Library/Caches/kithara/releases")
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
        bail!("Package.swift is missing the `let version` / `let checksum` lines");
    }
    Ok(out)
}

/// Replace the podspec `s.version = '...'` line. Bails when the line is
/// missing so a refactor cannot silently ship an unstamped podspec.
fn stamp_podspec(content: &str, version: &str) -> Result<String> {
    let mut out = String::with_capacity(content.len());
    let mut seen = false;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("s.version") {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(&format!("{indent}s.version = '{version}'\n"));
            seen = true;
        } else {
            out.push_str(line);
        }
    }
    if !seen {
        bail!("podspec is missing the `s.version` line");
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
        .with_context(|| format!("`let {key} = \"...\"` not found in Package.swift"))
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
        "// header\nlet version = \"0.0.1-alpha2\"\nlet checksum = \"abc\"\nlet other = 1\n";

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
            "0.0.1-alpha2"
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
    fn stamp_podspec_replaces_version() {
        let src = "Pod::Spec.new do |s|\n  s.name = 'Kithara'\n  s.version = '0.0.1-alpha2'\nend\n";
        let out = stamp_podspec(src, "0.0.2").unwrap();
        assert!(out.contains("  s.version = '0.0.2'\n"), "{out}");
        assert!(!out.contains("0.0.1-alpha2"), "{out}");
        assert!(stamp_podspec("s.name = 'x'\n", "0.0.2").is_err());
    }

    #[test]
    fn release_notes_mention_both_hosts() {
        let cfg = ReleaseConfig {
            github_repo: "zvuk/kithara".into(),
            gitlab_host: "gitlab.zvq.me".into(),
            gitlab_project: "disrupt/kithara".into(),
            gitlab_package: "kithara".into(),
            asset: "KitharaFFIInternal.xcframework.zip".into(),
            single_asset: "Kithara.xcframework.zip".into(),
            podspec: "Kithara.podspec".into(),
        };
        let notes = release_notes(&cfg, "v0.0.2", "deadbeef");
        assert!(notes.contains("deadbeef"));
        assert!(notes.contains("github.com/zvuk/kithara"));
        assert!(notes.contains("disrupt%2Fkithara/packages/generic/kithara"));
        assert!(notes.contains("from: \"0.0.2\""));
    }
}
