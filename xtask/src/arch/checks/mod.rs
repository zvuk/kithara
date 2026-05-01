//! Registry of architectural checks.
//!
//! Each check implements `Check`. The runner iterates the registry and
//! aggregates `Violation`s into a `Report`.

use std::path::Path;

use anyhow::Result;
use cargo_metadata::Metadata;

use super::config::ArchConfig;
use crate::common::{scope::Scope, violation::Violation};

pub(crate) mod arc_clone_hotspots;
pub(crate) mod canonical_types;
pub(crate) mod direction;
pub(crate) mod duplicate_error_enums;
pub(crate) mod file_density;
pub(crate) mod file_size;
pub(crate) mod flat_directory;
pub(crate) mod max_nesting;
pub(crate) mod mixed_entities;
pub(crate) mod module_layers;
pub(crate) mod readme_presence;
pub(crate) mod redundant_accessors;
pub(crate) mod redundant_reexport;
pub(crate) mod shared_state;
pub(crate) mod single_impl_size;
pub(crate) mod single_word_filenames;
pub(crate) mod stray_rs_files;

pub(crate) struct Context<'a> {
    pub(crate) workspace_root: &'a Path,
    pub(crate) metadata: &'a Metadata,
    pub(crate) config: &'a ArchConfig,
    pub(crate) scope: &'a Scope,
}

pub(crate) trait Check {
    fn id(&self) -> &'static str;
    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>>;
}

pub(crate) fn registry() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(direction::Direction),
        Box::new(canonical_types::CanonicalTypes),
        Box::new(arc_clone_hotspots::ArcCloneHotspots),
        Box::new(duplicate_error_enums::DuplicateErrorEnums),
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
