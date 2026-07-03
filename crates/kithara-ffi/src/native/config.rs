use std::{path::PathBuf, sync::Arc};

use kithara::{assets::AssetLayout, play::ResourceConfig};

use crate::config::StoreOptions;

pub(crate) fn configure_resource(
    config: &mut ResourceConfig,
    store: &StoreOptions,
    layout: Option<&Arc<dyn AssetLayout>>,
) {
    if let Some(ref dir) = store.cache_dir {
        config.store.cache_dir = PathBuf::from(dir);
    }
    if let Some(layout) = layout {
        config.store.layout = Some(Arc::clone(layout));
    }
}

#[cfg(test)]
mod tests {
    use kithara::assets::ResourceInfo;
    use url::Url;

    use super::*;
    use crate::{
        layout::{FfiAssetLayout, FfiLayout, FfiResourceInfo},
        native::layout::resolve_layout,
    };

    #[kithara::test]
    fn configure_resource_applies_explicit_cache_dir() {
        let store = StoreOptions {
            cache_dir: Some("/tmp/kithara-ffi-config-test".to_owned()),
            layout: None,
        };
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        configure_resource(&mut config, &store, None);
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
        configure_resource(&mut config, &store, None);
        assert_eq!(config.store.cache_dir, original);
    }

    #[kithara::test]
    fn omitted_layout_leaves_store_layout_none() {
        let store = StoreOptions::default();
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        let resolved = resolve_layout(store.layout.as_ref());
        configure_resource(&mut config, &store, resolved.as_ref());
        assert!(
            config.store.layout.is_none(),
            "None layout must leave store layout untouched (DefaultLayout)"
        );
    }

    #[kithara::test]
    fn custom_layout_reaches_store_layout() {
        struct FlatLayout;
        impl FfiAssetLayout for FlatLayout {
            fn rel_path(&self, _info: FfiResourceInfo) -> String {
                "flat/track.mp3".to_string()
            }
        }
        let store = StoreOptions {
            cache_dir: None,
            layout: Some(FfiLayout::Custom {
                delegate: Arc::new(FlatLayout),
            }),
        };
        let mut config = ResourceConfig::new("https://example.com/song.mp3").unwrap();
        let resolved = resolve_layout(store.layout.as_ref());
        configure_resource(&mut config, &store, resolved.as_ref());

        let layout = config.store.layout.expect("custom layout must reach store");
        let url = Url::parse("https://example.com/song.mp3").unwrap();
        let rel = layout.rel_path(&ResourceInfo::Track {
            url: &url,
            name: None,
            ext_hint: Some("mp3"),
        });
        assert_eq!(rel, "flat/track.mp3");
    }
}
