use std::{collections::VecDeque, num::NonZeroU32};

use kithara::{
    analysis::AnalysisProgress,
    decode::DecodeError,
    events::{EngineEvent, Envelope, Event, EventReceiver, SessionEvent, TrackId},
    platform::{
        CancelToken,
        sync::{Arc, Mutex},
        tokio::{
            self,
            sync::{broadcast::error::RecvError, watch},
            task::JoinHandle,
        },
    },
    queue::QueueEvent,
};
use tracing::warn;

use crate::{
    config::AppConfig,
    pools::{AppQueueControl, AppResourceConfig, AppTrackSource},
    sources::build_resource_config,
    state::{UiState, apply_event},
    wave_cache::{
        AnalysisPersistence, AnalysisPersistenceError, AnalysisTarget, TrackAnalysisCache,
        token_for,
    },
    waveform::{TrackAnalysis, TrackAnalysisRunner},
};

/// Analysis-aware state listener: mirrors queue events into [`UiState`] and
/// drives the background [`AnalysisController`]. Starts analysing the already
/// loaded library immediately — independent of which UI is open.
pub(crate) async fn listen(
    queue: AppQueueControl,
    state: Arc<Mutex<UiState>>,
    config: AppConfig,
    cancel: CancelToken,
    mut rx: EventReceiver,
    persistence: AnalysisPersistence,
) {
    let mut driver = AnalysisController::new(&cancel, &config, Some(persistence));

    // Analyse whatever is already loaded; later tracks arrive as events.
    driver.on_tracks_changed(&queue, &state, &config);

    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            () = driver.drive(&queue, &state, &config) => {}
            event = rx.recv() => match event {
                Ok(Envelope { event, .. }) => {
                    handle_event(&mut driver, &event, &queue, &state, &config);
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
        }
    }
}

fn handle_event(
    driver: &mut AnalysisController,
    event: &Event,
    queue: &AppQueueControl,
    state: &Mutex<UiState>,
    config: &AppConfig,
) {
    let track_changed = matches!(event, Event::Queue(QueueEvent::CurrentTrackChanged { .. }));
    let tracks_changed = matches!(
        event,
        Event::Queue(QueueEvent::TrackAdded { .. } | QueueEvent::TrackRemoved { .. })
    );
    let output_axis_changed = matches!(
        event,
        Event::Engine(EngineEvent::Started) | Event::Session(SessionEvent::RouteChanged { .. })
    );

    apply_event(event, queue, state);
    if tracks_changed {
        driver.on_tracks_changed(queue, state, config);
    }
    if track_changed {
        driver.on_track_changed(queue, state, config);
    }
    if output_axis_changed {
        driver.pump(queue, state, config);
    }
}

/// Background source-analysis controller owned by the state listener task.
/// Results land in the two-tier [`TrackAnalysisCache`];
pub(crate) struct AnalysisController {
    activity: Option<Activity>,
    cache: TrackAnalysisCache,
    persistence: Option<AnalysisPersistence>,
    runner: TrackAnalysisRunner,
    /// Tracks waiting for background analysis, current track first.
    pending: VecDeque<TrackId>,
}

enum Activity {
    Running(Run),
    Committing(Commit),
}

/// An in-flight analysis: the track it is for (stale-guard), its content cache
/// key (`None` for an unkeyable source), and its result channel.
struct Run {
    target: Option<AnalysisTarget>,
    /// The rate axis this pass was opened on. A pass pins it, so a host rate
    /// change means the pass has to end rather than keep measuring in frames
    /// that would come to mean something else.
    axis: NonZeroU32,
    rx: watch::Receiver<Option<AnalysisProgress>>,
    /// Highest revision this run has published. Revisions are monotonic within
    /// one pass and start over in the next, so the guard belongs to the run
    /// rather than to what the UI happens to show.
    shown_revision: Option<u64>,
    track_id: TrackId,
}

struct Commit {
    task: JoinHandle<Result<(), AnalysisPersistenceError>>,
}

impl AnalysisController {
    /// `cancel` must be a child of the app master so analysis stops on app
    /// shutdown. Each source config supplies the store used for its durable
    /// analysis resource.
    pub(crate) fn new(
        cancel: &CancelToken,
        config: &AppConfig,
        persistence: Option<AnalysisPersistence>,
    ) -> Self {
        let runner = TrackAnalysisRunner::new(
            cancel,
            config.base_worker.clone(),
            config.analysis_chunk_seconds,
            config.waveform_max_buckets,
            config.beat_analysis.clone(),
            config.worker.pools().clone(),
        );
        Self {
            cache: TrackAnalysisCache::new(
                runner.fingerprint().clone(),
                config.worker.pools(),
                config.analysis_chunk_seconds,
            ),
            persistence,
            runner,
            activity: None,
            pending: VecDeque::new(),
        }
    }

    /// Persist a closed run before allowing the next analysis to start.
    fn finish_run(&mut self, state: &Mutex<UiState>) {
        let Some(Activity::Running(run)) = self.activity.take() else {
            return;
        };
        let Some(progress) = run.rx.borrow().clone() else {
            return;
        };

        if let Some(target) = &run.target {
            self.cache.put(target.clone(), progress.clone());
        }
        publish_if_current(state, run.track_id, progress.analysis().clone());

        let (Some(target), Some(persistence)) = (run.target, self.persistence.clone()) else {
            return;
        };
        let task = tokio::task::spawn(async move { persistence.store(target, progress).await });
        self.activity = Some(Activity::Committing(Commit { task }));
    }

    /// Await the run's next event and handle it: publish the staged
    /// intermediate, or commit and pump on close. Parks when no run is active.
    pub(crate) async fn drive(
        &mut self,
        queue: &AppQueueControl,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        match &mut self.activity {
            Some(Activity::Running(run)) => {
                let closed = run.rx.changed().await.is_err();
                if closed {
                    self.finish_run(state);
                    self.pump(queue, state, config);
                } else {
                    self.publish_intermediate(state);
                }
            }
            Some(Activity::Committing(commit)) => {
                match (&mut commit.task).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => warn!(%error, "analysis: final checkpoint commit failed"),
                    Err(error) => warn!(%error, "analysis: final checkpoint task failed"),
                }
                self.activity = None;
                self.pump(queue, state, config);
            }
            None => std::future::pending::<()>().await,
        }
    }

    /// The current track changed: put it at the front of the queue and
    /// preempt an in-flight background run so the visible deck wins.
    pub(crate) fn on_track_changed(
        &mut self,
        queue: &AppQueueControl,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        if let Some(id) = current_track_id(state) {
            self.pending.retain(|t| *t != id);
            self.pending.push_front(id);
            if let Some(Activity::Running(run)) = &self.activity
                && run.track_id != id
            {
                let preempted = run.track_id;
                self.runner.clear();
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
        queue: &AppQueueControl,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        {
            let st = state.lock();
            let ids: Vec<TrackId> = st.tracks.iter().map(|entry| entry.id).collect();
            self.pending = pending_order(&ids, st.current_track_index);
        }
        if let Some(Activity::Running(run)) = &self.activity {
            self.pending.retain(|t| *t != run.track_id);
        }
        self.pump(queue, state, config);
    }

    /// End a pass on a stale rate axis and schedule a fresh pass without
    /// overriding a newer queue decision.
    fn retire_stale_axis(&mut self, queue: &AppQueueControl, state: &Mutex<UiState>) {
        let Some(Activity::Running(run)) = &self.activity else {
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
        if !self.pending.contains(&track_id) {
            if current_track_id(state) == Some(track_id) {
                self.pending.push_front(track_id);
            } else {
                self.pending.push_back(track_id);
            }
        }
    }

    /// Publish and best-effort checkpoint a newer intermediate revision.
    fn publish_intermediate(&mut self, state: &Mutex<UiState>) {
        let Some(Activity::Running(run)) = &mut self.activity else {
            return;
        };

        let Some(progress) = run.rx.borrow().clone() else {
            return;
        };
        let analysis = progress.analysis();
        if run
            .shown_revision
            .is_some_and(|shown| analysis.revision() <= shown)
        {
            return;
        }
        run.shown_revision = Some(analysis.revision());
        let target = run.target.clone();
        let track_id = run.track_id;

        if let Some(target) = target {
            self.cache.put(target.clone(), progress.clone());
            if let Some(persistence) = &self.persistence {
                let _ = persistence.try_store(target, progress.clone());
            }
        }
        publish_if_current(state, track_id, progress.into());
    }

    /// Start the next analysis worth running, if none is in flight: serve
    /// the current track from cache, skip background tracks that are cached
    /// or unkeyable, decode the first genuine miss.
    pub(crate) fn pump(
        &mut self,
        queue: &AppQueueControl,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) {
        self.retire_stale_axis(queue, state);
        if self.activity.is_some() {
            return;
        }

        // No analyzers found: decoding would produce nothing.
        if !self.runner.is_active() {
            self.pending.clear();
            return;
        }

        while let Some(track_id) = self.pending.pop_front() {
            if let Some(run) = self.open_run(track_id, queue, state, config) {
                self.activity = Some(Activity::Running(run));
                return;
            }
        }
    }

    fn open_run(
        &mut self,
        track_id: TrackId,
        queue: &AppQueueControl,
        state: &Mutex<UiState>,
        config: &AppConfig,
    ) -> Option<Run> {
        let source = queue.track_source(track_id)?;
        let cfg = resource_config_from_source(source, config)?;
        let target = match AnalysisTarget::for_config(&cfg) {
            Ok(target) => Some(target),
            Err(error) => {
                Self::reject_target(state, track_id, &error);
                return None;
            }
        };
        let is_current = current_track_id(state) == Some(track_id);
        let Some(rate) = NonZeroU32::new(queue.sample_rate()) else {
            warn!("analysis: the engine reports no sample rate; pass not opened");
            return None;
        };

        let mut served = false;
        let mut resume = None;
        let decode = match plan_analysis(target.as_ref(), &mut self.cache, rate) {
            Plan::Serve { progress, refill } => {
                if is_current {
                    state.lock().set_analysis(Some(progress.analysis().clone()));
                    served = true;
                }
                if refill && progress.is_resumable() {
                    resume = Some(*progress);
                }
                refill
            }
            Plan::Decode => true,
        };
        if !decode {
            return None;
        }

        // An unkeyable source cannot be cached, so a background decode
        // would be thrown away; decode it only for display.
        if !is_current && target.is_none() {
            return None;
        }

        // A refill keeps what it just served on screen: the pass produces
        // the missing artifact, not a blank deck.
        if is_current && !served {
            state.lock().set_analysis(None);
        }

        // The handle waits where this track's load will find it. A track
        // already loaded keeps it waiting until it loads again, so a pass
        // opened mid-play warms nothing this time round.
        let queue = queue.clone();
        let rx = if let Some(progress) = resume {
            match self.runner.resume(cfg, progress, move |producer| {
                queue.attach_observer(track_id, producer);
            }) {
                Ok(rx) => rx,
                Err(error) => {
                    warn!(%error, ?track_id, "analysis: cached checkpoint rejected");
                    return None;
                }
            }
        } else {
            let token = target.as_ref().map_or_else(
                || format!("track:{}", u64::from(track_id)).into(),
                |target| token_for(target.key()),
            );
            self.runner.analyze(cfg, token, rate, move |producer| {
                queue.attach_observer(track_id, producer);
            })
        };
        Some(Run {
            target,
            axis: rate,
            rx,
            shown_revision: None,
            track_id,
        })
    }

    fn reject_target(state: &Mutex<UiState>, track_id: TrackId, error: &DecodeError) {
        tracing::warn!(
            %error,
            ?track_id,
            "analysis layout rejected the derived resource key"
        );
        let mut state = state.lock();
        if current_track_id_in(&state) == Some(track_id) {
            state.set_analysis(None);
        }
    }
}

/// What [`AnalysisController::pump`] should do for a track.
enum Plan {
    /// Cached (memory or disk): publish it. `refill` when the hit is missing
    /// an artifact the active configuration expects, which happens when one
    /// artifact's tag moved and the other's did not: the pass still has to run
    /// to produce what was dropped. Boxed because a snapshot dwarfs the other
    /// two variants.
    Serve {
        progress: Box<AnalysisProgress>,
        refill: bool,
    },
    /// Not cached (or an unkeyable source): analyse.
    Decode,
}

/// Decide the action for a track from its durable identity and source axis.
/// An in-flight run needs no guard here: `pump` returns before planning while
/// one is active.
fn plan_analysis(
    target: Option<&AnalysisTarget>,
    cache: &mut TrackAnalysisCache,
    source_sample_rate: NonZeroU32,
) -> Plan {
    let Some(target) = target else {
        // No stable key (the reserved non-exhaustive source seam): cannot
        return Plan::Decode;
    };

    let Some(progress) = cache.get(target, source_sample_rate) else {
        return Plan::Decode;
    };
    Plan::Serve {
        refill: !cache.is_sufficient(&progress) || progress.is_resumable(),
        progress: Box::new(progress),
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
    source: AppTrackSource,
    config: &AppConfig,
) -> Option<AppResourceConfig> {
    match source {
        AppTrackSource::Config(cfg) => Some(*cfg),
        AppTrackSource::Uri(url) => build_resource_config(&url, config),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroUsize};

    use ::kithara::{
        analysis::{
            AnalysisFile, AnalysisFingerprint, AnalysisProgress, Coverage, FrameRange, Waveform,
        },
        assets::{
            AssetLayout, AssetLayoutRegistry, AssetResource, AssetSource, ReadSide, StorageBackend,
        },
        events::{EngineEvent, Event, RouteChangeReason, RouteDescription, SessionEvent, TrackId},
        file::File,
        host::HostConfig,
        net::{HttpClient, NetOptions},
        platform::{
            CancelToken,
            sync::{Arc, Mutex},
            time::Duration,
            tokio::{runtime::Handle, sync::watch},
        },
        play::{PlayWorkerConfig, PlayerConfig, PlayerImpl, policy::DomainKeyPolicy},
        prelude::ResourceSrc,
        queue::QueueConfig,
        stream::dl::{Downloader, DownloaderConfig},
        worker::{DispatcherConfig, TaskConfig, Worker, WorkerConfig},
    };
    use kithara_test_utils::kithara;

    use super::{
        Activity, AnalysisController, Plan, Run, handle_event, pending_order, plan_analysis,
        resource_config_from_source,
    };
    use crate::{
        pools::{
            self, AppHost, AppPools, AppQueue, AppQueueControl, AppResourceConfig, AppStore,
            AppWorker, Pools,
        },
        state::UiState,
        wave_cache::{
            AnalysisPersistence, AnalysisTarget, TrackAnalysisCache,
            persistence::AnalysisPersistenceConfig,
        },
        waveform::TrackAnalysis,
    };

    fn chunk_seconds() -> NonZeroU32 {
        NonZeroU32::new(16).expect("fixture chunk duration is non-zero")
    }

    fn test_pools() -> Pools {
        pools::build(&pools::PoolsSection::default()).expect("valid app pool policy")
    }

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(44_100).expect("fixture rate is non-zero")
    }

    fn fingerprint() -> AnalysisFingerprint {
        AnalysisFingerprint::new(None, Some("wave:test:v1"))
    }

    fn progress(analysis: TrackAnalysis) -> AnalysisProgress {
        AnalysisProgress::try_from(analysis).expect("settled fixture is valid progress")
    }

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
            .source_sample_rate(sample_rate())
            .extent(1_000)
            .settled(true)
            .coverage(coverage)
            .fingerprint(fingerprint())
            .waveform(one_bucket_wave())
            .build()
    }

    fn revision_of(revision: u64) -> TrackAnalysis {
        let mut coverage = Coverage::default();
        coverage.insert(FrameRange::new(0, 1_000));
        TrackAnalysis::builder()
            .token("test-track".into())
            .revision(revision)
            .source_sample_rate(sample_rate())
            .extent(1_000)
            .settled(true)
            .coverage(coverage)
            .fingerprint(fingerprint())
            .waveform(one_bucket_wave())
            .build()
    }

    fn shown_revision(state: &Mutex<UiState>) -> Option<u64> {
        state.lock().analysis.as_ref().map(TrackAnalysis::revision)
    }

    fn cache() -> TrackAnalysisCache {
        TrackAnalysisCache::new(fingerprint(), &test_pools(), chunk_seconds())
    }

    fn target(discriminator: &str) -> AnalysisTarget {
        let store = AppStore::builder(test_pools())
            .backend(StorageBackend::Memory)
            .build();
        target_in(&store, discriminator)
    }

    fn target_in(store: &AppStore, discriminator: &str) -> AnalysisTarget {
        let config = AppResourceConfig::for_src(
            ResourceSrc::parse("https://analysis.test.invalid/track.mp3")
                .expect("valid test source"),
        )
        .store(store.clone())
        .discriminator(discriminator)
        .build();
        AnalysisTarget::for_config(&config).expect("test source has an analysis target")
    }

    fn state_with_current(ids: &[TrackId], current: usize) -> Mutex<UiState> {
        let (_host, queue) = queue();
        for id in ids {
            queue
                .append_with_id(*id, format!("file:///tmp/track-{id}.mp3"))
                .expect("append test track");
        }
        let mut state = UiState::empty();
        state.tracks = queue.tracks();
        state.current_track_index = Some(current);
        Mutex::new(state)
    }

    fn queue() -> (AppHost, AppQueueControl) {
        let worker = AppWorker::new(PlayWorkerConfig::builder(test_pools()).build());
        let mut host = AppHost::new(HostConfig::builder().build()).expect("test host");
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .worker(worker)
                .sample_rate(host.requested_sample_rate())
                .build(),
        );
        let queue = AppQueue::new(QueueConfig::builder().player(player).build());
        let queue = host.insert(queue).expect("host accepts queue");
        let control = queue.control().clone();
        (host, control)
    }

    fn controller_with_run(
        track_id: TrackId,
        target: AnalysisTarget,
        value: Option<TrackAnalysis>,
    ) -> (AnalysisController, watch::Sender<Option<AnalysisProgress>>) {
        controller_with_run_and_persistence(track_id, target, value, None)
    }

    fn controller_with_run_and_persistence(
        track_id: TrackId,
        target: AnalysisTarget,
        value: Option<TrackAnalysis>,
        persistence: Option<AnalysisPersistence>,
    ) -> (AnalysisController, watch::Sender<Option<AnalysisProgress>>) {
        let cancel = CancelToken::root();
        let config = app_config(&cancel);
        let mut controller = AnalysisController::new(&cancel, &config, persistence);
        let (tx, rx) = watch::channel(value.map(progress));
        controller.activity = Some(Activity::Running(Run {
            shown_revision: None,
            axis: sample_rate(),
            track_id,
            rx,
            target: Some(target),
        }));
        (controller, tx)
    }

    fn persistence(cancel: &CancelToken, pools: Pools) -> AnalysisPersistence {
        let worker = Worker::new(
            WorkerConfig::new()
                .with_cancel(cancel.child())
                .with_runtime(Handle::current()),
        );
        AnalysisPersistence::new(AnalysisPersistenceConfig::new(
            worker,
            pools,
            NonZeroUsize::MIN,
            Duration::from_secs(u64::from(chunk_seconds().get())),
            DispatcherConfig::builder()
                .name("analysis-persistence-controller-test")
                .build(),
            TaskConfig::new(),
        ))
        .expect("persistence fixture starts")
    }

    #[kithara::test(native, flash(false))]
    fn plan_decodes_a_new_or_unkeyable_track() {
        let a = target("root_a");
        let mut cache = cache();
        assert!(matches!(
            plan_analysis(Some(&a), &mut cache, sample_rate()),
            Plan::Decode
        ));
        assert!(matches!(
            plan_analysis(None, &mut cache, sample_rate()),
            Plan::Decode
        ));
    }

    #[kithara::test(native, flash(false))]
    fn plan_serves_a_cached_track_without_decoding() {
        let a = target("root_a");
        let mut cache = cache();
        cache.put(a.clone(), progress(analysis()));
        assert!(matches!(
            plan_analysis(Some(&a), &mut cache, sample_rate()),
            Plan::Serve { .. }
        ));
    }

    #[kithara::test(native, tokio)]
    async fn pump_serves_an_incomplete_hit_while_refilling_it() {
        let track_id = TrackId::from(1);
        let (_host, queue) = queue();
        queue
            .append_with_id(track_id, "file:///tmp/track-1.mp3".to_owned())
            .expect("append test track");
        let state = state_with_current(&[track_id], 0);
        let cancel = CancelToken::root();
        let config = app_config(&cancel);
        let mut controller = AnalysisController::new(&cancel, &config, None);
        let fingerprint = controller.runner.fingerprint().clone();
        assert!(
            fingerprint.beat().is_some(),
            "fixture needs an active artifact to omit"
        );
        let rate = NonZeroU32::new(queue.sample_rate()).expect("engine rate is non-zero");
        let source = queue.track_source(track_id).expect("track has a source");
        let resource = resource_config_from_source(source, &config).expect("source is cacheable");
        let target = AnalysisTarget::for_config(&resource).expect("source has a cache target");
        let mut coverage = Coverage::default();
        coverage.insert(FrameRange::new(0, 1_000));
        let cached = TrackAnalysis::builder()
            .token("cached-track".into())
            .revision(7)
            .source_sample_rate(rate)
            .extent(1_000)
            .settled(true)
            .coverage(coverage)
            .fingerprint(fingerprint)
            .waveform(one_bucket_wave())
            .build();
        controller.cache.put(target, progress(cached));
        controller.pending.push_back(track_id);

        controller.pump(&queue, &state, &config);

        assert_eq!(shown_revision(&state), Some(7), "the cache hit is shown");
        assert!(
            matches!(
                controller.activity.as_ref(),
                Some(Activity::Running(Run { track_id: running, .. })) if *running == track_id
            ),
            "the missing artifact is refilled for the same track"
        );
        cancel.cancel();
    }

    #[kithara::test(native, tokio)]
    fn a_stale_revision_from_the_run_is_not_published() {
        let ids = [TrackId::from(1u64)];
        let state = state_with_current(&ids, 0);
        let track_id = TrackId::from(1u64);
        let (mut controller, tx) = controller_with_run(track_id, target("root_rev"), None);

        tx.send(Some(progress(revision_of(5))))
            .expect("receiver is alive");
        controller.publish_intermediate(&state);
        assert_eq!(shown_revision(&state), Some(5));

        tx.send(Some(progress(revision_of(3))))
            .expect("receiver is alive");
        controller.publish_intermediate(&state);
        assert_eq!(
            shown_revision(&state),
            Some(5),
            "an older revision of the same pass must not replace it"
        );

        tx.send(Some(progress(revision_of(6))))
            .expect("receiver is alive");
        controller.publish_intermediate(&state);
        assert_eq!(shown_revision(&state), Some(6), "a newer one wins");
    }

    fn app_config(cancel: &CancelToken) -> crate::config::AppConfig {
        let pools = test_pools();
        let worker = AppWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
        crate::config::AppConfig::builder()
            .drm(crate::config::AppDrm::new(DomainKeyPolicy::new(Vec::new())))
            .downloader(Downloader::new(
                DownloaderConfig::for_client(HttpClient::new(
                    NetOptions::builder().build(),
                    pools.clone(),
                    cancel.child(),
                ))
                .build(),
            ))
            .shutdown(cancel.child())
            .worker(worker)
            .store(
                AppStore::builder(pools)
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build()
    }

    #[kithara::test(native, tokio)]
    async fn a_pass_is_opened_on_the_engine_axis() {
        let (_host, queue) = queue();
        queue
            .append_with_id(TrackId::from(1), "file:///tmp/track-1.mp3".to_owned())
            .expect("append test track");
        let state = state_with_current(&[TrackId::from(1)], 0);
        let cancel = CancelToken::root();
        let config = app_config(&cancel);
        let mut controller = AnalysisController::new(&cancel, &config, None);

        controller.on_tracks_changed(&queue, &state, &config);

        let Some(Activity::Running(run)) = controller.activity.as_ref() else {
            panic!("a pass is opened");
        };
        assert_eq!(
            run.axis.get(),
            queue.sample_rate(),
            "the pass is measured on the axis the engine plays at, not the source's native one"
        );
    }

    #[kithara::test(native, tokio, flash(false))]
    fn a_stale_axis_waits_for_its_final_checkpoint_before_reopening() {
        let (_host, queue) = queue();
        let track_id = TrackId::from(1);
        let state = state_with_current(&[track_id], 0);
        let (mut controller, _tx) = controller_with_run(track_id, target("root_a"), None);

        // The fixture run is pinned to 44.1 kHz.
        let engine = queue.sample_rate();
        if engine == 44_100 {
            let Some(Activity::Running(run)) = controller.activity.as_mut() else {
                panic!("fixture run is active");
            };
            run.axis = NonZeroU32::new(48_000).expect("test rate is non-zero");
        }

        controller.retire_stale_axis(&queue, &state);

        assert!(
            matches!(controller.activity, Some(Activity::Running(_))),
            "the receiver stays owned until the cancelled pass publishes its final checkpoint"
        );
        assert_eq!(
            controller.pending.front(),
            Some(&TrackId::from(1)),
            "its track goes back to the front for a reopen"
        );
    }

    #[kithara::test(native, tokio, flash(false))]
    fn a_pass_on_the_engine_axis_is_left_alone() {
        let (_host, queue) = queue();
        let track_id = TrackId::from(1);
        let state = state_with_current(&[track_id], 0);
        let (mut controller, _tx) = controller_with_run(track_id, target("root_a"), None);
        let Some(Activity::Running(run)) = controller.activity.as_mut() else {
            panic!("fixture run is active");
        };
        run.axis = NonZeroU32::new(queue.sample_rate()).expect("engine rate is non-zero");

        controller.retire_stale_axis(&queue, &state);

        assert!(
            matches!(controller.activity, Some(Activity::Running(_))),
            "a pass still on the engine's axis keeps running"
        );
    }

    async fn assert_axis_event_restarts(event: Event) {
        let track_id = TrackId::from(1);
        let (_host, queue) = queue();
        queue
            .append_with_id(track_id, "file:///tmp/track-1.mp3".to_owned())
            .expect("append test track");
        let state = state_with_current(&[track_id], 0);
        let cancel = CancelToken::root();
        let config = app_config(&cancel);
        let (mut controller, tx) = controller_with_run(track_id, target("stale_axis"), None);
        let engine_axis = NonZeroU32::new(queue.sample_rate()).expect("engine rate is non-zero");
        let stale_axis = NonZeroU32::new(if engine_axis.get() == 44_100 {
            48_000
        } else {
            44_100
        })
        .expect("fixture rate is non-zero");
        let Some(Activity::Running(run)) = controller.activity.as_mut() else {
            panic!("fixture run is active");
        };
        run.axis = stale_axis;

        handle_event(&mut controller, &event, &queue, &state, &config);

        let Some(Activity::Running(ending)) = controller.activity.as_ref() else {
            panic!("cancelled pass remains owned until its final checkpoint");
        };
        assert_eq!(ending.track_id, track_id);
        assert_eq!(
            controller.pending.front(),
            Some(&track_id),
            "the event loop queues a reopen but does not cross the commit barrier"
        );

        drop(tx);
        controller.drive(&queue, &state, &config).await;
        let Some(Activity::Running(reopened)) = controller.activity.as_ref() else {
            panic!("the pass reopens after the old run closes");
        };
        assert_eq!(reopened.track_id, track_id);
        assert_eq!(
            reopened.axis, engine_axis,
            "the replacement pass uses the engine's current axis"
        );
        cancel.cancel();
    }

    #[kithara::test(native, tokio)]
    async fn engine_start_and_route_change_restart_a_stale_axis() {
        assert_axis_event_restarts(EngineEvent::Started.into()).await;
        assert_axis_event_restarts(
            SessionEvent::RouteChanged {
                reason: RouteChangeReason::Unknown,
                previous_route: RouteDescription::default(),
            }
            .into(),
        )
        .await;
    }

    #[kithara::test(native, tokio)]
    async fn a_stale_axis_does_not_move_a_preempted_track_ahead_of_the_current_one() {
        let track_a = TrackId::from(1);
        let track_b = TrackId::from(2);
        let (_host, queue) = queue();
        queue
            .append_with_id(track_a, "file:///tmp/track-a.mp3".to_owned())
            .expect("append first track");
        queue
            .append_with_id(track_b, "file:///tmp/track-b.mp3".to_owned())
            .expect("append second track");
        let state = state_with_current(&[track_a, track_b], 0);
        let cancel = CancelToken::root();
        let config = app_config(&cancel);
        let (mut controller, tx) =
            controller_with_run(track_a, target("stale_preempted_axis"), None);
        let engine_axis = NonZeroU32::new(queue.sample_rate()).expect("engine rate is non-zero");
        let Some(Activity::Running(run)) = controller.activity.as_mut() else {
            panic!("fixture run is active");
        };
        run.axis = engine_axis;

        state.lock().current_track_index = Some(1);
        controller.on_track_changed(&queue, &state, &config);
        assert_eq!(controller.pending.front(), Some(&track_b));
        controller.on_tracks_changed(&queue, &state, &config);
        assert_eq!(controller.pending.front(), Some(&track_b));

        let Some(Activity::Running(run)) = controller.activity.as_mut() else {
            panic!("preempted run remains active until its channel closes");
        };
        run.axis = NonZeroU32::new(if engine_axis.get() == 44_100 {
            48_000
        } else {
            44_100
        })
        .expect("fixture rate is non-zero");
        handle_event(
            &mut controller,
            &EngineEvent::Started.into(),
            &queue,
            &state,
            &config,
        );

        assert_eq!(
            controller.pending.front(),
            Some(&track_b),
            "the current track stays ahead of the already-preempted run"
        );

        drop(tx);
        controller.drive(&queue, &state, &config).await;
        assert!(matches!(
            controller.activity.as_ref(),
            Some(Activity::Running(Run { track_id, .. })) if *track_id == track_b
        ));
        cancel.cancel();
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

    #[kithara::test(native, tokio)]
    async fn preemption_commits_a_checkpoint_before_starting_the_next_track() {
        let directory = tempfile::tempdir().expect("temporary analysis store");
        let pools = test_pools();
        let store = AppStore::builder(pools.clone())
            .backend(StorageBackend::Disk {
                root: directory.path().into(),
            })
            .build();
        let target = target_in(&store, "track-a");
        let cancel = CancelToken::root();
        let persistence = persistence(&cancel, pools.clone());
        let track_a = TrackId::from(1);
        let track_b = TrackId::from(2);
        let (_host, queue) = queue();
        queue
            .append_with_id(track_a, "file:///tmp/track-a.mp3".to_owned())
            .expect("append first track");
        queue
            .append_with_id(track_b, "file:///tmp/track-b.mp3".to_owned())
            .expect("append second track");
        let state = state_with_current(&[track_a, track_b], 0);
        let config = app_config(&cancel);
        let (mut controller, publication) = controller_with_run_and_persistence(
            track_a,
            target.clone(),
            Some(analysis()),
            Some(persistence),
        );

        state.lock().current_track_index = Some(1);
        controller.on_track_changed(&queue, &state, &config);
        assert!(matches!(
            controller.activity.as_ref(),
            Some(Activity::Running(Run { track_id, .. })) if *track_id == track_a
        ));

        drop(publication);
        controller.drive(&queue, &state, &config).await;
        assert!(matches!(
            controller.activity.as_ref(),
            Some(Activity::Committing(_))
        ));
        assert_eq!(controller.pending.front(), Some(&track_b));

        controller.drive(&queue, &state, &config).await;
        assert!(matches!(
            controller.activity.as_ref(),
            Some(Activity::Running(Run { track_id, .. })) if *track_id == track_b
        ));

        let reader = store
            .open_resource(target.key(), None)
            .expect("acknowledged checkpoint is committed");
        let mut bytes = pools.get::<u8>();
        reader
            .read_into(&mut bytes)
            .expect("committed checkpoint reads");
        let restored =
            AnalysisFile::parse(&bytes, &fingerprint()).expect("committed checkpoint validates");
        assert_eq!(restored.latest().analysis().revision(), 1);
        cancel.cancel();
    }

    /// Commits a run holding `value`; true when its key landed in the cache.
    fn finish_caches(value: Option<TrackAnalysis>) -> bool {
        let target = target("root");
        let (mut controller, tx) = controller_with_run(TrackId::allocate(), target.clone(), value);
        let state = Mutex::new(UiState::empty());
        controller.finish_run(&state);
        drop(tx);
        controller.cache.get(&target, sample_rate()).is_some()
    }

    #[kithara::test(native, flash(false))]
    fn commit_caches_the_complete_result() {
        assert!(
            finish_caches(Some(analysis())),
            "a close carrying a value caches the complete analysis"
        );
    }

    #[kithara::test(native, flash(false))]
    fn commit_caches_nothing_when_the_run_failed() {
        assert!(
            !finish_caches(None),
            "a run that closes with no value (failure/cancel) caches nothing"
        );
    }

    #[kithara::test(native, tokio)]
    fn finish_publishes_the_current_track() {
        let target = target("root_current");
        let analysis = analysis();
        let ids = [
            TrackId::allocate(),
            TrackId::allocate(),
            TrackId::allocate(),
        ];
        let (mut controller, tx) = controller_with_run(ids[1], target.clone(), Some(analysis));
        let state = state_with_current(&ids, 1);

        controller.finish_run(&state);

        let has_analysis = state.lock().analysis.is_some();
        assert!(has_analysis, "current run publishes to the UI");
        drop(tx);
    }

    #[kithara::test(native, tokio)]
    fn finish_caches_a_stale_track_without_publishing_it() {
        let target = target("root_stale");
        let analysis = analysis();
        let ids = [
            TrackId::allocate(),
            TrackId::allocate(),
            TrackId::allocate(),
        ];
        let (mut controller, tx) = controller_with_run(ids[0], target.clone(), Some(analysis));
        let state = state_with_current(&ids, 1);

        controller.finish_run(&state);

        let has_analysis = state.lock().analysis.is_some();
        assert!(
            !has_analysis,
            "stale run must not replace the current track's analysis"
        );
        assert!(
            controller.cache.get(&target, sample_rate()).is_some(),
            "stale run is still reusable by content key"
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
        let layouts =
            AssetLayoutRegistry::default().with::<File<AppPools>>(Arc::new(InvalidLayout));
        let store = AppStore::builder(test_pools())
            .backend(StorageBackend::Memory)
            .layouts(layouts)
            .build();
        let resource_config = AppResourceConfig::for_src(
            ResourceSrc::parse("https://analysis.test.invalid/invalid.mp3")
                .expect("valid test source"),
        )
        .store(store)
        .build();
        let error =
            AnalysisTarget::for_config(&resource_config).expect_err("layout must be rejected");
        let current = TrackId::allocate();
        let state = state_with_current(&[current], 0);
        state.lock().set_analysis(Some(analysis()));

        AnalysisController::reject_target(&state, current, &error);

        assert!(state.lock().analysis.is_none());
    }
}
