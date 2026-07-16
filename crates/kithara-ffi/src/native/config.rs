use std::{num::NonZeroUsize, path::PathBuf};

use kithara_assets::{
    AssetLayout, AssetLayoutRegistry, AssetStore, AssetStoreBuilder, BytePool, StorageBackend,
};
use kithara_platform::{CancelToken, sync::Arc};

use crate::{
    config::{FfiCacheConfig, FfiCacheLayoutRegistration, FfiCacheLayoutTarget},
    native::layout::ForeignLayout,
};

struct Consts;

impl Consts {
    const ASSET_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(128).unwrap();
}

pub(crate) fn build_store(
    config: &FfiCacheConfig,
    cancel: CancelToken,
    pool: BytePool,
) -> AssetStore {
    let backend = config
        .cache_dir
        .as_ref()
        .map_or_else(StorageBackend::default, |dir| StorageBackend::Disk {
            root: PathBuf::from(dir),
        });
    let layouts = layout_registry(&config.layouts);

    build_store_with_backend(backend, layouts, cancel, pool)
}

fn layout_registry(registrations: &[FfiCacheLayoutRegistration]) -> AssetLayoutRegistry {
    let mut layouts = AssetLayoutRegistry::default();
    for registration in registrations {
        let layout =
            Arc::new(ForeignLayout::new(Arc::clone(&registration.layout))) as Arc<dyn AssetLayout>;
        match registration.target {
            FfiCacheLayoutTarget::File => layouts.register::<kithara::file::File>(layout),
            FfiCacheLayoutTarget::Hls => layouts.register::<kithara::hls::Hls>(layout),
        }
    }
    layouts
}

fn build_store_with_backend(
    backend: StorageBackend,
    layouts: AssetLayoutRegistry,
    cancel: CancelToken,
    pool: BytePool,
) -> AssetStore {
    AssetStoreBuilder::default()
        .backend(backend)
        .cache_capacity(Consts::ASSET_CACHE_CAPACITY)
        .cancel(cancel)
        .layouts(layouts)
        .pool(pool)
        .build()
}

#[cfg(test)]
mod tests {
    use kithara_assets::{AssetLayout, AssetResource, AssetSource, DefaultLayout};
    use tempfile::tempdir;
    use url::Url;

    use super::*;
    use crate::layout::{FfiAssetLayout, FfiAssetResource, FfiAssetSource};

    struct TestProtocol;

    struct FixedLayout(&'static str);

    impl FfiAssetLayout for FixedLayout {
        fn root(&self, _source: FfiAssetSource) -> String {
            format!("{}-root", self.0)
        }

        fn path(&self, _resource: FfiAssetResource) -> String {
            format!("{}/resource.bin", self.0)
        }
    }

    fn registration(target: FfiCacheLayoutTarget, tag: &'static str) -> FfiCacheLayoutRegistration {
        FfiCacheLayoutRegistration {
            target,
            layout: Arc::new(FixedLayout(tag)),
        }
    }

    fn remote_source() -> AssetSource {
        AssetSource::Remote {
            url: Url::parse("https://example.com/audio.mp3").expect("valid URL"),
            discriminator: None,
        }
    }

    fn assert_layout<T: 'static>(store: &AssetStore, root: &str, path: &str) {
        let scope = store
            .scope::<T>(&remote_source())
            .expect("valid asset scope");
        let key = scope
            .key(&AssetResource::Source {
                extension: "mp3".to_string(),
            })
            .expect("valid resource key");

        assert_eq!(scope.asset_root(), root);
        assert_eq!(key.rel_path(), Some(path));
    }

    fn assert_default_layout<T: 'static>(store: &AssetStore) {
        let source = remote_source();
        let root = DefaultLayout.root(&source);
        assert_layout::<T>(store, &root, "track/track.mp3");
    }

    #[kithara::test]
    fn build_store_applies_explicit_cache_dir() {
        let dir = tempdir().expect("temp dir");
        let config = FfiCacheConfig {
            cache_dir: Some(dir.path().to_string_lossy().into_owned()),
            layouts: Vec::new(),
        };

        let store = build_store(&config, CancelToken::never(), BytePool::default());

        assert_eq!(store.root_dir(), dir.path());
    }

    #[kithara::test]
    fn build_store_preserves_the_platform_default_cache_dir() {
        let store = build_store(
            &FfiCacheConfig::default(),
            CancelToken::never(),
            BytePool::default(),
        );
        let StorageBackend::Disk { root } = StorageBackend::default() else {
            panic!("native storage defaults to disk");
        };

        assert_eq!(store.root_dir(), root);
    }

    #[kithara::test]
    fn build_store_installs_shared_hls_cache_capacity() {
        let store = build_store_with_backend(
            StorageBackend::Memory,
            AssetLayoutRegistry::default(),
            CancelToken::never(),
            BytePool::default(),
        );

        assert_eq!(
            store.ephemeral_cache_capacity(),
            Some(Consts::ASSET_CACHE_CAPACITY)
        );
    }

    #[kithara::test]
    fn omitted_layouts_use_the_default_layout() {
        let store = build_store_with_backend(
            StorageBackend::Memory,
            layout_registry(&[]),
            CancelToken::never(),
            BytePool::default(),
        );

        assert_default_layout::<TestProtocol>(&store);
    }

    #[kithara::test]
    fn file_and_hls_layouts_are_registered_independently() {
        let layouts = vec![
            registration(FfiCacheLayoutTarget::File, "file"),
            registration(FfiCacheLayoutTarget::Hls, "hls"),
        ];
        let store = build_store_with_backend(
            StorageBackend::Memory,
            layout_registry(&layouts),
            CancelToken::never(),
            BytePool::default(),
        );

        assert_layout::<kithara::file::File>(&store, "file-root", "file/resource.bin");
        assert_layout::<kithara::hls::Hls>(&store, "hls-root", "hls/resource.bin");
        assert_default_layout::<TestProtocol>(&store);
    }

    #[kithara::test]
    fn duplicate_target_uses_the_last_registration() {
        let layouts = vec![
            registration(FfiCacheLayoutTarget::File, "first"),
            registration(FfiCacheLayoutTarget::File, "second"),
        ];
        let store = build_store_with_backend(
            StorageBackend::Memory,
            layout_registry(&layouts),
            CancelToken::never(),
            BytePool::default(),
        );

        assert_layout::<kithara::file::File>(&store, "second-root", "second/resource.bin");
    }

    #[kithara::test]
    fn cache_directory_and_layout_registry_are_independent() {
        let dir = tempdir().expect("temp dir");
        let config = FfiCacheConfig {
            cache_dir: Some(dir.path().to_string_lossy().into_owned()),
            layouts: vec![registration(FfiCacheLayoutTarget::Hls, "custom")],
        };
        let store = build_store(&config, CancelToken::never(), BytePool::default());

        assert_eq!(store.root_dir(), dir.path());
        assert_layout::<kithara::hls::Hls>(&store, "custom-root", "custom/resource.bin");
    }
}
