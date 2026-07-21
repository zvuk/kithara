#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

use super::fingerprint;

struct Consts;

impl Consts {
    const BINARY: &'static str = "xtask";
    const CACHE_DIRECTORY: &'static str = "kithara-agent-hook";
    const CACHE_ENV: &'static str = "KITHARA_AGENT_HOOK_CACHE";
    const CURRENT: &'static str = "current";
    const FINGERPRINT: &'static str = "fingerprint";
    const GENERATION_PREFIX: &'static str = "generation-";
    const INACTIVE_RETENTION: Duration = Duration::from_secs(60 * 60);
    const ROOT_ENV: &'static str = "KITHARA_AGENT_HOOK_ROOT";
}

pub(super) fn install() -> Result<()> {
    if env::var_os(Consts::CACHE_ENV).is_some() {
        bail!("run installation through `cargo xtask agent-hook install`");
    }
    let root = project_root()?;
    let cache = cache_dir(&root)?;
    let source = env::current_exe().context("resolve current xtask executable")?;
    let fingerprint = fingerprint::compute(&root)?;
    let binary = publish(&source, &cache, &fingerprint)?;
    println!("installed agent hook at {}", binary.display());
    Ok(())
}

pub(super) fn warn_if_stale() {
    let Some(cache) = env::var_os(Consts::CACHE_ENV).filter(|value| !value.is_empty()) else {
        return;
    };
    let state = project_root().and_then(|root| is_current(&root, Path::new(&cache)));
    match state {
        Ok(true) => {}
        Ok(false) => warn_reinstall(None),
        Err(error) => warn_reinstall(Some(&error)),
    }
}

fn project_root() -> Result<PathBuf> {
    let root = match env::var_os(Consts::ROOT_ENV).filter(|value| !value.is_empty()) {
        Some(root) => PathBuf::from(root),
        None => find_project_root(&env::current_dir().context("resolve current directory")?)?,
    };
    let root = fs::canonicalize(&root)
        .with_context(|| format!("resolve Kithara project root {}", root.display()))?;
    if !root.join("xtask/agent-hook").is_file() {
        bail!("project root does not contain xtask/agent-hook");
    }
    Ok(root)
}

fn find_project_root(start: &Path) -> Result<PathBuf> {
    start
        .ancestors()
        .find(|path| path.join("xtask/agent-hook").is_file() && path.join(".git").exists())
        .map(Path::to_path_buf)
        .with_context(|| format!("find Kithara project root from {}", start.display()))
}

fn cache_dir(root: &Path) -> Result<PathBuf> {
    Ok(resolve_git_dir(root)?.join(Consts::CACHE_DIRECTORY))
}

fn resolve_git_dir(root: &Path) -> Result<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return fs::canonicalize(&dot_git)
            .with_context(|| format!("resolve Git directory {}", dot_git.display()));
    }
    if !dot_git.is_file() {
        bail!("cannot resolve Git directory from {}", dot_git.display());
    }
    let body = fs::read_to_string(&dot_git)
        .with_context(|| format!("read Git directory file {}", dot_git.display()))?;
    let line = body.lines().next().context("Git directory file is empty")?;
    let value = line
        .strip_prefix("gitdir: ")
        .context("Git directory file has an invalid prefix")?;
    let path = PathBuf::from(value);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    fs::canonicalize(&path).with_context(|| format!("resolve Git directory {}", path.display()))
}

fn publish(source: &Path, cache: &Path, fingerprint: &str) -> Result<PathBuf> {
    publish_with_activation(source, cache, fingerprint, |temporary, current| {
        fs::rename(temporary, current).context("activate agent hook generation")
    })
}

fn publish_with_activation<F>(
    source: &Path,
    cache: &Path,
    fingerprint: &str,
    activate: F,
) -> Result<PathBuf>
where
    F: FnOnce(&Path, &Path) -> Result<()>,
{
    fs::create_dir_all(cache)
        .with_context(|| format!("create agent hook cache {}", cache.display()))?;
    cleanup_stale_artifacts(cache, Consts::INACTIVE_RETENTION)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("resolve agent hook generation time")?
        .as_nanos();
    let mut reservation = None;
    for attempt in 0..1_000 {
        let suffix = format!("{}-{timestamp}-{attempt}", std::process::id());
        let generation = format!("{}{fingerprint}-{suffix}", Consts::GENERATION_PREFIX);
        let temporary = cache.join(format!(".{generation}"));
        match fs::create_dir(&temporary) {
            Ok(()) => {
                reservation = Some((generation, temporary, suffix));
                break;
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create agent hook generation {}", temporary.display())
                });
            }
        }
    }
    let (generation, temp_generation, suffix) =
        reservation.context("reserve agent hook generation")?;
    let generation_path = cache.join(&generation);
    let temp_current = cache.join(format!(".current-{suffix}"));
    let result = (|| {
        let temp_binary = temp_generation.join(Consts::BINARY);
        fs::copy(source, &temp_binary).with_context(|| {
            format!(
                "copy current xtask from {} to {}",
                source.display(),
                temp_binary.display()
            )
        })?;
        make_executable(&temp_binary)?;
        let temp_fingerprint = temp_generation.join(Consts::FINGERPRINT);
        fs::write(&temp_fingerprint, format!("{fingerprint}\n")).with_context(|| {
            format!(
                "write agent hook fingerprint {}",
                temp_fingerprint.display()
            )
        })?;
        fs::rename(&temp_generation, &generation_path).context("publish agent hook generation")?;
        create_current_link(&generation, &temp_current)?;
        activate(&temp_current, &cache.join(Consts::CURRENT))?;
        cleanup_legacy_cache(cache);
        Ok(generation_path.join(Consts::BINARY))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temp_generation);
        let _ = fs::remove_dir_all(&generation_path);
        let _ = fs::remove_file(&temp_current);
    }
    result
}

fn cleanup_stale_artifacts(cache: &Path, retention: Duration) -> Result<()> {
    let current = current_generation(cache).ok().flatten();
    for entry in
        fs::read_dir(cache).with_context(|| format!("read agent hook cache {}", cache.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("read agent hook generation type"),
        };
        let is_generation = file_type.is_dir() && name.starts_with(Consts::GENERATION_PREFIX);
        let is_temporary = (file_type.is_dir() && name.starts_with(".generation-"))
            || (file_type.is_symlink() && name.starts_with(".current-"));
        if (!is_generation && !is_temporary) || current.as_deref() == Some(name.as_ref()) {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("read agent hook generation metadata"),
        };
        let old_enough = metadata
            .modified()?
            .elapsed()
            .is_ok_and(|age| age >= retention);
        if old_enough {
            let removed = if file_type.is_dir() {
                fs::remove_dir_all(entry.path())
            } else {
                fs::remove_file(entry.path())
            };
            match removed {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "remove stale agent hook artifact {}",
                            entry.path().display()
                        )
                    });
                }
            }
        }
    }
    Ok(())
}

fn current_generation(cache: &Path) -> Result<Option<String>> {
    let current = cache.join(Consts::CURRENT);
    let generation = match fs::read_link(&current) {
        Ok(generation) => generation,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read agent hook pointer {}", current.display()));
        }
    };
    let generation = generation
        .to_str()
        .context("agent hook pointer is not valid UTF-8")?;
    anyhow::ensure!(
        generation.starts_with(Consts::GENERATION_PREFIX)
            && Path::new(generation).components().count() == 1,
        "agent hook pointer is invalid"
    );
    Ok(Some(generation.to_owned()))
}

#[cfg(unix)]
fn create_current_link(generation: &str, path: &Path) -> Result<()> {
    symlink(generation, path)
        .with_context(|| format!("write agent hook pointer {}", path.display()))
}

#[cfg(not(unix))]
fn create_current_link(_generation: &str, _path: &Path) -> Result<()> {
    bail!("agent hook installation requires a Unix host")
}

fn cleanup_legacy_cache(cache: &Path) {
    for name in [Consts::BINARY, Consts::FINGERPRINT, "build.lock"] {
        let _ = fs::remove_file(cache.join(name));
    }
    let _ = fs::remove_dir_all(cache.join("target"));
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn is_current(root: &Path, cache: &Path) -> Result<bool> {
    let stamp = cache.join(Consts::FINGERPRINT);
    if !stamp.is_file() {
        return Ok(false);
    }
    let installed = fs::read_to_string(&stamp)
        .with_context(|| format!("read agent hook fingerprint {}", stamp.display()))?;
    Ok(installed.trim() == fingerprint::compute(root)?)
}

fn warn_reinstall(error: Option<&anyhow::Error>) {
    if let Some(error) = error {
        eprintln!(
            "warning: cached agent hook is stale or unreadable ({error:#}); run `cargo xtask agent-hook install`"
        );
    } else {
        eprintln!("warning: cached agent hook is stale; run `cargo xtask agent-hook install`");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        fs,
        os::unix::fs::symlink,
        sync::{Arc, Barrier},
        thread,
        time::Duration,
    };

    use anyhow::{Result, anyhow};

    use super::{
        Consts, cleanup_stale_artifacts, current_generation, publish, publish_with_activation,
    };

    #[test]
    fn failed_activation_preserves_current_generation() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cache = root.path().join("cache");
        let old_source = root.path().join("old");
        let new_source = root.path().join("new");
        fs::write(&old_source, "old binary")?;
        fs::write(&new_source, "new binary")?;
        let old_binary = publish(&old_source, &cache, "old")?;
        let current = current_generation(&cache)?;

        let result = publish_with_activation(&new_source, &cache, "new", |_, _| {
            Err(anyhow!("forced activation failure"))
        });

        assert!(result.is_err());
        assert_eq!(current, current_generation(&cache)?);
        assert_eq!(fs::read_to_string(old_binary)?, "old binary");
        Ok(())
    }

    #[test]
    fn concurrent_publication_never_mixes_generation_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cache = root.path().join("cache");
        let alpha = root.path().join("alpha");
        let beta = root.path().join("beta");
        fs::write(&alpha, "alpha binary")?;
        fs::write(&beta, "beta binary")?;
        let barrier = Arc::new(Barrier::new(3));
        let handles = [(alpha, "alpha"), (beta, "beta")].map(|(source, fingerprint)| {
            let barrier = Arc::clone(&barrier);
            let cache = cache.clone();
            thread::spawn(move || {
                barrier.wait();
                publish(&source, &cache, fingerprint)
            })
        });
        barrier.wait();
        for handle in handles {
            handle.join().map_err(|_| anyhow!("publisher panicked"))??;
        }

        let generation = current_generation(&cache)?.ok_or_else(|| anyhow!("missing current"))?;
        let active = cache.join(generation);
        let binary = fs::read_to_string(active.join(Consts::BINARY))?;
        let fingerprint = fs::read_to_string(active.join(Consts::FINGERPRINT))?;
        assert!(
            matches!(
                (binary.as_str(), fingerprint.trim()),
                ("alpha binary", "alpha") | ("beta binary", "beta")
            ),
            "active generation mixed binary and fingerprint"
        );
        Ok(())
    }

    #[test]
    fn stale_artifact_cleanup_preserves_current_generation() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cache = root.path().join("cache");
        let source = root.path().join("source");
        fs::write(&source, "current binary")?;
        let current = publish(&source, &cache, "current")?;
        let inactive = cache.join("generation-orphan");
        let temporary = cache.join(".generation-orphan");
        let pointer = cache.join(".current-orphan");
        fs::create_dir(&inactive)?;
        fs::create_dir(&temporary)?;
        symlink("generation-orphan", &pointer)?;

        cleanup_stale_artifacts(&cache, Duration::ZERO)?;

        assert!(current.is_file());
        assert!(!inactive.exists());
        assert!(!temporary.exists());
        assert!(fs::symlink_metadata(pointer).is_err());
        Ok(())
    }
}
