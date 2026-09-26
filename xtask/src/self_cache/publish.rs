#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{layout, lease, manifest::CacheManifest};
use crate::consts;

fn publish(root: &Path, source: &Path, manifest: &CacheManifest) -> Result<PathBuf> {
    publish_with_activation(root, source, manifest, activate)
}

pub(super) fn from_source(root: &Path, source: &Path, manifest: &CacheManifest) -> Result<PathBuf> {
    publish(root, source, manifest)
}

fn publish_with_activation<F>(
    root: &Path,
    source: &Path,
    manifest: &CacheManifest,
    activation: F,
) -> Result<PathBuf>
where
    F: FnOnce(&Path, &Path, &str) -> Result<()>,
{
    let cache = layout::cache_dir(root)?;
    fs::create_dir_all(&cache)
        .with_context(|| format!("create self-cache directory {}", cache.display()))?;
    // std has no Windows directory-entry sync equivalent; file syncs still run on every host.
    #[cfg(unix)]
    sync_directory(
        cache
            .parent()
            .context("self-cache directory has no parent")?,
    )?;
    cleanup_temporaries(&cache, Duration::from_secs(60 * 60))?;
    cleanup_locator_temporaries(root, Duration::from_secs(60 * 60))?;
    let (temporary, generation, suffix) = reserve(&cache, &manifest.source_stamp)?;
    let result = publish_reserved(
        root,
        source,
        manifest,
        &temporary,
        &generation,
        &suffix,
        activation,
    );
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

fn publish_reserved<F>(
    root: &Path,
    source: &Path,
    manifest: &CacheManifest,
    temporary: &Path,
    generation: &Path,
    suffix: &str,
    activation: F,
) -> Result<PathBuf>
where
    F: FnOnce(&Path, &Path, &str) -> Result<()>,
{
    let binary = temporary.join(consts::BINARY);
    copy_executable(source, &binary)?;
    write_new(&temporary.join(consts::MANIFEST_FILE), &manifest.encode()?)?;
    write_new(
        &temporary.join(consts::STAMP_FILE),
        format!("{}\n", manifest.source_stamp).as_bytes(),
    )?;
    write_new(&temporary.join(consts::LEASE_FILE), b"")?;
    #[cfg(unix)]
    sync_directory(temporary)?;
    fs::rename(temporary, generation).with_context(|| {
        format!(
            "publish self-cache generation {} as {}",
            temporary.display(),
            generation.display()
        )
    })?;
    #[cfg(unix)]
    sync_directory(
        generation
            .parent()
            .context("self-cache generation has no parent")?,
    )?;
    activation(root, generation, suffix)?;
    if let Err(error) = lease::cleanup(
        root,
        generation,
        manifest.keep_generations,
        Duration::from_secs(manifest.generation_grace_secs),
    ) {
        eprintln!("warning: self-cache cleanup failed: {error:#}");
    }
    Ok(generation.join(consts::BINARY))
}

fn reserve(cache: &Path, stamp: &str) -> Result<(PathBuf, PathBuf, String)> {
    let process = std::process::id();
    for attempt in 0..consts::RESERVATION_ATTEMPTS {
        let suffix = format!("{process}-{attempt}");
        let name = format!(
            "{GENERATION_PREFIX}{stamp}-{suffix}",
            GENERATION_PREFIX = consts::GENERATION_PREFIX
        );
        let temporary = cache.join(format!(".{name}"));
        let generation = cache.join(name);
        match fs::create_dir(&temporary) {
            Ok(()) => match fs::symlink_metadata(&generation) {
                Ok(_) => {
                    fs::remove_dir(&temporary).with_context(|| {
                        format!(
                            "release occupied self-cache reservation {}",
                            temporary.display()
                        )
                    })?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok((temporary, generation, suffix));
                }
                Err(error) => {
                    let _ = fs::remove_dir(&temporary);
                    return Err(error).with_context(|| {
                        format!(
                            "inspect self-cache generation reservation {}",
                            generation.display()
                        )
                    });
                }
            },
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reserve self-cache generation {}", temporary.display())
                });
            }
        }
    }
    bail!("could not reserve a self-cache generation")
}

fn activate(root: &Path, generation: &Path, suffix: &str) -> Result<()> {
    let locator = layout::locator(root)?;
    let temporary = locator.with_file_name(format!("active.{suffix}"));
    let body = format!("{}\n", shell_readable(generation));
    let result = (|| -> Result<()> {
        write_new(&temporary, body.as_bytes())?;
        fs::rename(&temporary, &locator)
            .with_context(|| format!("activate self-cache locator {}", locator.display()))?;
        #[cfg(unix)]
        sync_directory(
            locator
                .parent()
                .context("self-cache locator has no parent")?,
        )?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Render a generation path the way the shell that reads the locator can use it.
///
/// Every path here is canonical, and on Windows that means the extended-length
/// form `\\?\C:\...`. The justfile transport reads this file with `sh`, which
/// knows a leading slash and a drive letter and nothing else, so the prefix
/// turns a healthy cache into "cached xtask transport is unavailable". Stripping
/// it changes nothing for the reader that canonicalises the path again.
fn shell_readable(generation: &Path) -> String {
    let rendered = generation.display().to_string();
    rendered
        .strip_prefix(r"\\?\")
        .unwrap_or(&rendered)
        .to_owned()
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create self-cache file {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write self-cache file {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync self-cache file {}", path.display()))
}

fn copy_executable(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("read current xtask {}", source.display()))?;
    if !metadata.file_type().is_file() {
        bail!("current xtask is not a regular file: {}", source.display());
    }
    let mut input = fs::File::open(source)
        .with_context(|| format!("open current xtask {}", source.display()))?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .with_context(|| format!("create cached xtask {}", target.display()))?;
    io::copy(&mut input, &mut output).with_context(|| {
        format!(
            "copy current xtask from {} to {}",
            source.display(),
            target.display()
        )
    })?;
    output.flush()?;
    output.sync_all()?;
    #[cfg(unix)]
    {
        let mut permissions = output.metadata()?.permissions();
        permissions.set_mode(0o755);
        output.set_permissions(permissions)?;
        output.sync_all()?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("open self-cache directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync self-cache directory {}", path.display()))
}

fn cleanup_temporaries(cache: &Path, grace: Duration) -> Result<()> {
    for entry in
        fs::read_dir(cache).with_context(|| format!("read self-cache {}", cache.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(".generation-") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .with_context(|| format!("read self-cache temporary {}", entry.path().display()))?;
        if metadata.modified()?.elapsed().is_ok_and(|age| age >= grace) {
            if metadata.file_type().is_dir() {
                fs::remove_dir_all(entry.path()).with_context(|| {
                    format!("remove self-cache temporary {}", entry.path().display())
                })?;
            } else {
                fs::remove_file(entry.path()).with_context(|| {
                    format!("remove self-cache temporary {}", entry.path().display())
                })?;
            }
        }
    }
    Ok(())
}

fn cleanup_locator_temporaries(root: &Path, grace: Duration) -> Result<()> {
    let directory = layout::cache_dir(root)?;
    for entry in fs::read_dir(&directory)
        .with_context(|| format!("read self-cache locator directory {}", directory.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("active.") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).with_context(|| {
            format!(
                "read self-cache locator temporary {}",
                entry.path().display()
            )
        })?;
        if !metadata.modified()?.elapsed().is_ok_and(|age| age >= grace) {
            continue;
        }
        if metadata.file_type().is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs, path::PathBuf};

    use anyhow::{Result, anyhow};

    use super::{CacheManifest, layout, publish, publish_with_activation, shell_readable};
    use crate::config::XtaskCacheConfig;

    /// The locator is read by the justfile's `sh`, which accepts a leading slash
    /// or a drive letter. A canonical Windows path carries neither until the
    /// extended-length prefix comes off, and the transport refuses what is left.
    #[test]
    fn the_locator_names_a_generation_a_shell_can_read() {
        assert_eq!(
            shell_readable(&PathBuf::from(r"\\?\C:\work\.git\xtask-cache\generation-1")),
            r"C:\work\.git\xtask-cache\generation-1"
        );
        assert_eq!(
            shell_readable(&PathBuf::from("/work/.git/xtask-cache/generation-1")),
            "/work/.git/xtask-cache/generation-1"
        );
    }

    fn fixture() -> Result<(tempfile::TempDir, PathBuf, CacheManifest)> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join(".git"))?;
        fs::create_dir_all(root.join(".config"))?;
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
        fs::write(
            root.join(".config/xtask.toml"),
            r#"[ext.xtask.cache]
extra_inputs = []
keep_generations = 2
generation_grace_secs = 3600
"#,
        )?;
        let config = XtaskCacheConfig {
            extra_inputs: Vec::new(),
            keep_generations: 2,
            generation_grace_secs: 3600,
        };
        let manifest = CacheManifest::from_roots(&root, &config, vec![PathBuf::from("xtask")])?;
        Ok((temp, root, manifest))
    }

    #[test]
    fn failed_activation_preserves_the_active_locator() -> Result<()> {
        let (_temp, root, manifest) = fixture()?;
        let old = root.join("old");
        let new = root.join("new");
        fs::write(&old, b"old")?;
        fs::write(&new, b"new")?;
        publish(&root, &old, &manifest)?;
        let locator = layout::locator(&root)?;
        let before = fs::read(&locator)?;
        let activation_called = Cell::new(false);

        let result = publish_with_activation(&root, &new, &manifest, |_, _, _| {
            activation_called.set(true);
            Err(anyhow!("forced activation failure"))
        });

        assert!(result.is_err());
        assert!(activation_called.get());
        assert_eq!(fs::read(locator)?, before);
        Ok(())
    }

    /// A CI runner cleans the working tree before every job.
    ///
    /// The generations live in the Git directory, which the clean spares. A
    /// pointer kept in the working tree did not survive it, so the cached
    /// binary was present and unreachable and every job rebuilt it - the
    /// whole dependency fetch included. Here the working tree is emptied the
    /// way the runner empties it, and the generation must still be found.
    #[test]
    fn the_active_generation_survives_a_working_tree_clean() -> Result<()> {
        let (_temp, root, manifest) = fixture()?;
        let source = root.join("source");
        fs::write(&source, b"binary")?;
        publish(&root, &source, &manifest)?;

        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }

        let generation = layout::active(&root)?;
        assert_eq!(fs::read(generation.binary)?, b"binary");
        Ok(())
    }

    #[test]
    fn published_generation_contains_a_coherent_manifest_and_stamp() -> Result<()> {
        let (_temp, root, manifest) = fixture()?;
        let source = root.join("source");
        fs::write(&source, b"binary")?;

        let published = publish(&root, &source, &manifest)?;
        let generation = layout::active(&root)?;

        assert_eq!(published, generation.binary);
        assert_eq!(fs::read(generation.binary)?, b"binary");
        assert_eq!(generation.manifest.source_stamp, manifest.source_stamp);
        Ok(())
    }

    #[test]
    fn ambiguous_activation_failure_keeps_a_resolvable_generation() -> Result<()> {
        let (_temp, root, manifest) = fixture()?;
        let source = root.join("source");
        fs::write(&source, b"binary")?;
        let locator = layout::locator(&root)?;

        let result = publish_with_activation(&root, &source, &manifest, |_, generation, _| {
            fs::write(&locator, format!("{}\n", generation.display()))?;
            Err(anyhow!("forced post-rename sync failure"))
        });

        assert!(result.is_err());
        let generation = layout::active(&root)?;
        assert_eq!(fs::read(generation.binary)?, b"binary");
        Ok(())
    }
}
