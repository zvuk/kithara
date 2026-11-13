use std::{
    collections::{BTreeMap, VecDeque},
    num::NonZeroU32,
};

use kithara::{
    events::TrackId,
    platform::{
        CancelToken,
        sync::Arc,
        tokio::{
            self,
            sync::{mpsc, watch},
        },
    },
};
use tracing::{debug, warn};

use super::{
    TrackArtifacts,
    entry::{Entry, Stage},
    handle::{AnalysisHandle, Request},
    load::LoadReply,
    run::Activity,
};
use crate::{
    config::AppConfig,
    pools::{AppQueueControl, AppResourceConfig, AppTrackSource},
    sources::build_resource_config,
    wave_cache::{AnalysisPersistence, AnalysisTarget, TrackAnalysisCache},
    waveform::TrackAnalysisRunner,
};

pub(crate) struct AnalysisService {
    pub(super) owner: Owner,
    cancel: CancelToken,
    rx: mpsc::Receiver<Request>,
    bpms: watch::Sender<Arc<BTreeMap<String, f64>>>,
}

pub(super) struct Owner {
    pub(super) persistence: AnalysisPersistence,
    pub(super) config: AppConfig,
    pub(super) active: Option<Activity>,
    pub(super) axis: Option<NonZeroU32>,
    pub(super) replies: mpsc::Receiver<LoadReply>,
    /// Where an artifact read hands its answer back to the one task that owns
    /// the entries, and where that task takes it.
    pub(super) loads: mpsc::Sender<LoadReply>,
    pub(super) cache: TrackAnalysisCache,
    pub(super) runner: TrackAnalysisRunner,
    pub(super) entries: Vec<Entry>,
    pub(super) pending: VecDeque<usize>,
    load_epoch: u64,
}

impl AnalysisService {
    pub(crate) fn new(
        config: &AppConfig,
        persistence: AnalysisPersistence,
        cancel: CancelToken,
    ) -> (Self, AnalysisHandle) {
        /// How many artifact answers may queue before a reading task waits. One track
        /// answers at most twice, so this only ever bounds a burst of re-pointed
        /// entries.
        const LOAD_REPLIES: usize = 16;

        let (bpms, published) = watch::channel(Arc::new(BTreeMap::new()));
        let (handle, rx) = AnalysisHandle::channel(published);
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
            config.worker.pools(),
            config.analysis_chunk_seconds,
        );
        let (loads, replies) = mpsc::channel(LOAD_REPLIES);
        let owner = Owner {
            loads,
            replies,
            runner,
            cache,
            persistence,
            config: config.clone(),
            entries: Vec::new(),
            pending: VecDeque::new(),
            active: None,
            axis: None,
            load_epoch: 0,
        };
        (
            Self {
                owner,
                cancel,
                rx,
                bpms,
            },
            handle,
        )
    }

    pub(crate) async fn run(self) {
        let Self {
            mut rx,
            mut owner,
            cancel,
            bpms,
        } = self;
        loop {
            let changed = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                request = rx.recv() => match request {
                    Some(request) => { owner.handle(request); true },
                    None => break,
                },
                changed = owner.drive() => changed
            };
            if !changed {
                continue;
            }
            let next = owner.bpms();
            if **bpms.borrow() != next {
                bpms.send_replace(Arc::new(next));
            }
        }
    }
}

impl Owner {
    pub(super) fn bpms(&self) -> BTreeMap<String, f64> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let bpm = TrackArtifacts::grid_from(
                    self.cache.analysis(entry.target()),
                    entry.prepared(),
                )?
                .as_raw()
                .bpm;
                (bpm.is_finite() && bpm > 0.0).then(|| (entry.config().source().to_string(), bpm))
            })
            .collect()
    }

    fn entry_for(
        &mut self,
        queue: &AppQueueControl,
        track_id: TrackId,
        source: AppTrackSource,
    ) -> Option<(usize, AppResourceConfig)> {
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
        let index = known.unwrap_or_else(|| {
            self.entries
                .push(Entry::new(target, config.clone(), queue.clone(), track_id));
            self.entries.len() - 1
        });
        Some((index, config))
    }

    fn point_entry(
        &mut self,
        index: usize,
        config: AppResourceConfig,
        queue: AppQueueControl,
        track_id: TrackId,
    ) {
        self.load_epoch = self.load_epoch.wrapping_add(1);
        self.entries[index].point_at(config, queue, track_id, self.load_epoch);
    }

    pub(super) fn prune_entries(&mut self) {
        for index in (0..self.entries.len()).rev() {
            let entry = &self.entries[index];
            if entry.is_held() || entry.queue().track_source(entry.track_id()).is_some() {
                continue;
            }
            if let Some(Activity::Running(run)) = &mut self.active
                && run.entry == index
            {
                run.requeue = false;
                self.runner.clear();
                continue;
            }
            self.entries.remove(index);
            self.pending.retain(|pending| *pending != index);
            for pending in &mut self.pending {
                if *pending > index {
                    *pending -= 1;
                }
            }
            if let Some(Activity::Running(run)) = &mut self.active
                && run.entry > index
            {
                run.entry -= 1;
            }
        }
    }

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

    fn next_pending(&mut self) -> Option<usize> {
        let position = self
            .pending
            .iter()
            .position(|&index| self.entries[index].is_held())
            .unwrap_or(0);
        self.pending.remove(position)
    }

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

    pub(super) fn pump(&mut self) {
        self.retire_stale_axis();
        self.prune_entries();
        let Some(axis) = self.axis else {
            return;
        };
        if self.active.is_some() {
            return;
        }
        while let Some(index) = self.next_pending() {
            if let Some(run) = self.open_run(index, axis) {
                self.active = Some(Activity::Running(run));
                return;
            }
        }
    }

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

    fn schedule(&mut self, index: usize, axis: NonZeroU32) {
        if !self.runner.is_active() {
            return;
        }
        let fingerprint = self.runner.fingerprint();
        let entry = &mut self.entries[index];
        let track_id = entry.track_id();
        let held = entry.is_held();
        if entry
            .value_for(axis)
            .is_some_and(|progress| entry.prepared().settled_for(&progress, fingerprint))
        {
            debug!(?track_id, held, "analysis: settled; nothing to schedule");
            return;
        }
        match entry.stage() {
            Stage::Queued | Stage::Running => {}
            Stage::Ended(on) | Stage::Failed(on) if on == axis => {
                debug!(
                    ?track_id,
                    held, "analysis: the pass ran its course; left alone"
                );
                return;
            }
            Stage::Idle | Stage::Ended(_) | Stage::Failed(_) => {
                entry.set_stage(Stage::Queued);
                self.pending.push_back(index);
                debug!(?track_id, held, "analysis: scheduled");
            }
        }
        if held {
            self.preempt_background(index);
        }
    }

    pub(super) fn seed(&mut self, index: usize, axis: NonZeroU32) {
        let entry = &self.entries[index];
        if entry.value_for(axis).is_some() {
            return;
        }
        let cached = self.cache.get(entry.target(), axis);
        let track_id = entry.track_id();
        let entry = &mut self.entries[index];
        let Some(progress) = cached else {
            entry.republish();
            return;
        };
        debug!(
            ?track_id,
            revision = progress.analysis().revision(),
            complete = progress.analysis().is_complete(),
            resumable = progress.is_resumable(),
            "analysis: cached snapshot served"
        );
        entry.offer(progress);
    }

    pub(super) fn subscribe(
        &mut self,
        queue: AppQueueControl,
        track_id: TrackId,
        source: AppTrackSource,
        axis: NonZeroU32,
    ) -> watch::Receiver<Option<TrackArtifacts>> {
        self.axis = Some(axis);
        let Some((index, config)) = self.entry_for(&queue, track_id, source) else {
            return watch::channel(None).1;
        };
        self.point_entry(index, config, queue, track_id);
        if matches!(self.entries[index].stage(), Stage::Failed(_)) {
            self.entries[index].set_stage(Stage::Idle);
        }
        self.start_loads(index);
        self.seed(index, axis);
        let rx = self.entries[index].subscribe();
        self.schedule(index, axis);
        self.pump();
        rx
    }

    pub(super) fn warm(
        &mut self,
        queue: &AppQueueControl,
        track_ids: &[TrackId],
        axis: NonZeroU32,
    ) {
        self.axis = Some(axis);
        self.prune_entries();
        for &track_id in track_ids {
            let Some(source) = queue.track_source(track_id) else {
                continue;
            };
            let Some((index, config)) = self.entry_for(queue, track_id, source) else {
                continue;
            };
            if !self.entries[index].is_held() {
                self.point_entry(index, config, queue.clone(), track_id);
                self.start_loads(index);
            }
            self.schedule(index, axis);
        }
        self.pump();
    }
}

pub(super) fn resource_config_from_source(
    source: AppTrackSource,
    config: &AppConfig,
) -> Option<AppResourceConfig> {
    match source {
        AppTrackSource::Config(cfg) => Some(*cfg),
        AppTrackSource::Uri(url) => build_resource_config(&url, config),
        _ => None,
    }
}
