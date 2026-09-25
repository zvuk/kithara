use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::Result;

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{relative_to, walk_rs_files},
};

pub(crate) const ID: &str = "split_module";

/// A module whose declaring file sits beside its own directory: `foo.rs` next
/// to `foo/`. The module is read from two places, which is the shape a split
/// left half done. The directory takes the module instead: `foo/mod.rs`
/// declares and re-exports its named implementation files. A crate root such
/// as `tests/foo.rs` moves to `tests/foo/main.rs`.
pub(crate) struct SplitModule;

impl Check for SplitModule {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let files = ctx.scan.text_files(ctx.scope)?;
        let crate_roots: HashSet<&Path> = ctx
            .metadata
            .workspace_packages()
            .into_iter()
            .flat_map(|package| &package.targets)
            .map(|target| target.src_path.as_std_path())
            .collect();
        split_modules(
            ctx.workspace_root,
            &crate_roots,
            files.iter().map(PathBuf::as_path),
        )
    }

    fn uses_global_lint_excludes(&self) -> bool {
        false
    }
}

fn split_modules<'a>(
    workspace_root: &Path,
    crate_roots: &HashSet<&Path>,
    files: impl Iterator<Item = &'a Path>,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for path in files {
        let Some(stem) = module_stem(path) else {
            continue;
        };
        let dir = path.with_file_name(stem);
        if !dir.is_dir() || walk_rs_files(&dir)?.is_empty() {
            continue;
        }
        let key = relative_to(workspace_root, path)
            .to_string_lossy()
            .replace('\\', "/");
        let dir_key = relative_to(workspace_root, &dir)
            .to_string_lossy()
            .replace('\\', "/");
        let target = if crate_roots.contains(path) {
            "main.rs"
        } else {
            "mod.rs"
        };
        let remedy = if target == "mod.rs" {
            format!(
                "put declarations and re-exports in `{dir_key}/{target}`, and implementation in a named file"
            )
        } else {
            format!("move the crate root to `{dir_key}/{target}`")
        };
        violations.push(Violation::deny(
            ID,
            key.clone(),
            format!("`{key}` sits beside its own directory; {remedy}"),
        ));
    }
    Ok(violations)
}

/// The module name a `.rs` file declares, or `None` for a file that already
/// names its directory's module rather than a sibling one.
fn module_stem(path: &Path) -> Option<&str> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    (!matches!(stem, "mod" | "lib" | "main")).then_some(stem)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn run(root: &Path, crate_roots: &[&str], files: &[&str]) -> Vec<Violation> {
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture dir");
            fs::write(&path, "").expect("write fixture");
        }
        let paths: Vec<PathBuf> = files.iter().map(|file| root.join(file)).collect();
        let roots: Vec<PathBuf> = crate_roots.iter().map(|file| root.join(file)).collect();
        let roots: HashSet<&Path> = roots.iter().map(PathBuf::as_path).collect();
        split_modules(root, &roots, paths.iter().map(PathBuf::as_path)).expect("scan")
    }

    #[test]
    fn denies_a_module_file_beside_its_directory() {
        let dir = tempfile::tempdir().expect("tempdir");

        let violations = run(
            dir.path(),
            &[],
            &["crates/a/src/pool.rs", "crates/a/src/pool/slot.rs"],
        );

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].key, "crates/a/src/pool.rs");
        assert!(violations[0].message.contains("`crates/a/src/pool/mod.rs`"));
        assert!(
            violations[0]
                .message
                .contains("implementation in a named file")
        );
    }

    #[test]
    fn accepts_a_module_owned_by_its_directory() {
        let dir = tempfile::tempdir().expect("tempdir");

        let violations = run(
            dir.path(),
            &[],
            &[
                "crates/a/src/pool/mod.rs",
                "crates/a/src/pool/core.rs",
                "crates/a/src/pool/slot.rs",
            ],
        );

        assert!(violations.is_empty());
    }

    #[test]
    fn accepts_a_directory_without_rust_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = dir.path().join("crates/a/src/pool/data.bin");
        fs::create_dir_all(fixture.parent().expect("fixture parent")).expect("create fixture dir");
        fs::write(&fixture, "").expect("write fixture");

        let violations = run(dir.path(), &[], &["crates/a/src/pool.rs"]);

        assert!(violations.is_empty());
    }

    #[test]
    fn sends_a_crate_root_to_main_rs() {
        let dir = tempfile::tempdir().expect("tempdir");

        let violations = run(
            dir.path(),
            &["crates/a/tests/gallery.rs"],
            &[
                "crates/a/tests/gallery.rs",
                "crates/a/tests/gallery/checks.rs",
            ],
        );

        assert_eq!(violations.len(), 1);
        assert!(
            violations[0]
                .message
                .contains("crates/a/tests/gallery/main.rs")
        );
        assert!(!violations[0].message.contains("mod.rs"));
    }

    #[test]
    fn sends_a_module_under_tests_to_mod_rs() {
        let dir = tempfile::tempdir().expect("tempdir");

        let violations = run(
            dir.path(),
            &["crates/a/tests/suite.rs"],
            &[
                "crates/a/tests/suite.rs",
                "crates/a/tests/continuity.rs",
                "crates/a/tests/continuity/timeline.rs",
            ],
        );

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].key, "crates/a/tests/continuity.rs");
        assert!(
            violations[0]
                .message
                .contains("`crates/a/tests/continuity/mod.rs`")
        );
    }
}
