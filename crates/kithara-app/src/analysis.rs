use std::collections::VecDeque;

use kithara::{
    audio::analysis::BeatAnalysisConfig,
    bufpool::PcmPool,
    decode::DecodeError,
    events::{Envelope, Event, EventReceiver, TrackId},
    prelude::{PlaybackResamplerBackend, ResourceConfig},
};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    tokio::{
        self,
        sync::{broadcast::error::RecvError, watch},
    },
};
use kithara_queue::{Queue, QueueEvent, TrackSource};

use crate::{
    config::AppConfig,
    sources::build_resource_config,
    state::{UiState, apply_event},
    wave_cache::{AnalysisTarget, TrackAnalysisCache},
    waveform::{TrackAnalysis, TrackAnalysisRunner},
};

type AppBeatAnalysisConfig = BeatAnalysisConfig<PlaybackResamplerBackend>;
type AppResourceConfig = ResourceConfig<PlaybackResamplerBackend>;

/// Upper bound on waveform buckets (native = one per FFT window); only caps very
/// long tracks to bound the cached blob.
const WAVEFORM_MAX_BUCKETS: usize = 96_000;

/// Analysis-aware state listener: mirrors queue events into [`UiState`] and
/// drives the background [`AnalysisController`]. Starts analysing the already
/// loaded library immediately — independent of which UI is open.
pub(crate) async fn listen(
    queue: Arc<Queue>,
    state: Arc<Mutex<UiState>>,
    config: AppConfig,
    cancel: CancelToken,
    mut rx: EventReceiver,
) {
    let mut driver =
        AnalysisController::new(&cancel, &config.beat_analysis, config.pcm_pool.clone());

    // Analyse whatever is already loaded; later tracks arrive as events.
    driver.on_tracks_changed(&queue, &state, &config);

    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            () = driver.drive(&queue, &state, &config) => {}
            event = rx.recv() => match event {
                Ok(Envelope { event, .. }) => {
                    let track_changed =
                        matches!(event, Event::Queue(QueueEvent::CurrentTrackChanged { .. }));
                    let tracks_changed = matches!(
                        event,
                        Event::Queue(QueueEvent::TrackAdded { .. } | QueueEvent::TrackRemoved { .. })
                    );
                    apply_event(event, &queue, &state);
                    if tracks_changed {
                        driver.on_tracks_changed(&queue, &state, &config);
                    }
                    if track_changed {
                        driver.on_track_changed(&queue, &state, &config);
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// Background source-analysis controller owned by the state listener task.
/// Results land in the two-tier [`TrackAnalysisCache`];
pub(crate) struct AnalysisController {
    current: Option<Run>,
    /// Store-qualified analysis target currently published to the UI.
    displayed: Option<AnalysisTarget>,
    cache: TrackAnalysisCache,
    runner: TrackAnalysisRunner,
    /// Tracks waiting for background analysis, current track first.
    pending: VecDeque<TrackId>,
}

/// An in-flight analysis: the track it is for (stale-guard), its content cache
/// key (`None` for an unkeyable source), and its result channel.
struct Run {
    target: Option<AnalysisTarget>,
    rx: watch::Receiver<Option<TrackAnalysis>>,
    track_id: TrackId,
}

/// A run that closed with a usable analysis result.
struct CompletedRun {
    target: Option<AnalysisTarget>,
    analysis: TrackAnalysis,
    track_id: TrackId,
}

impl AnalysisController {
    /// `cancel` must be a child of the app master so analysis stops on app
    /// shutdown. Each source config supplies the store used for its durable
    /// analysis resource.
    pub(crate) fn new(
        cancel: &CancelToken,
        beat_config: &AppBeatAnalysisConfig,
        pcm_pool: PcmPool,
    ) -> Self {
        Self {
            runner: TrackAnalysisRunner::new(
                cancel,
                WAVEFORM_MAX_BUCKETS,
                beat_config.clone(),
                pcm_pool,
            ),
            cache: TrackAnalysisCache::new(analysis_fingerprint(beat_config)),
            current: None,
            displayed: None,
            pending: VecDeque::new(),
        }
    }

    fn cache_completed(&mut self, completed: &CompletedRun) {
        if let Some(target) = &completed.target {
            self.cache.put(target.clone(), completed.analysis.clone());
        }
    }

    /// Cache the finished analysis under its content key, publish it if its
    /// track is still current, and clear the run.
    fn commit(&mut self, state: &Mutex<UiState>) {
        let Some(completed) = self.take_completed_run() else {
            return;
        };

        self.cache_completed(&completed);

        let displayed = completed.target;
        if publish_if_current(state, completed.track_id, completed.analysis) {
            self.displayed = displayed;
        }
    }

    /// Await the run's next event and handle it: publish the staged
    /// intermediate, or commit and pump on close. Parks when no run is active.
    pub(crate) async fn drive(
        &mut self,
        queue: &Arc<Queue>,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        let closed = match &mut self.current {
            Some(run) => run.rx.changed().await.is_err(),
            None => std::future::pending::<bool>().await,
        };

        if closed {
            self.commit(state);
            self.pump(queue, state, config);
        } else {
            self.publish_intermediate(state);
        }
    }

    /// The current track changed: put it at the front of the queue and
    /// preempt an in-flight background run so the visible deck wins.
    pub(crate) fn on_track_changed(
        &mut self,
        queue: &Arc<Queue>,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        if let Some(id) = current_track_id(state) {
            self.pending.retain(|t| *t != id);
            self.pending.push_front(id);
            if let Some(run) = &self.current
                && run.track_id != id
            {
                let preempted = run.track_id;
                self.runner.clear();
                self.current = None;
                self.pending.retain(|t| *t != preempted);
                self.pending.push_back(preempted);
            }
        }
        self.pump(queue, state, config);
    }

    /// Re-sync the pending queue with the library (current track first) and
    /// keep the background pass going. Cached tracks are skipped cheaply.
    pub(crate) fn on_tracks_changed(
        &mut self,
        queue: &Arc<Queue>,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        {
            let st = state.lock();
            let ids: Vec<TrackId> = st.tracks.iter().map(|entry| entry.id).collect();
            self.pending = pending_order(&ids, st.current_track_index);
        }
        if let Some(run) = &self.current {
            self.pending.retain(|t| *t != run.track_id);
        }
        self.pump(queue, state, config);
    }

    /// Publish the first part emit to the UI (no caching) when its
    /// track is still current; the beat overlay arrives on the closing commit.
    fn publish_intermediate(&self, state: &Mutex<UiState>) {
        let Some(run) = &self.current else {
            return;
        };

        let Some(analysis) = run.rx.borrow().clone() else {
            return;
        };

        publish_if_current(state, run.track_id, analysis);
    }

    /// Start the next analysis worth running, if none is in flight: serve
    /// the current track from cache, skip background tracks that are cached
    /// or unkeyable, decode the first genuine miss.
    pub(crate) fn pump(&mut self, queue: &Arc<Queue>, state: &Mutex<UiState>, config: &AppConfig) {
        if self.current.is_some() {
            return;
        }

        // No analyzers found: decoding would produce nothing.
        if !self.runner.is_active() {
            self.pending.clear();
            return;
        }

        while let Some(track_id) = self.pending.pop_front() {
            // Track gone from the queue since it was enqueued: skip.
            let Some(source) = queue.track_source(track_id) else {
                continue;
            };

            let Some(cfg) = resource_config_from_source(source, config) else {
                continue;
            };
            let target = match AnalysisTarget::for_config(&cfg) {
                Ok(target) => Some(target),
                Err(error) => {
                    self.reject_target(state, track_id, &error);
                    continue;
                }
            };
            let is_current = current_track_id(state) == Some(track_id);

            match plan_analysis(target.as_ref(), self.displayed.as_ref(), &mut self.cache) {
                Plan::Skip => {}
                Plan::Serve(analysis) => {
                    if is_current {
                        state.lock().set_analysis(Some(analysis));
                        self.displayed = target;
                    }
                }
                Plan::Decode => {
                    // An unkeyable source cannot be cached, so a background
                    // decode would be thrown away; decode it only for display.
                    if !is_current && target.is_none() {
                        continue;
                    }

                    if is_current {
                        state.lock().set_analysis(None);
                        self.displayed = None;
                    }

                    let rx = self.runner.analyze(cfg);
                    self.current = Some(Run {
                        target,
                        rx,
                        track_id,
                    });
                    return;
                }
            }
        }
    }

    fn reject_target(&mut self, state: &Mutex<UiState>, track_id: TrackId, error: &DecodeError) {
        tracing::warn!(
            %error,
            ?track_id,
            "analysis layout rejected the derived resource key"
        );
        let cleared = {
            let mut state = state.lock();
            if current_track_id_in(&state) != Some(track_id) {
                false
            } else {
                state.set_analysis(None);
                true
            }
        };
        if cleared {
            self.displayed = None;
        }
    }

    fn take_completed_run(&mut self) -> Option<CompletedRun> {
        let run = self.current.take()?;
        let analysis = run.rx.borrow().clone()?;

        Some(CompletedRun {
            analysis,
            track_id: run.track_id,
            target: run.target,
        })
    }
}

/// Fingerprint of the active analysis configuration, stored inside each durable blob.
/// A mismatch is a cache miss, so config changes re-analyse.
fn analysis_fingerprint(beat_config: &AppBeatAnalysisConfig) -> String {
    let beat = beat_config.cache_tag().unwrap_or_else(|| "off".to_string());
    format!("wave=native:max{WAVEFORM_MAX_BUCKETS};beat={beat}")
}

/// What [`AnalysisController::pump`] should do for a track.
enum Plan {
    /// Already shown for this content: leave the analysis as is.
    Skip,
    /// Cached (memory or disk): publish it without re-decoding.
    Serve(TrackAnalysis),
    /// Not cached (or an unkeyable source): analyse.
    Decode,
}

/// Decide the action for a track, guarding against re-decoding content that
/// is already shown or cached. An in-flight run needs no guard here: `pump`
/// returns before planning while one is active.
fn plan_analysis(
    target: Option<&AnalysisTarget>,
    displayed: Option<&AnalysisTarget>,
    cache: &mut TrackAnalysisCache,
) -> Plan {
    let Some(target) = target else {
        // No stable key (the reserved non-exhaustive source seam): cannot
        return Plan::Decode;
    };

    if displayed.is_some_and(|displayed| displayed.is_same(target)) {
        return Plan::Skip;
    }

    cache.get(target).map_or(Plan::Decode, Plan::Serve)
}

/// Library tracks in background-analysis order: the current track first,
/// then the rest in list order.
fn pending_order(ids: &[TrackId], current: Option<usize>) -> VecDeque<TrackId> {
    let mut order = VecDeque::with_capacity(ids.len());
    if let Some(id) = current.and_then(|i| ids.get(i)) {
        order.push_back(*id);
    }
    for (i, id) in ids.iter().enumerate() {
        if current != Some(i) {
            order.push_back(*id);
        }
    }
    order
}

fn current_track_id(state: &Mutex<UiState>) -> Option<TrackId> {
    let st = state.lock();
    current_track_id_in(&st)
}

fn current_track_id_in(st: &UiState) -> Option<TrackId> {
    st.current_track_index
        .and_then(|i| st.tracks.get(i))
        .map(|entry| entry.id)
}

fn publish_if_current(state: &Mutex<UiState>, track_id: TrackId, analysis: TrackAnalysis) -> bool {
    let mut st = state.lock();
    if current_track_id_in(&st) != Some(track_id) {
        return false;
    }
    st.set_analysis(Some(analysis));
    true
}

/// Build an analysis resource from a track's source, reusing the shared
/// stores so the analysis and the player share one download.
fn resource_config_from_source(
    source: TrackSource,
    config: &AppConfig,
) -> Option<AppResourceConfig> {
    match source {
        TrackSource::Config(cfg) => Some(*cfg),
        TrackSource::Uri(url) => build_resource_config(&url, config),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ::kithara::{
        assets::{
            AssetLayout, AssetLayoutRegistry, AssetResource, AssetSource, AssetStoreBuilder,
            StorageBackend,
        },
        audio::{Waveform, analysis::BeatAnalysisConfig},
        bufpool::{BytePool, PcmPool},
        events::TrackId,
        file::File,
        prelude::{PlaybackResamplerBackend, ResourceConfig},
    };
    use kithara_platform::{
        CancelToken,
        sync::{Arc, Mutex},
        tokio::sync::watch,
    };
    use kithara_queue::{Queue, QueueConfig};
    use kithara_test_utils::kithara;

    use super::{AnalysisController, Plan, Run, pending_order, plan_analysis};
    use crate::{
        state::UiState,
        wave_cache::{AnalysisTarget, TrackAnalysisCache},
        waveform::TrackAnalysis,
    };

    fn one_bucket_wave() -> Waveform {
        // version 1 + one bucket of three 0.5 band heights (0.5 = 0x3F000000).
        Waveform::try_from([1, 0, 0, 0, 0, 0, 0, 63, 0, 0, 0, 63, 0, 0, 0, 63].as_slice())
            .expect("hand-built blob is valid")
    }

    fn analysis() -> TrackAnalysis {
        TrackAnalysis::new(None, Some(one_bucket_wave()), 0)
    }

    fn cache() -> TrackAnalysisCache {
        TrackAnalysisCache::new("test".to_string())
    }

    fn target(discriminator: &str) -> AnalysisTarget {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .build();
        let config = ResourceConfig::for_src("https://analysis.test.invalid/track.mp3")
            .expect("valid test source")
            .store(store)
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .discriminator(discriminator.to_string())
            .build();
        AnalysisTarget::for_config(&config).expect("test source has an analysis target")
    }

    fn state_with_current(ids: &[TrackId], current: usize) -> Mutex<UiState> {
        let queue = Queue::new(QueueConfig::new());
        for id in ids {
            queue.append_with_id(*id, format!("file:///tmp/track-{id}.mp3"));
        }
        let mut state = UiState::empty();
        state.tracks = queue.tracks();
        state.current_track_index = Some(current);
        Mutex::new(state)
    }

    fn controller_with_run(
        track_id: TrackId,
        target: AnalysisTarget,
        value: Option<TrackAnalysis>,
    ) -> (AnalysisController, watch::Sender<Option<TrackAnalysis>>) {
        let cancel = CancelToken::root();
        let mut controller = AnalysisController::new(
            &cancel,
            &BeatAnalysisConfig::<PlaybackResamplerBackend>::default(),
            PcmPool::default(),
        );
        let (tx, rx) = watch::channel(value);
        controller.current = Some(Run {
            track_id,
            rx,
            target: Some(target),
        });
        (controller, tx)
    }

    #[test]
    fn plan_skips_shown_track() {
        let a = target("root_a");
        let displayed = a.clone();
        let mut cache = cache();
        assert!(matches!(
            plan_analysis(Some(&a), Some(&displayed), &mut cache),
            Plan::Skip
        ));
    }

    #[test]
    fn plan_decodes_a_new_or_unkeyable_track() {
        let a = target("root_a");
        let b = target("root_b");
        let mut cache = cache();
        // A different shown track does not block a fresh decode.
        assert!(matches!(
            plan_analysis(Some(&a), Some(&b), &mut cache),
            Plan::Decode
        ));
        // An unkeyable source cannot be memoized.
        assert!(matches!(
            plan_analysis(None, None, &mut cache),
            Plan::Decode
        ));
    }

    #[test]
    fn plan_serves_a_cached_track_without_decoding() {
        let a = target("root_a");
        let mut cache = cache();
        cache.put(a.clone(), analysis());
        assert!(matches!(
            plan_analysis(Some(&a), None, &mut cache),
            Plan::Serve(_)
        ));
    }

    #[test]
    fn pending_puts_current_track_first_then_list_order() {
        let ids: Vec<TrackId> = [10u64, 11, 12].into_iter().map(TrackId::from).collect();

        let order: Vec<u64> = pending_order(&ids, Some(1))
            .into_iter()
            .map(u64::from)
            .collect();
        assert_eq!(order, vec![11, 10, 12], "current first, then list order");

        let order: Vec<u64> = pending_order(&ids, None)
            .into_iter()
            .map(u64::from)
            .collect();
        assert_eq!(order, vec![10, 11, 12], "no current: plain list order");
    }

    /// Drive `commit` with a run whose result channel already holds `value`,
    /// returning whether the run's key landed in the cache.
    fn commit_caches(value: Option<TrackAnalysis>) -> bool {
        let target = target("root");
        let (mut controller, tx) = controller_with_run(TrackId::allocate(), target.clone(), value);
        let state = Mutex::new(UiState::empty());
        controller.commit(&state);
        drop(tx);
        controller.cache.get(&target).is_some()
    }

    #[test]
    fn commit_caches_the_complete_result() {
        assert!(
            commit_caches(Some(analysis())),
            "a close carrying a value caches the complete analysis"
        );
    }

    #[test]
    fn commit_caches_nothing_when_the_run_failed() {
        assert!(
            !commit_caches(None),
            "a run that closes with no value (failure/cancel) caches nothing"
        );
    }

    #[kithara::test(native, tokio)]
    fn commit_publishes_current_track_and_marks_displayed() {
        let target = target("root_current");
        let analysis = analysis();
        let ids = [
            TrackId::allocate(),
            TrackId::allocate(),
            TrackId::allocate(),
        ];
        let (mut controller, tx) = controller_with_run(ids[1], target.clone(), Some(analysis));
        let state = state_with_current(&ids, 1);

        controller.commit(&state);

        let has_analysis = state.lock().analysis.is_some();
        assert!(has_analysis, "current run publishes to the UI");
        assert!(
            controller
                .displayed
                .as_ref()
                .is_some_and(|displayed| displayed.is_same(&target)),
            "displayed tracks the content key currently shown in the UI"
        );
        drop(tx);
    }

    #[kithara::test(native, tokio)]
    fn commit_caches_stale_track_without_publishing_or_marking_displayed() {
        let target = target("root_stale");
        let analysis = analysis();
        let ids = [
            TrackId::allocate(),
            TrackId::allocate(),
            TrackId::allocate(),
        ];
        let (mut controller, tx) = controller_with_run(ids[0], target.clone(), Some(analysis));
        let state = state_with_current(&ids, 1);

        controller.commit(&state);

        let has_analysis = state.lock().analysis.is_some();
        assert!(
            !has_analysis,
            "stale run must not replace the current track's analysis"
        );
        assert!(
            controller.cache.get(&target).is_some(),
            "stale run is still reusable by content key"
        );
        assert!(
            controller.displayed.is_none(),
            "stale cached analysis is not the analysis displayed by the UI"
        );
        drop(tx);
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

    #[kithara::test(native, tokio)]
    fn invalid_layout_for_current_track_clears_previous_analysis() {
        let layouts = AssetLayoutRegistry::default().with::<File>(Arc::new(InvalidLayout));
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .layouts(layouts)
            .build();
        let config = ResourceConfig::for_src("https://analysis.test.invalid/invalid.mp3")
            .expect("valid test source")
            .store(store)
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .build();
        let error = AnalysisTarget::for_config(&config).expect_err("layout must be rejected");
        let current = TrackId::allocate();
        let state = state_with_current(&[current], 0);
        state.lock().set_analysis(Some(analysis()));

        let cancel = CancelToken::root();
        let mut controller = AnalysisController::new(
            &cancel,
            &BeatAnalysisConfig::<PlaybackResamplerBackend>::default(),
            PcmPool::default(),
        );
        controller.displayed = Some(target("previous"));

        controller.reject_target(&state, current, &error);

        assert!(state.lock().analysis.is_none());
        assert!(controller.displayed.is_none());
    }
}
