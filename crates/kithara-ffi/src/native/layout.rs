use std::{fmt, sync::Arc};

use kithara::assets::{AssetLayout, PrettyLayout, Rendition, RenditionDesc, ResourceInfo};

use crate::layout::{FfiAssetLayout, FfiLayout, FfiRendition, FfiRenditionDesc, FfiResourceInfo};

/// Adapts a foreign [`FfiAssetLayout`] to the core [`AssetLayout`] trait.
pub(crate) struct ForeignLayout(Arc<dyn FfiAssetLayout>);

impl fmt::Debug for ForeignLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForeignLayout").finish_non_exhaustive()
    }
}

impl AssetLayout for ForeignLayout {
    fn rel_path(&self, info: &ResourceInfo<'_>) -> String {
        to_ffi(info).map_or_else(
            || {
                tracing::error!("unmapped ResourceInfo variant reached ForeignLayout");
                String::new()
            },
            |ffi| self.0.rel_path(ffi),
        )
    }
}

/// Resolve an [`FfiLayout`] into a shared core layout. `None` keeps the
/// store's [`DefaultLayout`](kithara::assets::DefaultLayout).
pub(crate) fn resolve_layout(layout: Option<&FfiLayout>) -> Option<Arc<dyn AssetLayout>> {
    match layout {
        None | Some(FfiLayout::Default) => None,
        Some(FfiLayout::Pretty) => Some(Arc::new(PrettyLayout)),
        Some(FfiLayout::Custom { delegate }) => Some(Arc::new(ForeignLayout(Arc::clone(delegate)))),
    }
}

fn to_ffi(info: &ResourceInfo<'_>) -> Option<FfiResourceInfo> {
    let ffi = match info {
        ResourceInfo::Manifest { url, rendition } => FfiResourceInfo::Manifest {
            url: url.as_str().to_string(),
            rendition: rendition.as_ref().map(desc_to_ffi),
        },
        ResourceInfo::InitSegment { url, rendition } => FfiResourceInfo::InitSegment {
            url: url.as_str().to_string(),
            rendition: desc_to_ffi(rendition),
        },
        ResourceInfo::MediaSegment {
            url,
            rendition,
            sequence,
        } => FfiResourceInfo::MediaSegment {
            url: url.as_str().to_string(),
            rendition: desc_to_ffi(rendition),
            sequence: *sequence,
        },
        ResourceInfo::Key { url } => FfiResourceInfo::Key {
            url: url.as_str().to_string(),
        },
        ResourceInfo::Track {
            url,
            name,
            ext_hint,
        } => FfiResourceInfo::Track {
            url: url.as_str().to_string(),
            name: name.map(str::to_string),
            ext_hint: ext_hint.map(str::to_string),
        },
        _ => return None,
    };
    Some(ffi)
}

fn desc_to_ffi(desc: &RenditionDesc<'_>) -> FfiRenditionDesc {
    let idx = u32::try_from(desc.idx).unwrap_or_else(|_| {
        tracing::error!(idx = desc.idx, "BUG: rendition idx exceeds u32::MAX");
        0
    });
    FfiRenditionDesc {
        idx,
        siblings: desc.siblings.iter().map(rendition_to_ffi).collect(),
    }
}

fn rendition_to_ffi(rendition: &Rendition) -> FfiRendition {
    FfiRendition {
        bandwidth: rendition.bandwidth,
        name: rendition.name.clone(),
        uri_stem: rendition.uri_stem.clone(),
    }
}

#[cfg(test)]
mod tests {
    use kithara::assets::{
        AcquisitionResult, AssetResource, AssetStoreBuilder, AssetsError, WriteSide,
    };
    use tempfile::tempdir;
    use url::Url;

    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("valid test URL")
    }

    fn siblings() -> Vec<Rendition> {
        vec![
            Rendition::new(Some(1_000), Some("Lo".to_string()), None),
            Rendition::new(Some(2_000), Some("Hi".to_string()), None),
        ]
    }

    #[kithara::test]
    fn to_ffi_maps_manifest_master_and_variant() {
        let u = url("https://x/master.m3u8");
        let master = to_ffi(&ResourceInfo::Manifest {
            url: &u,
            rendition: None,
        });
        assert!(matches!(
            master,
            Some(FfiResourceInfo::Manifest { rendition: None, ref url }) if url == "https://x/master.m3u8"
        ));

        let sibs = siblings();
        let variant = to_ffi(&ResourceInfo::Manifest {
            url: &u,
            rendition: Some(RenditionDesc::new(1, &sibs)),
        });
        let Some(FfiResourceInfo::Manifest {
            rendition: Some(desc),
            ..
        }) = variant
        else {
            panic!("expected Manifest with rendition");
        };
        assert_eq!(desc.idx, 1);
        assert_eq!(desc.siblings.len(), 2);
        assert_eq!(desc.siblings[0].name.as_deref(), Some("Lo"));
        assert_eq!(desc.siblings[1].bandwidth, Some(2_000));
    }

    #[kithara::test]
    fn to_ffi_maps_init_segment() {
        let u = url("https://x/init.mp4");
        let sibs = siblings();
        let ffi = to_ffi(&ResourceInfo::InitSegment {
            url: &u,
            rendition: RenditionDesc::new(0, &sibs),
        });
        assert!(matches!(
            ffi,
            Some(FfiResourceInfo::InitSegment { rendition, ref url })
                if url == "https://x/init.mp4" && rendition.idx == 0
        ));
    }

    #[kithara::test]
    fn to_ffi_maps_media_segment_with_sequence() {
        let u = url("https://x/seg3.m4s");
        let sibs = siblings();
        let ffi = to_ffi(&ResourceInfo::MediaSegment {
            url: &u,
            rendition: RenditionDesc::new(1, &sibs),
            sequence: 42,
        });
        assert!(matches!(
            ffi,
            Some(FfiResourceInfo::MediaSegment { sequence: 42, rendition, ref url })
                if url == "https://x/seg3.m4s" && rendition.idx == 1
        ));
    }

    #[kithara::test]
    fn to_ffi_maps_key() {
        let u = url("https://x/aes/key.bin");
        let ffi = to_ffi(&ResourceInfo::Key { url: &u });
        assert!(matches!(
            ffi,
            Some(FfiResourceInfo::Key { ref url }) if url == "https://x/aes/key.bin"
        ));
    }

    #[kithara::test]
    fn to_ffi_maps_track_with_name_and_ext() {
        let u = url("https://x/song.mp3");
        let ffi = to_ffi(&ResourceInfo::Track {
            url: &u,
            name: Some("My Track"),
            ext_hint: Some("mp3"),
        });
        assert!(matches!(
            ffi,
            Some(FfiResourceInfo::Track { ref url, name: Some(ref n), ext_hint: Some(ref e) })
                if url == "https://x/song.mp3" && n == "My Track" && e == "mp3"
        ));
    }

    #[kithara::test]
    fn resolve_layout_default_is_none() {
        assert!(resolve_layout(None).is_none());
        assert!(resolve_layout(Some(&FfiLayout::Default)).is_none());
    }

    #[kithara::test]
    fn resolve_layout_pretty_is_pretty() {
        let layout = resolve_layout(Some(&FfiLayout::Pretty)).expect("pretty resolves to Some");
        let u = url("https://x/master.m3u8");
        assert_eq!(
            layout.rel_path(&ResourceInfo::Manifest {
                url: &u,
                rendition: None,
            }),
            "master.m3u8"
        );
    }

    /// Stream `data` through the Pending writer and commit it.
    fn write_commit(acq: AssetResource, data: &[u8]) {
        let AcquisitionResult::Pending(w) = acq else {
            panic!("expected a Pending writer");
        };
        w.write_at(0, data).expect("write");
        w.commit(Some(data.len() as u64)).expect("commit");
    }

    #[kithara::test(native, timeout(kithara_platform::time::Duration::from_secs(5)))]
    fn foreign_layout_dictates_real_on_disk_path() {
        struct FlatLayout;
        impl FfiAssetLayout for FlatLayout {
            fn rel_path(&self, info: FfiResourceInfo) -> String {
                match info {
                    FfiResourceInfo::Track { ext_hint, .. } => {
                        format!("custom/audio.{}", ext_hint.as_deref().unwrap_or("bin"))
                    }
                    _ => "custom/other".to_string(),
                }
            }
        }

        let dir = tempdir().expect("tempdir");
        let layout = resolve_layout(Some(&FfiLayout::Custom {
            delegate: Arc::new(FlatLayout),
        }))
        .expect("custom resolves to Some");
        let store = AssetStoreBuilder::default()
            .root_dir(dir.path())
            .layout(layout)
            .build();
        let scope = store.scope("root");

        let u = url("https://example.com/audio.mp3");
        let key = scope.key_for(&ResourceInfo::Track {
            url: &u,
            name: None,
            ext_hint: Some("mp3"),
        });
        write_commit(
            scope.store().acquire_resource(&key, None).expect("acquire"),
            b"payload",
        );

        let expected = dir.path().join("root").join("custom").join("audio.mp3");
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
            fn rel_path(&self, _info: FfiResourceInfo) -> String {
                self.0.to_string()
            }
        }

        let dir = tempdir().expect("tempdir");
        let layout = resolve_layout(Some(&FfiLayout::Custom {
            delegate: Arc::new(HostileForeign(hostile)),
        }))
        .expect("custom resolves to Some");
        let store = AssetStoreBuilder::default()
            .root_dir(dir.path())
            .layout(layout)
            .build();
        let scope = store.scope("root");

        let u = url("https://example.com/audio.mp3");
        let key = scope.key_for(&ResourceInfo::Track {
            url: &u,
            name: None,
            ext_hint: Some("mp3"),
        });
        let err = scope
            .store()
            .acquire_resource(&key, None)
            .expect_err("hostile rel_path must be rejected");
        assert!(
            matches!(err, AssetsError::InvalidKey),
            "expected InvalidKey, got {err:?}"
        );
    }
}
