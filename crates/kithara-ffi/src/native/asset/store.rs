use std::{fmt, num::NonZeroUsize, path::PathBuf};

use kithara::{
    assets::{AssetLayoutRegistry, StorageBackend},
    bufpool::PoolError,
    platform::{CancelScope, sync::Arc},
};

use super::FfiAssetLayoutRegistry;
use crate::pools::{FfiStore, Pools, build as build_pools};

mod consts {
    use super::NonZeroUsize;

    pub(super) const ASSET_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(128).unwrap();
}

/// Shareable Rust-owned asset store used by one or more players.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct FfiAssetStore {
    shutdown: CancelScope,
    #[field(get = handle, vis = "pub(crate)")]
    inner: FfiStore,
    #[field(get, vis = "pub(crate)")]
    pools: Pools,
}

impl FfiAssetStore {
    fn build(root: Option<String>, layouts: AssetLayoutRegistry) -> Result<Self, PoolError> {
        let backend = root.map_or_else(super::super::storage::default_backend, |root| {
            StorageBackend::Disk {
                root: PathBuf::from(root),
            }
        });
        Self::build_with_backend(backend, layouts)
    }

    fn build_with_backend(
        backend: StorageBackend,
        layouts: AssetLayoutRegistry,
    ) -> Result<Self, PoolError> {
        let pools = build_pools()?;
        let shutdown = CancelScope::new(None);
        let inner = FfiStore::builder(pools.clone())
            .backend(backend)
            .cache_capacity(consts::ASSET_CACHE_CAPACITY)
            .cancel(shutdown.token())
            .layouts(layouts)
            .build();
        Ok(Self {
            shutdown,
            inner,
            pools,
        })
    }

    #[cfg(test)]
    pub(crate) fn cancel_token(&self) -> kithara::platform::CancelToken {
        self.shutdown.token()
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::build(None, AssetLayoutRegistry::default())
            .unwrap_or_else(|error| panic!("test FFI asset store initialization failed: {error}"))
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl FfiAssetStore {
    /// Create an asset store rooted at `root` with a snapshot of `layouts`.
    /// An absent root uses Documents/Files/Kithara on iOS, excluded from backup
    /// when supported. Other platforms retain their native storage default.
    ///
    /// # Panics
    ///
    /// Panics if Kithara's built-in FFI buffer pools cannot be initialized.
    #[must_use]
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new(root: Option<String>, layouts: Arc<FfiAssetLayoutRegistry>) -> Arc<Self> {
        let snapshot = layouts.snapshot();
        drop(layouts);
        Arc::new(
            Self::build(root, snapshot)
                .unwrap_or_else(|error| panic!("failed to initialize built-in FFI pools: {error}")),
        )
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

    use kithara::assets::{AssetLayout, AssetResource, AssetSource, DefaultLayout};
    use tempfile::tempdir;
    use url::Url;

    use super::*;
    use crate::{
        asset::{FfiAssetLayoutTarget, query_identity_layout},
        layout::{FfiAssetLayout, FfiAssetResource, FfiAssetSource, FfiCacheIdentityRule},
        pools::FfiPools,
    };

    struct FixedLayout {
        tag: &'static str,
        _lifetime: Arc<()>,
    }

    impl FfiAssetLayout for FixedLayout {
        fn path(&self, _resource: FfiAssetResource) -> String {
            format!("{}/resource.bin", self.tag)
        }

        fn root(&self, _source: FfiAssetSource) -> String {
            format!("{}-root", self.tag)
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
        remote_source_for("https://example.com/audio.mp3")
    }

    fn remote_source_for(value: &str) -> AssetSource {
        AssetSource::Remote {
            url: Url::parse(value).expect("valid URL"),
            discriminator: None,
        }
    }

    fn asset_root<T: 'static>(store: &FfiAssetStore, source: &AssetSource) -> String {
        store
            .handle()
            .scope::<T>(source)
            .expect("valid asset scope")
            .asset_root()
            .to_string()
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
    fn shared_cache_capacity_is_installed() {
        let store = FfiAssetStore::build_with_backend(
            StorageBackend::Memory,
            AssetLayoutRegistry::default(),
        )
        .expect("asset store");

        assert_eq!(
            store.handle().ephemeral_cache_capacity(),
            Some(consts::ASSET_CACHE_CAPACITY)
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
        assert_layout::<kithara::file::File<FfiPools>>(&store, "first-root", "first/resource.bin");
        assert_layout::<kithara::file::File<FfiPools>>(
            &second_store,
            "second-root",
            "second/resource.bin",
        );

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

        assert_layout::<kithara::file::File<FfiPools>>(&store, "file-root", "file/resource.bin");
        assert_layout::<kithara::hls::Hls<FfiPools>>(&store, "hls-root", "hls/resource.bin");
    }

    #[kithara::test]
    fn repeated_registration_replaces_target_layout() {
        let registry = FfiAssetLayoutRegistry::new();
        let (first, _) = layout("first");
        let (second, _) = layout("second");
        registry.register(FfiAssetLayoutTarget::File, first);
        registry.register(FfiAssetLayoutTarget::File, second);
        let store = FfiAssetStore::new(None, registry);

        assert_layout::<kithara::file::File<FfiPools>>(
            &store,
            "second-root",
            "second/resource.bin",
        );
    }

    #[kithara::test]
    fn omitted_layout_uses_default_layout() {
        struct TestProtocol;

        let store = FfiAssetStore::for_test();
        let source = remote_source();
        let root = DefaultLayout.root(&source);

        assert_layout::<TestProtocol>(&store, &root, "track/track.mp3");
    }

    #[kithara::test]
    fn query_identity_rules_route_through_the_native_registry() {
        let registry = FfiAssetLayoutRegistry::new();
        let layout = query_identity_layout(vec![FfiCacheIdentityRule {
            domains: vec!["media.example".to_string()],
            query_parameters: vec!["content_ref".to_string(), "edition".to_string()],
        }]);
        registry.register(FfiAssetLayoutTarget::File, Arc::clone(&layout));
        registry.register(FfiAssetLayoutTarget::Hls, layout);
        let store = FfiAssetStore::build_with_backend(StorageBackend::Memory, registry.snapshot())
            .expect("asset store");
        let first = remote_source_for(
            "https://media.example/audio.mp3?content_ref=alpha&edition=studio&signature=one",
        );
        let second = remote_source_for(
            "https://media.example/audio.mp3?content_ref=beta&edition=studio&signature=one",
        );
        let renewed = remote_source_for(
            "https://media.example/audio.mp3?signature=two&edition=studio&content_ref=alpha",
        );

        let first_root = asset_root::<kithara::file::File<FfiPools>>(&store, &first);
        let second_root = asset_root::<kithara::file::File<FfiPools>>(&store, &second);
        let renewed_root = asset_root::<kithara::file::File<FfiPools>>(&store, &renewed);
        let hls_first_root = asset_root::<kithara::hls::Hls<FfiPools>>(&store, &first);
        let hls_second_root = asset_root::<kithara::hls::Hls<FfiPools>>(&store, &second);
        let hls_renewed_root = asset_root::<kithara::hls::Hls<FfiPools>>(&store, &renewed);

        assert_ne!(first_root, second_root);
        assert_eq!(first_root, renewed_root);
        assert_ne!(hls_first_root, hls_second_root);
        assert_eq!(hls_first_root, hls_renewed_root);
    }

    #[kithara::test]
    fn dropping_store_cancels_its_subtree() {
        let store = Arc::new(FfiAssetStore::for_test());
        let cancel = store.cancel_token();

        drop(store);

        assert!(cancel.is_cancelled());
    }
}
