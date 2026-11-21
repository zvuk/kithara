mod core;
mod cut;
mod fade;
mod feeder;
mod gate;
mod motion;
mod output;
mod read;
mod sink;
mod view;

#[cfg(test)]
mod legacy_tests;

pub use core::PlayerTrack;

pub(in crate::rt) use cut::{CutPlan, CutPoint, CutPrefix, scan_cut};
pub(crate) use feeder::ReadPcm;
pub use feeder::{PlayerResource, ReadOutcome};
pub(in crate::rt) use motion::same_motion;
pub(crate) use read::TrackBuffers;
pub use sink::RtSink;

#[cfg(test)]
mod tests;
