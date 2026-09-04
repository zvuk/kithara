use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroU32,
};

use kithara::{
    analysis::{AnalysisFile, AnalysisFingerprint, AnalysisProgress, AnalysisToken},
    assets::{AssetResource, AssetResourceState, ReadSide, ResourceKey},
    decode::DecodeError,
    platform::time::Duration,
};
use tracing::{debug, warn};

use crate::pools::{AppResourceConfig, AppStore, Pools};

pub(crate) mod persistence;

pub(crate) use persistence::{AnalysisPersistence, AnalysisPersistenceError};

/// Tunables for the analysis cache, grouped to keep the module surface small.
struct Consts;

impl Consts {
    /// Cap on the in-memory tier; past it the oldest entries fall back to disk.
    const MAX_MEM_ENTRIES: usize = 64;
}

/// Physical analysis resource together with the store that owns it.
#[derive(Clone, Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct AnalysisTarget {
    store: AppStore,
    #[field(get, vis = "pub(crate)")]
    key: ResourceKey,
}

impl AnalysisTarget {
    pub(crate) fn for_config(config: &AppResourceConfig) -> Result<Self, DecodeError> {
        let key = config.asset_key(&AssetResource::Named {
            namespace: "analysis".to_string(),
            name: "track.analysis".to_string(),
        })?;
        Ok(Self {
            key,
            store: config.store().clone(),
        })
    }

    pub(crate) fn is_same(&self, other: &Self) -> bool {
        self.key == other.key && self.store.is_same(&other.store)
    }
}

struct MemoryEntry {
    target: AnalysisTarget,
    progress: AnalysisProgress,
}

/// Two-tier track-analysis memoization: a session in-memory map plus durable
/// blobs stored as resources of each track's `AssetScope` (so they follow the
/// track's storage lifecycle). Owned by the single listener task, so it needs
/// no synchronization.
pub(crate) struct TrackAnalysisCache {
    pools: Pools,
    chunk_duration: Duration,
    mem: HashMap<ResourceKey, Vec<MemoryEntry>>,
    /// Active analysis configuration, per artifact: a stored artifact whose
    /// tag differs is dropped on its own, so a waveform resolution change no
    /// longer invalidates stored beat results.
    fingerprint: AnalysisFingerprint,
    /// Insertion order of store-qualified targets; the oldest is evicted past
    /// the cap.
    order: VecDeque<AnalysisTarget>,
}

impl TrackAnalysisCache {
    pub(crate) fn new(
        fingerprint: AnalysisFingerprint,
        pools: Pools,
        chunk_seconds: NonZeroU32,
    ) -> Self {
        Self {
            pools,
            chunk_duration: Duration::from_secs(u64::from(chunk_seconds.get())),
            fingerprint,
            mem: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Look up a cached analysis: memory first, then the scope resource.
    /// `None` on a miss or an unreadable blob.
    pub(crate) fn get(
        &mut self,
        target: &AnalysisTarget,
        source_sample_rate: NonZeroU32,
    ) -> Option<AnalysisProgress> {
        if let Some(progress) = self.mem.get(&target.key).and_then(|entries| {
            entries
                .iter()
                .find(|entry| {
                    entry.target.is_same(target)
                        && entry.progress.analysis().source_sample_rate() == source_sample_rate
                })
                .map(|entry| entry.progress.clone())
        }) {
            return Some(progress);
        }
        let progress = self.load_disk(target, source_sample_rate)?;
        self.remember(target.clone(), progress.clone());
        Some(progress)
    }

    fn load_disk(
        &self,
        target: &AnalysisTarget,
        source_sample_rate: NonZeroU32,
    ) -> Option<AnalysisProgress> {
        let resource = &target.key;
        // Side-effect-free probe first: opening a missing key would create it.
        match target.store.resource_state(resource).ok()? {
            AssetResourceState::Committed { .. } => {}
            _ => return None,
        }
        let reader = target.store.open_resource(resource, None).ok()?;
        let mut bytes = self.pools.get::<u8>();
        reader.read_into(&mut bytes).ok()?;
        match AnalysisFile::parse(&bytes, &self.fingerprint) {
            Ok(file)
                if file.spec().source_sample_rate() == source_sample_rate
                    && file.spec().matches_chunk_duration(self.chunk_duration) =>
            {
                debug!("track analysis cache: disk hit");
                Some(file.into())
            }
            Ok(_) => None,
            Err(e) => {
                warn!(%e, ?resource, "track analysis cache: ignoring stale/unreadable progress");
                None
            }
        }
    }

    /// Store the latest publication in the bounded memory tier.
    pub(crate) fn put(&mut self, target: AnalysisTarget, progress: AnalysisProgress) {
        let analysis = progress.analysis();
        // An analysis with no meaningful slots would be served forever as
        // emptiness on later hits; skip memoizing it in either tier.
        if analysis.waveform().is_none() && analysis.beat().is_none() && !progress.is_resumable() {
            return;
        }
        self.remember(target, progress);
    }

    /// Insert into the bounded memory tier, evicting the oldest entry past
    /// [`Consts::MAX_MEM_ENTRIES`]. Evicted entries are still served from disk.
    fn remember(&mut self, target: AnalysisTarget, progress: AnalysisProgress) {
        let entries = self.mem.entry(target.key.clone()).or_default();
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.target.is_same(&target))
        {
            entry.progress = progress;
            return;
        }

        entries.push(MemoryEntry {
            progress,
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
}

/// The token a stored blob carries: derived from the resource key the blob
/// lives under, so a restored snapshot identifies the same content it was
/// analysed from rather than a session-scoped id.
pub(crate) fn token_for(key: &ResourceKey) -> AnalysisToken {
    match (key.asset_root(), key.rel_path()) {
        (Some(root), Some(rel)) => format!("{root}/{rel}").into(),
        _ => key
            .as_absolute_path()
            .map_or_else(|| "unkeyed".into(), |path| path.display().to_string())
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use ::kithara::platform::sync::Arc;
    /// The test macro import shadows the `kithara` crate name; use absolute path.
    use ::kithara::{
        analysis::{
            AnalysisFingerprint, AnalysisProgress, BeatArtifact, BeatSnapshot, BeatState, Coverage,
            FrameRange, TrackAnalysis, Waveform,
        },
        assets::{AssetLayout, AssetLayoutRegistry, AssetResource, AssetSource, StorageBackend},
        file::File,
        prelude::ResourceSrc,
    };
    use kithara_test_utils::kithara;

    use super::{AnalysisTarget, Consts, TrackAnalysisCache};
    use crate::pools::{self, AppPools, AppResourceConfig, AppStore, Pools};

    /// The beat tag two tests must agree on: one of them keeps it while the
    /// waveform tag moves.
    const BEAT_TAG: &str = "beat:test:v1";

    fn fingerprint(wave: &str, beat: &str) -> AnalysisFingerprint {
        AnalysisFingerprint::new(Some(beat), Some(wave))
    }

    fn fp() -> AnalysisFingerprint {
        fingerprint("wave:native:max1500:v1", BEAT_TAG)
    }

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(44_100).expect("fixture rate is non-zero")
    }

    fn chunk_seconds() -> NonZeroU32 {
        NonZeroU32::new(16).expect("fixture chunk duration is non-zero")
    }

    fn test_pools() -> Pools {
        pools::build().expect("valid app pool policy")
    }

    fn progress(analysis: TrackAnalysis) -> AnalysisProgress {
        AnalysisProgress::try_from(analysis).expect("settled fixture is valid progress")
    }

    fn wave() -> Waveform {
        // version 1 + one bucket of three 0.5 band heights (0.5 = 0x3F000000).
        Waveform::try_from([1, 0, 0, 0, 0, 0, 0, 63, 0, 0, 0, 63, 0, 0, 0, 63].as_slice())
            .expect("hand-built blob is valid")
    }

    fn grid() -> BeatArtifact {
        BeatArtifact::new(
            128.0,
            vec![(0, Some(0.9)), (10_000, Some(0.75)), (20_000, None)],
            vec![(0, Some(0.9)), (40_000, None)],
        )
    }

    fn analysis(
        beat: Option<BeatArtifact>,
        waveform: Option<Waveform>,
        extent: u64,
    ) -> TrackAnalysis {
        let mut coverage = Coverage::default();
        coverage.insert(FrameRange::new(0, extent));
        TrackAnalysis::builder()
            .token("assets/track.analysis".into())
            .revision(7)
            .source_sample_rate(rate())
            .extent(extent)
            .settled(true)
            .coverage(coverage)
            .fingerprint(fp())
            .maybe_waveform(waveform)
            .maybe_beat(beat.map(|grid| {
                BeatSnapshot::new(grid, BeatState::Provisional, vec![FrameRange::new(100, 50)])
            }))
            .build()
    }

    fn full_analysis() -> TrackAnalysis {
        analysis(Some(grid()), Some(wave()), 1_234_567)
    }

    fn memory_store() -> AppStore {
        AppStore::builder(test_pools())
            .backend(StorageBackend::Memory)
            .build()
    }

    fn config(store: &AppStore, src: &str, discriminator: Option<&str>) -> AppResourceConfig {
        let builder =
            AppResourceConfig::for_src(ResourceSrc::parse(src).expect("valid test source"))
                .store(store.clone());
        match discriminator {
            Some(discriminator) => builder.discriminator(discriminator).build(),
            None => builder.build(),
        }
    }

    fn target_for(store: &AppStore, src: &str, discriminator: Option<&str>) -> AnalysisTarget {
        AnalysisTarget::for_config(&config(store, src, discriminator))
            .expect("test source has a layout-owned analysis target")
    }

    fn target(store: &AppStore, discriminator: &str) -> AnalysisTarget {
        target_for(
            store,
            "https://analysis.test.invalid/track.mp3",
            Some(discriminator),
        )
    }

    fn analysis_cache() -> TrackAnalysisCache {
        TrackAnalysisCache::new(fp(), test_pools(), chunk_seconds())
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
        fn path(&self, _resource: &AssetResource) -> String {
            "../escape".to_string()
        }

        fn root(&self, _source: &AssetSource) -> String {
            "root".to_string()
        }
    }

    #[kithara::test]
    fn invalid_layout_is_not_treated_as_an_uncacheable_source() {
        let layouts =
            AssetLayoutRegistry::default().with::<File<AppPools>>(Arc::new(InvalidLayout));
        let store = AppStore::builder(test_pools())
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
        assert!(cache.get(&target, rate()).is_none());
        cache.put(target.clone(), progress(full_analysis()));
        let cached = cache.get(&target, rate()).expect("analysis must be cached");
        assert_eq!(
            cached.analysis().waveform().expect("waveform cached").len(),
            1
        );
        assert!(cached.analysis().beat().is_some(), "beat grid rides along");
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
        cache.put(first.clone(), progress(analysis(None, Some(wave()), 111)));
        cache.put(second.clone(), progress(analysis(None, Some(wave()), 222)));

        assert_eq!(
            cache
                .get(&first, rate())
                .expect("first store entry")
                .analysis()
                .source_frames(),
            111
        );
        assert_eq!(
            cache
                .get(&second, rate())
                .expect("second store entry")
                .analysis()
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
        cache.put(target.clone(), progress(analysis(None, None, 0)));
        assert!(
            cache.get(&target, rate()).is_none(),
            "an analysis with no slots must not be served from the cache"
        );
    }

    #[kithara::test]
    fn memory_tier_is_bounded() {
        let store = memory_store();
        let mut cache = analysis_cache();
        let oldest = target(&store, "root_0");
        for i in 0..=Consts::MAX_MEM_ENTRIES {
            cache.put(
                target(&store, &format!("root_{i}")),
                progress(full_analysis()),
            );
        }
        assert!(
            cache.order.len() <= Consts::MAX_MEM_ENTRIES,
            "memory tier stays bounded under a whole-library sweep"
        );
        assert!(
            !cache.mem.contains_key(oldest.key()),
            "oldest entry evicted"
        );
        assert!(cache.get(&oldest, rate()).is_none());
    }

    #[kithara::test]
    fn an_unsettled_snapshot_without_resume_state_is_rejected() {
        let mut coverage = Coverage::default();
        coverage.insert(FrameRange::new(0, 500));
        let partial = TrackAnalysis::builder()
            .token("assets/track.analysis".into())
            .revision(3)
            .source_sample_rate(rate())
            .extent(1_000)
            .coverage(coverage)
            .fingerprint(fp())
            .waveform(wave())
            .build();
        assert_eq!(partial.coverage().frames(), 500);
        assert_eq!(partial.extent(), Some(1_000));
        assert!(
            AnalysisProgress::try_from(partial).is_err(),
            "partial cache entries must carry opaque analyzer resume state"
        );
    }

    #[kithara::test]
    fn a_settled_snapshot_is_cached_even_with_a_gap_left_in_it() {
        let store = memory_store();
        let target = target(&store, "root_settled");
        let mut cache = analysis_cache();

        // Encoder priming: the source cannot deliver its first frames, so the
        // pass ended with them uncovered and nothing left to try.
        let mut coverage = Coverage::default();
        coverage.insert(FrameRange::new(20, 980));
        let settled = TrackAnalysis::builder()
            .token("assets/track.analysis".into())
            .revision(3)
            .source_sample_rate(rate())
            .extent(1_000)
            .settled(true)
            .coverage(coverage)
            .fingerprint(fp())
            .waveform(wave())
            .build();
        assert!(!settled.is_complete(), "a gap is left at the head");

        cache.put(target.clone(), progress(settled));
        assert!(
            cache.get(&target, rate()).is_some(),
            "a pass with nothing left to reach must not be re-run every launch"
        );
    }
}
