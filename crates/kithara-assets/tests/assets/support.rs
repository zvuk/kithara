use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use kithara_assets::{
    AcquisitionResult, AssetLayout, AssetLayoutRegistry, AssetResource, AssetResourceState,
    AssetScope, AssetSource, AssetStore, Assets, AssetsResult, ReadSide, ResourceKey,
    StorageBackend, WriteSide,
    index::{PinDurability, PinsIndex as InnerPinsIndex},
};
use kithara_platform::sync::Arc;
use kithara_test_utils::{
    TestTempDir,
    bufpool::{TestPools, pools},
};
use url::Url;

#[path = "../support/xor.rs"]
mod xor;

pub(super) use xor::xor_processor;

const RESOURCE_NAMESPACE: &str = "test-resource";

#[derive(Debug)]
pub(super) struct LiteralLayout;

impl AssetLayout for LiteralLayout {
    fn root(&self, source: &AssetSource) -> String {
        let AssetSource::Remote {
            discriminator: Some(root),
            ..
        } = source
        else {
            panic!("literal test layout requires an explicit root")
        };
        root.clone()
    }

    fn path(&self, resource: &AssetResource) -> String {
        let AssetResource::Named { namespace, name } = resource else {
            panic!("literal test layout requires a named resource")
        };
        assert_eq!(namespace, RESOURCE_NAMESPACE);
        name.clone()
    }
}

pub(super) fn literal_layouts() -> AssetLayoutRegistry {
    AssetLayoutRegistry::new(Arc::new(LiteralLayout))
}

pub(super) fn source(asset_root: &str) -> AssetSource {
    AssetSource::Remote {
        url: Url::parse("https://cache.test/source").expect("valid test URL"),
        discriminator: Some(asset_root.to_string()),
    }
}

pub(super) fn resource(path: impl Into<String>) -> AssetResource {
    AssetResource::Named {
        namespace: RESOURCE_NAMESPACE.to_string(),
        name: path.into(),
    }
}

pub(super) fn pending<W: WriteSide>(acquisition: AcquisitionResult<W, W::Reader>) -> W {
    let AcquisitionResult::Pending(writer) = acquisition else {
        panic!("expected a Pending writer")
    };
    writer
}

pub(super) fn asset_scope(temp_dir: &TestTempDir, asset_root: &str) -> AssetScope<TestPools> {
    #[cfg(not(target_arch = "wasm32"))]
    let backend = StorageBackend::Disk {
        root: temp_dir.path().into(),
    };
    #[cfg(target_arch = "wasm32")]
    let backend = {
        let _ = temp_dir;
        StorageBackend::Memory
    };

    AssetStore::builder(pools())
        .backend(backend)
        .layouts(literal_layouts())
        .build()
        .scope::<LiteralLayout>(&source(asset_root))
        .expect("scope")
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn asset_dir_exists(root: &Path, asset_root: &str) -> bool {
    root.join(asset_root).exists()
}

/// Reads up to `len` bytes from `reader` at `offset`.
pub(super) fn read_bytes<R: ReadSide>(reader: &R, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = pools()
        .get_with_len::<u8>(len)
        .expect("read buffer fits the test pool budget");
    let read = reader.read_at(offset, &mut buf).unwrap_or(0);
    buf[..read].to_vec()
}

/// `true` when `key` resolves to a committed resource.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn has_committed_resource(store: &AssetStore<TestPools>, key: &ResourceKey) -> bool {
    matches!(
        store.resource_state(key),
        Ok(AssetResourceState::Committed { .. })
    )
}

/// Disk-backed pins index at `<root_dir>/_index/pins.bin` of `assets`, with
/// whole-set `load` / `store`.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct PinsIndex {
    inner: InnerPinsIndex,
}

#[cfg(not(target_arch = "wasm32"))]
impl PinsIndex {
    pub(super) fn open<A: Assets>(assets: &A) -> AssetsResult<Self> {
        let path = assets.root_dir().join("_index").join("pins.bin");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create the pins index directory");
        }
        Ok(Self {
            inner: InnerPinsIndex::with_persist_at(path, pools().get::<u8>()),
        })
    }

    pub(super) fn load(&self) -> HashSet<String> {
        self.inner.snapshot()
    }

    pub(super) fn store(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        let current = self.inner.snapshot();
        for outdated in current.difference(pins) {
            self.inner.remove(outdated, PinDurability::Durable)?;
        }
        for added in pins.difference(&current) {
            self.inner.add(added, PinDurability::Durable)?;
        }
        self.inner.flush()
    }
}
