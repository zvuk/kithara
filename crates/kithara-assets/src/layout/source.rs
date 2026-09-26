#![forbid(unsafe_code)]

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use url::Url;

use crate::consts;

/// Logical asset whose resources share one cache lifecycle and root.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AssetSource {
    /// Network asset identified by a canonical URL and optional discriminator.
    Remote {
        /// Source URL. Query and fragment do not affect the default root.
        url: Url,
        /// Explicit identity when the canonical URL alone is insufficient.
        discriminator: Option<String>,
    },
    /// Local asset identified by its absolute lexical path.
    Local {
        /// Absolute path, hashed without canonicalization or filesystem I/O.
        path: PathBuf,
    },
}

pub(crate) fn remote_root(url: &Url, discriminator: Option<&str>) -> String {
    let canonical = canonical_remote(url);
    let mut hasher = Sha256::new();
    if let Some(discriminator) = discriminator {
        hasher.update(consts::REMOTE_DOMAIN);
        hash_field(&mut hasher, canonical.as_bytes());
        hash_field(&mut hasher, discriminator.as_bytes());
    } else {
        hasher.update(canonical.as_bytes());
    }
    finish_hash(hasher)
}

pub(crate) fn local_root(path: &Path) -> String {
    let mut hasher = Sha256::new();

    #[cfg(unix)]
    {
        hasher.update(consts::LOCAL_UNIX_DOMAIN);
        hash_field(&mut hasher, path.as_os_str().as_bytes());
    }

    #[cfg(windows)]
    {
        let byte_len = path.as_os_str().encode_wide().count().saturating_mul(2);
        hasher.update(consts::LOCAL_WINDOWS_DOMAIN);
        hasher.update(u64::try_from(byte_len).unwrap_or(u64::MAX).to_be_bytes());
        for unit in path.as_os_str().encode_wide() {
            hasher.update(unit.to_le_bytes());
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let path = path.to_string_lossy();
        hasher.update(consts::LOCAL_UNIX_DOMAIN);
        hash_field(&mut hasher, path.as_bytes());
    }

    finish_hash(hasher)
}

fn canonical_remote(url: &Url) -> String {
    let mut canonical = url.clone();
    canonical.set_query(None);
    canonical.set_fragment(None);
    canonical.to_string()
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

fn finish_hash(hasher: Sha256) -> String {
    let hash = hasher.finalize();
    hex::encode(&hash[..consts::HASH_BYTES])
}
