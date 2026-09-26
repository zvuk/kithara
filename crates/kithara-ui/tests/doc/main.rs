//! Documents, their compilation and the built-in presets, driven through the
//! public API.

mod builtin_presets;
#[path = "../common/mod.rs"]
mod common;
mod compile;
mod document_group;
mod document_measured;
mod document_object;
mod document_placed;
mod document_wave;
mod envelope;
mod expand_kind;
mod module_doc;
mod motion;
mod multi_deck;
mod package;
mod roundtrip;
mod skin;
mod skin_custom;
mod skin_document;
#[cfg(not(target_arch = "wasm32"))]
mod source_file;
mod source_mem;
mod source_overlay;
mod swatch;
mod text;
mod validate;
