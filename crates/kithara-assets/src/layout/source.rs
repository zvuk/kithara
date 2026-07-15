#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use url::Url;

struct Consts;

impl Consts {
    const HASH_BYTES: usize = 16;
    const LOCAL_UNIX_DOMAIN: &[u8] = b"kithara.asset-root.local.unix.v1\0";
    #[cfg(windows)]
    const LOCAL_WINDOWS_DOMAIN: &[u8] = b"kithara.asset-root.local.windows.v1\0";
    const REMOTE_DOMAIN: &[u8] = b"kithara.asset-root.remote.v1\0";
}

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
        hasher.update(Consts::REMOTE_DOMAIN);
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
        use std::os::unix::ffi::OsStrExt;

        hasher.update(Consts::LOCAL_UNIX_DOMAIN);
        hash_field(&mut hasher, path.as_os_str().as_bytes());
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let byte_len = path.as_os_str().encode_wide().count().saturating_mul(2);
        hasher.update(Consts::LOCAL_WINDOWS_DOMAIN);
        hasher.update(u64::try_from(byte_len).unwrap_or(u64::MAX).to_be_bytes());
        for unit in path.as_os_str().encode_wide() {
            hasher.update(unit.to_le_bytes());
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let path = path.to_string_lossy();
        hasher.update(Consts::LOCAL_UNIX_DOMAIN);
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
    hex::encode(&hash[..Consts::HASH_BYTES])
}
