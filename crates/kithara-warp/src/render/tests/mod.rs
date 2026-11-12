#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
mod backend;
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-glide"))]
mod entry;
mod exact;
pub(in crate::render) mod fixtures;
#[cfg(feature = "stretch-identity")]
mod playback;
mod projection;
mod retirement;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
mod target;
mod timeline;
mod trajectory;

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use fixtures::{LaneFeed, dominant_bin, drain_tail, expected_bin, flush_serviced, samples};
use fixtures::{WarpRenderer, chunk, f64_of, renderer, spec};

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use crate::WarpConfig;
