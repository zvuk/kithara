use kithara::platform::sync::Arc;

use crate::{
    asset::FfiAssetStore,
    types::{FfiActionAtItemEnd, FfiCrossfadeSettings, FfiKeyOptions, FfiPlaybackOrder},
};

/// FFI-friendly player configuration.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiPlayerConfig {
    /// Shared asset store used by every item created by this player.
    pub store: Arc<FfiAssetStore>,
    /// DRM key handling. Pass an empty [`FfiKeyOptions`] when no DRM is needed.
    pub key_options: FfiKeyOptions,
    /// Number of EQ bands (log-spaced), at most 128. Default: 10.
    pub eq_band_count: u32,
    /// Player-wide auth token merged into item HTTP headers. Empty means no token.
    pub auth_token: String,
    /// Initial playback-rate target (1.0 = normal).
    pub playing_rate: f32,
    pub playback_order: FfiPlaybackOrder,
    pub action_at_item_end: FfiActionAtItemEnd,
    pub crossfade_settings: FfiCrossfadeSettings,
}

#[cfg(test)]
impl FfiPlayerConfig {
    pub(crate) fn for_test() -> Self {
        super::session::initialize_test_host();
        Self {
            eq_band_count: 10,
            auth_token: String::new(),
            playing_rate: kithara::play::DEFAULT_PLAYING_RATE,
            key_options: FfiKeyOptions::default(),
            store: Arc::new(FfiAssetStore::for_test()),
            playback_order: FfiPlaybackOrder::Sequential,
            action_at_item_end: FfiActionAtItemEnd::Advance,
            crossfade_settings: FfiCrossfadeSettings::default(),
        }
    }
}
