use std::{
    fmt::Write as _,
    fs,
    fs::OpenOptions,
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
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Appends `KITHARA_FIXTURE_BUILD` to the cache root, whether that root came
/// from the environment or from the default.
///
/// The fingerprint covers the fixture-encoding code and the encoder versions the
/// lockfile resolved, and entry keys are content-addressed over the spec alone —
/// so the sub-directory is the only thing standing between a new encoder and the
/// bytes an old one produced. A root that skipped it, as an explicitly configured
/// one used to, is a cache that never invalidates: shared across runners and
/// branches, it serves one build's fixtures to another. Identical across
/// `suite_stress`, `suite_heavy`, … of one build, and launch-independent (no
/// `current_exe`), so nextest and IDE runs of that build share one directory.
fn resolve_cache_dir(root: Option<PathBuf>) -> PathBuf {
    let root = root.unwrap_or_else(|| std::env::temp_dir().join("kithara-fixture-cache"));
    root.join(env!("KITHARA_FIXTURE_BUILD"))
}

/// Overrides the default cache root; the build fingerprint is always appended.
pub(crate) const CACHE_ENV: &str = "KITHARA_FIXTURE_CACHE";

/// Cross-process on-disk content cache.
///
/// [`from_env`](Self::from_env) appends the build fingerprint to the configured
/// or default root. Only [`from_dir(None)`](Self::from_dir) disables the cache.
#[derive(Clone)]
pub(crate) struct FixtureCache {
    dir: Option<PathBuf>,
}

#[must_use = "the cache entry lock must be held while producing the entry"]
pub(crate) struct FixtureCacheLock {
    _file: Option<fs::File>,
}

impl FixtureCache {
    pub(crate) fn from_env() -> Self {
        let root = std::env::var_os(CACHE_ENV).map(PathBuf::from);
        let dir = resolve_cache_dir(root);
        Self::from_dir(Some(dir))
    }

    pub(crate) const fn from_dir(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    fn entry_path(dir: &Path, domain: &str, spec: &[u8]) -> PathBuf {
        dir.join(format!("{}.bin", cache_key(domain, spec)))
    }

    fn lock_path(dir: &Path, domain: &str, spec: &[u8]) -> PathBuf {
        dir.join(format!("{}.lock", cache_key(domain, spec)))
    }

    pub(crate) fn get(&self, domain: &str, spec: &[u8]) -> Option<Vec<u8>> {
        let dir = self.dir.as_ref()?;
        let bytes = fs::read(Self::entry_path(dir, domain, spec)).ok()?;
        if bytes.is_empty() { None } else { Some(bytes) }
    }

    /// Serialize one cache miss across nextest processes. Callers must re-check
    /// the entry after acquiring the lock before producing it.
    pub(crate) fn lock_entry(&self, domain: &str, spec: &[u8]) -> FixtureCacheLock {
        let file = self.dir.as_ref().and_then(|dir| {
            fs::create_dir_all(dir).ok()?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(Self::lock_path(dir, domain, spec))
                .ok()?;
            file.lock().ok()?;
            Some(file)
        });
        FixtureCacheLock { _file: file }
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
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
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

    #[kithara::test(native, flash(false))]
    fn disabled_only_via_explicit_none() {
        let store = FixtureCache::from_dir(None);
        assert!(store.get("signal", b"spec").is_none());
        store.store("signal", b"spec", b"payload");
        assert!(store.get("signal", b"spec").is_none());
    }

    #[kithara::test(native, flash(false))]
    fn default_cache_dir_is_fingerprinted_under_temp() {
        let dir = resolve_cache_dir(None);
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

    #[kithara::test(native, flash(false))]
    fn explicit_cache_root_is_preserved() {
        let root = PathBuf::from("explicit-cache-root");
        let dir = resolve_cache_dir(Some(root.clone()));

        assert!(
            dir.starts_with(&root),
            "resolved dir {dir:?} must live under {root:?}",
        );
    }

    #[kithara::test(native, flash(false))]
    fn explicit_cache_root_ends_with_build_fingerprint() {
        let dir = resolve_cache_dir(Some(PathBuf::from("explicit-cache-root")));

        assert!(
            dir.ends_with(env!("KITHARA_FIXTURE_BUILD")),
            "resolved dir {dir:?} must end with the build fingerprint",
        );
    }

    #[kithara::test(native, flash(false))]
    fn from_env_is_enabled_by_default() {
        // Read-only env probe: under the opt-in `cold` profile the setup
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

    #[kithara::test(native, flash(false))]
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

    #[kithara::test(native, flash(false))]
    fn entry_lock_is_exclusive_and_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("fixcache-lock-{}", uuid::Uuid::new_v4()));
        let store = FixtureCache::from_dir(Some(dir.clone()));
        let held = store.lock_entry("signal", b"spec");
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(FixtureCache::lock_path(&dir, "signal", b"spec"))
            .expect("open cache lock contender");

        assert!(
            matches!(contender.try_lock(), Err(fs::TryLockError::WouldBlock)),
            "entry lock must exclude a second producer"
        );
        drop(held);
        contender
            .try_lock()
            .expect("entry lock must release with its file handle");
        drop(contender);
        let _ = fs::remove_dir_all(&dir);
    }

    #[kithara::test(native, flash(false))]
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
