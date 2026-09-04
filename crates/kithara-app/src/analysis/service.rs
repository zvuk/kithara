use std::{collections::VecDeque, num::NonZeroU32};

use kithara::{
    analysis::AnalysisProgress,
    events::TrackId,
    platform::{
        CancelToken,
        tokio::{
            self,
            sync::{mpsc, watch},
            task::{self, JoinHandle},
        },
    },
};
use tracing::{debug, warn};

use super::{
    entry::{Entry, Stage, complete_for},
    handle::{AnalysisHandle, Request},
};
use crate::{
    config::AppConfig,
    pools::{AppQueueControl, AppResourceConfig, AppTrackSource},
    sources::build_resource_config,
    wave_cache::{
        AnalysisPersistence, AnalysisPersistenceError, AnalysisTarget, TrackAnalysisCache,
        token_for,
    },
    waveform::TrackAnalysisRunner,
};

/// The one owner of track analysis in the app: the runner, the two-tier
/// cache, the persistence client, and one value per analysed resource.
pub(crate) struct AnalysisService {
    rx: mpsc::Receiver<Request>,
    owner: Owner,
    cancel: CancelToken,
}

struct Owner {
    config: AppConfig,
    runner: TrackAnalysisRunner,
    cache: TrackAnalysisCache,
    persistence: AnalysisPersistence,
    entries: Vec<Entry>,
    /// Entries waiting for the runner, in request order.
    pending: VecDeque<usize>,
    active: Option<Activity>,
    /// The rate axis the last request named; every pass opens on it.
    axis: Option<NonZeroU32>,
}

enum Activity {
    Running(Run),
    Committing(JoinHandle<Result<(), AnalysisPersistenceError>>),
}

struct Run {
    entry: usize,
    axis: NonZeroU32,
    rx: watch::Receiver<Option<AnalysisProgress>>,
    /// Set when the owner ended the pass itself; the entry goes back in line.
    requeue: bool,
}

impl AnalysisService {
    /// `cancel` must descend from the app master; the runner's worker and
    /// every pass live under it.
    pub(crate) fn new(
        config: &AppConfig,
        persistence: AnalysisPersistence,
        cancel: CancelToken,
    ) -> (Self, AnalysisHandle) {
        let (handle, rx) = AnalysisHandle::channel();
        let runner = TrackAnalysisRunner::new(
            &cancel,
            config.base_worker.clone(),
            config.analysis_chunk_seconds,
            config.waveform_max_buckets,
            config.beat_analysis.clone(),
            config.worker.pools().clone(),
        );
        let cache = TrackAnalysisCache::new(
            runner.fingerprint().clone(),
            config.worker.pools().clone(),
            config.analysis_chunk_seconds,
        );
        let owner = Owner {
            config: config.clone(),
            runner,
            cache,
            persistence,
            entries: Vec::new(),
            pending: VecDeque::new(),
            active: None,
            axis: None,
        };
        (Self { rx, owner, cancel }, handle)
    }

    pub(crate) async fn run(self) {
        let Self {
            mut rx,
            mut owner,
            cancel,
        } = self;
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                request = rx.recv() => match request {
                    Some(request) => owner.handle(request),
                    None => break,
                },
                () = owner.drive() => {}
            }
        }
    }
}

impl Owner {
    fn handle(&mut self, request: Request) {
        match request {
            Request::Subscribe {
                queue,
                track_id,
                source,
                axis,
                reply,
            } => {
                let rx = self.subscribe(queue, track_id, source, axis);
                if reply.send(rx).is_err() {
                    debug!(?track_id, "analysis: subscriber left before its reply");
                }
            }
            Request::Warm {
                queue,
                track_ids,
                axis,
            } => self.warm(&queue, &track_ids, axis),
        }
    }

    /// The entry's receiver, seeded with what the owner holds. A source with
    /// no analysis resource gets a closed receiver holding nothing.
    fn subscribe(
        &mut self,
        queue: AppQueueControl,
        track_id: TrackId,
        source: AppTrackSource,
        axis: NonZeroU32,
    ) -> watch::Receiver<Option<AnalysisProgress>> {
        self.axis = Some(axis);
        let Some(index) = self.entry_for(queue, track_id, source, axis) else {
            return watch::channel(None).1;
        };
        let rx = self.entries[index].subscribe();
        self.schedule(index, axis);
        self.pump();
        rx
    }

    fn warm(&mut self, queue: &AppQueueControl, track_ids: &[TrackId], axis: NonZeroU32) {
        self.axis = Some(axis);
        for &track_id in track_ids {
            let Some(source) = queue.track_source(track_id) else {
                continue;
            };
            if let Some(index) = self.entry_for(queue.clone(), track_id, source, axis) {
                self.schedule(index, axis);
            }
        }
        self.pump();
    }

    /// The entry for `source`, created on first sight and seeded from the
    /// cache for `axis`.
    fn entry_for(
        &mut self,
        queue: AppQueueControl,
        track_id: TrackId,
        source: AppTrackSource,
        axis: NonZeroU32,
    ) -> Option<usize> {
        let Some(config) = resource_config_from_source(source, &self.config) else {
            debug!(
                ?track_id,
                "analysis: source yields no resource; nothing to analyse"
            );
            return None;
        };
        let target = match AnalysisTarget::for_config(&config) {
            Ok(target) => target,
            Err(error) => {
                warn!(%error, ?track_id, "analysis layout rejected the derived resource key");
                return None;
            }
        };
        let known = self
            .entries
            .iter()
            .position(|entry| entry.target().is_same(&target));
        let index = if let Some(index) = known {
            self.entries[index].point_at(config, queue, track_id);
            index
        } else {
            self.entries
                .push(Entry::new(target, config, queue, track_id));
            self.entries.len() - 1
        };
        let entry = &self.entries[index];
        if entry.value_for(axis).is_none()
            && let Some(progress) = self.cache.get(entry.target(), axis)
        {
            debug!(
                ?track_id,
                revision = progress.analysis().revision(),
                complete = progress.analysis().is_complete(),
                resumable = progress.is_resumable(),
                "analysis: cached snapshot served"
            );
            entry.offer(progress);
        }
        Some(index)
    }

    /// Put the entry in line unless it is complete on `axis` or a pass on
    /// that axis already ran its course. A held entry ends a background pass.
    fn schedule(&mut self, index: usize, axis: NonZeroU32) {
        let fingerprint = self.runner.fingerprint();
        let entry = &mut self.entries[index];
        let track_id = entry.track_id();
        let held = entry.is_held();
        if entry
            .value_for(axis)
            .is_some_and(|progress| complete_for(&progress, fingerprint))
        {
            debug!(?track_id, held, "analysis: complete; nothing to schedule");
            return;
        }
        match entry.stage() {
            Stage::Queued | Stage::Running => {}
            Stage::Ended(on) if on == axis => {
                debug!(
                    ?track_id,
                    held, "analysis: the pass ran its course; left alone"
                );
                return;
            }
            Stage::Idle | Stage::Ended(_) => {
                entry.set_stage(Stage::Queued);
                self.pending.push_back(index);
                debug!(?track_id, held, "analysis: scheduled");
            }
        }
        if held {
            self.preempt_background(index);
        }
    }

    /// End a background pass so the held entry `index` can take the runner.
    fn preempt_background(&mut self, index: usize) {
        let Some(Activity::Running(run)) = &mut self.active else {
            return;
        };
        if run.entry == index || run.requeue || self.entries[run.entry].is_held() {
            return;
        }
        debug!(
            preempted = ?self.entries[run.entry].track_id(),
            held = ?self.entries[index].track_id(),
            "analysis: background pass preempted by a held track"
        );
        run.requeue = true;
        self.runner.clear();
    }

    /// End a pass measured on another axis than the last request named.
    fn retire_stale_axis(&mut self) {
        let (Some(Activity::Running(run)), Some(axis)) = (&mut self.active, self.axis) else {
            return;
        };
        if run.axis == axis || run.requeue {
            return;
        }
        warn!(
            from = run.axis.get(),
            to = axis.get(),
            "analysis: the host rate moved; the pass restarts on the new axis"
        );
        run.requeue = true;
        self.runner.clear();
    }

    /// Start the next pass when the runner is free: held entries first, then
    /// the rest in request order.
    fn pump(&mut self) {
        self.retire_stale_axis();
        if self.active.is_some() {
            return;
        }
        if !self.runner.is_active() {
            self.pending.clear();
            return;
        }
        while let Some(index) = self.next_pending() {
            if let Some(run) = self.open_run(index) {
                self.active = Some(Activity::Running(run));
                return;
            }
        }
    }

    fn next_pending(&mut self) -> Option<usize> {
        let position = self
            .pending
            .iter()
            .position(|&index| self.entries[index].is_held())
            .unwrap_or(0);
        self.pending.remove(position)
    }

    fn open_run(&mut self, index: usize) -> Option<Run> {
        let axis = self.axis?;
        let fingerprint = self.runner.fingerprint();
        let entry = &mut self.entries[index];
        let track_id = entry.track_id();
        let held = entry.is_held();
        let seed = entry.value_for(axis);
        if seed
            .as_ref()
            .is_some_and(|progress| complete_for(progress, fingerprint))
        {
            entry.set_stage(Stage::Ended(axis));
            return None;
        }
        let queue = entry.queue().clone();
        let deliver = move |producer| queue.attach_observer(track_id, producer);
        let config = entry.config().clone();
        let resumed = seed.as_ref().is_some_and(AnalysisProgress::is_resumable);
        let rx = match seed.filter(AnalysisProgress::is_resumable) {
            Some(progress) => match self.runner.resume(config, progress, deliver) {
                Ok(rx) => rx,
                Err(error) => {
                    warn!(%error, ?track_id, "analysis: cached checkpoint rejected");
                    entry.set_stage(Stage::Ended(axis));
                    return None;
                }
            },
            None => self
                .runner
                .analyze(config, token_for(entry.target().key()), axis, deliver),
        };
        debug!(
            ?track_id,
            held,
            resumed,
            axis = axis.get(),
            "analysis: pass opened"
        );
        entry.set_stage(Stage::Running);
        Some(Run {
            entry: index,
            axis,
            rx,
            requeue: false,
        })
    }

    /// Await the pass's next publication or the commit of its last one.
    /// Parks while the runner is idle.
    async fn drive(&mut self) {
        match &mut self.active {
            Some(Activity::Running(run)) => {
                if run.rx.changed().await.is_err() {
                    self.finish_run();
                    self.pump();
                } else {
                    self.publish();
                }
            }
            Some(Activity::Committing(task)) => {
                match task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => warn!(%error, "analysis: final checkpoint commit failed"),
                    Err(error) => warn!(%error, "analysis: final checkpoint task failed"),
                }
                self.active = None;
                self.pump();
            }
            None => std::future::pending().await,
        }
    }

    /// Hand the pass's latest revision to the entry, the cache, and (best
    /// effort) persistence.
    fn publish(&mut self) {
        let Some(Activity::Running(run)) = &self.active else {
            return;
        };
        let Some(progress) = run.rx.borrow().clone() else {
            return;
        };
        let entry = &self.entries[run.entry];
        let revision = progress.analysis().revision();
        let complete = progress.analysis().is_complete();
        let target = entry.target().clone();
        self.cache.put(target.clone(), progress.clone());
        let queued = self.persistence.try_store(target, progress.clone());
        let sent = entry.offer(progress);
        debug!(
            track_id = ?entry.track_id(),
            revision,
            complete,
            held = entry.is_held(),
            sent,
            queued,
            "analysis: revision published"
        );
    }

    /// The pass closed: keep its last value everywhere and commit it before
    /// the runner takes the next entry.
    fn finish_run(&mut self) {
        let Some(Activity::Running(run)) = self.active.take() else {
            return;
        };
        let entry = &mut self.entries[run.entry];
        let track_id = entry.track_id();
        if run.requeue {
            entry.set_stage(Stage::Queued);
            self.pending.push_back(run.entry);
        } else {
            entry.set_stage(Stage::Ended(run.axis));
        }
        let Some(progress) = run.rx.borrow().clone() else {
            debug!(
                ?track_id,
                requeued = run.requeue,
                "analysis: pass closed without a value"
            );
            return;
        };
        let target = entry.target().clone();
        self.cache.put(target.clone(), progress.clone());
        let sent = entry.offer(progress.clone());
        debug!(
            ?track_id,
            revision = progress.analysis().revision(),
            complete = progress.analysis().is_complete(),
            held = entry.is_held(),
            sent,
            requeued = run.requeue,
            "analysis: final published"
        );
        let persistence = self.persistence.clone();
        let commit = task::spawn(async move { persistence.store(target, progress).await });
        self.active = Some(Activity::Committing(commit));
    }
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
            AnalysisFile, AnalysisFingerprint, AnalysisProgress, BeatArtifact, BeatSnapshot,
            BeatState, Coverage, FrameRange, Waveform,
        },
        assets::{
            AssetLayout, AssetLayoutRegistry, AssetResource, AssetSource, ReadSide, StorageBackend,
        },
        events::TrackId,
        file::File,
        host::HostConfig,
        net::{HttpClient, NetOptions},
        platform::{
            CancelToken,
            sync::Arc,
            time::Duration,
            tokio::{runtime::Handle, sync::watch},
        },
        play::{PlayWorkerConfig, PlayerConfig, PlayerImpl},
        queue::QueueConfig,
        stream::dl::{Downloader, DownloaderConfig},
        worker::{DispatcherConfig, TaskConfig, Worker, WorkerConfig},
    };
    use kithara_test_utils::kithara;

    use super::{Activity, AnalysisService, Owner, Run, resource_config_from_source};
    use crate::{
        analysis::entry::Stage,
        config::AppConfig,
        pools::{
            self, AppHost, AppPools, AppQueue, AppQueueControl, AppStore, AppTrackSource,
            AppWorker, Pools,
        },
        wave_cache::{AnalysisPersistence, AnalysisTarget, persistence::AnalysisPersistenceConfig},
        waveform::TrackAnalysis,
    };

    fn chunk_seconds() -> NonZeroU32 {
        NonZeroU32::new(16).expect("fixture chunk duration is non-zero")
    }

    fn test_pools() -> Pools {
        pools::build().expect("valid app pool policy")
    }

    fn axis() -> NonZeroU32 {
        NonZeroU32::new(44_100).expect("fixture rate is non-zero")
    }

    fn other_axis() -> NonZeroU32 {
        NonZeroU32::new(48_000).expect("fixture rate is non-zero")
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

    fn grid() -> BeatSnapshot {
        BeatSnapshot::new(
            BeatArtifact::new(
                128.0,
                vec![(0, Some(0.9)), (500, None)],
                vec![(0, Some(0.9))],
            ),
            BeatState::Final,
            Vec::new(),
        )
    }

    /// A settled snapshot covering `[0, covered)` of a 1000-frame track.
    fn snapshot(
        revision: u64,
        covered: u64,
        fingerprint: AnalysisFingerprint,
        beat: Option<BeatSnapshot>,
    ) -> TrackAnalysis {
        let mut coverage = Coverage::default();
        coverage.insert(FrameRange::new(0, covered));
        TrackAnalysis::builder()
            .token("test-track".into())
            .revision(revision)
            .source_sample_rate(axis())
            .extent(1_000)
            .settled(true)
            .coverage(coverage)
            .fingerprint(fingerprint)
            .waveform(one_bucket_wave())
            .maybe_beat(beat)
            .build()
    }

    fn analysis() -> TrackAnalysis {
        snapshot(1, 1_000, fingerprint(), None)
    }

    fn revision_of(revision: u64) -> TrackAnalysis {
        snapshot(revision, 1_000, fingerprint(), None)
    }

    fn revision_held(rx: &watch::Receiver<Option<AnalysisProgress>>) -> Option<u64> {
        rx.borrow().as_ref().map(|p| p.analysis().revision())
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

    fn track(queue: &AppQueueControl, id: u64, url: &str) -> (TrackId, AppTrackSource) {
        let track_id = TrackId::from(id);
        queue
            .append_with_id(track_id, url.to_owned())
            .expect("append test track");
        let source = queue.track_source(track_id).expect("track has a source");
        (track_id, source)
    }

    fn memory_store() -> AppStore {
        AppStore::builder(test_pools())
            .backend(StorageBackend::Memory)
            .build()
    }

    fn app_config(cancel: &CancelToken, store: AppStore) -> AppConfig {
        let pools = test_pools();
        let worker = AppWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
        AppConfig::builder()
            .downloader(Downloader::new(
                DownloaderConfig::for_client(HttpClient::new(
                    NetOptions::builder().build(),
                    pools,
                    cancel.child(),
                ))
                .build(),
            ))
            .shutdown(cancel.child())
            .worker(worker)
            .store(store)
            .build()
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
                .name("analysis-service-test")
                .build(),
            TaskConfig::new(),
        ))
        .expect("persistence fixture starts")
    }

    fn owner_in(cancel: &CancelToken, store: AppStore) -> Owner {
        let config = app_config(cancel, store);
        let persistence = persistence(cancel, test_pools());
        let (service, _handle) = AnalysisService::new(&config, persistence, cancel.child());
        service.owner
    }

    fn owner(cancel: &CancelToken) -> Owner {
        owner_in(cancel, memory_store())
    }

    fn target_of(owner: &Owner, source: &AppTrackSource) -> AnalysisTarget {
        let config = resource_config_from_source(source.clone(), &owner.config)
            .expect("source yields a resource");
        AnalysisTarget::for_config(&config).expect("source has an analysis target")
    }

    fn running_entry(owner: &Owner) -> Option<usize> {
        match owner.active.as_ref() {
            Some(Activity::Running(run)) => Some(run.entry),
            _ => None,
        }
    }

    fn running_track(owner: &Owner) -> Option<TrackId> {
        running_entry(owner).map(|index| owner.entries[index].track_id())
    }

    fn requeued(owner: &Owner) -> bool {
        matches!(owner.active.as_ref(), Some(Activity::Running(run)) if run.requeue)
    }

    fn pending_tracks(owner: &Owner) -> Vec<TrackId> {
        owner
            .pending
            .iter()
            .map(|&index| owner.entries[index].track_id())
            .collect()
    }

    /// Replace whatever pass the runner opened with a channel the test feeds.
    fn take_over_run(
        owner: &mut Owner,
        value: Option<TrackAnalysis>,
    ) -> watch::Sender<Option<AnalysisProgress>> {
        let index = running_entry(owner).expect("a pass is in flight");
        owner.runner.clear();
        let (tx, rx) = watch::channel(value.map(progress));
        owner.active = Some(Activity::Running(Run {
            entry: index,
            axis: axis(),
            rx,
            requeue: false,
        }));
        tx
    }

    /// A stored snapshot that covers only part of the track carries both
    /// artifacts and no resume, so it looks finished to a check that counts
    /// artifacts. It is not: the track is analysed to the end, not to where
    /// an earlier pass happened to stop.
    #[kithara::test(native, tokio)]
    async fn an_incomplete_stored_analysis_is_finished_rather_than_served_as_final() {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let (_host, queue) = queue();
        let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let partial = snapshot(3, 400, owner.runner.fingerprint().clone(), Some(grid()));
        assert!(!partial.is_complete());
        owner
            .cache
            .put(target_of(&owner, &source), progress(partial));

        let rx = owner.subscribe(queue, track_id, source, axis());

        assert_eq!(
            revision_held(&rx),
            Some(3),
            "the partial snapshot is served as far as it goes"
        );
        assert_eq!(
            running_track(&owner),
            Some(track_id),
            "and a pass finishes it"
        );
        cancel.cancel();
    }

    /// A hit whose configuration expects an artifact it lacks is served and
    /// refilled by a pass for the same entry.
    #[kithara::test(native, tokio)]
    async fn a_hit_missing_an_artifact_is_served_and_refilled() {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let fingerprint = owner.runner.fingerprint().clone();
        assert!(
            fingerprint.beat().is_some(),
            "fixture needs an artifact to omit"
        );
        let (_host, queue) = queue();
        let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
        owner.cache.put(
            target_of(&owner, &source),
            progress(snapshot(7, 1_000, fingerprint, None)),
        );

        let rx = owner.subscribe(queue, track_id, source, axis());

        assert_eq!(revision_held(&rx), Some(7), "the hit is served");
        assert_eq!(
            running_track(&owner),
            Some(track_id),
            "the artifact is refilled"
        );
        cancel.cancel();
    }

    #[kithara::test(native, tokio)]
    async fn an_entry_is_held_only_while_a_deck_keeps_its_receiver() {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let (_host, queue) = queue();
        let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");

        let rx = owner.subscribe(queue, track_id, source, axis());
        assert!(owner.entries[0].is_held());

        drop(rx);
        assert!(
            !owner.entries[0].is_held(),
            "the owner's own handle is no receiver"
        );
        cancel.cancel();
    }

    #[kithara::test(native, tokio)]
    async fn a_complete_hit_is_served_without_a_pass() {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let (_host, queue) = queue();
        let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let complete = snapshot(5, 1_000, owner.runner.fingerprint().clone(), Some(grid()));
        owner
            .cache
            .put(target_of(&owner, &source), progress(complete));

        let rx = owner.subscribe(queue, track_id, source, axis());

        assert_eq!(revision_held(&rx), Some(5));
        assert!(owner.active.is_none(), "nothing is left to analyse");
        assert!(owner.pending.is_empty());
        cancel.cancel();
    }

    /// The run publishes for the track it was opened on. Which track the
    /// player reports as current is playback state; it does not decide whether
    /// a deck holding the analysed track gets to see the revision.
    #[kithara::test(native, tokio)]
    async fn every_revision_reaches_the_deck_that_holds_the_track() {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let (_host, queue) = queue();
        let (_playing, _) = track(&queue, 8, "file:///tmp/track-8.mp3");
        let (held, source) = track(&queue, 7, "file:///tmp/track-7.mp3");
        assert_eq!(
            queue.current_index(),
            Some(0),
            "the player sits on another track"
        );
        let rx = owner.subscribe(queue, held, source, axis());
        let tx = take_over_run(&mut owner, None);

        tx.send(Some(progress(revision_of(1))))
            .expect("run publishes");
        owner.publish();
        assert_eq!(revision_held(&rx), Some(1));
        tx.send(Some(progress(revision_of(2))))
            .expect("run publishes");
        owner.publish();

        assert_eq!(
            revision_held(&rx),
            Some(2),
            "the deck holding the track sees the latest revision"
        );
        cancel.cancel();
    }

    #[kithara::test(native, tokio)]
    async fn two_decks_holding_one_track_share_one_pass() {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let (_host_a, queue_a) = queue();
        let (_host_b, queue_b) = queue();
        let (track_a, source_a) = track(&queue_a, 1, "file:///tmp/shared.mp3");
        let (track_b, source_b) = track(&queue_b, 2, "file:///tmp/shared.mp3");

        let rx_a = owner.subscribe(queue_a, track_a, source_a, axis());
        let tx = take_over_run(&mut owner, None);
        let rx_b = owner.subscribe(queue_b, track_b, source_b, axis());

        assert_eq!(owner.entries.len(), 1, "one resource, one entry");
        assert!(!requeued(&owner), "the pass in flight serves both decks");
        assert!(owner.pending.is_empty());
        tx.send(Some(progress(revision_of(1))))
            .expect("run publishes");
        owner.publish();
        assert_eq!(revision_held(&rx_a), Some(1));
        assert_eq!(revision_held(&rx_b), Some(1));
        cancel.cancel();
    }

    #[kithara::test(native, tokio)]
    async fn a_held_track_preempts_a_background_run() {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let (_host, queue) = queue();
        let (background, _) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let (held, source) = track(&queue, 2, "file:///tmp/track-2.mp3");
        owner.warm(&queue, &[background], axis());
        assert_eq!(running_track(&owner), Some(background));
        let tx = take_over_run(&mut owner, None);

        let _rx = owner.subscribe(queue, held, source, axis());

        assert_eq!(
            running_track(&owner),
            Some(background),
            "the ended pass stays owned until its channel closes"
        );
        assert!(requeued(&owner), "and goes back in line");
        assert_eq!(pending_tracks(&owner), vec![held]);

        drop(tx);
        owner.drive().await;
        assert_eq!(
            running_track(&owner),
            Some(held),
            "the held track takes the runner"
        );
        assert_eq!(pending_tracks(&owner), vec![background]);
        cancel.cancel();
    }

    #[kithara::test(native, tokio)]
    async fn a_background_track_waits_for_a_held_one() {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let (_host, queue) = queue();
        let (held, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let (background, _) = track(&queue, 2, "file:///tmp/track-2.mp3");
        let (later, later_source) = track(&queue, 3, "file:///tmp/track-3.mp3");
        let _rx = owner.subscribe(queue.clone(), held, source, axis());
        let _tx = take_over_run(&mut owner, None);

        owner.warm(&queue, &[background], axis());
        assert_eq!(running_track(&owner), Some(held));
        assert!(!requeued(&owner), "a warm request ends no pass");

        let _later_rx = owner.subscribe(queue, later, later_source, axis());
        assert!(
            !requeued(&owner),
            "a held pass is not preempted by another held track"
        );
        assert_eq!(pending_tracks(&owner), vec![background, later]);
        assert_eq!(
            owner
                .next_pending()
                .map(|index| owner.entries[index].track_id()),
            Some(later),
            "held entries go first, then request order"
        );
        cancel.cancel();
    }

    #[kithara::test(native, tokio)]
    async fn a_pass_restarts_on_the_axis_the_next_request_names() {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let (_host, queue) = queue();
        let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let _rx = owner.subscribe(queue.clone(), track_id, source.clone(), axis());
        let tx = take_over_run(&mut owner, None);

        let _again = owner.subscribe(queue, track_id, source, other_axis());
        assert_eq!(running_track(&owner), Some(track_id));
        assert!(
            requeued(&owner),
            "the stale pass ends and the entry waits for its close"
        );

        drop(tx);
        owner.drive().await;
        let Some(Activity::Running(run)) = owner.active.as_ref() else {
            panic!("the pass reopens after the old run closes");
        };
        assert_eq!(owner.entries[run.entry].track_id(), track_id);
        assert_eq!(run.axis, other_axis(), "on the axis the request named");
        cancel.cancel();
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
        let cancel = CancelToken::root();
        let mut owner = owner_in(&cancel, store.clone());
        let (_host, queue) = queue();
        let (track_a, source_a) = track(&queue, 1, "file:///tmp/track-a.mp3");
        let (track_b, source_b) = track(&queue, 2, "file:///tmp/track-b.mp3");
        let target = target_of(&owner, &source_a);
        let rx_a = owner.subscribe(queue.clone(), track_a, source_a, axis());
        let publication = take_over_run(&mut owner, Some(analysis()));
        drop(rx_a);

        let _rx_b = owner.subscribe(queue, track_b, source_b, axis());
        assert_eq!(running_track(&owner), Some(track_a));

        drop(publication);
        owner.drive().await;
        assert!(matches!(owner.active, Some(Activity::Committing(_))));
        assert_eq!(pending_tracks(&owner), vec![track_b, track_a]);

        owner.drive().await;
        assert_eq!(running_track(&owner), Some(track_b));

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

    /// Closes a run holding `value`; what the entry and the cache hold after.
    async fn close_run(value: Option<TrackAnalysis>) -> (Option<u64>, bool, Stage) {
        let cancel = CancelToken::root();
        let mut owner = owner(&cancel);
        let (_host, queue) = queue();
        let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
        let target = target_of(&owner, &source);
        let rx = owner.subscribe(queue, track_id, source, axis());
        let tx = take_over_run(&mut owner, value);

        drop(tx);
        owner.drive().await;

        let cached = owner.cache.get(&target, axis()).is_some();
        let stage = owner.entries[0].stage();
        cancel.cancel();
        (revision_held(&rx), cached, stage)
    }

    #[kithara::test(native, tokio)]
    async fn a_close_carrying_a_value_publishes_and_caches_it() {
        let (held, cached, stage) = close_run(Some(analysis())).await;
        assert_eq!(held, Some(1));
        assert!(cached);
        assert_eq!(stage, Stage::Ended(axis()), "the pass ran its course");
    }

    #[kithara::test(native, tokio)]
    async fn a_close_without_a_value_leaves_the_entry_as_it_was() {
        let (held, cached, stage) = close_run(None).await;
        assert_eq!(held, None);
        assert!(!cached, "a run that closes with no value caches nothing");
        assert_eq!(stage, Stage::Ended(axis()), "and is not retried on its own");
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
    async fn an_invalid_layout_yields_no_analysis() {
        let layouts =
            AssetLayoutRegistry::default().with::<File<AppPools>>(Arc::new(InvalidLayout));
        let store = AppStore::builder(test_pools())
            .backend(StorageBackend::Memory)
            .layouts(layouts)
            .build();
        let cancel = CancelToken::root();
        let mut owner = owner_in(&cancel, store);
        let (_host, queue) = queue();
        let (track_id, source) = track(&queue, 1, "file:///tmp/invalid.mp3");

        let mut rx = owner.subscribe(queue, track_id, source, axis());

        assert!(revision_held(&rx).is_none(), "the deck shows nothing");
        assert!(rx.changed().await.is_err(), "and nothing will come");
        assert!(owner.entries.is_empty());
        cancel.cancel();
    }
}
