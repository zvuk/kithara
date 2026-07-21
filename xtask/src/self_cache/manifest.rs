use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env, fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use cargo_metadata::MetadataCommand;
use serde::{Deserialize, Serialize};

use super::{
    digest::TreeDigest,
    layout::{self, validate_relative},
};
use crate::config::XtaskCacheConfig;

struct Consts;

impl Consts {
    const BUILD_RECIPE: u32 = 1;
    const CACHE_SCHEMA: u32 = 1;
    const MANIFEST_LIMIT: usize = 1024 * 1024;
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CacheManifest {
    architecture: String,
    build_profile: String,
    build_recipe: u32,
    cache_config_digest: String,
    cache_schema: u32,
    extra_inputs: Vec<PathBuf>,
    pub(super) generation_grace_secs: u64,
    git_dir: PathBuf,
    pub(super) keep_generations: usize,
    operating_system: String,
    package_roots: Vec<PathBuf>,
    root: PathBuf,
    pub(super) source_stamp: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Freshness {
    Current,
    Stale,
}

impl CacheManifest {
    pub(super) fn discover(root: &Path, config: &XtaskCacheConfig) -> Result<Self> {
        let package_roots = discover_package_roots(root)?;
        Self::from_roots(root, config, package_roots)
    }

    pub(super) fn read(path: &Path) -> Result<Self> {
        let bytes = layout::read_bounded(path, "self-cache manifest", Consts::MANIFEST_LIMIT)?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("parse self-cache manifest {}", path.display()))
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self).context("serialize self-cache manifest")?;
        bytes.push(b'\n');
        ensure!(
            bytes.len() <= Consts::MANIFEST_LIMIT,
            "self-cache manifest exceeds {} bytes",
            Consts::MANIFEST_LIMIT
        );
        Ok(bytes)
    }

    pub(super) fn is_release(&self) -> bool {
        self.build_profile == "release"
    }

    pub(super) fn validate(&self, root: &Path, cache: &Path) -> Result<()> {
        ensure!(
            self.cache_schema == Consts::CACHE_SCHEMA,
            "unsupported self-cache schema"
        );
        ensure!(
            self.build_recipe == Consts::BUILD_RECIPE,
            "unsupported self-cache build recipe"
        );
        ensure!(
            self.operating_system == env::consts::OS,
            "self-cache operating system does not match"
        );
        ensure!(
            self.architecture == env::consts::ARCH,
            "self-cache architecture does not match"
        );
        ensure!(
            self.build_profile == build_profile(),
            "self-cache build profile does not match"
        );
        ensure!(
            self.root == root,
            "self-cache repository root does not match"
        );
        ensure!(
            self.git_dir
                == cache
                    .parent()
                    .context("self-cache directory has no parent")?,
            "self-cache Git directory does not match"
        );
        ensure!(
            valid_digest(&self.cache_config_digest),
            "self-cache config digest is invalid"
        );
        ensure!(
            valid_digest(&self.source_stamp),
            "self-cache source stamp is invalid"
        );
        ensure!(
            !self.package_roots.is_empty(),
            "self-cache package roots are empty"
        );
        ensure!(
            self.keep_generations >= 2,
            "self-cache generation retention is invalid"
        );
        ensure!(
            self.generation_grace_secs > 0,
            "self-cache generation grace period is invalid"
        );
        for path in &self.package_roots {
            validate_relative(path, "self-cache package root")?;
        }
        for path in &self.extra_inputs {
            validate_relative(path, "self-cache extra input")?;
        }
        ensure!(
            strictly_sorted(&self.package_roots),
            "self-cache package roots are not normalized"
        );
        ensure!(
            strictly_sorted(&self.extra_inputs),
            "self-cache extra inputs are not normalized"
        );
        let embedded = XtaskCacheConfig {
            extra_inputs: self.extra_inputs.clone(),
            keep_generations: self.keep_generations,
            generation_grace_secs: self.generation_grace_secs,
        };
        let normalized = normalize_config(&embedded)?;
        ensure!(
            normalized.extra_inputs == self.extra_inputs
                && normalized.digest == self.cache_config_digest,
            "self-cache manifest policy digest does not match"
        );
        Ok(())
    }

    pub(super) fn freshness(&self, root: &Path, config: &XtaskCacheConfig) -> Result<Freshness> {
        let normalized = normalize_config(config)?;
        let source_stamp = source_stamp(
            root,
            &self.package_roots,
            &normalized.extra_inputs,
            &normalized.digest,
        )?;
        Ok(if source_stamp == self.source_stamp {
            Freshness::Current
        } else {
            Freshness::Stale
        })
    }

    pub(super) fn from_roots(
        root: &Path,
        config: &XtaskCacheConfig,
        mut package_roots: Vec<PathBuf>,
    ) -> Result<Self> {
        package_roots.sort();
        package_roots.dedup();
        ensure!(!package_roots.is_empty(), "xtask package closure is empty");
        let normalized = normalize_config(config)?;
        let git_dir = layout::git_dir(root)?;
        let source_stamp = source_stamp(
            root,
            &package_roots,
            &normalized.extra_inputs,
            &normalized.digest,
        )?;
        Ok(Self {
            architecture: env::consts::ARCH.to_owned(),
            build_profile: build_profile().to_owned(),
            build_recipe: Consts::BUILD_RECIPE,
            cache_config_digest: normalized.digest,
            cache_schema: Consts::CACHE_SCHEMA,
            extra_inputs: normalized.extra_inputs,
            generation_grace_secs: config.generation_grace_secs,
            git_dir,
            keep_generations: config.keep_generations,
            operating_system: env::consts::OS.to_owned(),
            package_roots,
            root: root.to_path_buf(),
            source_stamp,
        })
    }
}

struct NormalizedConfig {
    digest: String,
    extra_inputs: Vec<PathBuf>,
}

fn normalize_config(config: &XtaskCacheConfig) -> Result<NormalizedConfig> {
    let mut extra_inputs = config
        .extra_inputs
        .iter()
        .map(|path| normalize_relative(path))
        .collect::<Result<Vec<_>>>()?;
    extra_inputs.sort();
    extra_inputs.dedup();
    let mut digest = TreeDigest::new();
    let keep_generations = u64::try_from(config.keep_generations)
        .context("normalize self-cache generation retention")?;
    digest.add_bytes(
        b"cache-config",
        Path::new("keep-generations"),
        &keep_generations.to_be_bytes(),
    );
    digest.add_bytes(
        b"cache-config",
        Path::new("generation-grace-seconds"),
        &config.generation_grace_secs.to_be_bytes(),
    );
    for path in &extra_inputs {
        digest.add_bytes(
            b"cache-config-extra-input",
            path,
            path.as_os_str().as_encoded_bytes(),
        );
    }
    Ok(NormalizedConfig {
        digest: digest.finish(),
        extra_inputs,
    })
}

fn source_stamp(
    root: &Path,
    package_roots: &[PathBuf],
    extra_inputs: &[PathBuf],
    config_digest: &str,
) -> Result<String> {
    let mut digest = TreeDigest::new();
    digest.add_bytes(
        b"protocol",
        Path::new("cache-schema"),
        &Consts::CACHE_SCHEMA.to_be_bytes(),
    );
    digest.add_bytes(
        b"protocol",
        Path::new("build-recipe"),
        &Consts::BUILD_RECIPE.to_be_bytes(),
    );
    digest.add_bytes(b"platform", Path::new("os"), env::consts::OS.as_bytes());
    digest.add_bytes(
        b"platform",
        Path::new("architecture"),
        env::consts::ARCH.as_bytes(),
    );
    digest.add_bytes(
        b"platform",
        Path::new("profile"),
        build_profile().as_bytes(),
    );
    digest.add_bytes(
        b"cache-config",
        Path::new("digest"),
        config_digest.as_bytes(),
    );
    for relative in package_roots {
        let physical = checked_input(root, relative)?;
        digest.add_path(&physical, &Path::new("package").join(relative))?;
    }
    for relative in [Path::new("Cargo.toml"), Path::new("Cargo.lock")] {
        let physical = checked_input(root, relative)?;
        digest.add_path(&physical, &Path::new("workspace").join(relative))?;
    }
    for relative in [
        Path::new(".cargo/config.toml"),
        Path::new("rust-toolchain.toml"),
        Path::new("rust-toolchain"),
    ] {
        let physical = root.join(relative);
        if fs::symlink_metadata(&physical).is_ok() {
            let physical = checked_input(root, relative)?;
            digest.add_path(&physical, &Path::new("workspace").join(relative))?;
        }
    }
    for relative in extra_inputs {
        let physical = checked_input(root, relative)?;
        digest.add_path(&physical, &Path::new("extra").join(relative))?;
    }
    Ok(digest.finish())
}

fn checked_input(root: &Path, relative: &Path) -> Result<PathBuf> {
    validate_relative(relative, "self-cache input")?;
    let mut components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .peekable();
    let mut current = root.to_path_buf();
    if components.peek().is_none() {
        return Ok(current);
    }
    while let Some(component) = components.next() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current)
            .with_context(|| format!("read self-cache input {}", current.display()))?;
        if components.peek().is_some() && metadata.file_type().is_symlink() {
            bail!(
                "self-cache input traverses a symlink: {}",
                current.display()
            );
        }
    }
    Ok(current)
}

fn normalize_relative(path: &Path) -> Result<PathBuf> {
    validate_relative(path, "self-cache config input")?;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if let Component::Normal(value) = component {
            normalized.push(value);
        }
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    Ok(normalized)
}

fn discover_package_roots(root: &Path) -> Result<Vec<PathBuf>> {
    let mut command = MetadataCommand::new();
    command
        .current_dir(root)
        .manifest_path(root.join("Cargo.toml"))
        .other_options(vec!["--locked".to_owned()]);
    let metadata = command.exec().context("discover xtask source closure")?;
    let resolve = metadata
        .resolve
        .as_ref()
        .context("Cargo metadata did not return a dependency graph")?;
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.clone(), package))
        .collect::<BTreeMap<_, _>>();
    let nodes = resolve
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let manifest =
        fs::canonicalize(root.join("xtask/Cargo.toml")).context("resolve xtask manifest")?;
    let xtask = metadata
        .packages
        .iter()
        .find(|package| {
            package.name.as_ref() == "xtask"
                && fs::canonicalize(package.manifest_path.as_std_path())
                    .is_ok_and(|path| path == manifest)
        })
        .context("find xtask package in Cargo metadata")?;
    let mut pending = VecDeque::from([xtask.id.clone()]);
    let mut visited = BTreeSet::new();
    let mut roots = Vec::new();
    while let Some(id) = pending.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let package = packages
            .get(&id)
            .with_context(|| format!("resolve Cargo package {id}"))?;
        ensure!(
            package.source.is_none(),
            "xtask local closure reached a registry package"
        );
        let package_root = package
            .manifest_path
            .parent()
            .context("Cargo package manifest has no parent")?;
        let package_root = fs::canonicalize(package_root.as_std_path())
            .with_context(|| format!("resolve Cargo package root {}", package_root))?;
        let relative = package_root.strip_prefix(root).with_context(|| {
            format!(
                "Cargo package root is outside the repository: {}",
                package_root.display()
            )
        })?;
        roots.push(if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative.to_path_buf()
        });
        let node = nodes
            .get(&id)
            .with_context(|| format!("resolve Cargo dependency node {id}"))?;
        for dependency in &node.dependencies {
            let dependency_package = packages
                .get(dependency)
                .with_context(|| format!("resolve Cargo dependency {dependency}"))?;
            if dependency_package.source.is_none() {
                pending.push_back(dependency.clone());
            }
        }
    }
    Ok(roots)
}

fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn strictly_sorted(paths: &[PathBuf]) -> bool {
    paths.windows(2).all(|pair| pair[0] < pair[1])
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use anyhow::Result;

    use super::{CacheManifest, Freshness};
    use crate::config::{KitharaExt, XtaskCacheConfig};

    fn fixture() -> Result<(tempfile::TempDir, PathBuf, XtaskCacheConfig)> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join(".git"))?;
        fs::create_dir_all(root.join(".config"))?;
        fs::create_dir_all(root.join("xtask/src"))?;
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"xtask\"]\n",
        )?;
        fs::write(root.join("Cargo.lock"), "version = 4\n")?;
        fs::write(
            root.join("xtask/Cargo.toml"),
            "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
        )?;
        fs::write(root.join("xtask/src/main.rs"), "fn main() {}\n")?;
        let config = XtaskCacheConfig {
            extra_inputs: Vec::new(),
            keep_generations: 2,
            generation_grace_secs: 3600,
        };
        Ok((temp, root, config))
    }

    #[test]
    fn source_and_cache_policy_changes_are_stale() -> Result<()> {
        let (_temp, root, mut config) = fixture()?;
        let manifest = CacheManifest::from_roots(&root, &config, vec![PathBuf::from("xtask")])?;
        assert_eq!(manifest.freshness(&root, &config)?, Freshness::Current);

        fs::write(root.join("xtask/src/main.rs"), "fn main() { changed(); }\n")?;
        assert_eq!(manifest.freshness(&root, &config)?, Freshness::Stale);
        fs::write(root.join("xtask/src/main.rs"), "fn main() {}\n")?;
        fs::write(root.join("Cargo.lock"), "version = 4\n# changed\n")?;
        assert_eq!(manifest.freshness(&root, &config)?, Freshness::Stale);
        fs::write(root.join("Cargo.lock"), "version = 4\n")?;
        config.keep_generations = 3;
        assert_eq!(manifest.freshness(&root, &config)?, Freshness::Stale);
        Ok(())
    }

    #[test]
    fn hook_route_only_change_does_not_make_cache_stale() -> Result<()> {
        let (_temp, root, config) = fixture()?;
        let config_path = root.join(".config/xtask.toml");
        fs::write(
            &config_path,
            r#"[ext.xtask.cache]
extra_inputs = []
keep_generations = 2
generation_grace_secs = 3600

[ext.agent_hook]
destructive_git_override_env = "OVERRIDE"

[[ext.agent_hook.routes]]
event = "pre-tool-use"
tool_kind = "shell"
handler = "command-guard"
"#,
        )?;
        let manifest = CacheManifest::from_roots(&root, &config, vec![PathBuf::from("xtask")])?;
        fs::write(
            &config_path,
            r#"[ext.xtask.cache]
extra_inputs = []
keep_generations = 2
generation_grace_secs = 3600

[ext.agent_hook]
destructive_git_override_env = "CHANGED"

[[ext.agent_hook.routes]]
event = "post-tool-use"
tool_kind = "file-edit"
handler = "format-edited-paths"
"#,
        )?;
        let loaded = KitharaExt::load(&root)?;

        assert_eq!(
            manifest.freshness(&root, loaded.xtask_cache()?)?,
            Freshness::Current
        );
        Ok(())
    }

    #[test]
    fn oversized_manifest_is_rejected_before_deserialization() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("manifest.json");
        fs::write(&path, vec![b' '; super::Consts::MANIFEST_LIMIT + 1])?;

        assert!(CacheManifest::read(&path).is_err());
        Ok(())
    }

    #[test]
    fn changed_retention_without_a_matching_digest_is_rejected() -> Result<()> {
        let (_temp, root, config) = fixture()?;
        let mut manifest = CacheManifest::from_roots(&root, &config, vec![PathBuf::from("xtask")])?;
        manifest.keep_generations = 3;
        let cache = super::layout::cache_dir(&root)?;

        assert!(manifest.validate(&root, &cache).is_err());
        Ok(())
    }
}
