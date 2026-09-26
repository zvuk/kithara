#![cfg(feature = "render")]

//! What a compiled document draws with (addresses, fonts, pictures, skins) and
//! the surface it mounts into, driven through the public API.

mod address;
#[path = "../common/mod.rs"]
mod common;
#[cfg(feature = "iced")]
mod fonts;
mod picture_library;
mod picture_sprite;
mod scope;
mod skin_custom;
mod skin_override;
