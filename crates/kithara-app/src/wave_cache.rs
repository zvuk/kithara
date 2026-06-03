use std::{
    collections::HashMap,
    fmt::{self, Write as _},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use kithara_queue::TrackSource;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use url::Url;

use crate::waveform::TrackAnalysis;

/// Cap on persisted blobs (~18 KB each); the oldest are pruned past it.
const MAX_DISK_ENTRIES: usize = 512;
const ANALYSIS_BYTES_VERSION: u32 = 0x4b41_0001;

/// Stable, source-derived cache key for a track's analysis.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct AnalysisKey(String);

impl AnalysisKey {
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
        name.push_str(".analysis");
        name
    }
}

/// Cache key for a track source, or `None` for a source with no stable
/// cross-session identity.
pub(crate) fn source_key(source: &TrackSource) -> Option<AnalysisKey> {
    match source {
        TrackSource::Uri(url) => Some(AnalysisKey::from_source(url)),
        TrackSource::Config(cfg) => Some(AnalysisKey::from_source(&cfg.src.to_string())),
        _ => None,
    }
}

/// Two-tier track-analysis memoization: a session in-memory map plus an on-disk blob
/// store. Owned by the single listener task, so it needs no synchronization.
pub(crate) struct TrackAnalysisCache {
    mem: HashMap<AnalysisKey, TrackAnalysis>,
    dir: Option<PathBuf>,
}

impl TrackAnalysisCache {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self {
            mem: HashMap::new(),
            dir,
        }
    }

    /// Look up a cached waveform: memory first, then disk (promoting a disk
    /// hit into memory). `None` on a miss or an unreadable/stale blob.
    pub(crate) fn get(&mut self, key: &AnalysisKey) -> Option<TrackAnalysis> {
        if let Some(analysis) = self.mem.get(key) {
            return Some(analysis.clone());
        }
        let analysis = self.load_disk(key)?;
        debug!("track analysis cache: disk hit");
        self.mem.insert(key.clone(), analysis.clone());
        Some(analysis)
    }

    /// Store freshly derived track analysis in both tiers.
    pub(crate) fn put(&mut self, key: AnalysisKey, analysis: TrackAnalysis) {
        self.store_disk(&key, &analysis);
        self.mem.insert(key, analysis);
    }

    fn load_disk(&self, key: &AnalysisKey) -> Option<TrackAnalysis> {
        let path = self.dir.as_ref()?.join(key.file_name());
        let bytes = fs::read(&path).ok()?;
        match analysis_from_bytes(&bytes) {
            Ok(analysis) => Some(analysis),
            Err(e) => {
                warn!(?e, ?path, "track analysis cache: ignoring unreadable blob");
                None
            }
        }
    }

    fn store_disk(&self, key: &AnalysisKey, analysis: &TrackAnalysis) {
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let path = dir.join(key.file_name());
        let bytes = match analysis_to_bytes(analysis) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(?e, ?path, "track analysis cache: encode failed");
                return;
            }
        };
        if let Err(e) = write_durable(dir, &path, &bytes) {
            warn!(?e, ?path, "track analysis cache: disk write failed");
            return;
        }
        prune_disk(dir);
    }
}

#[derive(Debug)]
enum AnalysisBytesError {
    Version { found: u32, expected: u32 },
    TooLarge,
    Corrupt,
}

impl fmt::Display for AnalysisBytesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version { found, expected } => {
                write!(
                    f,
                    "track analysis blob version {found} != expected {expected}"
                )
            }
            Self::TooLarge => f.write_str("track analysis blob is too large"),
            Self::Corrupt => f.write_str("track analysis blob is corrupt"),
        }
    }
}

fn analysis_to_bytes(analysis: &TrackAnalysis) -> Result<Vec<u8>, AnalysisBytesError> {
    let waveform = analysis.waveform.to_bytes();
    let waveform_len = u64::try_from(waveform.len()).map_err(|_| AnalysisBytesError::TooLarge)?;
    let mut out = Vec::with_capacity(4 + 8 + waveform.len());
    out.extend_from_slice(&ANALYSIS_BYTES_VERSION.to_le_bytes());
    out.extend_from_slice(&waveform_len.to_le_bytes());
    out.extend_from_slice(&waveform);
    Ok(out)
}

fn analysis_from_bytes(bytes: &[u8]) -> Result<TrackAnalysis, AnalysisBytesError> {
    let mut cursor = 0usize;
    let version = read_u32(bytes, &mut cursor)?;
    if version != ANALYSIS_BYTES_VERSION {
        return Err(AnalysisBytesError::Version {
            found: version,
            expected: ANALYSIS_BYTES_VERSION,
        });
    }
    let waveform_len =
        usize::try_from(read_u64(bytes, &mut cursor)?).map_err(|_| AnalysisBytesError::Corrupt)?;
    let end = cursor
        .checked_add(waveform_len)
        .ok_or(AnalysisBytesError::Corrupt)?;
    let waveform_bytes = bytes.get(cursor..end).ok_or(AnalysisBytesError::Corrupt)?;
    if end != bytes.len() {
        return Err(AnalysisBytesError::Corrupt);
    }
    let waveform = kithara::audio::Waveform::from_bytes(waveform_bytes)
        .map_err(|_| AnalysisBytesError::Corrupt)?;
    Ok(TrackAnalysis { waveform })
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, AnalysisBytesError> {
    let chunk = read_array::<4>(bytes, cursor)?;
    Ok(u32::from_le_bytes(chunk))
}

fn read_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64, AnalysisBytesError> {
    let chunk = read_array::<8>(bytes, cursor)?;
    Ok(u64::from_le_bytes(chunk))
}

fn read_array<const N: usize>(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<[u8; N], AnalysisBytesError> {
    let end = cursor.checked_add(N).ok_or(AnalysisBytesError::Corrupt)?;
    let chunk = bytes.get(*cursor..end).ok_or(AnalysisBytesError::Corrupt)?;
    let mut out = [0u8; N];
    out.copy_from_slice(chunk);
    *cursor = end;
    Ok(out)
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
/// more than [`MAX_DISK_ENTRIES`] `.analysis` files, remove the oldest by mtime.
fn prune_disk(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut blobs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "analysis") {
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

    use crate::waveform::TrackAnalysis;

    use super::{AnalysisKey, TrackAnalysisCache};

    fn wave() -> Waveform {
        // version 1 + one bucket of three 0.5 band heights (0.5 = 0x3F000000).
        Waveform::from_bytes(&[1, 0, 0, 0, 0, 0, 0, 63, 0, 0, 0, 63, 0, 0, 0, 63])
            .expect("hand-built blob is valid")
    }

    #[kithara::test]
    fn url_key_strips_query_and_fragment() {
        let a = AnalysisKey::from_source("https://h/track.mp3?token=abc#x");
        let b = AnalysisKey::from_source("https://h/track.mp3?token=zzz");
        assert_eq!(a, b, "signing query/fragment must not change the key");
    }

    fn analysis() -> TrackAnalysis {
        TrackAnalysis {
            waveform: wave(),
        }
    }

    #[kithara::test]
    fn memory_round_trips_without_dir() {
        let mut cache = TrackAnalysisCache::new(None);
        let key = AnalysisKey::from_source("file:///a.mp3");
        assert!(cache.get(&key).is_none());
        cache.put(key.clone(), analysis());
        let cached = cache.get(&key).expect("analysis must be cached");
        assert_eq!(cached.waveform.len(), 1);
    }

    #[kithara::test]
    fn disk_survives_a_fresh_cache_instance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = AnalysisKey::from_source("file:///b.mp3");
        let mut writer = TrackAnalysisCache::new(Some(dir.path().to_path_buf()));
        writer.put(key.clone(), analysis());

        // A new cache with an empty memory tier must still find the blob.
        let mut reader = TrackAnalysisCache::new(Some(dir.path().to_path_buf()));
        let cached = reader.get(&key).expect("disk analysis must load");
        assert_eq!(cached.waveform.len(), 1);
    }
}
