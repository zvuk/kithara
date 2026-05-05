//! Browser-only platform shim: gathers all wasm32-only modules under one
//! `cfg(target_arch = "wasm32")`-gated namespace.
//!
//! This crate IS the kithara browser/wasm platform layer (mirroring how
//! `kithara-platform` is the native one). The single cfg gate is in
//! [`crate::lib`] (`mod wasm;`); inside this module, all sources are
//! unconditionally wasm-only and require no per-item gating.

pub mod bindings;
pub mod commands;
pub mod js;
pub mod player;
pub mod worker;
