use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use fs4::{FileExt, TryLockError};

use super::layout::{self, GENERATION_PREFIX, LEASE_FILE, REFRESH_LOCK};

#[derive(Debug)]
pub(crate) struct GenerationLease {
    file: Option<File>,
    cleanup: Option<LeaseCleanup>,
}

#[derive(Debug)]
struct LeaseCleanup {
    expected_locator: Vec<u8>,
    generation: PathBuf,
    root: PathBuf,
}

impl GenerationLease {
    fn open(generation: &Path) -> Result<File> {
        let path = generation.join(LEASE_FILE);
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open self-cache lease {}", path.display()))
    }

    #[cfg(test)]
    fn acquire(generation: &Path) -> Result<Self> {
        let file = Self::open(generation)?;
        FileExt::lock_shared(&file)
            .with_context(|| format!("lock self-cache generation {}", generation.display()))?;
        Ok(Self {
            file: Some(file),
            cleanup: None,
        })
    }

    fn finish(root: &Path, generation: &Path, file: File) -> Result<Self> {
        FileExt::lock_shared(&file)
            .with_context(|| format!("lock self-cache generation {}", generation.display()))?;
        let current = layout::load(root, generation)?;
        Ok(Self {
            file: Some(file),
            cleanup: Some(LeaseCleanup::new(root, current.path)),
        })
    }
}

impl Drop for GenerationLease {
    fn drop(&mut self) {
        drop(self.file.take());
        if let Some(cleanup) = self.cleanup.take() {
            let _ = cleanup.run();
        }
    }
}

impl LeaseCleanup {
    fn new(root: &Path, generation: PathBuf) -> Self {
        let expected_locator = format!("{}\n", generation.display()).into_bytes();
        Self {
            expected_locator,
            generation,
            root: root.to_path_buf(),
        }
    }

    fn run(self) -> Result<()> {
        match layout::locator_snapshot(&self.root)? {
            Some(locator) if locator == self.expected_locator => return Ok(()),
            Some(_) => {}
            None => return Ok(()),
        }
        let Some(_refresh) = try_refresh(&self.root)? else {
            return Ok(());
        };
        let active = layout::active(&self.root)?;
        if active.path == self.generation {
            return Ok(());
        }
        cleanup(
            &self.root,
            &active.path,
            active.manifest.keep_generations,
            Duration::from_secs(active.manifest.generation_grace_secs),
        )
    }
}

#[derive(Debug)]
pub(super) struct RefreshLock {
    _file: File,
}

pub(crate) fn lease_current() -> Result<Option<GenerationLease>> {
    let Some(generation) = layout::current()? else {
        return Ok(None);
    };
    let root = layout::root()?;
    let file = GenerationLease::open(&generation.path)?;
    GenerationLease::finish(&root, &generation.path, file).map(Some)
}

pub(super) fn refresh(root: &Path) -> Result<RefreshLock> {
    let (file, path) = open_refresh(root)?;
    FileExt::lock(&file).with_context(|| format!("lock self-cache refresh {}", path.display()))?;
    Ok(RefreshLock { _file: file })
}

fn try_refresh(root: &Path) -> Result<Option<RefreshLock>> {
    let (file, path) = open_refresh(root)?;
    match FileExt::try_lock(&file) {
        Ok(()) => Ok(Some(RefreshLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("try lock self-cache refresh {}", path.display()))
        }
    }
}

fn open_refresh(root: &Path) -> Result<(File, PathBuf)> {
    let cache = layout::cache_dir(root)?;
    fs::create_dir_all(&cache)
        .with_context(|| format!("create self-cache directory {}", cache.display()))?;
    let path = cache.join(REFRESH_LOCK);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open self-cache refresh lock {}", path.display()))?;
    Ok((file, path))
}

pub(super) fn cleanup(
    root: &Path,
    active: &Path,
    keep_generations: usize,
    grace: Duration,
) -> Result<()> {
    let cache = layout::cache_dir(root)?;
    let active = fs::canonicalize(active)
        .with_context(|| format!("resolve active self-cache generation {}", active.display()))?;
    let keep_inactive = keep_generations.saturating_sub(1);
    let mut newest = Vec::new();
    for entry in
        fs::read_dir(&cache).with_context(|| format!("read self-cache {}", cache.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path == active || !is_generation(&entry)? || !has_regular_lease(&path)? {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .with_context(|| format!("read self-cache age {}", path.display()))?;
        insert_newest(&mut newest, (modified, path), keep_inactive);
    }
    let protected = newest.into_iter().map(|(_, path)| path).collect::<Vec<_>>();
    for entry in
        fs::read_dir(&cache).with_context(|| format!("read self-cache {}", cache.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path == active || !is_generation(&entry)? || !old_enough(&entry, grace)? {
            continue;
        }
        if !has_regular_lease(&path)? {
            remove_generation(&path)?;
            continue;
        }
        if protected.iter().any(|candidate| candidate == &path) {
            continue;
        }
        let lease_path = path.join(LEASE_FILE);
        let lease = match OpenOptions::new().read(true).write(true).open(&lease_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                remove_generation(&path)?;
                continue;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("open self-cache lease {}", lease_path.display()));
            }
        };
        match FileExt::try_lock(&lease) {
            Ok(()) => remove_generation(&path)?,
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| {
                    format!("lock inactive self-cache generation {}", path.display())
                });
            }
        }
    }
    Ok(())
}

fn insert_newest(
    newest: &mut Vec<(SystemTime, PathBuf)>,
    candidate: (SystemTime, PathBuf),
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    let position = newest
        .binary_search(&candidate)
        .unwrap_or_else(|position| position);
    newest.insert(position, candidate);
    if newest.len() > limit {
        newest.remove(0);
    }
}

fn is_generation(entry: &fs::DirEntry) -> Result<bool> {
    let file_type = entry
        .file_type()
        .with_context(|| format!("read self-cache type {}", entry.path().display()))?;
    Ok(file_type.is_dir()
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(GENERATION_PREFIX)))
}

fn has_regular_lease(generation: &Path) -> Result<bool> {
    let path = generation.join(LEASE_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("read self-cache lease metadata {}", path.display()))
        }
    }
}

fn remove_generation(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("remove inactive self-cache generation {}", path.display())),
    }
}

fn old_enough(entry: &fs::DirEntry, grace: Duration) -> Result<bool> {
    let modified = entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .with_context(|| format!("read self-cache age {}", entry.path().display()))?;
    Ok(modified.elapsed().is_ok_and(|age| age >= grace))
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, FileTimes},
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };

    use anyhow::Result;

    use super::{GenerationLease, LeaseCleanup, cleanup};
    use crate::{
        config::XtaskCacheConfig,
        self_cache::{layout, manifest::CacheManifest, publish},
    };

    fn published_fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf, XtaskCacheConfig)> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let source = root.join("built-xtask");
        fs::create_dir_all(root.join(".git"))?;
        fs::create_dir_all(root.join("xtask/src"))?;
        fs::write(root.join("justfile"), b"")?;
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
        fs::write(&source, b"binary")?;
        let config = XtaskCacheConfig {
            extra_inputs: Vec::new(),
            keep_generations: 2,
            generation_grace_secs: 3600,
        };
        Ok((temp, root, source, config))
    }

    fn publish_active(
        root: &Path,
        source: &Path,
        config: &XtaskCacheConfig,
    ) -> Result<layout::Generation> {
        let manifest = CacheManifest::from_roots(root, config, vec![PathBuf::from("xtask")])?;
        publish::from_source(root, source, &manifest)?;
        layout::active(root)
    }

    fn set_age(path: &Path, age: Duration) -> Result<()> {
        let times = FileTimes::new().set_modified(SystemTime::now() - age);
        fs::File::open(path)?.set_times(times)?;
        Ok(())
    }

    #[test]
    fn cleanup_skips_a_leased_inactive_generation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let cache = root.join(".git/xtask-cache");
        let active = cache.join("generation-active");
        let leased = cache.join("generation-a");
        let previous = cache.join("generation-z");
        fs::create_dir_all(root.join("xtask"))?;
        for generation in [&active, &leased, &previous] {
            fs::create_dir_all(generation)?;
            fs::write(generation.join("lease.lock"), b"")?;
        }
        let lease = GenerationLease::acquire(&leased)?;

        cleanup(&root, &active, 2, Duration::ZERO)?;
        assert!(leased.is_dir());
        drop(lease);
        cleanup(&root, &active, 2, Duration::ZERO)?;
        assert!(!leased.exists());
        assert!(active.is_dir());
        assert!(previous.is_dir());
        Ok(())
    }

    #[test]
    fn cleanup_removes_a_corrupt_generation_without_a_lease() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let cache = root.join(".git/xtask-cache");
        let active = cache.join("generation-active");
        let corrupt = cache.join("generation-corrupt");
        fs::create_dir_all(&active)?;
        fs::write(active.join("lease.lock"), b"")?;
        fs::create_dir(&corrupt)?;

        cleanup(&root, &active, 2, Duration::ZERO)?;

        assert!(!corrupt.exists());
        assert!(active.is_dir());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn lease_revalidates_the_generation_after_locking() -> Result<()> {
        let (_temp, root, source, config) = published_fixture()?;
        let generation = publish_active(&root, &source, &config)?;
        let path = generation.path;
        let file = GenerationLease::open(&path)?;
        fs::remove_dir_all(&path)?;

        assert!(GenerationLease::finish(&root, &path, file).is_err());
        Ok(())
    }

    #[test]
    fn active_lease_drop_stays_on_the_fast_path() -> Result<()> {
        let (_temp, root, source, config) = published_fixture()?;
        let generation = publish_active(&root, &source, &config)?;
        let path = generation.path;
        let file = GenerationLease::open(&path)?;
        let lease = GenerationLease::finish(&root, &path, file)?;
        let refresh = layout::cache_dir(&root)?.join(layout::REFRESH_LOCK);

        drop(lease);

        assert!(path.is_dir());
        assert!(!refresh.exists());
        Ok(())
    }

    #[test]
    fn last_inactive_lease_drop_retries_retention_cleanup() -> Result<()> {
        let (_temp, root, source, mut config) = published_fixture()?;
        config.generation_grace_secs = 1;
        let active = publish_active(&root, &source, &config)?.path;
        let cache = layout::cache_dir(&root)?;
        let leased = cache.join("generation-a");
        let retained = cache.join("generation-z");
        for generation in [&leased, &retained] {
            fs::create_dir(generation)?;
            fs::write(generation.join(layout::LEASE_FILE), b"")?;
        }
        set_age(&leased, Duration::from_secs(30))?;
        set_age(&retained, Duration::from_secs(10))?;
        let mut first = GenerationLease::acquire(&leased)?;
        first.cleanup = Some(LeaseCleanup::new(&root, leased.clone()));
        let mut second = GenerationLease::acquire(&leased)?;
        second.cleanup = Some(LeaseCleanup::new(&root, leased.clone()));

        drop(first);
        assert!(leased.is_dir());
        drop(second);

        assert!(!leased.exists());
        assert!(retained.is_dir());
        assert!(active.is_dir());
        Ok(())
    }

    #[test]
    fn inactive_lease_uses_the_active_retention_policy() -> Result<()> {
        let (_temp, root, source, mut old_config) = published_fixture()?;
        old_config.generation_grace_secs = 1;
        let old = publish_active(&root, &source, &old_config)?.path;
        let file = GenerationLease::open(&old)?;
        let lease = GenerationLease::finish(&root, &old, file)?;
        let mut active_config = old_config;
        active_config.keep_generations = 3;
        let active = publish_active(&root, &source, &active_config)?.path;
        let cache = layout::cache_dir(&root)?;
        let first = cache.join("generation-extra-a");
        let second = cache.join("generation-extra-b");
        for generation in [&first, &second] {
            fs::create_dir(generation)?;
            fs::write(generation.join(layout::LEASE_FILE), b"")?;
        }
        set_age(&old, Duration::from_secs(30))?;
        set_age(&first, Duration::from_secs(20))?;
        set_age(&second, Duration::from_secs(10))?;

        drop(lease);

        assert!(!old.exists());
        assert!(first.is_dir());
        assert!(second.is_dir());
        assert!(active.is_dir());
        Ok(())
    }

    #[test]
    fn inactive_lease_drop_does_not_wait_for_refresh() -> Result<()> {
        let (_temp, root, source, config) = published_fixture()?;
        let active = publish_active(&root, &source, &config)?.path;
        let cache = layout::cache_dir(&root)?;
        let leased = cache.join("generation-inactive");
        fs::create_dir(&leased)?;
        fs::write(leased.join(layout::LEASE_FILE), b"")?;
        let mut lease = GenerationLease::acquire(&leased)?;
        lease.cleanup = Some(LeaseCleanup::new(&root, leased.clone()));
        let _refresh = super::refresh(&root)?;

        drop(lease);

        assert!(leased.is_dir());
        assert!(active.is_dir());
        Ok(())
    }
}
