use std::{fs, path::PathBuf};

use cargo_metadata::MetadataCommand;

use super::DeclarationIndex;
use crate::{
    arch::{checks::Context, config::ArchConfig},
    common::scope::Scope,
};

const ROOT: &str = "crates/fixture/src/lib.rs";

fn fixture(files: &[(&str, &str)], scope: &Scope) -> (tempfile::TempDir, DeclarationIndex) {
    let dir = tempfile::tempdir().expect("workspace");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
    )
    .expect("workspace manifest");
    for (rel, content) in files {
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().expect("file parent")).expect("source directory");
        fs::write(path, content).expect("fixture source");
    }
    let metadata = MetadataCommand::new()
        .manifest_path(dir.path().join("Cargo.toml"))
        .no_deps()
        .exec()
        .expect("fixture metadata");
    let config = ArchConfig::default();
    let context = Context::new(&config, &metadata, dir.path(), scope);
    let index = DeclarationIndex::build(&context).expect("declaration graph");
    (dir, index)
}

fn single(source: &str) -> (tempfile::TempDir, DeclarationIndex) {
    fixture(
        &[
            (
                "crates/fixture/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            ),
            (ROOT, source),
        ],
        &Scope::default(),
    )
}

fn resolved(
    index: &DeclarationIndex,
    rel: &str,
    inline: &[&str],
    path: &str,
) -> Option<super::DeclarationKey> {
    index.resolve_path(
        rel,
        &inline
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>(),
        &syn::parse_str(path).expect("path"),
    )
}

#[test]
fn same_named_inline_declarations_have_distinct_keys() {
    let (_, index) = single("mod first { struct Meter; } mod second { struct Meter; }");
    let first = resolved(&index, ROOT, &[], "crate::first::Meter").expect("first declaration");
    let second = resolved(&index, ROOT, &[], "crate::second::Meter").expect("second declaration");
    assert_ne!(first, second);
    assert_eq!(first.1, ["first", "Meter"]);
    assert!(resolved(&index, ROOT, &[], "Meter").is_none());
}

#[test]
fn grouped_aliases_and_reexports_resolve_the_declaration() {
    let (_, index) = single(
        r#"
        mod model { pub struct Meter; pub struct Player; }
        mod api { pub use crate::model::{Meter as Gauge, Player}; }
        mod caller { use super::api::{Gauge as LocalMeter, Player}; }
    "#,
    );
    assert_eq!(
        resolved(&index, ROOT, &["caller"], "LocalMeter"),
        resolved(&index, ROOT, &[], "crate::model::Meter")
    );
    assert_eq!(
        resolved(&index, ROOT, &["caller"], "self::Player"),
        resolved(&index, ROOT, &[], "crate::model::Player")
    );
}

#[test]
fn known_derive_and_single_cfg_declarations_keep_identity() {
    let (_, index) = single(
        r#"
        #[derive(Clone)] struct Meter;
        #[cfg(feature = "optional")] struct Optional;
        #[cfg_attr(feature = "optional", derive(Clone))] struct ConditionalDerive;
    "#,
    );
    for name in ["Meter", "Optional", "ConditionalDerive"] {
        assert!(resolved(&index, ROOT, &[], name).is_some(), "{name}");
    }
}

#[test]
fn competing_cfg_declarations_are_ambiguous() {
    let (_, index) = single(
        r#"
        #[cfg(feature = "a")] struct Meter;
        #[cfg(not(feature = "a"))] struct Meter;
    "#,
    );
    assert!(resolved(&index, ROOT, &[], "Meter").is_none());
    assert!(index.key_for_decl(ROOT, &[], "Meter").is_none());
}

#[test]
fn unknown_attribute_and_module_macro_do_not_guess_identity() {
    let (_, index) = single("#[rewrite] struct Meter; mod nested { struct Known; } generate!();");
    assert!(resolved(&index, ROOT, &[], "Meter").is_none());
    assert!(
        index
            .key_for_decl(ROOT, &["nested".to_string()], "Known")
            .is_none()
    );
}

#[test]
fn glob_imports_aliases_and_associated_types_are_unresolved() {
    let (_, index) = single(
        r#"
        mod first { pub struct Meter; }
        mod second { pub struct Meter; }
        mod caller { use super::first::*; use super::second::*; }
        type Alias = first::Meter;
    "#,
    );
    for path in [
        "Meter",
        "Alias",
        "first::Meter::Associated",
        "foreign::Meter",
    ] {
        assert!(
            resolved(&index, ROOT, &["caller"], path).is_none(),
            "{path}"
        );
    }
    assert!(resolved(&index, ROOT, &[], "Alias").is_none());
    assert!(
        index
            .resolve_type(
                ROOT,
                &[],
                &syn::parse_str("<first::Meter as Trait>::Associated").expect("qualified type")
            )
            .is_none()
    );
}

#[test]
fn reexport_cycle_declines_instead_of_recursing() {
    let (_, index) = single(
        "mod first { pub use super::second::Meter; } mod second { pub use super::first::Meter; }",
    );
    assert!(resolved(&index, ROOT, &[], "first::Meter").is_none());
}

#[test]
fn growing_reexport_cycle_declines_at_the_repeated_binding() {
    let (_, index) = single("pub use self::looped::looped as looped;");
    assert!(resolved(&index, ROOT, &[], "looped::Meter").is_none());
}

#[test]
fn custom_cargo_root_and_exact_module_path_define_identity() {
    let root = "crates/fixture/code/entry.rs";
    let (dir, index) = fixture(
        &[
            (
                "crates/fixture/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[lib]\npath = \"code/entry.rs\"\n",
            ),
            (root, "mod inline { #[path = \"shared.rs\"] mod owned; }"),
            ("crates/fixture/code/inline/shared.rs", "pub struct Meter;"),
            ("crates/fixture/code/orphan.rs", "pub struct Meter;"),
        ],
        &Scope::default(),
    );
    let key =
        resolved(&index, root, &[], "crate::inline::owned::Meter").expect("exact module path");
    assert_eq!(
        key.0,
        fs::canonicalize(dir.path().join(root)).expect("canonical target")
    );
    assert_eq!(key.1, ["inline", "owned", "Meter"]);
    assert!(
        index
            .key_for_decl("crates/fixture/code/orphan.rs", &[], "Meter")
            .is_none()
    );
}

#[test]
fn reused_source_has_no_unique_module_context() {
    let (_, index) = fixture(
        &[
            (
                "crates/fixture/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            ),
            (
                ROOT,
                "#[path = \"shared.rs\"] mod first; #[path = \"shared.rs\"] mod second;",
            ),
            ("crates/fixture/src/shared.rs", "struct Meter;"),
        ],
        &Scope::default(),
    );
    assert!(
        index
            .key_for_decl("crates/fixture/src/shared.rs", &[], "Meter")
            .is_none()
    );
    assert!(resolved(&index, ROOT, &[], "crate::first::Meter").is_none());
}

#[test]
fn competing_module_files_and_conditional_paths_decline() {
    let (_, index) = fixture(
        &[
            (
                "crates/fixture/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            ),
            (
                ROOT,
                "mod model; #[cfg_attr(feature = \"other\", path = \"alternate.rs\")] mod selected;",
            ),
            ("crates/fixture/src/model.rs", "pub struct Meter;"),
            ("crates/fixture/src/model/mod.rs", "pub struct Meter;"),
            ("crates/fixture/src/selected.rs", "pub struct Meter;"),
            ("crates/fixture/src/alternate.rs", "pub struct Meter;"),
        ],
        &Scope::default(),
    );
    assert!(resolved(&index, ROOT, &[], "crate::model::Meter").is_none());
    assert!(resolved(&index, ROOT, &[], "crate::selected::Meter").is_none());
}

#[test]
fn narrowed_finding_scope_preserves_the_workspace_graph() {
    let (_, index) = fixture(
        &[
            (
                "crates/fixture/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            ),
            (ROOT, "mod model; mod caller;"),
            ("crates/fixture/src/model.rs", "pub struct Meter;"),
            ("crates/fixture/src/caller.rs", "use crate::model::Meter;"),
        ],
        &Scope::new(
            Vec::new(),
            vec![PathBuf::from("crates/fixture/src/caller.rs")],
        ),
    );
    assert_eq!(
        resolved(&index, "crates/fixture/src/caller.rs", &[], "Meter")
            .expect("out-of-scope declaration")
            .1,
        ["model", "Meter"]
    );
}

#[test]
fn custom_target_keeps_declared_siblings_and_leaves_orphans_unknown() {
    let (_, index) = fixture(
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"component\"]\nresolver = \"2\"\n",
            ),
            (
                "component/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[lib]\npath = \"entry.rs\"\n",
            ),
            ("component/entry.rs", "struct Known; mod sibling;"),
            ("component/sibling.rs", "pub struct Meter;"),
            ("component/orphan.rs", "pub struct Unknown;"),
        ],
        &Scope::default(),
    );
    assert!(resolved(&index, "component/entry.rs", &[], "Known").is_some());
    assert_eq!(
        resolved(&index, "component/entry.rs", &[], "sibling::Meter")
            .expect("declared sibling")
            .1,
        ["sibling", "Meter"]
    );
    assert!(
        index
            .key_for_decl("component/orphan.rs", &[], "Unknown")
            .is_none()
    );
}

#[test]
fn cargo_targets_and_dependency_renames_keep_distinct_roots() {
    let (_, index) = fixture(
        &[
            (
                "crates/fixture/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\nrenamed = { package = \"owner\", path = \"../owner\" }\n",
            ),
            (ROOT, "pub struct Meter; use renamed::Meter as Imported;"),
            (
                "crates/fixture/src/main.rs",
                "struct Meter; use fixture::Meter as LibraryMeter;",
            ),
            (
                "crates/owner/Cargo.toml",
                "[package]\nname = \"owner\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            ),
            ("crates/owner/src/lib.rs", "pub struct Meter;"),
        ],
        &Scope::default(),
    );
    let library = resolved(&index, ROOT, &[], "Meter").expect("library declaration");
    let binary =
        resolved(&index, "crates/fixture/src/main.rs", &[], "Meter").expect("binary declaration");
    assert_ne!(library.0, binary.0);
    assert_eq!(
        Some(library),
        resolved(&index, "crates/fixture/src/main.rs", &[], "LibraryMeter")
    );
    assert_eq!(
        resolved(&index, ROOT, &[], "Imported"),
        resolved(&index, "crates/owner/src/lib.rs", &[], "Meter")
    );
}

#[test]
fn functions_and_nominal_types_use_their_own_kinds() {
    let (_, index) = single("struct Meter; fn consume(_: Meter) {}");
    let consume = syn::parse_str("consume").expect("function path");
    let meter = syn::parse_str("Meter").expect("type path");
    assert!(index.resolve_function(ROOT, &[], &consume).is_some());
    assert!(index.resolve_function(ROOT, &[], &meter).is_none());
    assert!(
        index
            .resolve_type(ROOT, &[], &syn::parse_str("Meter").expect("type"))
            .is_some()
    );
    assert!(
        index
            .resolve_type(
                ROOT,
                &[],
                &syn::parse_str("consume").expect("function as type")
            )
            .is_none()
    );
}

#[test]
fn cfg_impl_attributes_keep_identity_and_unknown_transformations_decline() {
    let (_, index) = single("#[derive(Clone)] struct Meter;");
    let known = syn::parse_str("#[cfg(feature = \"optional\")] impl Meter {}").expect("cfg impl");
    let unknown = syn::parse_str("#[rewrite] impl Meter {}").expect("unknown impl attribute");
    assert!(index.resolve_impl(ROOT, &[], &known).is_some());
    assert!(index.resolve_impl(ROOT, &[], &unknown).is_none());
}

#[test]
fn builtin_root_and_function_attributes_preserve_identity_and_literal_sites() {
    let source = r#"
        #![forbid(unsafe_code)]
        #![cfg_attr(all(rtsan, not(rtsan_standalone)),
            cfg_attr(feature = "nightly", feature(sanitize)))]
        struct Meter { value: u32 }
        #[inline]
        fn first() -> Meter { Meter { value: 7 } }
        impl Meter {
            #[inline(always)]
            fn second() -> Self { Self { value: 7 } }
            #[track_caller]
            fn third() -> Self { Self { value: 7 } }
        }
    "#;
    let (dir, index) = single(source);
    let declaration = index
        .key_for_decl(ROOT, &[], "Meter")
        .expect("root declaration");
    assert!(
        index
            .resolve_function(ROOT, &[], &syn::parse_str("first").expect("function path"))
            .is_some()
    );
    let file = syn::parse_file(source).expect("source syntax");
    let implementation = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Impl(item) => Some(item),
            _ => None,
        })
        .expect("impl");
    assert_eq!(
        index.resolve_impl(ROOT, &[], implementation),
        Some(declaration.clone())
    );

    let metadata = MetadataCommand::new()
        .manifest_path(dir.path().join("Cargo.toml"))
        .no_deps()
        .exec()
        .expect("fixture metadata");
    let config = ArchConfig::default();
    let scope = Scope::default();
    let context = Context::new(&config, &metadata, dir.path(), &scope);
    let facts =
        crate::arch::checks::struct_index::build_index(&context, &[]).expect("struct index");
    assert_eq!(
        facts
            .literals
            .get(&declaration)
            .expect("same-owner literals")
            .len(),
        3
    );
    let findings = crate::arch::checks::Check::run(
        &crate::arch::checks::field_always_constant::FieldAlwaysConstant,
        &context,
    )
    .expect("real field checker");
    assert_eq!(findings.len(), 1, "{findings:?}");
}

#[test]
fn unknown_root_attribute_still_declines_decl_impl_and_function_identity() {
    let (_, index) = single("#![rewrite]\nstruct Meter; impl Meter {} fn consume(_: Meter) {}");
    assert!(index.key_for_decl(ROOT, &[], "Meter").is_none());
    assert!(
        index
            .resolve_impl(ROOT, &[], &syn::parse_str("impl Meter {}").expect("impl"))
            .is_none()
    );
    assert!(
        index
            .resolve_function(
                ROOT,
                &[],
                &syn::parse_str("consume").expect("function path")
            )
            .is_none()
    );
}
