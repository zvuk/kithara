use std::{fmt::Write as _, path::Path};

use anyhow::{Context, Result, bail};
use kithara_devtools::common::tools::ToolsConfig;

use super::{
    artifact::sha256,
    changelog,
    git::{Remote, command, committer, git, output, token},
    github, gitlab,
};
use crate::config::ReleaseConfig;

/// The tag a version is released under.
pub(super) fn tag_of(version: &str) -> String {
    format!("v{version}")
}

pub(super) fn validate_version(version: &str) -> Result<()> {
    let ok = regex::Regex::new(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$")
        .context("compile version regex")?
        .is_match(version);
    if !ok {
        bail!("invalid version {version:?}; expected e.g. 0.0.2 or 0.0.2-alpha1 (no v prefix)");
    }
    Ok(())
}

/// Tag `version` on a commit that stamps the Swift manifest with the built
/// framework's checksum and renders the changelog on top of `source`, the
/// commit the release jobs built, and answer with that commit. No branch
/// moves: the tag alone reaches the stamp commit. A re-run finds the tag it
/// made and pushes it wherever it is missing. A tag another build made is
/// replaced, releases and all, while `registered` — the release's crates
/// crates.io already holds at `version` — is empty, and stops the release once
/// it is not: the registry keeps what it took, so that release is final.
pub(super) fn ensure(
    cfg: &ReleaseConfig,
    tools: &ToolsConfig,
    root: &Path,
    source: &str,
    version: &str,
    artifacts: &Path,
    registered: impl FnOnce() -> Result<Vec<String>>,
) -> Result<String> {
    validate_version(version)?;
    let tag = tag_of(version);
    let checksum = sha256(&artifacts.join(&cfg.core_asset))?;
    let remotes = [
        Remote::github(&cfg.github_repo, Some(&token("GH_TOKEN")?))?,
        Remote::gitlab(
            &cfg.gitlab_host,
            &cfg.gitlab_project,
            &token("GITLAB_TOKEN")?,
        ),
    ];
    remotes[0].fetch_tags(root)?;

    let mut found = Vec::with_capacity(remotes.len());
    for remote in &remotes {
        let listing = output(
            remote.git(root)?.args([
                "ls-remote",
                &remote.url,
                &format!("refs/tags/{tag}"),
                &format!("refs/tags/{tag}^{{}}"),
            ]),
            None,
        )?;
        found.push(tag_commit(&listing, &tag));
    }

    let existing = match agreed(&tag, &remotes, &found)? {
        Some(commit) => {
            for (remote, at) in remotes.iter().zip(&found) {
                if at.is_some() && !present(root, &commit) {
                    output(
                        remote.git(root)?.args([
                            "fetch",
                            "--no-tags",
                            &remote.url,
                            &format!("+refs/tags/{tag}:refs/tags/{tag}"),
                        ]),
                        None,
                    )?;
                }
            }
            let parents = git(root, &["rev-list", "--parents", "-n", "1", &commit])?;
            let manifest = output(
                command(root).args(["show", &format!("{commit}:{}", cfg.manifest)]),
                None,
            )?;
            let matched = stamp_matches(&parents, &manifest, source, version, &checksum);
            match existing_tag(&tag, matched, registered)? {
                Existing::Reuse => {
                    println!("[tag] {tag} already stamps {source}");
                    Some(commit)
                }
                Existing::Replace => {
                    retract(cfg, root, &remotes, &mut found, &tag)?;
                    None
                }
            }
        }
        None => None,
    };
    let commit = match existing {
        Some(commit) => commit,
        None => stamp(cfg, tools, root, source, version, &checksum)?,
    };

    for (remote, at) in remotes.iter().zip(&found) {
        if at.is_none() {
            println!("[tag] pushing {tag} to {}...", remote.name);
            output(
                remote
                    .git(root)?
                    .args(["push", &remote.url, &format!("{commit}:refs/tags/{tag}")]),
                None,
            )?;
        }
    }
    Ok(commit)
}

/// What becomes of a tag the remotes already carry.
#[derive(Debug, PartialEq, Eq)]
enum Existing {
    Reuse,
    Replace,
}

/// A tag that stamps this build is reused. One another build made is replaced
/// until crates.io holds a crate at its version; after that it stops the run.
fn existing_tag(
    tag: &str,
    matched: Result<()>,
    registered: impl FnOnce() -> Result<Vec<String>>,
) -> Result<Existing> {
    let Err(mismatch) = matched else {
        return Ok(Existing::Reuse);
    };
    let held = registered()?;
    if !held.is_empty() {
        return Err(mismatch.context(format!(
            "{tag} belongs to another build, and crates.io already holds {}; that release is final",
            held.join(", ")
        )));
    }
    println!("[tag] {tag} belongs to another build ({mismatch:#}); replacing it");
    Ok(Existing::Replace)
}

/// Take the release under `tag` down on both remotes, then the tag itself,
/// here and wherever `found` lists it, so the stamp that replaces it is pushed
/// everywhere and the changelog no longer sees the old one.
fn retract(
    cfg: &ReleaseConfig,
    root: &Path,
    remotes: &[Remote],
    found: &mut [Option<String>],
    tag: &str,
) -> Result<()> {
    github::retract(&cfg.github_repo, tag)?;
    gitlab::retract(cfg, tag)?;
    for (remote, at) in remotes.iter().zip(found.iter_mut()) {
        if at.take().is_some() {
            println!("[tag] removing {tag} from {}...", remote.name);
            output(
                remote
                    .git(root)?
                    .args(["push", &remote.url, &format!(":refs/tags/{tag}")]),
                None,
            )?;
        }
    }
    git(root, &["update-ref", "-d", &format!("refs/tags/{tag}")]).map(drop)
}

/// The commit the release tags: `source` with the stamped manifest and the
/// changelog that names the version. It is built from objects alone, so the
/// checkout the release jobs share stays as they built it.
fn stamp(
    cfg: &ReleaseConfig,
    tools: &ToolsConfig,
    root: &Path,
    source: &str,
    version: &str,
    checksum: &str,
) -> Result<String> {
    let manifest = output(
        command(root).args(["show", &format!("{source}:{}", cfg.manifest)]),
        None,
    )?;
    let manifest = stamp_manifest(&manifest, version, checksum)?;
    let changelog = changelog::render(root, &cfg.changelog, tools, source, Some(version))?;

    let index = tempfile::tempdir().context("creating a scratch index directory")?;
    let index = index.path().join("index");
    let indexed = |args: &[&str]| {
        output(command(root).env("GIT_INDEX_FILE", &index).args(args), None)
            .map(|text| text.trim().to_string())
    };
    indexed(&["read-tree", source])?;
    for (path, text) in [
        (&cfg.manifest, &manifest),
        (&cfg.changelog.output, &changelog),
    ] {
        let blob = output(
            command(root).args(["hash-object", "-w", "--stdin"]),
            Some(text.as_bytes()),
        )?;
        indexed(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{},{path}", blob.trim()),
        ])?;
    }
    let tree = indexed(&["write-tree"])?;
    let commit = output(
        committer(root).args([
            "commit-tree",
            &tree,
            "-p",
            source,
            "-m",
            &format!("chore(release): {version}"),
        ]),
        None,
    )?;
    let commit = commit.trim().to_string();
    println!("[tag] stamped {version} with checksum {checksum} as {commit}");
    Ok(commit)
}

fn present(root: &Path, commit: &str) -> bool {
    git(root, &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_ok()
}

/// The commit `ls-remote` lists for `tag`. An annotated tag is listed twice,
/// and the peeled line names the commit under it.
fn tag_commit(listing: &str, tag: &str) -> Option<String> {
    let direct = format!("refs/tags/{tag}");
    let peeled = format!("{direct}^{{}}");
    let lookup = |reference: &str| {
        listing.lines().find_map(|line| {
            let (sha, name) = line.split_once('\t')?;
            (name.trim() == reference).then(|| sha.trim().to_string())
        })
    };
    lookup(&peeled).or_else(|| lookup(&direct))
}

/// The commit every remote that carries the tag agrees on.
fn agreed(tag: &str, remotes: &[Remote], found: &[Option<String>]) -> Result<Option<String>> {
    let mut commits = remotes
        .iter()
        .zip(found)
        .filter_map(|(remote, commit)| Some((remote.name, commit.as_deref()?)));
    let Some((_, first)) = commits.next() else {
        return Ok(None);
    };
    let mut listed = String::new();
    let mut disagree = false;
    for (name, commit) in remotes.iter().map(|remote| remote.name).zip(found) {
        let commit = commit.as_deref().unwrap_or("nothing");
        disagree |= commit != first && commit != "nothing";
        let _ = write!(listed, " {name}={commit}");
    }
    if disagree {
        bail!("{tag} names different commits:{listed}");
    }
    Ok(Some(first.to_string()))
}

/// Whether the tagged commit is the stamp of `source` for `version` with the
/// framework built here: one parent, the built commit, and a manifest that
/// names the version and the checksum.
fn stamp_matches(
    parents: &str,
    manifest: &str,
    source: &str,
    version: &str,
    checksum: &str,
) -> Result<()> {
    let parents: Vec<_> = parents.split_whitespace().skip(1).collect();
    if parents != [source] {
        bail!("its commit has parents {parents:?}, the release built {source}");
    }
    for (key, expected) in [("version", version), ("checksum", checksum)] {
        let found = manifest_field(manifest, key)?;
        if found != expected {
            bail!("its manifest has {key} {found}, the release has {expected}");
        }
    }
    Ok(())
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
            let _ = writeln!(out, "let version = \"{version}\"");
            seen_version = true;
        } else if line.starts_with("let checksum = ") {
            let _ = writeln!(out, "let checksum = \"{checksum}\"");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts;

    fn remotes() -> [Remote; 2] {
        [
            Remote::github("zvuk/kithara", None).unwrap(),
            Remote::gitlab("gitlab.example", "group/kithara", "token"),
        ]
    }

    #[test]
    fn stamp_replaces_version_and_checksum() {
        let out = stamp_manifest(consts::MANIFEST_SAMPLE, "0.0.2", "deadbeef").unwrap();
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
            manifest_field(consts::MANIFEST_SAMPLE, "version").unwrap(),
            "0.0.1-alpha3"
        );
        assert_eq!(
            manifest_field(consts::MANIFEST_SAMPLE, "checksum").unwrap(),
            "abc"
        );
        assert!(manifest_field(consts::MANIFEST_SAMPLE, "missing").is_err());
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
    fn an_annotated_tag_names_the_commit_under_it() {
        let listing = "aaa\trefs/tags/v0.0.2\nccc\trefs/tags/v0.0.2^{}\n";

        assert_eq!(tag_commit(listing, "v0.0.2").as_deref(), Some("ccc"));
        assert_eq!(
            tag_commit("ccc\trefs/tags/v0.0.2\n", "v0.0.2").as_deref(),
            Some("ccc")
        );
        assert_eq!(tag_commit("", "v0.0.2"), None);
        assert_eq!(tag_commit("aaa\trefs/tags/v0.0.20\n", "v0.0.2"), None);
    }

    #[test]
    fn a_tag_of_this_build_is_reused_without_asking_the_registry() {
        let existing = existing_tag("v0.0.2", Ok(()), || panic!("registry consulted")).unwrap();

        assert_eq!(existing, Existing::Reuse);
    }

    #[test]
    fn a_tag_of_another_build_is_replaced_while_no_crate_is_published() {
        let existing =
            existing_tag("v0.0.2", Err(anyhow::anyhow!("other build")), || Ok(vec![])).unwrap();

        assert_eq!(existing, Existing::Replace);
    }

    #[test]
    fn a_tag_of_another_build_is_final_once_a_crate_is_published() {
        let error = existing_tag("v0.0.2", Err(anyhow::anyhow!("other build")), || {
            Ok(vec!["kithara-derive".to_string()])
        })
        .unwrap_err();

        assert!(format!("{error:#}").contains("kithara-derive"), "{error:#}");
    }

    #[test]
    fn a_tag_on_one_remote_is_pushed_to_the_other() {
        let remotes = remotes();

        assert_eq!(agreed("v0.0.2", &remotes, &[None, None]).unwrap(), None);
        assert_eq!(
            agreed("v0.0.2", &remotes, &[None, Some("ccc".into())])
                .unwrap()
                .as_deref(),
            Some("ccc")
        );
    }

    #[test]
    fn remotes_that_tag_different_commits_stop_the_release() {
        let error = agreed(
            "v0.0.2",
            &remotes(),
            &[Some("aaa".into()), Some("ccc".into())],
        )
        .unwrap_err();

        assert!(error.to_string().contains("github=aaa"), "{error}");
        assert!(error.to_string().contains("gitlab=ccc"), "{error}");
    }

    #[test]
    fn a_tag_stamps_only_the_build_it_was_made_for() {
        let manifest = "let version = \"0.0.2\"\nlet checksum = \"abc\"\n";

        assert!(stamp_matches("tag src\n", manifest, "src", "0.0.2", "abc").is_ok());
        assert!(stamp_matches("tag other\n", manifest, "src", "0.0.2", "abc").is_err());
        assert!(stamp_matches("tag src extra\n", manifest, "src", "0.0.2", "abc").is_err());
        let error = stamp_matches("tag src\n", manifest, "src", "0.0.2", "def").unwrap_err();
        assert!(error.to_string().contains("checksum abc"), "{error}");
    }
}
