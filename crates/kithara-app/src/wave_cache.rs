use std::{
    collections::{HashMap, VecDeque},
    io::{Error as IoError, ErrorKind},
    num::NonZeroU32,
};

use kithara::{
    assets::{
        AcquisitionResult, AssetResource, AssetResourceState, AssetStore, AssetsError, ReadSide,
        ResourceKey, WriteSide,
    },
    audio::{BeatGrid, Waveform},
    decode::DecodeError,
    prelude::ResourceConfig,
};
use tracing::{debug, warn};

use crate::waveform::TrackAnalysis;

/// Tunables for the analysis cache, grouped to keep the module surface small.
struct Consts;

impl Consts {
    const ANALYSIS_BYTES_VERSION: u32 = 0x4b41_0005;
    /// Cap on the in-memory tier; past it the oldest entries fall back to disk.
    const MAX_MEM_ENTRIES: usize = 64;
}

/// Physical analysis resource together with the store that owns it.
#[derive(Clone, Debug)]
pub(crate) struct AnalysisTarget {
    key: ResourceKey,
    store: AssetStore,
}

impl AnalysisTarget {
    pub(crate) fn for_config(config: &ResourceConfig) -> Result<Self, DecodeError> {
        let key = config.asset_key(&AssetResource::Named {
            namespace: "analysis".to_string(),
            name: "track.analysis".to_string(),
        })?;
        Ok(Self {
            key,
            store: config.store().clone(),
        })
    }

    pub(crate) fn key(&self) -> &ResourceKey {
        &self.key
    }

    pub(crate) fn is_same(&self, other: &Self) -> bool {
        self.key == other.key && self.store.is_same(&other.store)
    }
}

struct MemoryEntry {
    analysis: TrackAnalysis,
    target: AnalysisTarget,
}

/// Two-tier track-analysis memoization: a session in-memory map plus durable
/// blobs stored as resources of each track's `AssetScope` (so they follow the
/// track's storage lifecycle). Owned by the single listener task, so it needs
/// no synchronization.
pub(crate) struct TrackAnalysisCache {
    mem: HashMap<ResourceKey, Vec<MemoryEntry>>,
    /// Active analysis configuration; blobs carrying a different one are
    /// cache misses.
    fingerprint: String,
    /// Insertion order of store-qualified targets; the oldest is evicted past
    /// the cap.
    order: VecDeque<AnalysisTarget>,
}

impl TrackAnalysisCache {
    pub(crate) fn new(fingerprint: String) -> Self {
        Self {
            fingerprint,
            mem: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Look up a cached analysis: memory first, then the scope resource.
    /// `None` on a miss or an unreadable blob.
    pub(crate) fn get(&mut self, target: &AnalysisTarget) -> Option<TrackAnalysis> {
        if let Some(analysis) = self.mem.get(&target.key).and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.target.is_same(target))
                .map(|entry| entry.analysis.clone())
        }) {
            return Some(analysis);
        }
        let analysis = self.load_disk(target)?;
        self.remember(target.clone(), analysis.clone());
        Some(analysis)
    }

    fn load_disk(&self, target: &AnalysisTarget) -> Option<TrackAnalysis> {
        let resource = &target.key;
        // Side-effect-free probe first: opening a missing key would create it.
        match target.store.resource_state(resource).ok()? {
            AssetResourceState::Committed { .. } => {}
            _ => return None,
        }
        let reader = target.store.open_resource(resource, None).ok()?;
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
    pub(crate) fn put(&mut self, target: AnalysisTarget, analysis: TrackAnalysis) {
        // An analysis with no meaningful slots would be served forever as
        // emptiness on later hits; skip memoizing it in either tier.
        if analysis.waveform().is_none() && analysis.beat().is_none() {
            return;
        }
        self.store_disk(&target, &analysis);
        self.remember(target, analysis);
    }

    /// Insert into the bounded memory tier, evicting the oldest entry past
    /// [`Consts::MAX_MEM_ENTRIES`]. Evicted entries are still served from disk.
    fn remember(&mut self, target: AnalysisTarget, analysis: TrackAnalysis) {
        let entries = self.mem.entry(target.key.clone()).or_default();
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.target.is_same(&target))
        {
            entry.analysis = analysis;
            return;
        }

        entries.push(MemoryEntry {
            analysis,
            target: target.clone(),
        });
        self.order.push_back(target);

        while self.order.len() > Consts::MAX_MEM_ENTRIES {
            if let Some(old) = self.order.pop_front() {
                let bucket_is_empty = self.mem.get_mut(old.key()).is_some_and(|entries| {
                    entries.retain(|entry| !entry.target.is_same(&old));
                    entries.is_empty()
                });
                if bucket_is_empty {
                    self.mem.remove(old.key());
                }
            }
        }
    }

    fn store_disk(&self, target: &AnalysisTarget, analysis: &TrackAnalysis) {
        let resource = &target.key;
        let bytes = match analysis_to_bytes(analysis, &self.fingerprint) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(%e, ?resource, "track analysis cache: encode failed");
                return;
            }
        };
        if let Err(e) = write_resource(&target.store, resource, &bytes) {
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

#[derive(Debug, derive_more::Display)]
enum AnalysisBytesError {
    #[display("track analysis blob version {found} != expected {expected}")]
    Version { found: u32, expected: u32 },
    #[display("track analysis blob has a stale config fingerprint")]
    Fingerprint,
    #[display("track analysis blob is too large")]
    TooLarge,
    #[display("track analysis blob is corrupt")]
    Corrupt,
}

fn analysis_to_bytes(
    analysis: &TrackAnalysis,
    fingerprint: &str,
) -> Result<Vec<u8>, AnalysisBytesError> {
    let waveform = analysis.waveform().map(Vec::<u8>::from).unwrap_or_default();
    let beat = analysis.beat().map(Vec::<u8>::from).unwrap_or_default();
    let mut out =
        Vec::with_capacity(4 + 4 + fingerprint.len() + 8 + waveform.len() + 8 + beat.len() + 8 + 4);
    out.extend_from_slice(&Consts::ANALYSIS_BYTES_VERSION.to_le_bytes());
    let fingerprint_len =
        u32::try_from(fingerprint.len()).map_err(|_| AnalysisBytesError::TooLarge)?;
    out.extend_from_slice(&fingerprint_len.to_le_bytes());
    out.extend_from_slice(fingerprint.as_bytes());
    write_section(&mut out, &waveform)?;
    write_section(&mut out, &beat)?;
    out.extend_from_slice(&analysis.source_frames().to_le_bytes());
    let source_sample_rate = analysis.source_sample_rate().map_or(0, NonZeroU32::get);
    out.extend_from_slice(&source_sample_rate.to_le_bytes());
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
    let source_sample_rate = NonZeroU32::new(read_u32(bytes, &mut cursor)?);
    if cursor != bytes.len() {
        return Err(AnalysisBytesError::Corrupt);
    }

    let waveform = (!waveform_bytes.is_empty())
        .then(|| Waveform::try_from(waveform_bytes))
        .transpose()
        .map_err(|_| AnalysisBytesError::Corrupt)?;
    let beat = (!beat_bytes.is_empty())
        .then(|| BeatGrid::try_from(beat_bytes))
        .transpose()
        .map_err(|_| AnalysisBytesError::Corrupt)?;
    Ok(match source_sample_rate {
        Some(sample_rate) => {
            TrackAnalysis::with_source_rate(beat, waveform, source_frames, sample_rate)
        }
        None => TrackAnalysis::new(beat, waveform, source_frames),
    })
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
    use std::{num::NonZeroU32, path::Path};

    // The test macro import shadows the `kithara` crate name; use absolute path.
    use ::kithara::{
        assets::{
            AssetLayout, AssetLayoutRegistry, AssetResource, AssetResourceState, AssetSource,
            AssetStore, AssetStoreBuilder, StorageBackend,
        },
        audio::{BeatGrid, GridSegment, Waveform},
        bufpool::{BytePool, PcmPool},
        file::File,
        prelude::ResourceConfig,
    };
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;
    use url::Url;

    use super::{
        AnalysisBytesError, AnalysisTarget, Consts, TrackAnalysisCache, analysis_from_bytes,
        analysis_to_bytes,
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
            vec![0, 10_000, 20_000, 30_000, 40_000],
            vec![0, 40_000],
            vec![GridSegment::new(0, 40_000, 1.01)],
        )
    }

    fn full_analysis() -> TrackAnalysis {
        TrackAnalysis::with_source_rate(
            Some(grid()),
            Some(wave()),
            1_234_567,
            NonZeroU32::new(44_100).expect("test rate"),
        )
    }

    fn wave_only() -> TrackAnalysis {
        TrackAnalysis::new(None, Some(wave()), 0)
    }

    fn store_in(dir: &Path) -> AssetStore {
        AssetStoreBuilder::default()
            .backend(StorageBackend::Disk { root: dir.into() })
            .build()
    }

    fn memory_store() -> AssetStore {
        AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .build()
    }

    fn config(store: &AssetStore, src: &str, discriminator: Option<&str>) -> ResourceConfig {
        let builder = ResourceConfig::for_src(src)
            .expect("valid test source")
            .store(store.clone())
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default());
        match discriminator {
            Some(discriminator) => builder.discriminator(discriminator.to_string()).build(),
            None => builder.build(),
        }
    }

    fn target_for(store: &AssetStore, src: &str, discriminator: Option<&str>) -> AnalysisTarget {
        AnalysisTarget::for_config(&config(store, src, discriminator))
            .expect("test source has a layout-owned analysis target")
    }

    fn target(store: &AssetStore, discriminator: &str) -> AnalysisTarget {
        target_for(
            store,
            "https://analysis.test.invalid/track.mp3",
            Some(discriminator),
        )
    }

    fn analysis_cache() -> TrackAnalysisCache {
        TrackAnalysisCache::new(FP.to_string())
    }

    #[kithara::test]
    fn codec_round_trips_waveform_and_beat() {
        let analysis = full_analysis();
        let bytes = analysis_to_bytes(&analysis, FP).expect("encodes");
        let back = analysis_from_bytes(&bytes, FP).expect("decodes");
        assert_eq!(
            back.waveform().expect("waveform survives").buckets(),
            wave().buckets()
        );
        assert_eq!(back.beat().expect("beat grid survives"), &grid());
        assert_eq!(
            back.source_frames(),
            1_234_567,
            "source_frames must survive the round-trip"
        );
        assert_eq!(
            back.source_sample_rate(),
            NonZeroU32::new(44_100),
            "source sample rate must survive the round-trip"
        );
    }

    #[kithara::test]
    fn codec_round_trips_without_beat() {
        let bytes = analysis_to_bytes(&wave_only(), FP).expect("encodes");
        let back = analysis_from_bytes(&bytes, FP).expect("decodes");
        assert!(back.waveform().is_some());
        assert!(back.beat().is_none(), "absent beat must stay absent");
    }

    #[kithara::test]
    fn codec_round_trips_beat_only() {
        let analysis = TrackAnalysis::with_source_rate(
            Some(grid()),
            None,
            40_000,
            NonZeroU32::new(48_000).expect("test rate"),
        );
        let bytes = analysis_to_bytes(&analysis, FP).expect("encodes");
        let back = analysis_from_bytes(&bytes, FP).expect("decodes");
        assert!(back.waveform().is_none());
        assert_eq!(back.beat().expect("beat grid survives"), &grid());
        assert_eq!(back.source_sample_rate(), NonZeroU32::new(48_000));
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
    fn source_identity_ignores_query_without_a_discriminator() {
        let store = memory_store();
        let a = target_for(&store, "https://h.example/track/streamhq.mp3?id=123", None);
        let b = target_for(&store, "https://h.example/track/streamhq.mp3?id=456", None);
        assert_eq!(
            a.key(),
            b.key(),
            "query credentials do not fragment one logical asset"
        );

        let again = target_for(&store, "https://h.example/track/streamhq.mp3?id=123", None);
        assert_eq!(a.key(), again.key(), "keys are stable across calls");
    }

    #[kithara::test]
    fn explicit_discriminator_separates_query_selected_content() {
        let store = memory_store();
        let src = "https://h.example/track/streamhq.mp3?id=123";
        let a = target_for(&store, src, Some("content-a"));
        let b = target_for(&store, src, Some("content-b"));

        assert_ne!(a.key(), b.key());
    }

    #[kithara::test]
    fn config_target_is_stable_and_layout_owned() {
        let store = memory_store();
        let cfg = config(&store, "https://h.example/a.mp3?token=1", None);
        let first = AnalysisTarget::for_config(&cfg).expect("config source is keyable");
        let second = AnalysisTarget::for_config(&cfg).expect("config source is keyable");

        assert_eq!(first.key(), second.key());
        assert_eq!(first.key().rel_path(), Some("analysis/track.analysis"));
    }

    #[kithara::test(native)]
    fn local_path_sources_are_keyable() {
        let store = memory_store();
        let target = AnalysisTarget::for_config(&config(&store, "/tmp/song.mp3", None));
        assert!(target.is_ok(), "local files must cache their analysis");
    }

    #[derive(Debug)]
    struct InvalidLayout;

    impl AssetLayout for InvalidLayout {
        fn root(&self, _source: &AssetSource) -> String {
            "root".to_string()
        }

        fn path(&self, _resource: &AssetResource) -> String {
            "../escape".to_string()
        }
    }

    #[kithara::test]
    fn invalid_layout_is_not_treated_as_an_uncacheable_source() {
        let layouts = AssetLayoutRegistry::default().with::<File>(Arc::new(InvalidLayout));
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .layouts(layouts)
            .build();
        let target =
            AnalysisTarget::for_config(&config(&store, "https://h.example/track.mp3", None));

        assert!(
            target.is_err(),
            "invalid layout output must remain an error"
        );
    }

    #[kithara::test]
    fn memory_store_round_trips() {
        let store = memory_store();
        let target = target(&store, "root_a");
        let mut cache = analysis_cache();
        assert!(cache.get(&target).is_none());
        cache.put(target.clone(), full_analysis());
        let cached = cache.get(&target).expect("analysis must be cached");
        assert_eq!(cached.waveform().expect("waveform cached").len(), 1);
        assert!(cached.beat().is_some(), "beat grid rides along");
    }

    #[kithara::test]
    fn same_key_in_different_stores_keeps_distinct_memory_entries() {
        let first_store = memory_store();
        let second_store = memory_store();
        let src = "https://analysis.test.invalid/shared.mp3";
        let first = target_for(&first_store, src, None);
        let second = target_for(&second_store, src, None);
        assert_eq!(first.key(), second.key());
        assert!(!first.is_same(&second));

        let mut cache = analysis_cache();
        cache.put(first.clone(), TrackAnalysis::new(None, Some(wave()), 111));
        cache.put(second.clone(), TrackAnalysis::new(None, Some(wave()), 222));

        assert_eq!(
            cache
                .get(&first)
                .expect("first store entry")
                .source_frames(),
            111
        );
        assert_eq!(
            cache
                .get(&second)
                .expect("second store entry")
                .source_frames(),
            222
        );
        assert_eq!(cache.order.len(), 2);
        assert_eq!(cache.mem.get(first.key()).map(Vec::len), Some(2));
    }

    #[kithara::test]
    fn empty_analysis_is_not_memoized() {
        let store = memory_store();
        let target = target(&store, "root_empty");
        let mut cache = analysis_cache();
        cache.put(target.clone(), TrackAnalysis::default());
        assert!(
            cache.get(&target).is_none(),
            "an analysis with no slots must not be served from the cache"
        );
    }

    #[kithara::test]
    fn memory_tier_is_bounded_with_disk_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store_in(dir.path());
        let mut cache = analysis_cache();
        let oldest = target(&store, "root_0");
        for i in 0..=Consts::MAX_MEM_ENTRIES {
            cache.put(target(&store, &format!("root_{i}")), full_analysis());
        }
        assert!(
            cache.order.len() <= Consts::MAX_MEM_ENTRIES,
            "memory tier stays bounded under a whole-library sweep"
        );
        assert!(
            !cache.mem.contains_key(oldest.key()),
            "oldest entry evicted"
        );
        assert!(
            cache.get(&oldest).is_some(),
            "evicted entry is still served from the disk tier"
        );
    }

    #[kithara::test]
    fn disk_survives_a_fresh_cache_instance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store_in(dir.path());
        let target = target(&store, "root_b");
        let mut writer = analysis_cache();
        writer.put(target.clone(), full_analysis());

        // A new cache with an empty memory tier must still find the blob.
        let mut reader = analysis_cache();
        let cached = reader.get(&target).expect("disk analysis must load");
        assert_eq!(cached.waveform().expect("waveform persisted").len(), 1);
        assert_eq!(cached.beat().expect("beat grid persisted"), &grid());
    }

    #[kithara::test]
    fn artifact_is_a_scope_resource_and_dies_with_the_asset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store_in(dir.path());
        let url = Url::parse("https://analysis.test.invalid/track.mp3").expect("valid test URL");
        let discriminator = "track_root";
        let target = target_for(&store, url.as_str(), Some(discriminator));
        let mut cache = analysis_cache();
        cache.put(target.clone(), full_analysis());

        assert!(
            matches!(
                store.resource_state(target.key()),
                Ok(AssetResourceState::Committed { .. })
            ),
            "analysis blob must be a committed resource under the track scope"
        );
        // Deleting the asset takes the analysis with it.
        let source = AssetSource::Remote {
            url,
            discriminator: Some(discriminator.to_string()),
        };
        let scope = store.scope::<File>(&source).expect("valid analysis scope");
        scope.delete_asset().expect("asset deletes");
        let mut fresh = analysis_cache();
        assert!(
            fresh.get(&target).is_none(),
            "analysis must follow the track asset's lifecycle"
        );
    }

    #[kithara::test]
    fn stale_fingerprint_blob_is_re_analysed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store_in(dir.path());
        let target = target(&store, "root_fp");
        let mut old = TrackAnalysisCache::new("old-config".to_string());
        old.put(target.clone(), full_analysis());

        let mut current = analysis_cache();
        assert!(
            current.get(&target).is_none(),
            "a blob from another analysis config must be a miss"
        );
        // Overwriting with the current config works.
        current.put(target.clone(), full_analysis());
        let mut fresh = analysis_cache();
        assert!(fresh.get(&target).is_some());
    }
}
