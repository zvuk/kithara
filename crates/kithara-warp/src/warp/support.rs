/// Whether this target includes a Warp rendering backend that changes rate.
#[must_use]
pub const fn supports_playback_rate() -> bool {
    cfg!(any(
        feature = "stretch-signalsmith",
        feature = "stretch-bungee",
        feature = "stretch-glide"
    ))
}
