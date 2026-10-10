mod consumer;
mod core;
mod fade;
mod feeder;
mod gate;
mod read;
mod sink;

#[cfg(test)]
mod legacy_tests;

pub use core::PlayerTrack;

pub use consumer::PcmConsumer;
pub use feeder::{PlayerResource, ReadOutcome};
pub use read::TrackReadOutcome;
pub use sink::RtSink;

#[cfg(test)]
mod tests;
