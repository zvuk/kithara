#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    env, fs,
    io::{ErrorKind, Read},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

use super::manifest::CacheManifest;

pub(super) const BINARY: &str = "xtask";
pub(super) const CACHE_DIRECTORY: &str = "xtask-cache";
pub(super) const GENERATION_PREFIX: &str = "generation-";
pub(super) const LEASE_FILE: &str = "lease.lock";
pub(super) const LOCATOR: &str = "xtask/.xtask-cache";
pub(super) const MANIFEST_FILE: &str = "manifest.json";
pub(super) const REFRESH_LOCK: &str = "refresh.lock";
pub(super) const STAMP_FILE: &str = "stamp";

const CONTROL_FILE_LIMIT: usize = 16 * 1024;

#[derive(Debug)]
pub(super) struct Generation {
    pub(super) binary: PathBuf,
    pub(super) manifest: CacheManifest,
    pub(super) path: PathBuf,
}

pub(super) fn root() -> Result<PathBuf> {
    let current = env::current_dir().context("resolve self-cache working directory")?;
    let root = fs::canonicalize(&current)
        .with_context(|| format!("resolve self-cache root {}", current.display()))?;
    ensure!(
        root.join("justfile").is_file() && root.join("xtask/Cargo.toml").is_file(),
        "self-cache must run from the repository root: {}",
        root.display()
    );
    Ok(root)
}

pub(super) fn git_dir(root: &Path) -> Result<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return fs::canonicalize(&dot_git)
            .with_context(|| format!("resolve Git directory {}", dot_git.display()));
    }
    let metadata = fs::symlink_metadata(&dot_git)
        .with_context(|| format!("read Git directory file {}", dot_git.display()))?;
    ensure!(metadata.file_type().is_file(), "invalid Git directory file");
    let body = String::from_utf8(read_bounded(
        &dot_git,
        "Git directory file",
        CONTROL_FILE_LIMIT,
    )?)
    .context("Git directory file is not UTF-8")?;
    let mut lines = body.lines();
    let line = lines.next().context("Git directory file is empty")?;
    ensure!(lines.next().is_none(), "Git directory file has extra lines");
    let value = line
        .strip_prefix("gitdir: ")
        .context("Git directory file has an invalid prefix")?;
    ensure!(!value.is_empty(), "Git directory file has an empty path");
    let path = PathBuf::from(value);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    fs::canonicalize(&path).with_context(|| format!("resolve Git directory {}", path.display()))
}

pub(super) fn cache_dir(root: &Path) -> Result<PathBuf> {
    Ok(git_dir(root)?.join(CACHE_DIRECTORY))
}

pub(super) fn target_dir(root: &Path) -> PathBuf {
    root.join("target/xtask-self-cache")
}

pub(super) fn locator(root: &Path) -> PathBuf {
    root.join(LOCATOR)
}

pub(super) fn active(root: &Path) -> Result<Generation> {
    let path = read_locator(root)?;
    load(root, &path)
}

pub(super) fn locator_snapshot(root: &Path) -> Result<Option<Vec<u8>>> {
    match read_bounded(&locator(root), "self-cache locator", CONTROL_FILE_LIMIT) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).context("read self-cache locator snapshot"),
    }
}

pub(super) fn current() -> Result<Option<Generation>> {
    let executable = fs::canonicalize(env::current_exe().context("resolve current xtask")?)
        .context("resolve current xtask executable")?;
    let Some(generation) = executable.parent() else {
        return Ok(None);
    };
    if !generation.join(MANIFEST_FILE).is_file() || !generation.join(LEASE_FILE).is_file() {
        return Ok(None);
    }
    let root = root()?;
    let loaded = load(&root, generation)?;
    ensure!(
        loaded.binary == executable,
        "self-cache executable does not match its generation"
    );
    Ok(Some(loaded))
}

pub(super) fn load(root: &Path, path: &Path) -> Result<Generation> {
    ensure!(
        path.is_absolute(),
        "self-cache generation path is not absolute"
    );
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("read self-cache generation {}", path.display()))?;
    ensure!(
        metadata.file_type().is_dir(),
        "self-cache generation is not a directory: {}",
        path.display()
    );
    let path = fs::canonicalize(path)
        .with_context(|| format!("resolve self-cache generation {}", path.display()))?;
    let expected_cache = cache_dir(root)?;
    let expected_cache = fs::canonicalize(&expected_cache)
        .with_context(|| format!("resolve self-cache directory {}", expected_cache.display()))?;
    ensure!(
        path.parent() == Some(expected_cache.as_path()),
        "self-cache generation is owned by another worktree: {}",
        path.display()
    );
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("self-cache generation has an invalid name")?;
    ensure!(
        name.starts_with(GENERATION_PREFIX),
        "self-cache generation has an invalid name"
    );

    let manifest = CacheManifest::read(&path.join(MANIFEST_FILE))?;
    manifest.validate(root, &expected_cache)?;
    let stamp = read_line(&path.join(STAMP_FILE), "self-cache stamp")?;
    ensure!(
        stamp == manifest.source_stamp,
        "self-cache stamp does not match its manifest"
    );
    let binary = path.join(BINARY);
    let binary_metadata = fs::symlink_metadata(&binary)
        .with_context(|| format!("read self-cache binary {}", binary.display()))?;
    ensure!(
        binary_metadata.file_type().is_file(),
        "self-cache binary is not a regular file"
    );
    #[cfg(unix)]
    ensure!(
        binary_metadata.permissions().mode() & 0o111 != 0,
        "self-cache binary is not executable"
    );
    let lease =
        fs::symlink_metadata(path.join(LEASE_FILE)).context("read self-cache lease file")?;
    ensure!(
        lease.file_type().is_file(),
        "self-cache lease is not a regular file"
    );

    Ok(Generation {
        binary,
        manifest,
        path,
    })
}

pub(super) fn validate_relative(path: &Path, label: &str) -> Result<()> {
    if path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        bail!("{label} is not a project-relative path: {}", path.display());
    }
    Ok(())
}

pub(super) fn read_line(path: &Path, label: &str) -> Result<String> {
    let bytes = read_bounded(path, label, CONTROL_FILE_LIMIT)?;
    let body = String::from_utf8(bytes)
        .with_context(|| format!("{label} is not UTF-8: {}", path.display()))?;
    let value = body
        .strip_suffix('\n')
        .filter(|value| !value.is_empty() && !value.contains('\n'))
        .with_context(|| format!("{label} must contain one newline-terminated value"))?;
    Ok(value.to_owned())
}

pub(super) fn read_bounded(path: &Path, label: &str, limit: usize) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("read {label} metadata {}", path.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "{label} is not a regular file: {}",
        path.display()
    );
    let file = fs::File::open(path).with_context(|| format!("read {label} {}", path.display()))?;
    let take = u64::try_from(limit)
        .context("convert self-cache control file limit")?
        .checked_add(1)
        .context("self-cache control file limit overflow")?;
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    file.take(take)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {label} {}", path.display()))?;
    ensure!(
        bytes.len() <= limit,
        "{label} exceeds {limit} bytes: {}",
        path.display()
    );
    Ok(bytes)
}

fn read_locator(root: &Path) -> Result<PathBuf> {
    let locator = locator(root);
    let metadata = fs::symlink_metadata(&locator)
        .with_context(|| format!("read self-cache locator {}", locator.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "self-cache locator is not a regular file"
    );
    let value = read_line(&locator, "self-cache locator")?;
    let path = PathBuf::from(value);
    ensure!(
        path.is_absolute(),
        "self-cache locator path is not absolute"
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use anyhow::Result;

    use super::{git_dir, read_line};

    #[test]
    fn linked_worktree_git_dir_is_resolved_without_git() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("repo");
        let git = fixture.path().join("git/worktrees/repo");
        fs::create_dir_all(&root)?;
        fs::create_dir_all(&git)?;
        fs::write(root.join(".git"), "gitdir: ../git/worktrees/repo\n")?;

        assert_eq!(git_dir(&root)?, fs::canonicalize(git)?);
        Ok(())
    }

    #[test]
    fn malformed_git_dir_file_is_rejected() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        fs::write(fixture.path().join(".git"), "gitdir: one\ngitdir: two\n")?;

        assert!(git_dir(fixture.path()).is_err());
        Ok(())
    }

    #[test]
    fn control_files_are_read_with_a_fixed_limit() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let path = fixture.path().join("locator");
        fs::write(&path, vec![b'x'; 32 * 1024])?;

        assert!(read_line(&path, "test locator").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn bounded_reader_rejects_non_regular_files() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let target = fixture.path().join("target");
        let link = fixture.path().join("link");
        fs::write(&target, b"value\n")?;
        symlink(&target, &link)?;

        assert!(read_line(&link, "test locator").is_err());
        assert!(read_line(fixture.path(), "test locator").is_err());
        Ok(())
    }
}
