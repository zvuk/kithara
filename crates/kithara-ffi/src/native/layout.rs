use std::fmt;

use kithara_assets::{AssetLayout, AssetResource, AssetSource};
use kithara_platform::sync::Arc;

use crate::layout::{FfiAssetLayout, FfiAssetResource, FfiAssetSource};

/// Adapts a foreign layout to the core layout contract.
pub(crate) struct ForeignLayout(Arc<dyn FfiAssetLayout>);

impl ForeignLayout {
    pub(crate) fn new(layout: Arc<dyn FfiAssetLayout>) -> Self {
        Self(layout)
    }
}

impl fmt::Debug for ForeignLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForeignLayout").finish_non_exhaustive()
    }
}

impl AssetLayout for ForeignLayout {
    fn root(&self, source: &AssetSource) -> String {
        let source = match source {
            AssetSource::Remote { url, discriminator } => FfiAssetSource::Remote {
                url: url.as_str().to_string(),
                discriminator: discriminator.clone(),
            },
            AssetSource::Local { path } => {
                let Some(path) = path.to_str() else {
                    return String::new();
                };
                FfiAssetSource::Local {
                    path: path.to_string(),
                }
            }
            _ => return String::new(),
        };
        self.0.root(source)
    }

    fn path(&self, resource: &AssetResource) -> String {
        let resource = match resource {
            AssetResource::Source { extension } => FfiAssetResource::Source {
                extension: extension.clone(),
            },
            AssetResource::Url(url) => FfiAssetResource::Url {
                url: url.as_str().to_string(),
            },
            AssetResource::Named { namespace, name } => FfiAssetResource::Named {
                namespace: namespace.clone(),
                name: name.clone(),
            },
            _ => return String::new(),
        };
        self.0.path(resource)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

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

    fn layout<L: FfiAssetLayout + 'static>(layout: L) -> Arc<dyn AssetLayout> {
        Arc::new(ForeignLayout::new(Arc::new(layout)))
    }

    struct EchoLayout;

    impl FfiAssetLayout for EchoLayout {
        fn root(&self, source: FfiAssetSource) -> String {
            match source {
                FfiAssetSource::Remote { url, discriminator } => {
                    format!(
                        "remote:{url}:{}",
                        discriminator.as_deref().unwrap_or("none")
                    )
                }
                FfiAssetSource::Local { path } => format!("local:{path}"),
            }
        }

        fn path(&self, resource: FfiAssetResource) -> String {
            match resource {
                FfiAssetResource::Source { extension } => format!("source:{extension}"),
                FfiAssetResource::Url { url } => format!("url:{url}"),
                FfiAssetResource::Named { namespace, name } => {
                    format!("named:{namespace}:{name}")
                }
            }
        }
    }

    #[kithara::test]
    fn foreign_layout_receives_every_source_variant() {
        let layout = ForeignLayout::new(Arc::new(EchoLayout));
        let remote = AssetSource::Remote {
            url: url("https://example.com/track.mp3?token=secret"),
            discriminator: Some("quality".to_string()),
        };
        let local = AssetSource::Local {
            path: PathBuf::from("/music/track.flac"),
        };

        assert_eq!(
            layout.root(&remote),
            "remote:https://example.com/track.mp3?token=secret:quality"
        );
        assert_eq!(layout.root(&local), "local:/music/track.flac");
    }

    #[kithara::test]
    fn foreign_layout_receives_every_resource_variant() {
        let layout = ForeignLayout::new(Arc::new(EchoLayout));

        assert_eq!(
            layout.path(&AssetResource::Source {
                extension: "mp3".to_string(),
            }),
            "source:mp3"
        );
        assert_eq!(
            layout.path(&AssetResource::Url(url(
                "https://example.com/hls/seg.m4s?part=2"
            ))),
            "url:https://example.com/hls/seg.m4s?part=2"
        );
        assert_eq!(
            layout.path(&AssetResource::Named {
                namespace: "analysis".to_string(),
                name: "waveform.json".to_string(),
            }),
            "named:analysis:waveform.json"
        );
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

    #[kithara::test(native, timeout(kithara_platform::time::Duration::from_secs(5)))]
    fn foreign_layout_dictates_real_on_disk_root_and_path() {
        struct FixedLayout;

        impl FfiAssetLayout for FixedLayout {
            fn root(&self, _source: FfiAssetSource) -> String {
                "foreign-root".to_string()
            }

            fn path(&self, resource: FfiAssetResource) -> String {
                let FfiAssetResource::Url { url } = resource else {
                    panic!("expected URL resource");
                };
                let leaf = url.rsplit('/').next().unwrap_or("res");
                format!("custom/{leaf}")
            }
        }

        let dir = tempdir().expect("tempdir");
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .layouts(AssetLayoutRegistry::new(layout(FixedLayout)))
            .build();

        let resource_url = url("https://example.com/audio.mp3");
        let source = AssetSource::Remote {
            url: resource_url.clone(),
            discriminator: Some("root".to_string()),
        };
        let scope = store.scope::<FixedLayout>(&source).expect("valid scope");
        let key = scope
            .key(&AssetResource::Url(resource_url))
            .expect("valid key");
        write_commit(
            scope.store().acquire_resource(&key, None).expect("acquire"),
            b"payload",
        );

        assert!(dir.path().join("foreign-root/custom/audio.mp3").exists());
    }

    #[kithara::test(native, timeout(kithara_platform::time::Duration::from_secs(5)))]
    #[case("../escape")]
    #[case("/absolute/path")]
    #[case("")]
    #[case("dir/../escape")]
    fn hostile_foreign_path_is_rejected(#[case] hostile: &'static str) {
        struct HostileForeign(&'static str);

        impl FfiAssetLayout for HostileForeign {
            fn root(&self, _source: FfiAssetSource) -> String {
                "foreign-root".to_string()
            }

            fn path(&self, _resource: FfiAssetResource) -> String {
                self.0.to_string()
            }
        }

        let layout = layout(HostileForeign(hostile));
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .layouts(AssetLayoutRegistry::new(layout))
            .build();
        let resource_url = url("https://example.com/audio.mp3");
        let source = AssetSource::Remote {
            url: resource_url.clone(),
            discriminator: None,
        };
        let scope = store.scope::<HostileForeign>(&source).expect("valid scope");
        let error = scope
            .key(&AssetResource::Url(resource_url))
            .expect_err("hostile path must be rejected");

        assert!(matches!(error, AssetsError::InvalidKey));
    }

    #[kithara::test]
    #[case("../escape")]
    #[case("/absolute")]
    #[case("")]
    #[case("nested/root")]
    fn hostile_foreign_root_is_rejected(#[case] hostile: &'static str) {
        struct HostileRoot(&'static str);

        impl FfiAssetLayout for HostileRoot {
            fn root(&self, _source: FfiAssetSource) -> String {
                self.0.to_string()
            }

            fn path(&self, _resource: FfiAssetResource) -> String {
                "resource.bin".to_string()
            }
        }

        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .layouts(AssetLayoutRegistry::new(layout(HostileRoot(hostile))))
            .build();
        let error = store
            .scope::<HostileRoot>(&AssetSource::Remote {
                url: url("https://example.com/audio.mp3"),
                discriminator: None,
            })
            .expect_err("hostile root must be rejected");

        assert!(matches!(error, AssetsError::InvalidKey));
    }

    #[cfg(unix)]
    #[kithara::test]
    fn non_utf8_local_source_is_rejected_without_lossy_conversion() {
        struct RejectNonUtf8;

        impl FfiAssetLayout for RejectNonUtf8 {
            fn root(&self, _source: FfiAssetSource) -> String {
                panic!("non-UTF-8 path must not reach the foreign delegate");
            }

            fn path(&self, _resource: FfiAssetResource) -> String {
                "resource.bin".to_string()
            }
        }

        let path = PathBuf::from(OsString::from_vec(vec![b'/', 0xff]));
        let source = AssetSource::Local { path };
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .layouts(AssetLayoutRegistry::new(layout(RejectNonUtf8)))
            .build();
        let error = store
            .scope::<RejectNonUtf8>(&source)
            .expect_err("non-UTF-8 source must be rejected");

        assert!(matches!(error, AssetsError::InvalidKey));
    }
}
