mod core;
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
mod parity;

pub(crate) use core::with_output_source_frames;
