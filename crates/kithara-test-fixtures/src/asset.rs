use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;

use thiserror::Error;

/// One generated asset as the manifest records it.
#[derive(Debug)]
#[non_exhaustive]
pub struct AssetEntry {
    /// MIME type declared by the generator.
    pub content_type: &'static str,
    /// Content address inside the cache revision namespace.
    pub id: &'static str,
    /// Accessor name, `{func}_{case}`.
    pub name: &'static str,
    /// Path relative to the store root, including the cache revision.
    pub path: &'static str,
    /// Redacted build-time reason an optional asset is unavailable.
    pub unavailable: Option<&'static str>,
}

/// Failure to access one generated or hydrated fixture.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AssetError {
    /// Runtime store configuration is invalid.
    #[error("fixture store configuration: {0}")]
    Store(#[source] std::io::Error),
    /// An optional producer did not materialize this asset.
    #[error("fixture `{name}` is unavailable: {reason}")]
    Unavailable {
        /// Generated accessor name.
        name: &'static str,
        /// Redacted build-time failure.
        reason: &'static str,
    },
    /// A required entry is missing or unreadable in the runtime store.
    #[error("fixture `{name}` is missing from {}: {source}", path.display())]
    Read {
        /// Generated accessor name.
        name: &'static str,
        /// Expected store path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
}

/// Handle to one generated asset.
pub struct Asset {
    entry: &'static AssetEntry,
    source: Source,
}

enum Source {
    Embedded(&'static [u8]),
    #[cfg(not(target_arch = "wasm32"))]
    OnDisk(&'static OnceLock<Vec<u8>>),
}

impl Asset {
    /// Bytes of the asset.
    ///
    /// # Panics
    ///
    /// Panics when an optional asset is unavailable or a store entry is
    /// missing, or the runtime store override is invalid.
    #[must_use]
    pub fn bytes(&self) -> &'static [u8] {
        self.try_bytes().unwrap_or_else(|error| panic!("{error}"))
    }

    /// Asset baked into the binary at compile time.
    #[must_use]
    pub const fn embedded(entry: &'static AssetEntry, bytes: &'static [u8]) -> Self {
        Self {
            entry,
            source: Source::Embedded(bytes),
        }
    }

    /// Manifest record this handle points at: name, id, path, content type.
    #[must_use]
    pub const fn entry(&self) -> &'static AssetEntry {
        self.entry
    }

    /// Asset read from the store on first use.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub const fn on_disk(entry: &'static AssetEntry, cell: &'static OnceLock<Vec<u8>>) -> Self {
        Self {
            entry,
            source: Source::OnDisk(cell),
        }
    }

    /// Store path, or `None` for an asset baked into the binary.
    ///
    /// # Panics
    /// Panics if the runtime store override is not an absolute path.
    #[must_use]
    pub fn path(&self) -> Option<PathBuf> {
        match self.source {
            Source::Embedded(_) => None,
            #[cfg(not(target_arch = "wasm32"))]
            Source::OnDisk(_) => Some(self.store_path().unwrap_or_else(|error| panic!("{error}"))),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn store_path(&self) -> Result<PathBuf, AssetError> {
        crate::store::file(std::path::Path::new(self.entry.path)).map_err(|source| {
            if source.kind() == std::io::ErrorKind::InvalidInput {
                AssetError::Store(source)
            } else {
                AssetError::Read {
                    name: self.entry.name,
                    path: PathBuf::from(self.entry.path),
                    source,
                }
            }
        })
    }

    /// Bytes of the asset, or the redacted reason an optional producer failed.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError::Unavailable`] for an optional asset that was not
    /// hydrated, [`AssetError::Read`] when a store entry is unreadable or the
    /// HTTP origin cannot supply it, or [`AssetError::Store`] when the runtime
    /// store override is invalid.
    pub fn try_bytes(&self) -> Result<&'static [u8], AssetError> {
        if let Some(reason) = self.entry.unavailable {
            return Err(AssetError::Unavailable {
                reason,
                name: self.entry.name,
            });
        }
        match self.source {
            Source::Embedded(bytes) => Ok(bytes),
            #[cfg(not(target_arch = "wasm32"))]
            Source::OnDisk(cell) => {
                if let Some(bytes) = cell.get() {
                    return Ok(bytes);
                }
                let path = self.store_path()?;
                let loaded = std::fs::read(&path).map_err(|source| AssetError::Read {
                    source,
                    name: self.entry.name,
                    path,
                })?;
                Ok(cell.get_or_init(|| loaded))
            }
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Asset, AssetEntry, AssetError, OnceLock};

    #[kithara::test(native, flash(false))]
    fn unavailable_asset_returns_the_build_failure() {
        static ENTRY: AssetEntry = AssetEntry {
            name: "remote_reference",
            id: "remote-id",
            path: "revision/remote.m3u8",
            content_type: "application/vnd.apple.mpegurl",
            unavailable: Some("HTTP 403; refresh KITHARA_TOKEN"),
        };
        static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
        let asset = Asset::on_disk(&ENTRY, &BYTES);

        assert!(matches!(
            asset.try_bytes(),
            Err(AssetError::Unavailable {
                name: "remote_reference",
                reason: "HTTP 403; refresh KITHARA_TOKEN",
            })
        ));
    }
}
