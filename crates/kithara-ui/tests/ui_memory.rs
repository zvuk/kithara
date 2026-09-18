#![cfg(feature = "capture")]

//! Memory budget for the drawing hosts, alone in its own binary.
//!
//! The graphics device counts bytes for the whole process, so any other test
//! drawing beside this one is counted into its readings. That makes the answer
//! a property of the test schedule rather than of the host, which is why this
//! is a binary of its own holding a single test: nothing else runs while it
//! measures, and the two hosts are asked one after the other rather than at
//! once.
#[path = "../examples/gallery/app.rs"]
mod app;
#[path = "../examples/gallery/capture.rs"]
mod capture;
#[path = "../examples/gallery/cli.rs"]
mod cli;
#[path = "../examples/gallery/custom.rs"]
mod custom;
#[path = "../examples/gallery/demo/mod.rs"]
mod demo;
#[path = "../examples/gallery/fixture.rs"]
mod fixture;
#[cfg(feature = "masonry")]
#[path = "../examples/gallery/host.rs"]
mod host;
#[path = "../examples/gallery/sections.rs"]
mod sections;

// A sibling in `tests/` would be a test binary of its own, and this one
// carries the gallery modules the checks are written against. Under this
// directory cargo leaves it alone and only this binary claims it.
#[path = "ui_memory/checks.rs"]
mod checks;
