#[cfg(feature = "resample-rubato")]
mod selected {
    pub(super) type Backend = kithara_audio::RubatoBackend;
}

#[cfg(all(not(feature = "resample-rubato"), feature = "resample-glide"))]
mod selected {
    pub(super) type Backend = kithara_audio::GlideBackend;
}

#[cfg(not(any(feature = "resample-rubato", feature = "resample-glide")))]
mod selected {
    pub(super) type Backend = kithara_audio::NoResamplerBackend;
}

pub type PlaybackResamplerBackend = selected::Backend;
