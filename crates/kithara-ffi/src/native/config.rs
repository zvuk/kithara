use std::path::PathBuf;

use kithara::{
    assets::{AssetLayout, StorageBackend},
    play::ResourceConfig,
};
use kithara_platform::sync::Arc;

use crate::config::StoreOptions;

pub(crate) fn configure_resource(
    config: &mut ResourceConfig,
    store: &StoreOptions,
    layout: Option<&Arc<dyn AssetLayout>>,
) {
    if let Some(ref dir) = store.cache_dir {
        config.store_mut().backend = StorageBackend::Disk {
            root: PathBuf::from(dir),
        };
    }
    if let Some(layout) = layout {
        config.store_mut().layout = Some(Arc::clone(layout));
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;
    use crate::{layout::FfiAssetLayout, native::layout::resolve_layout};

    #[kithara::test]
    fn configure_resource_applies_explicit_cache_dir() {
        let store = StoreOptions {
            cache_dir: Some("/tmp/kithara-ffi-config-test".to_owned()),
            layout: None,
        };
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        configure_resource(&mut config, &store, None);
        assert_eq!(
            config.store().backend.clone(),
            StorageBackend::Disk {
                root: PathBuf::from("/tmp/kithara-ffi-config-test")
            }
        );
    }

    #[kithara::test]
    fn configure_resource_preserves_default_when_unset() {
        let store = StoreOptions::default();
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        let original = config.store().backend.clone();
        configure_resource(&mut config, &store, None);
        assert_eq!(config.store().backend, original);
    }

    #[kithara::test]
    fn omitted_layout_leaves_store_layout_none() {
        let store = StoreOptions::default();
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        let resolved = resolve_layout(store.layout.as_ref());
        configure_resource(&mut config, &store, resolved.as_ref());
        assert!(
            config.store().layout.is_none(),
            "None layout must leave store layout untouched (DefaultLayout)"
        );
    }

    #[kithara::test]
    fn custom_layout_reaches_store_layout() {
        struct FlatLayout;
        impl FfiAssetLayout for FlatLayout {
            fn rel_path(&self, _url: String) -> String {
                "flat/track.mp3".to_string()
            }
        }
        let store = StoreOptions {
            cache_dir: None,
            layout: Some(Arc::new(FlatLayout)),
        };
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        let resolved = resolve_layout(store.layout.as_ref());
        configure_resource(&mut config, &store, resolved.as_ref());

        let layout = config
            .store()
            .layout
            .clone()
            .expect("custom layout must reach store");
        let url = Url::parse("https://example.com/song.mp3").unwrap();
        let rel = layout.rel_path(&url);
        assert_eq!(rel, "flat/track.mp3");
    }
}
