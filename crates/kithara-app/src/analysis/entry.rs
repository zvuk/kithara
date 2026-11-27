use std::num::NonZeroU32;

use kithara::{analysis::AnalysisProgress, events::TrackId, platform::tokio::sync::watch};

use super::artifacts::TrackArtifacts;
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
    tx: watch::Sender<Option<TrackArtifacts>>,
    #[field(get, copy)]
    stage: Stage,
    #[field(get, copy)]
    track_id: TrackId,
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
            config,
            queue,
            track_id,
            tx: watch::channel(None).0,
            held: None,
            stage: Stage::Idle,
        }
    }

    /// Publish what this track holds. A pass result is accepted only when it
    /// outranks the one already published.
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
    ) {
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

    /// Publish what the track holds: its pass result, or its failed pass.
    pub(crate) fn republish(&self) {
        let analysis = self
            .held
            .as_ref()
            .map(|progress| progress.analysis().clone());
        let failed = self.pass_failed();
        let published =
            (analysis.is_some() || failed).then(|| TrackArtifacts::new(analysis, failed));
        self.tx.send_replace(published);
    }

    const fn pass_failed(&self) -> bool {
        matches!(self.stage, Stage::Failed(_))
    }

    pub(crate) fn set_stage(&mut self, stage: Stage) {
        let failed = self.pass_failed();
        self.stage = stage;
        if failed != self.pass_failed() {
            self.republish();
        }
    }

    pub(crate) fn value_for(&self, axis: NonZeroU32) -> Option<AnalysisProgress> {
        self.held
            .as_ref()
            .filter(|progress| progress.analysis().source_sample_rate() == axis)
            .cloned()
    }
}
