use std::{fmt, num::NonZeroUsize, path::PathBuf};

use kithara::bufpool::Region;
use kithara_assets::{AssetLayoutRegistry, AssetStore, AssetStoreBuilder, StorageBackend};
use kithara_platform::{CancelScope, sync::Arc};

use super::FfiAssetLayoutRegistry;

struct Consts;

impl Consts {
    const ASSET_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(128).unwrap();
}

/// Shareable Rust-owned asset store used by one or more players.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct FfiAssetStore {
    inner: AssetStore,
    region: Region,
    shutdown: CancelScope,
}

impl FfiAssetStore {
    fn build(root: Option<String>, layouts: AssetLayoutRegistry) -> Self {
        let backend = root.map_or_else(StorageBackend::default, |root| StorageBackend::Disk {
            root: PathBuf::from(root),
        });
        Self::build_with_backend(backend, layouts)
    }

    fn build_with_backend(backend: StorageBackend, layouts: AssetLayoutRegistry) -> Self {
        let region = Region::default();
        let shutdown = CancelScope::new(None);
        let inner = AssetStoreBuilder::default()
            .backend(backend)
            .cache_capacity(Consts::ASSET_CACHE_CAPACITY)
            .cancel(shutdown.token())
            .layouts(layouts)
            .pool(region.byte_pool())
            .build();
        Self {
            inner,
            region,
            shutdown,
        }
    }

    pub(crate) fn handle(&self) -> &AssetStore {
        &self.inner
    }

    pub(crate) fn region(&self) -> &Region {
        &self.region
    }

    #[cfg(test)]
    pub(crate) fn cancel_token(&self) -> kithara_platform::CancelToken {
        self.shutdown.token()
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl FfiAssetStore {
    /// Create an asset store rooted at `root` with a snapshot of `layouts`.
    #[must_use]
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new(root: Option<String>, layouts: Arc<FfiAssetLayoutRegistry>) -> Arc<Self> {
        let snapshot = layouts.snapshot();
        drop(layouts);
        Arc::new(Self::build(root, snapshot))
    }
}

impl Default for FfiAssetStore {
    fn default() -> Self {
        Self::build(None, AssetLayoutRegistry::default())
    }
}

impl fmt::Debug for FfiAssetStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FfiAssetStore")
            .field("root", &self.inner.root_dir())
            .finish_non_exhaustive()
    }
}

impl Drop for FfiAssetStore {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;

    use kithara_assets::{AssetLayout, AssetResource, AssetSource, DefaultLayout};
    use tempfile::tempdir;
    use url::Url;

    use super::*;
    use crate::{
        asset::FfiAssetLayoutTarget,
        layout::{FfiAssetLayout, FfiAssetResource, FfiAssetSource},
    };

    struct FixedLayout {
        tag: &'static str,
        _lifetime: Arc<()>,
    }

    impl FfiAssetLayout for FixedLayout {
        fn root(&self, _source: FfiAssetSource) -> String {
            format!("{}-root", self.tag)
        }

        fn path(&self, _resource: FfiAssetResource) -> String {
            format!("{}/resource.bin", self.tag)
        }
    }

    fn layout(tag: &'static str) -> (Arc<FixedLayout>, Weak<()>) {
        let lifetime = Arc::new(());
        (
            Arc::new(FixedLayout {
                tag,
                _lifetime: Arc::clone(&lifetime),
            }),
            Arc::downgrade(&lifetime),
        )
    }

    fn remote_source() -> AssetSource {
        AssetSource::Remote {
            url: Url::parse("https://example.com/audio.mp3").expect("valid URL"),
            discriminator: None,
        }
    }

    fn assert_layout<T: 'static>(store: &FfiAssetStore, root: &str, path: &str) {
        let scope = store
            .handle()
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

    #[kithara::test]
    fn explicit_root_is_applied() {
        let dir = tempdir().expect("temp dir");
        let store = FfiAssetStore::new(
            Some(dir.path().to_string_lossy().into_owned()),
            FfiAssetLayoutRegistry::new(),
        );

        assert_eq!(store.handle().root_dir(), dir.path());
    }

    #[kithara::test]
    fn platform_default_root_is_preserved() {
        let store = FfiAssetStore::default();
        let StorageBackend::Disk { root } = StorageBackend::default() else {
            panic!("native storage defaults to disk");
        };

        assert_eq!(store.handle().root_dir(), root);
    }

    #[kithara::test]
    fn shared_cache_capacity_is_installed() {
        let store = FfiAssetStore::build_with_backend(
            StorageBackend::Memory,
            AssetLayoutRegistry::default(),
        );

        assert_eq!(
            store.handle().ephemeral_cache_capacity(),
            Some(Consts::ASSET_CACHE_CAPACITY)
        );
    }

    #[kithara::test]
    fn store_snapshots_registry_and_retains_foreign_layout() {
        let registry = FfiAssetLayoutRegistry::new();
        let (first, lifetime) = layout("first");
        registry.register(FfiAssetLayoutTarget::File, first.clone());
        drop(first);
        let store = FfiAssetStore::new(None, Arc::clone(&registry));

        let (second, _) = layout("second");
        registry.register(FfiAssetLayoutTarget::File, second);
        let second_store = FfiAssetStore::new(None, Arc::clone(&registry));
        drop(registry);

        assert!(lifetime.upgrade().is_some());
        assert_layout::<kithara::file::File>(&store, "first-root", "first/resource.bin");
        assert_layout::<kithara::file::File>(&second_store, "second-root", "second/resource.bin");

        drop(store);
        assert!(lifetime.upgrade().is_none());
    }

    #[kithara::test]
    fn targets_are_registered_independently() {
        let registry = FfiAssetLayoutRegistry::new();
        let (file, _) = layout("file");
        let (hls, _) = layout("hls");
        registry.register(FfiAssetLayoutTarget::File, file);
        registry.register(FfiAssetLayoutTarget::Hls, hls);
        let store = FfiAssetStore::new(None, registry);

        assert_layout::<kithara::file::File>(&store, "file-root", "file/resource.bin");
        assert_layout::<kithara::hls::Hls>(&store, "hls-root", "hls/resource.bin");
    }

    #[kithara::test]
    fn repeated_registration_replaces_target_layout() {
        let registry = FfiAssetLayoutRegistry::new();
        let (first, _) = layout("first");
        let (second, _) = layout("second");
        registry.register(FfiAssetLayoutTarget::File, first);
        registry.register(FfiAssetLayoutTarget::File, second);
        let store = FfiAssetStore::new(None, registry);

        assert_layout::<kithara::file::File>(&store, "second-root", "second/resource.bin");
    }

    #[kithara::test]
    fn omitted_layout_uses_default_layout() {
        struct TestProtocol;

        let store = FfiAssetStore::default();
        let source = remote_source();
        let root = DefaultLayout.root(&source);

        assert_layout::<TestProtocol>(&store, &root, "track/track.mp3");
    }

    #[kithara::test]
    fn dropping_store_cancels_its_subtree() {
        let store = Arc::new(FfiAssetStore::default());
        let cancel = store.cancel_token();

        drop(store);

        assert!(cancel.is_cancelled());
    }
}
