use std::{
    collections::{HashMap, VecDeque},
    fmt,
    io::{Error as IoError, ErrorKind},
    sync::Arc,
};

use kithara::{
    assets::{
        AcquisitionResult, AssetResourceState, AssetStore, AssetsError, ReadSide, ResourceKey,
        WriteSide, asset_root_for_url,
    },
    audio::{BeatGrid, Waveform},
    prelude::{ResourceConfig, ResourceSrc},
};
use kithara_queue::TrackSource;
use tracing::{debug, warn};
use url::Url;

use crate::waveform::TrackAnalysis;

/// Tunables for the analysis cache, grouped to keep the module surface small.
struct Consts;

impl Consts {
    const ANALYSIS_BYTES_VERSION: u32 = 0x4b41_0004;
    /// Analysis artifact resource under the track's asset scope. One blob per
    /// track: the active config fingerprint is checked inside it.
    const ANALYSIS_REL_PATH: &str = "analysis/track.analysis";
    /// Cap on the in-memory tier; past it the oldest entries fall back to disk.
    const MAX_MEM_ENTRIES: usize = 64;
}

/// Cache key for a track's analysis.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct AnalysisKey {
    asset_root: String,
}

impl AnalysisKey {
    pub(crate) fn new(asset_root: impl Into<String>) -> Self {
        Self {
            asset_root: asset_root.into(),
        }
    }
}

/// Cache key for a track source, or `None` for a source with no stable
/// cross-session identity.
pub(crate) fn source_key(source: &TrackSource) -> Option<AnalysisKey> {
    match source {
        TrackSource::Uri(src) => key_for_src(&ResourceConfig::parse_src(src).ok()?, None),
        TrackSource::Config(cfg) => key_for_src(&cfg.src, cfg.name.as_deref()),
        _ => None,
    }
}

/// `asset_root` for a source, mirroring `kithara-file`'s remote derivation
/// (`name` first, else the URL query).
fn key_for_src(src: &ResourceSrc, name: Option<&str>) -> Option<AnalysisKey> {
    match src {
        ResourceSrc::Url(url) => {
            let name_or_query = name.or_else(|| url.query()).filter(|s| !s.is_empty());
            Some(AnalysisKey::new(asset_root_for_url(url, name_or_query)))
        }
        ResourceSrc::Path(path) => {
            let url = Url::from_file_path(path).ok()?;
            Some(AnalysisKey::new(asset_root_for_url(&url, None)))
        }
    }
}

/// Two-tier track-analysis memoization: a session in-memory map plus durable
/// blobs stored as resources of each track's `AssetScope` (so they follow the
/// track's storage lifecycle). Owned by the single listener task, so it needs
/// no synchronization.
pub(crate) struct TrackAnalysisCache {
    mem: HashMap<AnalysisKey, TrackAnalysis>,
    store: Option<Arc<AssetStore>>,
    /// Active analysis configuration; blobs carrying a different one are
    /// cache misses.
    fingerprint: String,
    /// Insertion order of `mem` keys; the oldest is evicted past the cap.
    order: VecDeque<AnalysisKey>,
}

impl TrackAnalysisCache {
    pub(crate) fn new(store: Option<Arc<AssetStore>>, fingerprint: String) -> Self {
        Self {
            store,
            fingerprint,
            mem: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Look up a cached analysis: memory first, then the scope resource.
    /// `None` on a miss or an unreadable blob.
    pub(crate) fn get(&mut self, key: &AnalysisKey) -> Option<TrackAnalysis> {
        if let Some(analysis) = self.mem.get(key) {
            return Some(analysis.clone());
        }
        let analysis = self.load_disk(key)?;
        self.remember(key.clone(), analysis.clone());
        Some(analysis)
    }

    fn load_disk(&self, key: &AnalysisKey) -> Option<TrackAnalysis> {
        let store = self.store.as_ref()?;
        let resource = Self::resource_key(store, key);
        // Side-effect-free probe first: opening a missing key would create it.
        match store.resource_state(&resource).ok()? {
            AssetResourceState::Committed { .. } => {}
            _ => return None,
        }
        let reader = store.open_resource(&resource, None).ok()?;
        let mut bytes = Vec::new();
        reader.read_into(&mut bytes).ok()?;
        match analysis_from_bytes(&bytes, &self.fingerprint) {
            Ok(analysis) => {
                debug!("track analysis cache: disk hit");
                Some(analysis)
            }
            Err(e) => {
                warn!(%e, ?resource, "track analysis cache: ignoring stale/unreadable blob");
                None
            }
        }
    }

    /// Store freshly derived track analysis in both tiers.
    pub(crate) fn put(&mut self, key: AnalysisKey, analysis: TrackAnalysis) {
        // An analysis with no meaningful slots would be served forever as
        // emptiness on later hits; skip memoizing it in either tier.
        if analysis.waveform.is_none() && analysis.beat.is_none() {
            return;
        }
        self.store_disk(&key, &analysis);
        self.remember(key, analysis);
    }

    /// Insert into the bounded memory tier, evicting the oldest entry past
    /// [`Consts::MAX_MEM_ENTRIES`]. Evicted entries are still served from disk.
    fn remember(&mut self, key: AnalysisKey, analysis: TrackAnalysis) {
        if self.mem.insert(key.clone(), analysis).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > Consts::MAX_MEM_ENTRIES {
            if let Some(old) = self.order.pop_front() {
                self.mem.remove(&old);
            }
        }
    }

    fn resource_key(store: &AssetStore, key: &AnalysisKey) -> ResourceKey {
        store
            .scope(key.asset_root.as_str())
            .key(Consts::ANALYSIS_REL_PATH)
    }

    fn store_disk(&self, key: &AnalysisKey, analysis: &TrackAnalysis) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let resource = Self::resource_key(store, key);
        let bytes = match analysis_to_bytes(analysis, &self.fingerprint) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(%e, ?resource, "track analysis cache: encode failed");
                return;
            }
        };
        if let Err(e) = write_resource(store, &resource, &bytes) {
            warn!(%e, ?resource, "track analysis cache: blob write failed");
        }
    }
}

fn write_resource(
    store: &AssetStore,
    resource: &ResourceKey,
    bytes: &[u8],
) -> Result<(), AssetsError> {
    let writer = match store.acquire_resource(resource, None)? {
        AcquisitionResult::Pending(writer) => writer,
        AcquisitionResult::Ready(reader) => reader.reactivate()?,
        _ => return Ok(()),
    };
    writer.write_at(0, bytes)?;
    let final_len = u64::try_from(bytes.len()).map_err(|_| {
        AssetsError::Io(IoError::new(
            ErrorKind::InvalidInput,
            "track analysis blob length does not fit u64",
        ))
    })?;
    writer.commit(Some(final_len))?;
    Ok(())
}

#[derive(Debug)]
enum AnalysisBytesError {
    Version { found: u32, expected: u32 },
    Fingerprint,
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
            Self::Fingerprint => f.write_str("track analysis blob has a stale config fingerprint"),
            Self::TooLarge => f.write_str("track analysis blob is too large"),
            Self::Corrupt => f.write_str("track analysis blob is corrupt"),
        }
    }
}

fn analysis_to_bytes(
    analysis: &TrackAnalysis,
    fingerprint: &str,
) -> Result<Vec<u8>, AnalysisBytesError> {
    let waveform = analysis
        .waveform
        .as_ref()
        .map(Vec::<u8>::from)
        .unwrap_or_default();
    let beat = analysis
        .beat
        .as_ref()
        .map(Vec::<u8>::from)
        .unwrap_or_default();
    let mut out =
        Vec::with_capacity(4 + 4 + fingerprint.len() + 8 + waveform.len() + 8 + beat.len() + 8);
    out.extend_from_slice(&Consts::ANALYSIS_BYTES_VERSION.to_le_bytes());
    let fingerprint_len =
        u32::try_from(fingerprint.len()).map_err(|_| AnalysisBytesError::TooLarge)?;
    out.extend_from_slice(&fingerprint_len.to_le_bytes());
    out.extend_from_slice(fingerprint.as_bytes());
    write_section(&mut out, &waveform)?;
    write_section(&mut out, &beat)?;
    out.extend_from_slice(&analysis.source_frames.to_le_bytes());
    Ok(out)
}

fn analysis_from_bytes(
    bytes: &[u8],
    fingerprint: &str,
) -> Result<TrackAnalysis, AnalysisBytesError> {
    let mut cursor = 0usize;
    let version = read_u32(bytes, &mut cursor)?;
    if version != Consts::ANALYSIS_BYTES_VERSION {
        return Err(AnalysisBytesError::Version {
            found: version,
            expected: Consts::ANALYSIS_BYTES_VERSION,
        });
    }
    let fingerprint_len =
        usize::try_from(read_u32(bytes, &mut cursor)?).map_err(|_| AnalysisBytesError::Corrupt)?;
    let stored = read_slice(bytes, &mut cursor, fingerprint_len)?;
    if stored != fingerprint.as_bytes() {
        return Err(AnalysisBytesError::Fingerprint);
    }
    let waveform_bytes = read_section(bytes, &mut cursor)?;
    let beat_bytes = read_section(bytes, &mut cursor)?;
    let source_frames = read_u64(bytes, &mut cursor)?;
    if cursor != bytes.len() {
        return Err(AnalysisBytesError::Corrupt);
    }

    let mut analysis = TrackAnalysis::default();
    if !waveform_bytes.is_empty() {
        analysis.waveform =
            Some(Waveform::try_from(waveform_bytes).map_err(|_| AnalysisBytesError::Corrupt)?);
    }
    if !beat_bytes.is_empty() {
        analysis.beat =
            Some(BeatGrid::try_from(beat_bytes).map_err(|_| AnalysisBytesError::Corrupt)?);
    }
    analysis.source_frames = source_frames;
    Ok(analysis)
}

fn write_section(out: &mut Vec<u8>, section: &[u8]) -> Result<(), AnalysisBytesError> {
    let len = u64::try_from(section.len()).map_err(|_| AnalysisBytesError::TooLarge)?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(section);
    Ok(())
}

fn read_section<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], AnalysisBytesError> {
    let len = usize::try_from(read_u64(bytes, cursor)?).map_err(|_| AnalysisBytesError::Corrupt)?;
    read_slice(bytes, cursor, len)
}

fn read_slice<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    len: usize,
) -> Result<&'a [u8], AnalysisBytesError> {
    let end = cursor.checked_add(len).ok_or(AnalysisBytesError::Corrupt)?;
    let slice = bytes.get(*cursor..end).ok_or(AnalysisBytesError::Corrupt)?;
    *cursor = end;
    Ok(slice)
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

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    // The test macro import shadows the `kithara` crate name; use absolute path.
    use ::kithara::{
        assets::{AssetResourceState, AssetStore, AssetStoreBuilder},
        audio::{BeatGrid, GridSegment, Waveform},
        prelude::ResourceConfig,
    };
    use kithara_queue::TrackSource;
    use kithara_test_utils::kithara;

    use super::{
        AnalysisBytesError, AnalysisKey, Consts, TrackAnalysisCache, analysis_from_bytes,
        analysis_to_bytes, source_key,
    };
    use crate::waveform::TrackAnalysis;

    const FP: &str = "buckets=1500;beat=test";

    fn wave() -> Waveform {
        // version 1 + one bucket of three 0.5 band heights (0.5 = 0x3F000000).
        Waveform::try_from([1, 0, 0, 0, 0, 0, 0, 63, 0, 0, 0, 63, 0, 0, 0, 63].as_slice())
            .expect("hand-built blob is valid")
    }

    fn grid() -> BeatGrid {
        BeatGrid::new(
            128.0,
            vec![0, 10_000, 20_000],
            vec![0, 40_000],
            vec![GridSegment::new(0, 40_000, 1.01)],
        )
    }

    fn full_analysis() -> TrackAnalysis {
        let mut analysis = TrackAnalysis::default();
        analysis.waveform = Some(wave());
        analysis.beat = Some(grid());
        analysis.source_frames = 1_234_567;
        analysis
    }

    fn wave_only() -> TrackAnalysis {
        let mut analysis = TrackAnalysis::default();
        analysis.waveform = Some(wave());
        analysis
    }

    fn store_in(dir: &Path) -> Arc<AssetStore> {
        Arc::new(AssetStoreBuilder::<()>::default().root_dir(dir).build())
    }

    fn cache_over(store: &Arc<AssetStore>) -> TrackAnalysisCache {
        TrackAnalysisCache::new(Some(Arc::clone(store)), FP.to_string())
    }

    #[kithara::test]
    fn codec_round_trips_waveform_and_beat() {
        let analysis = full_analysis();
        let bytes = analysis_to_bytes(&analysis, FP).expect("encodes");
        let back = analysis_from_bytes(&bytes, FP).expect("decodes");
        assert_eq!(
            back.waveform.expect("waveform survives").buckets(),
            wave().buckets()
        );
        assert_eq!(back.beat.expect("beat grid survives"), grid());
        assert_eq!(
            back.source_frames, 1_234_567,
            "source_frames must survive the round-trip"
        );
    }

    #[kithara::test]
    fn codec_round_trips_without_beat() {
        let bytes = analysis_to_bytes(&wave_only(), FP).expect("encodes");
        let back = analysis_from_bytes(&bytes, FP).expect("decodes");
        assert!(back.waveform.is_some());
        assert!(back.beat.is_none(), "absent beat must stay absent");
    }

    #[kithara::test]
    fn codec_round_trips_beat_only() {
        let mut analysis = TrackAnalysis::default();
        analysis.beat = Some(grid());
        let bytes = analysis_to_bytes(&analysis, FP).expect("encodes");
        let back = analysis_from_bytes(&bytes, FP).expect("decodes");
        assert!(back.waveform.is_none());
        assert_eq!(back.beat.expect("beat grid survives"), grid());
    }

    #[kithara::test]
    fn stale_fingerprint_is_a_miss() {
        let bytes = analysis_to_bytes(&full_analysis(), FP).expect("encodes");
        assert!(
            matches!(
                analysis_from_bytes(&bytes, "buckets=1500;beat=other"),
                Err(AnalysisBytesError::Fingerprint)
            ),
            "a config change must invalidate the blob"
        );
    }

    #[kithara::test]
    fn url_query_distinguishes_tracks_like_the_stream_layer() {
        let a = source_key(&TrackSource::Uri(
            "https://h.example/track/streamhq?id=123".to_string(),
        ))
        .expect("url source is keyable");
        let b = source_key(&TrackSource::Uri(
            "https://h.example/track/streamhq?id=456".to_string(),
        ))
        .expect("url source is keyable");
        assert_ne!(a, b, "query carries track identity in the file layer");

        let again = source_key(&TrackSource::Uri(
            "https://h.example/track/streamhq?id=123".to_string(),
        ))
        .expect("url source is keyable");
        assert_eq!(a, again, "keys are stable across calls");
    }

    #[kithara::test]
    fn config_source_shares_the_players_asset_root() {
        // A Config source must key by the same name-or-query rule the
        // file layer uses for its asset root.
        let cfg = ResourceConfig::for_src("https://h.example/a.mp3?token=1")
            .expect("valid url")
            .build();
        let from_cfg =
            source_key(&TrackSource::Config(Box::new(cfg))).expect("config source is keyable");
        let from_uri = source_key(&TrackSource::Uri(
            "https://h.example/a.mp3?token=1".to_string(),
        ))
        .expect("url source is keyable");
        assert_eq!(from_cfg, from_uri);
    }

    #[kithara::test(native)]
    fn local_path_sources_are_keyable() {
        let key = source_key(&TrackSource::Uri("/tmp/song.mp3".to_string()));
        assert!(key.is_some(), "local files must cache their analysis");
    }

    #[kithara::test]
    fn memory_round_trips_without_store() {
        let mut cache = TrackAnalysisCache::new(None, FP.to_string());
        let key = AnalysisKey::new("root_a");
        assert!(cache.get(&key).is_none());
        cache.put(key.clone(), full_analysis());
        let cached = cache.get(&key).expect("analysis must be cached");
        assert_eq!(cached.waveform.expect("waveform cached").len(), 1);
        assert!(cached.beat.is_some(), "beat grid rides along");
    }

    #[kithara::test]
    fn empty_analysis_is_not_memoized() {
        let mut cache = TrackAnalysisCache::new(None, FP.to_string());
        let key = AnalysisKey::new("root_empty");
        cache.put(key.clone(), TrackAnalysis::default());
        assert!(
            cache.get(&key).is_none(),
            "an analysis with no slots must not be served from the cache"
        );
    }

    #[kithara::test]
    fn memory_tier_is_bounded_with_disk_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store_in(dir.path());
        let mut cache = cache_over(&store);
        for i in 0..=Consts::MAX_MEM_ENTRIES {
            cache.put(AnalysisKey::new(format!("root_{i}")), full_analysis());
        }
        assert!(
            cache.mem.len() <= Consts::MAX_MEM_ENTRIES,
            "memory tier stays bounded under a whole-library sweep"
        );
        let oldest = AnalysisKey::new("root_0");
        assert!(!cache.mem.contains_key(&oldest), "oldest entry evicted");
        assert!(
            cache.get(&oldest).is_some(),
            "evicted entry is still served from the disk tier"
        );
    }

    #[kithara::test]
    fn disk_survives_a_fresh_cache_instance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store_in(dir.path());
        let key = AnalysisKey::new("root_b");
        let mut writer = cache_over(&store);
        writer.put(key.clone(), full_analysis());

        // A new cache with an empty memory tier must still find the blob.
        let mut reader = cache_over(&store);
        let cached = reader.get(&key).expect("disk analysis must load");
        assert_eq!(cached.waveform.expect("waveform persisted").len(), 1);
        assert_eq!(cached.beat.expect("beat grid persisted"), grid());
    }

    #[kithara::test]
    fn artifact_is_a_scope_resource_and_dies_with_the_asset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store_in(dir.path());
        let key = AnalysisKey::new("track_root");
        let mut cache = cache_over(&store);
        cache.put(key.clone(), full_analysis());

        // The artifact is a resource of the track's scope...
        let scope = store.scope("track_root");
        let resource = scope.key(Consts::ANALYSIS_REL_PATH);
        assert!(
            matches!(
                store.resource_state(&resource),
                Ok(AssetResourceState::Committed { .. })
            ),
            "analysis blob must be a committed resource under the track scope"
        );
        // Deleting the asset takes the analysis with it.
        scope.delete_asset().expect("asset deletes");
        let mut fresh = cache_over(&store);
        assert!(
            fresh.get(&key).is_none(),
            "analysis must follow the track asset's lifecycle"
        );
    }

    #[kithara::test]
    fn stale_fingerprint_blob_is_re_analysed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store_in(dir.path());
        let key = AnalysisKey::new("root_fp");
        let mut old = TrackAnalysisCache::new(Some(Arc::clone(&store)), "old-config".to_string());
        old.put(key.clone(), full_analysis());

        let mut current = cache_over(&store);
        assert!(
            current.get(&key).is_none(),
            "a blob from another analysis config must be a miss"
        );
        // Overwriting with the current config works.
        current.put(key.clone(), full_analysis());
        let mut fresh = cache_over(&store);
        assert!(fresh.get(&key).is_some());
    }
}
