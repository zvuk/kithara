#![cfg(feature = "render")]

//! The two hosts laid out, drawn and driven side by side, through the public
//! API.

#[cfg(all(feature = "masonry", feature = "capture"))]
mod census;
#[path = "../common/mod.rs"]
mod common;
#[cfg(all(feature = "masonry", feature = "capture"))]
mod immediate;
mod layout;
#[cfg(all(feature = "masonry", feature = "capture"))]
mod shared;
#[cfg(all(feature = "masonry", feature = "capture"))]
mod used;
