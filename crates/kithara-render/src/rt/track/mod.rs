mod consumer;
mod core;
mod fade;
mod feeder;
mod gate;
mod output;
mod read;
mod sink;
mod view;

#[cfg(test)]
mod legacy_tests;

pub use core::PlayerTrack;

pub use consumer::PcmConsumer;
pub use feeder::{PlayerResource, ReadOutcome};
pub(crate) use read::TrackBuffers;
pub use read::TrackReadOutcome;
pub use sink::RtSink;

#[cfg(test)]
mod tests;
