use std::path::PathBuf;

use kithara::play::ResourceConfig;

/// Store configuration forwarded from platform layer to resource creation.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Record))]
pub struct StoreOptions {
    pub cache_dir: Option<String>,
}

pub(crate) fn configure_resource(config: &mut ResourceConfig, store: &StoreOptions) {
    if let Some(ref dir) = store.cache_dir {
        config.store.cache_dir = PathBuf::from(dir);
    }
    // Dev-mode `insecure = true` is applied on the shared `Downloader` in
    // `AudioPlayer::new`, not here — `ResourceConfig` has no `net` field.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[kithara::test]
    fn configure_resource_applies_explicit_cache_dir() {
        let store = StoreOptions {
            cache_dir: Some("/tmp/kithara-ffi-config-test".to_owned()),
        };
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        configure_resource(&mut config, &store);
        assert_eq!(
            config.store.cache_dir,
            PathBuf::from("/tmp/kithara-ffi-config-test")
        );
    }

    #[kithara::test]
    fn configure_resource_preserves_default_when_unset() {
        let store = StoreOptions::default();
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        let original = config.store.cache_dir.clone();
        configure_resource(&mut config, &store);
        assert_eq!(config.store.cache_dir, original);
    }
}
