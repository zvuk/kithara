use std::{fmt, sync::Arc};

/// Foreign on-disk layout callback. Mirrors [`kithara::assets::AssetLayout`]:
/// maps a fetched resource to a relative path inside the asset directory.
///
/// `rel_path` is a pure function - the same info must always yield the same
/// path. It runs on arbitrary background threads, must not block. It must not throw; a hostile or empty return is
/// rejected store-side with a typed error (never silently rewritten).
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait FfiAssetLayout: Send + Sync {
    fn rel_path(&self, info: FfiResourceInfo) -> String;
}

/// Owned mirror of [`kithara::assets::Rendition`].
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiRendition {
    pub bandwidth: Option<u64>,
    pub name: Option<String>,
    pub uri_stem: Option<String>,
}

/// Owned mirror of [`kithara::assets::RenditionDesc`]. `siblings` is the full
/// master rendition set in master order; `idx` selects this rendition.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiRenditionDesc {
    pub idx: u32,
    pub siblings: Vec<FfiRendition>,
}

/// Owned mirror of [`kithara::assets::ResourceInfo`].
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiResourceInfo {
    Manifest {
        url: String,
        rendition: Option<FfiRenditionDesc>,
    },
    InitSegment {
        url: String,
        rendition: FfiRenditionDesc,
    },
    MediaSegment {
        url: String,
        rendition: FfiRenditionDesc,
        sequence: u64,
    },
    Key {
        url: String,
    },
    Track {
        url: String,
        name: Option<String>,
        ext_hint: Option<String>,
    },
}

/// On-disk layout selection for the shared asset store. `None` on
/// [`crate::config::StoreOptions`] keeps [`Default`](Self::Default).
#[derive(Clone)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiLayout {
    Default,
    Pretty,
    Custom { delegate: Arc<dyn FfiAssetLayout> },
}

impl fmt::Debug for FfiLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("Default"),
            Self::Pretty => f.write_str("Pretty"),
            Self::Custom { .. } => f.debug_struct("Custom").finish_non_exhaustive(),
        }
    }
}
