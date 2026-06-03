use std::{
    collections::HashMap,
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use kithara::audio::Waveform;
use kithara_queue::TrackSource;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use url::Url;

/// Cap on persisted blobs (~18 KB each); the oldest are pruned past it.
const MAX_DISK_ENTRIES: usize = 512;

/// Stable, source-derived cache key for a track's waveform.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct WaveKey(String);

impl WaveKey {
    /// Key from a source string, canonicalized by dropping URL query/fragment
    /// (signing tokens vary per session); non-URL paths pass through.
    pub(crate) fn from_source(src: &str) -> Self {
        let canonical = Url::parse(src).map_or_else(
            |_| src.to_string(),
            |mut url| {
                url.set_query(None);
                url.set_fragment(None);
                url.to_string()
            },
        );
        Self(canonical)
    }

    /// Filesystem-safe blob name: hex sha256 of the canonical key.
    fn file_name(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        let mut name = String::with_capacity(digest.len() * 2 + 5);
        for byte in digest {
            let _ = write!(name, "{byte:02x}");
        }
        name.push_str(".wave");
        name
    }
}

/// Cache key for a track source, or `None` for a source with no stable
/// cross-session identity.
pub(crate) fn source_key(source: &TrackSource) -> Option<WaveKey> {
    match source {
        TrackSource::Uri(url) => Some(WaveKey::from_source(url)),
        TrackSource::Config(cfg) => Some(WaveKey::from_source(&cfg.src.to_string())),
        _ => None,
    }
}

/// Two-tier waveform memoization: a session in-memory map plus an on-disk blob
/// store. Owned by the single listener task, so it needs no synchronization.
pub(crate) struct WaveformCache {
    mem: HashMap<WaveKey, Waveform>,
    dir: Option<PathBuf>,
}

impl WaveformCache {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self {
            mem: HashMap::new(),
            dir,
        }
    }

    /// Look up a cached waveform: memory first, then disk (promoting a disk
    /// hit into memory). `None` on a miss or an unreadable/stale blob.
    pub(crate) fn get(&mut self, key: &WaveKey) -> Option<Waveform> {
        if let Some(wave) = self.mem.get(key) {
            return Some(wave.clone());
        }
        let wave = self.load_disk(key)?;
        debug!("waveform cache: disk hit");
        self.mem.insert(key.clone(), wave.clone());
        Some(wave)
    }

    /// Store a freshly analysed waveform in both tiers.
    pub(crate) fn put(&mut self, key: WaveKey, wave: Waveform) {
        self.store_disk(&key, &wave);
        self.mem.insert(key, wave);
    }

    fn load_disk(&self, key: &WaveKey) -> Option<Waveform> {
        let path = self.dir.as_ref()?.join(key.file_name());
        let bytes = fs::read(&path).ok()?;
        match Waveform::from_bytes(&bytes) {
            Ok(wave) => Some(wave),
            Err(e) => {
                warn!(?e, ?path, "waveform cache: ignoring unreadable blob");
                None
            }
        }
    }

    fn store_disk(&self, key: &WaveKey, wave: &Waveform) {
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let path = dir.join(key.file_name());
        if let Err(e) = write_durable(dir, &path, &wave.to_bytes()) {
            warn!(?e, ?path, "waveform cache: disk write failed");
            return;
        }
        prune_disk(dir);
    }
}

/// Write atomically: stage in a temp file in the same directory, flush, then
/// rename into place.
fn write_durable(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_data()?;
    }
    fs::rename(&tmp, path)
}

/// Best-effort cap on the number of persisted blobs: when the directory holds
/// more than [`MAX_DISK_ENTRIES`] `.wave` files, remove the oldest by mtime.
fn prune_disk(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut blobs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "wave") {
                let mtime = entry.metadata().ok()?.modified().ok()?;
                Some((mtime, path))
            } else {
                None
            }
        })
        .collect();
    if blobs.len() <= MAX_DISK_ENTRIES {
        return;
    }
    blobs.sort_by_key(|(mtime, _)| *mtime);
    let excess = blobs.len() - MAX_DISK_ENTRIES;
    for (_, path) in blobs.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    // The test macro import shadows the `kithara` crate name; use absolute path.
    use ::kithara::audio::Waveform;
    use kithara_test_utils::kithara;

    use super::{WaveKey, WaveformCache};

    fn wave() -> Waveform {
        // version 1 + one bucket of three 0.5 band heights (0.5 = 0x3F000000).
        Waveform::from_bytes(&[1, 0, 0, 0, 0, 0, 0, 63, 0, 0, 0, 63, 0, 0, 0, 63])
            .expect("hand-built blob is valid")
    }

    #[kithara::test]
    fn url_key_strips_query_and_fragment() {
        let a = WaveKey::from_source("https://h/track.mp3?token=abc#x");
        let b = WaveKey::from_source("https://h/track.mp3?token=zzz");
        assert_eq!(a, b, "signing query/fragment must not change the key");
    }

    #[kithara::test]
    fn memory_round_trips_without_dir() {
        let mut cache = WaveformCache::new(None);
        let key = WaveKey::from_source("file:///a.mp3");
        assert!(cache.get(&key).is_none());
        cache.put(key.clone(), wave());
        assert_eq!(cache.get(&key).map(|w| w.len()), Some(1));
    }

    #[kithara::test]
    fn disk_survives_a_fresh_cache_instance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = WaveKey::from_source("file:///b.mp3");
        let mut writer = WaveformCache::new(Some(dir.path().to_path_buf()));
        writer.put(key.clone(), wave());

        // A new cache with an empty memory tier must still find the blob.
        let mut reader = WaveformCache::new(Some(dir.path().to_path_buf()));
        assert_eq!(reader.get(&key).map(|w| w.len()), Some(1));
    }
}
