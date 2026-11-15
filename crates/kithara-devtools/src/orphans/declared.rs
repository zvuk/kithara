use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use syn::visit::{self, Visit};

use crate::common::exclude::module_paths;

/// Every file some `mod` declaration names, whatever gates it.
///
/// `cargo modules orphans` answers from one resolved configuration and pairs a
/// file with its parent by directory convention, so a module behind a `cfg`
/// this build does not set, or one reached through `#[path]` from a sibling
/// file, reads as unreferenced to it. Both are declared in the source, and this
/// walk reads the declaration instead of the resolution.
///
/// The build script is read alongside `src` because `cargo modules` selects
/// only a library or a binary: a file the build script alone names is
/// declared in the source, yet invisible to every target a sweep can ask
/// about.
pub(super) fn declared_files(src: &Path, build_script: Option<&Path>) -> Result<HashSet<PathBuf>> {
    let mut declared = HashSet::new();
    walk(src, &mut declared)?;
    if let Some(script) = build_script {
        let text =
            fs::read_to_string(script).with_context(|| format!("read {}", script.display()))?;
        collect(script, &text, &mut declared)?;
    }
    Ok(declared)
}

fn walk(dir: &Path, declared: &mut HashSet<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry
            .with_context(|| format!("walk {}", dir.display()))?
            .path();
        if path.is_dir() {
            walk(&path, declared)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text =
                fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            collect(&path, &text, declared)?;
        }
    }
    Ok(())
}

/// Resolve declarations from syntax so inline modules retain their directory.
fn collect(file: &Path, text: &str, declared: &mut HashSet<PathBuf>) -> Result<()> {
    let Some(dir) = file.parent() else {
        return Ok(());
    };
    let syntax = syn::parse_file(text).with_context(|| format!("parse {}", file.display()))?;
    let directory = if owns_its_directory(file) {
        dir.to_owned()
    } else {
        dir.join(file.file_stem().unwrap_or_default())
    };
    Declarations {
        directory,
        path_base: dir.to_owned(),
        files: declared,
    }
    .visit_file(&syntax);
    Ok(())
}

struct Declarations<'a> {
    directory: PathBuf,
    path_base: PathBuf,
    files: &'a mut HashSet<PathBuf>,
}

impl<'ast> Visit<'ast> for Declarations<'_> {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        let (explicit, conditional) = module_paths(&module.attrs);
        let mut paths: HashSet<_> = explicit
            .iter()
            .map(|path| normalize(&self.path_base.join(path)))
            .collect();
        let conventional = self.directory.join(module.ident.to_string());
        if module.content.is_some() {
            if paths.is_empty() || conditional {
                paths.insert(conventional);
            }
            for directory in paths {
                visit::visit_item_mod(
                    &mut Declarations {
                        path_base: directory.clone(),
                        directory,
                        files: self.files,
                    },
                    module,
                );
            }
        } else {
            if paths.is_empty() || conditional {
                paths.insert(conventional.with_extension("rs"));
                paths.insert(conventional.join("mod.rs"));
            }
            self.files.extend(paths);
        }
    }
}

fn owns_its_directory(file: &Path) -> bool {
    file.file_name()
        .is_some_and(|name| name == "mod.rs" || name == "lib.rs" || name == "main.rs")
}

/// Resolves `..` lexically, so a `#[path]` that climbs out of its directory
/// compares equal to the path the tool reports.
pub(super) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn declared(files: &[(&str, &str)]) -> HashSet<PathBuf> {
        let temp = tempdir().expect("tempdir");
        for (name, contents) in files {
            let path = temp.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create source directory");
            }
            fs::write(&path, contents).expect("write source");
        }
        let found = declared_files(temp.path(), None).expect("declared files");
        found
            .into_iter()
            .map(|path| {
                path.strip_prefix(temp.path())
                    .unwrap_or(&path)
                    .to_path_buf()
            })
            .collect()
    }

    fn names(files: &[(&str, &str)]) -> HashSet<String> {
        declared(files)
            .into_iter()
            .map(|path| path.display().to_string())
            .collect()
    }

    fn names_at_package_root(files: &[(&str, &str)], script: Option<&str>) -> HashSet<String> {
        let temp = tempdir().expect("tempdir");
        for (name, contents) in files {
            let path = temp.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create source directory");
            }
            fs::write(&path, contents).expect("write source");
        }
        let build_script = script.map(|text| {
            let path = temp.path().join("build.rs");
            fs::write(&path, text).expect("write build script");
            path
        });
        let found = declared_files(&temp.path().join("src"), build_script.as_deref())
            .expect("declared files");
        found
            .into_iter()
            .map(|path| {
                path.strip_prefix(temp.path())
                    .unwrap_or(&path)
                    .display()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn a_plain_declaration_in_a_module_root_names_its_sibling() {
        let found = names(&[("mod.rs", "mod detector;\n")]);

        assert!(found.contains("detector.rs"));
    }

    #[test]
    fn a_plain_declaration_in_a_leaf_file_names_its_own_directory() {
        let found = names(&[("scan.rs", "mod detail;\n")]);

        assert!(found.contains("scan/detail.rs"));
    }

    #[test]
    fn inline_modules_keep_the_directory_of_their_children() {
        let found = names(&[("lib.rs", "pub mod ci { pub mod bootstrap; }")]);

        assert!(found.contains("ci/bootstrap/mod.rs"));
        assert!(!found.contains("bootstrap/mod.rs"));
    }

    #[test]
    fn nested_inline_modules_in_a_leaf_keep_their_full_directory() {
        let found = names(&[("scan.rs", "mod outer { mod inner { mod detail; } }")]);

        assert!(found.contains("scan/outer/inner/detail.rs"));
    }

    #[test]
    fn inline_module_paths_resolve_inside_the_inline_directory() {
        let found = names(&[(
            "lib.rs",
            "mod ci { #[path = \"bootstrap/mod.rs\"] mod bootstrap; }",
        )]);

        assert!(found.contains("ci/bootstrap/mod.rs"));
        assert!(!found.contains("bootstrap/mod.rs"));
    }

    #[test]
    fn commented_declarations_do_not_hide_an_orphan() {
        let found = names(&[("lib.rs", "/*\nmod stray;\n*/")]);

        assert!(!found.contains("stray.rs"));
    }

    /// The gate exists to find files nothing loads, so a `cfg` that is off in
    /// this build must not turn a declared module into one.
    #[test]
    fn a_cfg_gated_declaration_still_counts_as_declared() {
        let found = names(&[("mod.rs", "#[cfg(target_os = \"android\")]\nmod android;\n")]);

        assert!(found.contains("android/mod.rs"));
    }

    #[test]
    fn a_path_attribute_resolves_against_the_declaring_file() {
        let found = names(&[(
            "mp4/scan.rs",
            "#[cfg(test)]\n#[path = \"tests.rs\"]\nmod tests;\n",
        )]);

        assert!(found.contains("mp4/tests.rs"));
    }

    #[test]
    fn a_path_attribute_climbs_out_of_its_directory() {
        let found = names(&[(
            "backend/system.rs",
            "#[path = \"../system/sync/mod.rs\"]\nmod sync;\n",
        )]);

        assert!(found.contains("system/sync/mod.rs"));
    }

    #[test]
    fn a_cfg_attr_path_counts_as_declared() {
        let found = names(&[(
            "broadcast/mod.rs",
            "#[cfg_attr(feature = \"broadcast\", path = \"live.rs\")]\nmod backend;\n",
        )]);

        assert!(found.contains("broadcast/live.rs"));
    }

    /// One module, two files: whichever feature is off, the other file is
    /// still loaded by some build.
    #[test]
    fn both_arms_of_a_swapped_module_count_as_declared() {
        let found = names(&[(
            "timestretch/mod.rs",
            "#[cfg(feature = \"bungee\")]\n#[path = \"backend.rs\"]\nmod backend;\n\
             #[cfg(not(feature = \"bungee\"))]\n#[path = \"controls.rs\"]\nmod backend;\n",
        )]);

        assert!(
            found.contains("timestretch/backend.rs") && found.contains("timestretch/controls.rs")
        );
    }

    #[test]
    fn a_file_no_declaration_names_stays_undeclared() {
        let found = names(&[("mod.rs", "mod detector;\n"), ("stray.rs", "")]);

        assert!(!found.contains("stray.rs"));
    }

    #[test]
    fn a_visibility_qualifier_does_not_hide_the_declaration() {
        let found = names(&[("mod.rs", "pub(crate) mod system;\n")]);

        assert!(found.contains("system.rs"));
    }

    /// `path = "..."` in ordinary code is a string, not a declaration.
    #[test]
    fn a_path_assignment_outside_an_attribute_is_not_a_declaration() {
        let found = names(&[("mod.rs", "fn probe() {\n    let path = \"stray.rs\";\n}\n")]);

        assert!(!found.contains("stray.rs"));
    }

    /// `cargo modules` selects only a library or a binary, so a file the build
    /// script alone names is invisible to every target a sweep can ask about
    /// while still being declared in the source.
    #[test]
    fn a_declaration_the_build_script_alone_names_counts_as_declared() {
        let found = names_at_package_root(
            &[("src/lib.rs", "")],
            Some("#[path = \"src/defs/mod.rs\"]\nmod defs;\n"),
        );

        assert!(found.contains("src/defs/mod.rs"));
    }

    #[test]
    fn the_same_file_stays_undeclared_without_the_build_script() {
        let found = names_at_package_root(&[("src/lib.rs", "")], None);

        assert!(!found.contains("src/defs/mod.rs"));
    }
}
