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

/// Default L2 cache directory, used when `KITHARA_FIXTURE_CACHE` is unset.
///
/// Namespaced by `KITHARA_FIXTURE_BUILD` — a fingerprint of the fixture-encoding
/// code and dependency lockfile, emitted by `build.rs` and baked into every test
/// binary of this package. Because the value is identical across `suite_stress`,
/// `suite_heavy`, … of one build, a fixture encoded by any binary is reused by
/// all of them (and by repeated runs of the same build); because it changes when
/// the encoder or its deps change, a rebuild lands in a fresh sub-dir and can
/// never serve stale encoded bytes. Launch-independent (no `current_exe`), so
/// nextest and direct/IDE runs of the same build share one cache.
fn default_cache_dir() -> PathBuf {
    let build = env!("KITHARA_FIXTURE_BUILD");
    std::env::temp_dir()
        .join("kithara-fixture-cache")
        .join(build)
}

/// Overrides the default cache directory. When unset, [`FixtureCache::from_env`]
/// falls back to a build-fingerprinted default dir (see [`default_cache_dir`]),
/// so the L2 cache is **on by default** for every run mode — not only the
/// opt-in `cache` profile.
pub(crate) const CACHE_ENV: &str = "KITHARA_FIXTURE_CACHE";

/// Cross-process on-disk content cache.
///
/// On by default: [`from_env`](Self::from_env) resolves to `$KITHARA_FIXTURE_CACHE`
/// when set, otherwise to a build-fingerprinted temp dir, so plain `cargo test`,
/// `just test`, `just rtsan`, and IDE runs all reuse encode/mux output across
/// the per-test processes in a run. Only [`from_dir(None)`](Self::from_dir)
/// makes every op a no-op (used for the disabled-path unit tests).
#[derive(Clone)]
pub(crate) struct FixtureCache {
    dir: Option<PathBuf>,
}

impl FixtureCache {
    pub(crate) fn from_env() -> Self {
        let dir = std::env::var_os(CACHE_ENV).map_or_else(default_cache_dir, PathBuf::from);
        Self::from_dir(Some(dir))
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
    fn disabled_only_via_explicit_none() {
        let store = FixtureCache::from_dir(None);
        assert!(store.get("signal", b"spec").is_none());
        store.store("signal", b"spec", b"payload");
        assert!(store.get("signal", b"spec").is_none());
    }

    #[test]
    fn default_cache_dir_is_fingerprinted_under_temp() {
        let dir = default_cache_dir();
        let base = std::env::temp_dir().join("kithara-fixture-cache");
        assert!(
            dir.starts_with(&base),
            "default dir {dir:?} must live under {base:?}",
        );
        let fingerprint = dir
            .file_name()
            .and_then(|f| f.to_str())
            .expect("fingerprint component");
        assert_eq!(fingerprint.len(), 16, "build fingerprint is u64 hex");
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn from_env_is_enabled_by_default() {
        // Read-only env probe: under the opt-in `cache` profile the setup
        // script exports KITHARA_FIXTURE_CACHE, so only assert the default
        // path when nothing overrides it.
        if std::env::var_os(CACHE_ENV).is_some() {
            return;
        }
        let cache = FixtureCache::from_env();
        assert!(
            cache.dir.is_some(),
            "L2 cache must be on by default when KITHARA_FIXTURE_CACHE is unset",
        );
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
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_or_empty_entry_is_a_miss() {
        let dir = std::env::temp_dir().join(format!("fixcache-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let store = FixtureCache::from_dir(Some(dir.clone()));
        let path = dir.join(format!("{}.bin", cache_key("signal", b"spec")));
        fs::write(&path, b"").unwrap();
        assert!(store.get("signal", b"spec").is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
