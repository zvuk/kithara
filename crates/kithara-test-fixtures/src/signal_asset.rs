/// One generated body a test can ask for by name, and the path a server serves
/// it at.
///
/// The name travels instead of the accessor because this type compiles for
/// wasm, where the store is a host filesystem the browser cannot reach and only
/// the URL is usable. The census tests below hold the two sides together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalAsset {
    ext: &'static str,
    name: &'static str,
}

impl SignalAsset {
    pub const WAV_SAW_1S: Self = Self::new("signal_wav_saw_1s", "wav");
    pub const WAV_SILENCE_1S: Self = Self::new("signal_wav_silence_1s", "wav");
    pub const WAV_SINE440_120MS: Self = Self::new("signal_wav_sine440_120ms", "wav");
    pub const WAV_SINE440_1S: Self = Self::new("signal_wav_sine440_1s", "wav");
    pub const WAV_SINE440_60S: Self = Self::new("signal_wav_sine440_60s", "wav");
    pub const WAV_SINE880_240MS: Self = Self::new("signal_wav_sine880_240ms", "wav");
    pub const MP3_SAW_1S: Self = Self::new("signal_mp3_saw_1s", "mp3");
    pub const MP3_SAW_2S: Self = Self::new("signal_mp3_saw_2s", "mp3");
    pub const MP3_SAW_2S_64K: Self = Self::new("signal_mp3_saw_2s_64k", "mp3");
    pub const MP3_SAW_2S_320K: Self = Self::new("signal_mp3_saw_2s_320k", "mp3");
    pub const MP3_SINE1K_48K_1S: Self = Self::new("signal_mp3_sine1k_48k_1s", "mp3");
    pub const MP3_SINE440_60S: Self = Self::new("signal_mp3_sine440_60s", "mp3");
    pub const MP3_SINE440_60S_128K: Self = Self::new("signal_mp3_sine440_60s_128k", "mp3");
    pub const MP3_SINE440_60S_192K: Self = Self::new("signal_mp3_sine440_60s_192k", "mp3");
    pub const MP3_SINE440_60S_256K: Self = Self::new("signal_mp3_sine440_60s_256k", "mp3");
    pub const MP3_SINE440_60S_320K: Self = Self::new("signal_mp3_sine440_60s_320k", "mp3");
    pub const MP3_SINE880_30S: Self = Self::new("signal_mp3_sine880_30s", "mp3");
    pub const MP3_SINE880_48K_162S: Self = Self::new("signal_mp3_sine880_48k_162s", "mp3");
    pub const MP3_SWEEP_DOWN_60S: Self = Self::new("signal_mp3_sweep_down_60s", "mp3");
    pub const MP3_SWEEP_UP_60S: Self = Self::new("signal_mp3_sweep_up_60s", "mp3");
    pub const MP3_TRACK_SINE440_187S: Self = Self::new("signal_mp3_track_sine440_187s", "mp3");
    pub const FLAC_SAW_1S: Self = Self::new("signal_flac_saw_1s", "flac");
    pub const FLAC_SAW_6S: Self = Self::new("signal_flac_saw_6s", "flac");
    pub const FLAC_SAW_DOWN_6S: Self = Self::new("signal_flac_saw_down_6s", "flac");
    pub const FLAC_SINE1K_48K_1S: Self = Self::new("signal_flac_sine1k_48k_1s", "flac");
    pub const FLAC_SINE440_60S: Self = Self::new("signal_flac_sine440_60s", "flac");
    pub const AAC_SAW_1S: Self = Self::new("signal_aac_saw_1s", "aac");
    pub const AAC_SINE440_60S: Self = Self::new("signal_aac_sine440_60s", "aac");
    pub const AAC_SINE440_60S_128K: Self = Self::new("signal_aac_sine440_60s_128k", "aac");
    pub const AAC_SINE440_60S_192K: Self = Self::new("signal_aac_sine440_60s_192k", "aac");
    pub const AAC_SINE440_60S_256K: Self = Self::new("signal_aac_sine440_60s_256k", "aac");
    pub const AAC_SINE440_60S_320K: Self = Self::new("signal_aac_sine440_60s_320k", "aac");
    pub const M4A_SAW_1S: Self = Self::new("signal_m4a_saw_1s", "m4a");
    pub const M4A_SINE440_60S: Self = Self::new("signal_m4a_sine440_60s", "m4a");
    pub const M4A_SINE440_60S_128K: Self = Self::new("signal_m4a_sine440_60s_128k", "m4a");
    pub const M4A_SINE440_60S_192K: Self = Self::new("signal_m4a_sine440_60s_192k", "m4a");
    pub const M4A_SINE440_60S_256K: Self = Self::new("signal_m4a_sine440_60s_256k", "m4a");
    pub const M4A_SINE440_60S_320K: Self = Self::new("signal_m4a_sine440_60s_320k", "m4a");

    /// Every asset the `/signal` route can serve.
    pub const ALL: [Self; 38] = [
        Self::WAV_SAW_1S,
        Self::WAV_SILENCE_1S,
        Self::WAV_SINE440_120MS,
        Self::WAV_SINE440_1S,
        Self::WAV_SINE440_60S,
        Self::WAV_SINE880_240MS,
        Self::MP3_SAW_1S,
        Self::MP3_SAW_2S,
        Self::MP3_SAW_2S_64K,
        Self::MP3_SAW_2S_320K,
        Self::MP3_SINE1K_48K_1S,
        Self::MP3_SINE440_60S,
        Self::MP3_SINE440_60S_128K,
        Self::MP3_SINE440_60S_192K,
        Self::MP3_SINE440_60S_256K,
        Self::MP3_SINE440_60S_320K,
        Self::MP3_SINE880_30S,
        Self::MP3_SINE880_48K_162S,
        Self::MP3_SWEEP_DOWN_60S,
        Self::MP3_SWEEP_UP_60S,
        Self::MP3_TRACK_SINE440_187S,
        Self::FLAC_SAW_1S,
        Self::FLAC_SAW_6S,
        Self::FLAC_SAW_DOWN_6S,
        Self::FLAC_SINE1K_48K_1S,
        Self::FLAC_SINE440_60S,
        Self::AAC_SAW_1S,
        Self::AAC_SINE440_60S,
        Self::AAC_SINE440_60S_128K,
        Self::AAC_SINE440_60S_192K,
        Self::AAC_SINE440_60S_256K,
        Self::AAC_SINE440_60S_320K,
        Self::M4A_SAW_1S,
        Self::M4A_SINE440_60S,
        Self::M4A_SINE440_60S_128K,
        Self::M4A_SINE440_60S_192K,
        Self::M4A_SINE440_60S_256K,
        Self::M4A_SINE440_60S_320K,
    ];

    const fn new(name: &'static str, ext: &'static str) -> Self {
        Self { ext, name }
    }

    /// File extension the asset is stored and served under.
    #[must_use]
    pub const fn ext(self) -> &'static str {
        self.ext
    }

    /// Generator accessor name, which is also the path segment.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Path this asset is served at.
    #[must_use]
    pub fn path(self) -> String {
        format!("/signal/{}.{}", self.name, self.ext)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_test_utils::kithara;

    use super::SignalAsset;
    use crate::assets::MANIFEST;

    /// Registered signal assets, by accessor name.
    fn registered() -> Vec<&'static str> {
        MANIFEST
            .iter()
            .map(|entry| entry.name)
            .filter(|name| name.starts_with("signal_"))
            .collect()
    }

    #[kithara::test(native, flash(false))]
    fn every_declared_asset_is_registered() {
        let registered = registered();
        for asset in SignalAsset::ALL {
            assert!(
                registered.contains(&asset.name()),
                "`{}` is declared here but no generator registers it",
                asset.name(),
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn every_registered_asset_is_declared() {
        let declared: Vec<&str> = SignalAsset::ALL.iter().map(|a| a.name()).collect();
        for name in registered() {
            assert!(
                declared.contains(&name),
                "`{name}` is generated but nothing here can ask for it",
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn the_extension_matches_the_stored_entry() {
        for asset in SignalAsset::ALL {
            let entry = MANIFEST
                .iter()
                .find(|entry| entry.name == asset.name())
                .unwrap_or_else(|| panic!("`{}` is registered", asset.name()));
            assert!(
                entry.path.ends_with(asset.ext()),
                "`{}` is declared as `.{}` but stored at {}",
                asset.name(),
                asset.ext(),
                entry.path,
            );
        }
    }
}
