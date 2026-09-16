mod core;
mod fade;
mod feeder;
mod feeder_read;
mod read;
mod sink;
mod triggers;

pub use core::PlayerTrack;

pub use feeder::{PlayerResource, ReadOutcome};
pub(crate) use feeder_read::PreparedLaunchReadiness;
pub use read::TrackReadOutcome;
pub use sink::RtSink;
