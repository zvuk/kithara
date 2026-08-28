use std::{collections::VecDeque, num::NonZeroU32};

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
use tracing::warn;

use crate::{
    config::AppConfig,
    sources::build_resource_config,
    state::{UiState, apply_event},
    wave_cache::{AnalysisTarget, TrackAnalysisCache, token_for},
    waveform::{TrackAnalysis, TrackAnalysisRunner},
};

type AppBeatAnalysisConfig = BeatAnalysisConfig<PlaybackResamplerBackend>;
type AppResourceConfig = ResourceConfig<PlaybackResamplerBackend>;

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
    let mut driver = AnalysisController::new(
        &cancel,
        &config.beat_analysis,
        config.pcm_pool.clone(),
        config.waveform_max_buckets,
    );

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
                    apply_event(&event, &queue, &state);
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
    /// The rate axis this pass was opened on. A pass pins it, so a host rate
    /// change means the pass has to end rather than keep measuring in frames
    /// that would come to mean something else.
    axis: NonZeroU32,
    rx: watch::Receiver<Option<TrackAnalysis>>,
    /// Highest revision this run has published. Revisions are monotonic within
    /// one pass and start over in the next, so the guard belongs to the run
    /// rather than to what the UI happens to show.
    shown_revision: Option<u64>,
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
        waveform_max_buckets: usize,
    ) -> Self {
        let runner =
            TrackAnalysisRunner::new(cancel, waveform_max_buckets, beat_config.clone(), pcm_pool);
        Self {
            cache: TrackAnalysisCache::new(runner.fingerprint().clone()),
            runner,
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

    /// End a pass whose rate axis the engine has moved off, and put its track
    /// back at the front of the queue so the next pump opens a fresh pass on
    /// the new axis. Letting the old pass follow the device would leave one
    /// snapshot series whose frames mean two different things.
    fn retire_stale_axis(&mut self, queue: &Arc<Queue>) {
        let Some(run) = &self.current else {
            return;
        };
        let Some(rate) = NonZeroU32::new(queue.sample_rate()) else {
            return;
        };
        if run.axis == rate {
            return;
        }
        let track_id = run.track_id;
        warn!(
            from = run.axis.get(),
            to = rate.get(),
            "analysis: the host rate moved; the pass restarts on the new axis"
        );
        self.runner.clear();
        self.current = None;
        self.pending.retain(|pending| *pending != track_id);
        self.pending.push_front(track_id);
    }

    /// Publish the first part emit to the UI (no caching) when its
    /// track is still current; the beat overlay arrives on the closing commit.
    fn publish_intermediate(&mut self, state: &Mutex<UiState>) {
        let Some(run) = &mut self.current else {
            return;
        };

        let Some(analysis) = run.rx.borrow().clone() else {
            return;
        };
        if run
            .shown_revision
            .is_some_and(|shown| analysis.revision() <= shown)
        {
            return;
        }
        run.shown_revision = Some(analysis.revision());

        publish_if_current(state, run.track_id, analysis);
    }

    /// Start the next analysis worth running, if none is in flight: serve
    /// the current track from cache, skip background tracks that are cached
    /// or unkeyable, decode the first genuine miss.
    pub(crate) fn pump(&mut self, queue: &Arc<Queue>, state: &Mutex<UiState>, config: &AppConfig) {
        self.retire_stale_axis(queue);
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

            let mut served = false;
            let decode =
                match plan_analysis(target.as_ref(), self.displayed.as_ref(), &mut self.cache) {
                    Plan::Skip => false,
                    Plan::Serve { analysis, refill } => {
                        if is_current {
                            state.lock().set_analysis(Some(*analysis));
                            self.displayed = target.clone();
                            served = true;
                        }
                        refill
                    }
                    Plan::Decode => true,
                };
            if !decode {
                continue;
            }

            // An unkeyable source cannot be cached, so a background decode
            // would be thrown away; decode it only for display.
            if !is_current && target.is_none() {
                continue;
            }

            // A refill keeps what it just served on screen: the pass produces
            // the missing artifact, not a blank deck.
            if is_current && !served {
                state.lock().set_analysis(None);
                self.displayed = None;
            }

            let token = target.as_ref().map_or_else(
                || format!("track:{}", u64::from(track_id)).into(),
                |target| token_for(target.key()),
            );
            // The pass and its reader share the engine's axis, so a producer
            // feeding the same pass later measures its ranges in the same
            // frames. Without a rate there is no axis and so no pass.
            let Some(rate) = NonZeroU32::new(queue.sample_rate()) else {
                warn!("analysis: the engine reports no sample rate; pass not opened");
                continue;
            };
            // The handle waits where this track's load will find it. A track
            // already loaded keeps it waiting until it loads again, so a pass
            // opened mid-play warms nothing this time round.
            let queue = Arc::clone(queue);
            let rx = self.runner.analyze(cfg, token, rate, move |producer| {
                queue.set_analysis(track_id, producer);
            });
            self.current = Some(Run {
                target,
                axis: rate,
                rx,
                shown_revision: None,
                track_id,
            });
            return;
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

/// What [`AnalysisController::pump`] should do for a track.
enum Plan {
    /// Already shown for this content: leave the analysis as is.
    Skip,
    /// Cached (memory or disk): publish it. `refill` when the hit is missing
    /// an artifact the active configuration expects, which happens when one
    /// artifact's tag moved and the other's did not: the pass still has to run
    /// to produce what was dropped. Boxed because a snapshot dwarfs the other
    /// two variants.
    Serve {
        analysis: Box<TrackAnalysis>,
        refill: bool,
    },
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

    let Some(analysis) = cache.get(target) else {
        return Plan::Decode;
    };
    Plan::Serve {
        refill: !cache.is_sufficient(&analysis),
        analysis: Box::new(analysis),
    }
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
    use std::num::NonZeroU32;

    use ::kithara::{
        assets::{
            AssetLayout, AssetLayoutRegistry, AssetResource, AssetSource, AssetStore,
            StorageBackend,
        },
        audio::{
            Coverage, FrameRange, Waveform,
            analysis::{AnalysisFingerprint, BeatAnalysisConfig},
        },
        bufpool::{BytePool, PcmPool},
        events::TrackId,
        file::File,
        net::{HttpClient, NetOptions},
        prelude::{PlaybackResamplerBackend, ResourceConfig},
        stream::dl::{Downloader, DownloaderConfig},
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

    /// Bucket cap the controller tests run with; not a default.
    const MAX_BUCKETS: usize = 1_024;

    fn one_bucket_wave() -> Waveform {
        // version 1 + one bucket of three 0.5 band heights (0.5 = 0x3F000000).
        Waveform::try_from([1, 0, 0, 0, 0, 0, 0, 63, 0, 0, 0, 63, 0, 0, 0, 63].as_slice())
            .expect("hand-built blob is valid")
    }

    fn analysis() -> TrackAnalysis {
        let mut coverage = Coverage::default();
        coverage.insert(FrameRange::new(0, 1_000));
        TrackAnalysis::builder()
            .token("test-track".into())
            .revision(1)
            .source_sample_rate(NonZeroU32::new(44_100).expect("fixture rate is non-zero"))
            .extent(1_000)
            .settled(true)
            .coverage(coverage)
            .waveform(one_bucket_wave())
            .build()
    }

    fn revision_of(revision: u64) -> TrackAnalysis {
        let mut coverage = Coverage::default();
        coverage.insert(FrameRange::new(0, 1_000));
        TrackAnalysis::builder()
            .token("test-track".into())
            .revision(revision)
            .source_sample_rate(NonZeroU32::new(44_100).expect("fixture rate is non-zero"))
            .extent(1_000)
            .settled(true)
            .coverage(coverage)
            .waveform(one_bucket_wave())
            .build()
    }

    fn shown_revision(state: &Mutex<UiState>) -> Option<u64> {
        state.lock().analysis.as_ref().map(TrackAnalysis::revision)
    }

    fn cache() -> TrackAnalysisCache {
        TrackAnalysisCache::new(AnalysisFingerprint::default())
    }

    fn target(discriminator: &str) -> AnalysisTarget {
        let store = AssetStore::builder()
            .backend(StorageBackend::Memory)
            .build();
        let config = ResourceConfig::for_src(
            ResourceConfig::parse_src("https://analysis.test.invalid/track.mp3")
                .expect("valid test source"),
        )
        .store(store)
        .byte_pool(BytePool::default())
        .pcm_pool(PcmPool::default())
        .discriminator(discriminator)
        .build();
        AnalysisTarget::for_config(&config).expect("test source has an analysis target")
    }

    fn state_with_current(ids: &[TrackId], current: usize) -> Mutex<UiState> {
        let queue = Queue::new(QueueConfig::default());
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
            MAX_BUCKETS,
        );
        let (tx, rx) = watch::channel(value);
        controller.current = Some(Run {
            shown_revision: None,
            axis: NonZeroU32::new(44_100).expect("fixture rate is non-zero"),
            track_id,
            rx,
            target: Some(target),
        });
        (controller, tx)
    }

    #[kithara::test(native, flash(false))]
    fn plan_skips_shown_track() {
        let a = target("root_a");
        let displayed = a.clone();
        let mut cache = cache();
        assert!(matches!(
            plan_analysis(Some(&a), Some(&displayed), &mut cache),
            Plan::Skip
        ));
    }

    #[kithara::test(native, flash(false))]
    fn plan_decodes_a_new_or_unkeyable_track() {
        let a = target("root_a");
        let b = target("root_b");
        let mut cache = cache();
        assert!(matches!(
            plan_analysis(Some(&a), Some(&b), &mut cache),
            Plan::Decode
        ));
        assert!(matches!(
            plan_analysis(None, None, &mut cache),
            Plan::Decode
        ));
    }

    #[kithara::test(native, flash(false))]
    fn plan_serves_a_cached_track_without_decoding() {
        let a = target("root_a");
        let mut cache = cache();
        cache.put(a.clone(), analysis());
        assert!(matches!(
            plan_analysis(Some(&a), None, &mut cache),
            Plan::Serve { .. }
        ));
    }

    #[kithara::test(native, flash(false))]
    fn plan_refills_a_hit_that_lost_an_artifact() {
        let a = target("root_refill");
        let stored = TrackAnalysisCache::new(AnalysisFingerprint::new(None, Some("wave:v1")));
        let mut stored = stored;
        stored.put(a.clone(), analysis());

        let mut current = TrackAnalysisCache::new(AnalysisFingerprint::new(None, Some("wave:v2")));
        assert!(
            matches!(
                plan_analysis(Some(&a), None, &mut current),
                Plan::Decode | Plan::Serve { refill: true, .. }
            ),
            "a hit missing an expected artifact must still reach the decoder"
        );
    }

    #[kithara::test(native, tokio)]
    fn a_stale_revision_from_the_run_is_not_published() {
        let ids = [TrackId::from(1u64)];
        let state = state_with_current(&ids, 0);
        let track_id = TrackId::from(1u64);
        let (mut controller, tx) = controller_with_run(track_id, target("root_rev"), None);

        tx.send(Some(revision_of(5))).expect("receiver is alive");
        controller.publish_intermediate(&state);
        assert_eq!(shown_revision(&state), Some(5));

        tx.send(Some(revision_of(3))).expect("receiver is alive");
        controller.publish_intermediate(&state);
        assert_eq!(
            shown_revision(&state),
            Some(5),
            "an older revision of the same pass must not replace it"
        );

        tx.send(Some(revision_of(6))).expect("receiver is alive");
        controller.publish_intermediate(&state);
        assert_eq!(shown_revision(&state), Some(6), "a newer one wins");
    }

    fn app_config(cancel: &CancelToken) -> crate::config::AppConfig {
        crate::config::AppConfig::builder()
            .downloader(Downloader::new(
                DownloaderConfig::for_client(HttpClient::new(
                    NetOptions::builder().build(),
                    cancel.child(),
                ))
                .build(),
            ))
            .shutdown(cancel.child())
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .store(
                AssetStore::builder()
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build()
    }

    #[kithara::test(native, tokio)]
    async fn a_pass_is_opened_on_the_engine_axis() {
        let queue = Arc::new(Queue::new(QueueConfig::default()));
        queue.append_with_id(TrackId::from(1), "file:///tmp/track-1.mp3".to_owned());
        let state = state_with_current(&[TrackId::from(1)], 0);
        let cancel = CancelToken::root();
        let config = app_config(&cancel);
        let mut controller = AnalysisController::new(
            &cancel,
            &BeatAnalysisConfig::<PlaybackResamplerBackend>::default(),
            PcmPool::default(),
            MAX_BUCKETS,
        );

        controller.on_tracks_changed(&queue, &state, &config);

        let run = controller.current.as_ref().expect("a pass is opened");
        assert_eq!(
            run.axis.get(),
            queue.sample_rate(),
            "the pass is measured on the axis the engine plays at, not the source's native one"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_pass_ends_when_the_engine_leaves_its_axis() {
        let queue = Arc::new(Queue::new(QueueConfig::default()));
        let (mut controller, _tx) = controller_with_run(TrackId::from(1), target("root_a"), None);

        // The fixture run is pinned to 44.1 kHz.
        let engine = queue.sample_rate();
        if engine == 44_100 {
            controller.current = controller.current.take().map(|run| Run {
                axis: NonZeroU32::new(48_000).expect("test rate is non-zero"),
                ..run
            });
        }

        controller.retire_stale_axis(&queue);

        assert!(
            controller.current.is_none(),
            "a pass whose axis the engine left does not keep publishing on it"
        );
        assert_eq!(
            controller.pending.front(),
            Some(&TrackId::from(1)),
            "its track goes back to the front for a reopen"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_pass_on_the_engine_axis_is_left_alone() {
        let queue = Arc::new(Queue::new(QueueConfig::default()));
        let (mut controller, _tx) = controller_with_run(TrackId::from(1), target("root_a"), None);
        controller.current = controller.current.take().map(|run| Run {
            axis: NonZeroU32::new(queue.sample_rate()).expect("engine rate is non-zero"),
            ..run
        });

        controller.retire_stale_axis(&queue);

        assert!(
            controller.current.is_some(),
            "a pass still on the engine's axis keeps running"
        );
    }

    #[kithara::test(native, flash(false))]
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

    /// Commits a run holding `value`; true when its key landed in the cache.
    fn commit_caches(value: Option<TrackAnalysis>) -> bool {
        let target = target("root");
        let (mut controller, tx) = controller_with_run(TrackId::allocate(), target.clone(), value);
        let state = Mutex::new(UiState::empty());
        controller.commit(&state);
        drop(tx);
        controller.cache.get(&target).is_some()
    }

    #[kithara::test(native, flash(false))]
    fn commit_caches_the_complete_result() {
        assert!(
            commit_caches(Some(analysis())),
            "a close carrying a value caches the complete analysis"
        );
    }

    #[kithara::test(native, flash(false))]
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
        fn path(&self, _resource: &AssetResource) -> String {
            "../escape".to_string()
        }

        fn root(&self, _source: &AssetSource) -> String {
            "root".to_string()
        }
    }

    #[kithara::test(native, tokio)]
    fn invalid_layout_for_current_track_clears_previous_analysis() {
        let layouts = AssetLayoutRegistry::default().with::<File>(Arc::new(InvalidLayout));
        let store = AssetStore::builder()
            .backend(StorageBackend::Memory)
            .layouts(layouts)
            .build();
        let config = ResourceConfig::for_src(
            ResourceConfig::parse_src("https://analysis.test.invalid/invalid.mp3")
                .expect("valid test source"),
        )
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
            MAX_BUCKETS,
        );
        controller.displayed = Some(target("previous"));

        controller.reject_target(&state, current, &error);

        assert!(state.lock().analysis.is_none());
        assert!(controller.displayed.is_none());
    }
}
