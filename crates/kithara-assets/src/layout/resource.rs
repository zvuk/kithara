#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use kithara_platform::sync::Arc;
use url::Url;

use crate::error::{AssetsError, AssetsResult};

/// Semantic resource whose cache path is selected by an [`AssetLayout`](super::AssetLayout).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssetResource {
    /// Source media bytes with a resolved file extension.
    Source {
        /// Extension without the leading dot.
        extension: String,
    },
    /// A resource addressed by its full server URL.
    /// The layout owns safe query identity; the default ignores fragments.
    Url(Url),
    /// A logical or derived artifact with two semantic components.
    Named {
        /// Resource namespace, such as `analysis`.
        namespace: String,
        /// Resource name within the namespace.
        name: String,
    },
}

/// Self-identifying key for a single resource within an asset store.
///
/// Relative keys can only be minted by [`crate::AssetScope`]. Absolute keys
/// are the explicit local-file read-in-place capability.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ResourceKey(ResourceKeyKind);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ResourceKeyKind {
    Relative {
        asset_root: Arc<str>,
        rel_path: Arc<str>,
    },
    Absolute(PathBuf),
}

impl ResourceKey {
    /// Create an absolute resource key for a local file.
    ///
    /// # Errors
    /// Returns [`AssetsError::InvalidKey`] when `path` is relative.
    pub fn absolute<P: Into<PathBuf>>(path: P) -> AssetsResult<Self> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(AssetsError::InvalidKey);
        }
        Ok(Self(ResourceKeyKind::Absolute(path)))
    }

    /// Returns the absolute path for a local-file key.
    #[must_use]
    pub fn as_absolute_path(&self) -> Option<&Path> {
        match &self.0 {
            ResourceKeyKind::Absolute(path) => Some(path),
            ResourceKeyKind::Relative { .. } => None,
        }
    }

    /// Returns the asset namespace, or `None` for an absolute key.
    #[must_use]
    pub fn asset_root(&self) -> Option<&str> {
        match &self.0 {
            ResourceKeyKind::Relative { asset_root, .. } => Some(asset_root),
            ResourceKeyKind::Absolute(_) => None,
        }
    }

    /// Returns `true` for an absolute local-file key.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        self.as_absolute_path().is_some()
    }

    /// Returns the layout-owned relative path, or `None` for an absolute key.
    #[must_use]
    pub fn rel_path(&self) -> Option<&str> {
        match &self.0 {
            ResourceKeyKind::Relative { rel_path, .. } => Some(rel_path),
            ResourceKeyKind::Absolute(_) => None,
        }
    }

    pub(crate) fn relative(asset_root: impl Into<Arc<str>>, rel_path: impl Into<Arc<str>>) -> Self {
        Self(ResourceKeyKind::Relative {
            asset_root: asset_root.into(),
            rel_path: rel_path.into(),
        })
    }

    pub(crate) fn kind(&self) -> &ResourceKeyKind {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, env};

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn absolute_key_preserves_path() {
        let path = env::temp_dir().join("song.mp3");
        let key = ResourceKey::absolute(&path).expect("absolute test path");

        assert!(key.is_absolute());
        assert_eq!(key.as_absolute_path(), Some(path.as_path()));
        assert_eq!(key.asset_root(), None);
    }

    #[kithara::test]
    fn absolute_key_rejects_relative_path() {
        let result = ResourceKey::absolute("relative/track.mp3");

        assert!(matches!(result, Err(AssetsError::InvalidKey)));
    }

    #[kithara::test]
    fn relative_identity_includes_asset_root() {
        let first = ResourceKey::relative("root_a", "seg.m4s");
        let second = ResourceKey::relative("root_b", "seg.m4s");
        let same = ResourceKey::relative("root_a", "seg.m4s");
        let mut keys = HashSet::new();

        keys.insert(first.clone());
        keys.insert(second.clone());

        assert_ne!(first, second);
        assert_eq!(first, same);
        assert_eq!(keys.len(), 2);
        assert_eq!(first.asset_root(), Some("root_a"));
        assert_eq!(first.rel_path(), Some("seg.m4s"));
        assert!(!first.is_absolute());
    }
}
