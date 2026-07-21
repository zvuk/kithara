use std::{
    collections::hash_map::DefaultHasher,
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

struct Consts;

impl Consts {
    const AGENT_HOOK: &'static str = "xtask/src/agent_hook.rs";
    const CARGO_CONFIG: &'static str = ".cargo/config.toml";
    const MAIN: &'static str = "xtask/src/main.rs";
    const MANIFEST: &'static str = "xtask/Cargo.toml";
    const TOOLCHAIN: &'static str = "rust-toolchain.toml";
}

pub(super) fn compute(root: &Path) -> Result<String> {
    let mut paths = Vec::new();
    for relative in [Consts::AGENT_HOOK, Consts::MAIN, Consts::MANIFEST] {
        let path = root.join(relative);
        anyhow::ensure!(path.is_file(), "fingerprint input is missing: {relative}");
        paths.push(path);
    }
    for relative in [Consts::CARGO_CONFIG, Consts::TOOLCHAIN] {
        let path = root.join(relative);
        if path.is_file() {
            paths.push(path);
        }
    }
    collect_rust_sources(&root.join("xtask/src/agent_hook"), &mut paths)?;
    paths.sort();

    let mut hasher = DefaultHasher::new();
    env::consts::OS.hash(&mut hasher);
    env::consts::ARCH.hash(&mut hasher);
    for path in paths {
        path.strip_prefix(root)
            .with_context(|| format!("make {} relative to project root", path.display()))?
            .hash(&mut hasher);
        fs::read(&path)
            .with_context(|| format!("read fingerprint input {}", path.display()))?
            .hash(&mut hasher);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn collect_rust_sources(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("read fingerprint directory {}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, paths)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            paths.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::compute;

    fn fixture() -> Result<tempfile::TempDir> {
        let root = tempfile::tempdir()?;
        fs::create_dir_all(root.path().join("xtask/src/agent_hook"))?;
        fs::write(root.path().join("xtask/Cargo.toml"), "[package]\n")?;
        fs::write(root.path().join("xtask/src/main.rs"), "fn main() {}\n")?;
        fs::write(
            root.path().join("xtask/src/agent_hook.rs"),
            "pub fn run() {}\n",
        )?;
        fs::write(
            root.path().join("xtask/src/agent_hook/input.rs"),
            "pub fn read() {}\n",
        )?;
        Ok(root)
    }

    #[test]
    fn changes_when_hook_policy_changes() -> Result<()> {
        let root = fixture()?;
        let before = compute(root.path())?;
        fs::write(
            root.path().join("xtask/src/agent_hook/input.rs"),
            "pub fn read() { println!(\"changed\"); }\n",
        )?;

        assert_ne!(before, compute(root.path())?);
        Ok(())
    }

    #[test]
    fn ignores_unrelated_xtask_sources() -> Result<()> {
        let root = fixture()?;
        let before = compute(root.path())?;
        fs::write(root.path().join("xtask/src/apple.rs"), "pub fn run() {}\n")?;

        assert_eq!(before, compute(root.path())?);
        Ok(())
    }
}
