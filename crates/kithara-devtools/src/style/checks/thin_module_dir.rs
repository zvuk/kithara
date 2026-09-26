use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use super::{Check, Context};
use crate::common::{violation::Violation, walker::relative_to};

pub(crate) mod consts {
    pub(crate) const ID: &str = "thin_module_dir";
}

/// A module directory that holds only its `mod.rs`, or `mod.rs` and one more
/// source file. The directory adds a level without splitting anything, so the
/// module folds into one `foo.rs` beside it. A module directory directly under
/// a directory cargo scans for targets keeps its shape: `tests/common.rs`
/// would be a test target of its own.
pub(crate) struct ThinModuleDir;

impl Check for ThinModuleDir {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        /// Directories cargo scans for targets of their own: a module folded
        /// out of one of them would become a target.
        const TARGET_DIRS: [&str; 4] = ["tests", "benches", "examples", "src/bin"];

        let files = ctx.scan.text_files(ctx.scope)?;
        let target_dirs: HashSet<PathBuf> = ctx
            .metadata
            .workspace_packages()
            .into_iter()
            .filter_map(|package| package.manifest_path.parent())
            .flat_map(|dir| TARGET_DIRS.map(|target| dir.as_std_path().join(target)))
            .collect();
        thin_module_dirs(
            ctx.workspace_root,
            &target_dirs,
            files.iter().map(PathBuf::as_path),
        )
    }

    fn uses_global_lint_excludes(&self) -> bool {
        false
    }
}

fn thin_module_dirs<'a>(
    workspace_root: &Path,
    target_dirs: &HashSet<PathBuf>,
    files: impl Iterator<Item = &'a Path>,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for path in files {
        if path.file_name().and_then(|name| name.to_str()) != Some("mod.rs") {
            continue;
        }
        let Some(dir) = path.parent() else {
            continue;
        };
        if dir
            .parent()
            .is_some_and(|parent| target_dirs.contains(parent))
        {
            continue;
        }
        let entries = module_entries(dir)?;
        if entries.len() > 1 || entries.iter().any(|entry| !entry.ends_with(".rs")) {
            continue;
        }
        let dir_key = relative_to(workspace_root, dir)
            .to_string_lossy()
            .replace('\\', "/");
        let holds = entries.first().map_or_else(
            || "only `mod.rs`".to_owned(),
            |name| format!("only `mod.rs` and `{name}`"),
        );
        violations.push(Violation::deny(
            consts::ID,
            dir_key.clone(),
            format!("`{dir_key}/` holds {holds}; fold the module into `{dir_key}.rs`"),
        ));
    }
    Ok(violations)
}

/// The entries of `dir` beside its `mod.rs`, a subdirectory as `name/`.
/// Hidden entries are editor and OS litter, not module content.
fn module_entries(dir: &Path) -> Result<Vec<String>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if name == "mod.rs" || name.starts_with('.') {
            continue;
        }
        if entry.file_type()?.is_dir() {
            name.push('/');
        }
        entries.push(name);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, target_dirs: &[&str], files: &[&str]) -> Vec<Violation> {
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture dir");
            fs::write(&path, "").expect("write fixture");
        }
        let paths: Vec<PathBuf> = files.iter().map(|file| root.join(file)).collect();
        let target_dirs: HashSet<PathBuf> = target_dirs.iter().map(|dir| root.join(dir)).collect();
        thin_module_dirs(root, &target_dirs, paths.iter().map(PathBuf::as_path)).expect("scan")
    }

    #[test]
    fn denies_a_directory_with_one_source_beside_mod_rs() {
        let dir = tempfile::tempdir().expect("tempdir");

        let violations = run(
            dir.path(),
            &[],
            &["crates/a/src/pool/mod.rs", "crates/a/src/pool/slot.rs"],
        );

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].key, "crates/a/src/pool");
        assert!(violations[0].message.contains("`slot.rs`"));
        assert!(violations[0].message.contains("`crates/a/src/pool.rs`"));
    }

    #[test]
    fn denies_a_directory_with_only_mod_rs() {
        let dir = tempfile::tempdir().expect("tempdir");

        let violations = run(dir.path(), &[], &["crates/a/src/pool/mod.rs"]);

        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("holds only `mod.rs`;"));
    }

    #[test]
    fn accepts_a_directory_that_splits_the_module() {
        let dir = tempfile::tempdir().expect("tempdir");

        let violations = run(
            dir.path(),
            &[],
            &[
                "crates/a/src/pool/mod.rs",
                "crates/a/src/pool/core.rs",
                "crates/a/src/pool/slot.rs",
                "crates/a/src/cache/mod.rs",
                "crates/a/src/cache/entry.rs",
                "crates/a/src/cache/index/mod.rs",
                "crates/a/src/cache/index/key.rs",
                "crates/a/src/cache/index/slot.rs",
                "crates/a/src/shader/mod.rs",
                "crates/a/src/shader/blit.rs",
                "crates/a/src/shader/blit.wgsl",
            ],
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn ignores_hidden_entries() {
        let dir = tempfile::tempdir().expect("tempdir");

        let violations = run(
            dir.path(),
            &[],
            &[
                "crates/a/src/pool/mod.rs",
                "crates/a/src/pool/slot.rs",
                "crates/a/src/pool/.DS_Store",
            ],
        );

        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn keeps_a_module_directory_under_a_target_directory() {
        let dir = tempfile::tempdir().expect("tempdir");

        let violations = run(
            dir.path(),
            &["crates/a/tests"],
            &[
                "crates/a/tests/common/mod.rs",
                "crates/a/tests/common/registry.rs",
                "crates/a/tests/suite/main.rs",
                "crates/a/tests/suite/common/mod.rs",
                "crates/a/tests/suite/common/registry.rs",
            ],
        );

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].key, "crates/a/tests/suite/common");
    }
}
