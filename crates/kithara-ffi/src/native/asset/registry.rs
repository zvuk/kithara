use std::fmt;

use kithara_assets::{AssetLayout, AssetLayoutRegistry};
use kithara_platform::sync::{Arc, Mutex};

use crate::{layout::FfiAssetLayout, native::layout::ForeignLayout};

/// Playback protocol whose default asset layout can be replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiAssetLayoutTarget {
    /// Direct-file sources and their derived resources.
    File,
    /// HLS playlists, media resources, keys, and derived resources.
    Hls,
}

/// Rust-owned registry of protocol-specific asset layouts.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct FfiAssetLayoutRegistry {
    inner: Mutex<AssetLayoutRegistry>,
}

impl FfiAssetLayoutRegistry {
    pub(crate) fn snapshot(&self) -> AssetLayoutRegistry {
        self.inner.lock().clone()
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl FfiAssetLayoutRegistry {
    /// Create an empty registry that uses Kithara's default layout.
    #[must_use]
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register or replace the layout for `target`.
    pub fn register(&self, target: FfiAssetLayoutTarget, layout: Arc<dyn FfiAssetLayout>) {
        let layout = Arc::new(ForeignLayout::new(layout)) as Arc<dyn AssetLayout>;
        let replaced = {
            let mut layouts = self.inner.lock();
            match target {
                FfiAssetLayoutTarget::File => layouts.register::<kithara::file::File>(layout),
                FfiAssetLayoutTarget::Hls => layouts.register::<kithara::hls::Hls>(layout),
            }
        };
        drop(replaced);
    }
}

impl Default for FfiAssetLayoutRegistry {
    fn default() -> Self {
        Self {
            inner: Mutex::new(AssetLayoutRegistry::default()),
        }
    }
}

impl fmt::Debug for FfiAssetLayoutRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FfiAssetLayoutRegistry")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Weak,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;
    use crate::layout::{FfiAssetResource, FfiAssetSource};

    struct FixedLayout;

    impl FfiAssetLayout for FixedLayout {
        fn root(&self, _source: FfiAssetSource) -> String {
            "root".to_string()
        }

        fn path(&self, _resource: FfiAssetResource) -> String {
            "resource".to_string()
        }
    }

    struct ReentrantLayout {
        registry: Weak<FfiAssetLayoutRegistry>,
        dropped: Arc<AtomicBool>,
    }

    impl FfiAssetLayout for ReentrantLayout {
        fn root(&self, _source: FfiAssetSource) -> String {
            "root".to_string()
        }

        fn path(&self, _resource: FfiAssetResource) -> String {
            "resource".to_string()
        }
    }

    impl Drop for ReentrantLayout {
        fn drop(&mut self) {
            if let Some(registry) = self.registry.upgrade() {
                registry.register(FfiAssetLayoutTarget::Hls, Arc::new(FixedLayout));
            }
            self.dropped.store(true, Ordering::Release);
        }
    }

    #[kithara::test]
    fn replacement_drops_foreign_layout_after_unlock() {
        let registry = FfiAssetLayoutRegistry::new();
        let dropped = Arc::new(AtomicBool::new(false));
        registry.register(
            FfiAssetLayoutTarget::File,
            Arc::new(ReentrantLayout {
                registry: Arc::downgrade(&registry),
                dropped: Arc::clone(&dropped),
            }),
        );

        registry.register(FfiAssetLayoutTarget::File, Arc::new(FixedLayout));

        assert!(dropped.load(Ordering::Acquire));
    }
}
