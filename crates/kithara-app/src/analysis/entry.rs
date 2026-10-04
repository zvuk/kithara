use std::num::NonZeroU32;

use kithara::{analysis::AnalysisProgress, events::TrackId, platform::tokio::sync::watch};

use super::{artifacts::TrackArtifacts, load::Loaded, supply::Prepared};
use crate::{
    pools::{AppQueueControl, AppResourceConfig},
    wave_cache::AnalysisTarget,
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, vis = "pub(crate)")]
pub(crate) struct Entry {
    #[field(get)]
    target: AnalysisTarget,
    #[field(get)]
    queue: AppQueueControl,
    #[field(get)]
    config: AppResourceConfig,
    held: Option<AnalysisProgress>,
    #[field(get)]
    prepared: Prepared,
    tx: watch::Sender<Option<TrackArtifacts>>,
    #[field(get, copy)]
    stage: Stage,
    #[field(get, copy)]
    track_id: TrackId,
    /// Which load this entry is on. Every artifact read is tagged with it, so
    /// an answer that arrives after the entry was re-pointed is dropped
    /// instead of overwriting the track that holds it now.
    #[field(get, copy)]
    epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Idle,
    Queued,
    Running,
    Ended(NonZeroU32),
    Failed(NonZeroU32),
}

impl Entry {
    pub(crate) fn new(
        target: AnalysisTarget,
        config: AppResourceConfig,
        queue: AppQueueControl,
        track_id: TrackId,
    ) -> Self {
        Self {
            target,
            prepared: Prepared::for_config(&config),
            config,
            queue,
            track_id,
            tx: watch::channel(None).0,
            held: None,
            stage: Stage::Idle,
            epoch: 0,
        }
    }

    /// Take in an artifact its own source answered with, and republish: one
    /// publication carries every origin the track has.
    pub(super) fn accept(&mut self, loaded: Loaded) {
        match loaded {
            Loaded::BeatGrid(result) => self.prepared.beat_grid = result.into(),
            Loaded::Waveform(result) => self.prepared.waveform = result.into(),
        }
        self.republish();
    }

    /// Publish what this track holds. A pass result is accepted only when it
    /// outranks the one already published; a prepared artifact is republished
    /// with it, so one publication carries both origins.
    pub(crate) fn offer(&mut self, progress: AnalysisProgress) -> bool {
        let same = self.held.as_ref().is_some_and(|held| {
            let held = held.analysis();
            let next = progress.analysis();
            held.token() == next.token() && held.revision() == next.revision()
        });
        if same {
            return false;
        }
        self.held = Some(progress);
        self.republish();
        true
    }

    pub(crate) fn point_at(
        &mut self,
        config: AppResourceConfig,
        queue: AppQueueControl,
        track_id: TrackId,
        epoch: u64,
    ) {
        self.epoch = epoch;
        self.prepared = Prepared::for_config(&config);
        self.config = config;
        self.queue = queue;
        self.track_id = track_id;
    }

    pub(crate) fn release(&mut self) {
        if !self.is_held() {
            self.held = None;
            self.tx.send_replace(None);
        }
    }

    pub(super) fn has_terminal_publication(&self) -> bool {
        matches!(self.stage, Stage::Ended(_) | Stage::Failed(_)) && self.tx.borrow().is_some()
    }

    delegate::delegate! {
        to self.tx {
            pub(crate) fn subscribe(&self) -> watch::Receiver<Option<TrackArtifacts>>;
            #[call(receiver_count)]
            #[expr($ > 0)]
            pub(crate) fn is_held(&self) -> bool;
            pub(super) fn closed(&self) -> impl Future<Output = ()>;
        }
    }

    /// Publish the prepared artifacts alone, before or without a pass.
    pub(crate) fn republish(&self) {
        let analysis = self
            .held
            .as_ref()
            .map(|progress| progress.analysis().clone());
        if analysis.is_none() && self.prepared.is_empty() {
            return;
        }
        self.tx
            .send_replace(Some(TrackArtifacts::new(analysis, self.prepared.clone())));
    }

    pub(crate) fn set_stage(&mut self, stage: Stage) {
        self.stage = stage;
    }

    pub(crate) fn value_for(&self, axis: NonZeroU32) -> Option<AnalysisProgress> {
        self.held
            .as_ref()
            .filter(|progress| progress.analysis().source_sample_rate() == axis)
            .cloned()
    }
}
