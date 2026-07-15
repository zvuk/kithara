use std::{num::NonZeroUsize, path::PathBuf};

use kithara_assets::{
    AssetLayout, AssetLayoutRegistry, AssetStore, AssetStoreBuilder, BytePool, StorageBackend,
};
use kithara_platform::{CancelToken, sync::Arc};

use crate::config::StoreOptions;

struct Consts;

impl Consts {
    const ASSET_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(128).unwrap();
}

pub(crate) fn build_store(
    options: &StoreOptions,
    layout: Option<Arc<dyn AssetLayout>>,
    cancel: CancelToken,
    pool: BytePool,
) -> AssetStore {
    let backend = options
        .cache_dir
        .as_ref()
        .map_or_else(StorageBackend::default, |dir| StorageBackend::Disk {
            root: PathBuf::from(dir),
        });
    let layouts = layout.map(AssetLayoutRegistry::new);

    build_store_with_backend(backend, layouts, cancel, pool)
}

fn build_store_with_backend(
    backend: StorageBackend,
    layouts: Option<AssetLayoutRegistry>,
    cancel: CancelToken,
    pool: BytePool,
) -> AssetStore {
    AssetStoreBuilder::default()
        .backend(backend)
        .cache_capacity(Consts::ASSET_CACHE_CAPACITY)
        .cancel(cancel)
        .maybe_layouts(layouts)
        .pool(pool)
        .build()
}

#[cfg(test)]
mod tests {
    use kithara_assets::{
        AcquisitionResult, AssetResource, AssetSource, ResourceAcquisition, WriteSide,
    };
    use tempfile::tempdir;
    use url::Url;

    use super::*;
    use crate::{layout::FfiAssetLayout, native::layout::resolve_layout};

    struct TestProtocol;

    fn remote_source(url: &Url) -> AssetSource {
        AssetSource::Remote {
            url: url.clone(),
            discriminator: None,
        }
    }

    fn write_commit(acquisition: ResourceAcquisition, data: &[u8]) {
        let AcquisitionResult::Pending(writer) = acquisition else {
            panic!("expected a pending writer");
        };
        writer.write_at(0, data).expect("write resource");
        writer
            .commit(Some(data.len() as u64))
            .expect("commit resource");
    }

    #[kithara::test]
    fn build_store_applies_explicit_cache_dir() {
        let dir = tempdir().expect("temp dir");
        let options = StoreOptions {
            cache_dir: Some(dir.path().to_string_lossy().into_owned()),
            layout: None,
        };

        let store = build_store(&options, None, CancelToken::never(), BytePool::default());

        assert_eq!(store.root_dir(), dir.path());
    }

    #[kithara::test]
    fn build_store_preserves_the_platform_default_cache_dir() {
        let store = build_store(
            &StoreOptions::default(),
            None,
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
            None,
            CancelToken::never(),
            BytePool::default(),
        );

        assert_eq!(
            store.ephemeral_cache_capacity(),
            Some(Consts::ASSET_CACHE_CAPACITY)
        );
    }

    #[kithara::test]
    fn omitted_layout_uses_default_layout() {
        let dir = tempdir().expect("temp dir");
        let options = StoreOptions {
            cache_dir: Some(dir.path().to_string_lossy().into_owned()),
            layout: None,
        };
        let store = build_store(&options, None, CancelToken::never(), BytePool::default());
        let url = Url::parse("https://example.com/song.mp3").expect("valid URL");
        let scope = store
            .scope::<TestProtocol>(&remote_source(&url))
            .expect("valid scope");
        let key = scope
            .key(&AssetResource::Source {
                extension: "mp3".to_string(),
            })
            .expect("valid key");
        write_commit(
            store
                .acquire_resource(&key, None)
                .expect("acquire resource"),
            b"payload",
        );

        assert!(
            dir.path()
                .join(scope.asset_root())
                .join("track/track.mp3")
                .exists()
        );
    }

    #[kithara::test]
    fn custom_layout_is_installed_on_the_shared_store() {
        struct FlatLayout;
        impl FfiAssetLayout for FlatLayout {
            fn rel_path(&self, _url: String) -> String {
                "flat/track.mp3".to_string()
            }
        }

        let dir = tempdir().expect("temp dir");
        let options = StoreOptions {
            cache_dir: Some(dir.path().to_string_lossy().into_owned()),
            layout: Some(Arc::new(FlatLayout)),
        };
        let layout = resolve_layout(options.layout.as_ref());
        let store = build_store(&options, layout, CancelToken::never(), BytePool::default());
        let url = Url::parse("https://example.com/song.mp3").expect("valid URL");
        let scope = store
            .scope::<TestProtocol>(&remote_source(&url))
            .expect("valid scope");
        let key = scope.key(&AssetResource::Url(url)).expect("valid key");
        write_commit(
            store
                .acquire_resource(&key, None)
                .expect("acquire resource"),
            b"payload",
        );

        assert!(
            dir.path()
                .join(scope.asset_root())
                .join("flat/track.mp3")
                .exists()
        );
    }
}
