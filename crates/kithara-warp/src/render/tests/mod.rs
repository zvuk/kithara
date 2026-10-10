#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
mod backend;
mod exact;
mod fixtures;
#[cfg(feature = "stretch-identity")]
mod identity;
mod playback;
mod projection;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
mod target;
mod timeline;
mod trajectory;

use fixtures::{WarpRenderer, chunk, f64_of, renderer, spec};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use fixtures::{dominant_bin, expected_bin, flush_serviced, render_serviced};

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use crate::WarpConfig;
