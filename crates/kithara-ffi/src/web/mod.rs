//! Browser/wasm bindings for the kithara player.
//!
//! Single structural cfg boundary lives on `pub mod web;` in
//! [`crate::lib`]. Inside this module all sources are unconditionally
//! wasm-only and require no per-item gating.

pub mod bindings;
pub(crate) mod bridge;
pub(crate) mod commands;
pub(crate) mod inner;
pub(crate) mod js;
pub(crate) mod key_processor_bridge;
pub(crate) mod observer;
pub mod player;
pub mod queue;
pub mod surface;
pub(crate) mod worker;
