//! A job's own build directory under a build root, which every build reaches
//! through one fixed path: the root's `build` link, pointed at the job's
//! directory when the job enters it.
//!
//! The compiler cache keys a Rust compilation on the paths the compiler is
//! given, so builds that all see the one alias share their keys across lanes
//! and runners, while each lane keeps a directory of its own that no other
//! lane's build overwrites. A runner runs one job at a time, so the alias has
//! one writer.

mod entry;
#[cfg(test)]
pub(crate) mod fixture;
mod garbage;
mod sources;
mod target;
mod timings;

pub(crate) use entry::BuildDir;
pub(crate) use sources::{Claim, claim_beside_alias};
pub(crate) use target::{LaneTarget, Target};
