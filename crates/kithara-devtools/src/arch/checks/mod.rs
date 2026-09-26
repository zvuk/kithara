//! Registry of architectural checks.
//!
//! Each check implements `Check`. The runner iterates the registry and
//! aggregates `Violation`s into a `Report`.

pub(crate) mod arc_clone_hotspots;
pub(crate) mod args_wrapper_struct;
pub(crate) mod cancel_root_sites;
pub(crate) mod canonical_types;
pub(crate) mod cfg_density;
mod context;
pub(crate) mod dead_exports;
pub(crate) mod direction;
pub(crate) mod duplicate_error_enums;
pub(crate) mod field_always_constant;
pub(crate) mod field_always_equals_other_field;
pub(crate) mod field_passthrough;
pub(crate) mod file_density;
pub(crate) mod file_size;
pub(crate) mod firewheel_dsp_facade;
pub(crate) mod flat_directory;
pub(crate) mod fn_arg_count;
pub(crate) mod generic_param_count;
pub(crate) mod god_module;
pub(crate) mod god_struct;
pub(crate) mod god_trait;
pub(crate) mod max_nesting;
pub(crate) mod mixed_entities;
pub(crate) mod module_fan_out;
pub(crate) mod module_layers;
pub(crate) mod multi_constructor;
pub(crate) mod no_lib_statics;
pub(crate) mod platform_layer_hygiene;
pub(crate) mod pub_struct_open_fields;
pub(crate) mod readme_presence;
pub(crate) mod redundant_accessors;
pub(crate) mod redundant_reexport;
pub(crate) mod shared_state;
pub(crate) mod single_impl_size;
pub(crate) mod single_word_filenames;
pub(crate) mod smoothing_primitive_sites;
pub(crate) mod stray_rs_files;
pub(crate) mod struct_index;
pub(crate) mod tokio_dep_quarantine;
pub(crate) mod trait_impl_count;

pub(crate) use context::{Check, Context, registry};
