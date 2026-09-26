//! Browser/wasm bindings for the kithara player.
//!
//! Single structural cfg boundary lives on `pub mod web;` in the
//! [crate root](crate). Inside this module all sources are unconditionally
//! wasm-only and require no per-item gating.

pub(crate) mod analysis;
pub mod bindings;
pub(crate) mod bridge;
pub(crate) mod commands;
pub(crate) mod inner;
pub(crate) mod interop;
pub(crate) mod keys;
pub(crate) mod observer;
pub mod surface;
pub(crate) mod worker;
