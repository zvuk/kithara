#[cfg(test)]
use std::cell::Cell;
use std::{
    cell::OnceCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use cargo_metadata::Metadata;

use super::{
    super::config::ArchConfig, arc_clone_hotspots, args_wrapper_struct, cancel_root_sites,
    canonical_types, cfg_density, dead_exports, direction, duplicate_error_enums,
    field_always_constant, field_always_equals_other_field, field_passthrough, file_density,
    file_size, firewheel_dsp_facade, flat_directory, fn_arg_count, generic_param_count, god_module,
    god_struct, god_trait, max_nesting, mixed_entities, module_fan_out, module_layers,
    multi_constructor, no_lib_statics, platform_layer_hygiene, pub_struct_open_fields,
    readme_presence, redundant_accessors, redundant_reexport, shared_state, single_impl_size,
    single_word_filenames, smoothing_primitive_sites, stray_rs_files, tokio_dep_quarantine,
    trait_impl_count,
};
use crate::common::{
    fix::FixOutcome, scope::Scope, violation::Violation, walker::workspace_rs_files_scoped,
};

struct ParsedSource {
    syntax: Option<syn::File>,
    source: String,
}

enum FileSnapshot {
    Loaded(ParsedSource),
    Unreadable(std::io::Error),
}

struct ParsedFiles<'a> {
    files: OnceCell<BTreeMap<PathBuf, FileSnapshot>>,
    scope: &'a Scope,
    workspace_root: &'a Path,
    #[cfg(test)]
    parse_count: Cell<usize>,
}

impl<'a> ParsedFiles<'a> {
    fn new(workspace_root: &'a Path, scope: &'a Scope) -> Self {
        Self {
            scope,
            workspace_root,
            files: OnceCell::new(),
            #[cfg(test)]
            parse_count: Cell::new(0),
        }
    }

    fn all(&self) -> Result<&BTreeMap<PathBuf, FileSnapshot>> {
        if let Some(files) = self.files.get() {
            return Ok(files);
        }
        let paths = workspace_rs_files_scoped(self.workspace_root, self.scope)?;
        Ok(self.files.get_or_init(|| {
            paths
                .into_iter()
                .map(|path| {
                    let snapshot = match fs::read_to_string(&path) {
                        Ok(source) => {
                            #[cfg(test)]
                            self.parse_count.set(self.parse_count.get() + 1);
                            let syntax = syn::parse_file(&source).ok();
                            FileSnapshot::Loaded(ParsedSource { syntax, source })
                        }
                        Err(error) => FileSnapshot::Unreadable(error),
                    };
                    (path, snapshot)
                })
                .collect()
        }))
    }

    fn get(&self, path: &Path) -> Result<Option<&FileSnapshot>> {
        Ok(self.all()?.get(path))
    }

    #[cfg(test)]
    fn parse_count(&self) -> usize {
        self.parse_count.get()
    }

    fn parsed_file(&self, path: &Path) -> Result<Option<&syn::File>> {
        Ok(self.get(path)?.and_then(|snapshot| match snapshot {
            FileSnapshot::Loaded(parsed) => parsed.syntax.as_ref(),
            FileSnapshot::Unreadable(_) => None,
        }))
    }

    fn parsed_source(&self, path: &Path) -> Result<Option<(&str, &syn::File)>> {
        Ok(self.get(path)?.and_then(|snapshot| match snapshot {
            FileSnapshot::Loaded(parsed) => parsed
                .syntax
                .as_ref()
                .map(|syntax| (parsed.source.as_str(), syntax)),
            FileSnapshot::Unreadable(_) => None,
        }))
    }

    fn source_file(&self, path: &Path) -> Result<Option<(&str, Option<&syn::File>)>> {
        match self.get(path)? {
            Some(FileSnapshot::Loaded(parsed)) => {
                Ok(Some((parsed.source.as_str(), parsed.syntax.as_ref())))
            }
            Some(FileSnapshot::Unreadable(error)) => {
                Err(anyhow!("read {}: {error}", path.display()))
            }
            None => Ok(None),
        }
    }
}

pub(crate) struct Context<'a> {
    pub(crate) config: &'a ArchConfig,
    pub(crate) metadata: &'a Metadata,
    pub(crate) workspace_root: &'a Path,
    pub(crate) scope: &'a Scope,
    parsed_files: ParsedFiles<'a>,
}

impl<'a> Context<'a> {
    pub(in crate::arch) fn new(
        config: &'a ArchConfig,
        metadata: &'a Metadata,
        workspace_root: &'a Path,
        scope: &'a Scope,
    ) -> Self {
        Self {
            config,
            metadata,
            workspace_root,
            scope,
            parsed_files: ParsedFiles::new(workspace_root, scope),
        }
    }

    delegate::delegate! {
        to self.parsed_files {
            pub(super) fn parsed_file(&self, path: &Path) -> Result<Option<&syn::File>>;
            pub(super) fn parsed_source(&self, path: &Path) -> Result<Option<(&str, &syn::File)>>;
            pub(super) fn source_file(&self, path: &Path) -> Result<Option<(&str, Option<&syn::File>)>>;
        }
    }
}

pub(crate) trait Check {
    /// Apply an automatic fix. Default is a no-op (read-only check). `apply`
    /// distinguishes a dry run (report only) from writing changes to disk.
    fn fix(&self, _ctx: &Context<'_>, _apply: bool) -> Result<FixOutcome> {
        Ok(FixOutcome::default())
    }
    fn id(&self) -> &'static str;

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>>;
}

pub(crate) fn registry() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(cancel_root_sites::CancelRootSites),
        Box::new(smoothing_primitive_sites::SmoothingPrimitiveSites),
        Box::new(firewheel_dsp_facade::FirewheelDspFacade),
        Box::new(platform_layer_hygiene::PlatformLayerHygiene),
        Box::new(tokio_dep_quarantine::TokioDepQuarantine),
        Box::new(cfg_density::CfgDensity),
        Box::new(dead_exports::DeadExports),
        Box::new(direction::Direction),
        Box::new(args_wrapper_struct::ArgsWrapperStruct),
        Box::new(canonical_types::CanonicalTypes),
        Box::new(arc_clone_hotspots::ArcCloneHotspots),
        Box::new(fn_arg_count::FnArgCount),
        Box::new(generic_param_count::GenericParamCount),
        Box::new(god_module::GodModule),
        Box::new(god_struct::GodStruct),
        Box::new(god_trait::GodTrait),
        Box::new(module_fan_out::ModuleFanOut),
        Box::new(multi_constructor::MultiConstructor),
        Box::new(no_lib_statics::NoLibStatics),
        Box::new(pub_struct_open_fields::PubStructOpenFields),
        Box::new(trait_impl_count::TraitImplCount),
        Box::new(duplicate_error_enums::DuplicateErrorEnums),
        Box::new(field_always_constant::FieldAlwaysConstant),
        Box::new(field_always_equals_other_field::FieldAlwaysEqualsOtherField),
        Box::new(field_passthrough::FieldPassthrough),
        Box::new(stray_rs_files::StrayRsFiles),
        Box::new(file_size::FileSize),
        Box::new(flat_directory::FlatDirectory),
        Box::new(shared_state::SharedState),
        Box::new(max_nesting::MaxNesting),
        Box::new(readme_presence::ReadmePresence),
        Box::new(file_density::FileDensity),
        Box::new(mixed_entities::MixedEntities),
        Box::new(redundant_accessors::RedundantAccessors),
        Box::new(redundant_reexport::RedundantReexport),
        Box::new(single_impl_size::SingleImplSize),
        Box::new(single_word_filenames::SingleWordFilenames),
        Box::new(module_layers::ModuleLayers),
    ]
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parsed_files_parse_each_path_once() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("crates/fixture/src/lib.rs");
        fs::create_dir_all(path.parent().expect("fixture source directory"))
            .expect("create fixture source directory");
        fs::write(&path, "pub struct Fixture;\n").expect("write fixture");
        let scope = Scope::default();
        let files = ParsedFiles::new(dir.path(), &scope);

        assert!(files.parsed_file(&path).expect("first lookup").is_some());
        assert!(files.parsed_file(&path).expect("second lookup").is_some());
        assert_eq!(files.parse_count(), 1);
    }

    #[test]
    fn parsed_files_keep_source_and_syntax_in_one_snapshot() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("crates/fixture/src/lib.rs");
        fs::create_dir_all(path.parent().expect("fixture source directory"))
            .expect("create fixture source directory");
        fs::write(&path, "pub struct Before;\n").expect("write initial fixture");
        let scope = Scope::default();
        let files = ParsedFiles::new(dir.path(), &scope);

        let (source, syntax) = files
            .parsed_source(&path)
            .expect("first lookup")
            .expect("parsed fixture");
        assert!(source.contains("Before"));
        assert!(matches!(
            syntax.items.first(),
            Some(syn::Item::Struct(item)) if item.ident == "Before"
        ));

        fs::write(&path, "pub struct After;\n").expect("mutate fixture");
        let (source, syntax) = files
            .parsed_source(&path)
            .expect("second lookup")
            .expect("cached fixture");

        assert!(source.contains("Before"));
        assert!(!source.contains("After"));
        assert!(matches!(
            syntax.items.first(),
            Some(syn::Item::Struct(item)) if item.ident == "Before"
        ));
        assert_eq!(files.parse_count(), 1);
    }
}
