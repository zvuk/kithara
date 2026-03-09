use std::{path::PathBuf, sync::LazyLock};

use kithara::play::ResourceConfig;
use kithara_platform::Mutex;

#[derive(Clone, Debug, Default)]
pub(crate) struct FfiStoreOptions {
    pub(crate) cache_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FfiConfig {
    pub(crate) store: FfiStoreOptions,
}

static FFI_CONFIG: LazyLock<Mutex<FfiConfig>> = LazyLock::new(|| Mutex::new(FfiConfig::default()));

#[cfg(any(target_os = "android", test))]
pub(crate) fn set_store_options(store: FfiStoreOptions) {
    FFI_CONFIG.lock_sync().store = store;
}

pub(crate) fn configure_resource(config: &mut ResourceConfig) {
    if let Some(cache_dir) = FFI_CONFIG.lock_sync().store.cache_dir.clone() {
        config.store.cache_dir = cache_dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[kithara::test]
    fn configure_resource_applies_explicit_cache_dir_and_preserves_default_when_unset() {
        let custom_cache_dir = PathBuf::from("/tmp/kithara-ffi-config-test");
        set_store_options(FfiStoreOptions {
            cache_dir: Some(custom_cache_dir.clone()),
        });

        let mut configured = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        configure_resource(&mut configured);
        assert_eq!(configured.store.cache_dir, custom_cache_dir);

        set_store_options(FfiStoreOptions::default());

        let mut defaulted = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        let original_cache_dir = defaulted.store.cache_dir.clone();
        configure_resource(&mut defaulted);
        assert_eq!(defaulted.store.cache_dir, original_cache_dir);
    }
}
