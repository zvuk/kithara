use std::fmt;

use kithara_assets::{AssetLayout, AssetResource, AssetSource, DefaultLayout};
use kithara_platform::sync::Arc;

use crate::layout::FfiAssetLayout;

/// Adapts a foreign [`FfiAssetLayout`] to the core [`AssetLayout`] trait;
/// `rel_path` runs once per resource-key derivation, not per sample.
pub(crate) struct ForeignLayout(Arc<dyn FfiAssetLayout>);

impl fmt::Debug for ForeignLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForeignLayout").finish_non_exhaustive()
    }
}

impl AssetLayout for ForeignLayout {
    fn root(&self, source: &AssetSource) -> String {
        DefaultLayout.root(source)
    }

    fn path(&self, resource: &AssetResource) -> String {
        match resource {
            AssetResource::Url(url) => self.0.rel_path(url.as_str().to_string()),
            resource => DefaultLayout.path(resource),
        }
    }
}

/// Resolve the optional foreign delegate into a shared core layout. `None`
/// keeps the store's [`DefaultLayout`].
pub(crate) fn resolve_layout(
    layout: Option<&Arc<dyn FfiAssetLayout>>,
) -> Option<Arc<dyn AssetLayout>> {
    layout.map(|delegate| Arc::new(ForeignLayout(Arc::clone(delegate))) as Arc<dyn AssetLayout>)
}

#[cfg(test)]
mod tests {
    use kithara_assets::{
        AcquisitionResult, AssetLayoutRegistry, AssetStoreBuilder, AssetsError,
        ResourceAcquisition, StorageBackend, WriteSide,
    };
    use tempfile::tempdir;
    use url::Url;

    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("valid test URL")
    }

    fn delegate<L: FfiAssetLayout + 'static>(layout: L) -> Arc<dyn FfiAssetLayout> {
        Arc::new(layout)
    }

    #[kithara::test]
    fn resolve_layout_none_keeps_the_store_default() {
        assert!(resolve_layout(None).is_none());
    }

    #[kithara::test]
    fn foreign_layout_receives_the_resource_url() {
        struct EchoLayout;
        impl FfiAssetLayout for EchoLayout {
            fn rel_path(&self, url: String) -> String {
                url
            }
        }

        let layout =
            resolve_layout(Some(&delegate(EchoLayout))).expect("delegate resolves to Some");
        let u = url("https://zvuk.com/42/stream/hq/seg7.mp4");
        let rel = layout.path(&AssetResource::Url(u));
        assert_eq!(rel, "https://zvuk.com/42/stream/hq/seg7.mp4");
    }

    /// Stream `data` through the Pending writer and commit it.
    fn write_commit(acq: ResourceAcquisition, data: &[u8]) {
        let AcquisitionResult::Pending(w) = acq else {
            panic!("expected a Pending writer");
        };
        w.write_at(0, data).expect("write");
        w.commit(Some(data.len() as u64)).expect("commit");
    }

    #[kithara::test(native, timeout(kithara_platform::time::Duration::from_secs(5)))]
    fn foreign_layout_dictates_real_on_disk_path() {
        struct LeafFromUrl;
        impl FfiAssetLayout for LeafFromUrl {
            fn rel_path(&self, url: String) -> String {
                let leaf = url.rsplit('/').next().unwrap_or("res");
                format!("custom/{leaf}")
            }
        }

        let dir = tempdir().expect("tempdir");
        let layout =
            resolve_layout(Some(&delegate(LeafFromUrl))).expect("delegate resolves to Some");
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .layouts(AssetLayoutRegistry::new(layout))
            .build();

        let u = url("https://example.com/audio.mp3");
        let source = AssetSource::Remote {
            url: u.clone(),
            discriminator: Some("root".to_string()),
        };
        let scope = store
            .scope::<LeafFromUrl>(&source)
            .expect("valid foreign scope");
        let key = scope
            .key(&AssetResource::Url(u))
            .expect("valid foreign key");
        write_commit(
            scope.store().acquire_resource(&key, None).expect("acquire"),
            b"payload",
        );

        let expected = dir
            .path()
            .join(scope.asset_root())
            .join("custom")
            .join("audio.mp3");
        assert!(
            expected.exists(),
            "foreign layout must dictate the real on-disk path: {}",
            expected.display()
        );
    }

    #[kithara::test(native, timeout(kithara_platform::time::Duration::from_secs(5)))]
    #[case("../escape")]
    #[case("/absolute/path")]
    #[case("")]
    #[case("dir/../escape")]
    fn hostile_foreign_layout_is_rejected(#[case] hostile: &'static str) {
        struct HostileForeign(&'static str);
        impl FfiAssetLayout for HostileForeign {
            fn rel_path(&self, _url: String) -> String {
                self.0.to_string()
            }
        }

        let dir = tempdir().expect("tempdir");
        let layout = resolve_layout(Some(&delegate(HostileForeign(hostile))))
            .expect("delegate resolves to Some");
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .layouts(AssetLayoutRegistry::new(layout))
            .build();

        let u = url("https://example.com/audio.mp3");
        let source = AssetSource::Remote {
            url: u.clone(),
            discriminator: Some("root".to_string()),
        };
        let scope = store
            .scope::<HostileForeign>(&source)
            .expect("valid foreign scope");
        let err = scope
            .key(&AssetResource::Url(u))
            .expect_err("hostile path must be rejected");
        assert!(
            matches!(err, AssetsError::InvalidKey),
            "expected InvalidKey, got {err:?}"
        );
    }
}
