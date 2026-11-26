//! `kithara-derive` — the derive macros shared by Kithara's production crates.
//!
//! `lib.rs` holds only the `#[proc_macro_derive]` entry points Rust requires in
//! a crate root and delegates to the module that owns each expansion.

mod config;
#[macro_use]
mod entrypoints;
mod event;
mod mirror;
mod phase;
mod pool;
mod ranged;
mod ui;
mod vocabulary;

#[cfg(feature = "config")]
config::retained::config_derives!();

/// Implements `Default` by calling the type's existing no-input builder.
#[cfg(feature = "built-default")]
#[proc_macro_derive(BuiltDefault)]
pub fn built_default(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    config::built::expand(input)
}

/// Generates the document patch and merge operation for a configuration struct.
#[cfg(feature = "patch")]
#[proc_macro_derive(Patch, attributes(patch, config))]
pub fn patch(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    config::expand(input)
}

ui_derives!();

/// Implements ordered traversal of frame and text-role fields in a skin structure.
#[cfg(feature = "skin-walk")]
#[proc_macro_derive(SkinWalk, attributes(skin))]
pub fn skin_walk(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    ui::skin::expand(input)
}

/// Implements one of Kithara's closed typestate phase traits.
#[cfg(feature = "phase")]
#[proc_macro_derive(Phase, attributes(phase))]
pub fn phase(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    phase::expand(input)
}

/// Implements typed construction and key dispatch for a buffer-pool schema.
///
/// The struct keeps its private `PoolSlot<K>` fields; expansion adds a
/// module-private `builder(OverallBudget)` with one required `PoolConfig`
/// setter per field and one `HasPool<K>` implementation per field.
#[cfg(feature = "pool")]
#[proc_macro_derive(Pool)]
pub fn pool(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    pool::expand(input)
}

/// Declares a bounded numeric newtype.
///
/// `checked` and `Deserialize` refuse out-of-range values; the optional
/// `clamp` flag adds a clamping `From` and requires a declared default.
#[cfg(feature = "ranged")]
#[proc_macro_derive(Ranged, attributes(ranged))]
pub fn ranged(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    ranged::expand(input)
}

/// Implements a complete structural conversion between two product models.
#[cfg(feature = "mirror")]
#[proc_macro_derive(Mirror, attributes(mirror))]
pub fn mirror(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    mirror::expand(input)
}

#[cfg(any(feature = "enum-str", feature = "variants"))]
vocabulary_derives!();

#[cfg(feature = "event")]
event_derives!();
