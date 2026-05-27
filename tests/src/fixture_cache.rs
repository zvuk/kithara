use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

/// Content-addressed cache key: `sha2-256(domain || 0x00 || spec)` as hex.
fn cache_key(domain: &str, spec: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0u8]);
    hasher.update(spec);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Env var the nextest setup script exports; absent → L2 disabled.
pub(crate) const CACHE_ENV: &str = "KITHARA_FIXTURE_CACHE";

/// Cross-process on-disk content cache. Disabled (every op a no-op / miss) when
/// no directory is configured, so plain `cargo test` / `just rtsan` / IDE runs
/// fall back to the in-memory L1 cache.
#[derive(Clone)]
pub(crate) struct FixtureCache {
    dir: Option<PathBuf>,
}

impl FixtureCache {
    pub(crate) fn from_env() -> Self {
        Self::from_dir(std::env::var_os(CACHE_ENV).map(PathBuf::from))
    }

    pub(crate) fn from_dir(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    fn entry_path(dir: &Path, domain: &str, spec: &[u8]) -> PathBuf {
        dir.join(format!("{}.bin", cache_key(domain, spec)))
    }

    pub(crate) fn get(&self, domain: &str, spec: &[u8]) -> Option<Vec<u8>> {
        let dir = self.dir.as_ref()?;
        let bytes = fs::read(Self::entry_path(dir, domain, spec)).ok()?;
        if bytes.is_empty() { None } else { Some(bytes) }
    }

    pub(crate) fn store(&self, domain: &str, spec: &[u8], payload: &[u8]) {
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        let final_path = Self::entry_path(dir, domain, spec);
        let tmp_path = dir.join(format!(
            "{}.tmp.{}",
            cache_key(domain, spec),
            std::process::id()
        ));
        let write_ok = (|| -> std::io::Result<()> {
            let mut f = fs::File::create(&tmp_path)?;
            f.write_all(payload)?;
            f.sync_all()
        })();
        if write_ok.is_ok() {
            let _ = fs::rename(&tmp_path, &final_path);
        }
        let _ = fs::remove_file(&tmp_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_and_domain_separated() {
        let a = cache_key("signal", b"sine|44100|2|440|1.0|mp3");
        let a2 = cache_key("signal", b"sine|44100|2|440|1.0|mp3");
        let b = cache_key("signal", b"sine|44100|2|441|1.0|mp3");
        let other_domain = cache_key("hls-variant", b"sine|44100|2|440|1.0|mp3");
        assert_eq!(a, a2);
        assert_ne!(a, b);
        assert_ne!(a, other_domain);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn disabled_when_env_unset() {
        let store = FixtureCache::from_dir(None);
        assert!(store.get("signal", b"spec").is_none());
        store.store("signal", b"spec", b"payload");
        assert!(store.get("signal", b"spec").is_none());
    }

    #[test]
    fn roundtrip_hit_after_store() {
        let dir = std::env::temp_dir().join(format!("fixcache-test-{}", uuid::Uuid::new_v4()));
        let store = FixtureCache::from_dir(Some(dir.clone()));
        assert!(store.get("signal", b"spec").is_none());
        store.store("signal", b"spec", b"hello-bytes");
        assert_eq!(
            store.get("signal", b"spec").as_deref(),
            Some(b"hello-bytes".as_slice())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_or_empty_entry_is_a_miss() {
        let dir = std::env::temp_dir().join(format!("fixcache-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = FixtureCache::from_dir(Some(dir.clone()));
        let path = dir.join(format!("{}.bin", cache_key("signal", b"spec")));
        std::fs::write(&path, b"").unwrap();
        assert!(store.get("signal", b"spec").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
