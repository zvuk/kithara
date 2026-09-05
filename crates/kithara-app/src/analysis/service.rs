use std::{collections::VecDeque, num::NonZeroU32};

use kithara::{
    analysis::AnalysisProgress,
    events::TrackId,
    platform::{
        CancelToken,
        tokio::{
            self,
            sync::{mpsc, watch},
        },
    },
};
use tracing::{debug, warn};

use super::{
    entry::{Entry, Stage, settled_for},
    handle::{AnalysisHandle, Request},
    run::Activity,
};
use crate::{
    config::AppConfig,
    pools::{AppQueueControl, AppResourceConfig, AppTrackSource},
    sources::build_resource_config,
    wave_cache::{AnalysisPersistence, AnalysisTarget, TrackAnalysisCache},
    waveform::TrackAnalysisRunner,
};

/// The one owner of track analysis in the app: the runner, the two-tier
/// cache, the persistence client, and one value per analysed resource.
pub(crate) struct AnalysisService {
    rx: mpsc::Receiver<Request>,
    pub(super) owner: Owner,
    cancel: CancelToken,
}

pub(super) struct Owner {
    pub(super) config: AppConfig,
    pub(super) runner: TrackAnalysisRunner,
    pub(super) cache: TrackAnalysisCache,
    pub(super) persistence: AnalysisPersistence,
    pub(super) entries: Vec<Entry>,
    /// Entries waiting for the runner, in request order.
    pub(super) pending: VecDeque<usize>,
    pub(super) active: Option<Activity>,
    /// The rate axis the last request named; every pass opens on it.
    pub(super) axis: Option<NonZeroU32>,
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
            config.worker.pools(),
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
    pub(super) fn subscribe(
        &mut self,
        queue: AppQueueControl,
        track_id: TrackId,
        source: AppTrackSource,
        axis: NonZeroU32,
    ) -> watch::Receiver<Option<AnalysisProgress>> {
        self.axis = Some(axis);
        let Some((index, config)) = self.entry_for(&queue, track_id, source) else {
            return watch::channel(None).1;
        };
        self.entries[index].point_at(config, queue, track_id);
        self.seed(index, axis);
        let rx = self.entries[index].subscribe();
        self.schedule(index, axis);
        self.pump();
        rx
    }

    /// Put a library list in line behind every held entry; the cache is read
    /// when each pass is about to open.
    pub(super) fn warm(
        &mut self,
        queue: &AppQueueControl,
        track_ids: &[TrackId],
        axis: NonZeroU32,
    ) {
        self.axis = Some(axis);
        for &track_id in track_ids {
            let Some(source) = queue.track_source(track_id) else {
                continue;
            };
            let Some((index, config)) = self.entry_for(queue, track_id, source) else {
                continue;
            };
            if !self.entries[index].is_held() {
                self.entries[index].point_at(config, queue.clone(), track_id);
            }
            self.schedule(index, axis);
        }
        self.pump();
    }

    /// The entry for `source` and the resource it resolves to; the entry is
    /// created on first sight.
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

    /// Offer the cached value for `axis` to an entry holding none.
    pub(super) fn seed(&mut self, index: usize, axis: NonZeroU32) {
        let entry = &self.entries[index];
        if entry.value_for(axis).is_some() {
            return;
        }
        if let Some(progress) = self.cache.get(entry.target(), axis) {
            debug!(
                track_id = ?entry.track_id(),
                revision = progress.analysis().revision(),
                complete = progress.analysis().is_complete(),
                resumable = progress.is_resumable(),
                "analysis: cached snapshot served"
            );
            entry.offer(progress);
        }
    }

    /// Put the entry in line unless it is settled on `axis` or a pass on
    /// that axis already ran its course. A held entry ends a background pass.
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
            .is_some_and(|progress| settled_for(&progress, fingerprint))
        {
            debug!(?track_id, held, "analysis: settled; nothing to schedule");
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
    pub(super) fn pump(&mut self) {
        self.retire_stale_axis();
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

    fn next_pending(&mut self) -> Option<usize> {
        let position = self
            .pending
            .iter()
            .position(|&index| self.entries[index].is_held())
            .unwrap_or(0);
        self.pending.remove(position)
    }
}

/// Build an analysis resource from a track's source, reusing the shared
/// stores so the analysis and the player share one download.
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
