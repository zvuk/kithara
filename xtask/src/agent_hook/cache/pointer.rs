use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};

struct Consts;

impl Consts {
    const FILE: &'static str = "xtask/.agent-hook-cache";
    const TEMPORARY_PREFIX: &'static str = ".agent-hook-cache.";
}

pub(super) fn activate(root: &Path, generation: &Path, suffix: &str) -> Result<()> {
    let generation = fs::canonicalize(generation)
        .with_context(|| format!("resolve agent hook generation {}", generation.display()))?;
    let pointer = path(root);
    let temporary = pointer.with_file_name(format!("{}{suffix}", Consts::TEMPORARY_PREFIX));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("write agent hook pointer {}", temporary.display()))?;
        file.write_all(format!("{}\n", generation.display()).as_bytes())
            .with_context(|| format!("write agent hook pointer {}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, &pointer)
            .with_context(|| format!("activate agent hook pointer {}", pointer.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn cleanup_temporaries(root: &Path, retention: Duration) -> Result<()> {
    let directory = root.join("xtask");
    for entry in fs::read_dir(&directory)
        .with_context(|| format!("read agent hook directory {}", directory.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(Consts::TEMPORARY_PREFIX) {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("read agent hook pointer metadata"),
        };
        if !metadata
            .modified()?
            .elapsed()
            .is_ok_and(|age| age >= retention)
        {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("remove stale agent hook pointer {}", entry.path().display())
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn read(root: &Path) -> Result<PathBuf> {
    let pointer = path(root);
    let body = fs::read_to_string(&pointer)
        .with_context(|| format!("read agent hook pointer {}", pointer.display()))?;
    let generation = body
        .strip_suffix('\n')
        .filter(|value| !value.is_empty() && !value.contains('\n'))
        .context("agent hook pointer must contain one newline-terminated path")?;
    let generation = PathBuf::from(generation);
    anyhow::ensure!(
        generation.is_absolute(),
        "agent hook pointer is not absolute"
    );
    Ok(generation)
}

pub(super) fn path(root: &Path) -> PathBuf {
    root.join(Consts::FILE)
}
